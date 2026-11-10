//! Reflink/private-dir helpers for the snapshot integration tests.
#![allow(dead_code)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir_in(kima_test_support::reflink_test_dir(env!(
        "CARGO_MANIFEST_DIR"
    )))
    .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Positive snapshot tests need a reflink-capable filesystem. When there is none this
/// panics with an explicit message, unless `KIMA_TEST_ALLOW_NO_REFLINK=1`, in which case the
/// test prints `SKIPPED` to stderr and returns `false` (the caller must return early).
pub fn require_reflink(dir: &Path) -> bool {
    match kima_ml::input_snapshot::reflink_supported_in(dir) {
        Ok(true) => true,
        other => {
            let msg = format!("NO REFLINK SUPPORT in {} ({other:?})", dir.display());
            if std::env::var("KIMA_TEST_ALLOW_NO_REFLINK").as_deref() == Ok("1") {
                eprintln!("SKIPPED: {msg}");
                false
            } else {
                panic!("{msg}; run on btrfs/XFS-reflink or set KIMA_TEST_ALLOW_NO_REFLINK=1");
            }
        }
    }
}
