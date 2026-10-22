//! The fingerprint stage (spec §9, §10): claim tracks, decode them with ffmpeg, store the
//! AcoustID fingerprint and close each claim.
//!
//! Decodes run on untrusted files: each is bounded by [`STAGE_TIMEOUT`], at most
//! `fingerprint_workers` run at once, and cancellation abandons the batch at once, leaving
//! claims in flight `working` for the lease to expire and releasing the ones not started.

use std::time::Duration;

use kima_db::identity;
use kima_db::stages::{self, Claim, Stage};
use kima_library::scan::library_path;
use kima_net::Failure;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

pub use super::ClaimResult;
use super::{
    BatchReport, REDELIVERY_CAP, STAGE_TIMEOUT, StageFailure, lost_worker, record_for, run_claims,
};
use crate::IdentCtx;
use crate::chromaprint::{DecodeError, fingerprint_file};

/// Claims taken per batch.
pub const BATCH: i64 = 20;
/// `track_stages.source` of a fingerprint `done`.
pub const SOURCE: &str = "ffmpeg";
/// Slack on top of ffmpeg's own [`STAGE_TIMEOUT`] for the file checks around it; past it
/// the whole attempt is a `TIMEOUT`.
const OUTER_GRACE: Duration = Duration::from_secs(5);

/// Claims up to [`BATCH`] tracks and fingerprints them, at most `fingerprint_workers` at
/// once. Nothing is claimed when fingerprinting is unavailable or `cancel` already fired.
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
    if !ctx.config.fingerprinting || cancel.is_cancelled() || stop.is_cancelled() {
        return Ok(BatchReport::default());
    }
    let claims = stages::claim(&ctx.pool, Stage::Fingerprint, BATCH, true).await?;
    let workers = ctx.config.fingerprint_workers;
    run_claims(
        ctx,
        claims,
        workers,
        cancel,
        stop,
        |ctx, claim, cancel| async move { run_claim(&ctx, &claim, &cancel).await },
    )
    .await
}

/// Runs one claim to its report. A claim redelivered [`REDELIVERY_CAP`] times closes as
/// `LOST_WORKER` before anything else is done.
pub async fn run_claim(
    ctx: &IdentCtx,
    claim: &Claim,
    cancel: &CancellationToken,
) -> sqlx::Result<ClaimResult> {
    let pool = &ctx.pool;
    if claim.redelivered >= REDELIVERY_CAP {
        return closed(stages::report_failure(pool, claim, &lost_worker(claim)).await?);
    }

    if let Some((fp, _)) = identity::fingerprint_of(pool, claim.track_id).await? {
        let evidence = serde_json::json!({
            "reason": "fingerprint already stored",
            "characters": fp.chars().count(),
        });
        let mut conn = pool.acquire().await?;
        return done(stages::report_done(&mut conn, claim, SOURCE, &evidence).await?);
    }

    // The track is gone; its stage row went with it.
    let Some((rel, duration_secs)) =
        identity::track_path_and_duration(pool, claim.track_id).await?
    else {
        return Ok(ClaimResult::Stale);
    };
    if duration_secs <= 0 {
        let f = StageFailure::Permanent(Failure::new("NO_DURATION", "the track has no duration"));
        return fail(ctx, claim, &f).await;
    }
    let Some(path) = library_path(&ctx.library.root.get(), &rel) else {
        let f = StageFailure::FileMissing(format!("path escapes the library root: {rel}"));
        return fail(ctx, claim, &f).await;
    };

    let decoded = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(ClaimResult::Abandoned),
        r = tokio::time::timeout(STAGE_TIMEOUT + OUTER_GRACE, fingerprint_file(&path, STAGE_TIMEOUT)) => r,
    };
    let fp = match decoded {
        Ok(Ok(fp)) => fp,
        Ok(Err(e)) => return fail(ctx, claim, &decode_failure(&rel, e)).await,
        Err(_) => {
            let f = StageFailure::Transient(Failure::new("TIMEOUT", "timed out"));
            return fail(ctx, claim, &f).await;
        }
    };

    let evidence = serde_json::json!({
        "reason": "fingerprint computed",
        "characters": fp.encoded.len(),
    });
    let mut tx = pool.begin().await?;
    if !stages::report_done(&mut tx, claim, SOURCE, &evidence).await? {
        tx.rollback().await?;
        return Ok(ClaimResult::Stale);
    }
    // tracks.duration_secs, as AcoustID is asked with it (spec §9.1 step 5).
    identity::store_fingerprint(&mut tx, claim.track_id, &fp.encoded, duration_secs).await?;
    tx.commit().await?;
    Ok(ClaimResult::Done)
}

/// Only a missing file (`NotFound`) is `FILE_MISSING`; any other io error (permissions, a
/// flaky mount) is transient, like a decode error.
fn decode_failure(rel: &str, e: DecodeError) -> StageFailure {
    match e {
        DecodeError::Missing(io) if io.kind() == std::io::ErrorKind::NotFound => {
            StageFailure::FileMissing(format!("{rel}: {io}"))
        }
        DecodeError::Missing(io) => StageFailure::Transient(Failure::new("IO", io.to_string())),
        DecodeError::Timeout => StageFailure::Transient(Failure::new("TIMEOUT", e.message())),
        DecodeError::NoAudio => StageFailure::Transient(Failure::new("NO_AUDIO", e.message())),
        DecodeError::Ffmpeg { .. } => {
            StageFailure::Transient(Failure::new("DECODE_FAILED", e.message()))
        }
    }
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
