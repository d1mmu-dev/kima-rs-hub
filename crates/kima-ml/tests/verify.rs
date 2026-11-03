use sha2::{Digest, Sha256};
use std::fs;
use tempfile::tempdir;

use kima_ml::{ArtifactDescriptor, ArtifactError, ArtifactId, verify_artifact};

#[test]
fn test_verify_valid_file_returns_verified_bytes() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("model.bin");
    let synthetic_bytes = b"synthetic-model-payload-bytes";
    fs::write(&file_path, synthetic_bytes).unwrap();

    let expected_sha256: [u8; 32] = Sha256::digest(synthetic_bytes).into();
    let descriptor = ArtifactDescriptor {
        id: ArtifactId::from_static("test-model"),
        basename: "model.bin",
        expected_sha256,
        max_bytes: 1024,
    };

    let result = verify_artifact(dir.path(), &descriptor);
    let verified = result.expect("valid artifact verification should succeed");
    assert_eq!(verified.id(), descriptor.id);
    assert_eq!(verified.as_bytes(), synthetic_bytes);
}

#[test]
fn test_verify_missing_file_returns_missing_error() {
    let dir = tempdir().unwrap();
    let descriptor = ArtifactDescriptor {
        id: ArtifactId::from_static("test-model"),
        basename: "absent.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 1024,
    };

    let err = verify_artifact(dir.path(), &descriptor).expect_err("absent file should fail");
    assert_eq!(err, ArtifactError::Missing { id: descriptor.id });
}

#[test]
fn test_verify_hash_mismatch_returns_hash_mismatch_error() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("corrupted.bin");
    fs::write(&file_path, b"corrupted-data").unwrap();

    let expected_sha256 = [0xabu8; 32];
    let descriptor = ArtifactDescriptor {
        id: ArtifactId::from_static("test-model"),
        basename: "corrupted.bin",
        expected_sha256,
        max_bytes: 1024,
    };

    let err = verify_artifact(dir.path(), &descriptor).expect_err("hash mismatch should fail");
    match err {
        ArtifactError::HashMismatch {
            id,
            expected_hex,
            actual_hex,
        } => {
            assert_eq!(id, descriptor.id);
            assert_eq!(
                expected_hex,
                "abababababababababababababababababababababababababababababababab"
            );
            let actual_expected = Sha256::digest(b"corrupted-data");
            let actual_hex_str: String = actual_expected
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect();
            assert_eq!(actual_hex, actual_hex_str);
        }
        other => panic!("expected HashMismatch, got {:?}", other),
    }
}

#[test]
#[cfg(unix)]
fn test_verify_symlink_is_refused_without_following() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let real_file = dir.path().join("real.bin");
    let synthetic_bytes = b"real-target-bytes";
    fs::write(&real_file, synthetic_bytes).unwrap();

    let link_path = dir.path().join("link.bin");
    symlink(&real_file, &link_path).unwrap();

    let expected_sha256: [u8; 32] = Sha256::digest(synthetic_bytes).into();
    let descriptor = ArtifactDescriptor {
        id: ArtifactId::from_static("test-model"),
        basename: "link.bin",
        expected_sha256,
        max_bytes: 1024,
    };

    let err = verify_artifact(dir.path(), &descriptor).expect_err("symlink must be refused");
    assert_eq!(err, ArtifactError::SymlinkRefused { id: descriptor.id });
}

#[test]
#[cfg(unix)]
fn test_verify_fifo_is_rejected_nonblocking() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempdir().unwrap();
    let fifo_path = dir.path().join("fifo.bin");
    let c_path = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();

    let res = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(res, 0, "mkfifo failed");

    let descriptor = ArtifactDescriptor {
        id: ArtifactId::from_static("test-model"),
        basename: "fifo.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 1024,
    };

    // Note: opening a FIFO in blocking read mode with no writer would hang forever.
    // verify_artifact uses O_NONBLOCK + handle inspection and must return immediately with NotRegular.
    let err = verify_artifact(dir.path(), &descriptor).expect_err("fifo must not succeed");
    assert_eq!(err, ArtifactError::NotRegular { id: descriptor.id });
}

#[test]
fn test_verify_oversized_file_is_rejected() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("huge.bin");
    let synthetic_bytes = vec![0x42u8; 100];
    fs::write(&file_path, &synthetic_bytes).unwrap();

    let descriptor = ArtifactDescriptor {
        id: ArtifactId::from_static("test-model"),
        basename: "huge.bin",
        expected_sha256: [0u8; 32],
        max_bytes: 50, // max 50 bytes, file is 100 bytes
    };

    let err = verify_artifact(dir.path(), &descriptor).expect_err("oversized file must fail");
    assert_eq!(
        err,
        ArtifactError::TooLarge {
            id: descriptor.id,
            max_bytes: 50,
        }
    );
}

#[test]
fn test_verify_unsafe_basename_is_rejected() {
    let dir = tempdir().unwrap();
    for unsafe_name in &[
        "../evil.bin",
        "sub/model.bin",
        ".",
        "..",
        "bad\0name",
        "unicode_ümlaut.bin",
    ] {
        let descriptor = ArtifactDescriptor {
            id: ArtifactId::from_static("test-model"),
            basename: unsafe_name,
            expected_sha256: [0u8; 32],
            max_bytes: 1024,
        };

        let err = verify_artifact(dir.path(), &descriptor).expect_err("unsafe basename must fail");
        assert!(
            matches!(err, ArtifactError::UnsafeBasename { .. }),
            "expected UnsafeBasename for {:?}, got {:?}",
            unsafe_name,
            err
        );
    }
}
