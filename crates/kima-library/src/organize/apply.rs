//! Runs organizer runs on the `library` queue (spec §5.2): plan a preview, apply it (copy or
//! move), resume an interrupted apply from the journal, undo it, and switch the library root
//! to a verified copy.
//!
//! Safety rules:
//! - Nothing is ever overwritten. A file is placed with a hard link (which fails when the name
//!   is taken) and the old name is then unlinked, or, where hard links are not supported, with
//!   a rename after checking that the name is free.
//! - A copy is written to `<to>.kima-part`, fsynced, re-read and compared by SHA-256, and only
//!   then given its name. A move across filesystems is such a copy followed by deleting the
//!   source. A source is only ever deleted once an identical file sits at the target.
//! - The source checksum is written to the journal before a file is touched, and a row's new
//!   state is committed in the same transaction as the track's new path. A crash between the
//!   file operation and that commit is recognised on resume: the file is found at its target
//!   (or staging name) with the journalled checksum.
//! - Swap cycles (A→B while B→A) and case-only renames go through a hidden staging name inside
//!   the target folder (`.kima-stage-<row>-<name>`), journalled as `staged`, and are placed in
//!   a second pass once every other row has moved.

use std::collections::HashSet;
use std::fs::{self, File, FileTimes};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kima_core::events::Event;
use kima_db::organizer::{self as db, JournalRow, NewRow, Run};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::jobs;
use crate::organize::paths::{self, Fields};
use crate::scan::LibraryCtx;

/// How long the watcher keeps ignoring a path after the organizer touched it. Events reach
/// it only after its debounce (2 s), so the paths must outlive the operation.
pub const IGNORE_GRACE: Duration = Duration::from_secs(10);
/// At most four progress events per second.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const PART_SUFFIX: &str = ".kima-part";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Preview,
    Apply,
    Undo,
    SwitchRoot,
}

/// Called on a finished `.kima-part` copy (tests corrupt it).
pub type CopyHook = Arc<dyn Fn(&Path) + Send + Sync>;

/// Test hooks. Production passes [`Hooks::default`].
#[derive(Default, Clone)]
pub struct Hooks {
    /// Stop, as if the process died, once this many rows have finished in this call.
    pub stop_after: Option<usize>,
    /// With `stop_after`: do the next row's file operation, then stop before its journal
    /// commit (the worst moment to die).
    pub stop_mid_row: bool,
    /// Called on each finished `.kima-part` copy before it is verified.
    pub after_copy: Option<CopyHook>,
    /// Treat every move as crossing filesystems (copy, verify, delete the source).
    pub force_cross_fs: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum OrganizeError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Invalid(String),
    /// The action was refused before anything was touched; the run is back in its earlier
    /// state with the reason recorded.
    #[error("{0}")]
    Refused(String),
    /// The job's cancel token fired (shutdown or a lost claim), or a test hook stopped the run
    /// mid-way. The journal keeps what was done; the next run resumes from it.
    #[error("organizer run interrupted")]
    Interrupted,
}

impl From<io::Error> for OrganizeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

type Result<T> = std::result::Result<T, OrganizeError>;

/// Queues an organizer job for `run_id`.
pub async fn enqueue(pool: &kima_db::PgPool, run_id: Uuid, action: Action) -> sqlx::Result<Uuid> {
    kima_jobs::queue::enqueue(
        pool,
        jobs::QUEUE,
        json!({ "kind": "organize", "runId": run_id, "action": action }),
        jobs::MAX_ATTEMPTS,
    )
    .await
}

/// Startup: re-queues runs left `planning`, `applying` or `undoing` that have no organizer job
/// waiting or running (a job that died on its final attempt). Call after the queue's
/// `requeue_running`. Returns how many were queued.
pub async fn resume_interrupted(pool: &kima_db::PgPool) -> sqlx::Result<usize> {
    let mut queued = 0;
    for run in db::interrupted_runs(pool).await? {
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM jobs WHERE queue = $1 AND state IN ('queued', 'running')
                            AND payload ->> 'kind' = 'organize' AND payload ->> 'runId' = $2)",
        )
        .bind(jobs::QUEUE)
        .bind(run.id.to_string())
        .fetch_one(pool)
        .await?;
        if pending {
            continue;
        }
        let action = match run.state.as_str() {
            "planning" => Action::Preview,
            "undoing" => Action::Undo,
            _ => Action::Apply,
        };
        info!(run = %run.id, ?action, "resuming an interrupted organizer run");
        enqueue(pool, run.id, action).await?;
        queued += 1;
    }
    Ok(queued)
}

/// The job body. A failure ends the run as `failed` (an apply can be started again and resumes
/// from the journal); only a database error that also prevents recording that is returned.
pub async fn run_job(ctx: &LibraryCtx, run_id: Uuid, action: Action) -> Result<()> {
    run_job_cancellable(ctx, run_id, action, &CancellationToken::new()).await
}

/// [`run_job`], returning [`OrganizeError::Interrupted`] at the next row boundary once `cancel`
/// fires. The run is left as it is (not failed), to be resumed from the journal.
pub async fn run_job_cancellable(
    ctx: &LibraryCtx,
    run_id: Uuid,
    action: Action,
    cancel: &CancellationToken,
) -> Result<()> {
    match run_action_cancellable(ctx, run_id, action, &Hooks::default(), cancel).await {
        Ok(()) => Ok(()),
        Err(OrganizeError::Interrupted) => Err(OrganizeError::Interrupted),
        Err(OrganizeError::Refused(reason)) => {
            warn!(run = %run_id, ?action, %reason, "organizer action refused");
            Ok(())
        }
        Err(e) => {
            warn!(run = %run_id, ?action, error = %e, "organizer run failed");
            db::fail_run(&ctx.pool, run_id, &e.to_string()).await?;
            Ok(())
        }
    }
}

/// Runs one action with test hooks.
pub async fn run_action(
    ctx: &LibraryCtx,
    run_id: Uuid,
    action: Action,
    hooks: &Hooks,
) -> Result<()> {
    run_action_cancellable(ctx, run_id, action, hooks, &CancellationToken::new()).await
}

async fn run_action_cancellable(
    ctx: &LibraryCtx,
    run_id: Uuid,
    action: Action,
    hooks: &Hooks,
    cancel: &CancellationToken,
) -> Result<()> {
    let run = db::get_run(&ctx.pool, run_id)
        .await?
        .ok_or_else(|| OrganizeError::Invalid(format!("organizer run {run_id} not found")))?;
    match action {
        Action::Preview => preview(ctx, run).await,
        Action::Apply => apply(ctx, run, hooks, cancel).await,
        Action::Undo => undo(ctx, run, hooks, cancel).await,
        Action::SwitchRoot => switch_root(ctx, run).await,
    }
}

// ---------------------------------------------------------------------------------------------
// Preview

/// `rel` joined to `root`, refusing anything but plain components.
fn join_rel(root: &Path, rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(OrganizeError::Invalid(format!(
            "unsafe path in the journal: {rel}"
        )));
    }
    Ok(root.join(p))
}

fn rel_string(p: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Every non-hidden file under `root`, relative. Hidden files and folders (kima's own data,
/// staging names) are not part of the library.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .collect()
}

fn fields_of(t: &db::PlanTrack) -> Fields {
    Fields {
        album_artist: t.album_artist.clone(),
        track_artist: t.track_artist.clone(),
        album: t.album.clone(),
        year: t.album_year,
        disc: u32::try_from(t.disc).unwrap_or(1).max(1),
        disc_total: 0,
        track_no: u32::try_from(t.track_no).unwrap_or(0),
        title: t.title.clone(),
        ext: Path::new(&t.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase(),
        is_compilation: t.is_compilation,
    }
}

/// Plans the run into the journal. Reads the database and lists folders; touches no file.
async fn preview(ctx: &LibraryCtx, run: Run) -> Result<()> {
    if run.state != "planning" {
        return Err(OrganizeError::Invalid(format!(
            "run is {}, not planning",
            run.state
        )));
    }
    let source = PathBuf::from(&run.source_root);
    let target = PathBuf::from(&run.target_root);
    let copy = run.mode == db::MODE_COPY;
    let tracks = db::plan_tracks(&ctx.pool).await?;

    let (src_walk, dst_walk) = {
        let (s, t) = (source.clone(), target.clone());
        tokio::task::spawn_blocking(move || {
            let src = walk_files(&s);
            let dst = if copy { walk_files(&t) } else { Vec::new() };
            (src, dst)
        })
        .await
        .map_err(|e| OrganizeError::Io(e.to_string()))?
    };
    // In copy mode the sources live under another root than the targets. `plan` treats the
    // planned tracks' own paths as vacated and a path equal to its target as unchanged, so the
    // sources are given as absolute paths: they can then never be mistaken for target paths.
    let src_key = |rel: &Path| {
        if copy {
            source.join(rel)
        } else {
            rel.to_path_buf()
        }
    };
    let existing: HashSet<PathBuf> = if copy {
        dst_walk.into_iter().collect()
    } else {
        src_walk.iter().cloned().collect()
    };
    let inputs: Vec<(Uuid, Fields, PathBuf)> = tracks
        .iter()
        .map(|t| (t.id, fields_of(t), src_key(Path::new(&t.path))))
        .collect();
    let moves = paths::plan(&inputs, &existing);
    let sidecars: Vec<PathBuf> = src_walk
        .iter()
        .filter(|p| paths::is_sidecar(p))
        .map(|p| src_key(p))
        .collect();
    let sidecar_moves = paths::plan_sidecars(&moves, &sidecars, &existing);

    let strip = |p: &Path| -> Option<String> {
        if copy {
            rel_string(p.strip_prefix(&source).ok()?)
        } else {
            rel_string(p)
        }
    };
    let mut rows = Vec::new();
    let (mut unchanged, mut collisions, mut sidecar_rows) = (0usize, 0usize, 0usize);
    // Files already in place are journalled as `unchanged`, so the preview can list them;
    // apply and undo never touch them.
    for m in &moves {
        let (Some(from), Some(to)) = (strip(&m.from), rel_string(&m.to)) else {
            continue;
        };
        unchanged += usize::from(m.unchanged);
        collisions += usize::from(m.collision);
        rows.push(NewRow {
            track_id: Some(m.track_id),
            from_path: from,
            to_path: to,
            collision: m.collision,
            unchanged: m.unchanged,
        });
    }
    for s in &sidecar_moves {
        let (Some(from), Some(to)) = (strip(&s.from), rel_string(&s.to)) else {
            continue;
        };
        sidecar_rows += usize::from(!s.unchanged);
        unchanged += usize::from(s.unchanged);
        collisions += usize::from(s.collision);
        rows.push(NewRow {
            track_id: None,
            from_path: from,
            to_path: to,
            collision: s.collision,
            unchanged: s.unchanged,
        });
    }
    db::replace_rows(&ctx.pool, run.id, &rows).await?;
    db::set_summary(
        &ctx.pool,
        run.id,
        &json!({
            "tracks": tracks.len(),
            "planned": rows.len() - unchanged,
            "sidecars": sidecar_rows,
            "unchanged": unchanged,
            "collisions": collisions,
        }),
    )
    .await?;
    db::transition(&ctx.pool, run.id, &["planning"], "previewed").await?;
    info!(run = %run.id, planned = rows.len() - unchanged, unchanged, collisions, "organizer preview ready");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// File helpers (blocking)

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<T> {
    Ok(tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| OrganizeError::Io(e.to_string()))??)
}

/// Like [`blocking`], handing back the I/O result for the caller to inspect.
async fn blocking_io<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<io::Result<T>> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| OrganizeError::Io(e.to_string()))
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

fn sha256_file(p: &Path) -> io::Result<Vec<u8>> {
    let mut f = File::open(p)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

fn sha_matches(p: &Path, sha: &[u8]) -> bool {
    sha256_file(p).is_ok_and(|s| s == sha)
}

/// (device, inode, link count).
#[cfg(unix)]
fn ident(p: &Path) -> Option<(u64, u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(p).ok()?;
    Some((m.dev(), m.ino(), m.nlink()))
}

#[cfg(not(unix))]
fn ident(_p: &Path) -> Option<(u64, u64, u64)> {
    None
}

/// `a` and `b` are two names (differing beyond letter case) of one file: a hard-linked
/// placement that died before unlinking the old name.
fn hard_link_pair(a: &Path, b: &Path) -> bool {
    if lower(a) == lower(b) {
        return false;
    }
    match (ident(a), ident(b)) {
        (Some((da, ia, na)), Some((db_, ib, _))) => da == db_ && ia == ib && na >= 2,
        _ => false,
    }
}

fn lower(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn crosses_devices(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::CrossesDevices || e.raw_os_error() == Some(18)
}

/// Gives `src` the name `dst` without ever replacing an existing `dst`. Same filesystem only;
/// a cross-device error is returned as is.
fn place_no_replace(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::hard_link(src, dst) {
        Ok(()) => {
            fs::remove_file(src)?;
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists || crosses_devices(&e) => Err(e),
        // No hard links here (FAT, some network shares): an atomic no-replace rename where the
        // kernel and filesystem offer one, else check, then rename.
        Err(_) => match rename_noreplace(src, dst)? {
            true => Ok(()),
            false => {
                if exists(dst) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "target exists",
                    ));
                }
                fs::rename(src, dst)
            }
        },
    }?;
    if let Some(parent) = dst.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// `renameat2(RENAME_NOREPLACE)`. `Ok(false)` when the kernel or filesystem does not support
/// it (EINVAL, ENOSYS); an existing `dst` is `AlreadyExists`.
#[cfg(target_os = "linux")]
fn rename_noreplace(src: &Path, dst: &Path) -> io::Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let invalid = |_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte");
    let s = CString::new(src.as_os_str().as_bytes()).map_err(invalid)?;
    let d = CString::new(dst.as_os_str().as_bytes()).map_err(invalid)?;
    // SAFETY: both pointers are valid NUL-terminated strings that outlive the call.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            s.as_ptr(),
            libc::AT_FDCWD,
            d.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOSYS) => Ok(false),
        _ => Err(e),
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_noreplace(_src: &Path, _dst: &Path) -> io::Result<bool> {
    Ok(false)
}

fn part_path(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(PART_SUFFIX);
    dst.with_file_name(name)
}

enum Copied {
    Placed,
    /// The copy's checksum differed; nothing is left at the target.
    Mismatch,
}

/// Copies `src` to `<dst>.kima-part`, fsyncs it, checks it against `sha`, then gives it the
/// name `dst` (never replacing one). The copy keeps the source's mtime and permissions, so the
/// scanner sees it as unchanged.
fn copy_verified(
    src: &Path,
    dst: &Path,
    sha: &[u8],
    after_copy: Option<&(dyn Fn(&Path) + Send + Sync)>,
) -> io::Result<Copied> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if exists(dst) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target exists",
        ));
    }
    let part = part_path(dst);
    // Ours by name: left by an interrupted copy.
    let _ = fs::remove_file(&part);
    let meta = fs::metadata(src)?;
    {
        let mut input = File::open(src)?;
        let mut out = File::create_new(&part)?;
        io::copy(&mut input, &mut out)?;
        out.flush()?;
        let mut times = FileTimes::new();
        if let Ok(m) = meta.modified() {
            times = times.set_modified(m);
        }
        if let Ok(a) = meta.accessed() {
            times = times.set_accessed(a);
        }
        out.set_times(times)?;
        out.sync_all()?;
    }
    if let Some(hook) = after_copy {
        hook(&part);
    }
    if !sha_matches(&part, sha) {
        fs::remove_file(&part)?;
        return Ok(Copied::Mismatch);
    }
    let _ = fs::set_permissions(&part, meta.permissions());
    if let Err(e) = place_no_replace(&part, dst) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    Ok(Copied::Placed)
}

/// Moves `src` to `dst` (never replacing it): a rename on one filesystem, else a verified copy
/// and then deleting the source.
fn move_file(
    src: &Path,
    dst: &Path,
    sha: &[u8],
    force_cross_fs: bool,
    after_copy: Option<&(dyn Fn(&Path) + Send + Sync)>,
) -> io::Result<Copied> {
    // The deepest folder that already exists: every folder below it is created by this move.
    let existing_top = dst
        .parent()
        .and_then(|p| p.ancestors().find(|a| exists(a)))
        .map(Path::to_path_buf);
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if !force_cross_fs {
        match place_no_replace(src, dst) {
            Ok(()) => return Ok(Copied::Placed),
            Err(e) if crosses_devices(&e) => {}
            Err(e) => return Err(e),
        }
    }
    match copy_verified(src, dst, sha, after_copy)? {
        Copied::Placed => {
            // Power loss after the delete must not lose the only copy: make the new file and
            // the folders created for it durable first, from its own folder up to the
            // existing one.
            if let Ok(f) = File::open(dst) {
                let _ = f.sync_all();
            }
            let mut cur = dst.parent();
            while let Some(d) = cur {
                sync_dir(d);
                if Some(d) == existing_top.as_deref() {
                    break;
                }
                cur = d.parent();
            }
            fs::remove_file(src)?;
            if let Some(parent) = src.parent() {
                sync_dir(parent);
            }
            Ok(Copied::Placed)
        }
        Copied::Mismatch => Ok(Copied::Mismatch),
    }
}

/// Some existing folder between `root` (exclusive) and `path` (exclusive) is a symbolic link,
/// or `path` is not under `root`. Writing through such a folder could land outside the root.
fn symlinked_ancestor(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    let parts: Vec<_> = rel.components().collect();
    let mut cur = root.to_path_buf();
    for c in &parts[..parts.len().saturating_sub(1)] {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    false
}

const SYMLINK_NOTE: &str = "a folder on the target path is a symbolic link";
/// A resumed row found its copy in place but the source no longer matches the journalled
/// checksum: something wrote to it in between.
const SOURCE_CHANGED: &str = "the source changed after it was copied; both files were kept";

/// Removes `dir` and its parents while they are empty, stopping at `stop` (never removed).
/// Returns the folders removed.
fn remove_empty_dirs(dir: &Path, stop: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut cur = dir.to_path_buf();
    while cur.starts_with(stop) && cur != stop {
        if fs::remove_dir(&cur).is_err() {
            break;
        }
        removed.push(cur.clone());
        if !cur.pop() {
            break;
        }
    }
    removed
}

// ---------------------------------------------------------------------------------------------
// The runner

struct Progress {
    total: usize,
    done: usize,
    last: Option<Instant>,
}

struct Runner<'a> {
    ctx: &'a LibraryCtx,
    run: Run,
    source: PathBuf,
    target: PathBuf,
    hooks: &'a Hooks,
    cancel: &'a CancellationToken,
    finished: usize,
    progress: Progress,
    /// Paths given to the watcher's ignore set during the current row.
    held: Vec<PathBuf>,
}

/// Which way the files go.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Forward,
    Back,
}

impl Dir {
    fn staged_state(self) -> &'static str {
        match self {
            Dir::Forward => "staged",
            Dir::Back => "undo_staged",
        }
    }
    fn done_state(self) -> &'static str {
        match self {
            Dir::Forward => "done",
            Dir::Back => "undone",
        }
    }
}

/// Where one row's file is, where it goes, and its staging name.
struct Op {
    src: PathBuf,
    src_rel: String,
    dst: PathBuf,
    dst_rel: String,
    stage: PathBuf,
    stage_rel: String,
    /// The journal held the checksum before this call: the row was started earlier.
    started: bool,
}

impl<'a> Runner<'a> {
    fn new(ctx: &'a LibraryCtx, run: Run, hooks: &'a Hooks, cancel: &'a CancellationToken) -> Self {
        let source = PathBuf::from(&run.source_root);
        let target = PathBuf::from(&run.target_root);
        Self {
            ctx,
            run,
            source,
            target,
            hooks,
            cancel,
            finished: 0,
            progress: Progress {
                total: 0,
                done: 0,
                last: None,
            },
            held: Vec::new(),
        }
    }

    fn is_move(&self) -> bool {
        self.run.mode == db::MODE_MOVE
    }

    fn send_progress(&mut self, force: bool) {
        let due = self
            .progress
            .last
            .is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL);
        if !(force || due) {
            return;
        }
        self.progress.last = Some(Instant::now());
        self.ctx.events.publish(Event {
            user_id: None,
            data: json!({
                "type": "library:progress",
                "job": "organize",
                "done": self.progress.done,
                "total": self.progress.total,
            }),
        });
    }

    /// Before a row: stop once the job is cancelled, or at the test hook's plain stop.
    fn checkpoint(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(OrganizeError::Interrupted);
        }
        if self.hooks.stop_after == Some(self.finished) && !self.hooks.stop_mid_row {
            return Err(OrganizeError::Interrupted);
        }
        Ok(())
    }

    /// After a row's file operation, before its commit: the test hook's mid-row stop.
    fn crash_point(&self) -> Result<()> {
        if self.hooks.stop_after == Some(self.finished) && self.hooks.stop_mid_row {
            return Err(OrganizeError::Interrupted);
        }
        Ok(())
    }

    fn row_finished(&mut self) {
        self.finished += 1;
        self.progress.done += 1;
        self.release();
        self.send_progress(false);
    }

    /// Tells the watcher to ignore `paths` (absolute) from now on.
    fn hold(&mut self, paths: &[&Path]) {
        for p in paths {
            self.ctx.ignore.add(p.to_path_buf());
            self.held.push(p.to_path_buf());
        }
    }

    fn release(&mut self) {
        for p in self.held.drain(..) {
            self.ctx.ignore.release(&p, IGNORE_GRACE);
        }
    }

    /// Writes a row's new state, and the track's new path for a move, in one transaction.
    async fn commit(
        &self,
        row: &JournalRow,
        state: &str,
        note: Option<&str>,
        track_path: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.ctx.pool.begin().await?;
        db::set_row_state(&mut tx, row.id, state, note).await?;
        if self.is_move()
            && let (Some(track), Some(path)) = (row.track_id, track_path)
        {
            db::set_track_path(&mut tx, track, path).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The checksum for a row: the journalled one, or computed now and journalled before the
    /// file is touched.
    async fn sha_for(&self, row: &JournalRow, src: &Path) -> Result<Vec<u8>> {
        if let Some(sha) = &row.sha256 {
            return Ok(sha.clone());
        }
        let p = src.to_path_buf();
        let sha = blocking(move || sha256_file(&p)).await?;
        db::set_sha(&self.ctx.pool, row.id, &sha).await?;
        Ok(sha)
    }

    /// Removes folders a moved file left empty, below `root`.
    async fn tidy(&mut self, from: &Path, root: &Path) {
        let (dir, stop) = (from.parent().map(Path::to_path_buf), root.to_path_buf());
        let Some(dir) = dir else { return };
        self.hold(&[&dir]);
        if let Ok(removed) = blocking(move || Ok(remove_empty_dirs(&dir, &stop))).await {
            for d in removed {
                self.ctx.ignore.add(d.clone());
                self.held.push(d);
            }
        }
    }

    async fn track_conflict(&self, row: &JournalRow, path: &str) -> Result<bool> {
        let Some(track) = row.track_id else {
            return Ok(false);
        };
        Ok(db::track_at(&self.ctx.pool, path)
            .await?
            .is_some_and(|other| other != track))
    }
}

fn stage_name(row: &JournalRow, dst_rel: &str, dir: Dir) -> String {
    let (parent, name) = match dst_rel.rsplit_once('/') {
        Some((p, n)) => (format!("{p}/"), n),
        None => (String::new(), dst_rel),
    };
    let tag = match dir {
        Dir::Forward => "stage",
        Dir::Back => "unstage",
    };
    format!("{parent}.kima-{tag}-{}-{name}", row.id)
}

// ---------------------------------------------------------------------------------------------
// Apply

async fn apply(
    ctx: &LibraryCtx,
    run: Run,
    hooks: &Hooks,
    cancel: &CancellationToken,
) -> Result<()> {
    if run.state != "applying" {
        return Err(OrganizeError::Invalid(format!(
            "run is {}, not applying",
            run.state
        )));
    }
    let current = ctx.root.get();
    if current != Path::new(&run.source_root) {
        return Err(OrganizeError::Invalid(format!(
            "the library root changed since the preview ({} -> {})",
            run.source_root,
            current.display()
        )));
    }
    let mut r = Runner::new(ctx, run, hooks, cancel);
    let res = if r.is_move() {
        move_rows(&mut r, Dir::Forward).await
    } else {
        copy_rows(&mut r).await
    };
    // Whatever happened, the watcher stops ignoring the paths (after the grace period).
    r.release();
    res?;
    r.send_progress(true);
    finish(&r, Dir::Forward).await
}

/// Copy mode: every planned row is copied and verified into the target root.
async fn copy_rows(r: &mut Runner<'_>) -> Result<()> {
    let rows: Vec<JournalRow> = db::all_rows(&r.ctx.pool, r.run.id)
        .await?
        .into_iter()
        .filter(|row| row.state == "planned")
        .collect();
    r.progress.total = rows.len();
    r.send_progress(true);
    for row in rows {
        r.checkpoint()?;
        let src = join_rel(&r.source, &row.from_path)?;
        let dst = join_rel(&r.target, &row.to_path)?;
        let started = row.sha256.is_some();
        let (s, d) = (src.clone(), dst.clone());
        let (src_ok, dst_ok) = blocking(move || {
            let _ = fs::remove_file(part_path(&d));
            Ok((exists(&s), exists(&d)))
        })
        .await?;
        if dst_ok {
            let adopt = match (&row.sha256, started) {
                (Some(sha), true) => {
                    let (d, sha) = (dst.clone(), sha.clone());
                    blocking(move || Ok(sha_matches(&d, &sha))).await?
                }
                _ => false,
            };
            if adopt {
                r.commit(&row, "done", None, None).await?;
            } else {
                r.commit(&row, "skipped", Some("target exists"), None)
                    .await?;
            }
            r.row_finished();
            continue;
        }
        if !src_ok {
            r.commit(&row, "skipped", Some("source file missing"), None)
                .await?;
            r.row_finished();
            continue;
        }
        if let Some(track) = row.track_id
            && db::track_path(&r.ctx.pool, track).await?.as_deref() != Some(&row.from_path)
        {
            r.commit(
                &row,
                "skipped",
                Some("track changed since the preview"),
                None,
            )
            .await?;
            r.row_finished();
            continue;
        }
        let (root, d) = (r.target.clone(), dst.clone());
        if blocking(move || Ok(symlinked_ancestor(&root, &d))).await? {
            r.commit(&row, "skipped", Some(SYMLINK_NOTE), None).await?;
            r.row_finished();
            continue;
        }
        let sha = r.sha_for(&row, &src).await?;
        let hook = r.hooks.after_copy.clone();
        let (s, d) = (src.clone(), dst.clone());
        let copied = blocking_io(move || copy_verified(&s, &d, &sha, hook.as_deref())).await?;
        match copied {
            Ok(Copied::Placed) => {
                r.crash_point()?;
                r.commit(&row, "done", None, None).await?;
            }
            Ok(Copied::Mismatch) => {
                r.commit(&row, "failed", Some("the copy did not verify"), None)
                    .await?;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                r.commit(&row, "skipped", Some("target exists"), None)
                    .await?;
            }
            Err(e) => return Err(e.into()),
        }
        r.row_finished();
    }
    Ok(())
}

/// What to do with one row in the first pass.
enum Step {
    /// Both names hold the file already: drop `src`, the row is finished.
    DropSource,
    /// The file already is at `dst` (an earlier attempt placed it): just commit.
    Commit,
    /// It already sits under its staging name: commit `staged`.
    CommitStaged,
    /// Staging was cut short between linking the staging name and dropping the old one.
    DropSourceStaged,
    /// Something is wrong enough that no file may be touched: the row fails.
    Fail(&'static str),
    Stage,
    Direct,
    Refuse(&'static str),
}

/// Moves (forward) or moves back (undo) the rows of a move run, in two passes: first every
/// row is placed or staged, then the staged ones are placed.
async fn move_rows(r: &mut Runner<'_>, dir: Dir) -> Result<()> {
    let all = db::all_rows(&r.ctx.pool, r.run.id).await?;
    let (mut first, mut second): (Vec<JournalRow>, Vec<JournalRow>) = match dir {
        Dir::Forward => (
            all.iter()
                .filter(|x| x.state == "planned")
                .cloned()
                .collect(),
            all.iter()
                .filter(|x| x.state == "staged")
                .cloned()
                .collect(),
        ),
        Dir::Back => {
            // Done rows newest first; rows a failed apply left staged come back from their
            // staging name.
            let mut done: Vec<JournalRow> = all
                .iter()
                .filter(|x| matches!(x.state.as_str(), "done" | "staged" | "undo_failed"))
                .cloned()
                .collect();
            done.sort_by_key(|x| std::cmp::Reverse((x.applied_seq.unwrap_or(i64::MAX), x.id)));
            let mut staged: Vec<JournalRow> = all
                .iter()
                .filter(|x| x.state == "undo_staged")
                .cloned()
                .collect();
            staged.sort_by_key(|x| std::cmp::Reverse((x.applied_seq.unwrap_or(i64::MAX), x.id)));
            (done, staged)
        }
    };
    r.progress.total = first.len() + second.len();
    r.send_progress(true);

    let root = r.source.clone();
    let op_for = |row: &JournalRow| -> Result<Op> {
        let (src_rel, dst_rel) = match dir {
            Dir::Forward => (row.from_path.clone(), row.to_path.clone()),
            Dir::Back if row.state == "staged" => (
                stage_name(row, &row.to_path, Dir::Forward),
                row.from_path.clone(),
            ),
            Dir::Back => (row.to_path.clone(), row.from_path.clone()),
        };
        let stage_rel = stage_name(row, &dst_rel, dir);
        Ok(Op {
            src: join_rel(&root, &src_rel)?,
            src_rel: src_rel.clone(),
            dst: join_rel(&root, &dst_rel)?,
            stage: join_rel(&root, &stage_rel)?,
            dst_rel,
            stage_rel,
            started: row.sha256.is_some(),
        })
    };

    // Files still waiting at their current place. A row whose target is one of them is part of
    // a chain or cycle: it is staged and placed in the second pass.
    let mut waiting: HashSet<(u64, u64)> = HashSet::new();
    for row in &first {
        if let Some((d, i, _)) = ident(&op_for(row)?.src) {
            waiting.insert((d, i));
        }
    }

    let mut newly_staged = Vec::new();
    for row in first.drain(..) {
        r.checkpoint()?;
        let op = op_for(&row)?;
        let src_id = ident(&op.src).map(|(d, i, _)| (d, i));
        let staged = move_one(r, &row, &op, dir, &waiting).await?;
        if let Some(id) = src_id {
            waiting.remove(&id);
        }
        if staged {
            let mut row = row;
            row.state = dir.staged_state().to_string();
            newly_staged.push(row);
        }
        r.row_finished();
    }
    // A staged row takes a second step; count it so `done` never passes `total`.
    r.progress.total += newly_staged.len();
    second.extend(newly_staged);
    for row in second {
        r.checkpoint()?;
        let op = op_for(&row)?;
        place_staged(r, &row, &op, dir).await?;
        r.row_finished();
    }
    Ok(())
}

/// Refuses a row: forward it is skipped; an undo leaves it `done` with the reason.
async fn refuse(r: &Runner<'_>, row: &JournalRow, dir: Dir, note: &str) -> Result<()> {
    match dir {
        Dir::Forward => r.commit(row, "skipped", Some(note), None).await,
        Dir::Back => {
            let state = if row.state == "staged" {
                "staged"
            } else {
                "done"
            };
            let note = format!("could not undo: {note}");
            r.commit(row, state, Some(&note), None).await
        }
    }
}

/// First pass for one row. Returns whether it ended up staged.
async fn move_one(
    r: &mut Runner<'_>,
    row: &JournalRow,
    op: &Op,
    dir: Dir,
    waiting: &HashSet<(u64, u64)>,
) -> Result<bool> {
    // An undo only moves a file the library still expects where the apply put it.
    if dir == Dir::Back
        && let Some(track) = row.track_id
        && db::track_path(&r.ctx.pool, track).await?.as_deref() != Some(&op.src_rel)
    {
        let note = "could not undo: the track moved since the apply";
        r.commit(row, "skipped", Some(note), None).await?;
        return Ok(false);
    }
    let case_only = op.src != op.dst && lower(&op.src) == lower(&op.dst);
    let known_sha = row.sha256.clone();
    // A stat; cheap enough to do here.
    let dst_waiting = ident(&op.dst).is_some_and(|(d, i, _)| waiting.contains(&(d, i)));
    let step = {
        let (src, dst, stage) = (op.src.clone(), op.dst.clone(), op.stage.clone());
        let started = op.started;
        blocking(move || {
            let src_ok = exists(&src);
            let dst_ok = exists(&dst);
            let matches = |p: &Path| known_sha.as_deref().is_some_and(|s| sha_matches(p, s));
            Ok(if !src_ok {
                if started && dst_ok && matches(&dst) {
                    Step::Commit
                } else if started && exists(&stage) && matches(&stage) {
                    Step::CommitStaged
                } else {
                    Step::Refuse("source file missing")
                }
            } else if started && exists(&stage) && hard_link_pair(&src, &stage) {
                Step::DropSourceStaged
            } else if started && exists(&stage) && matches(&stage) {
                // A finished copy under the staging name. The old name may only go if it
                // still holds what was copied.
                if matches(&src) {
                    Step::DropSourceStaged
                } else {
                    Step::Fail(SOURCE_CHANGED)
                }
            } else if case_only {
                Step::Stage
            } else if dst_ok {
                // An earlier attempt may have linked the file into place (or copied it across
                // filesystems) and died before dropping the old name. A target that is another
                // row's waiting file comes first: identical content must not mean "ours".
                if started && hard_link_pair(&src, &dst) {
                    Step::DropSource
                } else if dst_waiting {
                    Step::Stage
                } else if started && matches(&dst) {
                    if matches(&src) {
                        Step::DropSource
                    } else {
                        Step::Fail(SOURCE_CHANGED)
                    }
                } else {
                    Step::Refuse("target exists")
                }
            } else {
                Step::Direct
            })
        })
        .await?
    };

    let staged_state = dir.staged_state();
    let done_state = dir.done_state();
    match step {
        Step::Refuse(note) => {
            refuse(r, row, dir, note).await?;
            Ok(false)
        }
        Step::Fail(note) => {
            // An undo failure is its own state: apply-time `failed` rows are never undone, and
            // must not keep the run from reaching `undone`. A row still parked from the apply
            // stays `staged`, so a retry finds its file under the staging name.
            let state = match dir {
                Dir::Forward => "failed",
                Dir::Back if row.state == "staged" => "staged",
                Dir::Back => "undo_failed",
            };
            r.commit(row, state, Some(note), None).await?;
            Ok(false)
        }
        Step::Commit => {
            r.commit(row, done_state, None, Some(&op.dst_rel)).await?;
            r.tidy(&op.src, &r.source.clone()).await;
            Ok(false)
        }
        Step::CommitStaged => {
            r.commit(row, staged_state, None, Some(&op.stage_rel))
                .await?;
            Ok(true)
        }
        Step::DropSourceStaged => {
            r.hold(&[&op.src, &op.stage]);
            let src = op.src.clone();
            blocking(move || fs::remove_file(&src)).await?;
            r.crash_point()?;
            r.commit(row, staged_state, None, Some(&op.stage_rel))
                .await?;
            r.tidy(&op.src, &r.source.clone()).await;
            Ok(true)
        }
        Step::DropSource => {
            r.hold(&[&op.src, &op.dst]);
            let src = op.src.clone();
            blocking(move || fs::remove_file(&src)).await?;
            r.crash_point()?;
            r.commit(row, done_state, None, Some(&op.dst_rel)).await?;
            r.tidy(&op.src, &r.source.clone()).await;
            Ok(false)
        }
        Step::Stage | Step::Direct => {
            if dir == Dir::Forward
                && let Some(track) = row.track_id
            {
                let current = db::track_path(&r.ctx.pool, track).await?;
                if current.as_deref() != Some(&row.from_path) {
                    refuse(r, row, dir, "track changed since the preview").await?;
                    return Ok(false);
                }
            }
            let stage = matches!(step, Step::Stage);
            let (dest, dest_rel) = if stage {
                (op.stage.clone(), op.stage_rel.clone())
            } else {
                (op.dst.clone(), op.dst_rel.clone())
            };
            if !stage && r.track_conflict(row, &op.dst_rel).await? {
                refuse(r, row, dir, "another track is recorded at the target").await?;
                return Ok(false);
            }
            let (root, d) = (r.source.clone(), op.dst.clone());
            if blocking(move || Ok(symlinked_ancestor(&root, &d))).await? {
                refuse(r, row, dir, SYMLINK_NOTE).await?;
                return Ok(false);
            }
            let sha = r.sha_for(row, &op.src).await?;
            r.hold(&[&op.src, &op.dst, &op.stage, &part_path(&dest)]);
            let (src, d) = (op.src.clone(), dest.clone());
            let force = r.hooks.force_cross_fs;
            let hook = r.hooks.after_copy.clone();
            let moved =
                blocking_io(move || move_file(&src, &d, &sha, force, hook.as_deref())).await?;
            match moved {
                Ok(Copied::Placed) => {}
                Ok(Copied::Mismatch) => {
                    match dir {
                        Dir::Forward => {
                            r.commit(row, "failed", Some("the copy did not verify"), None)
                                .await?
                        }
                        Dir::Back => refuse(r, row, dir, "the copy did not verify").await?,
                    }
                    return Ok(false);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    refuse(r, row, dir, "target exists").await?;
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            }
            r.crash_point()?;
            let state = if stage { staged_state } else { done_state };
            r.commit(row, state, None, Some(&dest_rel)).await?;
            r.tidy(&op.src, &r.source.clone()).await;
            Ok(stage)
        }
    }
}

/// Second pass: a staged file takes its final name.
async fn place_staged(r: &mut Runner<'_>, row: &JournalRow, op: &Op, dir: Dir) -> Result<()> {
    let sha = row.sha256.clone().unwrap_or_default();
    let (stage, dst) = (op.stage.clone(), op.dst.clone());
    let sha2 = sha.clone();
    let (stage_ok, dst_ok, dst_matches) = blocking(move || {
        let dst_ok = exists(&dst);
        Ok((exists(&stage), dst_ok, dst_ok && sha_matches(&dst, &sha2)))
    })
    .await?;
    if !stage_ok {
        if dst_matches {
            r.commit(row, dir.done_state(), None, Some(&op.dst_rel))
                .await?;
        } else {
            let note = "the staged file is missing";
            r.commit(row, &row.state, Some(note), None).await?;
        }
        return Ok(());
    }
    let (root, d) = (r.source.clone(), op.dst.clone());
    let symlinked = blocking(move || Ok(symlinked_ancestor(&root, &d))).await?;
    if dst_ok || symlinked || r.track_conflict(row, &op.dst_rel).await? {
        return park(r, row, op, dir).await;
    }
    r.hold(&[&op.stage, &op.dst]);
    let (s, d) = (op.stage.clone(), op.dst.clone());
    match blocking(move || place_no_replace(&s, &d)).await {
        Ok(()) => {}
        Err(OrganizeError::Io(e)) => {
            let note = format!("could not place the staged file: {e}");
            r.commit(row, &row.state, Some(&note), None).await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    }
    r.crash_point()?;
    r.commit(row, dir.done_state(), None, Some(&op.dst_rel))
        .await?;
    Ok(())
}

/// The target of a staged file was taken after all. The file goes back where it came from
/// when that place is free, and the track with it. Otherwise it stays under its staging name
/// (the row stays staged, the run ends failed, and scans do not flag the track missing).
async fn park(r: &mut Runner<'_>, row: &JournalRow, op: &Op, dir: Dir) -> Result<()> {
    // Back where it came from: the source of a forward row, the applied place of an undo.
    let home = match dir {
        Dir::Forward => true,
        Dir::Back => op.src_rel == row.to_path,
    };
    let (root, src) = (r.source.clone(), op.src.clone());
    let free = home
        && blocking(move || Ok(!exists(&src) && !symlinked_ancestor(&root, &src))).await?
        && !r.track_conflict(row, &op.src_rel).await?;
    if free {
        r.hold(&[&op.stage, &op.src]);
        let (s, d) = (op.stage.clone(), op.src.clone());
        let back = blocking_io(move || {
            if let Some(parent) = d.parent() {
                fs::create_dir_all(parent)?;
            }
            place_no_replace(&s, &d)
        })
        .await?;
        if back.is_ok() {
            r.crash_point()?;
            let (state, note) = match dir {
                Dir::Forward => ("skipped", "target exists; the file was left where it was"),
                Dir::Back => ("done", "could not undo: the original path is taken"),
            };
            return r.commit(row, state, Some(note), Some(&op.src_rel)).await;
        }
    }
    let note = format!("target exists; the file is parked at {}", op.stage_rel);
    r.commit(row, &row.state, Some(&note), None).await
}

/// Ends an apply or undo: `done` / `undone`, or `failed` when rows are left mid-way.
async fn finish(r: &Runner<'_>, dir: Dir) -> Result<()> {
    let counts = db::state_counts(&r.ctx.pool, r.run.id).await?;
    let count = |s: &str| counts.iter().find(|(k, _)| k == s).map_or(0, |(_, n)| *n);
    let mut summary = serde_json::Map::new();
    for (k, n) in &counts {
        summary.insert(k.clone(), json!(n));
    }
    db::set_summary(&r.ctx.pool, r.run.id, &json!({ "rows": summary })).await?;
    match dir {
        Dir::Forward => {
            let parked = count("staged");
            if parked > 0 {
                db::fail_run(
                    &r.ctx.pool,
                    r.run.id,
                    &format!("{parked} files are parked under a temporary name; see the journal"),
                )
                .await?;
            } else {
                db::transition(&r.ctx.pool, r.run.id, &["applying"], "done").await?;
            }
            info!(run = %r.run.id, done = count("done"), skipped = count("skipped"),
                  failed = count("failed"), "organizer apply finished");
        }
        Dir::Back => {
            let left =
                count("done") + count("staged") + count("undo_staged") + count("undo_failed");
            if left > 0 {
                db::fail_run(
                    &r.ctx.pool,
                    r.run.id,
                    &format!("{left} files could not be restored; see the journal"),
                )
                .await?;
            } else {
                db::transition(&r.ctx.pool, r.run.id, &["undoing"], "undone").await?;
            }
            info!(run = %r.run.id, undone = count("undone"), left, "organizer undo finished");
        }
    }
    // A safety net: anything the run could not account for is picked up by a scan.
    jobs::enqueue_reconcile(&r.ctx.pool).await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Undo

async fn undo(ctx: &LibraryCtx, run: Run, hooks: &Hooks, cancel: &CancellationToken) -> Result<()> {
    if run.state != "undoing" {
        return Err(OrganizeError::Invalid(format!(
            "run is {}, not undoing",
            run.state
        )));
    }
    if let Some(reason) = undo_refusal(ctx, &run) {
        db::revert_undo(&ctx.pool, run.id, &reason).await?;
        return Err(OrganizeError::Refused(reason));
    }
    let mut r = Runner::new(ctx, run, hooks, cancel);
    let res = if r.is_move() {
        move_rows(&mut r, Dir::Back).await
    } else {
        uncopy_rows(&mut r).await
    };
    r.release();
    res?;
    r.send_progress(true);
    finish(&r, Dir::Back).await
}

/// Why an undo of `run` may not start, if it may not.
fn undo_refusal(ctx: &LibraryCtx, run: &Run) -> Option<String> {
    if run.switched_at.is_some() {
        return Some("the library was switched to this copy; it cannot be undone".into());
    }
    let current = ctx.root.get();
    if run.mode == db::MODE_MOVE && current != Path::new(&run.source_root) {
        return Some(format!(
            "the library root changed since this run ({} -> {})",
            run.source_root,
            current.display()
        ));
    }
    let target = Path::new(&run.target_root);
    if run.mode == db::MODE_COPY && (current.starts_with(target) || target.starts_with(&current)) {
        return Some("the library uses this copy; it cannot be undone".into());
    }
    None
}

/// Copy undo: deletes each copy whose checksum still matches, newest first.
async fn uncopy_rows(r: &mut Runner<'_>) -> Result<()> {
    let mut rows: Vec<JournalRow> = db::all_rows(&r.ctx.pool, r.run.id)
        .await?
        .into_iter()
        .filter(|row| row.state == "done")
        .collect();
    rows.sort_by_key(|x| std::cmp::Reverse((x.applied_seq.unwrap_or(i64::MAX), x.id)));
    r.progress.total = rows.len();
    r.send_progress(true);
    for row in rows {
        r.checkpoint()?;
        let dst = join_rel(&r.target, &row.to_path)?;
        let sha = row.sha256.clone().unwrap_or_default();
        let (d, root) = (dst.clone(), r.target.clone());
        let outcome = blocking(move || {
            if symlinked_ancestor(&root, &d) {
                return Ok("symlink");
            }
            if !exists(&d) {
                return Ok("gone");
            }
            if sha.is_empty() || !sha_matches(&d, &sha) {
                return Ok("changed");
            }
            fs::remove_file(&d)?;
            Ok("removed")
        })
        .await?;
        if outcome == "removed" {
            r.crash_point()?;
        }
        match outcome {
            "changed" => {
                r.commit(
                    &row,
                    "skipped",
                    Some("the copy changed after it was made; kept"),
                    None,
                )
                .await?
            }
            "gone" => {
                r.commit(&row, "undone", Some("the copy was already gone"), None)
                    .await?
            }
            "symlink" => r.commit(&row, "skipped", Some(SYMLINK_NOTE), None).await?,
            _ => r.commit(&row, "undone", None, None).await?,
        }
        let target = r.target.clone();
        if let Some(parent) = dst.parent().map(Path::to_path_buf) {
            let _ = blocking(move || Ok(remove_empty_dirs(&parent, &target))).await;
        }
        r.row_finished();
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Switch root

/// Copy runs only, once `done`: tracks take their copied paths, the library root becomes the
/// target (stored in system settings and switched in the running process, watcher included),
/// and a reconcile is queued.
async fn switch_root(ctx: &LibraryCtx, run: Run) -> Result<()> {
    if run.mode != db::MODE_COPY || run.state != "done" {
        return Err(OrganizeError::Invalid(
            "only a finished copy run can become the library".into(),
        ));
    }
    if run.switched_at.is_some() {
        return Ok(());
    }
    let left = db::tracks_not_done(&ctx.pool, run.id).await?;
    if left > 0 {
        return Err(OrganizeError::Invalid(format!(
            "{left} tracks were not copied; switching would leave them missing"
        )));
    }
    let moved = db::switch_root(&ctx.pool, run.id, &run.target_root).await?;
    ctx.root.set(PathBuf::from(&run.target_root));
    jobs::enqueue_reconcile(&ctx.pool).await?;
    info!(run = %run.id, root = %run.target_root, tracks = moved, "library switched to the organized copy");
    Ok(())
}

/// Whether `dir` accepts a new file (a probe file is created and removed). Mode bits say
/// nothing about read-only mounts.
pub fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".kima-write-probe-{}", Uuid::now_v7()));
    match File::create_new(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, b"one").unwrap();
        fs::write(&b, b"two").unwrap();
        let err = place_no_replace(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&a).unwrap(), b"one");
        assert_eq!(fs::read(&b).unwrap(), b"two");
    }

    #[test]
    fn rename_noreplace_refuses_an_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        fs::write(&a, b"one").unwrap();
        fs::write(&b, b"two").unwrap();
        if rename_noreplace(&a, &c).unwrap() {
            assert_eq!(fs::read(&c).unwrap(), b"one");
            let err = rename_noreplace(&c, &b).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(&b).unwrap(), b"two");
        }
    }

    #[test]
    fn symlinked_ancestors_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("real/sub")).unwrap();
        assert!(!symlinked_ancestor(root, &root.join("real/sub/x.flac")));
        assert!(!symlinked_ancestor(root, &root.join("new/dir/x.flac")));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
            assert!(symlinked_ancestor(root, &root.join("link/sub/x.flac")));
        }
        assert!(symlinked_ancestor(root, Path::new("/elsewhere/x.flac")));
    }

    #[test]
    fn empty_dirs_go_up_to_the_root_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("a/keep.txt"), b"x").unwrap();
        let removed = remove_empty_dirs(&root.join("a/b/c"), root);
        assert_eq!(removed, vec![root.join("a/b/c"), root.join("a/b")]);
        assert!(root.join("a").is_dir());
        fs::remove_file(root.join("a/keep.txt")).unwrap();
        remove_empty_dirs(&root.join("a"), root);
        assert!(root.is_dir());
    }

    #[test]
    fn stage_names_are_hidden_and_in_the_target_folder() {
        let row = JournalRow {
            id: 7,
            run_id: Uuid::nil(),
            track_id: None,
            from_path: "x".into(),
            to_path: "A/B/01 - T.flac".into(),
            sha256: None,
            state: "planned".into(),
            note: None,
            collision: false,
            applied_seq: None,
        };
        assert_eq!(
            stage_name(&row, "A/B/01 - T.flac", Dir::Forward),
            "A/B/.kima-stage-7-01 - T.flac"
        );
        assert_eq!(
            stage_name(&row, "t.flac", Dir::Back),
            ".kima-unstage-7-t.flac"
        );
    }
}
