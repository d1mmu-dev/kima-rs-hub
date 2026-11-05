//! Real second mounts (bind mounts). Needs mount rights, so it only runs inside a private
//! user+mount namespace and is ignored otherwise:
//!
//! ```text
//! unshare -Urm cargo test -p kima-ml --test input_snapshot_mounts -- --ignored
//! ```
#![cfg(target_os = "linux")]
mod snapshot_support;
use kima_ml::input_snapshot::{SnapshotClass, SnapshotLimits, SnapshotStore};
use snapshot_support::{private_dir, require_reflink};
use std::ffi::CString;
use std::path::Path;

/// Unmounts on drop (declared after the temp dirs, so it runs before their cleanup).
struct Mounted(std::path::PathBuf);
impl Drop for Mounted {
    fn drop(&mut self) {
        let path = CString::new(self.0.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path.
        unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) };
    }
}

fn bind_mount(from: &Path, onto: &Path) -> Mounted {
    let c = |p: &Path| CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: valid NUL-terminated strings; null fstype/data are allowed for MS_BIND.
    let rc = unsafe {
        libc::mount(
            c(from).as_ptr(),
            c(onto).as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    assert_eq!(
        rc,
        0,
        "bind mount failed ({}); run under `unshare -Urm`",
        std::io::Error::last_os_error()
    );
    Mounted(onto.to_path_buf())
}

#[test]
#[ignore = "needs a private mount namespace: unshare -Urm cargo test ... -- --ignored"]
fn real_bind_mounts_are_refused_for_source_and_snapshot_directory() {
    let (lib, snap, other) = (private_dir(), private_dir(), private_dir());
    if !require_reflink(snap.path()) {
        return;
    }
    std::fs::write(lib.path().join("plain"), b"ok").unwrap();
    std::fs::write(other.path().join("f"), b"foreign").unwrap();
    let store = SnapshotStore::open(snap.path(), SnapshotLimits::default()).unwrap();

    // Control on a single mount.
    assert!(store.check_mount(lib.path()).is_ok());
    assert!(store.create(lib.path(), "plain".as_ref()).is_ok());

    // 1. A directory of ANOTHER mount inside the library: RESOLVE_NO_XDEV refuses to enter it.
    let mnt = lib.path().join("mnt");
    std::fs::create_dir(&mnt).unwrap();
    let _m1 = bind_mount(other.path(), &mnt);
    let e = store.create(lib.path(), "mnt/f".as_ref()).err().unwrap();
    assert_eq!(e.raw_os_error(), Some(libc::EXDEV), "{e}");
    assert_eq!(e.class(), SnapshotClass::Unreadable);
    assert_eq!(store.active(), 0);

    // 2. A snapshot directory that is a bind mount of a directory on the same filesystem:
    //    different mount ID than the library root.
    let bound = lib.path().join("bound-snap");
    std::fs::create_dir(&bound).unwrap();
    let _m2 = bind_mount(snap.path(), &bound);
    let bound_store = SnapshotStore::open(&bound, SnapshotLimits::default()).unwrap();
    let e = bound_store
        .create(lib.path(), "plain".as_ref())
        .err()
        .unwrap();
    assert!(e.to_string().contains("another mount"), "{e}");
    assert_eq!(e.class(), SnapshotClass::Infrastructure);
    let e = bound_store.check_mount(lib.path()).unwrap_err();
    assert!(e.to_string().contains("another mount"), "{e}");
    assert_eq!(bound_store.active(), 0);
}
