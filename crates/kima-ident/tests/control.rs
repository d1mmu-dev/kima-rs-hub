//! Operator controls (spec §10.6-§10.7): pause, resume, stop, sync, retry, re-decide,
//! wipe-and-rerun and concurrency. Ports of upstream `enrichmentStateMachine`,
//! `enrichmentState.heartbeat` (`detectHang`), `unifiedEnrichment.{redecideEverything,
//! retryFailedAndBlocked, noWipe, missingSkipped}` and `enrichmentRestart.integration` cases.

mod common;

use std::collections::BTreeMap;

use common::ident_ctx;
use kima_db::PgPool;
use kima_db::enrichment::{self, State, Status};
use kima_db::settings_kv::{self, Key};
use kima_db::stages::{self, Stage};
use kima_ident::IdentCtx;
use kima_ident::coordinator::control::ControlError;
use kima_ident::coordinator::{self, control};
use kima_net::Source;
use serde_json::{Value, json};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn ctx(pool: &PgPool) -> (IdentCtx, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (ident_ctx(pool, &root, common::dead_bases()), dir)
}

async fn set_state(pool: &PgPool, status: &str, halted: bool) {
    sqlx::query("UPDATE enrichment_state SET status = $1, halted = $2")
        .bind(status)
        .bind(halted)
        .execute(pool)
        .await
        .unwrap();
}

async fn queued_cycles(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'enrich' AND state = 'queued'")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A present track on its own artist and album.
async fn track(pool: &PgPool) -> Uuid {
    common::track(pool, &Uuid::new_v4().to_string(), 30).await
}

/// A stage row. `next` is an SQL expression for `next_attempt_at`; `class` is the error
/// code and, unless it is `RATE_LIMITED`, the error class.
async fn put(
    pool: &PgPool,
    t: Uuid,
    stage: &str,
    status: &str,
    next: &str,
    class: Option<&str>,
    evidence: Option<Value>,
) {
    sqlx::query(&format!(
        "INSERT INTO track_stages (track_id, stage, status, next_attempt_at, attempts, \
         redelivered, error_code, error_class, last_error, evidence, source, started_at, \
         finished_at) \
         VALUES ($1, $2, $3, {next}, 2, 1, $4, NULLIF($4, 'RATE_LIMITED'), CASE WHEN $4 IS NULL THEN NULL ELSE 'boom' END, \
                 $5, 'search', now() - interval '1 hour', now() - interval '30 minutes')"
    ))
    .bind(t)
    .bind(stage)
    .bind(status)
    .bind(class)
    .bind(evidence)
    .execute(pool)
    .await
    .unwrap();
}

const RETRY_LATER: &str = "now() + interval '1 hour'";
const RETRY_PAST: &str = "now() - interval '1 minute'";

/// The whole stage row as JSON, for "untouched by value" checks.
async fn stage_row(pool: &PgPool, t: Uuid, stage: &str) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(s) FROM track_stages s WHERE track_id = $1 AND stage = $2")
        .bind(t)
        .bind(stage)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn status_of(pool: &PgPool, t: Uuid, stage: &str) -> String {
    stage_row(pool, t, stage).await["status"]
        .as_str()
        .unwrap()
        .to_string()
}

fn skipped(reason: &str) -> Value {
    json!({"reason": "no match", "asked": ["file_tag", "search"], "acoustid": "skipped",
           "acoustidReason": reason})
}

// ---- state machine ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn pause_only_from_running(pool: PgPool) {
    for status in ["idle", "paused", "stopping"] {
        set_state(&pool, status, false).await;
        match control::pause(&pool).await {
            Err(ControlError::WrongState(msg)) => {
                assert_eq!(msg, format!("Cannot pause enrichment in {status} state"));
            }
            other => panic!("{status}: {other:?}"),
        }
        assert_eq!(
            enrichment::state(&pool).await.unwrap().status.as_str(),
            status
        );
    }
    set_state(&pool, "running", false).await;
    let st = control::pause(&pool).await.unwrap();
    assert_eq!(st.status, Status::Paused);
    assert!(st.paused_at.is_some());
    // Persisted, and pausing kicks nothing.
    assert_eq!(
        enrichment::state(&pool).await.unwrap().status,
        Status::Paused
    );
    assert_eq!(queued_cycles(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resume_from_paused_noop_when_running(pool: PgPool) {
    for status in ["idle", "stopping"] {
        set_state(&pool, status, false).await;
        match control::resume(&pool).await {
            Err(ControlError::WrongState(msg)) => {
                assert_eq!(msg, format!("Cannot resume enrichment in {status} state"));
            }
            other => panic!("{status}: {other:?}"),
        }
    }
    assert_eq!(
        queued_cycles(&pool).await,
        0,
        "a refused resume kicks nothing"
    );

    set_state(&pool, "paused", false).await;
    let st = control::resume(&pool).await.unwrap();
    assert_eq!(st.status, Status::Running);
    assert_eq!(queued_cycles(&pool).await, 1, "resume kicks a cycle");

    // Idempotent when running.
    let before = enrichment::state(&pool).await.unwrap();
    let st = control::resume(&pool).await.unwrap();
    assert_eq!(st, before);
    assert_eq!(queued_cycles(&pool).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stop_idempotent_when_idle(pool: PgPool) {
    let before = enrichment::state(&pool).await.unwrap();
    assert_eq!(before.status, Status::Idle);
    let st = control::stop(&pool).await.unwrap();
    assert_eq!(st, before, "stop while idle changes nothing");
    assert!(!st.halted);

    for status in ["running", "paused"] {
        set_state(&pool, status, false).await;
        let st = control::stop(&pool).await.unwrap();
        assert_eq!(st.status, Status::Stopping, "{status}");
        assert!(st.halted);
        assert!(st.stopped_at.is_some());
    }
    // The woken cycle finishes the stop and does nothing while halted.
    assert_eq!(queued_cycles(&pool).await, 1);
    let (ctx, _dir) = ctx(&pool);
    let report = coordinator::run_cycle(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!report.ran);
    let st = enrichment::state(&pool).await.unwrap();
    assert_eq!(st.status, Status::Idle);
    assert!(st.halted, "only sync lifts a stop");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sync_clears_halt_and_counts(pool: PgPool) {
    // Two pending artists with present tracks, one completed, and an artist with no
    // present track (not counted).
    let t1 = track(&pool).await;
    let t2 = track(&pool).await;
    let t3 = track(&pool).await;
    let t4 = track(&pool).await;
    // The helper's artists are `completed`; these are owed but t3's.
    sqlx::query("UPDATE artists SET enrichment_status = 'pending'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE artists SET enrichment_status = 'completed'
         WHERE id = (SELECT al.artist_id FROM tracks t JOIN albums al ON al.id = t.album_id
                     WHERE t.id = $1)",
    )
    .bind(t3)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(t4)
        .execute(&pool)
        .await
        .unwrap();
    // Recording owed: t1 (no row) and t3 (pending); t2 answered.
    put(&pool, t2, "recording", "done", "NULL", None, None).await;
    put(&pool, t3, "recording", "pending", "NULL", None, None).await;
    let _ = t1;

    for status in ["paused", "stopping", "idle"] {
        set_state(&pool, status, true).await;
        sqlx::query("UPDATE enrichment_state SET paused_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        let counts = control::sync(&pool).await.unwrap();
        assert_eq!(
            counts,
            control::SyncCounts {
                artists: 2,
                tracks: 2
            },
            "{status}"
        );
        let st = enrichment::state(&pool).await.unwrap();
        assert!(!st.halted, "{status}");
        assert_eq!(st.status, Status::Idle, "{status}");
        assert!(st.paused_at.is_none(), "{status}");
    }
    assert_eq!(queued_cycles(&pool).await, 1, "sync kicks");
    // A running pipeline stays running.
    set_state(&pool, "running", false).await;
    control::sync(&pool).await.unwrap();
    assert_eq!(
        enrichment::state(&pool).await.unwrap().status,
        Status::Running
    );
}

fn state_with(status: Status, last_activity: OffsetDateTime) -> State {
    State {
        status,
        halted: false,
        current_phase: None,
        started_at: None,
        paused_at: None,
        stopped_at: None,
        last_error: None,
        last_activity,
    }
}

#[test]
fn hung_after_15_minutes() {
    let now = OffsetDateTime::now_utc();
    let old = now - Duration::minutes(15) - Duration::seconds(1);
    assert!(coordinator::hung(&state_with(Status::Running, old), now));
    assert!(!coordinator::hung(
        &state_with(Status::Running, now - Duration::minutes(15)),
        now
    ));
    assert!(!coordinator::hung(
        &state_with(Status::Running, now - Duration::minutes(5)),
        now
    ));
    for status in [Status::Idle, Status::Paused, Status::Stopping] {
        assert!(
            !coordinator::hung(&state_with(status, old), now),
            "{status:?}"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_pause_and_resume_never_leave_an_invalid_state(pool: PgPool) {
    set_state(&pool, "running", false).await;
    let mut tasks = Vec::new();
    for i in 0..24 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            if i % 2 == 0 {
                control::pause(&pool).await.map(|_| ())
            } else {
                control::resume(&pool).await.map(|_| ())
            }
        }));
    }
    for t in tasks {
        match t.await.unwrap() {
            Ok(()) => {}
            Err(ControlError::WrongState(msg)) => {
                // Only a pause can be refused here (it found the pipeline paused).
                assert_eq!(msg, "Cannot pause enrichment in paused state");
            }
            Err(e) => panic!("{e:?}"),
        }
    }
    let st = enrichment::state(&pool).await.unwrap();
    assert!(matches!(st.status, Status::Running | Status::Paused));
    assert!(st.paused_at.is_some());
}

/// Carry (Task 18 m6): a cycle that completes without a batch error clears `last_error`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn successful_cycle_clears_last_error(pool: PgPool) {
    let (mut ctx, _dir) = ctx(&pool);
    ctx.config.fingerprinting = false;
    enrichment::record_error(&pool, "tracks phase: old trouble")
        .await
        .unwrap();
    // A cycle that does not run (halted) leaves the error for the operator to see.
    set_state(&pool, "idle", true).await;
    coordinator::run_cycle(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert!(enrichment::state(&pool).await.unwrap().last_error.is_some());

    set_state(&pool, "idle", false).await;
    *ctx.last_cycle.lock().unwrap() = None;
    let report = coordinator::run_cycle(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert!(report.ran);
    assert_eq!(report.errors, 0);
    assert_eq!(enrichment::state(&pool).await.unwrap().last_error, None);
}

// ---- retry-failed-and-blocked ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retry_reopens_skipped_acoustid_once_a_key_exists(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let no_key = track(&pool).await;
    put(
        &pool,
        no_key,
        "recording",
        "done",
        "NULL",
        None,
        Some(skipped("no_key")),
    )
    .await;
    let no_fp = track(&pool).await;
    put(
        &pool,
        no_fp,
        "recording",
        "done",
        "NULL",
        None,
        Some(skipped("no_fingerprint")),
    )
    .await;
    let unavailable = track(&pool).await;
    put(
        &pool,
        unavailable,
        "recording",
        "done",
        "NULL",
        None,
        Some(skipped("fingerprinting_unavailable")),
    )
    .await;
    let disabled = track(&pool).await;
    put(
        &pool,
        disabled,
        "recording",
        "done",
        "NULL",
        None,
        Some(skipped("disabled")),
    )
    .await;
    // An answered no-match that did ask AcoustID stays closed.
    let asked = track(&pool).await;
    put(
        &pool,
        asked,
        "recording",
        "done",
        "NULL",
        None,
        Some(json!({"reason": "no match", "asked": ["file_tag", "acoustid"]})),
    )
    .await;

    // No key, no fingerprint, fingerprinting off, AcoustID disabled: nothing holds less.
    let mut off = ctx.clone();
    off.config.fingerprinting = false;
    off.config.acoustid_disabled = true;
    assert_eq!(control::retry_failed_and_blocked(&off).await.unwrap(), 0);
    for t in [no_key, no_fp, unavailable, disabled, asked] {
        assert_eq!(status_of(&pool, t, "recording").await, "done");
    }
    assert_eq!(
        queued_cycles(&pool).await,
        1,
        "the control kicks even when nothing reopened"
    );

    // A key is configured (through the settings store, as the settings route writes it).
    settings_kv::set_secret(&pool, &ctx.cipher, Key::AcoustidApiKey, Some("k3y"))
        .await
        .unwrap();
    off.settings
        .replace(settings_kv::load(&pool, &ctx.cipher).await.unwrap());
    assert_eq!(control::retry_failed_and_blocked(&off).await.unwrap(), 1);
    let row = stage_row(&pool, no_key, "recording").await;
    assert_eq!(row["status"], "pending");
    assert_eq!(row["next_attempt_at"], Value::Null);
    assert_eq!(row["started_at"], Value::Null);
    assert_eq!(status_of(&pool, no_fp, "recording").await, "done");

    // A fingerprint is now stored for no_fp.
    sqlx::query(
        "INSERT INTO track_acoustid (track_id, fingerprint, duration_secs) VALUES ($1, 'AQAA', 30)",
    )
    .bind(no_fp)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(control::retry_failed_and_blocked(&off).await.unwrap(), 1);
    assert_eq!(status_of(&pool, no_fp, "recording").await, "pending");

    // Fingerprinting available and AcoustID enabled again.
    let mut on = off.clone();
    on.config.fingerprinting = true;
    on.config.acoustid_disabled = false;
    assert_eq!(control::retry_failed_and_blocked(&on).await.unwrap(), 2);
    assert_eq!(status_of(&pool, unavailable, "recording").await, "pending");
    assert_eq!(status_of(&pool, disabled, "recording").await, "pending");
    assert_eq!(status_of(&pool, asked, "recording").await, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retry_reopens_only_due_failures(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let later = track(&pool).await;
    put(
        &pool,
        later,
        "fingerprint",
        "failed",
        RETRY_LATER,
        Some("INCONCLUSIVE"),
        None,
    )
    .await;
    let past = track(&pool).await;
    put(
        &pool,
        past,
        "recording",
        "failed",
        RETRY_PAST,
        Some("INCONCLUSIVE"),
        None,
    )
    .await;
    let terminal = track(&pool).await;
    put(
        &pool,
        terminal,
        "recording",
        "failed",
        "NULL",
        Some("INCONCLUSIVE"),
        None,
    )
    .await;
    let terminal_before = stage_row(&pool, terminal, "recording").await;

    assert_eq!(control::retry_failed_and_blocked(&ctx).await.unwrap(), 2);
    for (t, stage) in [(later, "fingerprint"), (past, "recording")] {
        let row = stage_row(&pool, t, stage).await;
        assert_eq!(row["status"], "pending", "{stage}");
        assert_eq!(row["next_attempt_at"], Value::Null, "due now");
        // The attempt count carries on: the ladder is not reset.
        assert_eq!(row["attempts"], 2);
    }
    assert_eq!(
        stage_row(&pool, terminal, "recording").await,
        terminal_before
    );
    // Due now: the claim offers them.
    let claimed = stages::claim(&pool, Stage::Fingerprint, 10, true)
        .await
        .unwrap();
    assert!(claimed.iter().any(|c| c.track_id == later));
    assert_eq!(queued_cycles(&pool).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retry_never_touches_done_or_terminal(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let mut rows = Vec::new();
    for (stage, status, next, class) in [
        ("recording", "done", "NULL", None),
        ("fingerprint", "done", "NULL", None),
        ("recording", "failed", "NULL", Some("PERMANENT")),
        ("fingerprint", "failed", "NULL", Some("DAMAGED")),
        ("fingerprint", "failed", "NULL", Some("FILE_MISSING")),
        ("recording", "working", RETRY_LATER, None),
        ("recording", "pending", RETRY_LATER, Some("RATE_LIMITED")),
    ] {
        let t = track(&pool).await;
        put(&pool, t, stage, status, next, class, None).await;
        rows.push((t, stage, stage_row(&pool, t, stage).await));
    }
    assert_eq!(control::retry_failed_and_blocked(&ctx).await.unwrap(), 0);
    for (t, stage, before) in rows {
        assert_eq!(stage_row(&pool, t, stage).await, before);
    }
}

// ---- re-decide-everything ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redecide_resets_counters_keeps_matched_recordings(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    // Identified already: its recording answer is kept (AcoustID dedupe).
    let identified = track(&pool).await;
    sqlx::query("UPDATE tracks SET recording_mbid = gen_random_uuid(), recording_source = 'search' WHERE id = $1")
        .bind(identified)
        .execute(&pool)
        .await
        .unwrap();
    put(&pool, identified, "fingerprint", "done", "NULL", None, None).await;
    put(
        &pool,
        identified,
        "recording",
        "done",
        "NULL",
        None,
        Some(json!({"rung": "search"})),
    )
    .await;
    let identified_rec = stage_row(&pool, identified, "recording").await;
    // Never identified: fair to re-ask, done included.
    let unknown = track(&pool).await;
    put(&pool, unknown, "fingerprint", "done", "NULL", None, None).await;
    put(
        &pool,
        unknown,
        "recording",
        "done",
        "NULL",
        None,
        Some(skipped("no_key")),
    )
    .await;
    // Terminal and retrying failures and a claim in flight are all reopened.
    let closed = track(&pool).await;
    put(
        &pool,
        closed,
        "fingerprint",
        "failed",
        "NULL",
        Some("DAMAGED"),
        None,
    )
    .await;
    put(
        &pool,
        closed,
        "recording",
        "failed",
        RETRY_LATER,
        Some("INCONCLUSIVE"),
        None,
    )
    .await;
    let flying = track(&pool).await;
    put(
        &pool,
        flying,
        "recording",
        "working",
        RETRY_LATER,
        None,
        None,
    )
    .await;

    let reopened = control::redecide_everything(&ctx).await.unwrap();
    assert_eq!(reopened, 6, "every row but the identified recording");
    assert_eq!(
        stage_row(&pool, identified, "recording").await,
        identified_rec
    );
    for (t, stage) in [
        (identified, "fingerprint"),
        (unknown, "fingerprint"),
        (unknown, "recording"),
        (closed, "fingerprint"),
        (closed, "recording"),
        (flying, "recording"),
    ] {
        let row = stage_row(&pool, t, stage).await;
        assert_eq!(row["status"], "pending", "{stage}");
        assert_eq!(row["attempts"], 0);
        assert_eq!(row["redelivered"], 0);
        assert_eq!(row["next_attempt_at"], Value::Null);
        assert_eq!(row["last_error"], Value::Null);
        assert_eq!(row["error_code"], Value::Null);
        assert_eq!(row["error_class"], Value::Null);
        assert_eq!(row["evidence"], Value::Null);
        assert_eq!(row["started_at"], Value::Null);
        assert_eq!(row["finished_at"], Value::Null);
    }
    // Nothing deleted; the identified track keeps its answer; a cycle is kicked.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM track_stages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 7);
    let mbid: Option<Uuid> = sqlx::query_scalar("SELECT recording_mbid FROM tracks WHERE id = $1")
        .bind(identified)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(mbid.is_some());
    assert_eq!(queued_cycles(&pool).await, 1);
}

/// Upstream `unifiedEnrichment.noWipe`: re-deciding and retrying delete nothing.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redecide_and_retry_delete_nothing(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let t = track(&pool).await;
    put(&pool, t, "fingerprint", "done", "NULL", None, None).await;
    put(&pool, t, "recording", "done", "NULL", None, None).await;
    sqlx::query(
        "INSERT INTO track_acoustid (track_id, fingerprint, duration_secs) VALUES ($1, 'AQAA', 30)",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    kima_db::genres::replace_track_genres(
        &mut conn,
        t,
        kima_db::genres::GenreSource::MusicBrainz,
        &["rock".to_string()],
    )
    .await
    .unwrap();
    drop(conn);
    let counts = || async {
        let row = sqlx::query(
            "SELECT (SELECT count(*) FROM track_acoustid) AS fp,
                    (SELECT count(*) FROM track_genres) AS g,
                    (SELECT count(*) FROM track_stages) AS s,
                    (SELECT count(*) FROM genres) AS gs",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        (
            row.get::<i64, _>("fp"),
            row.get::<i64, _>("g"),
            row.get::<i64, _>("s"),
            row.get::<i64, _>("gs"),
        )
    };
    let before = counts().await;
    control::redecide_everything(&ctx).await.unwrap();
    assert_eq!(counts().await, before);
    control::retry_failed_and_blocked(&ctx).await.unwrap();
    control::redecide_everything(&ctx).await.unwrap();
    assert_eq!(counts().await, before);
}

/// Upstream `unifiedEnrichment.missingSkipped`: a missing track is never offered, whatever
/// the controls reopen, and is offered again once the scan clears the flag.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn controls_never_offer_missing_tracks(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let gone = track(&pool).await;
    put(
        &pool,
        gone,
        "fingerprint",
        "failed",
        RETRY_PAST,
        Some("INCONCLUSIVE"),
        None,
    )
    .await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    control::retry_failed_and_blocked(&ctx).await.unwrap();
    control::redecide_everything(&ctx).await.unwrap();
    assert!(
        stages::claim(&pool, Stage::Fingerprint, 10, true)
            .await
            .unwrap()
            .is_empty()
    );
    let p = enrichment::progress(&pool, true).await.unwrap();
    assert_eq!(p.fingerprints.waiting_missing, 1);
    assert_eq!(p.fingerprints.pending, 0);

    sqlx::query("UPDATE tracks SET missing_since = NULL WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    let claimed = stages::claim(&pool, Stage::Fingerprint, 10, true)
        .await
        .unwrap();
    assert_eq!(
        claimed.iter().map(|c| c.track_id).collect::<Vec<_>>(),
        vec![gone]
    );
}

// ---- wipe-and-rerun ----

const VA_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

struct Wiped {
    one: Uuid,
    failing: Uuid,
    va: Uuid,
    album: Uuid,
    trk: Uuid,
    id3: Uuid,
    asking: Uuid,
    asking_started: OffsetDateTime,
    terminal: Uuid,
    retrying: Uuid,
    waiting: Uuid,
    gone: Uuid,
    user: Uuid,
}

async fn exec(pool: &PgPool, sql: &str, ids: &[Uuid]) {
    let mut q = sqlx::query(sql);
    for id in ids {
        q = q.bind(id);
    }
    q.execute(pool).await.unwrap();
}

async fn genre(pool: &PgPool, table: &str, col: &str, id: Uuid, source: &str, name: &str) {
    let g = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO genres (id, name, key) VALUES ($1, $2, $2) ON CONFLICT (key) DO NOTHING",
    )
    .bind(g)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "INSERT INTO {table} ({col}, genre_id, source, rank) \
         SELECT $1, id, $2, 0 FROM genres WHERE key = $3"
    ))
    .bind(id)
    .bind(source)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

/// The library `enrichmentRestart.integration` seeds, in kima-rs's schema.
async fn seed(pool: &PgPool) -> Wiped {
    let one = Uuid::new_v4();
    exec(
        pool,
        &format!(
            "INSERT INTO artists (id, name, name_key, mbid, mbid_source, bio, image_path, image_source,
                 \"similar\", enrichment_status, enrichment_failures, last_enriched_at, last_error,
                 last_error_code, display_name, user_bio, user_genres)
             VALUES ($1, 'Wipe One', 'wipe one', '{}', 'tag', 'An old bio', 'one.jpg', 'wikidata',
                 '[{{\"name\":\"Wipe Two\",\"mbid\":null,\"match\":0.5}}]', 'completed', 0, now(),
                 'old trouble', 'TIMEOUT', 'My Name', 'My own words', '{{mine}}')",
            Uuid::new_v4()
        ),
        &[one],
    )
    .await;
    let failing = Uuid::new_v4();
    exec(
        pool,
        "INSERT INTO artists (id, name, name_key, enrichment_status, enrichment_failures,
             last_enriched_at, last_error) VALUES ($1, 'Failing', 'failing', 'failed', 2, now(), 'x')",
        &[failing],
    )
    .await;
    let va = Uuid::new_v4();
    exec(
        pool,
        &format!(
            "INSERT INTO artists (id, name, name_key, mbid, enrichment_status)
             VALUES ($1, 'Various Artists', 'various artists', '{VA_MBID}', 'completed')"
        ),
        &[va],
    )
    .await;
    genre(
        pool,
        "artist_genres",
        "artist_id",
        one,
        "musicbrainz",
        "mb-artist",
    )
    .await;
    genre(
        pool,
        "artist_genres",
        "artist_id",
        one,
        "file",
        "file-artist",
    )
    .await;

    let album = Uuid::new_v4();
    exec(
        pool,
        &format!(
            "INSERT INTO albums (id, artist_id, title, title_key, release_mbid, release_mbid_source,
                 release_group_mbid, release_group_mbid_source, label, image_path, image_source,
                 display_title, display_year, user_genres)
             VALUES ($1, $2, 'Wipe Album', 'wipe album', '{}', 'tag', '{}', 'tag', 'Label',
                 'cover.jpg', 'deezer', 'My Album', 1999, '{{mine}}')",
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
        &[album, one],
    )
    .await;
    genre(
        pool,
        "album_genres",
        "album_id",
        album,
        "musicbrainz",
        "mb-album",
    )
    .await;

    let mk = |title: &'static str, missing: bool| {
        let pool = pool.clone();
        async move {
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime,
                     fingerprint, duration_secs, tag_recording_mbid, missing_since)
                 VALUES ($1, $2, 'Wipe One', $3, 'flac', $4, 1, now(), '\\x00', 200,
                     gen_random_uuid(), CASE WHEN $5 THEN now() ELSE NULL END)",
            )
            .bind(id)
            .bind(album)
            .bind(title)
            .bind(id.to_string())
            .bind(missing)
            .execute(&pool)
            .await
            .unwrap();
            id
        }
    };
    let trk = mk("Wipe Track", false).await;
    let id3 = mk("Tagged ISRC", false).await;
    let asking = mk("Being Asked", false).await;
    let terminal = mk("Closed", false).await;
    let retrying = mk("Retrying", false).await;
    let waiting = mk("Waiting", false).await;
    let gone = mk("Gone Track", true).await;
    exec(
        pool,
        "UPDATE tracks SET recording_mbid = gen_random_uuid(), recording_source = 'search',
             isrc = 'GBAYE0601498', isrc_source = 'musicbrainz' WHERE id = $1",
        &[trk],
    )
    .await;
    exec(
        pool,
        "UPDATE tracks SET recording_mbid = gen_random_uuid(), recording_source = 'isrc',
             isrc = 'USRC17607839', isrc_source = 'id3' WHERE id = $1",
        &[id3],
    )
    .await;
    exec(
        pool,
        "UPDATE tracks SET recording_mbid = gen_random_uuid(), recording_source = 'file_tag'
         WHERE id = $1",
        &[gone],
    )
    .await;
    // A terminal recording that still holds an earlier answer: not reopened, so not cleared.
    exec(
        pool,
        "UPDATE tracks SET recording_mbid = gen_random_uuid(), recording_source = 'search',
             isrc = 'GBAYE0601499', isrc_source = 'musicbrainz' WHERE id = $1",
        &[terminal],
    )
    .await;
    for t in [trk, id3, asking, terminal, retrying, waiting, gone] {
        exec(
            pool,
            "INSERT INTO track_acoustid (track_id, fingerprint, duration_secs) VALUES ($1, 'AQAA', 200)",
            &[t],
        )
        .await;
        put(
            pool,
            t,
            "fingerprint",
            "done",
            "NULL",
            None,
            Some(json!({"chars": 4})),
        )
        .await;
    }
    genre(
        pool,
        "track_genres",
        "track_id",
        trk,
        "musicbrainz",
        "mb-track",
    )
    .await;
    genre(pool, "track_genres", "track_id", trk, "file", "file-track").await;
    genre(
        pool,
        "track_genres",
        "track_id",
        terminal,
        "musicbrainz",
        "mb-closed",
    )
    .await;
    put(
        pool,
        trk,
        "recording",
        "done",
        "NULL",
        None,
        Some(json!({"rung": "search"})),
    )
    .await;
    put(pool, id3, "recording", "done", "NULL", None, None).await;
    put(pool, gone, "recording", "done", "NULL", None, None).await;
    put(
        pool,
        asking,
        "recording",
        "working",
        RETRY_LATER,
        None,
        None,
    )
    .await;
    put(
        pool,
        terminal,
        "recording",
        "failed",
        "NULL",
        Some("PERMANENT"),
        None,
    )
    .await;
    put(
        pool,
        retrying,
        "recording",
        "failed",
        RETRY_LATER,
        Some("INCONCLUSIVE"),
        None,
    )
    .await;
    put(
        pool,
        waiting,
        "recording",
        "pending",
        RETRY_LATER,
        Some("RATE_LIMITED"),
        None,
    )
    .await;
    // A reserved stage (M4) is not this wipe's.
    put(pool, trk, "analysis", "done", "NULL", None, None).await;
    let asking_started: OffsetDateTime = sqlx::query_scalar(
        "SELECT started_at FROM track_stages WHERE track_id = $1 AND stage = 'recording'",
    )
    .bind(asking)
    .fetch_one(pool)
    .await
    .unwrap();

    // Everything people made.
    let user = Uuid::new_v4();
    exec(
        pool,
        "INSERT INTO users (id, username, password_hash) VALUES ($1, 'wiper', 'x')",
        &[user],
    )
    .await;
    exec(
        pool,
        "INSERT INTO plays (id, user_id, track_id) VALUES (gen_random_uuid(), $1, $2)",
        &[user, trk],
    )
    .await;
    let pl = Uuid::new_v4();
    exec(
        pool,
        "INSERT INTO playlists (id, user_id, name) VALUES ($1, $2, 'Mine')",
        &[pl, user],
    )
    .await;
    exec(
        pool,
        "INSERT INTO playlist_items (id, playlist_id, track_id, sort) VALUES (gen_random_uuid(), $1, $2, 0)",
        &[pl, trk],
    )
    .await;
    exec(
        pool,
        "INSERT INTO stars (id, user_id, track_id) VALUES (gen_random_uuid(), $1, $2)",
        &[user, trk],
    )
    .await;

    Wiped {
        one,
        failing,
        va,
        album,
        trk,
        id3,
        asking,
        asking_started,
        terminal,
        retrying,
        waiting,
        gone,
        user,
    }
}

/// `id -> row as JSON minus the listed columns`, over a whole table.
async fn snapshot(
    pool: &PgPool,
    table: &str,
    key: &str,
    minus: &[&str],
) -> BTreeMap<String, Value> {
    let minus: Vec<String> = minus.iter().map(|s| s.to_string()).collect();
    sqlx::query(&format!(
        "SELECT ({key})::text AS k, to_jsonb(x) - $1::text[] AS j FROM {table} x"
    ))
    .bind(&minus)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| (r.get("k"), r.get("j")))
    .collect()
}

async fn artist_col(pool: &PgPool, id: Uuid, col: &str) -> Value {
    sqlx::query_scalar(&format!(
        "SELECT to_jsonb(a) -> '{col}' FROM artists a WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn track_col(pool: &PgPool, id: Uuid, col: &str) -> Value {
    sqlx::query_scalar(&format!(
        "SELECT to_jsonb(t) -> '{col}' FROM tracks t WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn genre_sources(pool: &PgPool, table: &str, col: &str, id: Uuid) -> Vec<String> {
    sqlx::query_scalar(&format!(
        "SELECT source FROM {table} WHERE {col} = $1 ORDER BY source"
    ))
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_touches_only_listed_columns(pool: PgPool) {
    seed(&pool).await;
    let artists_cols = [
        "bio",
        "similar",
        "enrichment_status",
        "enrichment_failures",
        "last_enriched_at",
    ];
    let track_cols = ["recording_mbid", "recording_source", "isrc", "isrc_source"];
    let artists = snapshot(&pool, "artists", "id", &artists_cols).await;
    let albums = snapshot(&pool, "albums", "id", &[]).await;
    let tracks = snapshot(&pool, "tracks", "id", &track_cols).await;
    let fingerprints = snapshot(&pool, "track_acoustid", "track_id", &[]).await;
    let open_stages = snapshot(
        &pool,
        "(SELECT * FROM track_stages WHERE NOT (stage = 'recording' AND status IN ('done', 'working'))) ",
        "track_id || stage",
        &[],
    )
    .await;
    let others = snapshot(
        &pool,
        "(SELECT 'genres' AS t, count(*) AS n FROM genres UNION ALL
          SELECT 'album_genres', count(*) FROM album_genres UNION ALL
          SELECT 'artist_genres_file', count(*) FROM artist_genres WHERE source = 'file' UNION ALL
          SELECT 'track_genres_file', count(*) FROM track_genres WHERE source = 'file' UNION ALL
          SELECT 'plays', count(*) FROM plays UNION ALL
          SELECT 'playlist_items', count(*) FROM playlist_items UNION ALL
          SELECT 'stars', count(*) FROM stars UNION ALL
          SELECT 'users', count(*) FROM users)",
        "t",
        &[],
    )
    .await;

    control::wipe_and_rerun(&pool).await.unwrap();

    assert_eq!(
        snapshot(&pool, "artists", "id", &artists_cols).await,
        artists
    );
    assert_eq!(snapshot(&pool, "albums", "id", &[]).await, albums);
    assert_eq!(snapshot(&pool, "tracks", "id", &track_cols).await, tracks);
    assert_eq!(
        snapshot(&pool, "track_acoustid", "track_id", &[]).await,
        fingerprints
    );
    assert_eq!(
        snapshot(
            &pool,
            "(SELECT * FROM track_stages WHERE NOT (stage = 'recording' AND status IN ('done', 'working')) AND evidence IS DISTINCT FROM '{\"reason\":\"enrichment wiped\"}') ",
            "track_id || stage",
            &[],
        )
        .await,
        open_stages
    );
    assert_eq!(
        snapshot(
            &pool,
            "(SELECT 'genres' AS t, count(*) AS n FROM genres UNION ALL
              SELECT 'album_genres', count(*) FROM album_genres UNION ALL
              SELECT 'artist_genres_file', count(*) FROM artist_genres WHERE source = 'file' UNION ALL
              SELECT 'track_genres_file', count(*) FROM track_genres WHERE source = 'file' UNION ALL
              SELECT 'plays', count(*) FROM plays UNION ALL
              SELECT 'playlist_items', count(*) FROM playlist_items UNION ALL
              SELECT 'stars', count(*) FROM stars UNION ALL
              SELECT 'users', count(*) FROM users)",
            "t",
            &[],
        )
        .await,
        others
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_keeps_mbid_image_and_fingerprints(pool: PgPool) {
    let w = seed(&pool).await;
    let mbid = artist_col(&pool, w.one, "mbid").await;
    control::wipe_and_rerun(&pool).await.unwrap();
    assert_eq!(artist_col(&pool, w.one, "mbid").await, mbid);
    assert_eq!(artist_col(&pool, w.one, "mbid_source").await, "tag");
    assert_eq!(artist_col(&pool, w.one, "image_path").await, "one.jpg");
    assert_eq!(artist_col(&pool, w.one, "image_source").await, "wikidata");
    let alb = sqlx::query(
        "SELECT release_mbid IS NOT NULL AS r, release_group_mbid IS NOT NULL AS g, image_path
         FROM albums WHERE id = $1",
    )
    .bind(w.album)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(alb.get::<bool, _>("r") && alb.get::<bool, _>("g"));
    assert_eq!(alb.get::<String, _>("image_path"), "cover.jpg");
    // The fingerprints and their stage stay; the tag's recording id too.
    let fps: i64 =
        sqlx::query_scalar("SELECT count(*) FROM track_acoustid WHERE fingerprint = 'AQAA'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(fps, 7);
    assert_eq!(status_of(&pool, w.trk, "fingerprint").await, "done");
    assert_ne!(
        track_col(&pool, w.trk, "tag_recording_mbid").await,
        Value::Null
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_clears_bio_similar_and_mb_genres(pool: PgPool) {
    let w = seed(&pool).await;
    let counts = control::wipe_and_rerun(&pool).await.unwrap();
    for col in ["bio", "similar", "last_enriched_at"] {
        assert_eq!(artist_col(&pool, w.one, col).await, Value::Null, "{col}");
    }
    assert_eq!(
        artist_col(&pool, w.one, "enrichment_status").await,
        "pending"
    );
    assert_eq!(artist_col(&pool, w.one, "enrichment_failures").await, 0);
    assert_eq!(
        artist_col(&pool, w.failing, "enrichment_status").await,
        "pending"
    );
    assert_eq!(artist_col(&pool, w.failing, "enrichment_failures").await, 0);
    // MusicBrainz genres go; the file's stay; album genres are not the wipe's.
    assert_eq!(
        genre_sources(&pool, "artist_genres", "artist_id", w.one).await,
        ["file"]
    );
    assert_eq!(
        genre_sources(&pool, "track_genres", "track_id", w.trk).await,
        ["file"]
    );
    assert_eq!(
        genre_sources(&pool, "album_genres", "album_id", w.album).await,
        ["musicbrainz"]
    );
    // A terminal recording keeps its genres: its row was not reopened.
    assert_eq!(
        genre_sources(&pool, "track_genres", "track_id", w.terminal).await,
        ["musicbrainz"]
    );
    assert_eq!(counts.artists, 2, "every artist but Various Artists");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_keeps_various_artists_completed(pool: PgPool) {
    let w = seed(&pool).await;
    control::wipe_and_rerun(&pool).await.unwrap();
    assert_eq!(
        artist_col(&pool, w.va, "enrichment_status").await,
        "completed"
    );
    assert_eq!(artist_col(&pool, w.va, "mbid").await, VA_MBID);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_reopens_done_and_working_recordings_only(pool: PgPool) {
    let w = seed(&pool).await;
    let untouched = [w.terminal, w.retrying, w.waiting];
    let mut before = Vec::new();
    for t in untouched {
        before.push(stage_row(&pool, t, "recording").await);
    }
    let counts = control::wipe_and_rerun(&pool).await.unwrap();
    assert_eq!(counts.tracks, 4);
    for t in [w.trk, w.id3, w.asking, w.gone] {
        let row = stage_row(&pool, t, "recording").await;
        assert_eq!(row["status"], "pending");
        assert_eq!(row["attempts"], 0);
        assert_eq!(row["redelivered"], 0);
        assert_eq!(row["next_attempt_at"], Value::Null);
        assert_eq!(row["started_at"], Value::Null);
        assert_eq!(row["finished_at"], Value::Null);
        assert_eq!(row["evidence"], json!({"reason": "enrichment wiped"}));
        assert_eq!(track_col(&pool, t, "recording_mbid").await, Value::Null);
        assert_eq!(track_col(&pool, t, "recording_source").await, Value::Null);
    }
    for (t, b) in untouched.into_iter().zip(before) {
        assert_eq!(stage_row(&pool, t, "recording").await, b);
    }
}

/// Upstream "reopens an identification claimed when the wipe came": the late report of the
/// claim the wipe reopened is refused.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_reopens_a_claimed_identification(pool: PgPool) {
    let w = seed(&pool).await;
    control::wipe_and_rerun(&pool).await.unwrap();
    let claim = stages::Claim {
        track_id: w.asking,
        stage: Stage::Recording,
        started_at: w.asking_started,
        attempts: 2,
        redelivered: 1,
    };
    let mut conn = pool.acquire().await.unwrap();
    let accepted = stages::report_done(&mut conn, &claim, "search", &json!({}))
        .await
        .unwrap();
    assert!(!accepted, "the old claim no longer holds");
    assert_eq!(status_of(&pool, w.asking, "recording").await, "pending");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_clears_only_musicbrainz_isrc(pool: PgPool) {
    let w = seed(&pool).await;
    let terminal_mbid = track_col(&pool, w.terminal, "recording_mbid").await;
    control::wipe_and_rerun(&pool).await.unwrap();
    assert_eq!(track_col(&pool, w.trk, "isrc").await, Value::Null);
    assert_eq!(track_col(&pool, w.trk, "isrc_source").await, Value::Null);
    assert_eq!(track_col(&pool, w.id3, "isrc").await, "USRC17607839");
    assert_eq!(track_col(&pool, w.id3, "isrc_source").await, "id3");
    // Only reopened tracks are cleared: the terminal one keeps its answer and ISRC.
    assert_ne!(terminal_mbid, Value::Null);
    assert_eq!(
        track_col(&pool, w.terminal, "recording_mbid").await,
        terminal_mbid
    );
    assert_eq!(
        track_col(&pool, w.terminal, "recording_source").await,
        "search"
    );
    assert_eq!(track_col(&pool, w.terminal, "isrc").await, "GBAYE0601499");
    assert_eq!(
        track_col(&pool, w.terminal, "isrc_source").await,
        "musicbrainz"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_never_touches_user_overrides(pool: PgPool) {
    let w = seed(&pool).await;
    control::wipe_and_rerun(&pool).await.unwrap();
    assert_eq!(artist_col(&pool, w.one, "display_name").await, "My Name");
    assert_eq!(artist_col(&pool, w.one, "user_bio").await, "My own words");
    assert_eq!(
        artist_col(&pool, w.one, "user_genres").await,
        json!(["mine"])
    );
    let alb = sqlx::query(
        "SELECT display_title, display_year, user_genres, label FROM albums WHERE id = $1",
    )
    .bind(w.album)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(alb.get::<String, _>("display_title"), "My Album");
    assert_eq!(alb.get::<i32, _>("display_year"), 1999);
    assert_eq!(alb.get::<Vec<String>, _>("user_genres"), ["mine"]);
    assert_eq!(alb.get::<String, _>("label"), "Label");
    // Playlists, plays and stars stay.
    for table in ["plays", "playlists", "stars"] {
        let n: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE user_id = $1"))
                .bind(w.user)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(n, 1, "{table}");
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM playlist_items WHERE track_id = $1")
        .bind(w.trk)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

/// Upstream "drops the jobs queued for the old questions": kima-rs queues no per-item jobs;
/// the wipe leaves exactly one cycle queued, kicked after the commit.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_kicks_one_cycle(pool: PgPool) {
    seed(&pool).await;
    coordinator::kick(&pool).await.unwrap();
    control::wipe_and_rerun(&pool).await.unwrap();
    assert_eq!(queued_cycles(&pool).await, 1);
}

/// Upstream "leaves analysis and sound vectors ... as they left them": stages that are not
/// the recording stage are not the wipe's.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_leaves_other_stages_untouched(pool: PgPool) {
    let w = seed(&pool).await;
    let analysis = stage_row(&pool, w.trk, "analysis").await;
    let fp = stage_row(&pool, w.trk, "fingerprint").await;
    control::wipe_and_rerun(&pool).await.unwrap();
    assert_eq!(stage_row(&pool, w.trk, "analysis").await, analysis);
    assert_eq!(stage_row(&pool, w.trk, "fingerprint").await, fp);
}

/// Upstream "leaves a track whose file is gone closed as missing, so nothing waits on it":
/// its reopened row is never offered and counts as waiting for its file, not as owed work.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_missing_track_waits_for_its_file(pool: PgPool) {
    let w = seed(&pool).await;
    control::wipe_and_rerun(&pool).await.unwrap();
    let claimed = stages::claim(&pool, Stage::Recording, 50, true)
        .await
        .unwrap();
    assert!(!claimed.iter().any(|c| c.track_id == w.gone));
    let p = enrichment::progress(&pool, true).await.unwrap();
    assert_eq!(p.track_tags.waiting_missing, 1);
}

/// Upstream "clears the failures of the wiped steps, and only those".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_clears_artist_failures_keeps_track_failures(pool: PgPool) {
    let w = seed(&pool).await;
    let filter = kima_db::enrichment::FailureFilter {
        entity_type: None,
        limit: 100,
        offset: 0,
    };
    let (before, _) = enrichment::failures(&pool, &filter).await.unwrap();
    assert!(before.iter().any(|f| f.entity_id == w.failing));
    control::wipe_and_rerun(&pool).await.unwrap();
    let (after, total) = enrichment::failures(&pool, &filter).await.unwrap();
    let mut ids: Vec<Uuid> = after.iter().map(|f| f.entity_id).collect();
    ids.sort();
    let mut want = vec![w.terminal, w.retrying];
    want.sort();
    assert_eq!(ids, want);
    assert_eq!(total, 2);
}

// ---- concurrency ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrency_clamped_and_applied(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    for (asked, got) in [(9, 5), (3, 3), (0, 1), (-7, 1), (i64::MAX, 5)] {
        assert_eq!(control::set_concurrency(&ctx, asked).await.unwrap(), got);
        assert_eq!(
            ctx.settings.get().await.unwrap().enrichment_concurrency,
            got
        );
        let stored: Value =
            sqlx::query_scalar("SELECT value FROM settings_kv WHERE key = 'enrichmentConcurrency'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, json!(got));
        for source in [Source::MusicBrainz, Source::AcoustId, Source::Deezer] {
            assert_eq!(
                ctx.net.concurrency(source),
                Some(source.limits().concurrency_for(got)),
                "{source:?}"
            );
        }
    }
}

// ---- review fixes ----

async fn age_activity(pool: &PgPool, status: &str) {
    sqlx::query(
        "UPDATE enrichment_state SET status = $1, halted = false,
             last_activity = now() - interval '1 hour'",
    )
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

/// Review I1: a transition into `running` is activity (upstream `progressSignature` includes
/// the status), so a resume after a long pause is not hung.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resume_after_long_pause_is_not_hung(pool: PgPool) {
    age_activity(&pool, "paused").await;
    let st = control::resume(&pool).await.unwrap();
    assert_eq!(st.status, Status::Running);
    assert!(!coordinator::hung(&st, OffsetDateTime::now_utc()));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn idle_to_running_cycle_start_is_not_hung(pool: PgPool) {
    age_activity(&pool, "idle").await;
    let st = enrichment::begin_cycle(&pool).await.unwrap();
    assert_eq!(st.status, Status::Running);
    assert!(!coordinator::hung(&st, OffsetDateTime::now_utc()));
}

/// A pipeline already running whose counts have not moved is still hung: neither a cycle
/// start nor a resume (a no-op when running) stamps it.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stuck_running_state_is_still_hung(pool: PgPool) {
    age_activity(&pool, "running").await;
    let st = enrichment::begin_cycle(&pool).await.unwrap();
    assert!(coordinator::hung(&st, OffsetDateTime::now_utc()));
    let st = control::resume(&pool).await.unwrap();
    assert!(coordinator::hung(&st, OffsetDateTime::now_utc()));
    // A phase change alone is no heartbeat either.
    enrichment::set_phase(&pool, Some(enrichment::Phase::Tracks), false)
        .await
        .unwrap();
    let st = enrichment::state(&pool).await.unwrap();
    assert!(coordinator::hung(&st, OffsetDateTime::now_utc()));
}

/// Review m4: a kick that fails after the control committed is logged; the control answers
/// success, since its change happened.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn controls_succeed_when_the_kick_fails(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let w = seed(&pool).await;
    sqlx::query("ALTER TABLE jobs RENAME TO jobs_gone")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        coordinator::kick(&pool).await.is_err(),
        "the kick really fails"
    );

    let counts = control::wipe_and_rerun(&pool).await.unwrap();
    assert_eq!(counts.tracks, 4);
    assert_eq!(status_of(&pool, w.trk, "recording").await, "pending");
    control::redecide_everything(&ctx).await.unwrap();
    control::retry_failed_and_blocked(&ctx).await.unwrap();
    control::sync(&pool).await.unwrap();
    set_state(&pool, "paused", false).await;
    assert_eq!(
        control::resume(&pool).await.unwrap().status,
        Status::Running
    );
    assert_eq!(control::stop(&pool).await.unwrap().status, Status::Stopping);
}
