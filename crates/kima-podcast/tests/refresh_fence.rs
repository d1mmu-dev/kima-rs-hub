use base64::Engine;
use kima_core::identify::IdentifySwitch;
use kima_db::podcasts::{self, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use sqlx::PgPool;
use uuid::Uuid;

async fn server(status: u16, title: &str) -> Replay {
    let xml = format!(
        "<rss version=\"2.0\"><channel><title>{title}</title><item><guid>{title}</guid><title>Episode</title><enclosure url=\"https://example.org/a.mp3\" type=\"audio/mpeg\"/></item></channel></rss>"
    );
    Replay::start(Recording(vec![Entry {
        method: "GET".into(),
        url: "/feed".into(),
        status,
        headers: Default::default(),
        body: serde_json::Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(xml)),
        request_body_sha256: None,
    }]))
    .await
}

fn net(pool: &PgPool, replay: &Replay) -> Net {
    Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    )
}

fn catalog(pool: &PgPool, replay: &Replay) -> kima_podcast::catalog::Catalog {
    kima_podcast::catalog::Catalog::new(std::sync::Arc::new(net(pool, replay)), pool.clone())
}

async fn insert(pool: &PgPool, replay: &Replay) -> Uuid {
    let id = Uuid::now_v7();
    podcasts::insert_show(
        pool,
        id,
        &NewShow {
            feed_url: &format!("{}/feed", replay.url()),
            title: "Original",
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
    id
}

async fn wait_arrival(replay: &Replay) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while replay.arrived().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request must reach barrier");
}

async fn change_server(pool: &PgPool, id: Uuid, replay: &Replay) {
    sqlx::query("UPDATE podcasts SET feed_url = $2 WHERE id = $1")
        .bind(id)
        .bind(format!("{}/feed", replay.url()))
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn superseded_response_is_typed(pool: PgPool) {
    let old = server(200, "Old").await;
    let held = old.hold("/feed");
    let id = insert(&pool, &old).await;
    let old_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let new_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let old_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&old_pool)
        .await
        .unwrap();
    let new_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&new_pool)
        .await
        .unwrap();
    assert_ne!(old_pid, new_pid);
    let old_net = net(&old_pool, &old);
    let task =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&old_pool, &old_net, id).await },
        );
    wait_arrival(&old).await;
    let new = server(200, "New").await;
    change_server(&new_pool, id, &new).await;
    kima_podcast::refresh::refresh_one(&new_pool, &net(&new_pool, &new), id)
        .await
        .unwrap();
    held.release();
    let result = task.await.unwrap();
    assert!(
        matches!(
            result,
            Ok(kima_podcast::refresh::RefreshOutcome::Superseded)
        ),
        "{result:?}"
    );
    assert_eq!(
        podcasts::show_by_id(&new_pool, id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "New"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn old_200_is_fenced_after_new_200(pool: PgPool) {
    let old = server(200, "Old").await;
    let held = old.hold("/feed");
    let id = insert(&pool, &old).await;
    let old_net = net(&pool, &old);
    let old_pool = pool.clone();
    let task =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&old_pool, &old_net, id).await },
        );
    wait_arrival(&old).await;
    let new = server(200, "New").await;
    change_server(&pool, id, &new).await;
    kima_podcast::refresh::refresh_one(&pool, &net(&pool, &new), id)
        .await
        .unwrap();
    held.release();
    task.await.unwrap().unwrap();
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.title, "New");
    let guids: Vec<String> =
        sqlx::query_scalar("SELECT guid FROM podcast_episodes WHERE podcast_id = $1 ORDER BY guid")
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(guids, vec!["New"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn superseded_failure_does_not_clobber_new_success(pool: PgPool) {
    let old = server(500, "Old").await;
    let held = old.hold("/feed");
    let id = insert(&pool, &old).await;
    let old_net = net(&pool, &old);
    let old_pool = pool.clone();
    let task =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&old_pool, &old_net, id).await },
        );
    wait_arrival(&old).await;
    let new = server(200, "New").await;
    change_server(&pool, id, &new).await;
    kima_podcast::refresh::refresh_one(&pool, &net(&pool, &new), id)
        .await
        .unwrap();
    held.release();
    let result = task.await.unwrap();
    assert!(
        matches!(
            result,
            Ok(kima_podcast::refresh::RefreshOutcome::Superseded)
        ),
        "{result:?}"
    );
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.title, "New");
    assert_eq!(show.refresh_error, None);
    assert_eq!(show.refresh_failures, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cancelled_new_start_still_fences_old_response(pool: PgPool) {
    let old = server(200, "Old").await;
    let old_hold = old.hold("/feed");
    let id = insert(&pool, &old).await;
    let old_net = net(&pool, &old);
    let old_pool = pool.clone();
    let old_task =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&old_pool, &old_net, id).await },
        );
    wait_arrival(&old).await;
    let new = server(200, "New").await;
    let _new_hold = new.hold("/feed");
    change_server(&pool, id, &new).await;
    let new_net = net(&pool, &new);
    let new_pool = pool.clone();
    let new_task =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&new_pool, &new_net, id).await },
        );
    wait_arrival(&new).await;
    new_task.abort();
    assert!(new_task.await.unwrap_err().is_cancelled());
    old_hold.release();
    old_task.await.unwrap().unwrap();
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.title, "Original");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM podcast_episodes WHERE podcast_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn same_url_old_response_is_fenced(pool: PgPool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/feed", listener.local_addr().unwrap());
    let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let mut buf = [0; 4096];
        assert!(first.read(&mut buf).await.unwrap() > 0);
        arrived_tx.send(()).unwrap();
        let (mut second, _) = listener.accept().await.unwrap();
        assert!(second.read(&mut buf).await.unwrap() > 0);
        let response = |title: &str| {
            let body = format!(
                "<rss version=\"2.0\"><channel><title>{title}</title><item><guid>{title}</guid><title>E</title><enclosure url=\"https://example.org/a.mp3\" type=\"audio/mpeg\"/></item></channel></rss>"
            );
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        };
        second.write_all(response("New").as_bytes()).await.unwrap();
        release_rx.await.unwrap();
        first.write_all(response("Old").as_bytes()).await.unwrap();
    });
    let initial = server(200, "unused").await;
    let id = insert(&pool, &initial).await;
    sqlx::query("UPDATE podcasts SET feed_url = $2 WHERE id = $1")
        .bind(id)
        .bind(&url)
        .execute(&pool)
        .await
        .unwrap();
    let client = std::sync::Arc::new(Net::new(
        NetConfig::for_tests(Bases::all(&url)),
        pool.clone(),
        IdentifySwitch::new(false, false),
    ));
    let old_pool = pool.clone();
    let old_net = client.clone();
    let old =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&old_pool, &old_net, id).await },
        );
    tokio::time::timeout(std::time::Duration::from_secs(10), arrived_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        kima_podcast::refresh::refresh_one(&pool, &client, id)
            .await
            .unwrap(),
        kima_podcast::refresh::RefreshOutcome::Applied(_)
    ));
    release_tx.send(()).unwrap();
    assert!(matches!(
        old.await.unwrap().unwrap(),
        kima_podcast::refresh::RefreshOutcome::Superseded
    ));
    server_task.await.unwrap();
    assert_eq!(
        podcasts::show_by_id(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "New"
    );
    let guids: Vec<String> =
        sqlx::query_scalar("SELECT guid FROM podcast_episodes WHERE podcast_id = $1 ORDER BY guid")
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(guids, vec!["New"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn publication_rechecks_token_after_postgres_lock_wait(pool: PgPool) {
    let replay = server(200, "Old").await;
    let held = replay.hold("/feed");
    let id = insert(&pool, &replay).await;
    let worker = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&worker)
        .await
        .unwrap();
    let worker_net = net(&worker, &replay);
    let task =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&worker, &worker_net, id).await },
        );
    wait_arrival(&replay).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE podcasts SET refresh_token = $2 WHERE id = $1")
        .bind(id)
        .bind(Uuid::now_v7())
        .execute(&mut *tx)
        .await
        .unwrap();
    held.release();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock')")
                .bind(pid).fetch_one(&pool).await.unwrap();
            if blocked { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("publisher must wait on a PostgreSQL lock");
    tx.commit().await.unwrap();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        kima_podcast::refresh::RefreshOutcome::Superseded
    ));
    assert_eq!(
        podcasts::show_by_id(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Original"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM podcast_episodes WHERE podcast_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deleted_after_start_cannot_publish(pool: PgPool) {
    let replay = server(200, "Deleted").await;
    let held = replay.hold("/feed");
    let id = insert(&pool, &replay).await;
    let task_pool = pool.clone();
    let task_net = net(&pool, &replay);
    let task =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&task_pool, &task_net, id).await },
        );
    wait_arrival(&replay).await;
    sqlx::query("DELETE FROM podcasts WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    held.release();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        kima_podcast::refresh::RefreshOutcome::Missing
    ));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM podcast_episodes WHERE podcast_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// Review finding 5: a `Gone` answer from an attempt that a newer refresh has already
/// overtaken is stale news and must answer `Superseded`, like every other failure, instead of
/// telling the caller the feed is gone while the show was just refreshed successfully.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn old_404_is_superseded_after_new_200(pool: PgPool) {
    let old = server(404, "Old").await;
    let held = old.hold("/feed");
    let id = insert(&pool, &old).await;
    let old_net = net(&pool, &old);
    let old_pool = pool.clone();
    let task =
        tokio::spawn(
            async move { kima_podcast::refresh::refresh_one(&old_pool, &old_net, id).await },
        );
    wait_arrival(&old).await;
    let new = server(200, "New").await;
    change_server(&pool, id, &new).await;
    kima_podcast::refresh::refresh_one(&pool, &net(&pool, &new), id)
        .await
        .unwrap();
    held.release();
    let result = task.await.unwrap();
    assert!(
        matches!(
            result,
            Ok(kima_podcast::refresh::RefreshOutcome::Superseded)
        ),
        "{result:?}"
    );
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.title, "New");
    assert_eq!(show.refresh_failures, 0);
}

/// Review finding 1: a queued job (or its delayed retry) that starts after a newer manual
/// refresh already succeeded must not run: its intent is older than that success. It answers
/// `Superseded`, never asks the feed and records no failure.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn job_queued_before_a_later_success_does_not_run(pool: PgPool) {
    let good = server(200, "Manual").await;
    let id = insert(&pool, &good).await;
    let job = kima_jobs::queue::enqueue(
        &pool,
        "podcasts",
        serde_json::json!({"podcast_id": id, "key": format!("podcast-{id}")}),
        3,
    )
    .await
    .unwrap();
    // The person refreshes by hand after the job was queued; it succeeds.
    kima_podcast::refresh::refresh_one(&pool, &net(&pool, &good), id)
        .await
        .unwrap();
    // The feed is down by the time the old job runs.
    let down = server(500, "Down").await;
    change_server(&pool, id, &down).await;
    let result = kima_podcast::refresh::refresh_job(&catalog(&pool, &down), id, job).await;
    assert!(
        matches!(
            result,
            Ok(kima_podcast::refresh::RefreshOutcome::Superseded)
        ),
        "{result:?}"
    );
    assert!(down.asked().is_empty(), "the stale job must not fetch");
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.title, "Manual");
    assert_eq!(show.refresh_failures, 0);
}

/// The same job runs normally when nothing succeeded after it was queued.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn job_without_a_later_success_runs(pool: PgPool) {
    let good = server(200, "Fresh").await;
    let id = insert(&pool, &good).await;
    sqlx::query("UPDATE podcasts SET last_refreshed = now() - interval '2 hours' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let job = kima_jobs::queue::enqueue(
        &pool,
        "podcasts",
        serde_json::json!({"podcast_id": id, "key": format!("podcast-{id}")}),
        3,
    )
    .await
    .unwrap();
    let result = kima_podcast::refresh::refresh_job(&catalog(&pool, &good), id, job).await;
    assert!(
        matches!(
            result,
            Ok(kima_podcast::refresh::RefreshOutcome::Applied(_))
        ),
        "{result:?}"
    );
    assert_eq!(good.asked().len(), 1);
}
