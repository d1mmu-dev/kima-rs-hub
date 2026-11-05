//! The embedding stage (CLAP audio, M4, spec §10.3/§10.4 analog): claim tracks, decode,
//! run CLAP inference and atomically publish the vector and completion via `kima_db::ml`.
//!
//! Eligibility is unconditional (spec decision, 2026-11-04): a track is claimable as soon
//! as it exists, exactly like [`crate::stages::fingerprint`] — never gated on `Recording`
//! or any future analysis stage. The coordinator's `embedding_phase` runs this batch
//! alongside `fingerprint_phase`, independent of the MusicBrainz on/off switch. Retry,
//! lease and redelivery policy are the stage-generic constants in
//! [`crate::stages`] (`REDELIVERY_CAP`, `RETRY_CAP`, `RETRY_DELAYS`, `CLAIM_LEASE`),
//! exactly as already used by [`crate::stages::recording`] — nothing new here.
//!
//! Decode and CLAP inference are CPU-bound and run in `spawn_blocking`; the bounded ONNX
//! session pool ([`EmbeddingRuntime`]) is built once per process and cached on
//! [`crate::IdentCtx`], not once per claim. A missing or hash-mismatched model blocks
//! batch claims instead of consuming stage retries.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use kima_db::ml::{self, Provenance};
use kima_db::stages::{self, Claim, Stage};
use kima_ml::audio::decode_clap_auto;
use kima_ml::clap::{ClapMelFrontend, aggregate_window_embeddings, prepare_window};
use kima_ml::onnx::{ClapAudioSession, OnnxModel};
use kima_ml::{
    Capability, CapabilityConfig, CapabilityEvaluationReport, CapabilityStatus,
    evaluate_capabilities_with_loader,
};
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSource;
use tokio_util::sync::CancellationToken;

pub use super::ClaimResult;
use super::{BatchReport, REDELIVERY_CAP, StageFailure, lost_worker, record_for, run_claims};
use crate::IdentCtx;

/// Claims taken per batch.
pub const BATCH: i64 = 20;
/// `track_stages.source` of an embedding `done`.
pub const SOURCE: &str = "clap";
/// A fixed pipeline identity: bumped whenever the preprocessing/aggregation here changes
/// in a way that invalidates stored vectors, independent of the model's own identity.
pub const PIPELINE_VERSION: &str = "clap-audio-v1";

/// A loaded, bounded CLAP audio session plus the mel frontend and the fixed provenance of
/// this model+pipeline combination. Built once and cached on [`IdentCtx`].
pub struct EmbeddingRuntime {
    session: ClapAudioSession,
    mel: ClapMelFrontend,
    provenance: Provenance,
}

/// Loads (verifies + opens) the CLAP audio session from `model_dir`, reporting the result
/// as a [`CapabilityEvaluationReport`] exactly like process startup would: a missing or
/// hash-mismatched artifact becomes `Unavailable`, never a panic or crash.
fn load(
    model_dir: &Path,
    pool_size: usize,
) -> (CapabilityEvaluationReport, Option<EmbeddingRuntime>) {
    let mut built: Option<ClapAudioSession> = None;
    let mut requirements = BTreeMap::new();
    requirements.insert(
        Capability::ClapAudioEmbedding,
        vec![OnnxModel::ClapAudio.descriptor()],
    );
    let report = evaluate_capabilities_with_loader(
        model_dir,
        &CapabilityConfig::default(),
        &requirements,
        |cap, bytes| {
            if cap == Capability::ClapAudioEmbedding {
                built = Some(ClapAudioSession::from_verified_bytes(
                    &bytes, pool_size, None,
                )?);
            }
            Ok::<(), kima_ml::onnx::OnnxError>(())
        },
    );
    let runtime = built.map(|session| EmbeddingRuntime {
        session,
        mel: ClapMelFrontend::new(),
        provenance: Provenance {
            model_id: OnnxModel::ClapAudio.artifact_id().as_str().to_owned(),
            model_sha256: OnnxModel::ClapAudio.expected_sha256(),
            pipeline_version: PIPELINE_VERSION.to_owned(),
        },
    });
    (report, runtime)
}

/// Initialize once, retaining success or failure until the process restarts.
fn runtime_now(ctx: &IdentCtx) -> Result<Arc<EmbeddingRuntime>, CapabilityEvaluationReport> {
    let mut slot = ctx
        .embedding_runtime
        .runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(result) = slot.as_ref() {
        return result.clone();
    }
    let result = initialize_config(&ctx.config.embedding);
    *slot = Some(result.clone());
    result
}

fn single_status(cap: Capability, status: CapabilityStatus) -> CapabilityEvaluationReport {
    CapabilityEvaluationReport {
        capabilities: BTreeMap::from([(cap, status)]),
    }
}

fn disabled_status(cap: Capability) -> CapabilityEvaluationReport {
    single_status(
        cap,
        CapabilityStatus::Disabled {
            reason: "DISABLE_CLAP is active".into(),
        },
    )
}

/// An unset `KIMA_CLAP_MODEL_DIR` with `DISABLE_CLAP=false` is a provisioning error, not an
/// operator opt-out: every required artifact is reported missing.
fn unconfigured_status(
    cap: Capability,
    ids: Vec<kima_ml::ArtifactId>,
) -> CapabilityEvaluationReport {
    single_status(
        cap,
        CapabilityStatus::Unavailable {
            errors: ids
                .into_iter()
                .map(|artifact_id| kima_ml::ArtifactFailure {
                    artifact_id,
                    category: kima_ml::FailureCategory::Missing,
                    message: "KIMA_CLAP_MODEL_DIR is not configured".into(),
                })
                .collect(),
        },
    )
}

fn initialize_config(
    config: &crate::EmbeddingConfig,
) -> Result<Arc<EmbeddingRuntime>, CapabilityEvaluationReport> {
    if config.disabled {
        return Err(disabled_status(Capability::ClapAudioEmbedding));
    }
    let Some(model_dir) = config.model_dir.as_deref() else {
        return Err(unconfigured_status(
            Capability::ClapAudioEmbedding,
            vec![OnnxModel::ClapAudio.artifact_id()],
        ));
    };
    let (report, runtime) = load(model_dir, config.pool_size.max(1));
    runtime.map(Arc::new).ok_or(report)
}

/// Verified text components retained independently from the audio encoder.
pub struct TextRuntime {
    tokenizer: kima_ml::text_tokenizer::ClapTextTokenizer,
    session: kima_ml::onnx::ClapTextSession,
}

fn initialize_text_config(
    config: &crate::EmbeddingConfig,
) -> Result<Arc<TextRuntime>, CapabilityEvaluationReport> {
    let cap = Capability::ClapTextEmbedding;
    if config.disabled {
        return Err(disabled_status(cap));
    }
    let Some(model_dir) = config.model_dir.as_deref() else {
        return Err(unconfigured_status(
            cap,
            vec![
                kima_ml::text_tokenizer::artifact_id(),
                OnnxModel::ClapText.artifact_id(),
            ],
        ));
    };
    let mut tokenizer = None;
    let mut session = None;
    let requirements = BTreeMap::from([(
        cap,
        vec![
            kima_ml::text_tokenizer::descriptor(),
            OnnxModel::ClapText.descriptor(),
        ],
    )]);
    let report = evaluate_capabilities_with_loader(
        model_dir,
        &CapabilityConfig::default(),
        &requirements,
        |_, bytes| {
            if bytes.id() == kima_ml::text_tokenizer::artifact_id() {
                tokenizer = Some(
                    kima_ml::text_tokenizer::ClapTextTokenizer::from_verified_bytes(&bytes)
                        .map_err(|e| e.to_string())?,
                );
            } else {
                session = Some(
                    kima_ml::onnx::ClapTextSession::from_verified_bytes(
                        &bytes,
                        config.pool_size.max(1),
                        None,
                    )
                    .map_err(|e| e.to_string())?,
                );
            }
            Ok::<(), String>(())
        },
    );
    match (tokenizer, session) {
        (Some(tokenizer), Some(session)) if report.is_available(cap) => {
            Ok(Arc::new(TextRuntime { tokenizer, session }))
        }
        _ => Err(report),
    }
}

fn text_runtime_now(ctx: &IdentCtx) -> Result<Arc<TextRuntime>, CapabilityEvaluationReport> {
    let mut slot = ctx
        .embedding_runtime
        .text_runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(result) = slot.as_ref() {
        return result.clone();
    }
    let result = initialize_text_config(&ctx.config.embedding);
    *slot = Some(result.clone());
    result
}

/// Read the cached capability report without loading a model. Includes whichever of the
/// audio and text capabilities have been initialized; `None` if neither has.
pub fn cached_status(ctx: &IdentCtx) -> Option<CapabilityEvaluationReport> {
    let mut capabilities = BTreeMap::new();
    let audio = ctx
        .embedding_runtime
        .runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match audio.as_ref() {
        Some(Ok(_)) => {
            capabilities.insert(Capability::ClapAudioEmbedding, CapabilityStatus::Available);
        }
        Some(Err(report)) => capabilities.extend(report.capabilities.clone()),
        None => {}
    }
    let text = ctx
        .embedding_runtime
        .text_runtime
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match text.as_ref() {
        Some(Ok(_)) => {
            capabilities.insert(Capability::ClapTextEmbedding, CapabilityStatus::Available);
        }
        Some(Err(report)) => capabilities.extend(report.capabilities.clone()),
        None => {}
    }
    if let Some(reason) = cached_block(&ctx.embedding_runtime, &ctx.config.embedding) {
        apply_audio_block(&mut capabilities, &reason);
    }
    (!capabilities.is_empty()).then_some(CapabilityEvaluationReport { capabilities })
}

/// Initialize audio and text capabilities independently on a blocking worker (never on the
/// async executor), each caching its own success or failure until restart.
pub async fn initialize(
    ctx: &IdentCtx,
) -> Result<CapabilityEvaluationReport, tokio::task::JoinError> {
    let ctx = ctx.clone();
    tokio::task::spawn_blocking(move || {
        let _ = runtime_now(&ctx);
        let _ = text_runtime_now(&ctx);
        cached_status(&ctx).expect("both capabilities initialized")
    })
    .await
}

/// Why a text embedding could not be produced.
#[derive(Debug)]
pub enum TextEmbeddingError {
    /// The text capability is disabled or unavailable.
    Unavailable(CapabilityEvaluationReport),
    Tokenize(String),
    Inference(String),
    /// The model returned a zero or non-finite vector.
    Invalid(String),
    /// The blocking worker panicked or was cancelled.
    Worker(String),
}

/// L2-normalizes a raw text-encoder output; refuses zero-norm and non-finite vectors.
fn finish_text_vector(
    raw: &[f32; kima_ml::clap::CLAP_EMBEDDING_DIM],
) -> Result<[f32; kima_ml::clap::CLAP_EMBEDDING_DIM], TextEmbeddingError> {
    let unit = kima_ml::clap::normalize_embedding(raw)
        .map_err(|e| TextEmbeddingError::Invalid(e.to_string()))?;
    kima_ml::clap::ClapEmbedding::from_normalized(unit)
        .map(kima_ml::clap::ClapEmbedding::into_inner)
        .map_err(|e| TextEmbeddingError::Invalid(e.to_string()))
}

fn embed_text_blocking(
    ctx: &IdentCtx,
    text: &str,
) -> Result<[f32; kima_ml::clap::CLAP_EMBEDDING_DIM], TextEmbeddingError> {
    let runtime = text_runtime_now(ctx).map_err(TextEmbeddingError::Unavailable)?;
    let ids = runtime
        .tokenizer
        .encode(text)
        .map_err(|e| TextEmbeddingError::Tokenize(e.to_string()))?;
    let outputs = runtime
        .session
        .infer(&ids, 1, ids.len())
        .map_err(|e| TextEmbeddingError::Inference(e.to_string()))?;
    let raw = outputs
        .first()
        .ok_or_else(|| TextEmbeddingError::Inference("empty text model output".into()))?;
    finish_text_vector(raw)
}

/// Text -> unit-norm 512-d CLAP embedding. Tokenization and ONNX run on a blocking worker;
/// the (lazily cached) runtime is shared with [`initialize`].
pub async fn embed_text(
    ctx: &IdentCtx,
    text: &str,
) -> Result<[f32; kima_ml::clap::CLAP_EMBEDDING_DIM], TextEmbeddingError> {
    let ctx = ctx.clone();
    let text = text.to_owned();
    tokio::task::spawn_blocking(move || embed_text_blocking(&ctx, &text))
        .await
        .map_err(|e| TextEmbeddingError::Worker(e.to_string()))?
}

/// Claims up to [`BATCH`] tracks and embeds them, at most `embedding.workers` at once.
/// Unconditional (spec decision): nothing here checks `ctx.config.fingerprinting` or
/// MusicBrainz/`ctx.net`.
pub async fn run_batch(ctx: &IdentCtx, cancel: &CancellationToken) -> sqlx::Result<BatchReport> {
    run_batch_until(ctx, cancel, &CancellationToken::new()).await
}

/// [`run_batch`]; once `stop` fires no further claim starts and the ones in flight finish.
pub async fn run_batch_until(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
) -> sqlx::Result<BatchReport> {
    if cancel.is_cancelled() || stop.is_cancelled() {
        return Ok(BatchReport::default());
    }
    let check = ctx.clone();
    if tokio::task::spawn_blocking(move || runtime_now(&check))
        .await
        .map_or(true, |result| result.is_err())
    {
        return Ok(BatchReport::default());
    }
    // Snapshot setup unusable (no directory, failed startup check, library root moved to
    // another mount): claim nothing; the status says why.
    let (check, root) = (ctx.clone(), ctx.library.root.get());
    if tokio::task::spawn_blocking(move || {
        fresh_block(&check.embedding_runtime, &check.config.embedding, &root)
    })
    .await
    .map_or(true, |block| block.is_some())
    {
        return Ok(BatchReport::default());
    }
    // The `recording`-only gate in `claim`'s SQL never applies to `embedding`: due-ness is
    // unconditional, like `fingerprint`. The third argument is inert for this stage.
    let claims = stages::claim(&ctx.pool, Stage::Embedding, BATCH, true).await?;
    if claims.is_empty() {
        return Ok(BatchReport::default());
    }
    let workers = ctx.config.embedding.workers;
    // The first global snapshot failure fires `stopper`: no further claim starts, the ones
    // not started go back to `pending`, and the phase stops (`BatchReport::halted`).
    let stopper = stop.child_token();
    let mut report = run_claims(ctx, claims, workers, cancel, &stopper, {
        let stopper = stopper.clone();
        move |ctx, claim, cancel| {
            let stopper = stopper.clone();
            async move { run_claim_stopping(&ctx, &claim, &cancel, &stopper).await }
        }
    })
    .await?;
    report.halted = stopper.is_cancelled() && !stop.is_cancelled();
    Ok(report)
}

/// Runs one claim to its report. A claim redelivered [`REDELIVERY_CAP`] times closes as
/// `LOST_WORKER`, exactly like every other stage.
pub async fn run_claim(
    ctx: &IdentCtx,
    claim: &Claim,
    cancel: &CancellationToken,
) -> sqlx::Result<ClaimResult> {
    run_claim_stopping(ctx, claim, cancel, &CancellationToken::new()).await
}

/// [`run_claim`]; a global snapshot failure also fires `stopper`.
async fn run_claim_stopping(
    ctx: &IdentCtx,
    claim: &Claim,
    cancel: &CancellationToken,
    stopper: &CancellationToken,
) -> sqlx::Result<ClaimResult> {
    let pool = &ctx.pool;
    if claim.redelivered >= REDELIVERY_CAP {
        return closed(stages::report_failure(pool, claim, &lost_worker(claim)).await?);
    }
    let Some((rel, embed_claim)) =
        ml::embedding_input_for_worker(pool, claim.track_id, claim.started_at).await?
    else {
        return Ok(ClaimResult::Stale);
    };
    let root = ctx.library.root.get();
    let path = std::path::PathBuf::from(rel);
    let ctx_blocking = ctx.clone();
    let permits = ctx
        .embedding_runtime
        .workers
        .get_or_init(|| {
            Arc::new(tokio::sync::Semaphore::new(
                ctx.config.embedding.workers.max(1),
            ))
        })
        .clone();
    let outcome = compute_then_finish(
        cancel,
        permits,
        move || run_embedding_blocking(&ctx_blocking, &root, &path),
        |outcome| async move {
            match outcome {
                EmbedOutcome::Computed(embedding, provenance) => {
                    match kima_ml::publish::publish_clap_embedding(
                        pool,
                        &embed_claim,
                        &provenance,
                        &embedding,
                    )
                    .await
                    {
                        Ok(()) => Ok(ClaimResult::Done),
                        Err(kima_ml::publish::PublishError::Rejected) => Ok(ClaimResult::Stale),
                        Err(kima_ml::publish::PublishError::Storage(e)) => Err(e),
                    }
                }
                EmbedOutcome::Blocked(status) => {
                    let f = StageFailure::Transient(kima_net::Failure::new(
                        "CLAP_UNAVAILABLE",
                        format!("CLAP embedding capability is unavailable: {status:?}"),
                    ));
                    fail(ctx, claim, &f).await
                }
                EmbedOutcome::InferenceError(e) => {
                    let f = StageFailure::Inference(kima_net::Failure::new("INFERENCE_ERROR", e));
                    fail(ctx, claim, &f).await
                }
                EmbedOutcome::Snapshot(f) => {
                    if halts_batch(&f) {
                        stopper.cancel();
                    }
                    fail(ctx, claim, &f).await
                }
                EmbedOutcome::DecodeError(e) => {
                    let f = StageFailure::Transient(kima_net::Failure::new(
                        "DECODE_ERROR",
                        e.to_string(),
                    ));
                    fail(ctx, claim, &f).await
                }
            }
        },
    )
    .await;
    match outcome {
        Ok(Some(result)) => result,
        Ok(None) => Ok(ClaimResult::Abandoned),
        Err(join_err) if join_err.is_panic() => std::panic::resume_unwind(join_err.into_panic()),
        Err(_) => Ok(ClaimResult::Abandoned),
    }
}

/// Cancellation before the final check abandons the result. The final check is
/// the point at which finishing/publication starts: cancellation afterwards does
/// not interrupt it. Dropping the whole caller future is a separate event.
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
        permit = permits.acquire_owned() => permit.expect("embedding semaphore is never closed"),
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(None),
        result = tokio::task::spawn_blocking(move || { let _permit = permit; compute() }) => result?,
    };
    if cancel.is_cancelled() {
        return Ok(None);
    }
    Ok(Some(finish(result).await))
}

/// What the blocking embedding attempt produced, translated out of `kima_ml`'s types so
/// the async half above never has to touch ONNX/decode types directly.
enum EmbedOutcome {
    Computed(Box<kima_ml::clap::ClapEmbedding>, Provenance),
    InferenceError(String),
    Blocked(Vec<(Capability, CapabilityStatus)>),
    DecodeError(kima_ml::audio::AudioError),
    /// Snapshot creation or reopen failed; already classified for `record_for`.
    Snapshot(StageFailure),
}

/// Classifies a snapshot failure for the stage. Constant per-file failures are terminal
/// (`FILE_MISSING`, or `PERMANENT` with a snapshot-specific code); everything environmental
/// (quota, no snapshot directory, wrong mount, no reflink, read-only or full filesystem,
/// `EAGAIN`, ...) is `SNAPSHOT_UNAVAILABLE`, retried later without consuming an attempt.
pub(crate) fn snapshot_failure(error: &kima_ml::input_snapshot::SnapshotError) -> StageFailure {
    use kima_ml::input_snapshot::SnapshotClass as C;
    let detail = format!("creating private FICLONE snapshot: {error}");
    let permanent =
        |code: &str| StageFailure::Permanent(kima_net::Failure::new(code, detail.clone()));
    match error.class() {
        C::Missing => StageFailure::FileMissing(format!("not found: {error}")),
        C::TooLarge => permanent("SNAPSHOT_TOO_LARGE"),
        C::NotRegular => permanent("SNAPSHOT_NOT_REGULAR"),
        C::InvalidPath => permanent("SNAPSHOT_INVALID_PATH"),
        // This file only (permissions, nested mount): bounded retries, other files go on.
        C::Unreadable => {
            StageFailure::Unreadable(kima_net::Failure::new("SNAPSHOT_UNREADABLE", detail))
        }
        C::Infrastructure => {
            StageFailure::Infrastructure(kima_net::Failure::new("SNAPSHOT_UNAVAILABLE", detail))
        }
    }
}

/// A failure of the shared snapshot setup (directory, mount, reflink, quota, disk): every
/// further claim of the batch would fail the same way, so the phase stops instead of
/// closing the whole library as `INFRASTRUCTURE`.
pub(crate) fn halts_batch(failure: &StageFailure) -> bool {
    matches!(failure, StageFailure::Infrastructure(_))
}

/// Latch at worker detection, not in the cancellable async finish/publication path.
/// A detached blocking worker still shares this audio-only, first-cause state.
fn snapshot_outcome(ctx: &IdentCtx, failure: StageFailure) -> EmbedOutcome {
    if halts_batch(&failure) {
        ctx.embedding_runtime
            .audio_infrastructure_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(|| format!(
                "audio snapshot infrastructure failed: {failure:?}; repair the snapshot setup and restart to revalidate (no live reload)"
            ));
    }
    EmbedOutcome::Snapshot(failure)
}

/// Opens the configured private snapshot directory. Every failure here is infrastructure
/// (an unset directory included), never a missing input file.
fn open_store(
    embedding: &crate::EmbeddingConfig,
) -> Result<kima_ml::input_snapshot::SnapshotStore, kima_ml::input_snapshot::SnapshotError> {
    let dir = embedding.snapshot_dir.as_deref().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "KIMA_SNAPSHOT_DIR is not configured",
        )
    })?;
    Ok(kima_ml::input_snapshot::SnapshotStore::open(
        dir,
        embedding.snapshot_limits,
    )?)
}

/// The pinned private snapshot directory; opened once on first success, never guessed.
pub(crate) fn snapshot_store(
    ctx: &IdentCtx,
) -> Result<Arc<kima_ml::input_snapshot::SnapshotStore>, kima_ml::input_snapshot::SnapshotError> {
    snapshot_store_in(&ctx.embedding_runtime, &ctx.config.embedding)
}

fn snapshot_store_in(
    state: &crate::ctx::EmbeddingRuntimeState,
    config: &crate::EmbeddingConfig,
) -> Result<Arc<kima_ml::input_snapshot::SnapshotStore>, kima_ml::input_snapshot::SnapshotError> {
    let mut slot = state
        .snapshots
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(store) = slot.as_ref() {
        return Ok(store.clone());
    }
    let store = Arc::new(open_store(config)?);
    *slot = Some(store.clone());
    Ok(store)
}

/// Why embedding AUDIO cannot run for reasons of the snapshot setup, independent of the
/// operator's `DISABLE_CLAP` and of CLAP text: a failed startup check, or no directory.
fn static_block(config: &crate::EmbeddingConfig) -> Option<String> {
    if let Some(reason) = &config.audio_blocked_reason {
        return Some(reason.clone());
    }
    config.snapshot_dir.is_none().then(|| {
        "KIMA_SNAPSHOT_DIR is not configured; audio embedding is off (CLAP text is unaffected)"
            .to_owned()
    })
}

/// Startup setup block, latched runtime failure, or last root check; no filesystem reads.
pub(crate) fn cached_block(
    state: &crate::ctx::EmbeddingRuntimeState,
    config: &crate::EmbeddingConfig,
) -> Option<String> {
    static_block(config)
        .or_else(|| {
            state
                .audio_infrastructure_failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
        .or_else(|| {
            state
                .root_check
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(|(_, reason)| reason.clone())
        })
}

/// Honor startup/runtime latches first; otherwise re-check the snapshot directory against
/// `root` whenever it differs from the last check (`LibraryRoot::set` at runtime).
pub(crate) fn fresh_block(
    state: &crate::ctx::EmbeddingRuntimeState,
    config: &crate::EmbeddingConfig,
    root: &Path,
) -> Option<String> {
    if let Some(reason) = static_block(config).or_else(|| {
        state
            .audio_infrastructure_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }) {
        return Some(reason);
    }
    let mut slot = state
        .root_check
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((checked, reason)) = slot.as_ref()
        && checked == root
    {
        return reason.clone();
    }
    let reason = snapshot_store_in(state, config)
        .map_err(|e| e.to_string())
        .and_then(|store| store.check_mount(root).map_err(|e| e.to_string()))
        .err()
        .map(|e| {
            format!(
                "library root {} cannot be used with the snapshot directory: {e}",
                root.display()
            )
        });
    if let Some(reason) = &reason {
        tracing::warn!(root = %root.display(), %reason, "audio embedding blocked by the snapshot setup");
    }
    *slot = Some((root.to_path_buf(), reason.clone()));
    reason
}

/// Reports a snapshot-setup block on audio only; an operator opt-out or an existing
/// unavailability keeps its own (more fundamental) status.
fn apply_audio_block(capabilities: &mut BTreeMap<Capability, CapabilityStatus>, reason: &str) {
    let audio = capabilities.get_mut(&Capability::ClapAudioEmbedding);
    if let Some(status @ CapabilityStatus::Available) = audio {
        *status = CapabilityStatus::Unavailable {
            errors: vec![kima_ml::ArtifactFailure {
                artifact_id: OnnxModel::ClapAudio.artifact_id(),
                category: kima_ml::FailureCategory::Io,
                message: reason.to_owned(),
            }],
        };
    }
}

/// Fail-fast startup check of the snapshot setup against the library root: the directory
/// opens (absolute, no `..`, no symlink, private and owned), sits on the same mount as
/// `root` (absolute, no `..`) and its filesystem supports reflinks (ext4/overlayfs do not).
/// On failure `config.audio_blocked_reason` is set: no audio embedding work is claimed and
/// the capability status says why, while CLAP text and `DISABLE_CLAP` are untouched; the
/// server keeps running and the reason is returned for logging. Nothing to check when no
/// directory is configured (that is reported by [`static_block`]).
pub fn check_snapshot_setup(
    config: &mut crate::EmbeddingConfig,
    root: &Path,
) -> std::io::Result<()> {
    let Some(dir) = config.snapshot_dir.clone() else {
        return Ok(());
    };
    let checked = open_store(config)
        .map_err(|e| std::io::Error::new(e.kind(), e.to_string()))
        .and_then(|store| store.check_mount(root))
        .and_then(
            |()| match kima_ml::input_snapshot::reflink_supported_in(&dir) {
                Ok(true) => Ok(()),
                Ok(false) => Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!(
                        "snapshot directory {} is on a filesystem without reflink (FICLONE) \
                         support (use btrfs or XFS with reflink=1)",
                        dir.display()
                    ),
                )),
                Err(e) => Err(std::io::Error::new(
                    e.kind(),
                    format!("cannot probe reflink support in {}: {e}", dir.display()),
                )),
            },
        );
    if let Err(error) = &checked {
        config.audio_blocked_reason = Some(format!(
            "snapshot setup is invalid, audio embedding is blocked: {error}"
        ));
    }
    checked
}

/// Runs decode and inference only on a blocking thread; it cannot publish.
fn run_embedding_blocking(ctx: &IdentCtx, root: &Path, path: &Path) -> EmbedOutcome {
    let runtime = match runtime_now(ctx) {
        Ok(runtime) => runtime,
        Err(report) => {
            let blocked = report
                .capabilities
                .iter()
                .map(|(cap, status)| (*cap, status.clone()))
                .collect();
            return EmbedOutcome::Blocked(blocked);
        }
    };
    let mut hint = Hint::new();
    if let Some(ext) = path_extension(path) {
        hint.with_extension(&ext);
    }
    let snapshot = match snapshot_store(ctx).and_then(|store| store.create(root, path)) {
        Ok(snapshot) => snapshot,
        Err(error) => return snapshot_outcome(ctx, snapshot_failure(&error)),
    };
    let reopen_error = std::cell::RefCell::new(None);
    let open = || -> Result<Box<dyn MediaSource>, kima_ml::audio::AudioError> {
        match snapshot.open() {
            Ok(file) => Ok(Box::new(file)),
            Err(error) => {
                *reopen_error.borrow_mut() = Some(snapshot_outcome(
                    ctx,
                    StageFailure::Infrastructure(kima_net::Failure::new(
                        "SNAPSHOT_UNAVAILABLE",
                        format!("reopening private snapshot: {error}"),
                    )),
                ));
                Err(error.into())
            }
        }
    };
    let decoded = decode_clap_auto(open, &hint, None);
    if let Some(outcome) = reopen_error.into_inner() {
        return outcome;
    }
    let (_, windows, rate) = match decoded {
        Ok(decoded) => decoded,
        Err(error) => return EmbedOutcome::DecodeError(error),
    };
    match compute_embedding(&runtime, &windows, rate) {
        Ok(embedding) => EmbedOutcome::Computed(Box::new(embedding), runtime.provenance.clone()),
        Err(error) => EmbedOutcome::InferenceError(error),
    }
}

/// Native-rate windows -> CLAP inference -> mean+L2 aggregation.
fn compute_embedding(
    runtime: &EmbeddingRuntime,
    windows: &[Vec<f32>],
    rate: u32,
) -> Result<kima_ml::clap::ClapEmbedding, String> {
    let mut window_embeddings = Vec::with_capacity(windows.len());
    for window in windows {
        let prepared = prepare_window(window, rate).map_err(|e| e.to_string())?;
        let frames = runtime.mel.compute(&prepared).map_err(|e| e.to_string())?;
        let mut flat = Vec::with_capacity(frames.len() * 64);
        for frame in &frames {
            flat.extend_from_slice(frame);
        }
        let outputs = runtime
            .session
            .infer(&flat, 1, frames.len())
            .map_err(|e| e.to_string())?;
        let mut vector = [0f32; kima_ml::clap::CLAP_EMBEDDING_DIM];
        vector.copy_from_slice(&outputs[0]);
        window_embeddings.push(vector);
    }
    aggregate_window_embeddings(&window_embeddings).map_err(|e| e.to_string())
}

pub(crate) fn path_extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().into_owned())
}

pub(crate) async fn fail(
    ctx: &IdentCtx,
    claim: &Claim,
    f: &StageFailure,
) -> sqlx::Result<ClaimResult> {
    let rec = record_for(claim, f, time::OffsetDateTime::now_utc());
    closed(stages::report_failure(&ctx.pool, claim, &rec).await?)
}

pub(crate) fn closed(accepted: bool) -> sqlx::Result<ClaimResult> {
    Ok(if accepted {
        ClaimResult::Failed
    } else {
        ClaimResult::Stale
    })
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_keeps_snapshot_until_blocking_worker_exits() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, mpsc};
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let snap = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        std::fs::set_permissions(snap.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        match kima_ml::input_snapshot::reflink_supported_in(snap.path()) {
            Ok(true) => {}
            other => {
                let msg = format!(
                    "NO REFLINK SUPPORT in {} ({other:?})",
                    snap.path().display()
                );
                if std::env::var("KIMA_TEST_ALLOW_NO_REFLINK").as_deref() == Ok("1") {
                    eprintln!("SKIPPED: {msg}");
                    return;
                }
                panic!("{msg}; run on btrfs/XFS-reflink or set KIMA_TEST_ALLOW_NO_REFLINK=1");
            }
        }
        std::fs::write(dir.path().join("audio"), b"audio").unwrap();
        let store = Arc::new(
            kima_ml::input_snapshot::SnapshotStore::open(snap.path(), Default::default()).unwrap(),
        );
        let root = dir.path().to_path_buf();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permits = slots.clone();
        let task = tokio::spawn(async move {
            super::compute_then_finish(
                &token,
                permits,
                move || {
                    let snapshot = store.create(&root, "audio".as_ref()).unwrap();
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    drop(snapshot);
                    finished_tx.send(()).unwrap();
                },
                |()| async {},
            )
            .await
            .unwrap()
        });
        started_rx.await.unwrap();
        cancel.cancel();
        assert_eq!(task.await.unwrap(), None);
        assert_eq!(slots.available_permits(), 0);
        release_tx.send(()).unwrap();
        finished_rx.await.unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_during_compute_never_starts_finish() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let published = Arc::new(AtomicBool::new(false));
        let mark = published.clone();
        let token = cancel.clone();
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let slots = permits.clone();
        let task = tokio::spawn(async move {
            super::compute_then_finish(
                &token,
                slots,
                move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    completed_tx.send(()).unwrap();
                },
                move |()| async move { mark.store(true, Ordering::SeqCst) },
            )
            .await
            .unwrap()
        });
        started_rx.await.unwrap();
        cancel.cancel();
        assert_eq!(task.await.unwrap(), None);
        assert_eq!(
            permits.available_permits(),
            0,
            "blocking computation still owns its slot"
        );
        release_tx.send(()).unwrap();
        completed_rx.await.unwrap();
        assert!(!published.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_previous_batch_keeps_shared_slot_until_blocking_exit() {
        use std::sync::{Arc, mpsc};
        use std::task::Poll;
        let state = Arc::new(crate::ctx::EmbeddingRuntimeState::default());
        let slots = state
            .workers
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        let first = tokio::spawn(async move {
            super::compute_then_finish(
                &token,
                slots,
                move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
                |()| async {},
            )
            .await
            .unwrap()
        });
        started_rx.await.unwrap();
        cancel.cancel();
        assert_eq!(first.await.unwrap(), None);
        let next_slots = state
            .workers
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone();
        let second_cancel = tokio_util::sync::CancellationToken::new();
        let second =
            super::compute_then_finish(&second_cancel, next_slots, || 42, |n| async move { n });
        tokio::pin!(second);
        assert!(matches!(futures::poll!(&mut second), Poll::Pending));
        release_tx.send(()).unwrap();
        assert_eq!(second.await.unwrap(), Some(42));
    }

    #[tokio::test]
    async fn cancellation_after_finish_starts_does_not_interrupt_it() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            super::compute_then_finish(
                &token,
                std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                || 7,
                move |value| async move {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    value
                },
            )
            .await
            .unwrap()
        });
        started_rx.await.unwrap();
        cancel.cancel();
        release_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Some(7));
    }

    fn claim_with_attempts(attempts: i16) -> kima_db::stages::Claim {
        kima_db::stages::Claim {
            track_id: uuid::Uuid::nil(),
            stage: kima_db::stages::Stage::Embedding,
            started_at: time::OffsetDateTime::now_utc(),
            attempts,
            redelivered: 0,
        }
    }

    /// Per-file snapshot failures are terminal with their own category; missing is
    /// `FILE_MISSING`; everything environmental stays `SNAPSHOT_UNAVAILABLE`/INCONCLUSIVE and
    /// never burns an attempt.
    #[test]
    fn snapshot_errors_split_constant_file_failures_from_infrastructure() {
        use kima_ml::input_snapshot::SnapshotClass as C;
        let now = time::OffsetDateTime::now_utc();
        let claim = claim_with_attempts(3);
        let cases = [
            (C::Missing, "FILE_MISSING", "FILE_MISSING", true, false),
            (C::TooLarge, "SNAPSHOT_TOO_LARGE", "PERMANENT", true, false),
            (
                C::NotRegular,
                "SNAPSHOT_NOT_REGULAR",
                "PERMANENT",
                true,
                false,
            ),
            (
                C::InvalidPath,
                "SNAPSHOT_INVALID_PATH",
                "PERMANENT",
                true,
                false,
            ),
            (
                C::Infrastructure,
                "SNAPSHOT_UNAVAILABLE",
                "INCONCLUSIVE",
                false,
                true,
            ),
        ];
        for (class, code, error_class, counts, retried) in cases {
            let e =
                kima_ml::input_snapshot::SnapshotError::new(class, std::io::Error::other("boom"));
            let failure = super::snapshot_failure(&e);
            let rec = super::record_for(&claim, &failure, now);
            assert_eq!(rec.code, code, "{class:?}");
            assert_eq!(rec.class.as_deref(), Some(error_class), "{class:?}");
            assert_eq!(rec.count_attempt, counts, "{class:?}");
            assert_eq!(rec.retry_at.is_some(), retried, "{class:?}");
        }
    }

    #[test]
    fn unset_snapshot_dir_is_infrastructure_not_a_missing_file() {
        let ctx_config = crate::EmbeddingConfig::default();
        let e = super::open_store(&ctx_config).err().unwrap();
        assert_eq!(
            e.class(),
            kima_ml::input_snapshot::SnapshotClass::Infrastructure
        );
        let failure = super::snapshot_failure(&e);
        assert!(matches!(failure, super::StageFailure::Infrastructure(_)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_check_blocks_audio_for_a_bad_snapshot_dir_but_not_a_good_one() {
        use std::os::unix::fs::PermissionsExt;
        let lib = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let snap = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mode = |dir: &std::path::Path, m| {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(m)).unwrap()
        };
        mode(snap.path(), 0o700);
        let with_dir = |dir: std::path::PathBuf| crate::EmbeddingConfig {
            snapshot_dir: Some(dir),
            ..Default::default()
        };
        // Good: unchanged.
        let mut good = with_dir(snap.path().to_path_buf());
        assert!(super::check_snapshot_setup(&mut good, lib.path()).is_ok());
        assert!(!good.disabled);
        assert!(good.snapshot_dir.is_some());
        // Bad permissions, missing dir, `..` in the dir, `..` in the root: audio blocked + error.
        mode(snap.path(), 0o755);
        let dotdot = snap
            .path()
            .join("..")
            .join(snap.path().file_name().unwrap());
        let root_dotdot = lib.path().join("..").join(lib.path().file_name().unwrap());
        for (dir, root) in [
            (snap.path().to_path_buf(), lib.path().to_path_buf()),
            (snap.path().join("nope"), lib.path().to_path_buf()),
            (dotdot, lib.path().to_path_buf()),
            (snap.path().to_path_buf(), root_dotdot),
        ] {
            mode(snap.path(), 0o755);
            let mut cfg = with_dir(dir.clone());
            let root_is_dotdot = root
                .components()
                .any(|c| c == std::path::Component::ParentDir);
            if root_is_dotdot {
                mode(snap.path(), 0o700);
            }
            assert!(
                super::check_snapshot_setup(&mut cfg, &root).is_err(),
                "{dir:?} {root:?}"
            );
            // CLAP text and the operator switch are untouched; only audio is blocked.
            assert!(!cfg.disabled, "{dir:?} {root:?}");
            assert!(cfg.audio_blocked_reason.is_some(), "{dir:?} {root:?}");
            assert!(super::static_block(&cfg).is_some());
        }
        // Not configured at all: nothing to check, nothing disabled.
        let mut unset = crate::EmbeddingConfig::default();
        assert!(super::check_snapshot_setup(&mut unset, lib.path()).is_ok());
        assert!(!unset.disabled);
    }

    #[test]
    fn unreadable_snapshot_is_per_file_with_a_bounded_budget() {
        use kima_ml::input_snapshot::{SnapshotClass, SnapshotError};
        let e = SnapshotError::new(
            SnapshotClass::Unreadable,
            std::io::Error::from_raw_os_error(13),
        );
        let failure = super::snapshot_failure(&e);
        assert!(
            !super::halts_batch(&failure),
            "a per-file failure must not halt the phase"
        );
        let now = time::OffsetDateTime::now_utc();
        let first = super::record_for(&claim_with_attempts(0), &failure, now);
        assert_eq!(first.code, "SNAPSHOT_UNREADABLE");
        assert_eq!(first.class.as_deref(), Some("INCONCLUSIVE"));
        assert!(first.count_attempt, "must consume the attempt budget");
        assert!(first.retry_at.is_some());
        let last = super::record_for(&claim_with_attempts(3), &failure, now);
        assert!(last.count_attempt);
        assert!(last.retry_at.is_none(), "terminal once the budget is spent");
        assert_eq!(last.code, "SNAPSHOT_UNREADABLE");
    }

    #[test]
    fn only_global_snapshot_failures_halt_the_batch() {
        let infra = super::StageFailure::Infrastructure(kima_net::Failure::new(
            "SNAPSHOT_UNAVAILABLE",
            "no space",
        ));
        assert!(super::halts_batch(&infra));
        for f in [
            super::StageFailure::FileMissing("gone".into()),
            super::StageFailure::Permanent(kima_net::Failure::new("SNAPSHOT_TOO_LARGE", "big")),
            super::StageFailure::Transient(kima_net::Failure::new("DECODE_ERROR", "bad")),
            super::StageFailure::Inference(kima_net::Failure::new("INFERENCE_ERROR", "bad")),
        ] {
            assert!(!super::halts_batch(&f), "{f:?}");
        }
    }

    #[test]
    fn unset_snapshot_dir_blocks_audio_with_its_own_reason() {
        let cfg = crate::EmbeddingConfig::default();
        let reason = super::static_block(&cfg).expect("blocked without KIMA_SNAPSHOT_DIR");
        assert!(reason.contains("KIMA_SNAPSHOT_DIR"), "{reason}");
        assert!(!reason.contains("DISABLE_CLAP"), "{reason}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_block_leaves_text_and_the_operator_switch_alone() {
        use std::os::unix::fs::PermissionsExt;
        let lib = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let snap = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        std::fs::set_permissions(snap.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = crate::EmbeddingConfig {
            snapshot_dir: Some(snap.path().to_path_buf()),
            ..Default::default()
        };
        assert!(super::check_snapshot_setup(&mut cfg, lib.path()).is_err());
        assert!(!cfg.disabled);
        // Text never looks at the snapshot setup: still "not configured", not "disabled".
        let text = super::initialize_text_config(&cfg).err().unwrap();
        assert!(matches!(
            text.status(kima_ml::Capability::ClapTextEmbedding),
            kima_ml::CapabilityStatus::Unavailable { .. }
        ));
        // Audio is reported with the real reason, never "DISABLE_CLAP is active".
        let reason = super::static_block(&cfg).unwrap();
        assert!(!reason.contains("DISABLE_CLAP"), "{reason}");
        let mut caps = std::collections::BTreeMap::from([(
            kima_ml::Capability::ClapAudioEmbedding,
            kima_ml::CapabilityStatus::Available,
        )]);
        super::apply_audio_block(&mut caps, &reason);
        let status = serde_json::to_value(&caps).unwrap();
        let audio = &status["clap_audio_embedding"];
        assert_eq!(audio["state"], "unavailable", "{status}");
        assert_eq!(audio["errors"][0]["message"], reason.as_str());
        // An operator opt-out keeps its own wording.
        let mut off = std::collections::BTreeMap::from([(
            kima_ml::Capability::ClapAudioEmbedding,
            kima_ml::CapabilityStatus::Disabled {
                reason: "DISABLE_CLAP is active".into(),
            },
        )]);
        super::apply_audio_block(&mut off, &reason);
        assert!(matches!(
            off[&kima_ml::Capability::ClapAudioEmbedding],
            kima_ml::CapabilityStatus::Disabled { .. }
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_check_refuses_a_filesystem_without_reflink() {
        use std::os::unix::fs::PermissionsExt;
        let shm = std::path::Path::new("/dev/shm");
        if !shm.is_dir() {
            return;
        }
        let dir = tempfile::tempdir_in(shm).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        // tmpfs has no FICLONE.
        let mut cfg = crate::EmbeddingConfig {
            snapshot_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let error = super::check_snapshot_setup(&mut cfg, dir.path()).unwrap_err();
        assert!(error.to_string().contains("reflink"), "{error}");
        assert!(
            cfg.audio_blocked_reason
                .as_deref()
                .unwrap()
                .contains("reflink")
        );
        assert!(!cfg.disabled);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn changed_library_root_is_rechecked_against_the_snapshot_mount() {
        use std::os::unix::fs::PermissionsExt;
        let shm = std::path::Path::new("/dev/shm");
        if !shm.is_dir() {
            return;
        }
        let snap = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        std::fs::set_permissions(snap.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let same = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let other = tempfile::tempdir_in(shm).unwrap();
        let cfg = crate::EmbeddingConfig {
            snapshot_dir: Some(snap.path().to_path_buf()),
            ..Default::default()
        };
        let state = crate::ctx::EmbeddingRuntimeState::default();
        assert_eq!(super::fresh_block(&state, &cfg, same.path()), None);
        let reason = super::fresh_block(&state, &cfg, other.path()).expect("other mount");
        assert!(reason.contains("another mount"), "{reason}");
        // The status path sees the stored reason without touching the filesystem.
        assert_eq!(super::cached_block(&state, &cfg), Some(reason));
        // Switching back clears it.
        assert_eq!(super::fresh_block(&state, &cfg, same.path()), None);
        assert_eq!(super::cached_block(&state, &cfg), None);
    }

    #[test]
    fn snapshot_infrastructure_never_burns_attempts_even_at_retry_cap() {
        let claim = kima_db::stages::Claim {
            track_id: uuid::Uuid::nil(),
            stage: kima_db::stages::Stage::Embedding,
            started_at: time::OffsetDateTime::now_utc(),
            attempts: 3,
            redelivered: 0,
        };
        let now = time::OffsetDateTime::now_utc();
        let failure = super::StageFailure::Infrastructure(kima_net::Failure::new(
            "SNAPSHOT_UNAVAILABLE",
            "read-only filesystem",
        ));
        let rec = super::record_for(&claim, &failure, now);
        assert!(!rec.count_attempt);
        assert!(rec.retry_at.is_some());
        assert_eq!(rec.code, "SNAPSHOT_UNAVAILABLE");
        assert_eq!(rec.class.as_deref(), Some("INCONCLUSIVE"));
    }

    #[test]
    fn inference_failure_is_retryable_even_if_message_looks_like_decode_error() {
        use super::super::{StageFailure, record_for};
        let claim = kima_db::stages::Claim {
            track_id: uuid::Uuid::nil(),
            stage: kima_db::stages::Stage::Embedding,
            started_at: time::OffsetDateTime::now_utc(),
            attempts: 0,
            redelivered: 0,
        };
        let failure = StageFailure::Inference(kima_net::Failure::new(
            "INFERENCE_ERROR",
            "unsupported codec from model",
        ));
        let rec = record_for(&claim, &failure, time::OffsetDateTime::now_utc());
        assert_eq!(rec.class.as_deref(), Some("INCONCLUSIVE"));
        assert!(rec.retry_at.is_some());
    }

    #[tokio::test]
    async fn missing_model_failure_is_cached_until_restart() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let pool =
            kima_db::PgPool::connect_lazy("postgres://unused:unused@127.0.0.1/unused").unwrap();
        let cipher = Arc::new(kima_core::cipher::Cipher::from_secret("test"));
        let events = kima_core::events::EventBus::new(16);
        let ctx = crate::IdentCtx {
            pool: pool.clone(),
            net: Arc::new(kima_net::Net::new(
                kima_net::NetConfig::for_tests(kima_net::Bases::production("http://127.0.0.1:9")),
                pool.clone(),
                kima_core::identify::IdentifySwitch::new(false, true),
            )),
            cipher: cipher.clone(),
            events: events.clone(),
            library: kima_library::scan::LibraryCtx {
                pool: pool.clone(),
                root: dir.path().to_path_buf().into(),
                events: events.clone(),
                data_dir: dir.path().to_path_buf(),
                ignore: kima_library::watch::IgnoreSet::new(),
            },
            settings: kima_db::settings_kv::SettingsCache::new(pool, cipher),
            config: crate::IdentConfig {
                embedding: crate::EmbeddingConfig {
                    model_dir: Some(dir.path().to_path_buf()),
                    ..Default::default()
                },
                ..Default::default()
            },
            last_cycle: Default::default(),
            progress_events: crate::events::ProgressEvents::new(events),
            embedding_runtime: Default::default(),
            analysis_runtime: Default::default(),
        };
        assert!(super::cached_status(&ctx).is_none());
        let first = super::initialize(&ctx).await.unwrap();
        assert_eq!(super::cached_status(&ctx), Some(first.clone()));
        let status = serde_json::to_value(&first).unwrap();
        assert_eq!(
            status["capabilities"]["clap_audio_embedding"]["state"],
            "unavailable"
        );
        assert_eq!(
            status["capabilities"]["clap_text_embedding"]["state"],
            "unavailable"
        );
        assert_eq!(
            status["capabilities"]["clap_audio_embedding"]["errors"][0]["category"],
            "missing"
        );
        std::fs::write(dir.path().join("hf_audio.onnx"), b"changed").unwrap();
        let second = super::initialize(&ctx).await.unwrap();
        assert_eq!(first, second);
        let mut restarted = ctx.clone();
        restarted.embedding_runtime = Default::default();
        std::fs::write(dir.path().join("text_model.onnx"), b"corrupt").unwrap();
        let after_restart = super::initialize(&restarted).await.unwrap();
        assert_eq!(
            serde_json::to_value(&after_restart).unwrap()["capabilities"]["clap_text_embedding"]["errors"]
                [1]["category"],
            "hash_mismatch"
        );
        let mut disabled = ctx.clone();
        disabled.embedding_runtime = Default::default();
        disabled.config.embedding.disabled = true;
        let report = super::initialize(&disabled).await.unwrap();
        assert!(matches!(
            report.status(kima_ml::Capability::ClapTextEmbedding),
            kima_ml::CapabilityStatus::Disabled { .. }
        ));
        assert!(matches!(
            report.status(kima_ml::Capability::ClapAudioEmbedding),
            kima_ml::CapabilityStatus::Disabled { .. }
        ));
    }

    #[test]
    fn disabled_clap_does_not_attempt_to_load_missing_model() {
        let config = crate::EmbeddingConfig {
            model_dir: Some("/nonexistent-clap".into()),
            disabled: true,
            ..Default::default()
        };
        let report = super::initialize_config(&config).err().unwrap();
        assert!(
            matches!(report.status(kima_ml::Capability::ClapAudioEmbedding), kima_ml::CapabilityStatus::Disabled { reason } if reason.contains("DISABLE_CLAP"))
        );
    }

    fn ctx_with(embedding: crate::EmbeddingConfig) -> crate::IdentCtx {
        use std::sync::Arc;
        let dir = std::env::temp_dir();
        let pool = kima_db::PgPool::connect_lazy("postgres://unused:***@127.0.0.1/unused").unwrap();
        let cipher = Arc::new(kima_core::cipher::Cipher::from_secret("test"));
        let events = kima_core::events::EventBus::new(16);
        crate::IdentCtx {
            pool: pool.clone(),
            net: Arc::new(kima_net::Net::new(
                kima_net::NetConfig::for_tests(kima_net::Bases::production("http://127.0.0.1:9")),
                pool.clone(),
                kima_core::identify::IdentifySwitch::new(false, true),
            )),
            cipher: cipher.clone(),
            events: events.clone(),
            library: kima_library::scan::LibraryCtx {
                pool: pool.clone(),
                root: dir.clone().into(),
                events: events.clone(),
                data_dir: dir,
                ignore: kima_library::watch::IgnoreSet::new(),
            },
            settings: kima_db::settings_kv::SettingsCache::new(pool, cipher),
            config: crate::IdentConfig {
                embedding,
                ..Default::default()
            },
            last_cycle: Default::default(),
            progress_events: crate::events::ProgressEvents::new(events),
            embedding_runtime: Default::default(),
            analysis_runtime: Default::default(),
        }
    }

    #[tokio::test]
    async fn unconfigured_model_dir_is_unavailable_not_disabled_for_both() {
        let ctx = ctx_with(crate::EmbeddingConfig::default());
        let report = super::initialize(&ctx).await.unwrap();
        let json = serde_json::to_value(&report).unwrap();
        for cap in ["clap_audio_embedding", "clap_text_embedding"] {
            assert_eq!(json["capabilities"][cap]["state"], "unavailable", "{cap}");
            assert_eq!(
                json["capabilities"][cap]["errors"][0]["category"], "missing",
                "{cap}"
            );
        }
    }

    #[tokio::test]
    async fn disable_clap_disables_both_without_touching_the_directory() {
        let ctx = ctx_with(crate::EmbeddingConfig {
            model_dir: Some("/nonexistent-clap".into()),
            disabled: true,
            ..Default::default()
        });
        let report = super::initialize(&ctx).await.unwrap();
        for cap in [
            kima_ml::Capability::ClapAudioEmbedding,
            kima_ml::Capability::ClapTextEmbedding,
        ] {
            assert!(
                matches!(report.status(cap), kima_ml::CapabilityStatus::Disabled { reason } if reason.contains("DISABLE_CLAP")),
                "{cap:?}"
            );
        }
        assert!(matches!(
            super::embed_text(&ctx, "a dog").await,
            Err(super::TextEmbeddingError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn embed_text_without_model_is_unavailable_error() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with(crate::EmbeddingConfig {
            model_dir: Some(dir.path().into()),
            ..Default::default()
        });
        let err = super::embed_text(&ctx, "a dog").await.err().unwrap();
        assert!(
            matches!(err, super::TextEmbeddingError::Unavailable(_)),
            "{err:?}"
        );
    }

    #[test]
    fn finish_text_vector_normalizes_and_rejects_zero_and_non_finite() {
        let mut raw = [0f32; 512];
        raw[0] = 3.0;
        raw[1] = 4.0;
        let ok = super::finish_text_vector(&raw).unwrap();
        assert!((ok[0] - 0.6).abs() < 1e-6 && (ok[1] - 0.8).abs() < 1e-6);
        assert!((ok.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() - 1.0).abs() < 1e-6);
        assert!(matches!(
            super::finish_text_vector(&[0f32; 512]),
            Err(super::TextEmbeddingError::Invalid(_))
        ));
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut v = raw;
            v[7] = bad;
            assert!(
                matches!(
                    super::finish_text_vector(&v),
                    Err(super::TextEmbeddingError::Invalid(_))
                ),
                "{bad}"
            );
        }
    }

    const PRIVATE_TEXT_DIR: &str = "/home/dimmu/.cache/kima-models/m4-approved-private/xenova/onnx";
    const PRIVATE_AUDIO_DIR: &str = "/home/dimmu/.hermes/cache/scratch/clap-private-export";

    /// Builds a scratch model directory from private artifacts (hard link, else copy).
    /// Panics when an artifact is absent: these tests run only via `--ignored`, so a missing
    /// model is a failure to report, never a silent pass.
    fn private_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (src_dir, name) in files {
            let src = std::path::Path::new(src_dir).join(name);
            assert!(src.is_file(), "private artifact missing: {}", src.display());
            let dst = dir.path().join(name);
            if std::fs::hard_link(&src, &dst).is_err() {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
        dir
    }

    /// Model directory plus a configured snapshot directory: without the latter audio is
    /// (correctly) reported unavailable, see `unset_snapshot_dir_blocks_audio_with_its_own_reason`.
    fn cfg(dir: &tempfile::TempDir) -> crate::EmbeddingConfig {
        crate::EmbeddingConfig {
            model_dir: Some(dir.path().into()),
            snapshot_dir: Some(dir.path().join("snapshots")),
            ..Default::default()
        }
    }

    /// Exercise real worker detection, not a synthetic outcome handed to `finish`.
    /// The lazy, unreachable DB makes any attempted claim fail rather than pass silently.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "private noncommercial CLAP models + trusted ORT_DYLIB_PATH required"]
    async fn private_models_snapshot_latch_survives_cancel_and_caller_drop() {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };

        let models = private_dir(&[
            (PRIVATE_TEXT_DIR, "tokenizer.json"),
            (PRIVATE_TEXT_DIR, "text_model.onnx"),
            (PRIVATE_AUDIO_DIR, "hf_audio.onnx"),
        ]);
        for drop_caller in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let snapshots = tempfile::tempdir().unwrap();
            let permissions = |mode| {
                std::fs::set_permissions(snapshots.path(), std::fs::Permissions::from_mode(mode))
                    .unwrap()
            };
            permissions(0o700);
            // Invalid audio is enough: recovery must get past snapshots into decoding.
            std::fs::write(root.path().join("input.wav"), b"not an audio file").unwrap();
            let ctx = ctx_with(crate::EmbeddingConfig {
                model_dir: Some(models.path().into()),
                snapshot_dir: Some(snapshots.path().into()),
                ..Default::default()
            });
            ctx.library.root.set(root.path().to_path_buf());
            assert!(
                initialize(&ctx)
                    .await
                    .unwrap()
                    .is_available(Capability::ClapAudioEmbedding)
            );
            assert_eq!(
                fresh_block(&ctx.embedding_runtime, &ctx.config.embedding, root.path()),
                None
            );
            permissions(0o500);
            assert!(
                std::fs::File::create(snapshots.path().join("probe")).is_err(),
                "must run without CAP_DAC_OVERRIDE"
            );

            let (detected_tx, detected_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let permits = Arc::new(tokio::sync::Semaphore::new(1));
            let finished = Arc::new(AtomicBool::new(false));
            let cancel = CancellationToken::new();
            let task = tokio::spawn({
                let ctx = ctx.clone();
                let permits = permits.clone();
                let finished = finished.clone();
                let cancel = cancel.clone();
                async move {
                    compute_then_finish(
                        &cancel,
                        permits,
                        move || {
                            let outcome = run_embedding_blocking(
                                &ctx,
                                &ctx.library.root.get(),
                                Path::new("input.wav"),
                            );
                            assert!(
                                matches!(&outcome, EmbedOutcome::Snapshot(f) if halts_batch(f))
                            );
                            detected_tx.send(()).unwrap();
                            release_rx
                                .recv_timeout(std::time::Duration::from_secs(10))
                                .unwrap();
                            outcome
                        },
                        move |_| async move {
                            finished.store(true, Ordering::SeqCst);
                        },
                    )
                    .await
                    .unwrap()
                }
            });
            detected_rx.await.unwrap();
            // Detection has happened, but the worker has not returned its result.
            let at_detection = cached_status(&ctx).unwrap();
            if drop_caller {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                cancel.cancel();
                assert_eq!(task.await.unwrap(), None);
            }
            assert_eq!(
                permits.available_permits(),
                0,
                "detached work owns the permit"
            );
            release_tx.send(()).unwrap();
            let worker_done =
                tokio::time::timeout(std::time::Duration::from_secs(10), permits.acquire())
                    .await
                    .unwrap()
                    .unwrap();
            drop(worker_done);
            permissions(0o700);
            assert!(
                !finished.load(Ordering::SeqCst),
                "no finish/publication after cancellation or drop"
            );
            let json = serde_json::to_value(&at_detection).unwrap();
            assert_eq!(
                json["capabilities"]["clap_audio_embedding"]["state"], "unavailable",
                "{json}"
            );
            let original = json["capabilities"]["clap_audio_embedding"]["errors"][0]["message"]
                .as_str()
                .unwrap();
            assert!(
                original.contains("SNAPSHOT_UNAVAILABLE") && original.contains("restart"),
                "{original}"
            );
            assert_eq!(cached_status(&ctx).unwrap(), at_detection);
            // Repair is real, not merely a config change, but cannot reset the latch.
            assert!(
                snapshot_store(&ctx)
                    .unwrap()
                    .create(root.path(), Path::new("input.wav"))
                    .is_ok()
            );
            assert_eq!(
                run_batch(&ctx, &CancellationToken::new())
                    .await
                    .unwrap()
                    .claimed,
                0
            );
            assert!(
                embed_text(&ctx, "a short musical phrase")
                    .await
                    .unwrap()
                    .iter()
                    .all(|x| x.is_finite())
            );
            assert!(
                cached_status(&ctx)
                    .unwrap()
                    .is_available(Capability::ClapTextEmbedding)
            );
            // A different global failure must not replace the first cause.
            let other = tempfile::tempdir_in("/dev/shm").unwrap();
            std::fs::write(other.path().join("input.wav"), b"not audio").unwrap();
            assert!(
                matches!(run_embedding_blocking(&ctx, other.path(), Path::new("input.wav")), EmbedOutcome::Snapshot(f) if halts_batch(&f))
            );
            assert_eq!(cached_status(&ctx).unwrap(), at_detection);
            let mut restarted = ctx.clone();
            restarted.embedding_runtime = Default::default();
            assert_eq!(
                fresh_block(
                    &restarted.embedding_runtime,
                    &restarted.config.embedding,
                    root.path()
                ),
                None
            );
            assert!(
                matches!(
                    run_embedding_blocking(&restarted, root.path(), Path::new("input.wav")),
                    EmbedOutcome::DecodeError(_)
                ),
                "fresh context passes repaired snapshot setup"
            );
            assert!(
                cached_status(&restarted)
                    .unwrap()
                    .is_available(Capability::ClapAudioEmbedding)
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "private noncommercial CLAP models + trusted ORT_DYLIB_PATH required"]
    async fn private_models_audio_and_text_both_available_and_text_embeds() {
        let dir = private_dir(&[
            (PRIVATE_TEXT_DIR, "tokenizer.json"),
            (PRIVATE_TEXT_DIR, "text_model.onnx"),
            (PRIVATE_AUDIO_DIR, "hf_audio.onnx"),
        ]);
        let ctx = ctx_with(cfg(&dir));
        let report = super::initialize(&ctx).await.unwrap();
        assert_eq!(
            report.status(kima_ml::Capability::ClapAudioEmbedding),
            &kima_ml::CapabilityStatus::Available
        );
        assert_eq!(
            report.status(kima_ml::Capability::ClapTextEmbedding),
            &kima_ml::CapabilityStatus::Available
        );
        let dog = super::embed_text(&ctx, "a dog barking").await.unwrap();
        let again = super::embed_text(&ctx, "a dog barking").await.unwrap();
        let piano = super::embed_text(&ctx, "a calm piano melody")
            .await
            .unwrap();
        let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        assert!((norm(&dog) - 1.0).abs() < 1e-3 && (norm(&piano) - 1.0).abs() < 1e-3);
        assert_eq!(dog, again, "text embedding is deterministic");
        let cos: f64 = dog
            .iter()
            .zip(&piano)
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum();
        assert!(cos < 0.999, "distinct prompts must not collapse: cos={cos}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "private noncommercial CLAP models + trusted ORT_DYLIB_PATH required"]
    async fn private_models_corrupt_tokenizer_leaves_audio_available() {
        let dir = private_dir(&[
            (PRIVATE_TEXT_DIR, "text_model.onnx"),
            (PRIVATE_AUDIO_DIR, "hf_audio.onnx"),
        ]);
        std::fs::write(dir.path().join("tokenizer.json"), b"{}").unwrap();
        let report = super::initialize(&ctx_with(cfg(&dir))).await.unwrap();
        assert_eq!(
            report.status(kima_ml::Capability::ClapAudioEmbedding),
            &kima_ml::CapabilityStatus::Available
        );
        assert!(matches!(
            report.status(kima_ml::Capability::ClapTextEmbedding),
            kima_ml::CapabilityStatus::Unavailable { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "private noncommercial CLAP models + trusted ORT_DYLIB_PATH required"]
    async fn private_models_missing_audio_leaves_text_available() {
        let dir = private_dir(&[
            (PRIVATE_TEXT_DIR, "tokenizer.json"),
            (PRIVATE_TEXT_DIR, "text_model.onnx"),
        ]);
        let ctx = ctx_with(cfg(&dir));
        let report = super::initialize(&ctx).await.unwrap();
        assert!(matches!(
            report.status(kima_ml::Capability::ClapAudioEmbedding),
            kima_ml::CapabilityStatus::Unavailable { .. }
        ));
        assert_eq!(
            report.status(kima_ml::Capability::ClapTextEmbedding),
            &kima_ml::CapabilityStatus::Available
        );
        assert!(super::embed_text(&ctx, "rain").await.is_ok());
    }
}
