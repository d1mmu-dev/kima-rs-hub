use std::collections::BTreeMap;

use kima_ml::onnx::{
    MINIMAL_IDENTITY_ONNX, MusiCnnHead, MusiCnnSessions, OnnxError, OnnxModel, OnnxModelConsumer,
    SharedSession,
};
use kima_ml::{ArtifactId, Capability, ModelConsumer, VerifiedBytes};

#[test]
fn test_onnx_model_inventory_coverage() {
    let all = OnnxModel::all_models();
    assert_eq!(all.len(), 13);

    let heads = MusiCnnHead::all();
    assert_eq!(heads.len(), 10);

    for model in &all {
        let desc = model.descriptor();
        assert!(!desc.basename.is_empty());
        assert_eq!(desc.id, model.artifact_id());
        assert!(desc.max_bytes > 0);
        assert_ne!(desc.expected_sha256, [0u8; 32]);
    }

    let reqs = OnnxModel::requirements_by_capability();
    assert_eq!(reqs.get(&Capability::MusicnnAnalysis).unwrap().len(), 11);
    assert_eq!(reqs.get(&Capability::ClapAudioEmbedding).unwrap().len(), 1);
    assert_eq!(reqs.get(&Capability::ClapTextEmbedding).unwrap().len(), 1);
}

#[test]
fn test_musicnn_sessions_container_requires_core_and_all_10_heads() {
    let consumer = OnnxModelConsumer::new();
    let make_session = |id_str: &'static str| -> SharedSession {
        let bytes = MINIMAL_IDENTITY_ONNX.to_vec().into_boxed_slice();
        let artifact = VerifiedBytes::from_verified_bytes(ArtifactId::from_static(id_str), bytes);
        consumer
            .load(artifact)
            .expect("session load should succeed")
    };

    let core_session = make_session("msd-musicnn-1");
    let mut heads = BTreeMap::new();
    for head in MusiCnnHead::all() {
        heads.insert(*head, make_session(head.basename()));
    }

    // All present -> succeeds
    let musicnn = MusiCnnSessions::new(core_session.clone(), heads.clone())
        .expect("must succeed with all 10 heads and core");
    assert_eq!(musicnn.core().id().as_str(), "msd-musicnn-1");
    for head in MusiCnnHead::all() {
        assert_eq!(musicnn.head(*head).id().as_str(), head.basename());
    }

    // Missing one head (e.g. Danceability) -> fails with MissingHead
    let mut incomplete_heads = heads.clone();
    incomplete_heads.remove(&MusiCnnHead::Danceability);
    let err = MusiCnnSessions::new(core_session.clone(), incomplete_heads)
        .expect_err("must fail when a head is missing");
    match err {
        OnnxError::MissingHead(h) => assert_eq!(h, MusiCnnHead::Danceability),
        other => panic!("expected MissingHead, got {:?}", other),
    }

    // Wrong core id -> fails with MissingCore
    let wrong_core = make_session("wrong-core-id");
    let err =
        MusiCnnSessions::new(wrong_core, heads).expect_err("must fail when core artifact is wrong");
    match err {
        OnnxError::MissingCore => (),
        other => panic!("expected MissingCore, got {:?}", other),
    }
}

#[test]
fn test_model_session_registry() {
    use kima_ml::onnx::ModelSessionRegistry;

    let consumer = OnnxModelConsumer::new();
    let make_session = |id_str: &'static str| -> SharedSession {
        let bytes = MINIMAL_IDENTITY_ONNX.to_vec().into_boxed_slice();
        let artifact = VerifiedBytes::from_verified_bytes(ArtifactId::from_static(id_str), bytes);
        consumer
            .load(artifact)
            .expect("session load should succeed")
    };

    let mut registry = ModelSessionRegistry::new();
    registry.insert(make_session("msd-musicnn-1"));
    for head in MusiCnnHead::all() {
        registry.insert(make_session(head.basename()));
    }
    registry.insert(make_session("hf_audio"));
    registry.insert(make_session("text_model"));

    let musicnn = registry
        .build_musicnn()
        .expect("build_musicnn should succeed");
    assert_eq!(musicnn.core().id().as_str(), "msd-musicnn-1");
    assert!(registry.clap_audio().is_some());
    assert!(registry.clap_text().is_some());
}

#[test]
fn test_capability_evaluation_with_onnx_consumer() {
    use kima_ml::{
        CapabilityConfig, CapabilityStatus, FailureCategory, evaluate_capabilities_with_loader,
    };
    use sha2::{Digest, Sha256};
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();

    // Create synthetic ONNX files on disk matching descriptors
    let identity_bytes = MINIMAL_IDENTITY_ONNX;
    let identity_hash: [u8; 32] = Sha256::digest(identity_bytes).into();

    let core_desc = kima_ml::ArtifactDescriptor {
        id: ArtifactId::from_static("msd-musicnn-1"),
        basename: "msd-musicnn-1.onnx",
        expected_sha256: identity_hash,
        max_bytes: 1024,
    };
    fs::write(dir.path().join("msd-musicnn-1.onnx"), identity_bytes).unwrap();

    let reqs = BTreeMap::from([(Capability::MusicnnAnalysis, vec![core_desc])]);

    let config = CapabilityConfig::default();
    let consumer = OnnxModelConsumer::new();

    // With valid ONNX, consumer loads successfully and capability is Available
    let report = evaluate_capabilities_with_loader(dir.path(), &config, &reqs, |_cap, verified| {
        consumer.load(verified).map(|_| ())
    });
    assert_eq!(
        report.status(Capability::MusicnnAnalysis),
        &CapabilityStatus::Available
    );

    // Now corrupt the ONNX file: valid hash, but invalid ONNX protobuf bytes
    let bad_onnx = b"corrupted-onnx-bytes-protobuf";
    let bad_hash: [u8; 32] = Sha256::digest(bad_onnx).into();
    let bad_desc = kima_ml::ArtifactDescriptor {
        id: ArtifactId::from_static("msd-musicnn-1"),
        basename: "corrupted.onnx",
        expected_sha256: bad_hash,
        max_bytes: 1024,
    };
    fs::write(dir.path().join("corrupted.onnx"), bad_onnx).unwrap();
    let bad_reqs = BTreeMap::from([(Capability::MusicnnAnalysis, vec![bad_desc])]);

    let report_bad =
        evaluate_capabilities_with_loader(dir.path(), &config, &bad_reqs, |_cap, verified| {
            consumer.load(verified).map(|_| ())
        });
    match report_bad.status(Capability::MusicnnAnalysis) {
        CapabilityStatus::Unavailable { errors } => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].category, FailureCategory::ConsumerLoadFailed);
        }
        other => panic!(
            "expected Unavailable with ConsumerLoadFailed, got {:?}",
            other
        ),
    }
}
