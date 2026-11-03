use kima_ml::{ArtifactDescriptor, ArtifactId, VerifiedBytes, verify_artifact};
use sha2::{Digest, Sha256};

/// Synthetic fixture descriptor, deliberately distinct from the production manifest.
pub fn verified_fixture(id: ArtifactId, bytes: Box<[u8]>) -> VerifiedBytes {
    let dir = tempfile::tempdir().unwrap();
    let descriptor = ArtifactDescriptor {
        id,
        basename: "synthetic.onnx",
        expected_sha256: Sha256::digest(&bytes).into(),
        max_bytes: bytes.len() as u64,
    };
    std::fs::write(dir.path().join(descriptor.basename), &bytes).unwrap();
    verify_artifact(dir.path(), &descriptor).unwrap()
}
