//! The enrichment coordinator (spec §10.6): the cycle's state machine, what it runs and when
//! it queues the next one, kick, prepare, and Review Focus 1 (identification switched off
//! makes no request while fingerprints are still computed). Replay only.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{ident_ctx, ident_ctx_with};
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::enrichment::{self, Status};
use kima_ident::IdentCtx;
use kima_ident::coordinator::{self, CYCLE_KIND, MAX_ATTEMPTS, QUEUE};
use kima_jobs::queue;
use kima_jobs::worker::{WorkerConfig, spawn_worker};
use kima_net::Bases;
use kima_net::testing::{Entry, Recording, Replay};
use kima_test_support::{Tags, make_audio, require_ffmpeg};
use serde_json::{Value, json};
use sqlx::Row;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn rec_id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn get(url: &str, body: Value) -> Entry {
    Entry {
        method: "GET".into(),
        url: url.into(),
        status: 200,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

/// The one request the file-tag rung makes for `rec`.
fn details(rec: &str) -> Entry {
    get(
        &format!("/recording/{rec}?inc=isrcs+genres+tags&fmt=json"),
        json!({"id": rec, "title": "Tone", "length": 30_000, "isrcs": [], "genres": [], "tags": []}),
    )
}

/// Copies the 30 s chromaprint fixture to `root/rel` (a 2 s tone is too short to fingerprint).
fn tone(root: &Path, rel: &str) {
    let out = root.join(rel);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/chromaprint/tone.wav");
    std::fs::copy(fixture, out).unwrap();
}

/// A present track "Tone" at `rel` (30 s), tagged with `tag` as its recording id when given.
async fn track(pool: &PgPool, rel: &str, tag: Option<&str>) -> Uuid {
    let t = common::track(pool, rel, 30).await;
    sqlx::query("UPDATE tracks SET title = 'Tone', tag_recording_mbid = $2 WHERE id = $1")
        .bind(t)
        .bind(tag.map(|s| Uuid::parse_str(s).unwrap()))
        .execute(pool)
        .await
        .unwrap();
    t
}

async fn set_state(pool: &PgPool, status: &str, halted: bool) {
    sqlx::query("UPDATE enrichment_state SET status = $1, halted = $2")
        .bind(status)
        .bind(halted)
        .execute(pool)
        .await
        .unwrap();
}

async fn stage_rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM track_stages")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn stage_status(pool: &PgPool, t: Uuid, stage: &str) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM track_stages WHERE track_id = $1 AND stage = $2")
        .bind(t)
        .bind(stage)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// `(state, seconds from the database's now() to run_at)` of every enrich job.
async fn enrich_jobs(pool: &PgPool) -> Vec<(String, f64)> {
    sqlx::query(
        "SELECT state, extract(epoch FROM run_at - now())::float8 AS secs FROM jobs
         WHERE queue = 'enrich' ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.get("state"), r.get("secs")))
    .collect()
}

async fn queued_enrich(pool: &PgPool) -> Vec<f64> {
    enrich_jobs(pool)
        .await
        .into_iter()
        .filter(|(s, _)| s == "queued")
        .map(|(_, secs)| secs)
        .collect()
}

fn forget_last_cycle(ctx: &IdentCtx) {
    *ctx.last_cycle.lock().unwrap() = None;
}

async fn cycle(ctx: &IdentCtx) -> coordinator::CycleReport {
    forget_last_cycle(ctx);
    coordinator::run_cycle(ctx, &CancellationToken::new())
        .await
        .unwrap()
}

fn root(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().canonicalize().unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn paused_or_halted_cycle_does_nothing(pool: PgPool) {
    let replay = Replay::start(Recording(vec![])).await;
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ident_ctx(&pool, &root(&dir), Bases::all(&replay.url()));
    ctx.config.fingerprinting = false;
    track(&pool, "a.wav", Some(&rec_id(1))).await;

    for (status, halted) in [("paused", false), ("idle", true), ("running", true)] {
        set_state(&pool, status, halted).await;
        let report = cycle(&ctx).await;
        assert!(!report.ran, "{status} halted={halted}");
        assert_eq!(stage_rows(&pool).await, 0);
        assert!(enrich_jobs(&pool).await.is_empty(), "enqueues nothing");
        let st = enrichment::state(&pool).await.unwrap();
        assert!(!st.may_run());
    }
    // Paused stays paused.
    set_state(&pool, "paused", false).await;
    cycle(&ctx).await;
    assert_eq!(
        enrichment::state(&pool).await.unwrap().status,
        Status::Paused
    );
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stopping_cycle_goes_idle(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ident_ctx(&pool, &root(&dir), common::dead_bases());
    track(&pool, "a.wav", None).await;
    sqlx::query(
        "UPDATE enrichment_state SET status = 'stopping', halted = true, current_phase = 'tracks'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let report = cycle(&ctx).await;

    assert!(!report.ran);
    let st = enrichment::state(&pool).await.unwrap();
    assert_eq!(st.status, Status::Idle);
    assert!(st.halted, "stop's halt stays until sync");
    assert_eq!(st.current_phase, None);
    assert_eq!(stage_rows(&pool).await, 0);
    assert!(enrich_jobs(&pool).await.is_empty());
}

/// Waits until the enrich queue has run at least one job and nothing is queued or running.
async fn wait_enrich_quiet(pool: &PgPool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let jobs = enrich_jobs(pool).await;
        if !jobs.is_empty() && jobs.iter().all(|(s, _)| s == "done") {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "enrich queue never went quiet: {jobs:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn fast(queue: &str) -> WorkerConfig {
    let mut c = WorkerConfig::new(queue, 1);
    c.poll_interval = Duration::from_millis(100);
    c
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn halt_survives_kick_and_scan(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let replay = Replay::start(Recording(vec![])).await;
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let ctx = ident_ctx(&pool, &root, Bases::all(&replay.url()));
    set_state(&pool, "idle", true).await;
    make_audio(&root, "Artist/Album/01.flac", Tags::full());

    let shutdown = CancellationToken::new();
    let enrich = spawn_worker(
        pool.clone(),
        fast(QUEUE),
        coordinator::handler(ctx.clone()),
        shutdown.clone(),
    );
    let library = spawn_worker(
        pool.clone(),
        fast(kima_library::jobs::QUEUE),
        kima_library::jobs::handler_with(ctx.library.clone(), coordinator::kick_on_change()),
        shutdown.clone(),
    );

    // A reconcile that adds a track kicks the coordinator.
    let reconcile = kima_library::jobs::enqueue_reconcile(&pool)
        .await
        .unwrap()
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id = $1")
            .bind(reconcile)
            .fetch_one(&pool)
            .await
            .unwrap();
        if state == "done" {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let tracks: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tracks, 1, "the reconcile changed the library");
    wait_enrich_quiet(&pool).await;
    coordinator::kick(&pool).await.unwrap();
    wait_enrich_quiet(&pool).await;

    shutdown.cancel();
    let _ = tokio::join!(enrich, library);
    assert_eq!(stage_rows(&pool).await, 0, "no work ran while halted");
    assert!(
        queued_enrich(&pool).await.is_empty(),
        "no next cycle queued"
    );
    let st = enrichment::state(&pool).await.unwrap();
    assert!(st.halted);
    assert_eq!(st.status, Status::Idle);
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cycle_fingerprints_and_identifies(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let rec = rec_id(1);
    let replay = Replay::start(Recording(vec![details(&rec)])).await;
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let ctx = ident_ctx(&pool, &root, Bases::all(&replay.url()));
    tone(&root, "a.wav");
    let t = track(&pool, "a.wav", Some(&rec)).await;

    // The recording stage waits for the fingerprint to settle, which may take a second cycle.
    for _ in 0..3 {
        cycle(&ctx).await;
        if stage_status(&pool, t, "recording").await.as_deref() == Some("done") {
            break;
        }
    }

    assert_eq!(
        stage_status(&pool, t, "fingerprint").await.as_deref(),
        Some("done")
    );
    assert!(
        kima_db::identity::fingerprint_of(&pool, t)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        stage_status(&pool, t, "recording").await.as_deref(),
        Some("done")
    );
    let mbid: Option<Uuid> = sqlx::query_scalar("SELECT recording_mbid FROM tracks WHERE id = $1")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mbid, Some(Uuid::parse_str(&rec).unwrap()));
    let st = enrichment::state(&pool).await.unwrap();
    assert_ne!(st.status, Status::Paused);
}

/// Review Focus 1: with identification off, two cycles over tagged and untagged tracks make
/// no request, while every track is still fingerprinted.
async fn identify_off(pool: PgPool, switch: IdentifySwitch) {
    if !require_ffmpeg() {
        return;
    }
    let replay = Replay::start(Recording(vec![])).await;
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let ctx = ident_ctx_with(&pool, &root, Bases::all(&replay.url()), switch);
    tone(&root, "tagged.wav");
    tone(&root, "untagged.wav");
    let tagged = track(&pool, "tagged.wav", Some(&rec_id(1))).await;
    let untagged = track(&pool, "untagged.wav", None).await;

    cycle(&ctx).await;
    cycle(&ctx).await;

    assert!(replay.asked().is_empty(), "{:?}", replay.asked());
    for t in [tagged, untagged] {
        let acoustid: i64 =
            sqlx::query_scalar("SELECT count(*) FROM track_acoustid WHERE track_id = $1")
                .bind(t)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(acoustid, 1, "fingerprinted");
    }
    let done: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM track_stages WHERE stage = 'recording' AND status = 'done'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(done, 0);
    // The coordinator never ran the tracks phase (not merely refused by `Net`).
    let recording: i64 =
        sqlx::query_scalar("SELECT count(*) FROM track_stages WHERE stage = 'recording'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(recording, 0);
    let identified: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tracks WHERE recording_mbid IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(identified, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn identify_off_makes_no_request(pool: PgPool) {
    // The "Auto enrich metadata" toggle off.
    identify_off(pool, IdentifySwitch::new(false, false)).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn identify_env_off_makes_no_request(pool: PgPool) {
    // KIMA_IDENTIFY=off with the toggle on.
    identify_off(pool, IdentifySwitch::new(true, true)).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn paused_musicbrainz_skips_tracks_phase(pool: PgPool) {
    let rec = rec_id(1);
    let replay = Replay::start(Recording(vec![details(&rec)])).await;
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ident_ctx(&pool, &root(&dir), Bases::all(&replay.url()));
    ctx.config.fingerprinting = false;
    let t = track(&pool, "a.wav", Some(&rec)).await;
    sqlx::query(
        "INSERT INTO source_pauses (source, paused_until, reason)
         VALUES ('musicbrainz', now() + interval '10 minutes', 'HTTP 503')",
    )
    .execute(&pool)
    .await
    .unwrap();
    ctx.net.load_pauses().await.unwrap();

    cycle(&ctx).await;

    assert!(replay.asked().is_empty());
    assert_eq!(stage_status(&pool, t, "recording").await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn next_cycle_in_5s_after_work_and_60s_when_idle(pool: PgPool) {
    let rec = rec_id(1);
    let replay = Replay::start(Recording(vec![details(&rec)])).await;
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ident_ctx(&pool, &root(&dir), Bases::all(&replay.url()));
    ctx.config.fingerprinting = false;
    track(&pool, "a.wav", Some(&rec)).await;

    let report = cycle(&ctx).await;
    assert!(report.ran);
    assert_eq!(report.tracks.done, 1);
    let queued = queued_enrich(&pool).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert!((3.0..=6.0).contains(&queued[0]), "{queued:?}");
    assert_eq!(
        enrichment::state(&pool).await.unwrap().status,
        Status::Running
    );

    sqlx::query("DELETE FROM jobs")
        .execute(&pool)
        .await
        .unwrap();
    let report = cycle(&ctx).await;
    assert!(report.ran);
    assert_eq!(report.tracks.claimed, 0);
    let queued = queued_enrich(&pool).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert!((57.0..=61.0).contains(&queued[0]), "{queued:?}");
    let st = enrichment::state(&pool).await.unwrap();
    assert_eq!(st.status, Status::Idle);
    assert_eq!(st.current_phase, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn queued_kick_wins_over_the_next_cycle(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ident_ctx(&pool, &root(&dir), common::dead_bases());
    ctx.config.fingerprinting = false;
    coordinator::kick(&pool).await.unwrap();
    cycle(&ctx).await;
    let queued = queued_enrich(&pool).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert!(queued[0] <= 0.5, "the kick's job stays due now: {queued:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn kick_moves_queued_cycle_to_now(pool: PgPool) {
    let later = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    queue::enqueue_at(
        &pool,
        QUEUE,
        json!({"kind": CYCLE_KIND}),
        MAX_ATTEMPTS,
        later,
    )
    .await
    .unwrap();

    coordinator::kick(&pool).await.unwrap();

    let queued = queued_enrich(&pool).await;
    assert_eq!(queued.len(), 1);
    assert!(queued[0] <= 0.0, "{queued:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn kick_enqueues_when_none_queued(pool: PgPool) {
    coordinator::kick(&pool).await.unwrap();
    let jobs = sqlx::query("SELECT payload, max_attempts FROM jobs WHERE queue = 'enrich'")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].get::<Value, _>("payload"), json!({"kind": "cycle"}));
    assert_eq!(jobs[0].get::<i32, _>("max_attempts"), MAX_ATTEMPTS);
    assert!(queued_enrich(&pool).await[0] <= 0.0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_kicks_queue_one(pool: PgPool) {
    let kicks = (0..16).map(|_| {
        let pool = pool.clone();
        tokio::spawn(async move { coordinator::kick(&pool).await.unwrap() })
    });
    for k in futures::future::join_all(kicks).await {
        k.unwrap();
    }
    assert_eq!(queued_enrich(&pool).await.len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn min_interval_between_cycles(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ident_ctx(&pool, &root(&dir), common::dead_bases());
    ctx.config.fingerprinting = false;
    assert_eq!(coordinator::MIN_INTERVAL, Duration::from_secs(10));

    // The previous cycle started 8 s ago: this one waits the remaining 2 s.
    *ctx.last_cycle.lock().unwrap() = Some(Instant::now() - Duration::from_secs(8));
    let started = Instant::now();
    coordinator::run_cycle(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1900), "{waited:?}");
    assert!(waited < Duration::from_secs(6), "{waited:?}");

    // The wait is cancellable.
    *ctx.last_cycle.lock().unwrap() = Some(Instant::now());
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    assert!(coordinator::run_cycle(&ctx, &cancel).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prepare_resets_enriching_artists(pool: PgPool) {
    let artist = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO artists (id, name, name_key, enrichment_status) VALUES ($1, 'x', 'x', 'enriching')",
    )
    .bind(artist)
    .execute(&pool)
    .await
    .unwrap();
    // A cycle a dead process left running.
    let job = queue::enqueue(&pool, QUEUE, json!({"kind": CYCLE_KIND}), MAX_ATTEMPTS)
        .await
        .unwrap();
    queue::claim(&pool, QUEUE, 1, Duration::from_secs(300))
        .await
        .unwrap();

    coordinator::prepare(&pool).await.unwrap();

    let status: String = sqlx::query_scalar("SELECT enrichment_status FROM artists WHERE id = $1")
        .bind(artist)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    let jobs = sqlx::query("SELECT id, state FROM jobs WHERE queue = 'enrich'")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "the requeued cycle is the kick's");
    assert_eq!(jobs[0].get::<Uuid, _>("id"), job);
    assert_eq!(jobs[0].get::<String, _>("state"), "queued");
}

/// Carry (Task 17): a database error in a batch is recorded and retried next cycle; the
/// cycle itself does not fail and the coordinator keeps its schedule.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn batch_db_error_is_recorded_and_retried(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ident_ctx(&pool, &root(&dir), common::dead_bases());
    track(&pool, "a.wav", None).await;
    sqlx::query("ALTER TABLE track_stages RENAME TO track_stages_gone")
        .execute(&pool)
        .await
        .unwrap();

    let report = cycle(&ctx).await;

    assert!(report.ran);
    let st = enrichment::state(&pool).await.unwrap();
    let err = st.last_error.expect("recorded");
    assert!(err.contains("track_stages"), "{err}");
    assert!(err.chars().count() <= 2000);
    assert_eq!(
        queued_enrich(&pool).await.len(),
        1,
        "the next cycle is queued"
    );
}

/// Carry (Task 14 m1): cancelling a batch releases the claims it had not started back to
/// `pending`; the one in flight stays `working` for its lease.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cancel_releases_unstarted_claims(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = root(&dir);
    let fifo = root.join("stuck.flac");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    tone(&root, "b.wav");
    let mut ctx = ident_ctx(&pool, &root, common::dead_bases());
    ctx.config.fingerprint_workers = 1;
    let stuck = common::track(&pool, "stuck.flac", 100).await;
    let waiting = common::track(&pool, "b.wav", 30).await;
    // The stuck file is claimed first (newest first).
    sqlx::query("UPDATE tracks SET mtime = now() - interval '1 day' WHERE id = $1")
        .bind(waiting)
        .execute(&pool)
        .await
        .unwrap();

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        trigger.cancel();
    });
    let report = kima_ident::stages::fingerprint::run_batch(&ctx, &cancel)
        .await
        .unwrap();

    assert_eq!(report.claimed, 2);
    assert_eq!(
        stage_status(&pool, stuck, "fingerprint").await.as_deref(),
        Some("working")
    );
    let row = sqlx::query(
        "SELECT status, next_attempt_at, redelivered, started_at FROM track_stages
         WHERE track_id = $1 AND stage = 'fingerprint'",
    )
    .bind(waiting)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "pending");
    assert!(
        row.get::<Option<time::OffsetDateTime>, _>("next_attempt_at")
            .is_none()
    );
    assert_eq!(row.get::<i16, _>("redelivered"), 0);
    // Due again at once.
    let again = kima_db::stages::claim(&pool, kima_db::stages::Stage::Fingerprint, 5, true)
        .await
        .unwrap();
    assert_eq!(
        again.iter().map(|c| c.track_id).collect::<Vec<_>>(),
        vec![waiting]
    );
}
