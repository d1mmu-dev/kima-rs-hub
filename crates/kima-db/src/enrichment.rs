//! The enrichment coordinator's control state (spec §6.4 `enrichment_state`, §10.6): one row,
//! upstream's Redis key `enrichment:state`. Every transition is one statement (or one
//! transaction holding the row lock), so a crash never leaves half a transition.

use sqlx::PgPool;
use time::OffsetDateTime;

use crate::stages::{LAST_ERROR_MAX_CHARS, cut_chars};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Running,
    Paused,
    Stopping,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Running => "running",
            Status::Paused => "paused",
            Status::Stopping => "stopping",
        }
    }

    fn parse(s: &str) -> Status {
        match s {
            "running" => Status::Running,
            "paused" => Status::Paused,
            "stopping" => Status::Stopping,
            // The column's check allows nothing else.
            _ => Status::Idle,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Artists,
    Tracks,
    Fingerprint,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Artists => "artists",
            Phase::Tracks => "tracks",
            Phase::Fingerprint => "fingerprint",
        }
    }

    fn parse(s: &str) -> Option<Phase> {
        match s {
            "artists" => Some(Phase::Artists),
            "tracks" => Some(Phase::Tracks),
            "fingerprint" => Some(Phase::Fingerprint),
            _ => None,
        }
    }
}

/// The `enrichment_state` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub status: Status,
    /// Set by stop, cleared by sync (§10.7): while set, no cycle runs.
    pub halted: bool,
    pub current_phase: Option<Phase>,
    pub started_at: Option<OffsetDateTime>,
    pub paused_at: Option<OffsetDateTime>,
    pub stopped_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub last_activity: OffsetDateTime,
}

impl State {
    /// Whether a cycle may do work now: `running` and not halted.
    pub fn may_run(&self) -> bool {
        self.status == Status::Running && !self.halted
    }
}

struct Row {
    status: String,
    halted: bool,
    current_phase: Option<String>,
    started_at: Option<OffsetDateTime>,
    paused_at: Option<OffsetDateTime>,
    stopped_at: Option<OffsetDateTime>,
    last_error: Option<String>,
    last_activity: OffsetDateTime,
}

impl From<Row> for State {
    fn from(r: Row) -> State {
        State {
            status: Status::parse(&r.status),
            halted: r.halted,
            current_phase: r.current_phase.as_deref().and_then(Phase::parse),
            started_at: r.started_at,
            paused_at: r.paused_at,
            stopped_at: r.stopped_at,
            last_error: r.last_error,
            last_activity: r.last_activity,
        }
    }
}

/// The state now.
pub async fn state(pool: &PgPool) -> sqlx::Result<State> {
    sqlx::query_as!(
        Row,
        "SELECT status, halted, current_phase, started_at, paused_at, stopped_at, last_error,
                last_activity
         FROM enrichment_state"
    )
    .fetch_one(pool)
    .await
    .map(State::from)
}

/// Step 1-2 of a cycle (§10.6), in one statement: `paused` stays as it is; `stopping`, or any
/// status while `halted`, becomes `idle` (phase cleared); otherwise `running`, with
/// `started_at` and `last_activity` stamped when it was `idle` (a transition into `running` is
/// activity, upstream `progressSignature` includes the status). Returns the state after; the cycle runs only when
/// [`State::may_run`].
pub async fn begin_cycle(pool: &PgPool) -> sqlx::Result<State> {
    sqlx::query_as!(
        Row,
        r#"UPDATE enrichment_state SET
             status = CASE WHEN status = 'paused' THEN 'paused'
                           WHEN status = 'stopping' OR halted THEN 'idle'
                           ELSE 'running' END,
             current_phase = CASE WHEN status = 'stopping' OR (halted AND status <> 'paused')
                                  THEN NULL ELSE current_phase END,
             started_at = CASE WHEN status = 'idle' AND NOT halted THEN now()
                               ELSE started_at END,
             last_activity = CASE WHEN status = 'idle' AND NOT halted THEN now()
                                  ELSE last_activity END
           RETURNING status, halted, current_phase, started_at, paused_at, stopped_at,
                     last_error, last_activity"#
    )
    .fetch_one(pool)
    .await
    .map(State::from)
}

/// Records the phase; `activity` (counts changed) also stamps `last_activity`. A phase change
/// alone does not (upstream `progressSignature`).
pub async fn set_phase(pool: &PgPool, phase: Option<Phase>, activity: bool) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE enrichment_state
         SET current_phase = $1,
             last_activity = CASE WHEN $2 THEN now() ELSE last_activity END",
        phase.map(Phase::as_str),
        activity
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// End of a cycle: `stopping` always becomes `idle`; `running` becomes `idle` when `idle`
/// (nothing was left to do). Going idle clears the phase. `paused` is left alone.
pub async fn end_cycle(pool: &PgPool, idle: bool) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE enrichment_state
         SET status = 'idle', current_phase = NULL
         WHERE status = 'stopping' OR (status = 'running' AND $1)",
        idle
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Records `message` as `last_error`, cut to [`LAST_ERROR_MAX_CHARS`] characters.
pub async fn record_error(pool: &PgPool, message: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE enrichment_state SET last_error = $1",
        cut_chars(message, LAST_ERROR_MAX_CHARS)
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Startup (§10.6 `prepare`): artists a dead process left `enriching` go back to `pending`.
/// Returns how many.
pub async fn reset_enriching_artists(pool: &PgPool) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        "UPDATE artists SET enrichment_status = 'pending' WHERE enrichment_status = 'enriching'"
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// Clears `last_error`: a cycle completed without a batch error (spec §10.6).
pub async fn clear_error(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query!("UPDATE enrichment_state SET last_error = NULL WHERE last_error IS NOT NULL")
        .execute(pool)
        .await?;
    Ok(())
}

// ---- operator transitions (spec §10.7) ----
//
// Each runs in one transaction that first locks the state row, so concurrent controls queue up
// and every decision is made on the status it changes. The UPDATE repeats the status it expects.

async fn lock_status(tx: &mut sqlx::PgConnection) -> sqlx::Result<Status> {
    let s = sqlx::query_scalar!("SELECT status FROM enrichment_state FOR UPDATE")
        .fetch_one(&mut *tx)
        .await?;
    Ok(Status::parse(&s))
}

/// Pause: only from `running`; `status='paused'`, `paused_at`. `Ok(Err(status))` when the
/// pipeline was in another state (nothing changed).
pub async fn pause(pool: &PgPool) -> sqlx::Result<Result<State, Status>> {
    let mut tx = pool.begin().await?;
    let status = lock_status(&mut tx).await?;
    if status != Status::Running {
        tx.rollback().await?;
        return Ok(Err(status));
    }
    let st = sqlx::query_as!(
        Row,
        "UPDATE enrichment_state SET status = 'paused', paused_at = now()
         WHERE status = 'running'
         RETURNING status, halted, current_phase, started_at, paused_at, stopped_at,
                   last_error, last_activity"
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Ok(st.into()))
}

/// Resume: from `paused` to `running`, stamping `last_activity` (a resumed pipeline is not
/// hung); `running` is left as it is. `Ok(Err(status))` from
/// any other state.
pub async fn resume(pool: &PgPool) -> sqlx::Result<Result<State, Status>> {
    let mut tx = pool.begin().await?;
    let status = lock_status(&mut tx).await?;
    let st = match status {
        Status::Paused => sqlx::query_as!(
            Row,
            "UPDATE enrichment_state SET status = 'running', last_activity = now()
             WHERE status = 'paused'
             RETURNING status, halted, current_phase, started_at, paused_at, stopped_at,
                       last_error, last_activity"
        )
        .fetch_one(&mut *tx)
        .await?
        .into(),
        Status::Running => state_on(&mut tx).await?,
        other => {
            tx.rollback().await?;
            return Ok(Err(other));
        }
    };
    tx.commit().await?;
    Ok(Ok(st))
}

/// Stop: `idle` is left as it is; otherwise `status='stopping'`, `halted`, `stopped_at`. While
/// halted no cycle runs until [`sync`] lifts it.
pub async fn stop(pool: &PgPool) -> sqlx::Result<State> {
    let mut tx = pool.begin().await?;
    let st = match lock_status(&mut tx).await? {
        Status::Idle => state_on(&mut tx).await?,
        _ => sqlx::query_as!(
            Row,
            "UPDATE enrichment_state SET status = 'stopping', halted = true, stopped_at = now()
             WHERE status <> 'idle'
             RETURNING status, halted, current_phase, started_at, paused_at, stopped_at,
                       last_error, last_activity"
        )
        .fetch_one(&mut *tx)
        .await?
        .into(),
    };
    tx.commit().await?;
    Ok(st)
}

async fn state_on(conn: &mut sqlx::PgConnection) -> sqlx::Result<State> {
    sqlx::query_as!(
        Row,
        "SELECT status, halted, current_phase, started_at, paused_at, stopped_at, last_error,
                last_activity
         FROM enrichment_state"
    )
    .fetch_one(&mut *conn)
    .await
    .map(State::from)
}

/// What "Sync New" found owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct SyncCounts {
    /// Artists with a present track, Various Artists aside, still `pending`.
    pub artists: i64,
    /// Present tracks whose recording stage is pending (no row included).
    pub tracks: i64,
}

/// Sync ("Sync New", upstream `startEnrichmentNow`): clears `halted` and a pause (`paused` or
/// `stopping` become `idle`, phase and `paused_at` cleared) and answers what is owed.
pub async fn sync(pool: &PgPool) -> sqlx::Result<SyncCounts> {
    sqlx::query!(
        "UPDATE enrichment_state SET
             halted = false,
             current_phase = CASE WHEN status IN ('paused', 'stopping') THEN NULL
                                  ELSE current_phase END,
             status = CASE WHEN status IN ('paused', 'stopping') THEN 'idle' ELSE status END,
             paused_at = NULL"
    )
    .execute(pool)
    .await?;
    let r = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM artists a
              WHERE a.enrichment_status = 'pending' AND a.name_key <> 'various artists'
                AND EXISTS (SELECT 1 FROM albums al JOIN tracks t ON t.album_id = al.id
                            WHERE al.artist_id = a.id AND t.missing_since IS NULL)) AS "artists!",
             (SELECT count(*) FROM tracks t
              LEFT JOIN track_stages s ON s.track_id = t.id AND s.stage = 'recording'
              WHERE t.missing_since IS NULL
                AND (s.track_id IS NULL OR s.status = 'pending')) AS "tracks!""#
    )
    .fetch_one(pool)
    .await?;
    Ok(SyncCounts {
        artists: r.artists,
        tracks: r.tracks,
    })
}

// ---- reopen controls (spec §10.7) ----

/// What the skipped-AcoustID reopen needs to know about now: each `acoustidReason` holds no
/// longer when its flag is true (a stored fingerprint is checked per track).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcoustidNow {
    /// A client key resolves now (`no_key`).
    pub key: bool,
    /// Fingerprinting is available now (`fingerprinting_unavailable`).
    pub fingerprinting: bool,
    /// `ACOUSTID_DISABLED` is off now (`disabled`).
    pub enabled: bool,
}

/// Reopens `recording` rows closed `done` with `evidence.acoustid = "skipped"` whose
/// `acoustidReason` no longer holds: `no_key` with a key now, `no_fingerprint` with a
/// `track_acoustid` row now, `fingerprinting_unavailable` with fingerprinting now,
/// `disabled` with AcoustID enabled now. They go back to `pending`, due now. Returns rows.
pub async fn reopen_skipped_acoustid(
    conn: &mut sqlx::PgConnection,
    now: AcoustidNow,
) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        "UPDATE track_stages s
         SET status = 'pending', next_attempt_at = NULL, started_at = NULL, finished_at = NULL,
             updated_at = now()
         WHERE s.stage = 'recording' AND s.status = 'done'
           AND s.evidence ->> 'acoustid' = 'skipped'
           AND CASE s.evidence ->> 'acoustidReason'
                 WHEN 'no_key' THEN $1
                 WHEN 'no_fingerprint' THEN EXISTS (SELECT 1 FROM track_acoustid a
                                                    WHERE a.track_id = s.track_id)
                 WHEN 'fingerprinting_unavailable' THEN $2
                 WHEN 'disabled' THEN $3
                 ELSE false
               END",
        now.key,
        now.fingerprinting,
        now.enabled
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// Retry failed and blocked: the skipped-AcoustID reopen, then every `failed` row with a retry
/// scheduled (`next_attempt_at` set) goes to `pending`, due now, its attempts kept. Nothing
/// `done` or terminal is touched. Returns rows reopened.
pub async fn retry_failed(pool: &PgPool, now: AcoustidNow) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let skipped = reopen_skipped_acoustid(&mut tx, now).await?;
    let due = sqlx::query!(
        "UPDATE track_stages
         SET status = 'pending', next_attempt_at = NULL, updated_at = now()
         WHERE status = 'failed' AND next_attempt_at IS NOT NULL"
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(skipped + due)
}

/// Re-decide everything: every stage row back to `pending` with a clean slate (attempts,
/// redeliveries, errors, evidence and stamps cleared), except the `recording` rows of tracks
/// that hold a `recording_mbid` (upstream's AcoustID dedupe); then the skipped-AcoustID
/// reopen. Nothing is deleted. Returns rows changed.
pub async fn redecide(pool: &PgPool, now: AcoustidNow) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let reopened = sqlx::query!(
        "UPDATE track_stages s
         SET status = 'pending', attempts = 0, redelivered = 0, next_attempt_at = NULL,
             last_error = NULL, error_code = NULL, error_class = NULL, evidence = NULL,
             started_at = NULL, finished_at = NULL, updated_at = now()
         FROM tracks t
         WHERE t.id = s.track_id
           AND NOT (s.stage = 'recording' AND t.recording_mbid IS NOT NULL)"
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let skipped = reopen_skipped_acoustid(&mut tx, now).await?;
    tx.commit().await?;
    Ok(reopened + skipped)
}

/// What a wipe reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct WipeCounts {
    pub artists: i64,
    pub tracks: i64,
}

/// Wipe and re-run (spec §10.7), in one transaction:
/// - `recording` rows `done` or `working` → `pending` (attempts, redeliveries and stamps
///   cleared, evidence `{"reason":"enrichment wiped"}`); their tracks lose `recording_mbid`,
///   `recording_source`, their `musicbrainz` genres, and an ISRC whose source is `musicbrainz`;
/// - every artist but Various Artists → `bio`, `similar` and `last_enriched_at` NULL,
///   `enrichment_status='pending'`, `enrichment_failures=0`; `musicbrainz` artist genres
///   deleted.
///
/// Nothing else is touched: MBIDs, images, fingerprints (`track_acoustid` and the fingerprint
/// stage), file genres, user overrides, other stages and other recording rows stay.
///
/// The affected tracks are locked first, in id order, before their stage rows: the order the
/// recording stage's answer and the scanner use (track, then its stage rows), so a wipe and an
/// answer queue up rather than deadlock. A claim in flight finds its token gone and its late
/// report is dropped.
pub async fn wipe(pool: &PgPool) -> sqlx::Result<WipeCounts> {
    let mut tx = pool.begin().await?;
    let locked: Vec<uuid::Uuid> = sqlx::query_scalar!(
        "SELECT t.id FROM tracks t
         JOIN track_stages s ON s.track_id = t.id
         WHERE s.stage = 'recording' AND s.status IN ('done', 'working')
         ORDER BY t.id
         FOR NO KEY UPDATE OF t"
    )
    .fetch_all(&mut *tx)
    .await?;
    let reopened: Vec<uuid::Uuid> = sqlx::query_scalar!(
        r#"UPDATE track_stages
           SET status = 'pending', attempts = 0, redelivered = 0, next_attempt_at = NULL,
               evidence = '{"reason":"enrichment wiped"}'::jsonb,
               started_at = NULL, finished_at = NULL, updated_at = now()
           WHERE stage = 'recording' AND status IN ('done', 'working')
             AND track_id = ANY($1)
           RETURNING track_id"#,
        &locked
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM track_genres WHERE source = 'musicbrainz' AND track_id = ANY($1)",
        &reopened
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE tracks
         SET recording_mbid = NULL, recording_source = NULL,
             isrc = CASE WHEN isrc_source = 'musicbrainz' THEN NULL ELSE isrc END,
             isrc_source = CASE WHEN isrc_source = 'musicbrainz' THEN NULL ELSE isrc_source END
         WHERE id = ANY($1)",
        &reopened
    )
    .execute(&mut *tx)
    .await?;
    // Various Artists is never looked up (spec §6.3): it keeps `completed`.
    let artists = sqlx::query!(
        r#"UPDATE artists
           SET bio = NULL, "similar" = NULL, enrichment_status = 'pending',
               enrichment_failures = 0, last_enriched_at = NULL
           WHERE name_key <> 'various artists'"#
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query!(
        "DELETE FROM artist_genres
         WHERE source = 'musicbrainz'
           AND artist_id IN (SELECT id FROM artists WHERE name_key <> 'various artists')"
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(WipeCounts {
        artists: i64::try_from(artists).unwrap_or(i64::MAX),
        tracks: reopened.len() as i64,
    })
}

// ---- progress (spec §10.8) ----

/// `round(part / total × 100)` with JavaScript's `Math.round` (half up); 0 for no total.
fn percent(part: i64, total: i64) -> i64 {
    if total <= 0 {
        return 0;
    }
    (200 * part + total) / (2 * total)
}

/// `GET /api/enrichment/progress`, upstream's shape (`unifiedEnrichment.ts:2827-2941`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// Always `"TrackStage"`: where every number comes from.
    pub source: &'static str,
    pub artists: ArtistProgress,
    pub track_tags: TrackTagProgress,
    pub audio_analysis: AudioAnalysisProgress,
    pub clap_embeddings: ClapProgress,
    pub fingerprints: FingerprintProgress,
    pub similarity: SimilarityProgress,
    pub core_complete: bool,
    pub is_fully_complete: bool,
}

/// Artists with a present track, Various Artists aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct ArtistProgress {
    pub total: i64,
    /// `completed` + `unresolvable`.
    pub completed: i64,
    /// `pending` + `enriching`.
    pub pending: i64,
    /// Still to answer: `pending`, `enriching`, or `failed` and due a retry (upstream).
    pub owed: i64,
    pub failed: i64,
    pub progress: i64,
}

/// The recording stage over present tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackTagProgress {
    pub total: i64,
    pub enriched: i64,
    /// Open and actionable: no row, pending, working or retrying, with the fingerprint settled.
    pub pending: i64,
    /// Open but behind an unsettled fingerprint.
    pub waiting: i64,
    /// Open questions of missing files (and `FILE_MISSING` rows).
    pub waiting_missing: i64,
    pub progress: i64,
}

/// M4's analysis: all zero in M3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioAnalysisProgress {
    pub total: i64,
    pub completed: i64,
    pub pending: i64,
    pub queued: i64,
    pub processing: i64,
    pub failed: i64,
    pub permanently_failed: i64,
    pub waiting_missing: i64,
    pub progress: i64,
    pub is_background: bool,
}

impl Default for AudioAnalysisProgress {
    fn default() -> Self {
        Self {
            total: 0,
            completed: 0,
            pending: 0,
            queued: 0,
            processing: 0,
            failed: 0,
            permanently_failed: 0,
            waiting_missing: 0,
            progress: 0,
            is_background: true,
        }
    }
}

/// M4's embeddings: all zero in M3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClapProgress {
    pub total: i64,
    pub completed: i64,
    pub pending: i64,
    pub processing: i64,
    pub failed: i64,
    pub retrying: i64,
    pub waiting_missing: i64,
    pub progress: i64,
    pub is_background: bool,
}

impl Default for ClapProgress {
    fn default() -> Self {
        Self {
            total: 0,
            completed: 0,
            pending: 0,
            processing: 0,
            failed: 0,
            retrying: 0,
            waiting_missing: 0,
            progress: 0,
            is_background: true,
        }
    }
}

/// The fingerprint stage over present tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintProgress {
    pub total: i64,
    pub completed: i64,
    /// No row or pending.
    pub pending: i64,
    /// Working.
    pub processing: i64,
    /// Retrying + terminal (`FILE_MISSING` aside).
    pub failed: i64,
    pub waiting_missing: i64,
    pub progress: i64,
    pub is_background: bool,
}

/// M5's similarity: all zero in M3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarityProgress {
    pub total: i64,
    pub completed: i64,
    pub pending: i64,
    pub progress: i64,
    pub is_background: bool,
}

impl Default for SimilarityProgress {
    fn default() -> Self {
        Self {
            total: 0,
            completed: 0,
            pending: 0,
            progress: 0,
            is_background: true,
        }
    }
}

/// One stage's counts over present tracks, plus missing files' open questions.
#[derive(Debug, Clone, Copy, Default)]
struct StageCounts {
    total: i64,
    done: i64,
    /// No row or `pending`.
    open: i64,
    working: i64,
    retrying: i64,
    terminal: i64,
    waiting_missing: i64,
    /// Open (no row, pending, retrying) behind an unsettled fingerprint; recording only.
    waiting: i64,
}

async fn stage_counts(pool: &PgPool, stage: &str, gate: bool) -> sqlx::Result<StageCounts> {
    let r = sqlx::query!(
        r#"SELECT
             count(*) FILTER (WHERE t.missing_since IS NULL) AS "total!",
             count(*) FILTER (WHERE t.missing_since IS NULL AND s.status = 'done') AS "done!",
             count(*) FILTER (WHERE t.missing_since IS NULL
                                AND (s.track_id IS NULL OR s.status = 'pending')) AS "open!",
             count(*) FILTER (WHERE t.missing_since IS NULL AND s.status = 'working') AS "working!",
             count(*) FILTER (WHERE t.missing_since IS NULL AND s.status = 'failed'
                                AND s.next_attempt_at IS NOT NULL) AS "retrying!",
             count(*) FILTER (WHERE t.missing_since IS NULL AND s.status = 'failed'
                                AND s.next_attempt_at IS NULL
                                AND s.error_class IS DISTINCT FROM 'FILE_MISSING') AS "terminal!",
             count(*) FILTER (WHERE (s.status = 'failed' AND s.error_class = 'FILE_MISSING')
                                 OR (t.missing_since IS NOT NULL
                                     AND (s.track_id IS NULL OR s.status IN ('pending', 'working')
                                          OR (s.status = 'failed'
                                              AND s.next_attempt_at IS NOT NULL))))
               AS "waiting_missing!",
             count(*) FILTER (WHERE $2 AND t.missing_since IS NULL
                                AND (s.track_id IS NULL OR s.status = 'pending'
                                     OR (s.status = 'failed' AND s.next_attempt_at IS NOT NULL))
                                AND NOT COALESCE(f.status = 'done'
                                                 OR (f.status = 'failed'
                                                     AND f.next_attempt_at IS NULL), false))
               AS "waiting!"
           FROM tracks t
           LEFT JOIN track_stages s ON s.track_id = t.id AND s.stage = $1
           LEFT JOIN track_stages f ON f.track_id = t.id AND f.stage = 'fingerprint'"#,
        stage,
        gate
    )
    .fetch_one(pool)
    .await?;
    Ok(StageCounts {
        total: r.total,
        done: r.done,
        open: r.open,
        working: r.working,
        retrying: r.retrying,
        terminal: r.terminal,
        waiting_missing: r.waiting_missing,
        waiting: r.waiting,
    })
}

async fn artist_progress(pool: &PgPool) -> sqlx::Result<ArtistProgress> {
    let r = sqlx::query!(
        r#"SELECT
             count(*) AS "total!",
             count(*) FILTER (WHERE enrichment_status IN ('completed', 'unresolvable'))
               AS "completed!",
             count(*) FILTER (WHERE enrichment_status IN ('pending', 'enriching')) AS "pending!",
             count(*) FILTER (WHERE enrichment_status IN ('pending', 'enriching', 'failed'))
               AS "owed!",
             count(*) FILTER (WHERE enrichment_status = 'failed') AS "failed!"
           FROM artists a
           WHERE a.name_key <> 'various artists'
             AND EXISTS (SELECT 1 FROM albums al JOIN tracks t ON t.album_id = al.id
                         WHERE al.artist_id = a.id AND t.missing_since IS NULL)"#
    )
    .fetch_one(pool)
    .await?;
    Ok(ArtistProgress {
        total: r.total,
        completed: r.completed,
        pending: r.pending,
        owed: r.owed,
        failed: r.failed,
        progress: percent(r.completed, r.total),
    })
}

/// The progress readout (spec §10.8). `fingerprinting`: whether the fingerprint stage can run
/// (spec §9.4); when it cannot, the recording gate counts as settled (nothing `waiting`) and
/// the fingerprint stage does not hold `isFullyComplete` back.
pub async fn progress(pool: &PgPool, fingerprinting: bool) -> sqlx::Result<Progress> {
    let artists = artist_progress(pool).await?;
    let rec = stage_counts(pool, "recording", fingerprinting).await?;
    let fp = stage_counts(pool, "fingerprint", false).await?;

    let track_tags = TrackTagProgress {
        total: rec.total,
        enriched: rec.done,
        pending: rec.open + rec.retrying - rec.waiting + rec.working,
        waiting: rec.waiting,
        waiting_missing: rec.waiting_missing,
        progress: percent(rec.done, rec.total),
    };
    let fingerprints = FingerprintProgress {
        total: fp.total,
        completed: fp.done,
        pending: fp.open,
        processing: fp.working,
        failed: fp.retrying + fp.terminal,
        waiting_missing: fp.waiting_missing,
        progress: percent(fp.done, fp.total),
        is_background: true,
    };
    let core_complete = artists.owed == 0 && track_tags.pending == 0 && track_tags.waiting == 0;
    let fingerprints_settled =
        !fingerprinting || (fp.open == 0 && fp.working == 0 && fp.retrying == 0);
    Ok(Progress {
        source: "TrackStage",
        artists,
        track_tags,
        audio_analysis: AudioAnalysisProgress::default(),
        clap_embeddings: ClapProgress::default(),
        fingerprints,
        similarity: SimilarityProgress::default(),
        core_complete,
        is_fully_complete: core_complete && fingerprints_settled,
    })
}

/// `{total, completed, failed}` of a status counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct Counter {
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
}

/// The audio counter (M4): all zero in M3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct AudioCounter {
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
    pub processing: i64,
}

/// The counters of `GET /api/enrichment/status`, from the progress numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct StatusCounts {
    pub artists: Counter,
    pub tracks: Counter,
    pub audio: AudioCounter,
}

pub async fn status_counts(pool: &PgPool) -> sqlx::Result<StatusCounts> {
    let artists = artist_progress(pool).await?;
    let rec = stage_counts(pool, "recording", false).await?;
    Ok(StatusCounts {
        artists: Counter {
            total: artists.total,
            completed: artists.completed,
            failed: artists.failed,
        },
        tracks: Counter {
            total: rec.total,
            completed: rec.done,
            failed: rec.retrying + rec.terminal,
        },
        audio: AudioCounter::default(),
    })
}

// ---- failures list (spec §10.9) ----

/// The page size never exceeds this.
pub const FAILURES_MAX_LIMIT: i64 = 500;
/// Upstream's `STAGE_RETRY_CAP`, shown as `maxRetries`.
pub const MAX_RETRIES: i32 = 4;

/// `GET /api/enrichment/failures` query. `entity_type` is matched against a fixed list;
/// anything else lists nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureFilter {
    pub entity_type: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

/// One entry of the failures list, upstream's `EnrichmentFailure` shape.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureRow {
    /// `"<trackId>__<stage>"` for a stage row, the artist id for an artist.
    pub id: String,
    /// `track` (recording), `scan` (fingerprint) or `artist`.
    pub entity_type: String,
    pub entity_id: uuid::Uuid,
    pub entity_name: Option<String>,
    /// Stored error text, at most [`LAST_ERROR_MAX_CHARS`] characters.
    pub error_message: Option<String>,
    pub error_code: Option<String>,
    pub retry_count: i32,
    pub max_retries: i32,
    #[serde(serialize_with = "rfc3339")]
    pub first_failed_at: Option<OffsetDateTime>,
    #[serde(serialize_with = "rfc3339")]
    pub last_failed_at: Option<OffsetDateTime>,
    /// Always `false`: nothing is parked (D-108).
    pub skipped: bool,
    #[serde(serialize_with = "rfc3339")]
    pub skipped_at: Option<OffsetDateTime>,
    pub resolved: bool,
    #[serde(serialize_with = "rfc3339")]
    pub resolved_at: Option<OffsetDateTime>,
    pub metadata: serde_json::Value,
}

/// A timestamp as RFC 3339 text, or `null`.
fn rfc3339<S: serde::Serializer>(t: &Option<OffsetDateTime>, s: S) -> Result<S::Ok, S::Error> {
    match t {
        Some(t) => {
            let text = t
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(serde::ser::Error::custom)?;
            s.serialize_str(&text)
        }
        None => s.serialize_none(),
    }
}

/// Which parts of the list a filter selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Parts {
    track: bool,
    scan: bool,
    artist: bool,
}

impl Parts {
    /// Whitelisted entity types; `audio`, `vibe`, `podcast` and anything unknown select
    /// nothing.
    fn of(entity_type: Option<&str>) -> Parts {
        let (track, scan, artist) = match entity_type {
            None => (true, true, true),
            Some("track") => (true, false, false),
            Some("scan") => (false, true, false),
            Some("artist") => (false, false, true),
            Some(_) => (false, false, false),
        };
        Parts {
            track,
            scan,
            artist,
        }
    }
}

/// The failures list (spec §10.9): failed stage rows (retrying and terminal, `FILE_MISSING`
/// aside) and artists `failed` or `unresolvable` with failures, newest `lastFailedAt` first,
/// one page of it and the whole list's size. `limit` is clamped to 1..=[`FAILURES_MAX_LIMIT`],
/// `offset` to 0 or more.
pub async fn failures(pool: &PgPool, f: &FailureFilter) -> sqlx::Result<(Vec<FailureRow>, i64)> {
    let parts = Parts::of(f.entity_type.as_deref());
    let limit = f.limit.clamp(1, FAILURES_MAX_LIMIT);
    let offset = f.offset.max(0);
    let max_chars = LAST_ERROR_MAX_CHARS as i32;
    let rows = sqlx::query!(
        r#"WITH f AS (
             SELECT s.track_id::text || '__' || s.stage AS id,
                    CASE s.stage WHEN 'recording' THEN 'track' ELSE 'scan' END AS entity_type,
                    s.track_id AS entity_id,
                    t.title AS entity_name,
                    left(s.last_error, $6) AS error_message,
                    s.error_code,
                    s.attempts::int AS retry_count,
                    COALESCE(s.started_at, s.updated_at) AS first_failed_at,
                    s.updated_at AS last_failed_at,
                    jsonb_build_object('stage', s.stage, 'source', s.source,
                                       'evidence', s.evidence, 'errorClass', s.error_class)
                      AS metadata
             FROM track_stages s JOIN tracks t ON t.id = s.track_id
             WHERE s.status = 'failed' AND s.error_class IS DISTINCT FROM 'FILE_MISSING'
               AND ((s.stage = 'recording' AND $1) OR (s.stage = 'fingerprint' AND $2))
             UNION ALL
             SELECT a.id::text, 'artist', a.id, a.name, left(a.last_error, $6),
                    a.last_error_code, a.enrichment_failures::int,
                    a.last_enriched_at, a.last_enriched_at,
                    jsonb_build_object('status', a.enrichment_status)
             FROM artists a
             WHERE $3 AND a.enrichment_status IN ('failed', 'unresolvable')
               AND a.enrichment_failures > 0),
           page AS (
             SELECT * FROM f
             ORDER BY last_failed_at DESC NULLS LAST, id
             LIMIT $4 OFFSET $5)
           SELECT (SELECT count(*) FROM f) AS "total!",
                  p.id, p.entity_type, p.entity_id, p.entity_name, p.error_message,
                  p.error_code, p.retry_count, p.first_failed_at, p.last_failed_at, p.metadata
           FROM (SELECT 1) one
           LEFT JOIN page p ON true
           ORDER BY p.last_failed_at DESC NULLS LAST, p.id"#,
        parts.track,
        parts.scan,
        parts.artist,
        limit,
        offset,
        max_chars,
    )
    .fetch_all(pool)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let list = rows
        .into_iter()
        .filter_map(|r| {
            Some(FailureRow {
                id: r.id?,
                entity_type: r.entity_type?,
                entity_id: r.entity_id?,
                entity_name: r.entity_name,
                error_message: r.error_message,
                error_code: r.error_code,
                retry_count: r.retry_count.unwrap_or(0),
                max_retries: MAX_RETRIES,
                first_failed_at: r.first_failed_at,
                last_failed_at: r.last_failed_at,
                skipped: false,
                skipped_at: None,
                resolved: false,
                resolved_at: None,
                metadata: r.metadata.unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();
    Ok((list, total))
}

/// `GET /api/enrichment/failures/counts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct FailureCounts {
    pub artist: i64,
    pub track: i64,
    pub audio: i64,
    pub vibe: i64,
    pub scan: i64,
    pub podcast: i64,
    pub total: i64,
}

pub async fn failure_counts(pool: &PgPool) -> sqlx::Result<FailureCounts> {
    let r = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM artists
              WHERE enrichment_status IN ('failed', 'unresolvable') AND enrichment_failures > 0)
               AS "artist!",
             (SELECT count(*) FROM track_stages
              WHERE stage = 'recording' AND status = 'failed'
                AND error_class IS DISTINCT FROM 'FILE_MISSING') AS "track!",
             (SELECT count(*) FROM track_stages
              WHERE stage = 'fingerprint' AND status = 'failed'
                AND error_class IS DISTINCT FROM 'FILE_MISSING') AS "scan!""#
    )
    .fetch_one(pool)
    .await?;
    Ok(FailureCounts {
        artist: r.artist,
        track: r.track,
        audio: 0,
        vibe: 0,
        scan: r.scan,
        podcast: 0,
        total: r.artist + r.track + r.scan,
    })
}
