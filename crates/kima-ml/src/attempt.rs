//! Execution of one caller-admitted ML attempt, not a scheduler or result schema.
//!
//! Callbacks must finish fenced, atomic publication before returning `Ok(())`.
//! `Published` is the callback's acknowledgement, not an independently checked DB fact.
//! In particular, a stale claim rejected by storage must be returned as an error,
//! not converted to `Ok(())`. No transaction may span both callbacks or audio work.
//! Analysis receives the existing 16 kHz tail; embedding receives the existing
//! native-rate whole-track windows and their rate, not the 48 kHz tail. Embedding
//! preparation, model invocation and output persistence belong to the callback.
//! The caller owns eligibility, claims, cancellation, admission and a stable source;
//! this module must not be installed as a production worker without those boundaries.

use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSource;

use crate::audio::{AudioError, decode_and_resample_media_source, decode_clap_auto};
use crate::{Capability, CapabilityEvaluationReport, CapabilityStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptKind {
    Combined,
    /// Caller has read current, durably completed analysis for the same file generation.
    EmbeddingAfterAnalysis,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StageOutcome<E> {
    Published,
    AlreadyPublished,
    Failed(E),
    AnalysisPrerequisite,
    Blocked(Vec<(Capability, CapabilityStatus)>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct AttemptReport<E> {
    pub analysis: StageOutcome<E>,
    pub embedding: StageOutcome<E>,
}

/// Run analysis/publication before embedding/publication, sharing the bounded decode.
/// `frame_hint` has the existing decoder's semantics: absent/inaccurate hints may
/// require a second, fingerprint-verified pass. No decoded intermediate is persisted.
#[allow(clippy::too_many_arguments)]
pub fn run_attempt<E>(
    kind: AttemptKind,
    capabilities: &CapabilityEvaluationReport,
    mut open: impl FnMut() -> Result<Box<dyn MediaSource>, AudioError>,
    hint: &Hint,
    frame_hint: Option<usize>,
    analyze_and_publish: impl FnOnce(&[f32]) -> Result<(), E>,
    embed_and_publish: impl FnOnce(&[Vec<f32>], u32) -> Result<(), E>,
) -> Result<AttemptReport<E>, AudioError> {
    if kind == AttemptKind::Combined {
        let blocked: Vec<_> = [Capability::MusicnnAnalysis, Capability::Dsp]
            .into_iter()
            .filter(|&cap| !capabilities.is_available(cap))
            .map(|cap| (cap, capabilities.status(cap).clone()))
            .collect();
        if !blocked.is_empty() {
            return Ok(AttemptReport {
                analysis: StageOutcome::Blocked(blocked),
                embedding: StageOutcome::AnalysisPrerequisite,
            });
        }
    }
    let embedding_ready = capabilities.is_available(Capability::ClapAudioEmbedding);
    if kind == AttemptKind::EmbeddingAfterAnalysis && !embedding_ready {
        return Ok(AttemptReport {
            analysis: StageOutcome::AlreadyPublished,
            embedding: StageOutcome::Blocked(vec![(
                Capability::ClapAudioEmbedding,
                capabilities.status(Capability::ClapAudioEmbedding).clone(),
            )]),
        });
    }
    let (audio, windows, rate) = if embedding_ready {
        decode_clap_auto(open, hint, frame_hint)?
    } else {
        (
            decode_and_resample_media_source(open()?, hint)?,
            Vec::new(),
            0,
        )
    };
    let analysis = match kind {
        AttemptKind::EmbeddingAfterAnalysis => StageOutcome::AlreadyPublished,
        AttemptKind::Combined => match analyze_and_publish(&audio.mono_16k) {
            Ok(()) => StageOutcome::Published,
            Err(error) => StageOutcome::Failed(error),
        },
    };
    let embedding = if !matches!(
        analysis,
        StageOutcome::Published | StageOutcome::AlreadyPublished
    ) {
        StageOutcome::AnalysisPrerequisite
    } else if embedding_ready {
        match embed_and_publish(&windows, rate) {
            Ok(()) => StageOutcome::Published,
            Err(error) => StageOutcome::Failed(error),
        }
    } else {
        StageOutcome::Blocked(vec![(
            Capability::ClapAudioEmbedding,
            capabilities.status(Capability::ClapAudioEmbedding).clone(),
        )])
    };
    Ok(AttemptReport {
        analysis,
        embedding,
    })
}
