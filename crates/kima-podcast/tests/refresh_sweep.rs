use kima_db::podcasts::{self, NewShow};
use kima_db::users::{self, Registration};
use kima_podcast::refresh;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn competing_sweepers_skip_locked_and_retry_after_failure(pool: PgPool) {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(&pool)
        .await
        .unwrap();
    let Registration::Created(user) = users::register(&pool, Uuid::now_v7(), "competing", "hash")
        .await
        .unwrap()
    else {
        panic!("registration failed")
    };
    let show = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://example.invalid/competing",
            title: "Show",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    podcasts::subscribe(&pool, user.id, show.id).await.unwrap();
    sqlx::query("UPDATE podcasts SET last_refreshed = now() - interval '2 hours' WHERE id = $1")
        .bind(show.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE FUNCTION test_sweep_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(941872); RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER test_sweep_gate BEFORE INSERT ON jobs FOR EACH ROW EXECUTE FUNCTION test_sweep_gate()").execute(&pool).await.unwrap();
    let mut gate = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(941872)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let make_pool = || {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
    };
    let a = make_pool().await.unwrap();
    let b = make_pool().await.unwrap();
    let pid_a: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&a)
        .await
        .unwrap();
    let pid_b: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&b)
        .await
        .unwrap();
    assert_ne!(pid_a, pid_b);
    let first = tokio::spawn(async move { refresh::sweep(&a).await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock')").bind(pid_a).fetch_one(&pool).await.unwrap();
            if blocked { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("first sweeper must reach gated insert");
    assert_eq!(refresh::sweep(&b).await.unwrap(), 0);
    sqlx::query("SELECT pg_advisory_unlock(941872)")
        .execute(&mut *gate)
        .await
        .unwrap();
    assert_eq!(first.await.unwrap(), 1);
    assert_eq!(
        refresh::sweep(&b).await.unwrap(),
        0,
        "queued job deduplicated"
    );
    let claimed =
        kima_jobs::queue::claim(&pool, "podcasts", 1, std::time::Duration::from_secs(300))
            .await
            .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        refresh::sweep(&b).await.unwrap(),
        0,
        "running job deduplicated"
    );
    sqlx::query("UPDATE jobs SET state = 'failed' WHERE id = $1")
        .bind(claimed[0].id)
        .execute(&pool)
        .await
        .unwrap();
    // A failed job alone does not permit an immediate retry: the dispatch backoff keys off
    // `refresh_attempted_at`, set at dispatch time regardless of outcome, so this show stays
    // ineligible until that hour elapses (see sweep_running_job_is_not_duplicated_green).
    assert_eq!(
        refresh::sweep(&b).await.unwrap(),
        0,
        "failed job still waits out the dispatch backoff"
    );
    sqlx::query(
        "UPDATE podcasts SET refresh_attempted_at = now() - interval '2 hours' WHERE id = $1",
    )
    .bind(show.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        refresh::sweep(&b).await.unwrap(),
        1,
        "failed job permits retry once the backoff has elapsed"
    );
    assert_eq!(
        refresh::sweep(&b).await.unwrap(),
        0,
        "queued retry deduplicated"
    );
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE queue = 'podcasts' AND state IN ('queued','running')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sweep_running_job_is_not_duplicated_green(pool: PgPool) {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(&pool)
        .await
        .unwrap();
    let Registration::Created(user) =
        users::register(&pool, Uuid::now_v7(), "refresh-user", "hash")
            .await
            .unwrap()
    else {
        panic!("registration failed")
    };
    let show = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://example.invalid/feed",
            title: "Show",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    podcasts::subscribe(&pool, user.id, show.id).await.unwrap();
    sqlx::query("UPDATE podcasts SET last_refreshed = now() - interval '2 hours' WHERE id = $1")
        .bind(show.id)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(refresh::sweep(&pool).await.unwrap(), 1);
    let (fresh,): (bool,) = sqlx::query_as(
        "SELECT last_refreshed > now() - interval '1 minute' FROM podcasts WHERE id = $1",
    )
    .bind(show.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!fresh, "enqueue alone is not a successful refresh");
    let (payload, attempts): (serde_json::Value, i32) =
        sqlx::query_as("SELECT payload, max_attempts FROM jobs WHERE queue = 'podcasts'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload["podcast_id"], show.id.to_string());
    assert_eq!(attempts, 3);
    assert_eq!(refresh::sweep(&pool).await.unwrap(), 0);
    let attempted: bool = sqlx::query_scalar(
        "SELECT refresh_attempted_at > now() - interval '1 minute' FROM podcasts WHERE id = $1",
    )
    .bind(show.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        attempted,
        "dispatch records an attempt without advancing last success"
    );
    let claimed =
        kima_jobs::queue::claim(&pool, "podcasts", 1, std::time::Duration::from_secs(300))
            .await
            .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        refresh::sweep(&pool).await.unwrap(),
        0,
        "running refresh must block another job"
    );
    let other = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://example.invalid/other",
            title: "Other",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    podcasts::subscribe(&pool, user.id, other.id).await.unwrap();
    sqlx::query("UPDATE podcasts SET last_refreshed = now() - interval '2 hours' WHERE id = $1")
        .bind(other.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        refresh::sweep(&pool).await.unwrap(),
        1,
        "a different show must not collide with the queued key"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'podcasts'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);

    // A terminally failed job must not prevent a later sweep from retrying this show.
    sqlx::query("UPDATE jobs SET state = 'failed' WHERE payload ->> 'podcast_id' = $1")
        .bind(other.id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE podcasts SET last_refreshed = now() - interval '2 hours' WHERE id = $1")
        .bind(other.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        refresh::sweep(&pool).await.unwrap(),
        0,
        "failed refresh waits an hour before redispatch"
    );
    sqlx::query(
        "UPDATE podcasts SET refresh_attempted_at = now() - interval '2 hours' WHERE id = $1",
    )
    .bind(other.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(refresh::sweep(&pool).await.unwrap(), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'podcasts'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);
}
