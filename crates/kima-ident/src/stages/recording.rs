//! The recording stage (spec §8.3, §8.5, §10.3): claim tracks whose fingerprint has settled, run
//! the identification ladder, and close each claim.
//!
//! A match is written in one transaction that first locks the track row, then reports `done`,
//! then stores the answer: a report that finds its claim no longer current (lapsed, reopened by
//! a changed file, or the track purged) rolls everything back, so a stale answer writes nothing.
//! The stage never writes album identity, album year or artist MBIDs. Cancellation abandons the
//! batch at once, leaving claims in flight `working` for the lease to expire and releasing the
//! ones not started.

use kima_db::genres::{self, GenreSource};
use kima_db::identity;
use kima_db::stages::{self, Claim, Stage};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

pub use super::ClaimResult;
use super::{BatchReport, REDELIVERY_CAP, StageFailure, lost_worker, record_for, run_claims};
use crate::IdentCtx;
use crate::matching::{self, Answer, LadderResult, no_match_evidence};

/// Claims taken per batch.
pub const BATCH: i64 = 20;
/// `track_stages.source` of a no-match: the last rung that could answer.
const NO_MATCH_SOURCE: &str = "acoustid";

/// Claims up to [`BATCH`] tracks and identifies them, at most `enrichmentConcurrency` at once.
/// Nothing is claimed when `cancel` already fired.
pub async fn run_batch(ctx: &IdentCtx, cancel: &CancellationToken) -> sqlx::Result<BatchReport> {
    run_batch_until(ctx, cancel, &CancellationToken::new()).await
}

/// [`run_batch`]; once `stop` fires no further claim starts (the rest are released to
/// `pending`) and the ones in flight finish. `cancel` also abandons those in flight.
pub async fn run_batch_until(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
) -> sqlx::Result<BatchReport> {
    if cancel.is_cancelled() || stop.is_cancelled() {
        return Ok(BatchReport::default());
    }
    let claims = stages::claim(
        &ctx.pool,
        Stage::Recording,
        BATCH,
        ctx.config.fingerprinting,
    )
    .await?;
    if claims.is_empty() {
        return Ok(BatchReport::default());
    }
    // A failed settings read runs one at a time rather than failing the batch.
    let in_flight = ctx
        .settings
        .get()
        .await
        .map_or(1, |kv| usize::from(kv.enrichment_concurrency))
        .max(1);
    run_claims(
        ctx,
        claims,
        in_flight,
        cancel,
        stop,
        |ctx, claim, cancel| async move { run_claim(&ctx, &claim, &cancel).await },
    )
    .await
}

/// Runs one claim to its report. A claim redelivered [`REDELIVERY_CAP`] times closes as
/// `LOST_WORKER` before anything is asked; a track that is gone is `Stale`.
pub async fn run_claim(
    ctx: &IdentCtx,
    claim: &Claim,
    cancel: &CancellationToken,
) -> sqlx::Result<ClaimResult> {
    let pool = &ctx.pool;
    if claim.redelivered >= REDELIVERY_CAP {
        return closed(stages::report_failure(pool, claim, &lost_worker(claim)).await?);
    }
    // The track is gone; its stage row went with it.
    let Some(track) = identity::ladder_track(pool, claim.track_id).await? else {
        return Ok(ClaimResult::Stale);
    };
    let gate = matching::acoustid_gate(ctx, track.id).await?;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(ClaimResult::Abandoned),
        r = matching::identify(ctx, &track, gate) => r,
    };
    match result {
        LadderResult::Matched(answer) => store(ctx, claim, &track, answer).await,
        LadderResult::NoMatch { asked, skipped } => {
            let evidence = no_match_evidence(&asked, skipped);
            let mut conn = pool.acquire().await?;
            done(stages::report_done(&mut conn, claim, NO_MATCH_SOURCE, &evidence).await?)
        }
        LadderResult::Park { until } => {
            fail(ctx, claim, &StageFailure::RateLimited { until }).await
        }
        LadderResult::Fail(f) => fail(ctx, claim, &f).await,
    }
}

/// Spec §8.5, in one transaction: lock the track, report `done` (a stale claim rolls back),
/// then the answer, the track's MusicBrainz genres, and the album's blanks.
async fn store(
    ctx: &IdentCtx,
    claim: &Claim,
    track: &identity::LadderTrack,
    a: Answer,
) -> sqlx::Result<ClaimResult> {
    let mut tx = ctx.pool.begin().await?;
    let Some(album) = identity::lock_track(&mut tx, claim.track_id).await? else {
        tx.rollback().await?;
        return Ok(ClaimResult::Stale);
    };
    if !stages::report_done(&mut tx, claim, a.rung.as_str(), &a.evidence).await? {
        tx.rollback().await?;
        return Ok(ClaimResult::Stale);
    }
    identity::store_recording_answer(
        &mut tx,
        claim.track_id,
        a.recording.0,
        a.rung.as_str(),
        a.isrc,
    )
    .await?;
    genres::replace_track_genres(
        &mut tx,
        claim.track_id,
        GenreSource::MusicBrainz,
        &a.track_genres,
    )
    .await?;
    // The catalogue answered for the album the ladder read; a track since moved to another
    // album leaves both alone.
    let from_catalogue = a.album_label.is_some() || !a.album_genres.is_empty();
    if from_catalogue && album == track.album_id {
        // Locks the album row first, so two tracks of one album fill it one after the other.
        identity::fill_album_blank(&mut tx, album, a.album_label.as_deref()).await?;
        if !a.album_genres.is_empty() && !identity::album_has_mb_genres(&mut tx, album).await? {
            genres::replace_album_genres(&mut tx, album, GenreSource::MusicBrainz, &a.album_genres)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(ClaimResult::Done)
}

async fn fail(ctx: &IdentCtx, claim: &Claim, f: &StageFailure) -> sqlx::Result<ClaimResult> {
    let rec = record_for(claim, f, OffsetDateTime::now_utc());
    closed(stages::report_failure(&ctx.pool, claim, &rec).await?)
}

fn closed(accepted: bool) -> sqlx::Result<ClaimResult> {
    Ok(if accepted {
        ClaimResult::Failed
    } else {
        ClaimResult::Stale
    })
}

fn done(accepted: bool) -> sqlx::Result<ClaimResult> {
    Ok(if accepted {
        ClaimResult::Done
    } else {
        ClaimResult::Stale
    })
}
