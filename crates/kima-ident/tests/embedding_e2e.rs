//! End-to-end integration test for Stage::Embedding (M4):
//! Real small audio fixture (`tools/fixtures/chromaprint/tone.wav`) -> real symphonia decode
//! -> real CLAP inference with verified local private model -> real pgvector persistence
//! in an isolated Docker PostgreSQL container.
//!
//! These tests are `#[ignore]`d so a plain `cargo test` never needs a model or a database;
//! run them explicitly with `-- --ignored`. When run, they NEVER skip silently: any missing
//! prerequisite is a hard failure.
//!
//! Required environment:
//! - `DATABASE_URL`: `postgres://user:pass@127.0.0.1:<port>/<db>` of a dedicated, empty test
//!   cluster (port != 5456); `PGDATABASE` must equal `<db>`.
//! - `KIMA_CLAP_MODEL_DIR`: directory with the private CLAP audio export.
//! - `ORT_DYLIB_PATH`: trusted ONNX Runtime library.
//!
//! SAFETY GATE: before any SQLx setup (no `#[sqlx::test]` harness, which would create
//! databases first) [`isolated_db`] parses `DATABASE_URL` structurally, connects, and checks
//! the server's own view of its identity. Only then does it create a throwaway database.

mod common;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use common::ident_ctx_with_embedding;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::ml::{self, Provenance};
use kima_ident::coordinator;
use kima_ident::stages::embedding::{self, PIPELINE_VERSION, SOURCE};
use kima_ml::onnx::OnnxModel;
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

/// The CLAP model directory from `KIMA_CLAP_MODEL_DIR`; panics if unset or without the model.
fn model_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var_os("KIMA_CLAP_MODEL_DIR").expect("KIMA_CLAP_MODEL_DIR must be set"),
    );
    assert!(
        dir.join(OnnxModel::CLAP_AUDIO_BASENAME).is_file(),
        "CLAP audio model {} not found in KIMA_CLAP_MODEL_DIR={}",
        OnnxModel::CLAP_AUDIO_BASENAME,
        dir.display()
    );
    dir
}

fn embedding_config(model_dir: PathBuf, root: &Path) -> kima_ident::EmbeddingConfig {
    kima_ident::EmbeddingConfig {
        model_dir: Some(model_dir),
        pool_size: 1,
        workers: 1,
        disabled: false,
        snapshot_dir: Some(private_snapshot_dir(root)),
        audio_blocked_reason: None,
        snapshot_limits: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres + KIMA_CLAP_MODEL_DIR; run with --ignored"]
async fn real_audio_to_real_clap_inference_to_isolated_pgvector() {
    let model_dir = model_dir();
    let db = isolated_db().await;
    let pool = db.pool.clone();

    let dir = tempfile::tempdir().unwrap();
    let library_root = dir.path().canonicalize().unwrap();

    // Copy the real small audio fixture tone.wav (30 seconds, 44.1 kHz PCM)
    let fixture_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/fixtures/chromaprint/tone.wav")
        .canonicalize()
        .expect("fixture tone.wav must exist");
    let rel_audio_path = "music/tone.wav";
    let target_audio = library_root.join(rel_audio_path);
    std::fs::create_dir_all(target_audio.parent().unwrap()).unwrap();
    std::fs::copy(&fixture_src, &target_audio).unwrap();

    // Insert artist, album, track into the isolated database
    let track_id = common::track(&pool, rel_audio_path, 30).await;

    // Build IdentCtx with real CLAP model directory
    let embedding_config = embedding_config(model_dir, &library_root);
    let ctx = ident_ctx_with_embedding(
        &pool,
        &library_root,
        common::dead_bases(),
        IdentifySwitch::new(false, true),
        embedding_config,
    );

    // 1. Run the embedding stage batch (strict TDD: claim -> real decode -> real ONNX -> pgvector)
    let batch_report = embedding::run_batch(&ctx, &CancellationToken::new())
        .await
        .expect("embedding batch should succeed");
    if batch_report.done == 0 {
        let err_row = sqlx::query(
            "SELECT status, error_code, error_class, last_error FROM track_stages WHERE track_id = $1 AND stage = 'embedding'",
        )
        .bind(track_id)
        .fetch_optional(&pool)
        .await;
        eprintln!("STAGE FAILED ROW: {err_row:?}");
    }
    assert_eq!(
        batch_report.claimed, 1,
        "track should be claimed for embedding"
    );
    assert_eq!(
        batch_report.done, 1,
        "embedding should complete successfully"
    );
    assert_eq!(
        batch_report.failed, 0,
        "no failures expected on valid audio and verified model"
    );

    // 2. Assert stage ledger state
    let stage_row = sqlx::query(
        "SELECT status, source, error_code, last_error FROM track_stages WHERE track_id = $1 AND stage = 'embedding'",
    )
    .bind(track_id)
    .fetch_one(&pool)
    .await
    .expect("stage row must exist");
    assert_eq!(stage_row.get::<String, _>("status"), "done");
    assert_eq!(
        stage_row.get::<Option<String>, _>("source").as_deref(),
        Some(SOURCE)
    );
    assert!(stage_row.get::<Option<String>, _>("error_code").is_none());
    assert!(stage_row.get::<Option<String>, _>("last_error").is_none());

    // 3. Assert pgvector persistence in track_embeddings table
    let provenance = Provenance {
        model_id: OnnxModel::ClapAudio.artifact_id().as_str().to_owned(),
        model_sha256: OnnxModel::ClapAudio.expected_sha256(),
        pipeline_version: PIPELINE_VERSION.to_owned(),
    };
    let persisted = ml::embedding(&pool, track_id, &provenance)
        .await
        .expect("querying embedding should succeed")
        .expect("embedding row must exist for track");
    assert_eq!(
        persisted.len(),
        512,
        "CLAP embedding must be 512-dimensional"
    );

    // Verify L2 unit norm
    let l2_norm: f32 = persisted.iter().map(|&x| x * x).sum::<f32>().sqrt();
    assert!(
        (l2_norm - 1.0).abs() < 1e-3,
        "persisted embedding must satisfy unit norm guard: norm was {l2_norm}"
    );

    // 4. Assert claim fencing: a stale or superseded claim cannot overwrite
    let stale_claim = ml::EmbeddingClaim {
        track_id,
        started_at: time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        file_generation: 0,
    };
    let stale_attempt = ml::complete_embedding(&pool, &stale_claim, &provenance, &persisted)
        .await
        .expect("stale completion attempt should return cleanly");
    assert!(
        !stale_attempt,
        "stale claim must be rejected by complete_embedding fencing"
    );
    db.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres + KIMA_CLAP_MODEL_DIR; run with --ignored"]
async fn coordinator_cycle_runs_real_embedding_e2e() {
    let model_dir = model_dir();
    let db = isolated_db().await;
    let pool = db.pool.clone();

    let dir = tempfile::tempdir().unwrap();
    let library_root = dir.path().canonicalize().unwrap();

    let fixture_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/fixtures/chromaprint/tone.wav")
        .canonicalize()
        .expect("fixture tone.wav must exist");
    let rel_audio_path = "tone.wav";
    let target_audio = library_root.join(rel_audio_path);
    std::fs::copy(&fixture_src, &target_audio).unwrap();

    let track_id = common::track(&pool, rel_audio_path, 30).await;

    // MusicBrainz switch off (IdentifySwitch::new(false, false)): coordinator network phases
    // will make 0 network requests, but embedding phase must still run and complete!
    let embedding_config = embedding_config(model_dir, &library_root);
    let mut ctx = ident_ctx_with_embedding(
        &pool,
        &library_root,
        common::dead_bases(),
        IdentifySwitch::new(false, false),
        embedding_config,
    );
    ctx.config.fingerprinting = false; // isolate to embedding

    *ctx.last_cycle.lock().unwrap() = None;
    let cycle_report = coordinator::run_cycle(&ctx, &CancellationToken::new())
        .await
        .expect("coordinator cycle should succeed");

    assert!(cycle_report.ran);
    assert_eq!(cycle_report.embedding.claimed, 1);
    assert_eq!(cycle_report.embedding.done, 1);
    assert_eq!(
        cycle_report.tracks.claimed, 0,
        "tracks phase should not run with switch off"
    );

    let stage_status: String = sqlx::query_scalar(
        "SELECT status FROM track_stages WHERE track_id = $1 AND stage = 'embedding'",
    )
    .bind(track_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stage_status, "done");
    db.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres + KIMA_CLAP_MODEL_DIR; run with --ignored"]
async fn superseded_worker_cannot_publish_using_replacement_token() {
    let model_dir = model_dir();
    let db = isolated_db().await;
    let pool = db.pool.clone();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/chromaprint/tone.wav");
    std::fs::copy(fixture, root.join("tone.wav")).unwrap();
    let track = common::track(&pool, "tone.wav", 30).await;
    let ctx = ident_ctx_with_embedding(
        &pool,
        &root,
        common::dead_bases(),
        IdentifySwitch::new(false, true),
        embedding_config(model_dir, &root),
    );
    let old = kima_db::stages::claim(&pool, kima_db::stages::Stage::Embedding, 1, true)
        .await
        .unwrap()
        .remove(0);

    // Hold the stage row's lock with an uncommitted replacement token. `run_claim` still sees
    // the old, valid token (MVCC), so it runs the REAL decode + CLAP inference; its publication
    // then blocks on this lock until we commit the replacement.
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("UPDATE track_stages SET started_at=started_at + interval '1 second' WHERE track_id=$1 AND stage='embedding'")
        .bind(track).execute(&mut *holder).await.unwrap();

    let worker_ctx = ctx.clone();
    let worker = tokio::spawn(async move {
        embedding::run_claim(&worker_ctx, &old, &CancellationToken::new()).await
    });

    // Evidence that inference finished: the publication transaction is parked on the lock.
    let waiting = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = $1 AND wait_event_type = 'Lock' \
                   AND query ILIKE '%FROM track_stages%FOR UPDATE%'",
            )
            .bind(&db.name)
            .fetch_one(&db.admin)
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
        "publication never reached the stage lock: inference did not complete"
    );
    assert!(!worker.is_finished(), "worker must be parked on the lock");
    holder.commit().await.unwrap();

    let result = worker.await.unwrap().unwrap();
    assert!(matches!(result, embedding::ClaimResult::Stale));
    let (status, count): (String, i64) = sqlx::query_as("SELECT status, (SELECT count(*) FROM track_embeddings WHERE track_id=$1) FROM track_stages WHERE track_id=$1 AND stage='embedding'")
        .bind(track).fetch_one(&pool).await.unwrap();
    assert_eq!(status, "working");
    assert_eq!(count, 0);
    db.cleanup().await;
}

/// A 0700 snapshot directory on the library's own mount (required by `SnapshotStore`).
fn private_snapshot_dir(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = root.join(".kima-snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

// ---- Halting, per-file `Unreadable` and "no snapshot directory" (DB-backed) ----------------

/// A library root with `n` copies of the real fixture, `a0.wav`.., registered as tracks.
async fn tracks_with_audio(pool: &PgPool, root: &Path, n: usize) -> Vec<uuid::Uuid> {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/chromaprint/tone.wav");
    let mut ids = Vec::new();
    for i in 0..n {
        let rel = format!("a{i}.wav");
        std::fs::copy(&fixture, root.join(&rel)).unwrap();
        ids.push(common::track(pool, &rel, 30).await);
    }
    ids
}

/// `(status, attempts, redelivered, started_at is null, seconds until next_attempt_at,
/// error_code, error_class)` of a track's embedding row.
type StageRow = (
    String,
    i16,
    i16,
    bool,
    Option<f64>,
    Option<String>,
    Option<String>,
);

async fn stage_row(pool: &PgPool, track: uuid::Uuid) -> Option<StageRow> {
    sqlx::query_as(
        "SELECT status, attempts, redelivered, started_at IS NULL, \
                EXTRACT(EPOCH FROM next_attempt_at - now())::float8, error_code, error_class \
         FROM track_stages WHERE track_id = $1 AND stage = 'embedding'",
    )
    .bind(track)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// The shared snapshot directory becomes read-only after the store pinned it: every
/// snapshot creation then fails the same, global way (the first claim must halt the batch).
fn break_snapshot_dir(root: &Path) -> impl Drop {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let dir = root.join(".kima-snapshots");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    // Permission bits are not enforced for root: refuse to produce a false pass.
    assert!(
        std::fs::File::create(dir.join("probe")).is_err(),
        "the test must run unprivileged (no CAP_DAC_OVERRIDE)"
    );
    Restore(dir)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres + KIMA_CLAP_MODEL_DIR; run with --ignored"]
async fn global_snapshot_failure_halts_batch_and_releases_unstarted_claims() {
    let model_dir = model_dir();
    let db = isolated_db().await;
    let pool = db.pool.clone();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let ctx = ident_ctx_with_embedding(
        &pool,
        &root,
        common::dead_bases(),
        IdentifySwitch::new(false, true),
        embedding_config(model_dir, &root),
    );
    // No track yet: this opens and pins the snapshot store without claiming anything.
    let warm = embedding::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(warm.claimed, 0);
    let tracks = tracks_with_audio(&pool, &root, 3).await;
    let _restore = break_snapshot_dir(&root);

    let report = embedding::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.claimed, 3);
    assert_eq!(report.done, 0);
    assert_eq!(report.failed, 1, "only the first claim ran: {report:?}");
    assert!(report.halted, "{report:?}");

    let mut failed = 0;
    let mut released = 0;
    for track in &tracks {
        let (status, attempts, redelivered, no_token, wait, code, class) =
            stage_row(&pool, *track).await.expect("claimed rows exist");
        assert_eq!((attempts, redelivered), (0, 0), "no attempt is consumed");
        match status.as_str() {
            "failed" => {
                failed += 1;
                assert_eq!(code.as_deref(), Some("SNAPSHOT_UNAVAILABLE"));
                assert_eq!(class.as_deref(), Some("INCONCLUSIVE"));
                assert!(
                    wait.unwrap() > 3000.0,
                    "infrastructure waits ~1 h: {wait:?}"
                );
            }
            "pending" => {
                released += 1;
                assert!(no_token, "the claim token is cleared");
                assert_eq!(wait, None, "released claims are due immediately");
                assert_eq!(code, None);
            }
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!((failed, released), (1, 2));
    let status = embedding::cached_status(&ctx).unwrap();
    assert!(
        !status.is_available(kima_ml::Capability::ClapAudioEmbedding),
        "{status:?}"
    );
    assert!(format!("{status:?}").contains("restart"));
    // Runtime infrastructure blocks are sticky, not a probe of the next library file.
    drop(_restore);
    let next = embedding::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(next.claimed, 0);
    // Audio infrastructure must never poison the independently loaded text runtime.
    let text = embedding::embed_text(&ctx, "a short musical phrase")
        .await
        .unwrap();
    assert!(text.iter().all(|x| x.is_finite()));
    assert!(
        embedding::cached_status(&ctx)
            .unwrap()
            .is_available(kima_ml::Capability::ClapTextEmbedding)
    );
    // A fresh process context revalidates repaired infrastructure; no live reload.
    let restarted = ident_ctx_with_embedding(
        &pool,
        &root,
        common::dead_bases(),
        IdentifySwitch::new(false, true),
        ctx.config.embedding.clone(),
    );
    let next = embedding::run_batch(&restarted, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(next.done, 2);
    assert!(
        embedding::cached_status(&restarted)
            .unwrap()
            .is_available(kima_ml::Capability::ClapAudioEmbedding)
    );
    db.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres + KIMA_CLAP_MODEL_DIR; run with --ignored"]
async fn halted_embedding_phase_stops_and_later_cycles_burn_no_attempts() {
    let model_dir = model_dir();
    let db = isolated_db().await;
    let pool = db.pool.clone();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut ctx = ident_ctx_with_embedding(
        &pool,
        &root,
        common::dead_bases(),
        IdentifySwitch::new(false, false),
        embedding_config(model_dir, &root),
    );
    ctx.config.fingerprinting = false;
    let warm = coordinator::run_cycle(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(warm.embedding.claimed, 0);
    let tracks = tracks_with_audio(&pool, &root, 3).await;
    let _restore = break_snapshot_dir(&root);

    // Without the halt the phase would re-claim the released rows (3 + 2 + 1 claims).
    *ctx.last_cycle.lock().unwrap() = None;
    let first = coordinator::run_cycle(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert!(first.ran);
    assert_eq!(first.embedding.claimed, 3, "one batch only: {first:?}");
    assert_eq!(first.embedding.failed, 1);
    assert!(first.embedding.halted);

    // The next cycle must not probe either released row while infrastructure is latched.
    *ctx.last_cycle.lock().unwrap() = None;
    let second = coordinator::run_cycle(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        second.embedding.claimed, 0,
        "latched infrastructure block: {second:?}"
    );
    assert_eq!(second.embedding.failed, 0);
    assert!(!second.embedding.halted);

    let mut statuses = Vec::new();
    for track in &tracks {
        let row = stage_row(&pool, *track).await.unwrap();
        assert_eq!(
            (row.1, row.2),
            (0, 0),
            "infrastructure failures never burn attempts"
        );
        statuses.push(row.0);
    }
    statuses.sort();
    assert_eq!(statuses, ["failed", "pending", "pending"]);
    db.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres + KIMA_CLAP_MODEL_DIR; run with --ignored"]
async fn eacces_is_per_file_unreadable_with_bounded_retries() {
    use std::os::unix::fs::PermissionsExt;
    struct Unlock(PathBuf);
    impl Drop for Unlock {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let model_dir = model_dir();
    let db = isolated_db().await;
    let pool = db.pool.clone();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let ctx = ident_ctx_with_embedding(
        &pool,
        &root,
        common::dead_bases(),
        IdentifySwitch::new(false, true),
        embedding_config(model_dir, &root),
    );
    let good = tracks_with_audio(&pool, &root, 1).await.remove(0);
    let locked = root.join("locked");
    std::fs::create_dir(&locked).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/chromaprint/tone.wav");
    std::fs::copy(&fixture, locked.join("x.wav")).unwrap();
    let bad = common::track(&pool, "locked/x.wav", 30).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _unlock = Unlock(locked.clone());
    assert!(
        std::fs::read_dir(&locked).is_err(),
        "the test must run unprivileged (no CAP_DAC_OVERRIDE)"
    );

    // Delay after the n-th reported failure: 1 min, 10 min, 1 h; the 4th is terminal.
    let delays = [Some(60.0), Some(600.0), Some(3600.0), None];
    for (n, delay) in delays.into_iter().enumerate() {
        let report = embedding::run_batch(&ctx, &CancellationToken::new())
            .await
            .unwrap();
        assert!(
            !report.halted,
            "a per-file failure must not halt: {report:?}"
        );
        assert_eq!(report.failed, 1, "failure {}: {report:?}", n + 1);
        assert_eq!(report.done, usize::from(n == 0), "{report:?}");
        assert_eq!(report.claimed, if n == 0 { 2 } else { 1 }, "{report:?}");
        let (status, attempts, _, _, wait, code, class) = stage_row(&pool, bad).await.unwrap();
        assert_eq!(status, "failed");
        assert_eq!(attempts, i16::try_from(n + 1).unwrap());
        assert_eq!(code.as_deref(), Some("SNAPSHOT_UNREADABLE"));
        assert_eq!(class.as_deref(), Some("INCONCLUSIVE"));
        match (delay, wait) {
            (Some(expected), Some(w)) => {
                assert!((expected - 10.0..=expected).contains(&w), "{n}: {w}")
            }
            (None, None) => {}
            other => panic!("failure {}: retry schedule {other:?}", n + 1),
        }
        // Not due yet: nothing is claimed; then force it due for the next round.
        if delay.is_some() {
            let idle = embedding::run_batch(&ctx, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(idle.claimed, 0, "retry delay respected");
            sqlx::query(
                "UPDATE track_stages SET next_attempt_at = now() \
                 WHERE track_id = $1 AND stage = 'embedding'",
            )
            .bind(bad)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    // Terminal: never claimed again; the readable neighbour stayed done.
    let after = embedding::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(after.claimed, 0);
    assert_eq!(stage_row(&pool, good).await.unwrap().0, "done");
    db.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs isolated Postgres + KIMA_CLAP_MODEL_DIR; run with --ignored"]
async fn blocked_snapshot_setup_claims_nothing() {
    let model_dir = model_dir();
    let db = isolated_db().await;
    let pool = db.pool.clone();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let track = tracks_with_audio(&pool, &root, 1).await.remove(0);
    let mut unset = embedding_config(model_dir.clone(), &root);
    unset.snapshot_dir = None;
    let mut blocked = embedding_config(model_dir, &root);
    blocked.audio_blocked_reason = Some("startup check failed".into());
    for (name, embedding_config) in [("no snapshot dir", unset), ("blocked", blocked)] {
        let ctx = ident_ctx_with_embedding(
            &pool,
            &root,
            common::dead_bases(),
            IdentifySwitch::new(false, true),
            embedding_config,
        );
        let report = embedding::run_batch(&ctx, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report, Default::default(), "{name}");
        let mut cycle_ctx = ctx.clone();
        cycle_ctx.config.fingerprinting = false;
        let cycle = coordinator::run_cycle(&cycle_ctx, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(cycle.embedding, Default::default(), "{name}");
        assert!(stage_row(&pool, track).await.is_none(), "{name}: no claim");
    }
    db.cleanup().await;
}
