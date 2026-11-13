//! The `podcasts` queue worker (M6 Task 9) through the real job runtime: a feed that answers
//! 404 is gone for good, so its job fails on the first attempt instead of using up its retries.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewShow};
use kima_db::users::{self, Registration};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use kima_server::jobs::{Registry, drain, spawn_all};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn feed_host(status: u16) -> Replay {
    let xml = "<rss version=\"2.0\"><channel><title>Show</title></channel></rss>";
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

/// A followed show on `host`, with a refresh job queued for it.
async fn queued_refresh(pool: &PgPool, host: &Replay) -> Uuid {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(pool)
        .await
        .unwrap();
    let Registration::Created(user) = users::register(pool, Uuid::now_v7(), "listener", "hash")
        .await
        .unwrap()
    else {
        panic!("registration failed")
    };
    let feed_url = format!("{}/feed", host.url());
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &feed_url,
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
    podcasts::subscribe(pool, user.id, show.id).await.unwrap();
    sqlx::query("UPDATE podcasts SET last_refreshed = now() - interval '2 hours' WHERE id = $1")
        .bind(show.id)
        .execute(pool)
        .await
        .unwrap();
    kima_jobs::queue::enqueue(
        pool,
        "podcasts",
        serde_json::json!({"podcast_id": show.id, "key": format!("podcast-{}", show.id)}),
        3,
    )
    .await
    .unwrap()
}

async fn settled(pool: &PgPool, job: Uuid) -> (String, i32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let row: (String, i32) = sqlx::query_as("SELECT state, attempts FROM jobs WHERE id = $1")
            .bind(job)
            .fetch_one(pool)
            .await
            .unwrap();
        if row.0 == "failed" || row.0 == "done" || tokio::time::Instant::now() > deadline {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn gone_feed_fails_its_job_without_retrying(pool: PgPool) {
    let host = feed_host(404).await;
    let job = queued_refresh(&pool, &host).await;
    let mut registry = Registry::new();
    registry.register_podcast_refresh(kima_podcast::ctx::PodcastCtx {
        pool: pool.clone(),
        net: Arc::new(Net::new(
            NetConfig::for_tests(Bases::all(&host.url())),
            pool.clone(),
            IdentifySwitch::new(false, false),
        )),
        data_dir: std::path::PathBuf::new(),
    });
    let shutdown = CancellationToken::new();
    let workers = spawn_all(&pool, registry, shutdown.clone());
    let (state, attempts) = settled(&pool, job).await;
    shutdown.cancel();
    drain(workers, Duration::from_secs(5)).await;
    assert_eq!(state, "failed");
    assert_eq!(attempts, 1, "a gone feed must not use up the job's retries");
    assert_eq!(host.asked().len(), 1, "the feed was asked exactly once");
}
