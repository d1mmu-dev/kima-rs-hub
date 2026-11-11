//! End-to-end integration test for Stage::Analysis (M4, D-158):
//! Real small audio fixture (`tools/fixtures/chromaprint/tone.wav`) -> real symphonia decode
//! -> real DSP (loudness/key/tempo) and, when configured, real MusiCNN inference with the
//! verified local private model directory -> real `track_analysis` persistence in an
//! isolated Docker PostgreSQL container, run through the real orchestrator
//! (`kima_ident::stages::analysis::{run_batch, run_claim}`).
//!
//! Mirrors `embedding_e2e.rs`'s structure and safety gates exactly: its own identity-verified
//! throwaway database (no `#[sqlx::test]`), `#[ignore]`d so a plain `cargo test` never needs a
//! model or a database, and NEVER a silent skip when run with `-- --ignored`: any missing
//! prerequisite is a hard failure (`expect`/`panic`), not a quietly-passed test.
//!
//! Required environment:
//! - `DATABASE_URL`: `postgres://user:***@127.0.0.1:<port>/<db>` of a dedicated, empty test
//!   cluster (port != 5456); `PGDATABASE` must equal `<db>`.
//! - `KIMA_MUSICNN_MODEL_DIR` (only for the full-analysis test below): directory with the
//!   11 pinned private MusiCNN ONNX artifacts (core + 10 heads).
//!
//! SAFETY GATE: before any SQLx setup (no `#[sqlx::test]` harness, which would create
//! databases first) [`isolated_db`] parses `DATABASE_URL` structurally, connects, and checks
//! the server's own view of its identity. Only then does it create a throwaway database.

mod common;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use common::ident_ctx_with_embedding_and_analysis;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::stages::{self, Claim, Stage};
use kima_ident::coordinator;
use kima_ident::stages::ClaimResult;
use kima_ident::stages::analysis::{self, PIPELINE_VERSION, SOURCE};
use sqlx::Row;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio_util::sync::CancellationToken;

const DEV_PORT: u16 = 5456;

/// A throwaway database inside the verified isolated cluster.
struct TestDb {
    pool: PgPool,
    admin: PgPool,
    name: String,
}

impl TestDb {
    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
            .execute(&self.admin)
            .await
            .expect("drop throwaway database");
        self.admin.close().await;
    }
}

/// Verifies the target server's identity, then creates and migrates a fresh database.
/// Panics (never skips) when anything is missing or the target is not provably isolated.
async fn isolated_db() -> TestDb {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pg_database = std::env::var("PGDATABASE").expect("PGDATABASE must be set");
    let opts = PgConnectOptions::from_str(&url).expect("DATABASE_URL must parse");

    assert_eq!(
        opts.get_host(),
        "127.0.0.1",
        "SAFETY REFUSAL: host must be 127.0.0.1"
    );
    assert_ne!(
        opts.get_port(),
        DEV_PORT,
        "SAFETY REFUSAL: DATABASE_URL targets the development port {DEV_PORT}"
    );
    let url_db = opts
        .get_database()
        .expect("DATABASE_URL must name a database")
        .to_owned();
    assert_eq!(
        url_db, pg_database,
        "SAFETY REFUSAL: DATABASE_URL and PGDATABASE disagree"
    );
    assert_ne!(
        url_db, "kima",
        "SAFETY REFUSAL: refusing the development database name"
    );

    let admin = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts.clone())
        .await
        .expect("connect to the test cluster");

    // The server's own view, not the URL's.
    let (current_db, server_port): (String, Option<i32>) =
        sqlx::query_as("SELECT current_database()::text, inet_server_port()")
            .fetch_one(&admin)
            .await
            .expect("identity query");
    assert_eq!(
        current_db, pg_database,
        "SAFETY REFUSAL: connected database differs from PGDATABASE"
    );
    assert_ne!(
        server_port,
        Some(i32::from(DEV_PORT)),
        "SAFETY REFUSAL: server reports the development port"
    );
    // A development cluster has the schema applied; the dedicated one must be empty.
    let has_schema: bool = sqlx::query_scalar(
        "SELECT to_regclass('public.tracks') IS NOT NULL OR to_regclass('public._sqlx_migrations') IS NOT NULL",
    )
    .fetch_one(&admin)
    .await
    .expect("schema probe");
    assert!(
        !has_schema,
        "SAFETY REFUSAL: target database already holds the kima schema; use a dedicated empty database"
    );

    let name = format!("kima_e2e_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE \"{name}\""))
        .execute(&admin)
        .await
        .expect("create throwaway database");
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts.database(&name))
        .await
        .expect("connect to throwaway database");
    kima_db::MIGRATOR
        .run(&pool)
        .await
        .expect("migrate throwaway database");
    TestDb { pool, admin, name }
}

/// The MusiCNN model directory from `KIMA_MUSICNN_MODEL_DIR`; panics if unset or incomplete.
fn musicnn_model_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var_os("KIMA_MUSICNN_MODEL_DIR").expect("KIMA_MUSICNN_MODEL_DIR must be set"),
    );
    assert!(
        dir.join(kima_ml::onnx::OnnxModel::CORE_BASENAME).is_file(),
        "MusiCNN core model {} not found in KIMA_MUSICNN_MODEL_DIR={}",
        kima_ml::onnx::OnnxModel::CORE_BASENAME,
        dir.display()
    );
    for head in kima_ml::onnx::MusiCnnHead::all() {
        let basename = head.basename();
        assert!(
            dir.join(basename).is_file(),
            "MusiCNN head model {basename} not found in KIMA_MUSICNN_MODEL_DIR={}",
            dir.display()
        );
    }
    dir
}

fn analysis_config(model_dir: Option<PathBuf>) -> kima_ident::AnalysisConfig {
    kima_ident::AnalysisConfig {
        model_dir,
        disabled: false,
        pool_size: 1,
        workers: 1,
    }
}

/// A 0700 snapshot directory on the library's own mount (required by `SnapshotStore`,
/// which the analysis stage reuses from the embedding stage for its stable input).
fn private_snapshot_dir(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = root.join(".kima-snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Minimal embedding config that only sets up the snapshot store the analysis stage
/// depends on; CLAP itself stays unconfigured (`model_dir: None`) in every test here.
fn embedding_config_for_analysis(root: &Path) -> kima_ident::EmbeddingConfig {
    kima_ident::EmbeddingConfig {
        snapshot_dir: Some(private_snapshot_dir(root)),
        ..Default::default()
    }
}

/// Copies the real fixture into the library root at `rel` and inserts its track row.
async fn seed_track(pool: &PgPool, root: &Path, rel: &str) -> uuid::Uuid {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/chromaprint/tone.wav");
    let target = root.join(rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::copy(&fixture, &target).unwrap();
    common::track(pool, rel, 30).await
}

async fn analysis_stage_row(
    pool: &PgPool,
    track_id: uuid::Uuid,
) -> Option<(String, Option<String>, Option<String>, Option<String>)> {
    sqlx::query(
        "SELECT status, source, error_code, last_error FROM track_stages \
         WHERE track_id = $1 AND stage = 'analysis'",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .expect("stage row query")
    .map(|row| {
        (
            row.get::<String, _>("status"),
            row.get::<Option<String>, _>("source"),
            row.get::<Option<String>, _>("error_code"),
            row.get::<Option<String>, _>("last_error"),
        )
    })
}

// ---- 1. Full analysis (DSP+MusiCNN) success marks the stage done ---------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres + KIMA_MUSICNN_MODEL_DIR; run with --ignored"]
async fn full_analysis_dsp_and_musicnn_marks_stage_done() {
    let model_dir = musicnn_model_dir();
    let db = isolated_db().await;
    let pool = db.pool.clone();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let track_id = seed_track(&pool, &root, "music/tone.wav").await;

    let ctx = ident_ctx_with_embedding_and_analysis(
        &pool,
        &root,
        common::dead_bases(),
        IdentifySwitch::new(false, true),
        embedding_config_for_analysis(&root),
        analysis_config(Some(model_dir)),
    );

    let batch_report = analysis::run_batch(&ctx, &CancellationToken::new())
        .await
        .expect("analysis batch should succeed");
    if batch_report.done == 0 {
        let row = analysis_stage_row(&pool, track_id).await;
        eprintln!("STAGE FAILED ROW: {row:?}");
    }
    assert_eq!(batch_report.claimed, 1, "track should be claimed");
    assert_eq!(batch_report.done, 1, "analysis should complete");
    assert_eq!(batch_report.failed, 0, "no failures expected");

    let (status, source, error_code, last_error) = analysis_stage_row(&pool, track_id)
        .await
        .expect("stage row must exist");
    assert_eq!(status, "done");
    assert_eq!(source.as_deref(), Some(SOURCE));
    assert!(error_code.is_none());
    assert!(last_error.is_none());

    // The stored result must carry all four groups (musicnn + loudness + key + tempo),
    // each at the fixed pipeline version this attempt writes.
    let stored = kima_db::analysis::analysis(&pool, track_id)
        .await
        .expect("querying analysis should succeed")
        .expect("analysis row must exist for track");
    let (musicnn_pv, _) = stored.musicnn.expect("musicnn group must be present");
    let (loudness_pv, _) = stored.loudness.expect("loudness group must be present");
    let (key_pv, _) = stored.key.expect("key group must be present");
    let (tempo_pv, _) = stored.tempo.expect("tempo group must be present");
    for pv in [&musicnn_pv, &loudness_pv, &key_pv, &tempo_pv] {
        assert_eq!(pv, PIPELINE_VERSION);
    }

    db.cleanup().await;
}

// ---- 2. MusiCNN disabled/missing model_dir: DSP still runs, stage still marked done (D-157) -

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres; run with --ignored"]
async fn missing_musicnn_model_dir_still_runs_dsp_and_marks_stage_done() {
    let db = isolated_db().await;
    let pool = db.pool.clone();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let track_id = seed_track(&pool, &root, "tone.wav").await;

    // No KIMA_MUSICNN_MODEL_DIR configured: the MusicnnAnalysis capability is unavailable,
    // but DSP (always available, D-157) must still run and the stage must still complete.
    let ctx = ident_ctx_with_embedding_and_analysis(
        &pool,
        &root,
        common::dead_bases(),
        IdentifySwitch::new(false, true),
        embedding_config_for_analysis(&root),
        analysis_config(None),
    );

    let batch_report = analysis::run_batch(&ctx, &CancellationToken::new())
        .await
        .expect("analysis batch should succeed");
    assert_eq!(batch_report.claimed, 1);
    assert_eq!(
        batch_report.done, 1,
        "DSP-only analysis must still mark the stage done (D-157)"
    );
    assert_eq!(batch_report.failed, 0);

    let (status, source, error_code, _) = analysis_stage_row(&pool, track_id)
        .await
        .expect("stage row must exist");
    assert_eq!(status, "done");
    assert_eq!(source.as_deref(), Some(SOURCE));
    assert!(error_code.is_none());

    let stored = kima_db::analysis::analysis(&pool, track_id)
        .await
        .expect("querying analysis should succeed")
        .expect("analysis row must exist for track");
    assert!(
        stored.musicnn.is_none(),
        "musicnn group must be absent (not attempted), not a fabricated value"
    );
    assert!(stored.loudness.is_some(), "DSP loudness must be present");
    assert!(stored.key.is_some(), "DSP key must be present");
    assert!(stored.tempo.is_some(), "DSP tempo must be present");

    // Also exercise via the real coordinator, not just the stage batch directly.
    let dir2 = tempfile::tempdir().unwrap();
    let root2 = dir2.path().canonicalize().unwrap();
    let track2 = seed_track(&pool, &root2, "tone2.wav").await;
    let mut cycle_ctx = ident_ctx_with_embedding_and_analysis(
        &pool,
        &root2,
        common::dead_bases(),
        IdentifySwitch::new(false, false),
        embedding_config_for_analysis(&root2),
        analysis_config(None),
    );
    cycle_ctx.config.fingerprinting = false;
    *cycle_ctx.last_cycle.lock().unwrap() = None;
    let cycle_report = coordinator::run_cycle(&cycle_ctx, &CancellationToken::new())
        .await
        .expect("coordinator cycle should succeed");
    assert!(cycle_report.ran);
    assert_eq!(cycle_report.analysis.claimed, 1);
    assert_eq!(cycle_report.analysis.done, 1);
    let status2: String = sqlx::query_scalar(
        "SELECT status FROM track_stages WHERE track_id = $1 AND stage = 'analysis'",
    )
    .bind(track2)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status2, "done");

    db.cleanup().await;
}

// ---- 3. Stale file_generation causes storage rejection; claim is NOT marked done -----------

/// Polls `pg_stat_activity` until a `tracks ... FOR NO KEY UPDATE` query is observed waiting on a
/// lock, exactly like `embedding_e2e.rs`'s `superseded_worker_cannot_publish_using_replacement_token`.
async fn wait_for_generation_fence_lock_wait(admin: &PgPool, db_name: &str) {
    let waiting = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = $1 AND wait_event_type = 'Lock' \
                   AND query ILIKE '%FROM tracks%FOR NO KEY UPDATE%'",
            )
            .bind(db_name)
            .fetch_one(admin)
            .await
            .unwrap();
            if n > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        waiting.is_ok(),
        "publication never reached the track generation fence: analysis did not complete decode"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres; run with --ignored"]
async fn stale_file_generation_rejects_publication_and_claim_is_not_done() {
    let db = isolated_db().await;
    let pool = db.pool.clone();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let track_id = seed_track(&pool, &root, "tone.wav").await;

    // No MusiCNN model: DSP alone is enough to reach the publish/generation-fence call.
    let ctx = ident_ctx_with_embedding_and_analysis(
        &pool,
        &root,
        common::dead_bases(),
        IdentifySwitch::new(false, true),
        embedding_config_for_analysis(&root),
        analysis_config(None),
    );

    let claim: Claim = stages::claim(&pool, Stage::Analysis, 1, true)
        .await
        .unwrap()
        .remove(0);

    // Hold an uncommitted bump of tracks.ml_generation (any identity-tracked field changes
    // trigger it): the worker already captured the OLD generation via its own MVCC
    // snapshot before this transaction started, so it decodes and computes against it, but
    // its publish-time generation fence (`SELECT ... FOR NO KEY UPDATE` on `tracks`) blocks on our
    // held lock, then observes the NEW (mismatched) generation once we commit.
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("UPDATE tracks SET mtime = mtime + interval '1 second' WHERE id = $1")
        .bind(track_id)
        .execute(&mut *holder)
        .await
        .unwrap();

    let worker_ctx = ctx.clone();
    let worker = tokio::spawn(async move {
        analysis::run_claim(&worker_ctx, &claim, &CancellationToken::new()).await
    });

    wait_for_generation_fence_lock_wait(&db.admin, &db.name).await;
    assert!(!worker.is_finished(), "worker must be parked on the lock");
    holder.commit().await.unwrap();

    let result = worker.await.unwrap().unwrap();
    assert!(
        !matches!(result, ClaimResult::Done),
        "a stale file_generation must never close the claim as done: got {result:?}"
    );

    // The identity-change trigger itself resets the stage row to `pending` for redelivery
    // (never `done`), and no analysis result is ever persisted for the rejected attempt.
    let (status, _source, _error_code, _last_error) = analysis_stage_row(&pool, track_id)
        .await
        .expect("stage row must exist");
    assert_ne!(status, "done", "stale-generation attempt must not be done");

    let stored = kima_db::analysis::analysis(&pool, track_id)
        .await
        .expect("querying analysis should succeed");
    assert!(
        stored.is_none(),
        "no analysis result may be persisted for a rejected stale-generation attempt: {stored:?}"
    );

    db.cleanup().await;
}
