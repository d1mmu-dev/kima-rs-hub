mod common;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Cursor;

use kima_ml::attempt::{AnalysisAvailability, AttemptKind, StageOutcome, run_attempt};
use kima_ml::onnx::{MINIMAL_IDENTITY_ONNX, OnnxModelConsumer};
use kima_ml::{
    ArtifactId, Capability, CapabilityEvaluationReport, CapabilityStatus, ModelConsumer,
};
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSource;

fn capabilities() -> CapabilityEvaluationReport {
    CapabilityEvaluationReport {
        capabilities: BTreeMap::from([
            (Capability::MusicnnAnalysis, CapabilityStatus::Available),
            (Capability::Dsp, CapabilityStatus::Available),
            (Capability::ClapAudioEmbedding, CapabilityStatus::Available),
        ]),
    }
}

// Original synthetic PCM, not music or real-model reference output.
fn wav() -> Vec<u8> {
    let frames = 8_000u32;
    let size = frames * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&8_000u32.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&size.to_le_bytes());
    for i in 0..frames {
        bytes.extend_from_slice(&(((i % 101) as i16 - 50) * 100).to_le_bytes());
    }
    bytes
}

fn source(bytes: &[u8]) -> Box<dyn MediaSource> {
    Box::new(Cursor::new(bytes.to_vec()))
}

#[test]
fn duration_discovery_stays_inside_the_attempt_decode_boundary() {
    let bytes = wav();
    for frame_hint in [None, Some(0), Some(7_999), Some(8_001)] {
        let opens = Cell::new(0);
        let analyses = Cell::new(0);
        let report = run_attempt(
            AttemptKind::Combined,
            &capabilities(),
            || {
                opens.set(opens.get() + 1);
                Ok(source(&bytes))
            },
            &Hint::new(),
            frame_hint,
            |_, _| {
                analyses.set(analyses.get() + 1);
                Ok::<_, &str>(())
            },
            |windows, _| {
                assert!(!windows.is_empty());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(opens.get(), 2, "hint {frame_hint:?}");
        assert_eq!(analyses.get(), 1);
        assert_eq!(report.analysis, StageOutcome::Published);
        assert_eq!(report.embedding, StageOutcome::Published);
    }
}

#[test]
fn decode_failure_never_calls_publication_callbacks() {
    let error = run_attempt(
        AttemptKind::Combined,
        &capabilities(),
        || Ok(source(b"invalid synthetic audio")),
        &Hint::new(),
        Some(8_000),
        |_, _| -> Result<(), &str> { panic!("invalid audio cannot complete analysis") },
        |_, _| -> Result<(), &str> { panic!("invalid audio cannot complete embedding") },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        kima_ml::audio::AudioError::UnsupportedFormat(_)
    ));
}

#[test]
fn changed_source_between_discovery_passes_cannot_publish() {
    let bytes = wav();
    let mut changed = bytes.clone();
    changed[100] ^= 1;
    let opens = Cell::new(0);
    let error = run_attempt(
        AttemptKind::Combined,
        &capabilities(),
        || {
            opens.set(opens.get() + 1);
            Ok(source(if opens.get() == 1 { &bytes } else { &changed }))
        },
        &Hint::new(),
        None,
        |_, _| -> Result<(), &str> { panic!("changed source cannot complete analysis") },
        |_, _| -> Result<(), &str> { panic!("changed source cannot complete embedding") },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        kima_ml::audio::AudioError::DecodedIdentityMismatch
    ));
    assert_eq!(opens.get(), 2);
}

#[test]
fn embedding_retry_ignores_unavailable_analysis_models() {
    let bytes = wav();
    let mut caps = capabilities();
    caps.capabilities.remove(&Capability::MusicnnAnalysis);
    caps.capabilities.remove(&Capability::Dsp);
    let report = run_attempt(
        AttemptKind::EmbeddingAfterAnalysis,
        &caps,
        || Ok(source(&bytes)),
        &Hint::new(),
        Some(8_000),
        |_, _| -> Result<(), &str> { panic!("durably completed analysis must not rerun") },
        |windows, _| {
            assert!(!windows.is_empty());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(report.analysis, StageOutcome::AlreadyPublished);
    assert_eq!(report.embedding, StageOutcome::Published);
}

#[test]
fn disabled_embedding_retry_does_not_open_audio_or_claim_success() {
    let mut caps = capabilities();
    let disabled = CapabilityStatus::Disabled {
        reason: "DISABLE_CLAP".into(),
    };
    caps.capabilities
        .insert(Capability::ClapAudioEmbedding, disabled.clone());
    let report = run_attempt(
        AttemptKind::EmbeddingAfterAnalysis,
        &caps,
        || -> Result<Box<dyn MediaSource>, kima_ml::audio::AudioError> {
            panic!("no runnable work")
        },
        &Hint::new(),
        None,
        |_, _| -> Result<(), &str> { panic!("completed analysis") },
        |_, _| -> Result<(), &str> { panic!("disabled embedding") },
    )
    .unwrap();
    assert_eq!(report.analysis, StageOutcome::AlreadyPublished);
    assert_eq!(
        report.embedding,
        StageOutcome::Blocked(vec![(Capability::ClapAudioEmbedding, disabled)])
    );
}

#[test]
fn both_analysis_capabilities_missing_blocks_and_does_not_decode() {
    // D-157: the whole analysis attempt is only skipped (no decode at all) when *no*
    // group could possibly produce anything — MusiCNN and DSP both unavailable.
    let mut caps = capabilities();
    caps.capabilities.remove(&Capability::MusicnnAnalysis);
    caps.capabilities.remove(&Capability::Dsp);
    let report = run_attempt(
        AttemptKind::Combined,
        &caps,
        || -> Result<Box<dyn MediaSource>, kima_ml::audio::AudioError> {
            panic!("blocked attempt must not open audio")
        },
        &Hint::new(),
        Some(8_000),
        |_, _| -> Result<(), &str> { panic!("no analysis capability at all") },
        |_, _| -> Result<(), &str> { panic!("analysis prerequisite unavailable") },
    )
    .unwrap();
    assert_eq!(
        report.analysis,
        StageOutcome::Blocked(vec![
            (
                Capability::MusicnnAnalysis,
                caps.status(Capability::MusicnnAnalysis).clone()
            ),
            (Capability::Dsp, caps.status(Capability::Dsp).clone())
        ])
    );
    assert_eq!(report.embedding, StageOutcome::AnalysisPrerequisite);
}

#[test]
fn missing_musicnn_alone_still_decodes_and_degrades_only_that_group() {
    // D-157 / independent per-group degradation: MusiCNN missing must not block DSP's
    // groups (loudness/key/tempo) or the whole analysis stage.
    let bytes = wav();
    let mut caps = capabilities();
    caps.capabilities.remove(&Capability::MusicnnAnalysis);
    let opens = Cell::new(0);
    let report = run_attempt(
        AttemptKind::Combined,
        &caps,
        || {
            opens.set(opens.get() + 1);
            Ok(source(&bytes))
        },
        &Hint::new(),
        Some(8_000),
        |tail, availability| {
            assert!(!tail.is_empty());
            assert_eq!(
                availability,
                AnalysisAvailability {
                    musicnn: false,
                    dsp: true
                }
            );
            Ok::<_, &str>(())
        },
        |windows, _| {
            assert!(!windows.is_empty());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        opens.get(),
        1,
        "accurate frame_hint needs no discovery pass"
    );
    assert_eq!(report.analysis, StageOutcome::Published);
    assert_eq!(report.embedding, StageOutcome::Published);
}

#[test]
fn missing_dsp_alone_still_decodes_and_degrades_only_that_group() {
    let bytes = wav();
    let mut caps = capabilities();
    caps.capabilities.remove(&Capability::Dsp);
    let report = run_attempt(
        AttemptKind::Combined,
        &caps,
        || Ok(source(&bytes)),
        &Hint::new(),
        Some(8_000),
        |_, availability| {
            assert_eq!(
                availability,
                AnalysisAvailability {
                    musicnn: true,
                    dsp: false
                }
            );
            Ok::<_, &str>(())
        },
        |_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(report.analysis, StageOutcome::Published);
    assert_eq!(report.embedding, StageOutcome::Published);
}

#[test]
fn analysis_failure_blocks_combined_embedding_publication() {
    let bytes = wav();
    let report = run_attempt(
        AttemptKind::Combined,
        &capabilities(),
        || Ok(source(&bytes)),
        &Hint::new(),
        Some(8_000),
        |_, _| Err("analysis publication rejected"),
        |_, _| -> Result<(), &str> { panic!("analysis prerequisite did not publish") },
    )
    .unwrap();
    assert_eq!(
        report.analysis,
        StageOutcome::Failed("analysis publication rejected")
    );
    assert_eq!(report.embedding, StageOutcome::AnalysisPrerequisite);
}

#[test]
fn unavailable_clap_does_not_block_analysis_or_report_embedding_success() {
    let bytes = wav();
    let mut caps = capabilities();
    let unavailable = CapabilityStatus::Unavailable { errors: vec![] };
    caps.capabilities
        .insert(Capability::ClapAudioEmbedding, unavailable.clone());
    let opens = Cell::new(0);
    let report = run_attempt(
        AttemptKind::Combined,
        &caps,
        || {
            opens.set(opens.get() + 1);
            Ok(source(&bytes))
        },
        &Hint::new(),
        None,
        |tail, _| {
            assert_eq!(tail.len(), 16_000);
            Ok::<_, &str>(())
        },
        |_, _| -> Result<(), &str> { panic!("unavailable embedding must not run") },
    )
    .unwrap();
    assert_eq!(opens.get(), 1, "no CLAP discovery pass when unavailable");
    assert_eq!(report.analysis, StageOutcome::Published);
    assert_eq!(
        report.embedding,
        StageOutcome::Blocked(vec![(Capability::ClapAudioEmbedding, unavailable)])
    );
}

#[test]
fn embedding_retry_redecodes_without_repeating_published_analysis() {
    let bytes = wav();
    let opens = Cell::new(0);
    let analyses = Cell::new(0);
    let first = run_attempt(
        AttemptKind::Combined,
        &capabilities(),
        || {
            opens.set(opens.get() + 1);
            Ok(source(&bytes))
        },
        &Hint::new(),
        Some(8_000),
        |_, _| {
            analyses.set(analyses.get() + 1);
            Ok::<_, &str>(())
        },
        |_, _| Err("synthetic embedding failure"),
    )
    .unwrap();
    assert_eq!(first.analysis, StageOutcome::Published);
    assert_eq!(
        first.embedding,
        StageOutcome::Failed("synthetic embedding failure")
    );
    // A newly constructed attempt models restart: no in-memory PCM or runner state reused.
    let retry = run_attempt(
        AttemptKind::EmbeddingAfterAnalysis,
        &capabilities(),
        || {
            opens.set(opens.get() + 1);
            Ok(source(&bytes))
        },
        &Hint::new(),
        Some(8_000),
        |_, _| -> Result<(), &str> { panic!("completed analysis must not rerun") },
        |windows, rate| {
            assert!(!windows.is_empty());
            assert_eq!(rate, 8_000);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(retry.analysis, StageOutcome::AlreadyPublished);
    assert_eq!(retry.embedding, StageOutcome::Published);
    assert_eq!(opens.get(), 2);
    assert_eq!(analyses.get(), 1);
}

#[test]
fn combined_attempt_shares_decode_and_publishes_analysis_before_embedding() {
    let session = OnnxModelConsumer::new()
        .load(common::verified_fixture(
            ArtifactId::from_static("synthetic-attempt-identity"),
            MINIMAL_IDENTITY_ONNX.into(),
        ))
        .unwrap();
    let bytes = wav();
    let opens = Cell::new(0);
    let order = RefCell::new(Vec::new());
    let report = run_attempt(
        AttemptKind::Combined,
        &capabilities(),
        || {
            opens.set(opens.get() + 1);
            Ok(source(&bytes))
        },
        &Hint::new(),
        Some(8_000),
        |tail, availability| {
            assert_eq!(tail.len(), 16_000);
            assert!(tail.iter().any(|v| *v != 0.0));
            assert_eq!(
                availability,
                AnalysisAvailability {
                    musicnn: true,
                    dsp: true
                }
            );
            assert_eq!(session.run_1d(&[tail[100]]).unwrap(), vec![tail[100]]);
            order.borrow_mut().push("analysis published");
            Ok::<_, &'static str>(())
        },
        |windows, rate| {
            assert_eq!(rate, 8_000);
            assert!(!windows.is_empty());
            assert_eq!(*order.borrow(), ["analysis published"]);
            assert_eq!(
                session.run_1d(&[windows[0][100]]).unwrap(),
                vec![windows[0][100]]
            );
            order.borrow_mut().push("embedding published");
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(opens.get(), 1);
    assert_eq!(report.analysis, StageOutcome::Published);
    assert_eq!(report.embedding, StageOutcome::Published);
    assert_eq!(
        *order.borrow(),
        ["analysis published", "embedding published"]
    );
}
