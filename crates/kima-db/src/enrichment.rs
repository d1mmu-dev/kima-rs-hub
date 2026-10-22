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
/// `started_at` stamped when it was `idle`. Returns the state after; the cycle runs only when
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
                               ELSE started_at END
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
