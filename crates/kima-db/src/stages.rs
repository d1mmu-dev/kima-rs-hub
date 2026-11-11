//! The `track_stages` ledger (spec §10.2-§10.4): the work queue's claims and reports, and
//! the reopens the scanner makes (§8.1).
//!
//! Every lease and redelivery decision uses the database clock (`now()`); only a
//! [`FailureRecord`]'s `pending_until`/`retry_at` come from the caller.

use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

/// `last_error` holds at most this many characters (spec §10.4, upstream `MAX_ERROR_CHARS`).
pub const LAST_ERROR_MAX_CHARS: usize = 2000;

/// The stages M3 runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Fingerprint,
    Recording,
    /// CLAP audio embedding (M4): eligible as soon as the track exists, the same
    /// unconditional trigger as [`Stage::Fingerprint`] — never gated on `Recording` or
    /// any future analysis stage, and independent of the MusicBrainz on/off switch.
    Embedding,
    /// MusiCNN + DSP per-track analysis (M4): already reserved in the `track_stages.stage`
    /// CHECK constraint since the original schema (migration `0018_track_stages.sql`) and
    /// in `invalidate_track_ml()`'s reset list (`0024`/`0026`), alongside `Embedding`.
    /// Eligible as soon as the track exists, unconditionally like [`Stage::Fingerprint`]
    /// and [`Stage::Embedding`] — never gated on `Recording` or `Embedding` settling.
    /// Publication is `kima_ml::analysis_publish::publish_track_analysis`, which writes
    /// four independent `track_analysis` column groups outside any single transaction with
    /// this ledger row; closing or failing a claim must use the same lock-row-before-
    /// evaluating-the-lease ordering as `Embedding` so a report cannot race a lease
    /// expiry/redelivery during that gap.
    Analysis,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Fingerprint => "fingerprint",
            Stage::Recording => "recording",
            Stage::Embedding => "embedding",
            Stage::Analysis => "analysis",
        }
    }
}

/// A claimed stage row. `(track_id, stage, started_at)` is the claim token every report
/// carries: a report whose token is no longer current writes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub track_id: Uuid,
    pub stage: Stage,
    pub started_at: OffsetDateTime,
    /// Reported failures so far (rate limits never count).
    pub attempts: i16,
    /// Lapsed claims re-offered so far, this claim included.
    pub redelivered: i16,
}

/// Claim up to `n` due rows of `stage` (spec §10.3), newest files first, creating the rows
/// that do not exist yet. Due: no row; `pending` with no or a past `next_attempt_at`; a
/// `failed` retry or a `working` lease whose `next_attempt_at` has passed. A lapsed lease
/// counts one redelivery. The `recording` stage also waits for the track's fingerprint
/// to settle (`done`, or terminal `failed`), unless `fingerprinting` is unavailable.
///
/// Concurrent claimers never get the same row: the upsert re-checks due-ness against the
/// committed row, so a claim that lost a race to a claimer that already committed takes
/// nothing. Track rows are locked `FOR KEY SHARE ... SKIP LOCKED`: a track being deleted is
/// skipped (its stage row could not be written), but another stage's claim, failure report or
/// publication (`FOR NO KEY UPDATE` on the track) does not hide it. The coordinator runs the
/// stages side by side on the same tracks, and an empty claim ends a phase for the cycle.
///
/// The claim never waits on a stage row someone else holds, so it cannot close a lock cycle
/// with a writer that touches several tracks' stage rows in one transaction (the scanner's
/// `tracks_ml_identity` trigger resets them track by track): existing rows of `stage` are
/// locked `FOR NO KEY UPDATE ... SKIP LOCKED` up front (a row being reset or reported is
/// skipped this time), and new rows are inserted in track-id order, so two claimers that
/// race on the same new rows queue up instead of deadlocking.
pub async fn claim(
    pool: &PgPool,
    stage: Stage,
    n: i64,
    fingerprinting: bool,
) -> sqlx::Result<Vec<Claim>> {
    let rows = sqlx::query!(
        r#"WITH existing AS (
             SELECT t.id, t.mtime FROM tracks t
             JOIN track_stages s ON s.track_id = t.id AND s.stage = $1
             WHERE t.missing_since IS NULL
               AND ((s.status = 'pending'
                     AND (s.next_attempt_at IS NULL OR s.next_attempt_at <= now()))
                    OR (s.status IN ('failed', 'working') AND s.next_attempt_at <= now()))
               AND ($1 <> 'recording' OR NOT $3 OR EXISTS (
                    SELECT 1 FROM track_stages f
                    WHERE f.track_id = t.id AND f.stage = 'fingerprint'
                      AND (f.status = 'done'
                           OR (f.status = 'failed' AND f.next_attempt_at IS NULL))))
             ORDER BY t.mtime DESC, t.id
             LIMIT $2
             FOR KEY SHARE OF t SKIP LOCKED FOR NO KEY UPDATE OF s SKIP LOCKED),
           fresh AS (
             SELECT t.id, t.mtime FROM tracks t
             WHERE t.missing_since IS NULL
               AND NOT EXISTS (SELECT 1 FROM track_stages s
                               WHERE s.track_id = t.id AND s.stage = $1)
               AND ($1 <> 'recording' OR NOT $3 OR EXISTS (
                    SELECT 1 FROM track_stages f
                    WHERE f.track_id = t.id AND f.stage = 'fingerprint'
                      AND (f.status = 'done'
                           OR (f.status = 'failed' AND f.next_attempt_at IS NULL))))
             ORDER BY t.mtime DESC, t.id
             LIMIT $2
             FOR KEY SHARE OF t SKIP LOCKED),
           due AS (
             SELECT id FROM (SELECT id, mtime FROM existing
                             UNION ALL SELECT id, mtime FROM fresh) d
             ORDER BY mtime DESC, id
             LIMIT $2),
           claimed AS (
             INSERT INTO track_stages (track_id, stage, status, next_attempt_at, started_at, updated_at)
             SELECT id, $1, 'working', now() + interval '15 minutes', now(), now() FROM due
             ORDER BY id
             ON CONFLICT (track_id, stage) DO UPDATE SET
               status = 'working',
               redelivered = track_stages.redelivered + (track_stages.status = 'working')::int,
               next_attempt_at = EXCLUDED.next_attempt_at, started_at = now(), updated_at = now()
             WHERE (track_stages.status = 'pending'
                    AND (track_stages.next_attempt_at IS NULL
                         OR track_stages.next_attempt_at <= now()))
                OR (track_stages.status IN ('failed', 'working')
                    AND track_stages.next_attempt_at <= now())
             RETURNING track_id, started_at, attempts, redelivered)
           SELECT c.track_id, c.started_at AS "started_at!", c.attempts, c.redelivered
           FROM claimed c JOIN tracks t ON t.id = c.track_id
           ORDER BY t.mtime DESC, t.id"#,
        stage.as_str(),
        n,
        fingerprinting,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Claim {
            track_id: r.track_id,
            stage,
            started_at: r.started_at,
            attempts: r.attempts,
            redelivered: r.redelivered,
        })
        .collect())
}

/// Close a claim as answered (including "no match"): `done`, errors and `next_attempt_at`
/// cleared, `source`, `evidence` and `finished_at` set. Runs on the caller's connection so
/// the stage's own writes share its transaction. `false` when the claim is no longer
/// current (it lapsed and was re-offered, or was reopened): nothing is written.
pub async fn report_done(
    conn: &mut PgConnection,
    claim: &Claim,
    source: &str,
    evidence: &serde_json::Value,
) -> sqlx::Result<bool> {
    let r = sqlx::query!(
        "UPDATE track_stages
         SET status = 'done', next_attempt_at = NULL,
             last_error = NULL, error_code = NULL, error_class = NULL,
             source = $4, evidence = $5, finished_at = now(), updated_at = now()
         WHERE track_id = $1 AND stage = $2 AND status = 'working' AND started_at = $3",
        claim.track_id,
        claim.stage.as_str(),
        claim.started_at,
        source,
        evidence,
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Gives back claims that were taken but never started (a batch cancelled or stopped before
/// their turn): each goes back to `pending`, due now, its claim token cleared. Claims that are
/// no longer current are left alone. A lapse the claim counted (`redelivered`) stays counted:
/// that earlier worker really was lost. Returns rows changed.
pub async fn release(pool: &PgPool, claims: &[Claim]) -> sqlx::Result<u64> {
    if claims.is_empty() {
        return Ok(0);
    }
    let tracks: Vec<Uuid> = claims.iter().map(|c| c.track_id).collect();
    let stages: Vec<&str> = claims.iter().map(|c| c.stage.as_str()).collect();
    let tokens: Vec<OffsetDateTime> = claims.iter().map(|c| c.started_at).collect();
    let r = sqlx::query!(
        "UPDATE track_stages s
         SET status = 'pending', next_attempt_at = NULL, started_at = NULL, updated_at = now()
         FROM UNNEST($1::uuid[], $2::text[], $3::timestamptz[]) AS c(track_id, stage, started_at)
         WHERE s.track_id = c.track_id AND s.stage = c.stage AND s.status = 'working'
           AND s.started_at = c.started_at",
        &tracks,
        &stages as &[&str],
        &tokens
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// How a failed claim closes. `pending_until` set: parked `pending` until then (a paused
/// source). Otherwise `failed`, due again at `retry_at`, or terminal when it is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRecord {
    pub pending_until: Option<OffsetDateTime>,
    pub retry_at: Option<OffsetDateTime>,
    /// Whether this failure uses up an attempt (rate limits do not).
    pub count_attempt: bool,
    pub code: String,
    pub class: Option<String>,
    /// Raw message; stored cut to [`LAST_ERROR_MAX_CHARS`] characters.
    pub message: String,
}

/// Close a claim as failed per `rec`. Embedding and Analysis claims must also have a
/// live lease after the track and stage locks are acquired (both stages have an
/// out-of-transaction publication step — `ml::complete_embedding`'s single call, or
/// `analysis_publish`'s four independent group writes — that can span the gap between
/// lock wait and lease check). Rejected claims write nothing.
pub async fn report_failure(
    pool: &PgPool,
    claim: &Claim,
    rec: &FailureRecord,
) -> sqlx::Result<bool> {
    if matches!(claim.stage, Stage::Embedding | Stage::Analysis) {
        let mut tx = pool.begin().await?;
        // Match publication and invalidation: track first, then stage. `NO KEY UPDATE` fences
        // the scanner's writes and deletes without hiding the track from other stages' claims.
        let track = sqlx::query("SELECT id FROM tracks WHERE id = $1 FOR NO KEY UPDATE")
            .bind(claim.track_id)
            .fetch_optional(&mut *tx)
            .await?;
        if track.is_none() {
            return Ok(false);
        }
        let stage = sqlx::query(
            "SELECT track_id FROM track_stages WHERE track_id = $1 AND stage = $2 FOR UPDATE",
        )
        .bind(claim.track_id)
        .bind(claim.stage.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if stage.is_none() {
            return Ok(false);
        }
        // Separate statement: evaluate the database clock only after any lock wait.
        let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM track_stages WHERE track_id = $1 AND stage = $2 AND status = 'working' AND started_at = $3 AND next_attempt_at > clock_timestamp())")
            .bind(claim.track_id)
            .bind(claim.stage.as_str())
            .bind(claim.started_at)
            .fetch_one(&mut *tx)
            .await?;
        if !valid {
            return Ok(false);
        }
        let result = report_failure_on(&mut tx, claim, rec).await?;
        tx.commit().await?;
        return Ok(result);
    }
    let mut conn = pool.acquire().await?;
    report_failure_on(&mut conn, claim, rec).await
}

async fn report_failure_on(
    conn: &mut PgConnection,
    claim: &Claim,
    rec: &FailureRecord,
) -> sqlx::Result<bool> {
    let (status, next, finished) = match rec.pending_until {
        Some(until) => ("pending", Some(until), false),
        None => ("failed", rec.retry_at, true),
    };
    let r = sqlx::query!(
        "UPDATE track_stages
         SET status = $4, next_attempt_at = $5, attempts = attempts + $6,
             error_code = $7, error_class = $8, last_error = $9,
             finished_at = CASE WHEN $10 THEN now() ELSE NULL END, updated_at = now()
         WHERE track_id = $1 AND stage = $2 AND status = 'working' AND started_at = $3",
        claim.track_id,
        claim.stage.as_str(),
        claim.started_at,
        status,
        next,
        i16::from(rec.count_attempt),
        rec.code,
        rec.class,
        cut_chars(&rec.message, LAST_ERROR_MAX_CHARS),
        finished,
    )
    .execute(conn)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// `s` cut to at most `max` characters, on a character boundary.
pub fn cut_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// A changed file (the scanner's "changed" class): every stage row back to `pending` with a
/// clean slate and `evidence = {"reason":"file changed"}`, and the stored AcoustID
/// fingerprint deleted, since the audio may have changed (upstream
/// `reopenStagesForChangedFile`).
pub async fn reopen_for_changed_file(conn: &mut PgConnection, track: Uuid) -> sqlx::Result<()> {
    // Take the track fence before stage locks, just as ML publication does. Explicitly
    // bump even when the scanner detects changed bytes with identical stored metadata.
    sqlx::query!(
        "UPDATE tracks SET ml_generation = ml_generation + 1 WHERE id = $1",
        track
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"UPDATE track_stages
           SET status = 'pending', attempts = 0, redelivered = 0, next_attempt_at = NULL,
               last_error = NULL, error_code = NULL, error_class = NULL,
               evidence = '{"reason":"file changed"}'::jsonb,
               started_at = NULL, finished_at = NULL, updated_at = now()
           WHERE track_id = $1"#,
        track
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!("DELETE FROM track_acoustid WHERE track_id = $1", track)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The track's file is back (restored or re-pointed): its `FILE_MISSING` rows go back to
/// `pending`. Other rows are untouched.
pub async fn reopen_file_missing(conn: &mut PgConnection, track: Uuid) -> sqlx::Result<()> {
    reopen_file_missing_many(conn, &[track]).await.map(|_| ())
}

/// [`reopen_file_missing`] for several tracks in one statement. Returns rows changed.
pub async fn reopen_file_missing_many(
    conn: &mut PgConnection,
    tracks: &[Uuid],
) -> sqlx::Result<u64> {
    if tracks.is_empty() {
        return Ok(0);
    }
    let r = sqlx::query!(
        "UPDATE track_stages
         SET status = 'pending', attempts = 0, redelivered = 0, next_attempt_at = NULL,
             last_error = NULL, error_code = NULL, error_class = NULL,
             started_at = NULL, finished_at = NULL, updated_at = now()
         WHERE track_id = ANY($1) AND error_class = 'FILE_MISSING'",
        tracks
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// An album's release group changed (MetadataEditor, spec §16.3): its tracks' `recording` rows
/// that are `done` with an answer other than the file's own tag (`file_tag`) go back to
/// `pending` with a clean slate, so the ladder asks again against the new group. Rows still
/// open (`pending`, `working`, `failed`) are left as they are. Returns rows changed.
pub async fn reopen_album_recordings(conn: &mut PgConnection, album: Uuid) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        r#"UPDATE track_stages s
           SET status = 'pending', attempts = 0, redelivered = 0, next_attempt_at = NULL,
               last_error = NULL, error_code = NULL, error_class = NULL,
               evidence = '{"reason":"release group changed"}'::jsonb,
               started_at = NULL, finished_at = NULL, updated_at = now()
           FROM tracks t
           WHERE s.track_id = t.id AND t.album_id = $1 AND s.stage = 'recording'
             AND s.status = 'done' AND s.source IS DISTINCT FROM 'file_tag'"#,
        album
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// An autotag apply set the track's recording MBID (spec §19.6): its `recording` row becomes
/// `done` with `source = 'autotag'`, `evidence = {"reason":"autotag"}`, errors and
/// `next_attempt_at` cleared, `finished_at = now()`; created when the track had none. A claim
/// still out on the row is no longer current, so its report writes nothing.
pub async fn close_recording_autotag(conn: &mut PgConnection, track: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        r#"INSERT INTO track_stages (track_id, stage, status, source, evidence, finished_at,
                                     updated_at)
           VALUES ($1, 'recording', 'done', 'autotag', '{"reason":"autotag"}'::jsonb, now(), now())
           ON CONFLICT (track_id, stage) DO UPDATE SET
             status = 'done', source = 'autotag', evidence = EXCLUDED.evidence,
             next_attempt_at = NULL, last_error = NULL, error_code = NULL, error_class = NULL,
             finished_at = now(), updated_at = now()"#,
        track
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}
