use std::time::Duration;

use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_jobs::queue;
use kima_library::scan::LibraryCtx;
use kima_server::jobs::{Registry, drain, spawn_all};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn wait_for_state(pool: &PgPool, id: Uuid, want: &str) -> (String, Option<String>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let row: (String, Option<String>) =
            sqlx::query_as("SELECT state, last_error FROM jobs WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        if row.0 == want || tokio::time::Instant::now() > deadline {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_queue_runs_reconcile_and_path_jobs(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("readme.txt"), "not audio").unwrap();
    let events = EventBus::new(64);
    let mut rx = events.subscribe();
    let mut registry = Registry::new();
    registry.register_library(LibraryCtx {
        pool: pool.clone(),
        root: root.path().to_path_buf(),
        events,
        data_dir: root.path().join(".data"),
    });
    let shutdown = CancellationToken::new();
    let workers = spawn_all(&pool, registry, shutdown.clone());

    let reconcile = kima_library::jobs::enqueue_reconcile(&pool)
        .await
        .unwrap()
        .expect("nothing queued yet");
    let path = kima_library::jobs::enqueue_path(&pool, "gone.mp3")
        .await
        .unwrap();
    let organize = queue::enqueue(&pool, "library", json!({"kind": "organize"}), 1)
        .await
        .unwrap();

    assert_eq!(wait_for_state(&pool, reconcile, "done").await.0, "done");
    assert_eq!(wait_for_state(&pool, path, "done").await.0, "done");
    let (state, error) = wait_for_state(&pool, organize, "failed").await;
    assert_eq!(state, "failed");
    assert!(error.unwrap().contains("organize"));

    let ev = rx.recv().await.unwrap();
    assert_eq!(ev.data["type"], "library:progress");
    assert_eq!(ev.data["job"], "scan");

    shutdown.cancel();
    drain(workers, Duration::from_secs(5)).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn startup_reconcile_is_not_queued_twice(pool: PgPool) {
    assert!(
        kima_library::jobs::enqueue_reconcile(&pool)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        kima_library::jobs::enqueue_reconcile(&pool)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_root_setting_overrides_music_path(pool: PgPool) {
    let music = std::path::Path::new("/music");
    assert_eq!(
        kima_library::scan::effective_root(&pool, music)
            .await
            .unwrap(),
        music
    );
    kima_db::settings::update(
        &pool,
        kima_db::settings::SettingsPatch {
            library_root: Some("/organized".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        kima_library::scan::effective_root(&pool, music)
            .await
            .unwrap(),
        std::path::Path::new("/organized")
    );
}
