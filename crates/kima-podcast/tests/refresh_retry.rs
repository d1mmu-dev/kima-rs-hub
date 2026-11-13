//! Task 9 follow-up: failed podcast refreshes must be exposed to, and retriable from, the
//! enrichment control plane's "retry failed and blocked" action, not only the hourly sweep.

use kima_db::podcasts::{self, NewShow};
use kima_db::users::{self, Registration};
use kima_podcast::refresh;
use sqlx::PgPool;
use uuid::Uuid;

async fn show(pool: &PgPool, feed_url: &str) -> Uuid {
    podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url,
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
    .unwrap()
    .id
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retry_failed_requeues_failed_shows_ignoring_the_sweep_backoff(pool: PgPool) {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(&pool)
        .await
        .unwrap();
    let Registration::Created(user) = users::register(&pool, Uuid::now_v7(), "retry-user", "hash")
        .await
        .unwrap()
    else {
        panic!("registration failed")
    };
    let failed = show(&pool, "https://example.invalid/failed").await;
    podcasts::subscribe(&pool, user.id, failed).await.unwrap();
    // A recent failed attempt: inside the sweep's hourly backoff window, but a failure.
    sqlx::query(
        "UPDATE podcasts SET refresh_error = 'boom', refresh_failed_at = now(),\n         refresh_failures = 1, refresh_attempted_at = now() WHERE id = $1",
    )
    .bind(failed)
    .execute(&pool)
    .await
    .unwrap();

    let healthy = show(&pool, "https://example.invalid/healthy").await;
    podcasts::subscribe(&pool, user.id, healthy).await.unwrap();

    let not_followed = show(&pool, "https://example.invalid/not-followed").await;
    sqlx::query(
        "UPDATE podcasts SET refresh_error = 'boom', refresh_failed_at = now(),\n         refresh_failures = 1 WHERE id = $1",
    )
    .bind(not_followed)
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(
        refresh::sweep(&pool).await.unwrap(),
        0,
        "the failed show is still inside its backoff window"
    );

    let queued = refresh::retry_failed(&pool).await.unwrap();
    assert_eq!(queued, 1, "only the followed, failed show is requeued");

    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE queue = 'podcasts'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload["podcast_id"], failed.to_string());

    // Calling it again while the job is still queued must not double-enqueue.
    assert_eq!(refresh::retry_failed(&pool).await.unwrap(), 0);
}
