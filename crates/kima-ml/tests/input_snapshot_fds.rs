//! Own test binary with exactly one test, so no other thread opens descriptors while the
//! process-wide descriptor count is compared.
#![cfg(target_os = "linux")]
use kima_ml::input_snapshot::{SnapshotLimits, SnapshotStore};
use std::os::unix::fs::PermissionsExt;

mod snapshot_support;

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

#[test]
fn success_failure_and_drop_close_every_descriptor() {
    let lib = tempfile::tempdir_in(kima_test_support::reflink_test_dir(env!(
        "CARGO_MANIFEST_DIR"
    )))
    .unwrap();
    let snap = tempfile::tempdir_in(kima_test_support::reflink_test_dir(env!(
        "CARGO_MANIFEST_DIR"
    )))
    .unwrap();
    std::fs::set_permissions(snap.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    if !snapshot_support::require_reflink(snap.path()) {
        return;
    }
    std::fs::write(lib.path().join("ok"), b"1234").unwrap();
    std::fs::write(lib.path().join("big"), vec![0u8; 64]).unwrap();
    std::os::unix::fs::symlink("ok", lib.path().join("link")).unwrap();
    let limits = SnapshotLimits {
        max_bytes: 8,
        max_concurrent: 1,
    };
    let store = SnapshotStore::open(snap.path(), limits).unwrap();
    // Warm up lazily-created descriptors (none expected), then measure.
    drop(store.create(lib.path(), "ok".as_ref()).unwrap());
    let baseline = open_fds();
    for _ in 0..20 {
        let s = store.create(lib.path(), "ok".as_ref()).unwrap();
        // The slot is taken while it lives: a second attempt fails and leaks nothing.
        assert!(store.create(lib.path(), "ok".as_ref()).is_err());
        let reopened = s.open().unwrap();
        drop(reopened);
        drop(s);
        for bad in ["big", "link", "missing", "../ok"] {
            assert!(store.create(lib.path(), bad.as_ref()).is_err());
        }
    }
    assert_eq!(open_fds(), baseline);
    assert_eq!(store.active(), 0);
}
