//! The fingerprint stage (spec §9, §10.3-§10.4): reuse, decode and store, the failure
//! classes, stale reports, over-cap redeliveries and cancellation.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{dead_bases, ident_ctx, track};
use kima_db::PgPool;
use kima_db::stages::{self, Stage};
use kima_ident::stages::fingerprint::{self, ClaimResult};
use kima_ident::stages::{BatchReport, REDELIVERY_CAP, classify};
use kima_test_support::{Tags, make_audio, require_ffmpeg};
use sqlx::Row;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn stage_row(pool: &PgPool, t: Uuid) -> sqlx::postgres::PgRow {
    sqlx::query("SELECT * FROM track_stages WHERE track_id = $1 AND stage = 'fingerprint'")
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn acoustid_row(pool: &PgPool, t: Uuid) -> Option<(String, i32)> {
    kima_db::identity::fingerprint_of(pool, t).await.unwrap()
}

/// Copies the 30 s chromaprint fixture to `root/rel` (a 2 s tone is too short to fingerprint).
fn tone(root: &Path, rel: &str) {
    let out = root.join(rel);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/chromaprint/tone.wav");
    std::fs::copy(fixture, out).unwrap();
}

fn root(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().canonicalize().unwrap()
}

async fn run(ctx: &kima_ident::IdentCtx) -> BatchReport {
    fingerprint::run_batch(ctx, &CancellationToken::new())
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stored_fingerprint_reused_without_decoding(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ident_ctx(&pool, &root(&dir), dead_bases());
    // The file never exists: reuse must not touch it.
    let t = track(&pool, "gone.flac", 120).await;
    let mut conn = pool.acquire().await.unwrap();
    kima_db::identity::store_fingerprint(&mut conn, t, "AQAAabc", 120)
        .await
        .unwrap();
    drop(conn);

    let report = run(&ctx).await;
    assert_eq!((report.claimed, report.done, report.failed), (1, 1, 0));
    let row = stage_row(&pool, t).await;
    assert_eq!(row.get::<String, _>("status"), "done");
    assert_eq!(
        row.get::<serde_json::Value, _>("evidence"),
        serde_json::json!({"reason": "fingerprint already stored", "characters": 7})
    );
    assert_eq!(acoustid_row(&pool, t).await.unwrap().0, "AQAAabc");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn new_track_fingerprinted_and_stored(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let ctx = ident_ctx(&pool, &root, dead_bases());
    let mut ids = Vec::new();
    for (i, dur) in [(0, 2), (1, 7), (2, 11)] {
        let rel = format!("A/t{i}.wav");
        tone(&root, &rel);
        // tracks.duration_secs, not the file's 2 s, is what gets stored.
        ids.push((track(&pool, &rel, dur).await, dur));
    }

    let report = run(&ctx).await;
    assert_eq!((report.claimed, report.done, report.failed), (3, 3, 0));
    for (t, dur) in ids {
        let row = stage_row(&pool, t).await;
        assert_eq!(row.get::<String, _>("status"), "done");
        assert_eq!(
            row.get::<Option<String>, _>("source").as_deref(),
            Some("ffmpeg")
        );
        let (fp, stored_dur) = acoustid_row(&pool, t).await.expect("fingerprint stored");
        assert!(!fp.is_empty());
        assert_eq!(stored_dur, dur);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn zero_duration_is_no_duration(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ident_ctx(&pool, &root(&dir), dead_bases());
    let t = track(&pool, "x.flac", 0).await;

    let report = run(&ctx).await;
    assert_eq!((report.claimed, report.done, report.failed), (1, 0, 1));
    let row = stage_row(&pool, t).await;
    assert_eq!(row.get::<String, _>("status"), "failed");
    assert_eq!(
        row.get::<Option<String>, _>("error_code").as_deref(),
        Some("NO_DURATION")
    );
    assert_eq!(
        row.get::<Option<String>, _>("error_class").as_deref(),
        Some("PERMANENT")
    );
    assert!(
        row.get::<Option<time::OffsetDateTime>, _>("next_attempt_at")
            .is_none()
    );
}

async fn assert_file_missing(pool: &PgPool, t: Uuid) {
    let row = stage_row(pool, t).await;
    assert_eq!(row.get::<String, _>("status"), "failed");
    assert_eq!(
        row.get::<Option<String>, _>("error_class").as_deref(),
        Some("FILE_MISSING")
    );
    assert!(
        row.get::<Option<time::OffsetDateTime>, _>("next_attempt_at")
            .is_none()
    );
    assert!(acoustid_row(pool, t).await.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_file_is_file_missing(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ident_ctx(&pool, &root(&dir), dead_bases());
    let t = track(&pool, "A/nothing-here.flac", 100).await;
    let report = run(&ctx).await;
    assert_eq!((report.claimed, report.failed), (1, 1));
    assert_file_missing(&pool, t).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn escaping_path_is_file_missing(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir).join("lib");
    std::fs::create_dir_all(&root).unwrap();
    // The file exists, just outside the root.
    std::fs::write(root.parent().unwrap().join("x.flac"), b"not audio").unwrap();
    let ctx = ident_ctx(&pool, &root, dead_bases());
    let t = track(&pool, "../x.flac", 100).await;
    let report = run(&ctx).await;
    assert_eq!((report.claimed, report.failed), (1, 1));
    assert_file_missing(&pool, t).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn permission_denied_is_not_file_missing(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let locked = root.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("x.flac"), b"whatever").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::metadata(locked.join("x.flac")).is_ok() {
        // Root (or CAP_DAC_OVERRIDE) reads through the mode bits; nothing to test.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("permissions not enforced; skipping");
        return;
    }
    let ctx = ident_ctx(&pool, &root, dead_bases());
    let t = track(&pool, "locked/x.flac", 100).await;
    let report = run(&ctx).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!((report.claimed, report.failed), (1, 1));
    let row = stage_row(&pool, t).await;
    assert_eq!(row.get::<String, _>("status"), "failed");
    let class = row.get::<Option<String>, _>("error_class").unwrap();
    assert_ne!(class, "FILE_MISSING");
    let msg = row.get::<Option<String>, _>("last_error").unwrap();
    assert_eq!(class, classify(&msg).as_str(), "{msg}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn decode_failure_classified(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let file = make_audio(&root, "bad.flac", Tags::default());
    let bytes = std::fs::read(&file).unwrap();
    std::fs::write(&file, &bytes[..50]).unwrap();
    let ctx = ident_ctx(&pool, &root, dead_bases());
    let t = track(&pool, "bad.flac", 2).await;

    let report = run(&ctx).await;
    assert_eq!((report.claimed, report.done, report.failed), (1, 0, 1));
    let row = stage_row(&pool, t).await;
    assert_eq!(row.get::<String, _>("status"), "failed");
    let msg = row.get::<Option<String>, _>("last_error").unwrap();
    assert!(!msg.is_empty());
    let class = row.get::<Option<String>, _>("error_class").unwrap();
    assert_eq!(class, classify(&msg).as_str(), "{msg}");
    assert_ne!(class, "FILE_MISSING");
    assert!(acoustid_row(&pool, t).await.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_claim_report_dropped(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    tone(&root, "x.wav");
    let ctx = ident_ctx(&pool, &root, dead_bases());
    let t = track(&pool, "x.wav", 30).await;

    let claim = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .pop()
        .unwrap();
    // The file changed meanwhile: the claim is no longer current.
    let mut conn = pool.acquire().await.unwrap();
    stages::reopen_for_changed_file(&mut conn, t).await.unwrap();
    drop(conn);

    let res = fingerprint::run_claim(&ctx, &claim, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(res, ClaimResult::Stale);
    // Neither the fingerprint nor the report was written.
    assert!(acoustid_row(&pool, t).await.is_none());
    assert_eq!(
        stage_row(&pool, t).await.get::<String, _>("status"),
        "pending"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn over_cap_redelivery_closed_without_work(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ident_ctx(&pool, &root(&dir), dead_bases());
    // Had the stage run, this would close FILE_MISSING.
    let t = track(&pool, "nowhere.flac", 100).await;
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, redelivered, next_attempt_at, \
         started_at, updated_at) \
         VALUES ($1, 'fingerprint', 'working', $2, now() - interval '1 second', \
         now() - interval '16 minutes', now())",
    )
    .bind(t)
    .bind(REDELIVERY_CAP - 1)
    .execute(&pool)
    .await
    .unwrap();

    let report = run(&ctx).await;
    assert_eq!((report.claimed, report.done, report.failed), (1, 0, 1));
    let row = stage_row(&pool, t).await;
    assert_eq!(row.get::<i16, _>("redelivered"), REDELIVERY_CAP);
    assert_eq!(row.get::<String, _>("status"), "failed");
    assert_eq!(
        row.get::<Option<String>, _>("error_code").as_deref(),
        Some("LOST_WORKER")
    );
    assert_eq!(
        row.get::<Option<String>, _>("error_class").as_deref(),
        Some("INCONCLUSIVE")
    );
    assert!(
        row.get::<Option<time::OffsetDateTime>, _>("next_attempt_at")
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cancel_abandons_claims_working(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    // A FIFO with no writer: ffmpeg blocks opening it until killed.
    let fifo = root.join("stuck.flac");
    let ok = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let ctx = ident_ctx(&pool, &root, dead_bases());
    let t = track(&pool, "stuck.flac", 100).await;

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let report = fingerprint::run_batch(&ctx, &cancel).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!((report.claimed, report.done, report.failed), (1, 0, 0));
    let row = stage_row(&pool, t).await;
    assert_eq!(row.get::<String, _>("status"), "working");
    assert!(row.get::<Option<String>, _>("error_code").is_none());
}
