//! Watches the library folder and queues small `library` jobs for what changed (spec §4.4).
//!
//! `notify` events are debounced twice. `notify_debouncer_full` coalesces bursts and stitches
//! renames together. Then a settle step holds every path until it has been quiet for the
//! debounce time and its size and mtime have stopped moving, so a file still being copied in
//! is read once it is complete, not halfway. A watcher error or overflow queues a reconcile.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use kima_core::config::WatchMode;
use notify_debouncer_full::notify::event::ModifyKind;
use notify_debouncer_full::notify::{
    self, Config, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode,
};
use notify_debouncer_full::{
    DebounceEventResult, DebouncedEvent, RecommendedCache, new_debouncer_opt,
};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::jobs;
use crate::scan::LibraryCtx;
use crate::tags;

/// How long a path must be quiet before its job is queued.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(2);
/// How often an unavailable library folder is tried again.
pub const DEFAULT_RETRY: Duration = Duration::from_secs(30);

/// What the settings page shows about the watcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchStatus {
    /// The mode in effect: `auto`, `poll` or `off`. `auto` that fell back to polling says `poll`.
    pub mode: String,
    pub active: bool,
    pub last_error: Option<String>,
}

impl WatchStatus {
    pub fn new(mode: WatchMode) -> Self {
        Self {
            mode: mode_name(mode).to_string(),
            active: false,
            last_error: None,
        }
    }
}

impl Default for WatchStatus {
    fn default() -> Self {
        Self::new(WatchMode::Off)
    }
}

pub fn mode_name(mode: WatchMode) -> &'static str {
    match mode {
        WatchMode::Auto => "auto",
        WatchMode::Poll => "poll",
        WatchMode::Off => "off",
    }
}

pub type SharedStatus = Arc<RwLock<WatchStatus>>;

fn set_status(status: &SharedStatus, f: impl FnOnce(&mut WatchStatus)) {
    if let Ok(mut s) = status.write() {
        f(&mut s);
    }
}

/// Paths kima itself is changing (the organizer's moves). Events for them, or for anything
/// below them, are dropped. Cheap to clone; all clones share one set.
#[derive(Clone, Default)]
pub struct IgnoreSet(Arc<Mutex<HashMap<PathBuf, Option<Instant>>>>);

impl IgnoreSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Absolute paths (under the library root), as the watcher sees them. Held until
    /// [`remove`](Self::remove)d or [`release`](Self::release)d.
    pub fn add(&self, path: impl Into<PathBuf>) {
        if let Ok(mut set) = self.0.lock() {
            set.insert(path.into(), None);
        }
    }

    pub fn remove(&self, path: &Path) {
        if let Ok(mut set) = self.0.lock() {
            set.remove(path);
        }
    }

    /// Keeps ignoring `path` for `grace` more, then forgets it. File events reach the watcher
    /// only after its debounce, so a path removed right after kima touched it would still
    /// trigger an update.
    pub fn release(&self, path: &Path, grace: Duration) {
        if let Ok(mut set) = self.0.lock()
            && let Some(until) = set.get_mut(path)
        {
            *until = Some(Instant::now() + grace);
        }
    }

    /// `path` is in the set, or below a folder that is.
    pub fn contains(&self, path: &Path) -> bool {
        let now = Instant::now();
        self.0
            .lock()
            .map(|mut set| {
                set.retain(|_, until| until.is_none_or(|t| t > now));
                set.keys().any(|p| path.starts_with(p))
            })
            .unwrap_or(false)
    }
}

/// A running watcher. Dropping it does not stop it; cancel the shutdown token.
pub struct WatchHandle {
    status: SharedStatus,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl WatchHandle {
    pub fn status(&self) -> WatchStatus {
        self.status.read().map(|s| s.clone()).unwrap_or_default()
    }

    /// Waits for the watcher task to end (after the shutdown token fired).
    pub async fn join(mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// Starts watching `ctx.root` (and follows it when it is switched) per `mode`, with the production debounce. `status` is shared
/// with whoever serves the watch-status endpoint.
pub fn spawn(
    ctx: LibraryCtx,
    mode: WatchMode,
    poll_secs: u64,
    ignore: IgnoreSet,
    status: SharedStatus,
    shutdown: CancellationToken,
) -> WatchHandle {
    spawn_with(
        ctx,
        mode,
        Duration::from_secs(poll_secs.max(1)),
        DEFAULT_DEBOUNCE,
        DEFAULT_RETRY,
        ignore,
        status,
        shutdown,
    )
}

/// [`spawn`] with an explicit poll interval, debounce and retry interval (tests use short
/// ones). While the library folder cannot be watched (an unmounted disk), the task keeps
/// running and tries again every `retry`.
#[allow(clippy::too_many_arguments)]
pub fn spawn_with(
    ctx: LibraryCtx,
    mode: WatchMode,
    poll_interval: Duration,
    debounce: Duration,
    retry: Duration,
    ignore: IgnoreSet,
    status: SharedStatus,
    shutdown: CancellationToken,
) -> WatchHandle {
    set_status(&status, |s| *s = WatchStatus::new(mode));
    if mode == WatchMode::Off {
        return WatchHandle { status, task: None };
    }

    let (tx, rx) = mpsc::unbounded_channel::<DebounceEventResult>();
    let watcher = Watcher {
        mode,
        poll_interval,
        debounce,
        retry,
        tx,
    };
    let guard = watcher.try_start(&ctx.root.get(), &status);
    let task = tokio::spawn(run(
        ctx,
        rx,
        ignore,
        status.clone(),
        watcher,
        shutdown,
        guard,
    ));
    WatchHandle {
        status,
        task: Some(task),
    }
}

/// What it takes to (re)start the debouncer.
struct Watcher {
    mode: WatchMode,
    poll_interval: Duration,
    debounce: Duration,
    retry: Duration,
    tx: mpsc::UnboundedSender<DebounceEventResult>,
}

impl Watcher {
    /// Starts the debouncer and records the outcome in `status`.
    fn try_start(&self, root: &Path, status: &SharedStatus) -> Option<Box<dyn Any + Send>> {
        match start_debouncer(
            root,
            self.mode,
            self.poll_interval,
            self.debounce / 2,
            self.tx.clone(),
        ) {
            Ok((guard, effective, note)) => {
                set_status(status, |s| {
                    s.mode = mode_name(effective).to_string();
                    s.active = true;
                    s.last_error = note;
                });
                info!(root = %root.display(), mode = mode_name(effective), "watching the library");
                Some(guard)
            }
            Err(message) => {
                warn!(root = %root.display(), error = %message, "cannot watch the library; will retry");
                set_status(status, |s| {
                    s.active = false;
                    s.last_error = Some(message);
                });
                None
            }
        }
    }
}

const INOTIFY_HINT: &str = "the inotify watch limit was reached; raise fs.inotify.max_user_watches \
(sysctl fs.inotify.max_user_watches=524288) or set KIMA_WATCH_MODE=poll";

fn describe(err: &notify::Error) -> String {
    let limit = matches!(err.kind, notify::ErrorKind::MaxFilesWatch)
        || matches!(&err.kind, notify::ErrorKind::Io(e) if e.raw_os_error() == Some(28));
    if limit {
        format!("{INOTIFY_HINT} ({err})")
    } else {
        err.to_string()
    }
}

type Started = (Box<dyn Any + Send>, WatchMode, Option<String>);

/// Builds the debouncer. Returns what keeps it alive, the mode really in use, and a note when
/// `auto` had to fall back to polling.
fn start_debouncer(
    root: &Path,
    mode: WatchMode,
    poll_interval: Duration,
    timeout: Duration,
    tx: mpsc::UnboundedSender<DebounceEventResult>,
) -> Result<Started, String> {
    if !root.is_dir() {
        return Err(format!(
            "the library folder {} does not exist or is not a folder",
            root.display()
        ));
    }
    let timeout = timeout.max(Duration::from_millis(50));
    let tick = Some(timeout / 4);
    let poll = |tx: mpsc::UnboundedSender<DebounceEventResult>| -> Result<Started, String> {
        let config = Config::default().with_poll_interval(poll_interval);
        let mut deb = new_debouncer_opt::<_, PollWatcher, RecommendedCache>(
            timeout,
            tick,
            move |res| {
                let _ = tx.send(res);
            },
            RecommendedCache::new(),
            config,
        )
        .map_err(|e| describe(&e))?;
        deb.watch(root, RecursiveMode::Recursive)
            .map_err(|e| describe(&e))?;
        Ok((Box::new(deb), WatchMode::Poll, None))
    };
    if mode == WatchMode::Poll {
        return poll(tx);
    }
    let native = {
        let tx = tx.clone();
        let mut deb = new_debouncer_opt::<_, RecommendedWatcher, RecommendedCache>(
            timeout,
            tick,
            move |res| {
                let _ = tx.send(res);
            },
            RecommendedCache::new(),
            Config::default(),
        )
        .map_err(|e| describe(&e))?;
        deb.watch(root, RecursiveMode::Recursive)
            .map_err(|e| describe(&e))
            .map(|()| deb)
    };
    match native {
        Ok(deb) => Ok((Box::new(deb), WatchMode::Auto, None)),
        Err(reason) => {
            warn!(%reason, "native file watching failed; falling back to polling");
            let (guard, effective, _) = poll(tx)?;
            Ok((
                guard,
                effective,
                Some(format!("native watching failed, polling instead: {reason}")),
            ))
        }
    }
}

/// A path waiting to settle.
struct Pending {
    last_event: Instant,
    /// Size and mtime when the last event arrived; `None` when the file is not there.
    stat: Option<(u64, SystemTime)>,
}

fn stat(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

async fn run(
    ctx: LibraryCtx,
    mut rx: mpsc::UnboundedReceiver<DebounceEventResult>,
    ignore: IgnoreSet,
    status: SharedStatus,
    watcher: Watcher,
    shutdown: CancellationToken,
    mut guard: Option<Box<dyn Any + Send>>,
) {
    let debounce = watcher.debounce;
    let mut pending: HashMap<String, Pending> = HashMap::new();
    // The root can be switched at runtime (to an organized copy); the watch follows it.
    let mut root_rx = ctx.root.subscribe();
    let mut root = root_rx.borrow_and_update().clone();
    let mut root_id = identity(&root);
    let mut tick = tokio::time::interval((debounce / 4).max(Duration::from_millis(25)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let retry = watcher.retry.max(Duration::from_millis(10));
    let mut retry_tick = tokio::time::interval_at(tokio::time::Instant::now() + retry, retry);
    retry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            Ok(()) = root_rx.changed() => {
                let new_root = root_rx.borrow_and_update().clone();
                if new_root != root {
                    info!(from = %root.display(), to = %new_root.display(), "library root switched; watching the new one");
                    // Stop the old watch first; its queued events no longer map to the root.
                    drop(guard.take());
                    pending.clear();
                    root = new_root;
                    guard = watcher.try_start(&root, &status);
                    root_id = identity(&root);
                }
            }
            _ = retry_tick.tick() => {
                if guard.is_some() && identity(&root) != root_id {
                    // Unmounted, or unmounted and mounted again: the watch is dead even
                    // though the watcher does not say so.
                    warn!(root = %root.display(), "library folder became unavailable");
                    guard = None;
                    set_status(&status, |s| {
                        s.active = false;
                        s.last_error = Some("library folder became unavailable".to_string());
                    });
                } else if guard.is_none() {
                    guard = watcher.try_start(&root, &status);
                    if guard.is_some() {
                        root_id = identity(&root);
                        // Changes made while the folder was unavailable were not seen.
                        queue_reconcile(&ctx).await;
                    }
                }
            }
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                match msg {
                    Ok(events) => {
                        if collect(&ctx, &ignore, &status, events, &mut pending).await {
                            warn!("library folder removed; restarting the watcher");
                            guard = None;
                            set_status(&status, |s| {
                                s.active = false;
                                s.last_error =
                                    Some("library folder became unavailable".to_string());
                            });
                        }
                    }
                    Err(errors) => {
                        let text = errors.iter().map(describe).collect::<Vec<_>>().join("; ");
                        warn!(error = %text, "library watcher error; restarting it");
                        // Restarted on the next retry tick, which also queues a reconcile.
                        guard = None;
                        set_status(&status, |s| {
                            s.active = false;
                            s.last_error = Some(text);
                        });
                        queue_reconcile(&ctx).await;
                    }
                }
            }
            _ = tick.tick() => flush(&ctx, &status, debounce, &mut pending).await,
        }
    }
    drop(guard);
    set_status(&status, |s| s.active = false);
}

async fn queue_reconcile(ctx: &LibraryCtx) {
    if let Err(e) = jobs::enqueue_reconcile_followup(&ctx.pool).await {
        warn!(error = %e, "could not queue a library reconcile");
    }
}

/// The path relative to the root as `a/b/c`, or `None` when it is outside the root, is the
/// root itself, or has a hidden component.
fn rel_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str()?;
                if s.starts_with('.') {
                    return None;
                }
                parts.push(s);
            }
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

async fn collect(
    ctx: &LibraryCtx,
    ignore: &IgnoreSet,
    status: &SharedStatus,
    events: Vec<DebouncedEvent>,
    pending: &mut HashMap<String, Pending>,
) -> bool {
    let root = ctx.root.get();
    let mut reconcile = false;
    let mut root_gone = false;
    // Path -> whether a folder at that path should be expanded to its files.
    let mut paths: HashMap<PathBuf, bool> = HashMap::new();
    for event in events {
        if event.need_rescan() {
            reconcile = true;
            set_status(status, |s| {
                s.last_error = Some("the file event queue overflowed; reconciling".to_string());
            });
            continue;
        }
        if matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        ) && event.paths.contains(&root)
        {
            root_gone = true;
        }
        // A folder's own mtime changes whenever a file in it does; only a folder that
        // appeared, was renamed or went away stands for the files in it.
        let folder_counts = match event.kind {
            EventKind::Create(_) | EventKind::Remove(_) => Some(true),
            EventKind::Modify(ModifyKind::Name(_)) => Some(true),
            EventKind::Modify(_) => Some(false),
            EventKind::Access(_) | EventKind::Any | EventKind::Other => None,
        };
        // `None`: reads of a file and events the backend cannot classify change nothing.
        if let Some(expand_dirs) = folder_counts {
            for p in &event.paths {
                *paths.entry(p.clone()).or_default() |= expand_dirs;
            }
        }
    }
    if reconcile {
        warn!("library watcher overflow; queueing a reconcile");
        queue_reconcile(ctx).await;
    }
    for (path, expand_dirs) in paths {
        if ignore.contains(&path) {
            continue;
        }
        let Some(rel) = rel_of(&root, &path) else {
            continue;
        };
        for rel in expand(ctx, &path, rel, expand_dirs).await {
            // A folder that appeared stands for its files; the organizer may be placing some.
            if ignore.contains(&root.join(&rel)) {
                continue;
            }
            let stat = stat(&root.join(&rel));
            pending.insert(
                rel,
                Pending {
                    last_event: Instant::now(),
                    stat,
                },
            );
        }
    }
    root_gone
}

/// (device, inode) of the library folder; it changes when a disk is unmounted and mounted again.
#[cfg(unix)]
fn identity(root: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(root).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// Without device and inode numbers, only a missing folder is detected, not a replaced one.
#[cfg(not(unix))]
fn identity(root: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(root).ok().map(|_| (0, 0))
}

/// The audio files one event stands for: the file itself, every audio file under a folder
/// that appeared, or every indexed track under a folder that vanished.
async fn expand(ctx: &LibraryCtx, path: &Path, rel: String, expand_dirs: bool) -> Vec<String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() && !expand_dirs => Vec::new(),
        Ok(meta) if meta.is_dir() => {
            let root = ctx.root.get();
            let dir = path.to_path_buf();
            tokio::task::spawn_blocking(move || {
                walkdir::WalkDir::new(&dir)
                    .into_iter()
                    .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_file() && tags::is_audio(e.path()))
                    .filter_map(|e| rel_of(&root, e.path()))
                    .collect()
            })
            .await
            .unwrap_or_default()
        }
        Ok(_) => {
            if tags::is_audio(Path::new(&rel)) {
                vec![rel]
            } else {
                Vec::new()
            }
        }
        Err(_) => {
            // Gone. An audio file is one job; anything else may have been a folder.
            if tags::is_audio(Path::new(&rel)) {
                return vec![rel];
            }
            match kima_db::library::paths_under(&ctx.pool, &format!("{rel}/")).await {
                Ok(paths) => paths,
                Err(e) => {
                    warn!(error = %e, %rel, "could not look up the files under a removed folder");
                    queue_reconcile(ctx).await;
                    Vec::new()
                }
            }
        }
    }
}

async fn flush(
    ctx: &LibraryCtx,
    status: &SharedStatus,
    debounce: Duration,
    pending: &mut HashMap<String, Pending>,
) {
    let now = Instant::now();
    let mut due: Vec<String> = pending
        .iter()
        .filter(|(_, p)| now.duration_since(p.last_event) >= debounce)
        .map(|(rel, _)| rel.clone())
        .collect();
    // Vanished paths go first. A moved file is re-pointed by its fingerprint only while its old
    // row is flagged missing, so the old place must be handled before the new one.
    due.sort_by_key(|rel| (stat(&ctx.root.get().join(rel)).is_some(), rel.clone()));
    for rel in due {
        let current = stat(&ctx.root.get().join(&rel));
        let Some(p) = pending.get_mut(&rel) else {
            continue;
        };
        if p.stat != current {
            // Still growing (a copy that emits no events, as on network shares): wait more.
            p.stat = current;
            p.last_event = now;
            continue;
        }
        match jobs::enqueue_path(&ctx.pool, &rel).await {
            Ok(_) => {
                debug!(%rel, "queued a library file update");
                pending.remove(&rel);
            }
            Err(e) => {
                warn!(error = %e, %rel, "could not queue a library file update");
                set_status(status, |s| {
                    s.last_error = Some(format!("queueing {rel}: {e}"))
                });
                // Keep it; the next tick retries.
                p.last_event = now;
            }
        }
    }
}
