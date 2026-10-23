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
        root: root.path().to_path_buf().into(),
        events,
        data_dir: root.path().join(".data"),
        ignore: Default::default(),
    });
    let shutdown = CancellationToken::new();
    let workers = spawn_all(&pool, registry, shutdown.clone());

    let reconcile = kima_library::jobs::enqueue_reconcile(&pool)
        .await
        .unwrap()
        .expect("nothing queued yet");
    let path = kima_library::jobs::enqueue_path(&pool, "gone.mp3")
        .await
        .unwrap()
        .expect("queued");
    let organize = queue::enqueue(&pool, "library", json!({"kind": "organize"}), 1)
        .await
        .unwrap();

    assert_eq!(wait_for_state(&pool, reconcile, "done").await.0, "done");
    assert_eq!(wait_for_state(&pool, path, "done").await.0, "done");
    let (state, error) = wait_for_state(&pool, organize, "failed").await;
    assert_eq!(state, "failed");
    // An organize payload without a run id is malformed.
    assert!(error.unwrap().contains("invalid library job payload"));

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

/// A reconcile left `running` by a killed process must not block the startup reconcile for
/// its lease: startup requeues it (instead of adding a second one).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn startup_requeues_a_reconcile_left_running(pool: PgPool) {
    let stuck = kima_library::jobs::enqueue_reconcile(&pool)
        .await
        .unwrap()
        .unwrap();
    queue::claim(&pool, "library", 1, Duration::from_secs(300))
        .await
        .unwrap();
    kima_server::jobs::prepare_library_queue(&pool)
        .await
        .unwrap();
    let rows: Vec<(Uuid, String, bool)> =
        sqlx::query_as("SELECT id, state, run_at <= now() FROM jobs WHERE queue = 'library'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![(stuck, "queued".to_string(), true)]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn startup_queues_a_reconcile_when_none_is_pending(pool: PgPool) {
    kima_server::jobs::prepare_library_queue(&pool)
        .await
        .unwrap();
    kima_server::jobs::prepare_library_queue(&pool)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE queue = 'library' AND payload->>'kind' = 'reconcile'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_queue_uses_default_lease(pool: PgPool) {
    let mut registry = Registry::new();
    registry.register_library(LibraryCtx {
        pool: pool.clone(),
        root: std::path::PathBuf::from("/music").into(),
        events: EventBus::new(4),
        data_dir: "/data".into(),
        ignore: Default::default(),
    });
    let config = registry.config("library").unwrap();
    let default = kima_jobs::worker::WorkerConfig::new("library", 1);
    assert_eq!(config.concurrency, 1);
    assert_eq!(config.lease, default.lease);
    assert_eq!(config.heartbeat, default.heartbeat);
}

/// Review Focus 4, at startup: a run left `applying` whose job is gone (it died on its final
/// attempt) gets a new apply job; one whose job was merely requeued does not get a second.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn startup_resumes_interrupted_organizer_runs(pool: PgPool) {
    use kima_db::organizer;
    use kima_library::organize::apply::{self, Action};
    let orphan = organizer::create_run(&pool, Uuid::now_v7(), "move", "/m", "/m", None)
        .await
        .unwrap();
    let with_job = organizer::create_run(&pool, Uuid::now_v7(), "move", "/m", "/m", None)
        .await
        .unwrap();
    for run in [orphan.id, with_job.id] {
        assert!(
            organizer::transition(&pool, run, &["planning"], "applying")
                .await
                .unwrap()
        );
    }
    apply::enqueue(&pool, with_job.id, Action::Apply)
        .await
        .unwrap();
    kima_server::jobs::prepare_library_queue(&pool)
        .await
        .unwrap();
    kima_server::jobs::prepare_library_queue(&pool)
        .await
        .unwrap();
    let jobs: Vec<(String, String)> = sqlx::query_as(
        "SELECT payload ->> 'runId', payload ->> 'action' FROM jobs
         WHERE payload ->> 'kind' = 'organize' ORDER BY payload ->> 'runId'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut want = vec![
        (orphan.id.to_string(), "apply".to_string()),
        (with_job.id.to_string(), "apply".to_string()),
    ];
    want.sort();
    assert_eq!(jobs, want);
}

async fn enrich_jobs(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'enrich'")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Runs one reconcile through the registered `library` worker and waits for it.
async fn reconcile_once(pool: &PgPool, root: &std::path::Path) {
    let mut registry = Registry::new();
    registry.register_library(LibraryCtx {
        pool: pool.clone(),
        root: root.to_path_buf().into(),
        events: EventBus::new(64),
        data_dir: root.join(".data"),
        ignore: Default::default(),
    });
    let shutdown = CancellationToken::new();
    let workers = spawn_all(pool, registry, shutdown.clone());
    let id = kima_library::jobs::enqueue_reconcile(pool)
        .await
        .unwrap()
        .expect("nothing queued");
    assert_eq!(wait_for_state(pool, id, "done").await.0, "done");
    shutdown.cancel();
    drain(workers, Duration::from_secs(5)).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reconcile_with_changes_kicks(pool: PgPool) {
    if !kima_test_support::require_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    kima_test_support::make_audio(root.path(), "A/B/01.flac", kima_test_support::Tags::full());
    reconcile_once(&pool, root.path()).await;
    assert_eq!(enrich_jobs(&pool).await, 1, "the reconcile added a track");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unchanged_reconcile_does_not_kick(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("readme.txt"), "not audio").unwrap();
    reconcile_once(&pool, root.path()).await;
    assert_eq!(enrich_jobs(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn enrich_queue_registered_with_concurrency_one(pool: PgPool) {
    let ctx = kima_ident::IdentCtx {
        pool: pool.clone(),
        net: std::sync::Arc::new(kima_net::Net::disabled()),
        cipher: std::sync::Arc::new(kima_core::cipher::Cipher::from_secret("t")),
        events: EventBus::new(4),
        progress_events: kima_ident::events::ProgressEvents::new(EventBus::new(4)),
        library: LibraryCtx {
            pool: pool.clone(),
            root: std::path::PathBuf::from("/music").into(),
            events: EventBus::new(4),
            data_dir: "/data".into(),
            ignore: Default::default(),
        },
        settings: kima_db::settings_kv::SettingsCache::new(
            pool.clone(),
            std::sync::Arc::new(kima_core::cipher::Cipher::from_secret("t")),
        ),
        config: kima_ident::IdentConfig::default(),
        last_cycle: Default::default(),
    };
    let mut registry = Registry::new();
    registry.register_enrich(ctx);
    let config = registry.config("enrich").unwrap();
    let default = kima_jobs::worker::WorkerConfig::new("enrich", 1);
    assert_eq!(config.concurrency, 1);
    assert_eq!(config.lease, default.lease);
}

fn config(sync: Option<&str>) -> kima_core::config::Config {
    kima_core::config::Config::from_lookup(|k| match k {
        "DATABASE_URL" => Some("postgres://unused".into()),
        "KIMA_LIBRARY_SYNC_CRON" => sync.map(str::to_string),
        _ => None,
    })
    .unwrap()
}

#[test]
fn schedule_has_prune_and_kick_always() {
    let names = |sync: Option<kima_jobs::schedule::Cron>, cfg| {
        kima_server::schedule_entries(&cfg, sync)
            .into_iter()
            .map(|(name, _, _)| name)
            .collect::<Vec<_>>()
    };
    // Sync off still gives the two always-on entries.
    assert_eq!(
        names(None, config(Some("off"))),
        vec!["http-cache-prune", "enrich-kick"]
    );
    let cron = kima_jobs::schedule::Cron::parse("0 4 * * *").unwrap();
    assert_eq!(
        names(Some(cron), config(None)),
        vec!["library-sync", "http-cache-prune", "enrich-kick"]
    );
    // The fire times: 03:17 daily, every ten minutes.
    let entries = kima_server::schedule_entries(&config(Some("off")), None);
    let t = time::macros::datetime!(2026-01-01 00:00:00 UTC);
    assert_eq!(
        entries[0].1.next_after(t),
        Some(time::macros::datetime!(2026-01-01 03:17:00 UTC))
    );
    assert_eq!(
        entries[1].1.next_after(t),
        Some(time::macros::datetime!(2026-01-01 00:10:00 UTC))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn schedule_entries_prune_and_kick(pool: PgPool) {
    let entries = kima_server::schedule_entries(&config(Some("off")), None);
    sqlx::query(
        "INSERT INTO http_cache (key, source, request, outcome, body, expires_at)
         VALUES ('k', 'musicbrainz', 'r', 'answered', '{}', now() - interval '1 day')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO genres (id, name, key) VALUES ($1, 'Rock', 'rock')")
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    (entries[0].2)(pool.clone()).await.unwrap();
    let left: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM http_cache), (SELECT count(*) FROM genres)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left, (0, 0));
    (entries[1].2)(pool.clone()).await.unwrap();
    assert_eq!(enrich_jobs(&pool).await, 1);
}
