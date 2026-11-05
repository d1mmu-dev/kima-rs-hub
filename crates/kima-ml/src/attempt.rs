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
//!
//! **Analysis-stage group independence (D-157).** The analysis callback computes and
//! publishes four independently-nullable groups (MusiCNN aggregate; DSP loudness/key/
//! tempo — D-154). `AnalysisAvailability` tells the callback which model-backed group
//! (today only MusiCNN) is available *before* it decodes anything, mirroring the
//! existing CLAP-unavailable/`DISABLE_CLAP` degradation pattern: a missing or disabled
//! capability disables only its own group, never the whole analysis stage. DSP's three
//! groups have no model file (`Capability::Dsp` is satisfied by an empty artifact list)
//! so they are expected to stay available and are passed through the same way for
//! symmetry and any future per-group DSP kill switch. A capability-unavailable group and
//! a group whose own computation errors on this file are both represented identically by
//! the callback: simply omit that group from what it publishes (`None`/not attempted).
//! Only an actual storage rejection (the written group's `file_generation` fence lost a
//! race) must surface as `Err` from the callback — that invalidates the whole attempt's
//! premise, not one group, and must not be silently treated as a resolved group. See
//! [`analysis_groups_resolved`] for the resulting stage-completion policy and
//! `docs/DIVERGENCES.md` D-157 for the full grounding.

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

/// Which analysis groups' model-backed capability is available for this attempt,
/// decided once before decode so the analysis callback can skip only the dependent
/// group(s) instead of the whole stage (D-157). DSP's three groups (loudness/key/tempo)
/// have no model file and are expected to stay available; a future per-group DSP kill
/// switch would report it the same way MusiCNN already does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisAvailability {
    pub musicnn: bool,
    pub dsp: bool,
}

/// The analysis-stage completion policy (D-157), grounded in `track_analysis`'s
/// per-group independence (D-154): the attempt is resolved — the callback should
/// report success and the caller's `'analysis'` stage row should move to `done` —
/// once every group has reached *some* conclusion, whether that is a successful
/// publish, a definitively unavailable/disabled capability, or that group's own
/// compute/publish step simply not being attempted. It does **not** require all four
/// groups to succeed: a track with only MusiCNN+loudness available (or whose key/tempo
/// computation itself errored on this file) still completes, with those columns left
/// `NULL` in `track_analysis` (D-154's own "all-or-nothing per group, not across
/// groups" design).
///
/// The one outcome this policy does *not* accept as resolved is a group whose storage
/// write was itself rejected (`Some(Err(_))`): that means the attempt computed against
/// a `file_generation` that is no longer current for the whole track (changed file,
/// went missing, or was deleted) between claim and publish, which invalidates every
/// group's result, not just the rejected one's. Such an attempt must be reported as a
/// (retryable) failure so the caller redelivers it, not reported done with partial data
/// from an already-stale generation.
///
/// Generic over the per-group error type so this stays decoupled from
/// `kima_db`/`analysis_publish` and keeps this module's default build DB-free; a
/// caller maps `AnalysisPublishReport { musicnn, loudness, key, tempo }` into the
/// `[Option<Result<(), E>>; 4]` shape before calling this.
pub fn analysis_groups_resolved<E>(groups: [Option<&Result<(), E>>; 4]) -> bool {
    groups
        .into_iter()
        .all(|group| !matches!(group, Some(Err(_))))
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
///
/// `analyze_and_publish` receives [`AnalysisAvailability`] alongside the decoded tail:
/// per D-157, a missing/disabled model-backed group (today only MusiCNN) must disable
/// only that group, not the whole attempt — the attempt still decodes and the callback
/// still runs for the remaining groups. The attempt is only skipped entirely (no decode,
/// `StageOutcome::Blocked`) when *no* analysis group could possibly produce anything.
#[allow(clippy::too_many_arguments)]
pub fn run_attempt<E>(
    kind: AttemptKind,
    capabilities: &CapabilityEvaluationReport,
    mut open: impl FnMut() -> Result<Box<dyn MediaSource>, AudioError>,
    hint: &Hint,
    frame_hint: Option<usize>,
    analyze_and_publish: impl FnOnce(&[f32], AnalysisAvailability) -> Result<(), E>,
    embed_and_publish: impl FnOnce(&[Vec<f32>], u32) -> Result<(), E>,
) -> Result<AttemptReport<E>, AudioError> {
    let availability = AnalysisAvailability {
        musicnn: capabilities.is_available(Capability::MusicnnAnalysis),
        dsp: capabilities.is_available(Capability::Dsp),
    };
    if kind == AttemptKind::Combined && !availability.musicnn && !availability.dsp {
        let blocked: Vec<_> = [Capability::MusicnnAnalysis, Capability::Dsp]
            .into_iter()
            .map(|cap| (cap, capabilities.status(cap).clone()))
            .collect();
        return Ok(AttemptReport {
            analysis: StageOutcome::Blocked(blocked),
            embedding: StageOutcome::AnalysisPrerequisite,
        });
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
        AttemptKind::Combined => match analyze_and_publish(&audio.mono_16k, availability) {
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

#[cfg(test)]
mod completion_policy_tests {
    use super::analysis_groups_resolved;

    fn ok() -> Result<(), &'static str> {
        Ok(())
    }

    fn err() -> Result<(), &'static str> {
        Err("rejected")
    }

    #[test]
    fn all_groups_absent_is_resolved() {
        // Nothing attempted (e.g. no capability available at all): vacuously resolved,
        // matching D-157 — the stage still completes with an empty row rather than
        // waiting forever for groups that were never going to run.
        assert!(analysis_groups_resolved::<&str>([None, None, None, None]));
    }

    #[test]
    fn all_groups_published_is_resolved() {
        let (m, l, k, t) = (ok(), ok(), ok(), ok());
        assert!(analysis_groups_resolved([
            Some(&m),
            Some(&l),
            Some(&k),
            Some(&t)
        ]));
    }

    #[test]
    fn only_musicnn_and_loudness_present_and_published_is_resolved() {
        // The task's own grounding example: key/tempo never attempted (e.g. capability
        // degraded for this attempt in a hypothetical future per-group DSP switch), the
        // two present groups published — the stage still completes.
        let (m, l) = (ok(), ok());
        assert!(analysis_groups_resolved::<&str>([
            Some(&m),
            Some(&l),
            None,
            None
        ]));
    }

    #[test]
    fn a_group_that_errored_without_being_a_storage_rejection_is_still_resolved() {
        // D-157: the callback represents "attempted and failed to compute" the same way
        // as "not attempted" by simply omitting that group (`None`) before calling
        // `analysis_publish::publish_track_analysis` — this function only ever sees a
        // `Some(Err(_))` for an actual storage rejection, never a bare compute error, so
        // there is nothing additional to special-case here; this test pins that only the
        // *rejection* shape is treated as unresolved, not "some group is missing".
        let (m, l, k) = (ok(), ok(), ok());
        assert!(analysis_groups_resolved::<&str>([
            Some(&m),
            Some(&l),
            Some(&k),
            None
        ]));
    }

    #[test]
    fn any_rejected_group_is_not_resolved() {
        let (m, l, k, t) = (ok(), ok(), ok(), err());
        assert!(!analysis_groups_resolved([
            Some(&m),
            Some(&l),
            Some(&k),
            Some(&t)
        ]));
    }

    #[test]
    fn a_single_rejected_group_among_absent_ones_is_not_resolved() {
        let m = err();
        assert!(!analysis_groups_resolved::<&str>([
            Some(&m),
            None,
            None,
            None
        ]));
    }
}
