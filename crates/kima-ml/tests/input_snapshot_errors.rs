//! Error taxonomy: constant per-file failures are separated from infrastructure ones.
#![cfg(target_os = "linux")]
mod snapshot_support;
use kima_ml::input_snapshot::{SnapshotClass, SnapshotLimits, SnapshotStore};
use snapshot_support::{private_dir, require_reflink};
use std::io::Read;

fn store(snap: &tempfile::TempDir, limits: SnapshotLimits) -> SnapshotStore {
    SnapshotStore::open(snap.path(), limits).unwrap()
}

#[test]
fn constant_failures_are_classified_per_file() {
    let (lib, snap) = (private_dir(), private_dir());
    let s = store(
        &snap,
        SnapshotLimits {
            max_bytes: 4,
            max_concurrent: 2,
        },
    );
    std::fs::write(lib.path().join("big"), b"12345").unwrap();
    std::fs::create_dir(lib.path().join("dir")).unwrap();
    std::os::unix::fs::symlink("big", lib.path().join("link")).unwrap();
    std::fs::write(lib.path().join("file"), b"x").unwrap();
    let cases = [
        ("missing", SnapshotClass::Missing),
        ("dir/nope", SnapshotClass::Missing),
        ("big", SnapshotClass::TooLarge),
        ("dir", SnapshotClass::NotRegular),
        ("link", SnapshotClass::InvalidPath),
        ("file/below", SnapshotClass::InvalidPath),
        ("../x", SnapshotClass::InvalidPath),
        ("/etc/passwd", SnapshotClass::InvalidPath),
        ("", SnapshotClass::InvalidPath),
    ];
    for (rel, want) in cases {
        let e = s.create(lib.path(), rel.as_ref()).err().unwrap();
        assert_eq!(e.class(), want, "{rel:?}: {e}");
    }
    assert!(SnapshotClass::TooLarge.is_permanent());
    assert!(SnapshotClass::NotRegular.is_permanent());
    assert!(SnapshotClass::InvalidPath.is_permanent());
    assert!(!SnapshotClass::Missing.is_permanent());
    assert!(!SnapshotClass::Infrastructure.is_permanent());
}

#[test]
fn infrastructure_failures_stay_infrastructure() {
    let (lib, snap) = (private_dir(), private_dir());
    std::fs::write(lib.path().join("a"), b"x").unwrap();
    let s = store(
        &snap,
        SnapshotLimits {
            max_bytes: 4,
            max_concurrent: 1,
        },
    );
    // Relative or missing library root; `..` in the root.
    for root in ["relative", "/definitely/not/here"] {
        let e = s.create(root.as_ref(), "a".as_ref()).err().unwrap();
        assert_eq!(e.class(), SnapshotClass::Infrastructure, "{root}: {e}");
    }
    let dotdot = lib.path().join("..").join(lib.path().file_name().unwrap());
    let e = s.create(&dotdot, "a".as_ref()).err().unwrap();
    assert_eq!(e.class(), SnapshotClass::Infrastructure);
    // Quota: second live snapshot.
    if require_reflink(snap.path()) {
        let first = s.create(lib.path(), "a".as_ref()).unwrap();
        let e = s.create(lib.path(), "a".as_ref()).err().unwrap();
        assert_eq!(e.class(), SnapshotClass::Infrastructure);
        assert_eq!(e.kind(), std::io::ErrorKind::QuotaExceeded);
        drop(first);
    }
}

#[test]
fn missing_snapshot_directory_fails_at_open_not_as_a_missing_input() {
    let snap = private_dir();
    let gone = snap.path().join("gone");
    let e = SnapshotStore::open(&gone, SnapshotLimits::default())
        .err()
        .unwrap();
    // The store never exists, so no per-file class can be attached; the embedding stage maps
    // every open failure to infrastructure.
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn snapshot_file_holds_its_own_slot_until_the_last_descriptor() {
    let (lib, snap) = (private_dir(), private_dir());
    if !require_reflink(snap.path()) {
        return;
    }
    std::fs::write(lib.path().join("a"), b"data").unwrap();
    let s = store(
        &snap,
        SnapshotLimits {
            max_bytes: 16,
            max_concurrent: 1,
        },
    );
    let snapshot = s.create(lib.path(), "a".as_ref()).unwrap();
    let mut reopened = snapshot.open().unwrap();
    drop(snapshot);
    assert_eq!(s.active(), 1, "reopened file still holds the slot");
    assert_eq!(
        s.create(lib.path(), "a".as_ref()).err().unwrap().kind(),
        std::io::ErrorKind::QuotaExceeded
    );
    let mut bytes = Vec::new();
    reopened.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"data");
    drop(reopened);
    assert_eq!(s.active(), 0);
    assert!(s.create(lib.path(), "a".as_ref()).is_ok());
}

#[test]
fn dotdot_is_rejected_in_snapshot_dir_and_checked_root() {
    let (lib, snap) = (private_dir(), private_dir());
    let name = snap.path().file_name().unwrap().to_owned();
    let with_dotdot = snap.path().join("..").join(&name);
    assert!(SnapshotStore::open(&with_dotdot, SnapshotLimits::default()).is_err());
    let s = store(&snap, SnapshotLimits::default());
    assert!(s.check_mount(lib.path()).is_ok());
    let root_dotdot = lib.path().join("..").join(lib.path().file_name().unwrap());
    assert!(s.check_mount(&root_dotdot).is_err());
    assert!(s.check_mount("relative".as_ref()).is_err());
    assert!(s.check_mount("/definitely/not/here".as_ref()).is_err());
}

#[test]
fn unreadable_source_is_a_per_file_class_not_infrastructure() {
    use std::os::unix::fs::PermissionsExt;
    // Root bypasses mode bits, so the file would be readable.
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let (lib, snap) = (private_dir(), private_dir());
    if !require_reflink(snap.path()) {
        return;
    }
    let path = lib.path().join("secret");
    std::fs::write(&path, b"x").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let s = store(&snap, SnapshotLimits::default());
    let e = s.create(lib.path(), "secret".as_ref()).err().unwrap();
    assert_eq!(e.class(), SnapshotClass::Unreadable, "{e}");
    assert_eq!(e.raw_os_error(), Some(libc::EACCES));
    assert_eq!(s.active(), 0);
}
