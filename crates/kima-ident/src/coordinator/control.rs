//! Operator controls behind `/api/enrichment` (spec §10.7, upstream
//! `B/services/enrichmentState.ts`, `unifiedEnrichment.ts:1388-1459,2948-2983`,
//! `B/services/enrichmentRestart.ts`). Every control but pause kicks the coordinator, after
//! its own transaction has committed. A kick that fails then is logged and the control still
//! succeeds: its change happened, and the `enrich-kick` schedule is the backstop.
//!
//! The routes announce each control's change as an `enrichment:progress` event (spec §17):
//! [`announce_state`] when the control answers the state, [`announce`] when it does not.

use kima_db::PgPool;
use kima_db::enrichment::{self, AcoustidNow, State, Status};
pub use kima_db::enrichment::{SyncCounts, WipeCounts};
use kima_db::settings_kv::{self, Key};

use tracing::warn;

use crate::IdentCtx;
use crate::events::ProgressEvents;

/// Announces the state a control left (spec §17).
pub fn announce_state(events: &ProgressEvents, st: &State) {
    events.notify(st.current_phase, st.status);
}

/// Reads the state a control left and announces it. A failed read is logged only: the
/// control's change happened, and the next one or the coordinator announces again.
pub async fn announce(pool: &PgPool, events: &ProgressEvents) {
    match enrichment::state(pool).await {
        Ok(st) => announce_state(events, &st),
        Err(error) => warn!(%error, "reading the enrichment state to announce it failed"),
    }
}

/// Kicks after a committed control; a failure is logged, never returned.
async fn kick(pool: &PgPool) {
    if let Err(error) = super::kick(pool).await {
        warn!(%error, "could not kick the enrichment coordinator after a control");
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The control does not apply in the pipeline's state (the routes' 400).
    #[error("{0}")]
    WrongState(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Pause: only from `running` (else `Cannot pause enrichment in <status> state`). Does not
/// kick: the running cycle sees the pause and ends after its in-flight items.
pub async fn pause(pool: &PgPool) -> Result<State, ControlError> {
    enrichment::pause(pool)
        .await?
        .map_err(|s| wrong("pause", s))
}

/// Resume: from `paused` to `running`; idempotent when `running`; else
/// `Cannot resume enrichment in <status> state`. Kicks a cycle.
pub async fn resume(pool: &PgPool) -> Result<State, ControlError> {
    let st = enrichment::resume(pool)
        .await?
        .map_err(|s| wrong("resume", s))?;
    kick(pool).await;
    Ok(st)
}

fn wrong(action: &str, s: Status) -> ControlError {
    ControlError::WrongState(format!(
        "Cannot {action} enrichment in {} state",
        s.as_str()
    ))
}

/// Stop: a no-op when `idle`; else `stopping` and halted until [`sync`]. Kicks, so a pipeline
/// with no cycle running (paused) reaches `idle` too.
pub async fn stop(pool: &PgPool) -> sqlx::Result<State> {
    let st = enrichment::stop(pool).await?;
    if st.status != Status::Idle {
        kick(pool).await;
    }
    Ok(st)
}

/// Sync ("Sync New"): lifts a stop and a pause, kicks, and answers what is owed.
pub async fn sync(pool: &PgPool) -> sqlx::Result<SyncCounts> {
    let counts = enrichment::sync(pool).await?;
    kick(pool).await;
    Ok(counts)
}

/// Which skipped-AcoustID reasons no longer hold now.
async fn acoustid_now(ctx: &IdentCtx) -> AcoustidNow {
    AcoustidNow {
        key: ctx.acoustid_key().await.is_some(),
        fingerprinting: ctx.config.fingerprinting,
        enabled: !ctx.config.acoustid_disabled,
    }
}

/// Retry failed and blocked: reopens recordings that skipped AcoustID for a reason that no
/// longer holds, then every failed row with a retry scheduled, due now; kicks. Nothing `done`
/// or terminal is touched. Returns rows reopened (`queued`).
pub async fn retry_failed_and_blocked(ctx: &IdentCtx) -> sqlx::Result<u64> {
    // The scan goes first; the next cycle after it offers the same work. A queue that cannot
    // be read does not stop the control (as with the kick).
    match enrichment::library_scan_running(&ctx.pool).await {
        Ok(true) => return Ok(0),
        Ok(false) => {}
        Err(error) => warn!(%error, "scan check failed; retrying anyway"),
    }
    let n = enrichment::retry_failed(&ctx.pool, acoustid_now(ctx).await).await?;
    kick(&ctx.pool).await;
    Ok(n)
}

/// Re-decide everything: every stage row reopened with a clean slate, except the recording
/// rows of identified tracks; then the skipped-AcoustID reopen; kicks. Returns rows changed
/// (`reopened`).
pub async fn redecide_everything(ctx: &IdentCtx) -> sqlx::Result<u64> {
    let n = enrichment::redecide(&ctx.pool, acoustid_now(ctx).await).await?;
    kick(&ctx.pool).await;
    Ok(n)
}

/// Wipe and re-run (spec §10.7, one transaction); kicks after the commit.
pub async fn wipe_and_rerun(pool: &PgPool) -> sqlx::Result<WipeCounts> {
    let counts = enrichment::wipe(pool).await?;
    kick(pool).await;
    Ok(counts)
}

/// The six steps of the enrichment (`RerunStep`, `services/enrichmentRestart.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Identification,
    Artists,
    Analysis,
    Vectors,
    Moods,
    Similarity,
}

impl Step {
    pub const ALL: [Step; 6] = [
        Step::Identification,
        Step::Artists,
        Step::Analysis,
        Step::Vectors,
        Step::Moods,
        Step::Similarity,
    ];

    /// The step's id, as the route takes it.
    pub fn id(self) -> &'static str {
        match self {
            Step::Identification => "identification",
            Step::Artists => "artists",
            Step::Analysis => "analysis",
            Step::Vectors => "vectors",
            Step::Moods => "moods",
            Step::Similarity => "similarity",
        }
    }

    /// The step's name in a reply: its id, except `STEP_NAMES` (`routes/enrichment.ts`).
    pub fn name(self) -> &'static str {
        match self {
            Step::Analysis => "audio analysis",
            Step::Vectors => "sound vectors",
            other => other.id(),
        }
    }

    pub fn parse(id: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Whether the step can be re-run here: `identification` and `artists` reset what they
    /// own. `analysis` and `vectors` have stages but no reset yet; the mood and similarity
    /// steps are M5 (plan Task 14, owner question Q3).
    pub fn available(self) -> bool {
        matches!(self, Step::Identification | Step::Artists)
    }

    /// `"identification, artists, …"`, for the refusal text.
    pub fn list() -> String {
        Step::ALL.map(Step::id).join(", ")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RerunError {
    /// The step's subsystem has no reset here yet.
    #[error("The {} step is not available yet", .0.id())]
    Unavailable(Step),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Re-run the enrichment from `step` (upstream `rerunEnrichment`): the step's answers and what
/// depends on them are cleared in one transaction and worked out again; the coordinator is
/// kicked after the commit. `identification` is [`wipe_and_rerun`]; `artists` resets only the
/// artists (`artists` in the counts, `tracks` 0). Answers a person set aside or closed stay.
pub async fn rerun(pool: &PgPool, step: Step) -> Result<WipeCounts, RerunError> {
    match step {
        Step::Identification => Ok(wipe_and_rerun(pool).await?),
        Step::Artists => {
            let artists = enrichment::reset_artists(pool).await?;
            kick(pool).await;
            Ok(WipeCounts {
                artists: i64::try_from(artists).unwrap_or(i64::MAX),
                tracks: 0,
            })
        }
        other => Err(RerunError::Unavailable(other)),
    }
}

/// Stores `enrichmentConcurrency` clamped to 1..=5, makes it the cached setting at once and
/// applies it to every source's limiter. Returns the stored value.
pub async fn set_concurrency(ctx: &IdentCtx, c: i64) -> sqlx::Result<u8> {
    let c = c.clamp(1, 5) as u8;
    settings_kv::set_plain(
        &ctx.pool,
        Key::EnrichmentConcurrency,
        Some(&serde_json::json!(c)),
    )
    .await?;
    ctx.settings
        .replace(settings_kv::load(&ctx.pool, &ctx.cipher).await?);
    ctx.net.set_concurrency(c);
    Ok(c)
}
