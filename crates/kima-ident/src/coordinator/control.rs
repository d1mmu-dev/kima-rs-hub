//! Operator controls behind `/api/enrichment` (spec §10.7, upstream
//! `B/services/enrichmentState.ts`, `unifiedEnrichment.ts:1388-1459,2948-2983`,
//! `B/services/enrichmentRestart.ts`). Every control but pause kicks the coordinator, after
//! its own transaction has committed.

use kima_db::PgPool;
use kima_db::enrichment::{self, AcoustidNow, State, Status};
pub use kima_db::enrichment::{SyncCounts, WipeCounts};
use kima_db::settings_kv::{self, Key};

use super::kick;
use crate::IdentCtx;

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
    kick(pool).await?;
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
        kick(pool).await?;
    }
    Ok(st)
}

/// Sync ("Sync New"): lifts a stop and a pause, kicks, and answers what is owed.
pub async fn sync(pool: &PgPool) -> sqlx::Result<SyncCounts> {
    let counts = enrichment::sync(pool).await?;
    kick(pool).await?;
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
    let n = enrichment::retry_failed(&ctx.pool, acoustid_now(ctx).await).await?;
    kick(&ctx.pool).await?;
    Ok(n)
}

/// Re-decide everything: every stage row reopened with a clean slate, except the recording
/// rows of identified tracks; then the skipped-AcoustID reopen; kicks. Returns rows changed
/// (`reopened`).
pub async fn redecide_everything(ctx: &IdentCtx) -> sqlx::Result<u64> {
    let n = enrichment::redecide(&ctx.pool, acoustid_now(ctx).await).await?;
    kick(&ctx.pool).await?;
    Ok(n)
}

/// Wipe and re-run (spec §10.7, one transaction); kicks after the commit.
pub async fn wipe_and_rerun(pool: &PgPool) -> sqlx::Result<WipeCounts> {
    let counts = enrichment::wipe(pool).await?;
    kick(pool).await?;
    Ok(counts)
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
