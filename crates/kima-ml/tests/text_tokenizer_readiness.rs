use kima_ml::{
    ArtifactDescriptor, ArtifactId, Capability, CapabilityConfig, CapabilityStatus,
    evaluate_capabilities,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// `ClapTextEmbedding`'s real readiness contract lives in
/// `OnnxModel::requirements_by_capability`, which now lists both
/// `text_tokenizer::descriptor()` (tokenizer.json) and
/// `OnnxModel::ClapText.descriptor()` (text_model.onnx). This generic test
/// only re-confirms that `evaluate_capabilities` requires *every*
/// descriptor registered for a capability, which is what makes that
/// two-artifact readiness gate work.
#[test]
fn capability_is_not_ready_until_every_registered_descriptor_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let tokenizer_bytes = b"synthetic tokenizer bytes";
    let text_model_bytes = b"synthetic text model bytes";
    std::fs::write(dir.path().join("tokenizer.json"), tokenizer_bytes).unwrap();
    // text_model.onnx is deliberately NOT written to disk: missing.
    let tokenizer_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("tokenizer"),
        basename: "tokenizer.json",
        expected_sha256: Sha256::digest(tokenizer_bytes).into(),
        max_bytes: 1024,
    };
    let text_model_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("text_model"),
        basename: "text_model.onnx",
        expected_sha256: Sha256::digest(text_model_bytes).into(),
        max_bytes: 1024,
    };
    let report = evaluate_capabilities(
        dir.path(),
        &CapabilityConfig::default(),
        &BTreeMap::from([(
            Capability::ClapTextEmbedding,
            vec![tokenizer_desc.clone(), text_model_desc],
        )]),
    );
    assert!(matches!(
        report.status(Capability::ClapTextEmbedding),
        CapabilityStatus::Unavailable { errors } if errors.len() == 1
    ));

    // Once both artifacts verify, the capability becomes Available.
    std::fs::write(
        dir.path().join("text_model.onnx"),
        b"synthetic text model bytes",
    )
    .unwrap();
    let text_model_desc = ArtifactDescriptor {
        id: ArtifactId::from_static("text_model"),
        basename: "text_model.onnx",
        expected_sha256: Sha256::digest(text_model_bytes).into(),
        max_bytes: 1024,
    };
    let report = evaluate_capabilities(
        dir.path(),
        &CapabilityConfig::default(),
        &BTreeMap::from([(
            Capability::ClapTextEmbedding,
            vec![tokenizer_desc, text_model_desc],
        )]),
    );
    assert_eq!(
        report.status(Capability::ClapTextEmbedding),
        &CapabilityStatus::Available
    );
}
