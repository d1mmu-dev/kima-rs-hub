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
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Fingerprint => "fingerprint",
            Stage::Recording => "recording",
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
/// Concurrent claimers never get the same row: track rows are locked `SKIP LOCKED`, and the
/// upsert re-checks due-ness against the committed row, so a claim that lost a race to a
/// claimer that already committed takes nothing.
pub async fn claim(
    pool: &PgPool,
    stage: Stage,
    n: i64,
    fingerprinting: bool,
) -> sqlx::Result<Vec<Claim>> {
    let rows = sqlx::query!(
        r#"WITH due AS (
             SELECT t.id FROM tracks t
             LEFT JOIN track_stages s ON s.track_id = t.id AND s.stage = $1
             WHERE t.missing_since IS NULL
               AND (s.track_id IS NULL
                    OR (s.status = 'pending'
                        AND (s.next_attempt_at IS NULL OR s.next_attempt_at <= now()))
                    OR (s.status IN ('failed', 'working') AND s.next_attempt_at <= now()))
               AND ($1 <> 'recording' OR NOT $3 OR EXISTS (
                    SELECT 1 FROM track_stages f
                    WHERE f.track_id = t.id AND f.stage = 'fingerprint'
                      AND (f.status = 'done'
                           OR (f.status = 'failed' AND f.next_attempt_at IS NULL))))
             ORDER BY t.mtime DESC, t.id
             LIMIT $2
             FOR UPDATE OF t SKIP LOCKED),
           claimed AS (
             INSERT INTO track_stages (track_id, stage, status, next_attempt_at, started_at, updated_at)
             SELECT id, $1, 'working', now() + interval '15 minutes', now(), now() FROM due
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

/// Close a claim as failed per `rec`. `false` when the claim is no longer current: nothing
/// is written.
pub async fn report_failure(
    pool: &PgPool,
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
    .execute(pool)
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
