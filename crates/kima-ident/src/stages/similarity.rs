//! The similarity step (M5 Task 12; upstream `executeSimilarityPhase`): feel for new sound vectors and the library's
//! matching state when its inputs changed (`kima_db::vibe::state::build_matching`), then the vibe map, laid out or
//! extended with new tracks ([`kima_db::vibe::map::MapService`]). It needs the feel probe (capability `vibe_assets`);
//! without it nothing is read or written and the step reports why.
//!
//! The map service is single-flight and holds the pool, so it is kept for the life of the process, like the mood
//! step's state. The server stops it with [`shutdown`].

use std::sync::Arc;

use kima_db::enrichment::VibeProgress;
use kima_db::vibe::map::{MapError, MapService, MapUpdate};
use kima_db::vibe::moods::mood_progress;
use kima_db::vibe::state::{BuildOutcome, build_matching, matching_progress};
use kima_vibe::feel_probe::FeelProbe;

use super::vibe_assets;
use crate::IdentCtx;

/// What the similarity step keeps for the life of the process.
pub struct SimilarityRuntime {
    pub probe: Arc<FeelProbe>,
    pub map: Arc<MapService>,
}

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimilarityPass {
    /// No feel probe: nothing was read or written.
    Unavailable,
    Ran {
        build: BuildOutcome,
        map: MapUpdate,
    },
}

impl SimilarityPass {
    /// Tracks that got feel or a place in this pass. `feel_updated` and the map counts cover the same tracks in the
    /// common case (a fresh batch gets feel, then a place, in the one pass that computed it): take the larger of
    /// the two rather than their sum, which would count every such track twice.
    pub fn worked(&self) -> usize {
        match self {
            Self::Unavailable => 0,
            Self::Ran { build, map } => {
                (build.feel_updated as usize).max(map.laid_out + map.placed)
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SimilarityError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Map(#[from] Arc<MapError>),
}

/// The step's state, or `None` while the feel probe is unavailable. Loads the assets once per process.
pub async fn runtime(ctx: &IdentCtx) -> Option<Arc<SimilarityRuntime>> {
    let assets = vibe_assets::initialize(ctx).await.ok()?;
    let probe = assets.probe.clone()?;
    let mut slot = ctx
        .embedding_runtime
        .vibe_similarity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Some(
        slot.get_or_insert_with(|| {
            Arc::new(SimilarityRuntime {
                map: MapService::new(ctx.pool.clone(), (*probe).clone()),
                probe,
            })
        })
        .clone(),
    )
}

/// One run of the step. `inputs_settled`: analysis, sound vectors and artist genres have settled, which the first
/// (permanent) layout waits for (spec 7.3). Errors are the caller's to log; a failing pass never stops the cycle.
pub async fn run_pass(
    ctx: &IdentCtx,
    inputs_settled: bool,
) -> Result<SimilarityPass, SimilarityError> {
    let Some(rt) = runtime(ctx).await else {
        return Ok(SimilarityPass::Unavailable);
    };
    let build = build_matching(&ctx.pool, &rt.probe).await?;
    let map = rt.map.update(inputs_settled).await?;
    Ok(SimilarityPass::Ran { build, map })
}

/// Stops a running layout at its next checkpoint (server shutdown). Later passes end with `Cancelled`.
pub fn shutdown(ctx: &IdentCtx) {
    let slot = ctx
        .embedding_runtime
        .vibe_similarity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(rt) = slot.as_ref() {
        rt.map.cancel();
    }
}

/// The M5 steps' progress counts for `GET /api/enrichment/progress`, from the assets loaded so far (nothing is
/// loaded here): a step whose asset is unavailable has no counts and owes nothing.
pub async fn vibe_progress(ctx: &IdentCtx) -> sqlx::Result<VibeProgress> {
    let assets = vibe_assets::cached(ctx);
    let moods = match assets.as_ref().and_then(|a| a.heads.as_ref()) {
        Some(heads) => Some(mood_progress(&ctx.pool, heads).await?),
        None => None,
    };
    let similarity = match assets.as_ref().and_then(|a| a.probe.as_ref()) {
        Some(probe) => {
            let p = matching_progress(&ctx.pool, probe).await?;
            Some((p.total, p.completed))
        }
        None => None,
    };
    Ok(VibeProgress { moods, similarity })
}
