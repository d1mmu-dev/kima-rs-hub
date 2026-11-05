//! The analysis stage (M4): claim analysis-eligible tracks, capture their stable
//! input via the embedding snapshot store, run the pure DSP and MusiCNN compute
//! functions, publish each group independently, and close the claim as done when
//! every group has reached some conclusion (published, or never attempted).
//! A rejected storage claim (stale `file_generation`) closes the claim as a
//! failure so it is redelivered.
//!
//! The stage is local: it does not depend on the MusicBrainz switch, exactly
//! like [`kima_ident::stages::fingerprint`]. The computation functions live in
//! `kima_ml` and are pure: DSP (`kima_ml::dsp`), MusiCNN
//! (`kima_ml::musicnn::compute`/`kima_ml::musicnn::aggregate`). Publication is
//! done through `kima_ml::analysis_publish`, which fences each group against
//! the worker's exact `file_generation`.

use kima_db::analysis_claim;
use kima_db::stages as db_stages;
use kima_db::stages::{Claim, Stage};
use kima_ml::analysis_publish::{self, AnalysisGroupInputs, AnalysisPublishReport};
use kima_ml::attempt::AnalysisAvailability;
use kima_ml::audio::{self, DecodedAudio};
use kima_ml::dsp;
use kima_ml::musicnn::{self, MusicnnCompute, MusicnnComputeError};
use kima_ml::onnx::{MusiCnnHead, OnnxModel};
use kima_ml::{
    ArtifactError, Capability, CapabilityEvaluationReport, CapabilityStatus, verify_artifact,
};
use kima_net::Failure;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use symphonia::core::formats::probe::Hint;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use super::{
    BatchReport, ClaimResult, REDELIVERY_CAP, StageFailure, lost_worker, record_for, run_claims,
};

pub use analysis_claim::AnalysisClaim;

/// Claims taken per batch.
pub const BATCH: i64 = 20;
/// `track_stages.source` of an analysis `done`.
pub const SOURCE: &str = "analysis";
/// A fixed pipeline identity for every group this attempt writes.
pub const PIPELINE_VERSION: &str = "audio-analysis-v1";

/// Run a batch of analysis claims to completion.
pub async fn run_batch(
    ctx: &super::IdentCtx,
    cancel: &CancellationToken,
) -> sqlx::Result<BatchReport> {
    run_batch_until(ctx, cancel, &CancellationToken::new()).await
}

/// [`run_batch`]; once `stop` fires no further claim starts and the ones in
/// flight finish.
pub async fn run_batch_until(
    ctx: &super::IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
) -> sqlx::Result<BatchReport> {
    if cancel.is_cancelled() || stop.is_cancelled() {
        return Ok(BatchReport::default());
    }
    // Snapshot setup unusable (missing model directory, unverified artifacts,
    // unopenable store): claim nothing; DSP still runs.
    let workers = ctx.config.analysis.workers;
    let stopper = stop.child_token();
    let availability = analysis_availability(ctx);
    let worker_stop = stopper.clone();
    let mut report =
        run_claims(
            ctx,
            db_stages::claim(&ctx.pool, Stage::Analysis, BATCH, true).await?,
            workers,
            cancel,
            &stopper,
            move |ctx, claim, cancel| {
                let worker_stop = worker_stop.clone();
                async move {
                    run_claim_stopping(&ctx, &claim, &cancel, &worker_stop, availability).await
                }
            },
        )
        .await?;
    report.halted = stopper.is_cancelled() && !stop.is_cancelled();
    Ok(report)
}

/// Run one claim to its report.
pub async fn run_claim(
    ctx: &super::IdentCtx,
    claim: &Claim,
    cancel: &CancellationToken,
) -> sqlx::Result<ClaimResult> {
    run_claim_stopping(
        ctx,
        claim,
        cancel,
        &CancellationToken::new(),
        analysis_availability(ctx),
    )
    .await
}

async fn run_claim_stopping(
    ctx: &super::IdentCtx,
    claim: &Claim,
    cancel: &CancellationToken,
    _stopper: &CancellationToken,
    availability: AnalysisAvailability,
) -> sqlx::Result<ClaimResult> {
    if claim.redelivered >= REDELIVERY_CAP {
        let rec = lost_worker(claim);
        let accepted = db_stages::report_failure(&ctx.pool, claim, &rec).await?;
        return Ok(closed(accepted));
    }

    // Atomically capture the stable input for this worker's exact claim, then keep
    // the analysis claim alive across the blocking compute and the finishing
    // future by sharing a single `Arc`.
    let (path_str, claim) = match analysis_claim::analysis_input_for_worker(
        &ctx.pool,
        claim.track_id,
        claim.started_at,
    )
    .await?
    {
        Some((path, claim)) => (path, claim),
        None => return Ok(ClaimResult::Stale),
    };
    let claim = Arc::new(claim);
    let root = ctx.library.root.get();
    let path: PathBuf = path_str.into();
    let ctx_blocking = ctx.clone();
    let permits = Arc::new(tokio::sync::Semaphore::new(
        ctx.config.analysis.workers.max(1),
    ));
    let claim_for_compute = Arc::clone(&claim);
    let claim_for_finish = Arc::clone(&claim);
    let outcome = compute_then_finish(
        cancel,
        permits,
        move || {
            compute_and_publish(
                &ctx_blocking,
                claim_for_compute.as_ref(),
                availability,
                &root,
                path.as_path(),
            )
        },
        |outcome| async move { finish_outcome(ctx, claim_for_finish.as_ref(), outcome).await },
    )
    .await;

    match outcome {
        Ok(Some(result)) => result,
        Ok(None) => Ok(ClaimResult::Abandoned),
        Err(join_err) if join_err.is_panic() => std::panic::resume_unwind(join_err.into_panic()),
        Err(_) => Ok(ClaimResult::Abandoned),
    }
}

/// The blocking compute+publish for one claim. Returns a report when the attempt
/// is resolved; otherwise returns a failure (storage rejection, snapshot or
/// decode error).
fn compute_and_publish(
    ctx: &super::IdentCtx,
    claim: &AnalysisClaim,
    availability: AnalysisAvailability,
    root: &Path,
    path: &Path,
) -> Result<AnalysisPublishReport, StageFailure> {
    // ---- stable input ----------------------------------------------------
    let store = match super::embedding::snapshot_store(ctx) {
        Ok(s) => s,
        Err(e) => {
            return Err(StageFailure::Infrastructure(Failure::new(
                "SNAPSHOT_UNAVAILABLE",
                format!("snapshot open: {e}"),
            )));
        }
    };
    let src = store.create(root, path).map_err(|e| {
        StageFailure::Infrastructure(Failure::new(
            "SNAPSHOT_UNAVAILABLE",
            format!("snapshot create: {e}"),
        ))
    })?;
    let open = || -> Result<Box<dyn symphonia::core::io::MediaSource>, StageFailure> {
        Ok(Box::new(src.open().map_err(|e| {
            StageFailure::Infrastructure(Failure::new(
                "SNAPSHOT_UNAVAILABLE",
                format!("snapshot reopen: {e}"),
            ))
        })?))
    };
    let audio = decode_and_resample_media_source(open()?, &hint_for(path))
        .map_err(audio_error_to_stage_failure)?;

    // ---- capabilities ----------------------------------------------------
    let mut inputs = AnalysisGroupInputs::default();

    // DSP: always available; a failing group is simply omitted (not a failure).
    // The analysis results are owned here for the duration of `compute_and_publish`
    // so the `AnalysisGroupInputs` borrows remain valid for the `block_on` below.
    let mut loudness: Option<dsp::LoudnessAnalysis> = None;
    let mut key: Option<dsp::KeyAnalysis> = None;
    let mut tempo: Option<dsp::TempoAnalysis> = None;
    if availability.dsp {
        if let Ok(l) = dsp::analyze_loudness(&audio.mono_16k, 16_000) {
            loudness = Some(l);
        }
        if let Ok(k) = dsp::analyze_key(&audio.mono_16k, 16_000) {
            key = Some(k);
        }
        if let Ok(t) = dsp::analyze_tempo(&audio.mono_16k, 16_000) {
            tempo = Some(t);
        }
        if let Some(ref l) = loudness {
            inputs.loudness = Some((PIPELINE_VERSION, l));
        }
        if let Some(ref k) = key {
            inputs.key = Some((PIPELINE_VERSION, k));
        }
        if let Some(ref t) = tempo {
            inputs.tempo = Some((PIPELINE_VERSION, t));
        }
    }

    // MusiCNN: only when the verified model directory is configured. The runtime is
    // initialized once per process and cached on `ctx.analysis_runtime` (mirroring
    // `stages::embedding::runtime_now`), never reloaded per track.
    let mut musicnn_agg: Option<musicnn::MusicnnAggregate> = None;
    if availability.musicnn {
        let runtime = match runtime_now(ctx) {
            Ok(r) => r,
            Err(_) => {
                return Err(StageFailure::Transient(Failure {
                    code: "MUSICANN_UNAVAILABLE".to_owned(),
                    message: "failed to load MusiCNN models".to_owned(),
                }));
            }
        };
        if let Ok(comp) = runtime.compute(&audio.mono_16k) {
            if let musicnn::MusicnnAnalysisResult::Aggregate(a) = musicnn::aggregate(&comp) {
                musicnn_agg = Some(a);
            }
        }
        if let Some(ref agg) = musicnn_agg {
            inputs.musicnn = Some((PIPELINE_VERSION, agg));
        }
    }

    // ---- publish ----------------------------------------------------------
    let report = analysis_publish::publish_track_analysis_blocking(
        &tokio::runtime::Handle::current(),
        &ctx.pool,
        claim.track_id,
        claim.file_generation,
        &inputs,
    );

    // ---- evaluate completion (D-157) -------------------------------------
    if kima_ml::attempt::analysis_groups_resolved([
        report.musicnn.as_ref(),
        report.loudness.as_ref(),
        report.key.as_ref(),
        report.tempo.as_ref(),
    ]) {
        Ok(report)
    } else {
        Err(StageFailure::Transient(Failure::new(
            "ANALYSIS_STORAGE_REJECTED",
            "analysis group write rejected: stale or deleted file generation",
        )))
    }
}

/// Decode an already-open MediaSource into a mono 16 kHz tail, mirroring the
/// embedder's snapshot/decode contract.
fn decode_and_resample_media_source(
    source: Box<dyn symphonia::core::io::MediaSource>,
    hint: &Hint,
) -> Result<DecodedAudio, audio::AudioError> {
    audio::decode_and_resample_media_source(source, hint)
}

/// Decode hint derived from the track path.
fn hint_for(path: &Path) -> Hint {
    let mut h = Hint::new();
    if let Some(ext) = path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
        h.with_extension(&ext);
    }
    h
}

/// Populate the analysis availability: `musicnn` is `true` only when the cached
/// runtime (initialized once per process, see [`runtime_now`]) loaded successfully;
/// `dsp` is always `true`.
fn analysis_availability(ctx: &super::IdentCtx) -> AnalysisAvailability {
    AnalysisAvailability {
        musicnn: runtime_now(ctx).is_ok(),
        dsp: true,
    }
}

/// Initialize once, retaining success or failure until the process restarts,
/// exactly like [`super::embedding::runtime_now`]: the pinned MusiCNN models are
/// loaded and verified a single time per process, not once per claimed track.
fn runtime_now(ctx: &super::IdentCtx) -> Result<Arc<AnalysisRuntime>, CapabilityEvaluationReport> {
    let mut slot = ctx
        .analysis_runtime
        .runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(result) = slot.as_ref() {
        return result.clone();
    }
    let result = initialize_musicnn(&ctx.config.analysis);
    *slot = Some(result.clone());
    result
}

/// An unset `KIMA_MUSICNN_MODEL_DIR` with `DISABLE_MUSICNN=false` is a provisioning
/// error, not an operator opt-out: the required core artifact is reported missing.
fn musicnn_unconfigured_status() -> CapabilityEvaluationReport {
    CapabilityEvaluationReport {
        capabilities: [(
            Capability::MusicnnAnalysis,
            CapabilityStatus::Unavailable {
                errors: vec![kima_ml::ArtifactFailure {
                    artifact_id: OnnxModel::MusiCnnCore.artifact_id(),
                    category: kima_ml::FailureCategory::Missing,
                    message: "KIMA_MUSICNN_MODEL_DIR is not configured".into(),
                }],
            },
        )]
        .into(),
    }
}

fn musicnn_disabled_status() -> CapabilityEvaluationReport {
    CapabilityEvaluationReport {
        capabilities: [(
            Capability::MusicnnAnalysis,
            CapabilityStatus::Disabled {
                reason: "DISABLE_MUSICNN is active".into(),
            },
        )]
        .into(),
    }
}

/// Resolve the analysis config into a loaded runtime, or the reason it is
/// unavailable. Called at most once per process by [`runtime_now`].
fn initialize_musicnn(
    config: &crate::AnalysisConfig,
) -> Result<Arc<AnalysisRuntime>, CapabilityEvaluationReport> {
    if config.disabled {
        return Err(musicnn_disabled_status());
    }
    let Some(model_dir) = config.model_dir.as_deref() else {
        return Err(musicnn_unconfigured_status());
    };
    load_musicnn_compute(model_dir).map(|compute| {
        Arc::new(AnalysisRuntime {
            compute: Mutex::new(compute),
        })
    })
}

/// Build an unavailable capability report for a single artifact verification
/// failure.
fn capability_eval_unavailable(cap: Capability, e: ArtifactError) -> CapabilityEvaluationReport {
    CapabilityEvaluationReport {
        capabilities: [(
            cap,
            CapabilityStatus::Unavailable {
                errors: vec![e.into()],
            },
        )]
        .into(),
    }
}

/// Load the MusiCNN compute from the certified model directory. Fails closed
/// when any of the pinned artifacts cannot be verified.
fn load_musicnn_compute(model_dir: &Path) -> Result<MusicnnCompute, CapabilityEvaluationReport> {
    let requirements = OnnxModel::requirements_by_capability();
    let report = kima_ml::evaluate_capabilities_with_loader(
        model_dir,
        &kima_ml::CapabilityConfig::default(),
        &requirements,
        |_, _| -> Result<(), String> { Ok(()) },
    );
    if !report.is_available(Capability::MusicnnAnalysis) {
        return Err(report);
    }
    let core = verify_artifact(model_dir, &OnnxModel::MusiCnnCore.descriptor())
        .map_err(|e| capability_eval_unavailable(Capability::MusicnnAnalysis, e))?;
    let heads: std::collections::BTreeMap<kima_ml::onnx::MusiCnnHead, kima_ml::VerifiedBytes> =
        MusiCnnHead::all()
            .iter()
            .map(|&head| {
                let descriptor = head.descriptor();
                let vb = verify_artifact(model_dir, &descriptor)
                    .map_err(|e| capability_eval_unavailable(Capability::MusicnnAnalysis, e))?;
                Ok((head, vb))
            })
            .collect::<Result<_, CapabilityEvaluationReport>>()?;
    MusicnnCompute::from_verified_artifacts(&core, &heads)
        .map_err(|e| capability_compute_unavailable(Capability::MusicnnAnalysis, e))
}

/// Build an unavailable capability report for a MusiCNN compute construction
/// failure (e.g. `OnnxError` from a corrupted model artifact).
fn capability_compute_unavailable(
    cap: Capability,
    e: MusicnnComputeError,
) -> CapabilityEvaluationReport {
    let message = e.to_string();
    CapabilityEvaluationReport {
        capabilities: [(
            cap,
            CapabilityStatus::Unavailable {
                errors: vec![kima_ml::ArtifactFailure {
                    artifact_id: kima_ml::ArtifactId::from_static("muscnnb"),
                    category: kima_ml::FailureCategory::ConsumerLoadFailed,
                    message,
                }],
            },
        )]
        .into(),
    }
}

/// Convert an audio decode error into the stage's failure type. The caller
/// captures the decode failure before the `?` is applied.
fn audio_error_to_stage_failure(e: audio::AudioError) -> StageFailure {
    match e {
        audio::AudioError::Io(err) => {
            StageFailure::Infrastructure(Failure::new("DECODE_IO", err.to_string()))
        }
        audio::AudioError::UnsupportedFormat(msg) => {
            StageFailure::Unreadable(Failure::new("UNSUPPORTED_FORMAT", msg))
        }
        audio::AudioError::CorruptAudio(msg) => {
            StageFailure::Unreadable(Failure::new("CORRUPT_AUDIO", msg))
        }
        audio::AudioError::NoAudioTrack => {
            StageFailure::Unreadable(Failure::new("NO_AUDIO_TRACK", "no audio track found"))
        }
        audio::AudioError::EmptyAudio => StageFailure::Unreadable(Failure::new(
            "EMPTY_AUDIO",
            "audio stream contains no samples",
        )),
        audio::AudioError::UnsupportedSampleRate(rate) => StageFailure::Unreadable(Failure::new(
            "UNSUPPORTED_SAMPLE_RATE",
            format!("{rate} Hz"),
        )),
        audio::AudioError::UnsupportedChannels(ch) => StageFailure::Unreadable(Failure::new(
            "UNSUPPORTED_CHANNELS",
            format!("{ch} channels"),
        )),
        audio::AudioError::Resample(msg) => {
            StageFailure::Transient(Failure::new("RESAMPLE_ERROR", msg))
        }
        audio::AudioError::BufferOverflow => StageFailure::Transient(Failure::new(
            "BUFFER_OVERFLOW",
            "audio exceeds allocation bounds",
        )),
        audio::AudioError::FrameCountMismatch { expected, actual } => {
            StageFailure::Transient(Failure::new(
                "FRAME_COUNT_MISMATCH",
                format!("{expected} frames expected, {actual} decoded"),
            ))
        }
        audio::AudioError::DecodedIdentityMismatch => StageFailure::Transient(Failure::new(
            "DECODED_IDENTITY_MISMATCH",
            "decoded audio changed between passes",
        )),
    }
}

/// Build the evidence JSON used when a claim is reported done.
fn evidence(loudness: bool, key: bool, tempo: bool, musicnn: bool) -> Value {
    json!({
        "groups": {
            "loudness": loudness,
            "key": key,
            "tempo": tempo,
            "musicnn": musicnn,
        }
    })
}

/// A loaded MusiCNN compute, cached on [`IdentCtx`] alongside [`EmbeddingRuntime`].
/// Wrapped in a [`Mutex`] because [`MusicnnCompute::compute`] takes `&mut self` (the
/// mel frontend reuses scratch buffers) while the underlying ONNX session pools are
/// fixed at size 1 regardless of [`crate::AnalysisConfig::workers`].
pub struct AnalysisRuntime {
    compute: Mutex<MusicnnCompute>,
}

impl AnalysisRuntime {
    fn compute(
        &self,
        tail_mono_16k: &[f32],
    ) -> Result<musicnn::MusicnnComputation, MusicnnComputeError> {
        self.compute
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .compute(tail_mono_16k)
    }
}

/// Finalise a claim's outcome.
async fn finish_outcome(
    ctx: &super::IdentCtx,
    claim: &AnalysisClaim,
    outcome: Result<AnalysisPublishReport, StageFailure>,
) -> sqlx::Result<ClaimResult> {
    match outcome {
        Ok(report) => {
            let evidence = evidence(
                report.loudness.is_some(),
                report.key.is_some(),
                report.tempo.is_some(),
                report.musicnn.is_some(),
            );
            let mut conn = ctx.pool.acquire().await?;
            let done = db_stages::report_done(
                &mut conn,
                &Claim {
                    track_id: claim.track_id,
                    stage: Stage::Analysis,
                    started_at: claim.started_at,
                    attempts: 0,
                    redelivered: 0,
                },
                SOURCE,
                &evidence,
            )
            .await?;
            Ok(if done {
                ClaimResult::Done
            } else {
                ClaimResult::Stale
            })
        }
        Err(failure) => {
            let record = record_for(
                &Claim {
                    track_id: claim.track_id,
                    stage: Stage::Analysis,
                    started_at: claim.started_at,
                    attempts: 0,
                    redelivered: 0,
                },
                &failure,
                OffsetDateTime::now_utc(),
            );
            let accepted = db_stages::report_failure(
                &ctx.pool,
                &Claim {
                    track_id: claim.track_id,
                    stage: Stage::Analysis,
                    started_at: claim.started_at,
                    attempts: 0,
                    redelivered: 0,
                },
                &record,
            )
            .await?;
            Ok(closed(accepted))
        }
    }
}

/// The blocking `compute_then_finish` helper, mirroring the embedder's. The
/// `compute` closure runs on a blocking thread while holding one semaphore
/// permit; `finish` runs after it (the final check point) on the original
/// async worker. Cancellation before, during, or immediately after `compute`
/// abandons the result.
pub(crate) async fn compute_then_finish<T, R, F, Fut>(
    cancel: &CancellationToken,
    permits: Arc<tokio::sync::Semaphore>,
    compute: impl FnOnce() -> T + Send + 'static,
    finish: F,
) -> Result<Option<R>, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce(T) -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(None),
        permit = permits.acquire_owned() => permit.expect("the semaphore is never closed"),
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(None),
        result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            compute()
        }) => result?,
    };
    if cancel.is_cancelled() {
        return Ok(None);
    }
    Ok(Some(finish(result).await))
}

fn closed(accepted: bool) -> ClaimResult {
    if accepted {
        ClaimResult::Failed
    } else {
        ClaimResult::Stale
    }
}

#[cfg(test)]
mod tests {
    use kima_ml::attempt::analysis_groups_resolved;

    #[test]
    fn groups_resolved_accepts_absent_and_published_groups() {
        // No group attempted is vacuously resolved; one rejected group is not.
        assert!(analysis_groups_resolved::<()>([None, None, None, None]));
        let ok = Some(Ok(()));
        let err = Some(Err(()));
        assert!(analysis_groups_resolved([ok.as_ref(), None, None, None]));
        assert!(!analysis_groups_resolved([
            ok.as_ref(),
            ok.as_ref(),
            ok.as_ref(),
            err.as_ref()
        ]));
    }
}
