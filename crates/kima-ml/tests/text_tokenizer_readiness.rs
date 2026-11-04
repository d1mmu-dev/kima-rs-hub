use kima_ml::{
    ArtifactDescriptor, ArtifactId, Capability, CapabilityConfig, CapabilityStatus,
    evaluate_capabilities,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[test]
fn text_model_alone_does_not_make_tokenizer_capability_ready() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"synthetic model";
    std::fs::write(dir.path().join("text.onnx"), bytes).unwrap();
    let desc = ArtifactDescriptor {
        id: ArtifactId::from_static("text"),
        basename: "text.onnx",
        expected_sha256: Sha256::digest(bytes).into(),
        max_bytes: 1024,
    };
    let report = evaluate_capabilities(
        dir.path(),
        &CapabilityConfig::default(),
        &BTreeMap::from([(Capability::ClapTextEmbedding, vec![desc])]),
    );
    assert!(
        matches!(report.status(Capability::ClapTextEmbedding), CapabilityStatus::Disabled { reason } if reason.contains("tokenizer"))
    );
}
