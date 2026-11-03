use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use tempfile::tempdir;

use kima_ml::{
    ArtifactDescriptor, ArtifactId, Capability, CapabilityConfig, CapabilityStatus,
    FailureCategory, evaluate_capabilities,
};

fn create_synthetic_artifact(
    dir: &std::path::Path,
    basename: &'static str,
    id: &'static str,
    payload: &[u8],
) -> ArtifactDescriptor {
    let path = dir.join(basename);
    fs::write(&path, payload).unwrap();
    let expected_sha256: [u8; 32] = Sha256::digest(payload).into();
    ArtifactDescriptor {
        id: ArtifactId::from_static(id),
        basename,
        expected_sha256,
        max_bytes: 4096,
    }
}

#[test]
fn test_all_synthetic_artifacts_valid_evaluates_all_capabilities_available() {
    let dir = tempdir().unwrap();

    let musicnn_desc = create_synthetic_artifact(
        dir.path(),
        "msd-musicnn.bin",
        "musicnn-body",
        b"musicnn-weights",
    );
    let clap_audio_desc = create_synthetic_artifact(
        dir.path(),
        "hf_audio.bin",
        "clap-audio",
        b"clap-audio-weights",
    );
    let clap_text_desc = create_synthetic_artifact(
        dir.path(),
        "text_model.bin",
        "clap-text",
        b"clap-text-weights",
    );

    let mut requirements: BTreeMap<Capability, Vec<ArtifactDescriptor>> = BTreeMap::new();
    requirements.insert(Capability::MusicnnAnalysis, vec![musicnn_desc]);
    requirements.insert(Capability::ClapAudioEmbedding, vec![clap_audio_desc]);
    requirements.insert(Capability::ClapTextEmbedding, vec![clap_text_desc]);
    requirements.insert(Capability::Dsp, vec![]); // DSP requires no models

    let config = CapabilityConfig {
        disable_clap: false,
    };
    let report = evaluate_capabilities(dir.path(), &config, &requirements);

    assert_eq!(
        report.status(Capability::MusicnnAnalysis),
        &CapabilityStatus::Available
    );
    assert_eq!(
        report.status(Capability::ClapAudioEmbedding),
        &CapabilityStatus::Available
    );
    assert_eq!(
        report.status(Capability::ClapTextEmbedding),
        &CapabilityStatus::Available
    );
    assert_eq!(report.status(Capability::Dsp), &CapabilityStatus::Available);
}

#[test]
fn test_missing_musicnn_model_disables_only_musicnn_analysis() {
    let dir = tempdir().unwrap();

    // musicnn file is NOT created in dir (missing!)
    let musicnn_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("musicnn-body"),
        basename: "missing_musicnn.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 4096,
    };
    let clap_audio_desc = create_synthetic_artifact(
        dir.path(),
        "hf_audio.bin",
        "clap-audio",
        b"clap-audio-weights",
    );
    let clap_text_desc = create_synthetic_artifact(
        dir.path(),
        "text_model.bin",
        "clap-text",
        b"clap-text-weights",
    );

    let mut requirements: BTreeMap<Capability, Vec<ArtifactDescriptor>> = BTreeMap::new();
    requirements.insert(Capability::MusicnnAnalysis, vec![musicnn_desc]);
    requirements.insert(Capability::ClapAudioEmbedding, vec![clap_audio_desc]);
    requirements.insert(Capability::ClapTextEmbedding, vec![clap_text_desc]);
    requirements.insert(Capability::Dsp, vec![]);

    let config = CapabilityConfig {
        disable_clap: false,
    };
    let report = evaluate_capabilities(dir.path(), &config, &requirements);

    match report.status(Capability::MusicnnAnalysis) {
        CapabilityStatus::Unavailable { errors } => {
            assert_eq!(errors.len(), 1);
            assert_eq!(
                errors[0].artifact_id,
                ArtifactId::from_static("musicnn-body")
            );
            assert_eq!(errors[0].category, FailureCategory::Missing);
        }
        other => panic!(
            "expected MusicnnAnalysis to be Unavailable, got {:?}",
            other
        ),
    }

    // Other independent capabilities remain Available!
    assert_eq!(
        report.status(Capability::ClapAudioEmbedding),
        &CapabilityStatus::Available
    );
    assert_eq!(
        report.status(Capability::ClapTextEmbedding),
        &CapabilityStatus::Available
    );
    assert_eq!(report.status(Capability::Dsp), &CapabilityStatus::Available);
}

#[test]
fn test_disable_clap_config_skips_clap_without_treating_as_error() {
    let dir = tempdir().unwrap();

    let musicnn_desc = create_synthetic_artifact(
        dir.path(),
        "msd-musicnn.bin",
        "musicnn-body",
        b"musicnn-weights",
    );
    // CLAP models don't even exist on disk
    let clap_audio_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("clap-audio"),
        basename: "absent_audio.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 4096,
    };
    let clap_text_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("clap-text"),
        basename: "absent_text.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 4096,
    };

    let mut requirements = BTreeMap::new();
    requirements.insert(Capability::MusicnnAnalysis, vec![musicnn_desc]);
    requirements.insert(Capability::ClapAudioEmbedding, vec![clap_audio_desc]);
    requirements.insert(Capability::ClapTextEmbedding, vec![clap_text_desc]);
    requirements.insert(Capability::Dsp, vec![]);

    let config = CapabilityConfig { disable_clap: true };
    let report = evaluate_capabilities(dir.path(), &config, &requirements);

    assert_eq!(
        report.status(Capability::MusicnnAnalysis),
        &CapabilityStatus::Available
    );
    assert_eq!(report.status(Capability::Dsp), &CapabilityStatus::Available);

    // CLAP capabilities are marked Disabled, NOT Unavailable with errors
    match report.status(Capability::ClapAudioEmbedding) {
        CapabilityStatus::Disabled { reason } => assert!(reason.contains("DISABLE_CLAP")),
        other => panic!("expected Disabled, got {:?}", other),
    }
    match report.status(Capability::ClapTextEmbedding) {
        CapabilityStatus::Disabled { reason } => assert!(reason.contains("DISABLE_CLAP")),
        other => panic!("expected Disabled, got {:?}", other),
    }
}

#[test]
fn test_independent_audio_and_text_clap_failure() {
    let dir = tempdir().unwrap();

    let clap_audio_file = dir.path().join("hf_audio.bin");
    fs::write(&clap_audio_file, b"corrupted-audio").unwrap();
    let clap_audio_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("clap-audio"),
        basename: "hf_audio.bin",
        expected_sha256: [0x55u8; 32], // mismatch
        max_bytes: 4096,
    };

    let clap_text_desc =
        create_synthetic_artifact(dir.path(), "text_model.bin", "clap-text", b"valid-text");

    let mut requirements = BTreeMap::new();
    requirements.insert(Capability::ClapAudioEmbedding, vec![clap_audio_desc]);
    requirements.insert(Capability::ClapTextEmbedding, vec![clap_text_desc]);

    let config = CapabilityConfig {
        disable_clap: false,
    };
    let report = evaluate_capabilities(dir.path(), &config, &requirements);

    match report.status(Capability::ClapAudioEmbedding) {
        CapabilityStatus::Unavailable { errors } => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].category, FailureCategory::HashMismatch);
        }
        other => panic!("expected Unavailable, got {:?}", other),
    }
    assert_eq!(
        report.status(Capability::ClapTextEmbedding),
        &CapabilityStatus::Available
    );
}

#[test]
#[cfg(unix)]
fn test_fifo_causes_only_dependent_capability_unavailable() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempdir().unwrap();
    let fifo_path = dir.path().join("fifo_model.bin");
    let c_path = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
    unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };

    let fifo_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("musicnn-fifo"),
        basename: "fifo_model.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 4096,
    };

    let clap_audio_desc =
        create_synthetic_artifact(dir.path(), "hf_audio.bin", "clap-audio", b"valid-audio");

    let mut requirements = BTreeMap::new();
    requirements.insert(Capability::MusicnnAnalysis, vec![fifo_desc]);
    requirements.insert(Capability::ClapAudioEmbedding, vec![clap_audio_desc]);
    requirements.insert(Capability::Dsp, vec![]);

    let config = CapabilityConfig {
        disable_clap: false,
    };
    let report = evaluate_capabilities(dir.path(), &config, &requirements);

    match report.status(Capability::MusicnnAnalysis) {
        CapabilityStatus::Unavailable { errors } => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].category, FailureCategory::NotRegular);
        }
        other => panic!("expected NotRegular, got {:?}", other),
    }
    assert_eq!(
        report.status(Capability::ClapAudioEmbedding),
        &CapabilityStatus::Available
    );
    assert_eq!(report.status(Capability::Dsp), &CapabilityStatus::Available);
}

#[test]
fn test_multiple_failures_collected_without_early_exit() {
    let dir = tempdir().unwrap();

    let bad_musicnn_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("musicnn-head"),
        basename: "missing_head.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 4096,
    };
    let bad_clap_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("clap-audio"),
        basename: "missing_clap.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 4096,
    };

    let mut requirements = BTreeMap::new();
    requirements.insert(Capability::MusicnnAnalysis, vec![bad_musicnn_desc]);
    requirements.insert(Capability::ClapAudioEmbedding, vec![bad_clap_desc]);
    requirements.insert(Capability::Dsp, vec![]);

    let config = CapabilityConfig {
        disable_clap: false,
    };
    let report = evaluate_capabilities(dir.path(), &config, &requirements);

    assert!(matches!(
        report.status(Capability::MusicnnAnalysis),
        CapabilityStatus::Unavailable { .. }
    ));
    assert!(matches!(
        report.status(Capability::ClapAudioEmbedding),
        CapabilityStatus::Unavailable { .. }
    ));
    assert_eq!(report.status(Capability::Dsp), &CapabilityStatus::Available);
}

#[test]
fn test_consumer_load_failure_marks_dependent_capability_unavailable() {
    use kima_ml::{ModelConsumer, VerifiedBytes, evaluate_capabilities_with_loader};

    struct FailingConsumer;
    #[derive(Debug, thiserror::Error)]
    #[error("onnx session commit failed: invalid graph")]
    struct OnnxInitError;

    impl ModelConsumer for FailingConsumer {
        type Output = ();
        type Error = OnnxInitError;

        fn load(&self, _artifact: VerifiedBytes) -> Result<Self::Output, Self::Error> {
            Err(OnnxInitError)
        }
    }

    let dir = tempdir().unwrap();
    let musicnn_desc = create_synthetic_artifact(
        dir.path(),
        "msd-musicnn.bin",
        "musicnn-body",
        b"valid-sha-but-broken-onnx-bytes",
    );
    let clap_desc = create_synthetic_artifact(
        dir.path(),
        "hf_audio.bin",
        "clap-audio",
        b"valid-clap-bytes",
    );

    let mut requirements = BTreeMap::new();
    requirements.insert(Capability::MusicnnAnalysis, vec![musicnn_desc]);
    requirements.insert(Capability::ClapAudioEmbedding, vec![clap_desc]);
    requirements.insert(Capability::Dsp, vec![]);

    let config = CapabilityConfig {
        disable_clap: false,
    };
    // Pass consumer for musicnn, but for clap use a dummy passing loader
    let report =
        evaluate_capabilities_with_loader(dir.path(), &config, &requirements, |cap, verified| {
            if cap == Capability::MusicnnAnalysis {
                FailingConsumer
                    .load(verified)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        });

    match report.status(Capability::MusicnnAnalysis) {
        CapabilityStatus::Unavailable { errors } => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].category, FailureCategory::ConsumerLoadFailed);
            assert!(errors[0].message.contains("onnx session commit failed"));
        }
        other => panic!("expected ConsumerLoadFailed, got {:?}", other),
    }

    assert_eq!(
        report.status(Capability::ClapAudioEmbedding),
        &CapabilityStatus::Available
    );
    assert_eq!(report.status(Capability::Dsp), &CapabilityStatus::Available);
}
