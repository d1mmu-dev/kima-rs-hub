use sha2::{Digest, Sha256};
use std::fs;
use tempfile::tempdir;

use kima_ml::{ArtifactDescriptor, ArtifactId, ModelConsumer, verify_artifact};

struct FakeConsumer;

#[derive(Debug, PartialEq, Eq)]
struct FakeLoadedModel {
    id: ArtifactId,
    bytes: Box<[u8]>,
}

#[derive(Debug, thiserror::Error)]
#[error("fake consumer error")]
struct FakeConsumerError;

impl ModelConsumer for FakeConsumer {
    type Output = FakeLoadedModel;
    type Error = FakeConsumerError;

    fn load(&self, artifact: kima_ml::VerifiedBytes) -> Result<Self::Output, Self::Error> {
        Ok(FakeLoadedModel {
            id: artifact.id(),
            bytes: artifact.into_bytes(),
        })
    }
}

#[test]
fn test_consumer_receives_exact_verified_bytes_isolated_from_disk_tampering() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("model.bin");
    let original_bytes = b"original-synthetic-model-weights";
    fs::write(&file_path, original_bytes).unwrap();

    let expected_sha256: [u8; 32] = Sha256::digest(original_bytes).into();
    let descriptor = ArtifactDescriptor {
        id: ArtifactId::from_static("test-model"),
        basename: "model.bin",
        expected_sha256,
        max_bytes: 1024,
    };

    let verified =
        verify_artifact(dir.path(), &descriptor).expect("initial verification must succeed");

    // Adversary overwrites or deletes the model file on disk after verification
    fs::write(&file_path, b"adversarial-tampered-weights-payload").unwrap();

    let consumer = FakeConsumer;
    let loaded = consumer
        .load(verified)
        .expect("consumer loading must succeed");

    // The consumer receives the exact original verified bytes, proving disk mutation cannot affect consumer
    assert_eq!(loaded.id, descriptor.id);
    assert_eq!(&*loaded.bytes, original_bytes);
}
