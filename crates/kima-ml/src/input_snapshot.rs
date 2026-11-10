//! Private, unnamed Linux reflink snapshots of encoded input. No copying fallback.
//!
//! Design (see `docs/m4-input-snapshots.md`):
//! * the library root and the private snapshot directory are each opened once as a directory
//!   descriptor (no symlink in any component) and every later step is relative to those fds;
//! * the source is resolved by ONE `openat2(root_fd, full_relative_path, O_PATH,
//!   RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV)`, so there is no separately
//!   opened parent that could be moved away between steps;
//! * the unnamed `O_TMPFILE` lives in the private snapshot directory, not in the source's
//!   parent, and is cloned with `FICLONE`;
//! * mount IDs of root, source, snapshot directory and snapshot are compared on the open
//!   descriptors (`statx(AT_EMPTY_PATH)`); missing mount-ID data is an error, and so is any
//!   `statx` field that was requested but not reported (`stx_mask`);
//! * every failure is final: there is no copy fallback;
//! * failures are classified ([`SnapshotClass`]): properties of the input file (missing,
//!   too large, not regular, bad path, unreadable: `EACCES`/`EPERM`/source on another mount)
//!   are told apart from infrastructure problems
//!   (quota, no directory, wrong mount, no reflink, read-only, full disk, ...).
//!
//! Trust assumptions: `/proc` is the real procfs of this process's own mount namespace and
//! nobody can remount over `/proc/self/fd` (the reopen of an `O_PATH` fd and of the
//! snapshot goes through it). The snapshot directory must be owned by the effective user and
//! have no group/other access bits; ACLs and other principals running as the same UID or as
//! root are not checked and are not isolated.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// Safe defaults; the server makes both configurable.
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_CONCURRENT: usize = 2;
const CLONE_ATTEMPTS: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLimits {
    /// Largest source file (bytes) that is snapshotted.
    pub max_bytes: u64,
    /// Snapshots alive at once in this store (held until the last descriptor closes).
    pub max_concurrent: usize,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }
}

/// What a failed snapshot says about the input, as opposed to the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotClass {
    /// The path does not exist (`ENOENT`) beneath the library root.
    Missing,
    /// The file is larger than the limit (now, or after cloning).
    TooLarge,
    /// The path names something that is not a regular file.
    NotRegular,
    /// The relative path is malformed, or contains a symlink / a non-directory component.
    InvalidPath,
    /// This file cannot be read by the server: `EACCES`/`EPERM` opening it, or it (or a parent
    /// directory) lives on another mount than the library root (`EXDEV`, source mount ID
    /// mismatch). A property of this one file, not of the setup: the stage retries it with a
    /// bounded attempt budget instead of re-queuing it forever, and other files are unaffected.
    Unreadable,
    /// Everything else: quota, concurrency limit, missing or foreign snapshot directory,
    /// wrong mount, no reflink support, read-only or full filesystem, `EAGAIN`, procfs trouble.
    /// Retrying later can help; the input itself is not at fault.
    Infrastructure,
}

impl SnapshotClass {
    /// A constant property of the input file: retrying cannot change it.
    pub fn is_permanent(self) -> bool {
        matches!(self, Self::TooLarge | Self::NotRegular | Self::InvalidPath)
    }
}

/// A classified snapshot failure. `From<io::Error>` yields [`SnapshotClass::Infrastructure`]
/// (the safe default: no attempt is consumed); constant file failures are classified
/// explicitly at the place where they are detected.
#[derive(Debug)]
pub struct SnapshotError {
    class: SnapshotClass,
    source: io::Error,
}

impl SnapshotError {
    pub fn new(class: SnapshotClass, source: io::Error) -> Self {
        Self { class, source }
    }
    pub fn infrastructure(source: io::Error) -> Self {
        Self::new(SnapshotClass::Infrastructure, source)
    }
    pub fn class(&self) -> SnapshotClass {
        self.class
    }
    pub fn io(&self) -> &io::Error {
        &self.source
    }
    pub fn kind(&self) -> io::ErrorKind {
        self.source.kind()
    }
    pub fn raw_os_error(&self) -> Option<i32> {
        self.source.raw_os_error()
    }
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<io::Error> for SnapshotError {
    fn from(source: io::Error) -> Self {
        Self::infrastructure(source)
    }
}

/// Owns one concurrency slot; the slot is released when the last holder drops.
struct SlotInner(Arc<AtomicUsize>);
impl Drop for SlotInner {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Takes a slot with a compare-and-swap loop, so the counter never exceeds `max` (not even
/// transiently).
///
/// `fetch_update` is deprecated in favor of `try_update` on newer toolchains, but `try_update`
/// postdates this crate's pinned MSRV (`rust-version = "1.85"` at the workspace root); keep
/// `fetch_update` and silence the lint rather than raise the MSRV for a rename-only change.
#[allow(deprecated)]
fn reserve(active: &Arc<AtomicUsize>, max: usize) -> Option<Arc<SlotInner>> {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < max).then_some(n + 1)
        })
        .ok()
        .map(|_| Arc::new(SlotInner(active.clone())))
}

pub struct InputSnapshot {
    file: File,
    slot: Arc<SlotInner>,
}

/// A readable, seekable descriptor onto a snapshot with its own offset. It keeps the
/// snapshot's concurrency slot until it is dropped, so the slot is held until the LAST
/// descriptor of the unnamed inode closes, even if the [`InputSnapshot`] is gone.
pub struct SnapshotFile {
    file: File,
    _slot: Arc<SlotInner>,
}

impl SnapshotFile {
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.file.metadata()
    }
}

impl Read for SnapshotFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Seek for SnapshotFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
}

impl symphonia::core::io::MediaSource for SnapshotFile {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.file.metadata().ok().map(|m| m.len())
    }
}

/// Handle to the private snapshot directory, pinned by descriptor when opened.
pub struct SnapshotStore {
    #[cfg(target_os = "linux")]
    dir: OwnedFd,
    limits: SnapshotLimits,
    active: Arc<AtomicUsize>,
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn has_parent_dir(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

/// A mount identifier together with its kind; only equal kinds are comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountId {
    Unique(u64),
    Legacy(u64),
}

fn require_same(a: MountId, b: MountId, msg: &'static str) -> io::Result<()> {
    match (a, b) {
        (MountId::Unique(x), MountId::Unique(y)) | (MountId::Legacy(x), MountId::Legacy(y)) => {
            if x == y {
                Ok(())
            } else {
                Err(io::Error::other(msg))
            }
        }
        _ => Err(io::Error::other("mount ID kinds differ; not comparable")),
    }
}

/// Every requested `statx` field must have been reported; absence is an error, never a
/// silently-zero value.
fn require_fields(got: u32, wanted: u32) -> io::Result<()> {
    if got & wanted == wanted {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("statx did not report requested fields (asked {wanted:#x}, got {got:#x})"),
        ))
    }
}

/// Retries a raw system call interrupted by a signal (`EINTR`).
fn retry_eintr<T>(mut call: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match call() {
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => {}
            other => return other,
        }
    }
}

/// How a failed source resolution is classified.
fn classify_resolve(e: io::Error) -> SnapshotError {
    let class = match e.raw_os_error() {
        Some(libc::ENOENT) => SnapshotClass::Missing,
        // Symlink in the path (RESOLVE_NO_SYMLINKS), a file used as a directory, absurd names.
        Some(libc::ELOOP | libc::ENOTDIR | libc::ENAMETOOLONG) => SnapshotClass::InvalidPath,
        // No search permission on a parent, or a nested mount inside the library
        // (`RESOLVE_NO_XDEV`): both belong to this file's location.
        Some(libc::EACCES | libc::EPERM | libc::EXDEV) => SnapshotClass::Unreadable,
        _ => SnapshotClass::Infrastructure,
    };
    SnapshotError::new(class, e)
}

/// How a failed read-reopen of the SOURCE is classified: permission errors are per-file.
fn classify_reopen(e: io::Error) -> SnapshotError {
    let class = match e.raw_os_error() {
        Some(libc::EACCES | libc::EPERM) => SnapshotClass::Unreadable,
        _ => SnapshotClass::Infrastructure,
    };
    SnapshotError::new(class, e)
}

/// A mismatch between the source's mount and the library root's is a property of the file.
fn source_mount(root: MountId, source: MountId, msg: &'static str) -> Result<(), SnapshotError> {
    require_same(root, source, msg).map_err(|e| SnapshotError::new(SnapshotClass::Unreadable, e))
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "input snapshots require Linux")
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const RESOLVE_NO_XDEV: u64 = 0x01;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    const RESOLVE_BENEATH: u64 = 0x08;
    /// `FICLONE` = `_IOW(0x94, 9, int)`. The libc `ioctl` request parameter is `c_ulong` on
    /// glibc but `c_int` on musl; the value fits both, so it is cast with `as _` at the call.
    const FICLONE: u32 = 0x4004_9409;
    const STATX_MNT_ID: u32 = 0x0000_1000;
    const STATX_MNT_ID_UNIQUE: u32 = 0x0000_4000;
    const SOURCE_RESOLVE: u64 = RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV;

    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    fn openat2(dir: RawFd, path: &Path, flags: i32, resolve: u64) -> io::Result<OwnedFd> {
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("NUL in path"))?;
        let how = OpenHow {
            flags: flags as u64,
            mode: 0,
            resolve,
        };
        let fd = retry_eintr(|| {
            // SAFETY: pointers are valid for the duration of the syscall.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_openat2,
                    dir,
                    name.as_ptr(),
                    &how,
                    std::mem::size_of::<OpenHow>(),
                )
            };
            if fd < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(fd)
            }
        })?;
        // SAFETY: a successful openat2 returned a fresh descriptor we own.
        Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
    }

    fn open_dir_pinned(path: &Path) -> io::Result<OwnedFd> {
        // NO_SYMLINKS on an absolute path covers every component of it.
        openat2(
            libc::AT_FDCWD,
            path,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            RESOLVE_NO_SYMLINKS,
        )
    }

    fn statx_raw(fd: RawFd, mask: u32) -> io::Result<libc::statx> {
        // SAFETY: zeroed statx is a valid out-buffer; fd is open for the call.
        let mut st: libc::statx = unsafe { std::mem::zeroed() };
        retry_eintr(|| {
            let rc = unsafe {
                libc::statx(
                    fd,
                    c"".as_ptr(),
                    libc::AT_EMPTY_PATH | libc::AT_STATX_SYNC_AS_STAT,
                    mask,
                    &mut st,
                )
            };
            if rc < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })?;
        Ok(st)
    }

    /// `statx` that fails unless every requested field was reported.
    fn statx(fd: RawFd, mask: u32) -> io::Result<libc::statx> {
        let st = statx_raw(fd, mask)?;
        require_fields(st.stx_mask, mask)?;
        Ok(st)
    }

    /// (device, inode, is-regular) of an open descriptor.
    fn identity(fd: RawFd) -> io::Result<((u32, u32), u64, bool)> {
        let st = statx(fd, libc::STATX_TYPE | libc::STATX_INO)?;
        Ok((
            (st.stx_dev_major, st.stx_dev_minor),
            st.stx_ino,
            u32::from(st.stx_mode) & libc::S_IFMT == libc::S_IFREG,
        ))
    }

    pub fn mount_id(fd: RawFd) -> io::Result<MountId> {
        // The unique ID is preferred; older kernels ignore its bit and report the legacy one,
        // so "either" is the requirement here rather than the full mask.
        let st = statx_raw(fd, STATX_MNT_ID_UNIQUE | STATX_MNT_ID)?;
        if st.stx_mask & STATX_MNT_ID_UNIQUE != 0 {
            Ok(MountId::Unique(st.stx_mnt_id))
        } else if st.stx_mask & STATX_MNT_ID != 0 {
            Ok(MountId::Legacy(st.stx_mnt_id))
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "statx did not report a mount ID",
            ))
        }
    }

    pub fn ficlone(dst: RawFd, src: RawFd) -> io::Result<()> {
        retry_eintr(|| {
            // SAFETY: both descriptors are open; FICLONE takes the source fd by value.
            if unsafe { libc::ioctl(dst, FICLONE as _, src) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
    }

    fn open_tmpfile(dir: RawFd) -> io::Result<File> {
        let fd = retry_eintr(|| {
            // SAFETY: valid dirfd and NUL-terminated path.
            let fd = unsafe {
                libc::openat(
                    dir,
                    c".".as_ptr(),
                    libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(fd)
            }
        })?;
        // SAFETY: fresh descriptor we own.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Reopen an open descriptor for reading through `/proc/self/fd` (trusted procfs).
    /// (std's `open` already retries `EINTR`.)
    pub fn reopen_read(fd: RawFd, extra_flags: i32) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | extra_flags)
            .open(format!("/proc/self/fd/{fd}"))
    }

    /// Diagnostic probe: does `dir`'s filesystem support `O_TMPFILE` plus `FICLONE` within
    /// itself? `Ok(false)` means cloning is not supported there (`EOPNOTSUPP`, `EINVAL`,
    /// `ENOTTY`, `EXDEV`).
    pub fn reflink_supported_in(dir: &Path) -> io::Result<bool> {
        use std::io::Write;
        let fd = open_dir_pinned(dir)?;
        let mut src = open_tmpfile(fd.as_raw_fd())?;
        src.write_all(&[0u8; 8192])?;
        src.sync_all()?;
        let dst = open_tmpfile(fd.as_raw_fd())?;
        match ficlone(dst.as_raw_fd(), src.as_raw_fd()) {
            Ok(()) => Ok(true),
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EOPNOTSUPP | libc::EINVAL | libc::ENOTTY | libc::EXDEV)
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Step {
        RootOpened,
        SnapshotDirChecked,
        SourceResolved,
        SnapshotCreated,
    }

    /// Test seams exist only in test builds; production uses the zero-sized real variant.
    #[cfg(test)]
    #[derive(Clone, Copy)]
    pub struct Seams<'a> {
        pub mount_id: fn(RawFd) -> io::Result<MountId>,
        pub clone: fn(RawFd, RawFd) -> io::Result<()>,
        pub hook: &'a dyn Fn(Step),
    }

    #[cfg(test)]
    impl Seams<'static> {
        pub fn real() -> Self {
            Seams {
                mount_id,
                clone: ficlone,
                hook: &|_| {},
            }
        }
    }

    #[cfg(test)]
    impl Seams<'_> {
        fn mount_id_of(&self, fd: RawFd) -> io::Result<MountId> {
            (self.mount_id)(fd)
        }
        fn clone_fd(&self, dst: RawFd, src: RawFd) -> io::Result<()> {
            (self.clone)(dst, src)
        }
        fn step(&self, step: Step) {
            (self.hook)(step)
        }
    }

    #[cfg(not(test))]
    #[derive(Clone, Copy)]
    pub struct Seams<'a>(std::marker::PhantomData<&'a ()>);

    #[cfg(not(test))]
    impl Seams<'static> {
        pub fn real() -> Self {
            Seams(std::marker::PhantomData)
        }
    }

    #[cfg(not(test))]
    impl Seams<'_> {
        fn mount_id_of(&self, fd: RawFd) -> io::Result<MountId> {
            mount_id(fd)
        }
        fn clone_fd(&self, dst: RawFd, src: RawFd) -> io::Result<()> {
            ficlone(dst, src)
        }
        fn step(&self, _step: Step) {}
    }

    fn clone_with_retry(seams: &Seams<'_>, dst: RawFd, src: RawFd) -> io::Result<()> {
        let mut attempt = 0;
        loop {
            match seams.clone_fd(dst, src) {
                Err(e)
                    if e.raw_os_error() == Some(libc::EAGAIN) && attempt + 1 < CLONE_ATTEMPTS =>
                {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                other => return other,
            }
        }
    }

    impl SnapshotStore {
        /// Pin the private snapshot directory (absolute, no `..`, no symlinks, owned by the
        /// effective user, no group/other access).
        pub fn open(dir: &Path, limits: SnapshotLimits) -> io::Result<Self> {
            use std::os::unix::fs::MetadataExt;
            if !dir.is_absolute() {
                return Err(invalid("snapshot directory must be absolute"));
            }
            if has_parent_dir(dir) {
                return Err(invalid("snapshot directory must not contain '..'"));
            }
            if limits.max_bytes == 0 || limits.max_concurrent == 0 {
                return Err(invalid("snapshot limits must be positive"));
            }
            let fd = open_dir_pinned(dir)?;
            let meta = File::from(fd.try_clone()?).metadata()?;
            // SAFETY: geteuid has no preconditions.
            if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "snapshot directory must be owned by the server user with no group/other access",
                ));
            }
            Ok(Self {
                dir: fd,
                limits,
                active: Arc::new(AtomicUsize::new(0)),
            })
        }

        /// Fail-fast startup check: the library root (absolute, no `..`, no symlinks) is on
        /// the same mount as the snapshot directory. `create` repeats the check on every call.
        pub fn check_mount(&self, root: &Path) -> io::Result<()> {
            self.check_mount_with(root, Seams::real())
        }

        pub(super) fn check_mount_with(&self, root: &Path, seams: Seams<'_>) -> io::Result<()> {
            if !root.is_absolute() || has_parent_dir(root) {
                return Err(invalid("library root must be absolute without '..'"));
            }
            let root_fd = open_dir_pinned(root)?;
            require_same(
                seams.mount_id_of(root_fd.as_raw_fd())?,
                seams.mount_id_of(self.dir.as_raw_fd())?,
                "snapshot directory is on another mount than the library root",
            )
        }

        /// Snapshot `relative` (normal components only) beneath `root` into the private
        /// directory. Fails closed; never copies.
        pub fn create(&self, root: &Path, relative: &Path) -> Result<InputSnapshot, SnapshotError> {
            self.create_with(root, relative, Seams::real())
        }

        pub(super) fn create_with(
            &self,
            root: &Path,
            relative: &Path,
            seams: Seams<'_>,
        ) -> Result<InputSnapshot, SnapshotError> {
            if !root.is_absolute() || has_parent_dir(root) {
                return Err(invalid("expected absolute root without '..'").into());
            }
            if relative.as_os_str().is_empty()
                || !relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
            {
                return Err(SnapshotError::new(
                    SnapshotClass::InvalidPath,
                    invalid("expected a normal relative path"),
                ));
            }
            // Reserve a slot first; it is released on every exit path.
            let slot = reserve(&self.active, self.limits.max_concurrent).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::QuotaExceeded,
                    "too many concurrent input snapshots",
                )
            })?;

            let root_fd = open_dir_pinned(root)?;
            seams.step(Step::RootOpened);
            let root_mount = seams.mount_id_of(root_fd.as_raw_fd())?;
            require_same(
                root_mount,
                seams.mount_id_of(self.dir.as_raw_fd())?,
                "snapshot directory is on another mount",
            )?;
            seams.step(Step::SnapshotDirChecked);

            // One resolution of the whole path: no parent opened separately to swap.
            let path_fd = openat2(
                root_fd.as_raw_fd(),
                relative,
                libc::O_PATH | libc::O_CLOEXEC,
                SOURCE_RESOLVE,
            )
            .map_err(classify_resolve)?;
            seams.step(Step::SourceResolved);
            let st = statx(path_fd.as_raw_fd(), libc::STATX_TYPE | libc::STATX_SIZE)?;
            if u32::from(st.stx_mode) & libc::S_IFMT != libc::S_IFREG {
                return Err(SnapshotError::new(
                    SnapshotClass::NotRegular,
                    invalid("input is not a regular file"),
                ));
            }
            if st.stx_size > self.limits.max_bytes {
                return Err(SnapshotError::new(
                    SnapshotClass::TooLarge,
                    io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        "input exceeds the snapshot size limit",
                    ),
                ));
            }
            source_mount(
                root_mount,
                seams.mount_id_of(path_fd.as_raw_fd())?,
                "source is on another mount",
            )?;

            // Readable descriptor for FICLONE; O_NONBLOCK keeps a raced FIFO from blocking.
            let source =
                reopen_read(path_fd.as_raw_fd(), libc::O_NONBLOCK).map_err(classify_reopen)?;
            let (before, after) = (
                identity(path_fd.as_raw_fd())?,
                identity(source.as_raw_fd())?,
            );
            if before != after || !after.2 {
                return Err(io::Error::other("source reopen resolved to a different inode").into());
            }
            source_mount(
                root_mount,
                seams.mount_id_of(source.as_raw_fd())?,
                "reopened source is on another mount",
            )?;

            // Unnamed inode in the private directory: gone when the last descriptor closes.
            let tmp = open_tmpfile(self.dir.as_raw_fd())?;
            require_same(
                root_mount,
                seams.mount_id_of(tmp.as_raw_fd())?,
                "snapshot is on another mount",
            )?;
            seams.step(Step::SnapshotCreated);
            clone_with_retry(&seams, tmp.as_raw_fd(), source.as_raw_fd())?;
            if statx(tmp.as_raw_fd(), libc::STATX_SIZE)?.stx_size > self.limits.max_bytes {
                return Err(SnapshotError::new(
                    SnapshotClass::TooLarge,
                    io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        "source grew beyond the snapshot size limit while cloning",
                    ),
                ));
            }
            Ok(InputSnapshot { file: tmp, slot })
        }
    }

    impl InputSnapshot {
        /// Reopen the unnamed inode with an independent offset for each decoding pass. The
        /// returned file holds its own concurrency slot.
        pub fn open(&self) -> io::Result<SnapshotFile> {
            let file = reopen_read(self.file.as_raw_fd(), 0)?;
            if identity(self.file.as_raw_fd())? != identity(file.as_raw_fd())? {
                return Err(io::Error::other(
                    "snapshot reopen resolved to a different inode",
                ));
            }
            Ok(SnapshotFile {
                file,
                _slot: self.slot.clone(),
            })
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::reflink_supported_in;
#[cfg(all(test, target_os = "linux"))]
use linux::{Seams, Step};

#[cfg(not(target_os = "linux"))]
pub fn reflink_supported_in(_dir: &Path) -> io::Result<bool> {
    Err(unsupported())
}

#[cfg(not(target_os = "linux"))]
impl SnapshotStore {
    pub fn open(_dir: &Path, _limits: SnapshotLimits) -> io::Result<Self> {
        Err(unsupported())
    }
    pub fn check_mount(&self, _root: &Path) -> io::Result<()> {
        Err(unsupported())
    }
    pub fn create(&self, _root: &Path, _relative: &Path) -> Result<InputSnapshot, SnapshotError> {
        Err(unsupported().into())
    }
}

#[cfg(not(target_os = "linux"))]
impl InputSnapshot {
    pub fn open(&self) -> io::Result<SnapshotFile> {
        Err(unsupported())
    }
}

impl SnapshotStore {
    /// Currently live snapshots (and attempts in flight).
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Read;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir_in(kima_test_support::reflink_test_dir(env!(
            "CARGO_MANIFEST_DIR"
        )))
        .unwrap()
    }

    /// Library tree plus a private (0700) snapshot dir on the same filesystem.
    struct Fixture {
        lib: tempfile::TempDir,
        snap: tempfile::TempDir,
        store: SnapshotStore,
    }
    fn fixture_with(limits: SnapshotLimits) -> Fixture {
        let lib = tmp();
        let snap = tmp();
        std::fs::set_permissions(snap.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = SnapshotStore::open(snap.path(), limits).unwrap();
        Fixture { lib, snap, store }
    }
    /// For tests that fail before any clone (no reflink needed).
    fn fixture() -> Fixture {
        fixture_with(SnapshotLimits::default())
    }
    /// Positive tests: a filesystem without reflink is reported loudly, never a silent pass.
    /// `KIMA_TEST_ALLOW_NO_REFLINK=1` turns the failure into an explicit `SKIPPED` line.
    fn reflink_fixture_with(limits: SnapshotLimits) -> Option<Fixture> {
        let f = fixture_with(limits);
        match reflink_supported_in(f.snap.path()) {
            Ok(true) => Some(f),
            other => {
                let msg = format!(
                    "NO REFLINK SUPPORT in {} ({other:?})",
                    f.snap.path().display()
                );
                if std::env::var("KIMA_TEST_ALLOW_NO_REFLINK").as_deref() == Ok("1") {
                    eprintln!("SKIPPED: {msg}");
                    None
                } else {
                    panic!("{msg}; run on btrfs/XFS-reflink or set KIMA_TEST_ALLOW_NO_REFLINK=1");
                }
            }
        }
    }
    macro_rules! reflink_fixture {
        () => {
            reflink_fixture!(SnapshotLimits::default())
        };
        ($limits:expr) => {
            match reflink_fixture_with($limits) {
                Some(f) => f,
                None => return,
            }
        };
    }
    fn seams(hook: &dyn Fn(Step)) -> Seams<'_> {
        Seams {
            hook,
            ..Seams::real()
        }
    }
    fn read(s: &InputSnapshot) -> Vec<u8> {
        let mut v = Vec::new();
        s.open().unwrap().read_to_end(&mut v).unwrap();
        v
    }
    fn mkfifo(path: &Path) {
        let n = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(n.as_ptr(), 0o600) }, 0);
    }
    fn err_class(r: Result<InputSnapshot, SnapshotError>) -> SnapshotClass {
        r.err().expect("expected an error").class()
    }

    #[test]
    fn snapshot_keeps_original_bytes() {
        let f = reflink_fixture!();
        std::fs::write(f.lib.path().join("a"), b"original").unwrap();
        let s = f.store.create(f.lib.path(), "a".as_ref()).unwrap();
        std::fs::write(f.lib.path().join("a"), b"changed!").unwrap();
        assert_eq!(read(&s), b"original");
        assert_eq!(read(&s), b"original");
    }

    #[test]
    fn parent_moved_outside_root_after_root_open_cannot_be_reached() {
        let f = fixture();
        let outside = tmp();
        std::fs::create_dir(f.lib.path().join("d")).unwrap();
        std::fs::write(f.lib.path().join("d/a"), b"inside").unwrap();
        let moved = outside.path().join("d");
        let lib = f.lib.path().to_path_buf();
        let hook = |step| {
            if step == Step::RootOpened {
                std::fs::rename(lib.join("d"), &moved).unwrap();
            }
        };
        let err = f
            .store
            .create_with(f.lib.path(), "d/a".as_ref(), seams(&hook))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(err.class(), SnapshotClass::Missing);
        assert_eq!(f.store.active(), 0);
    }

    #[test]
    fn parent_replaced_by_symlink_after_root_open_is_refused() {
        let f = fixture();
        let outside = tmp();
        std::fs::write(outside.path().join("a"), b"secret").unwrap();
        std::fs::create_dir(f.lib.path().join("d")).unwrap();
        std::fs::write(f.lib.path().join("d/a"), b"inside").unwrap();
        let lib = f.lib.path().to_path_buf();
        let target = outside.path().to_path_buf();
        let hook = |step| {
            if step == Step::SnapshotDirChecked {
                std::fs::rename(lib.join("d"), lib.join("old")).unwrap();
                symlink(&target, lib.join("d")).unwrap();
            }
        };
        let err = f
            .store
            .create_with(f.lib.path(), "d/a".as_ref(), seams(&hook))
            .err()
            .unwrap();
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        assert_eq!(err.class(), SnapshotClass::InvalidPath);
    }

    #[test]
    fn basename_replaced_by_symlink_or_fifo_between_steps_is_refused() {
        for kind in ["symlink", "fifo"] {
            let f = fixture();
            let outside = tmp();
            std::fs::write(outside.path().join("f"), b"secret").unwrap();
            std::fs::write(f.lib.path().join("a"), b"x").unwrap();
            let lib = f.lib.path().to_path_buf();
            let target = outside.path().join("f");
            let hook = |step| {
                if step == Step::RootOpened {
                    std::fs::remove_file(lib.join("a")).unwrap();
                    if kind == "symlink" {
                        symlink(&target, lib.join("a")).unwrap();
                    } else {
                        mkfifo(&lib.join("a"));
                    }
                }
            };
            let class = err_class(
                f.store
                    .create_with(f.lib.path(), "a".as_ref(), seams(&hook)),
            );
            let want = if kind == "symlink" {
                SnapshotClass::InvalidPath
            } else {
                SnapshotClass::NotRegular
            };
            assert_eq!(class, want, "{kind}");
        }
    }

    #[test]
    fn basename_replaced_after_resolution_snapshots_the_resolved_inode() {
        let f = reflink_fixture!();
        std::fs::write(f.lib.path().join("a"), b"resolved").unwrap();
        let lib = f.lib.path().to_path_buf();
        let hook = |step| {
            if step == Step::SourceResolved {
                std::fs::remove_file(lib.join("a")).unwrap();
                std::fs::write(lib.join("a"), b"replaced").unwrap();
            }
        };
        let s = f
            .store
            .create_with(f.lib.path(), "a".as_ref(), seams(&hook))
            .unwrap();
        assert_eq!(read(&s), b"resolved");
    }

    #[test]
    fn root_renamed_after_open_stays_pinned() {
        let f = reflink_fixture!();
        std::fs::write(f.lib.path().join("a"), b"pinned").unwrap();
        let parent = tmp();
        let new = parent.path().join("moved");
        let lib = f.lib.path().to_path_buf();
        let hook = |step| {
            if step == Step::RootOpened {
                std::fs::rename(&lib, &new).unwrap();
            }
        };
        let r = f
            .store
            .create_with(f.lib.path(), "a".as_ref(), seams(&hook));
        // Move back so TempDir cleanup works.
        let _ = std::fs::rename(&new, f.lib.path());
        assert_eq!(read(&r.unwrap()), b"pinned");
    }

    #[test]
    fn symlink_in_every_component_is_refused() {
        let f = fixture();
        let real = f.lib.path().join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        std::fs::write(real.join("sub/file"), b"x").unwrap();
        symlink(&real, f.lib.path().join("l1")).unwrap();
        symlink(real.join("sub"), real.join("l2")).unwrap();
        symlink(real.join("sub/file"), real.join("sub/l3")).unwrap();
        for rel in ["l1/sub/file", "real/l2/file", "real/sub/l3"] {
            let e = f.store.create(f.lib.path(), rel.as_ref()).err().unwrap();
            assert_eq!(e.raw_os_error(), Some(libc::ELOOP), "{rel}");
            assert_eq!(e.class(), SnapshotClass::InvalidPath, "{rel}");
        }
        // Root itself, and the snapshot directory, may not be symlinks.
        let link = tmp();
        let link_path = link.path().join("root-link");
        symlink(f.lib.path(), &link_path).unwrap();
        let e = f
            .store
            .create(&link_path, "real/sub/file".as_ref())
            .err()
            .unwrap();
        assert_eq!(e.raw_os_error(), Some(libc::ELOOP));
        assert_eq!(e.class(), SnapshotClass::Infrastructure);
        let snap_link = link.path().join("snap-link");
        symlink(f.snap.path(), &snap_link).unwrap();
        let e = SnapshotStore::open(&snap_link, SnapshotLimits::default())
            .err()
            .unwrap();
        assert_eq!(e.raw_os_error(), Some(libc::ELOOP));
        assert_eq!(f.store.active(), 0);
    }

    #[test]
    fn rejects_fifo_absolute_traversal_and_directories_with_the_right_class() {
        let f = fixture();
        mkfifo(&f.lib.path().join("fifo"));
        std::fs::create_dir(f.lib.path().join("dir")).unwrap();
        let cases = [
            ("fifo", SnapshotClass::NotRegular),
            ("dir", SnapshotClass::NotRegular),
            ("/etc/passwd", SnapshotClass::InvalidPath),
            ("../x", SnapshotClass::InvalidPath),
            ("a/../a", SnapshotClass::InvalidPath),
            ("./a", SnapshotClass::InvalidPath),
            ("", SnapshotClass::InvalidPath),
            ("missing", SnapshotClass::Missing),
        ];
        for (p, want) in cases {
            assert_eq!(
                err_class(f.store.create(f.lib.path(), p.as_ref())),
                want,
                "{p:?}"
            );
        }
        for root in ["relative", "/x/../etc"] {
            let e = f.store.create(root.as_ref(), "a".as_ref()).err().unwrap();
            assert_eq!(e.class(), SnapshotClass::Infrastructure, "{root}");
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{root}");
        }
    }

    #[test]
    fn missing_mount_id_data_is_an_error() {
        let f = fixture();
        std::fs::write(f.lib.path().join("a"), b"x").unwrap();
        let s = Seams {
            mount_id: |_| Err(io::Error::from(io::ErrorKind::Unsupported)),
            ..seams(&|_| {})
        };
        let e = f
            .store
            .create_with(f.lib.path(), "a".as_ref(), s)
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::Unsupported);
        assert_eq!(e.class(), SnapshotClass::Infrastructure);
        assert_eq!(f.store.active(), 0);
    }

    thread_local!(static CALLS: Cell<u32> = const { Cell::new(0) });
    thread_local!(static MISMATCH_AT: Cell<u32> = const { Cell::new(0) });

    /// Reports mount 1 for every call except the `MISMATCH_AT`-th, which reports 999.
    fn mismatch_from_nth(_: RawFd) -> io::Result<MountId> {
        let n = CALLS.with(|c| {
            c.set(c.get() + 1);
            c.get()
        });
        Ok(MountId::Unique(if n == MISMATCH_AT.with(Cell::get) {
            999
        } else {
            1
        }))
    }

    /// Mount-ID calls, in order: 1 root, 2 snapshot dir, 3 source, 4 reopened source,
    /// 5 snapshot. A mismatch in each one is refused with its own message, leaks nothing and
    /// never reaches the clone.
    #[test]
    fn mount_id_mismatch_is_refused_at_every_comparison() {
        thread_local!(static CLONED: Cell<bool> = const { Cell::new(false) });
        let expected = [
            (1, "snapshot directory is on another mount"),
            (2, "snapshot directory is on another mount"),
            (3, "source is on another mount"),
            (4, "reopened source is on another mount"),
            (5, "snapshot is on another mount"),
        ];
        for (nth, message) in expected {
            let f = fixture();
            std::fs::write(f.lib.path().join("a"), b"x").unwrap();
            CALLS.with(|c| c.set(0));
            CLONED.with(|c| c.set(false));
            MISMATCH_AT.with(|m| m.set(nth));
            let s = Seams {
                mount_id: mismatch_from_nth,
                clone: |_, _| {
                    CLONED.with(|c| c.set(true));
                    Ok(())
                },
                ..seams(&|_| {})
            };
            let e = f
                .store
                .create_with(f.lib.path(), "a".as_ref(), s)
                .err()
                .unwrap();
            assert_eq!(e.to_string(), message, "mismatch at call {nth}");
            // Only the source's own mount is a property of that file; the rest is global.
            let want = if matches!(nth, 3 | 4) {
                SnapshotClass::Unreadable
            } else {
                SnapshotClass::Infrastructure
            };
            assert_eq!(e.class(), want, "mismatch at call {nth}");
            assert!(!CLONED.with(Cell::get), "clone ran at call {nth}");
            assert_eq!(f.store.active(), 0);
            assert_eq!(std::fs::read_dir(f.snap.path()).unwrap().count(), 0);
        }
        // Control: no mismatch -> exactly five comparisons, the clone runs, the attempt succeeds.
        let f = fixture();
        std::fs::write(f.lib.path().join("a"), b"x").unwrap();
        CALLS.with(|c| c.set(0));
        CLONED.with(|c| c.set(false));
        MISMATCH_AT.with(|m| m.set(0));
        let s = Seams {
            mount_id: mismatch_from_nth,
            clone: |_, _| {
                CLONED.with(|c| c.set(true));
                Ok(())
            },
            ..seams(&|_| {})
        };
        assert!(f.store.create_with(f.lib.path(), "a".as_ref(), s).is_ok());
        assert_eq!(CALLS.with(Cell::get), 5);
        assert!(CLONED.with(Cell::get));
    }

    #[test]
    fn check_mount_refuses_a_mismatch_and_bad_roots() {
        let f = fixture();
        MISMATCH_AT.with(|m| m.set(1));
        CALLS.with(|c| c.set(0));
        let s = Seams {
            mount_id: mismatch_from_nth,
            ..seams(&|_| {})
        };
        let e = f.store.check_mount_with(f.lib.path(), s).unwrap_err();
        assert!(e.to_string().contains("another mount"), "{e}");
        assert!(f.store.check_mount(f.lib.path()).is_ok());
        for root in ["relative", "/x/../y"] {
            assert_eq!(
                f.store.check_mount(root.as_ref()).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            f.store
                .check_mount("/definitely/not/here".as_ref())
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn mixed_mount_id_kinds_are_not_comparable() {
        let f = fixture();
        std::fs::write(f.lib.path().join("a"), b"x").unwrap();
        thread_local!(static K: Cell<bool> = const { Cell::new(false) });
        let s = Seams {
            mount_id: |_| {
                Ok(if K.with(|k| k.replace(true)) {
                    MountId::Legacy(1)
                } else {
                    MountId::Unique(1)
                })
            },
            ..seams(&|_| {})
        };
        let e = f
            .store
            .create_with(f.lib.path(), "a".as_ref(), s)
            .err()
            .unwrap();
        assert!(e.to_string().contains("kinds differ"), "{e}");
    }

    #[test]
    fn legacy_mount_ids_are_accepted_when_consistent() {
        let f = fixture();
        std::fs::write(f.lib.path().join("a"), b"x").unwrap();
        let s = Seams {
            mount_id: |_| Ok(MountId::Legacy(7)),
            clone: |_, _| Ok(()),
            ..seams(&|_| {})
        };
        assert!(f.store.create_with(f.lib.path(), "a".as_ref(), s).is_ok());
    }

    #[test]
    fn real_statx_reports_a_mount_id() {
        let d = tmp();
        let f = File::open(d.path()).unwrap();
        assert!(linux::mount_id(f.as_raw_fd()).is_ok());
    }

    #[test]
    fn unreported_statx_fields_are_an_error() {
        let all = libc::STATX_TYPE | libc::STATX_SIZE;
        assert!(require_fields(all, all).is_ok());
        assert!(require_fields(all | libc::STATX_INO, all).is_ok());
        for got in [0, libc::STATX_TYPE, libc::STATX_SIZE] {
            let e = require_fields(got, all).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::Unsupported, "{got:#x}");
        }
    }

    #[test]
    fn eintr_is_retried_for_raw_syscalls() {
        let calls = Cell::new(0);
        let r = retry_eintr(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 4 {
                Err(io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(calls.get())
            }
        });
        assert_eq!(r.unwrap(), 4);
        // Other errors are returned at once.
        let calls = Cell::new(0);
        let r: io::Result<()> = retry_eintr(|| {
            calls.set(calls.get() + 1);
            Err(io::Error::from_raw_os_error(libc::EAGAIN))
        });
        assert_eq!(r.unwrap_err().raw_os_error(), Some(libc::EAGAIN));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn source_access_and_xdev_errors_are_per_file_unreadable() {
        for errno in [libc::EACCES, libc::EPERM, libc::EXDEV] {
            let e = classify_resolve(io::Error::from_raw_os_error(errno));
            assert_eq!(
                e.class(),
                SnapshotClass::Unreadable,
                "resolve errno {errno}"
            );
        }
        for errno in [libc::EACCES, libc::EPERM] {
            let e = classify_reopen(io::Error::from_raw_os_error(errno));
            assert_eq!(e.class(), SnapshotClass::Unreadable, "reopen errno {errno}");
        }
        for errno in [
            libc::ENOSPC,
            libc::EROFS,
            libc::EMFILE,
            libc::EAGAIN,
            libc::EXDEV,
        ] {
            let e = classify_reopen(io::Error::from_raw_os_error(errno));
            assert_eq!(
                e.class(),
                SnapshotClass::Infrastructure,
                "reopen errno {errno}"
            );
        }
        for errno in [libc::ENOSPC, libc::EROFS, libc::EMFILE, libc::EAGAIN] {
            let e = classify_resolve(io::Error::from_raw_os_error(errno));
            assert_eq!(
                e.class(),
                SnapshotClass::Infrastructure,
                "resolve errno {errno}"
            );
        }
        assert!(!SnapshotClass::Unreadable.is_permanent());
    }

    #[test]
    fn exdev_and_no_reflink_fail_closed_without_fallback() {
        thread_local!(static E: Cell<i32> = const { Cell::new(0) });
        for errno in [
            libc::EXDEV,
            libc::EOPNOTSUPP,
            libc::EINVAL,
            libc::EROFS,
            libc::ENOSPC,
        ] {
            let f = fixture();
            std::fs::write(f.lib.path().join("a"), b"x").unwrap();
            E.with(|e| e.set(errno));
            let s = Seams {
                clone: |_, _| Err(io::Error::from_raw_os_error(E.with(Cell::get))),
                ..seams(&|_| {})
            };
            let err = f
                .store
                .create_with(f.lib.path(), "a".as_ref(), s)
                .err()
                .unwrap();
            assert_eq!(err.raw_os_error(), Some(errno));
            assert_eq!(err.class(), SnapshotClass::Infrastructure);
            assert_eq!(f.store.active(), 0);
        }
    }

    #[test]
    fn eagain_is_retried_a_bounded_number_of_times() {
        let f = reflink_fixture!();
        std::fs::write(f.lib.path().join("a"), b"x").unwrap();
        // Succeeds on the 3rd attempt.
        CALLS.with(|c| c.set(0));
        let ok = Seams {
            clone: |d, s| {
                CALLS.with(|c| c.set(c.get() + 1));
                if CALLS.with(Cell::get) < 3 {
                    Err(io::Error::from_raw_os_error(libc::EAGAIN))
                } else {
                    linux::ficlone(d, s)
                }
            },
            ..seams(&|_| {})
        };
        assert!(f.store.create_with(f.lib.path(), "a".as_ref(), ok).is_ok());
        assert_eq!(CALLS.with(Cell::get), 3);
        // Always EAGAIN: gives up after CLONE_ATTEMPTS, as infrastructure.
        CALLS.with(|c| c.set(0));
        let bad = Seams {
            clone: |_, _| {
                CALLS.with(|c| c.set(c.get() + 1));
                Err(io::Error::from_raw_os_error(libc::EAGAIN))
            },
            ..seams(&|_| {})
        };
        let f2 = fixture();
        std::fs::write(f2.lib.path().join("a"), b"x").unwrap();
        let e = f2
            .store
            .create_with(f2.lib.path(), "a".as_ref(), bad)
            .err()
            .unwrap();
        assert_eq!(e.raw_os_error(), Some(libc::EAGAIN));
        assert_eq!(e.class(), SnapshotClass::Infrastructure);
        assert_eq!(CALLS.with(Cell::get), CLONE_ATTEMPTS);
    }

    #[test]
    fn size_limit_and_concurrency_limit_are_enforced() {
        let f = reflink_fixture!(SnapshotLimits {
            max_bytes: 4,
            max_concurrent: 1,
        });
        std::fs::write(f.lib.path().join("big"), b"12345").unwrap();
        std::fs::write(f.lib.path().join("ok"), b"1234").unwrap();
        let e = f.store.create(f.lib.path(), "big".as_ref()).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::FileTooLarge);
        assert_eq!(e.class(), SnapshotClass::TooLarge);
        assert_eq!(f.store.active(), 0);
        let first = f.store.create(f.lib.path(), "ok".as_ref()).unwrap();
        let e = f.store.create(f.lib.path(), "ok".as_ref()).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::QuotaExceeded);
        assert_eq!(e.class(), SnapshotClass::Infrastructure);
        assert_eq!(f.store.active(), 1);
        drop(first);
        assert_eq!(f.store.active(), 0);
        assert!(f.store.create(f.lib.path(), "ok".as_ref()).is_ok());
    }

    #[test]
    fn slot_reservation_never_exceeds_the_limit_under_contention() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (active, peak) = (active.clone(), peak.clone());
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        if let Some(slot) = reserve(&active, 2) {
                            peak.fetch_max(active.load(Ordering::Acquire), Ordering::AcqRel);
                            drop(slot);
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(peak.load(Ordering::Acquire) <= 2);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn growth_during_clone_is_caught_after_the_fact() {
        let f = reflink_fixture!(SnapshotLimits {
            max_bytes: 4,
            max_concurrent: 1,
        });
        std::fs::write(f.lib.path().join("a"), b"1234").unwrap();
        let lib = f.lib.path().to_path_buf();
        let hook = |step| {
            if step == Step::SnapshotCreated {
                std::fs::write(lib.join("a"), b"1234567890").unwrap();
            }
        };
        let e = f
            .store
            .create_with(f.lib.path(), "a".as_ref(), seams(&hook))
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::FileTooLarge);
        assert_eq!(e.class(), SnapshotClass::TooLarge);
    }

    #[test]
    fn snapshot_dir_must_be_private_and_owned() {
        let d = tmp();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        let e = SnapshotStore::open(d.path(), SnapshotLimits::default())
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(SnapshotStore::open(d.path(), SnapshotLimits::default()).is_ok());
        let e = SnapshotStore::open("rel".as_ref(), SnapshotLimits::default())
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        let dotdot = d.path().join("..").join(d.path().file_name().unwrap());
        let e = SnapshotStore::open(&dotdot, SnapshotLimits::default())
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        let zero = SnapshotLimits {
            max_bytes: 0,
            max_concurrent: 1,
        };
        assert_eq!(
            SnapshotStore::open(d.path(), zero).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn snapshot_is_unnamed_private_and_leaves_no_entries() {
        use std::os::unix::fs::MetadataExt;
        let f = reflink_fixture!();
        std::fs::write(f.lib.path().join("a"), b"audio").unwrap();
        let s = f.store.create(f.lib.path(), "a".as_ref()).unwrap();
        let m = s.open().unwrap().metadata().unwrap();
        assert_eq!(m.nlink(), 0);
        assert_eq!(m.mode() & 0o077, 0);
        assert_eq!(std::fs::read_dir(f.lib.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(f.snap.path()).unwrap().count(), 0);
        drop(s);
        assert_eq!(std::fs::read_dir(f.snap.path()).unwrap().count(), 0);
    }

    #[test]
    fn tmpfs_without_reflink_fails_closed_as_infrastructure() {
        let lib = tempfile::tempdir_in("/dev/shm").unwrap();
        let snap = tempfile::tempdir_in("/dev/shm").unwrap();
        std::fs::set_permissions(snap.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(lib.path().join("a"), b"x").unwrap();
        assert!(!reflink_supported_in(snap.path()).unwrap());
        let store = SnapshotStore::open(snap.path(), SnapshotLimits::default()).unwrap();
        let e = store.create(lib.path(), "a".as_ref()).err().unwrap();
        assert!(
            matches!(
                e.raw_os_error(),
                Some(libc::EOPNOTSUPP | libc::EINVAL | libc::ENOTTY | libc::EXDEV)
            ),
            "{e:?}"
        );
        assert_eq!(e.class(), SnapshotClass::Infrastructure);
        assert_eq!(store.active(), 0);
    }
}
