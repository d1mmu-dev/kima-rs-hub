//! The mood step (M5 Task 11; upstream `executeMoodPhase`'s scoring half): scores for new or changed vectors, the
//! library's mood statistics, and the process-wide mood snapshot. The coordinator phase (Task 12) calls
//! [`run_pass`]; the server runs [`spawn_refresher`] so the snapshot also follows changes made by another process
//! or a re-run between cycles.
//!
//! The step needs the mood heads (capability `vibe_mood_heads`). Without them it does nothing and reports why:
//! similarity does not depend on moods (spec 4.4).

use std::sync::Arc;
use std::time::Duration;

use kima_db::PgPool;
use kima_db::vibe::moods::{MoodBuild, MoodBuilder, MoodSnapshot, MoodSnapshots};
use kima_vibe::mood_heads::MoodHeads;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::vibe_assets;
use crate::IdentCtx;

/// How often the snapshot worker looks for new statistics (upstream `moodSnapshotWorker`).
pub const SNAPSHOT_REFRESH_EVERY: Duration = Duration::from_secs(30);

/// What the mood step keeps for the life of the process.
pub struct MoodRuntime {
    pub builder: MoodBuilder,
    pub snapshots: MoodSnapshots,
    /// Fired when scores or statistics changed (M5b drops its cached mixes on it).
    changed: broadcast::Sender<()>,
}

impl MoodRuntime {
    pub fn new(heads: Arc<MoodHeads>) -> Self {
        Self {
            builder: MoodBuilder::new(heads),
            snapshots: MoodSnapshots::new(),
            changed: broadcast::channel(16).0,
        }
    }

    pub fn heads(&self) -> &Arc<MoodHeads> {
        self.builder.heads()
    }
}

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoodPass {
    /// No mood heads: nothing was read or written.
    Unavailable,
    Ran {
        build: MoodBuild,
        /// The snapshot was rebuilt at the end of the pass.
        snapshot_changed: bool,
    },
}

/// The step's state, or `None` while the mood heads are unavailable. Loads the assets once per process.
pub async fn runtime(ctx: &IdentCtx) -> Option<Arc<MoodRuntime>> {
    let assets = vibe_assets::initialize(ctx).await.ok()?;
    let heads = assets.heads.clone()?;
    let mut slot = ctx
        .embedding_runtime
        .vibe_moods
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Some(
        slot.get_or_insert_with(|| Arc::new(MoodRuntime::new(heads)))
            .clone(),
    )
}

/// One run of the step: [`MoodBuilder::build`], then the snapshot refresh. Errors are the caller's to log; a failing
/// mood pass never stops the enrichment cycle (spec 9.1).
pub async fn run_pass(ctx: &IdentCtx) -> sqlx::Result<MoodPass> {
    match runtime(ctx).await {
        None => Ok(MoodPass::Unavailable),
        Some(rt) => run_with(&ctx.pool, &rt).await,
    }
}

/// [`run_pass`] on a given runtime.
pub async fn run_with(pool: &PgPool, rt: &MoodRuntime) -> sqlx::Result<MoodPass> {
    let build = rt.builder.build(pool).await?;
    if build.scored > 0 || build.state_changed {
        // No receiver is not an error.
        let _ = rt.changed.send(());
    }
    let (_, snapshot_changed) = rt.snapshots.refresh(pool, rt.heads()).await?;
    Ok(MoodPass::Ran {
        build,
        snapshot_changed,
    })
}

/// A receiver of the "moods changed" event: one `()` per pass that wrote scores or statistics. `None` while the mood
/// heads are unavailable.
pub async fn subscribe_changes(ctx: &IdentCtx) -> Option<broadcast::Receiver<()>> {
    runtime(ctx).await.map(|rt| rt.changed.subscribe())
}

/// The last built mood snapshot, without touching the database.
pub fn snapshot(ctx: &IdentCtx) -> Option<Arc<MoodSnapshot>> {
    ctx.embedding_runtime
        .vibe_moods
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|rt| rt.snapshots.peek())
}

/// Refreshes the snapshot every [`SNAPSHOT_REFRESH_EVERY`] until `shutdown`. A failed refresh is logged and tried
/// again at the next tick.
pub fn spawn_refresher(ctx: IdentCtx, shutdown: CancellationToken) -> JoinHandle<()> {
    spawn_refresher_every(ctx, shutdown, SNAPSHOT_REFRESH_EVERY)
}

/// [`spawn_refresher`] with another period (tests).
pub fn spawn_refresher_every(
    ctx: IdentCtx,
    shutdown: CancellationToken,
    every: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = tick.tick() => {}
            }
            let Some(rt) = runtime(&ctx).await else {
                debug!("mood heads unavailable; the snapshot is not refreshed");
                continue;
            };
            if let Err(error) = rt.snapshots.refresh(&ctx.pool, rt.heads()).await {
                warn!(%error, "refreshing the mood snapshot failed");
            }
        }
    })
}
