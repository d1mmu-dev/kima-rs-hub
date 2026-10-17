mod common;

use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use common::{Tags, make_audio, require_ffmpeg};
use kima_core::config::WatchMode;
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_library::jobs;
use kima_library::scan::{self, LibraryCtx, PathOutcome};
use kima_library::watch::{self, IgnoreSet, WatchHandle, WatchStatus};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DEBOUNCE: Duration = Duration::from_millis(200);

struct Harness {
    ctx: LibraryCtx,
    ignore: IgnoreSet,
    shutdown: CancellationToken,
    handle: WatchHandle,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn root(&self) -> &Path {
        &self.ctx.root
    }
}

fn tags(title: &str) -> Tags {
    Tags {
        album_artist: Some("Artist".into()),
        artist: Some("Artist".into()),
        album: Some("Album".into()),
        title: Some(title.into()),
        date: Some("2001".into()),
        track: Some("1".into()),
        ..Default::default()
    }
}

fn start(pool: &PgPool, mode: WatchMode, poll: Duration, debounce: Duration) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let ctx = LibraryCtx {
        pool: pool.clone(),
        root,
        events: EventBus::new(64),
        data_dir: dir.path().join(".kima-data"),
    };
    let ignore = IgnoreSet::new();
    let shutdown = CancellationToken::new();
    let handle = watch::spawn_with(
        ctx.clone(),
        mode,
        poll,
        debounce,
        ignore.clone(),
        Arc::new(RwLock::new(WatchStatus::default())),
        shutdown.clone(),
    );
    Harness {
        ctx,
        ignore,
        shutdown,
        handle,
        _dir: dir,
    }
}

/// Starts the watcher on an empty root, then runs `setup` before it exists. For tests that
/// need rows in place first, index with `update_path` and then call [`start_on`].
fn start_on(pool: &PgPool, dir: tempfile::TempDir, mode: WatchMode, poll: Duration) -> Harness {
    let root = dir.path().canonicalize().unwrap();
    let ctx = LibraryCtx {
        pool: pool.clone(),
        root,
        events: EventBus::new(64),
        data_dir: dir.path().join(".kima-data"),
    };
    let ignore = IgnoreSet::new();
    let shutdown = CancellationToken::new();
    let handle = watch::spawn_with(
        ctx.clone(),
        mode,
        poll,
        DEBOUNCE,
        ignore.clone(),
        Arc::new(RwLock::new(WatchStatus::default())),
        shutdown.clone(),
    );
    Harness {
        ctx,
        ignore,
        shutdown,
        handle,
        _dir: dir,
    }
}

/// Paths of every `path` job ever queued (any state), in queue order.
async fn path_jobs(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT payload ->> 'path' FROM jobs
         WHERE queue = 'library' AND payload ->> 'kind' = 'path' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn count_kind(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE queue = 'library' AND payload ->> 'kind' = $1",
    )
    .bind(kind)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Polls until `check` holds, or fails the test after `secs`.
async fn wait_for<F, Fut>(what: &str, secs: u64, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if check().await {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_path_job(pool: &PgPool, rel: &str) {
    wait_for(&format!("a path job for {rel}"), 15, || async {
        path_jobs(pool).await.iter().any(|p| p == rel)
    })
    .await;
}

/// Runs and clears every queued library job, like the worker would.
async fn drain(ctx: &LibraryCtx) {
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "SELECT id, payload FROM jobs WHERE queue = 'library' AND state = 'queued' ORDER BY id",
    )
    .fetch_all(&ctx.pool)
    .await
    .unwrap();
    for (id, payload) in rows {
        jobs::run(ctx, &payload).await.unwrap();
        sqlx::query("UPDATE jobs SET state = 'done' WHERE id = $1")
            .bind(id)
            .execute(&ctx.pool)
            .await
            .unwrap();
    }
}

async fn title_of(pool: &PgPool, path: &str) -> Option<String> {
    sqlx::query_scalar("SELECT title FROM tracks WHERE path = $1")
        .bind(path)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn track_id(pool: &PgPool, path: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM tracks WHERE path = $1")
        .bind(path)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn is_missing(pool: &PgPool, path: &str) -> bool {
    sqlx::query_scalar("SELECT missing_since IS NOT NULL FROM tracks WHERE path = $1")
        .bind(path)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A tempdir with indexed files, ready for a watcher.
async fn indexed_dir(pool: &PgPool, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let ctx = LibraryCtx {
        pool: pool.clone(),
        root: dir.path().canonicalize().unwrap(),
        events: EventBus::new(8),
        data_dir: dir.path().join(".kima-data"),
    };
    for (rel, title) in files {
        make_audio(dir.path(), rel, tags(title));
        assert_eq!(
            scan::update_path(&ctx, rel).await.unwrap(),
            PathOutcome::Added
        );
    }
    dir
}

fn stop(h: &Harness) {
    h.shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn new_file_is_indexed(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let h = start(&pool, WatchMode::Auto, Duration::from_secs(60), DEBOUNCE);
    assert_eq!(h.handle.status().mode, "auto");
    assert!(h.handle.status().active);
    make_audio(h.root(), "A/B/01.flac", tags("Fresh"));
    wait_path_job(&pool, "A/B/01.flac").await;
    drain(&h.ctx).await;
    assert_eq!(
        title_of(&pool, "A/B/01.flac").await.as_deref(),
        Some("Fresh")
    );
    stop(&h);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn modified_file_updates(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = indexed_dir(&pool, &[("A/B/01.flac", "Old")]).await;
    let h = start_on(&pool, dir, WatchMode::Auto, Duration::from_secs(60));
    make_audio(h.root(), "A/B/01.flac", tags("New title"));
    wait_path_job(&pool, "A/B/01.flac").await;
    drain(&h.ctx).await;
    assert_eq!(
        title_of(&pool, "A/B/01.flac").await.as_deref(),
        Some("New title")
    );
    stop(&h);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deleted_file_marked_missing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = indexed_dir(&pool, &[("A/B/01.flac", "One"), ("A/B/02.flac", "Two")]).await;
    let h = start_on(&pool, dir, WatchMode::Auto, Duration::from_secs(60));
    std::fs::remove_file(h.root().join("A/B/01.flac")).unwrap();
    wait_path_job(&pool, "A/B/01.flac").await;
    drain(&h.ctx).await;
    assert!(is_missing(&pool, "A/B/01.flac").await);
    assert!(!is_missing(&pool, "A/B/02.flac").await);
    stop(&h);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deleted_folder_expands_to_its_files(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = indexed_dir(
        &pool,
        &[
            ("A/B/01.flac", "One"),
            ("A/B/deep/02.flac", "Two"),
            ("C/D/03.flac", "Three"),
        ],
    )
    .await;
    let h = start_on(&pool, dir, WatchMode::Auto, Duration::from_secs(60));
    std::fs::remove_dir_all(h.root().join("A")).unwrap();
    wait_path_job(&pool, "A/B/01.flac").await;
    wait_path_job(&pool, "A/B/deep/02.flac").await;
    drain(&h.ctx).await;
    assert!(is_missing(&pool, "A/B/01.flac").await);
    assert!(is_missing(&pool, "A/B/deep/02.flac").await);
    assert!(!is_missing(&pool, "C/D/03.flac").await);
    stop(&h);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rename_keeps_id(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = indexed_dir(&pool, &[("A/B/01.flac", "One"), ("A/B/02.flac", "Two")]).await;
    let h = start_on(&pool, dir, WatchMode::Auto, Duration::from_secs(60));
    let id = track_id(&pool, "A/B/01.flac").await;
    std::fs::rename(
        h.root().join("A/B/01.flac"),
        h.root().join("A/B/renamed.flac"),
    )
    .unwrap();
    wait_path_job(&pool, "A/B/renamed.flac").await;
    drain(&h.ctx).await;
    assert_eq!(track_id(&pool, "A/B/renamed.flac").await, id);
    assert_eq!(title_of(&pool, "A/B/01.flac").await, None);
    stop(&h);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn renamed_folder_keeps_ids(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = indexed_dir(&pool, &[("A/B/01.flac", "One"), ("A/B/02.flac", "Two")]).await;
    let h = start_on(&pool, dir, WatchMode::Auto, Duration::from_secs(60));
    let id1 = track_id(&pool, "A/B/01.flac").await;
    let id2 = track_id(&pool, "A/B/02.flac").await;
    std::fs::rename(h.root().join("A/B"), h.root().join("A/Renamed")).unwrap();
    wait_path_job(&pool, "A/Renamed/01.flac").await;
    wait_path_job(&pool, "A/Renamed/02.flac").await;
    drain(&h.ctx).await;
    assert_eq!(track_id(&pool, "A/Renamed/01.flac").await, id1);
    assert_eq!(track_id(&pool, "A/Renamed/02.flac").await, id2);
    stop(&h);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ignored_paths_not_enqueued(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let h = start(&pool, WatchMode::Auto, Duration::from_secs(60), DEBOUNCE);
    std::fs::create_dir_all(h.root().join("organizing")).unwrap();
    h.ignore.add(h.root().join("organizing"));
    make_audio(h.root(), "organizing/skipped.flac", tags("Skipped"));
    make_audio(h.root(), ".hidden/skipped.flac", tags("Hidden"));
    make_audio(h.root(), "kept/kept.flac", tags("Kept"));
    wait_path_job(&pool, "kept/kept.flac").await;
    // Long enough for anything wrongly watched to have settled too.
    tokio::time::sleep(DEBOUNCE * 4).await;
    assert_eq!(path_jobs(&pool).await, vec!["kept/kept.flac".to_string()]);
    h.ignore.remove(&h.root().join("organizing"));
    stop(&h);
}

/// Review Focus 1: a file that is still being copied must not be read halfway.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_written_slowly_indexed_once_complete(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let source = make_audio(scratch.path(), "src.flac", tags("Slow"));
    let bytes = std::fs::read(&source).unwrap();
    let h = start(
        &pool,
        WatchMode::Auto,
        Duration::from_secs(60),
        Duration::from_millis(500),
    );

    let target = h.root().join("Slow/Album/01.flac");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&target).unwrap();
        let chunk = bytes.len().div_ceil(5);
        for part in bytes.chunks(chunk) {
            f.write_all(part).unwrap();
            f.flush().unwrap();
            // Shorter than the debounce: the file never goes quiet until the last chunk.
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    // No job may have been queued while chunks were still arriving, so the one job that
    // appears reads the whole file.
    wait_path_job(&pool, "Slow/Album/01.flac").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let queued = path_jobs(&pool).await;
    assert_eq!(
        queued,
        vec!["Slow/Album/01.flac".to_string()],
        "one settled job"
    );

    let c = &h.ctx;
    let outcome = scan::update_path(c, "Slow/Album/01.flac").await.unwrap();
    assert_eq!(outcome, PathOutcome::Added);
    let (size, duration): (i64, i32) =
        sqlx::query_as("SELECT size, duration_secs FROM tracks WHERE path = 'Slow/Album/01.flac'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(size as usize, bytes.len());
    assert_eq!(duration, 2);
    stop(&h);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn poll_mode_sees_changes(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let h = start(&pool, WatchMode::Poll, Duration::from_secs(1), DEBOUNCE);
    assert_eq!(h.handle.status().mode, "poll");
    assert!(h.handle.status().active);
    make_audio(h.root(), "P/Q/01.flac", tags("Polled"));
    wait_path_job(&pool, "P/Q/01.flac").await;
    drain(&h.ctx).await;
    assert_eq!(
        title_of(&pool, "P/Q/01.flac").await.as_deref(),
        Some("Polled")
    );
    stop(&h);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn off_mode_watches_nothing(pool: PgPool) {
    let h = start(&pool, WatchMode::Off, Duration::from_secs(60), DEBOUNCE);
    let status = h.handle.status();
    assert_eq!(status.mode, "off");
    assert!(!status.active);
    std::fs::write(h.root().join("x.flac"), b"not audio").unwrap();
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert!(path_jobs(&pool).await.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_root_reports_an_error_and_falls_back(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("not-there");
    let ctx = LibraryCtx {
        pool: pool.clone(),
        root: gone,
        events: EventBus::new(8),
        data_dir: dir.path().join(".kima-data"),
    };
    let shutdown = CancellationToken::new();
    let handle = watch::spawn_with(
        ctx,
        WatchMode::Auto,
        Duration::from_secs(60),
        DEBOUNCE,
        IgnoreSet::new(),
        Arc::new(RwLock::new(WatchStatus::default())),
        shutdown.clone(),
    );
    let status = handle.status();
    assert!(!status.active);
    assert!(status.last_error.is_some());
    assert_eq!(count_kind(&pool, "reconcile").await, 0);
    shutdown.cancel();
}

#[test]
fn ignore_set_covers_paths_below() {
    let set = IgnoreSet::new();
    set.add("/lib/a");
    assert!(set.contains(Path::new("/lib/a")));
    assert!(set.contains(Path::new("/lib/a/b/c.flac")));
    assert!(!set.contains(Path::new("/lib/ab")));
    set.remove(Path::new("/lib/a"));
    assert!(!set.contains(Path::new("/lib/a")));
}
