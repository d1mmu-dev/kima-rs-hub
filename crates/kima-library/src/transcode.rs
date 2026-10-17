//! Cached mp3 transcoding via ffmpeg.
//!
//! One ffmpeg process runs per cache key (track, quality, source size and mtime). Concurrent
//! requests for the same key join it through an in-process map. Output is written to a uniquely
//! named `.part` file and renamed to `<key>.mp3` on success, so a cache entry is never
//! half-written. A global semaphore bounds how many ffmpeg processes run at once, and every run
//! has a deadline.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use lofty::file::AudioFile;
use lofty::probe::Probe;
use tempfile::NamedTempFile;
use tokio::process::Command;
use tokio::sync::{Semaphore, watch};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Stream quality: `original` is served as-is, the rest are mp3 via libmp3lame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Original,
    High,
    Medium,
    Low,
}

impl Quality {
    pub fn parse(s: &str) -> Option<Quality> {
        match s {
            "original" => Some(Quality::Original),
            "high" => Some(Quality::High),
            "medium" => Some(Quality::Medium),
            "low" => Some(Quality::Low),
            _ => None,
        }
    }

    pub fn bitrate_kbps(self) -> Option<u32> {
        match self {
            Quality::Original => None,
            Quality::High => Some(320),
            Quality::Medium => Some(192),
            Quality::Low => Some(128),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Quality::Original => "original",
            Quality::High => "high",
            Quality::Medium => "medium",
            Quality::Low => "low",
        }
    }
}

/// Progress of one transcode, as seen through `InProgress::done`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscodeState {
    Running,
    Done,
    /// ffmpeg failed, timed out or could not start; carries the reason. The partial file is
    /// already removed when this is observed.
    Failed(String),
}

#[derive(Debug)]
pub enum TranscodeOutput {
    /// A finished file (the source itself for `Quality::Original`).
    Cached(PathBuf),
    /// A transcode that is still running. `path` is the growing `.part` file.
    ///
    /// Reader protocol (what the streaming handler must do):
    /// - Open `path` right away. `NotFound` means the transcode finished (or failed) in the
    ///   meantime: call `get` again.
    /// - Read from the same file handle. At EOF while `done` is `Running`, wait (poll or
    ///   `done.changed()`) and keep reading from the same handle; after `Done`, drain to EOF
    ///   and finish. The rename on success does not disturb an open handle.
    /// - On `Failed`, abort the response (the data read so far is not a valid file).
    /// - There is no Content-Length and no range support: the final size is not known.
    /// - ffmpeg may rewrite the Xing/Info header frame at offset 0 when it finishes, so bytes
    ///   already sent from the start of the file can differ from the final file in that
    ///   region. Players tolerate this.
    InProgress {
        path: PathBuf,
        done: watch::Receiver<TranscodeState>,
    },
}

/// Tunables; the defaults are what the server uses. The rest exist for tests.
#[derive(Clone, Default)]
pub struct TranscodeOptions {
    /// The ffmpeg program (default `ffmpeg`).
    pub ffmpeg: Option<PathBuf>,
    /// Overrides the computed deadline of a run.
    pub timeout: Option<Duration>,
    /// Overrides the concurrent-process limit (default: half the CPUs, at least 1).
    pub max_concurrent: Option<usize>,
    /// Test hook: each transcode task takes (and keeps) a permit from this semaphore before it
    /// does anything, so a test can hold transcodes back until it releases permits.
    pub start_gate: Option<Arc<Semaphore>>,
}

struct Running {
    path: PathBuf,
    done: watch::Receiver<TranscodeState>,
}

struct Inner {
    dir: PathBuf,
    max_bytes: u64,
    ffmpeg: PathBuf,
    timeout: Option<Duration>,
    start_gate: Option<Arc<Semaphore>>,
    permits: Semaphore,
    running: Mutex<HashMap<String, Running>>,
    evict_lock: Mutex<()>,
    spawned: AtomicUsize,
}

#[derive(Clone)]
pub struct Transcoder {
    inner: Arc<Inner>,
}

/// Owns the cleanup of one transcode. When it drops (normally, on failure, on a timeout or while
/// unwinding from a panic) it deletes the `.part` file first, then removes the map entry, then
/// publishes the final state, so an observer of `Failed` never sees a leftover file.
struct Guard {
    inner: Arc<Inner>,
    key: String,
    tx: watch::Sender<TranscodeState>,
    part: Option<NamedTempFile>,
    state: Option<TranscodeState>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        drop(self.part.take());
        self.inner
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        let state = self
            .state
            .take()
            .unwrap_or_else(|| TranscodeState::Failed("transcode aborted".into()));
        self.tx.send_replace(state);
    }
}

enum Plan {
    Join(TranscodeOutput),
    Cached,
    Start,
}

impl Transcoder {
    /// Creates the cache. Leftover `.part` files from an earlier run are removed.
    pub fn new(cache_dir: impl Into<PathBuf>, max_bytes: u64) -> Transcoder {
        Self::with_options(cache_dir, max_bytes, TranscodeOptions::default())
    }

    pub fn with_options(
        cache_dir: impl Into<PathBuf>,
        max_bytes: u64,
        options: TranscodeOptions,
    ) -> Transcoder {
        let dir = cache_dir.into();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(".part") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let limit = options.max_concurrent.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get() / 2)
                .unwrap_or(1)
        });
        Transcoder {
            inner: Arc::new(Inner {
                dir,
                max_bytes,
                ffmpeg: options.ffmpeg.unwrap_or_else(|| PathBuf::from("ffmpeg")),
                timeout: options.timeout,
                start_gate: options.start_gate,
                permits: Semaphore::new(limit.max(1)),
                running: Mutex::new(HashMap::new()),
                evict_lock: Mutex::new(()),
                spawned: AtomicUsize::new(0),
            }),
        }
    }

    pub fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// How many ffmpeg runs this transcoder has registered (a test hook).
    pub fn spawn_count(&self) -> usize {
        self.inner.spawned.load(Ordering::SeqCst)
    }

    pub async fn get(
        &self,
        track_id: Uuid,
        source: &Path,
        quality: Quality,
    ) -> Result<TranscodeOutput, TranscodeError> {
        let Some(kbps) = quality.bitrate_kbps() else {
            return Ok(TranscodeOutput::Cached(source.to_path_buf()));
        };
        let meta = tokio::fs::metadata(source).await?;
        let mtime = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let key = format!("{track_id}-{}-{}-{mtime}", quality.name(), meta.len());
        let final_path = self.inner.dir.join(format!("{key}.mp3"));

        // The lock only guards the map (plus one stat that closes the "finished just now" race);
        // file creation and the touch happen outside it.
        let plan = self.plan(&key, &final_path, None);
        match plan {
            Plan::Join(out) => return Ok(out),
            Plan::Cached => {
                touch(&final_path);
                return Ok(TranscodeOutput::Cached(final_path));
            }
            Plan::Start => {}
        }
        std::fs::create_dir_all(&self.inner.dir)?;
        let part = tempfile::Builder::new()
            .prefix(&format!("{key}-"))
            .suffix(".part")
            .tempfile_in(&self.inner.dir)?;
        let (tx, rx) = watch::channel(TranscodeState::Running);
        let part_path = part.path().to_path_buf();
        match self.plan(&key, &final_path, Some((&part_path, rx))) {
            Plan::Join(out) => return Ok(out), // `part` drops (and is deleted) here
            Plan::Cached => {
                touch(&final_path);
                return Ok(TranscodeOutput::Cached(final_path));
            }
            Plan::Start => {}
        }
        self.inner.spawned.fetch_add(1, Ordering::SeqCst);

        let out = TranscodeOutput::InProgress {
            path: part_path,
            done: tx.subscribe(),
        };
        let inner = self.inner.clone();
        let source = source.to_path_buf();
        tokio::spawn(async move {
            let mut guard = Guard {
                inner: inner.clone(),
                key,
                tx,
                part: Some(part),
                state: None,
            };
            if let Some(gate) = &inner.start_gate
                && let Ok(p) = gate.acquire().await
            {
                p.forget();
            }
            let result =
                transcode(&inner, &source, guard.part.as_ref().unwrap().path(), kbps).await;
            guard.state = Some(match result {
                Ok(()) => {
                    let part = guard.part.take().unwrap();
                    match part.persist(&final_path) {
                        Ok(_) => TranscodeState::Done,
                        // The error owns the temp file and deletes it as it drops here.
                        Err(e) => TranscodeState::Failed(format!("could not store result: {e}")),
                    }
                }
                Err(msg) => TranscodeState::Failed(msg),
            });
            let ok = guard.state == Some(TranscodeState::Done);
            if ok {
                // Evict before publishing `Done`, so observers of `Done` see the trimmed cache.
                let _ = tokio::task::spawn_blocking(move || evict(&inner)).await;
            }
            drop(guard);
        });
        Ok(out)
    }

    /// Decides what a request does. With `register` it also inserts the new run when nobody else
    /// has started one.
    fn plan(
        &self,
        key: &str,
        final_path: &Path,
        register: Option<(&Path, watch::Receiver<TranscodeState>)>,
    ) -> Plan {
        let mut running = self.inner.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = running.get(key) {
            return Plan::Join(TranscodeOutput::InProgress {
                path: r.path.clone(),
                done: r.done.clone(),
            });
        }
        // A run persists the file before it leaves the map, so "not running" plus "file exists"
        // means finished.
        if final_path.is_file() {
            return Plan::Cached;
        }
        if let Some((path, rx)) = register {
            running.insert(
                key.to_string(),
                Running {
                    path: path.to_path_buf(),
                    done: rx,
                },
            );
        }
        Plan::Start
    }
}

/// Waits for a process slot, then runs ffmpeg under a deadline.
async fn transcode(inner: &Inner, source: &Path, dest: &Path, kbps: u32) -> Result<(), String> {
    let _permit = inner
        .permits
        .acquire()
        .await
        .map_err(|_| "transcoder closed".to_string())?;
    let deadline = match inner.timeout {
        Some(t) => t,
        None => {
            let src = source.to_path_buf();
            let secs = tokio::task::spawn_blocking(move || {
                Probe::open(&src)
                    .ok()?
                    .read()
                    .ok()
                    .map(|f| f.properties().duration().as_secs())
            })
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
            Duration::from_secs(600.max(secs.saturating_mul(3)))
        }
    };
    // Dropping the future on timeout drops the child, and `kill_on_drop` kills it.
    match tokio::time::timeout(deadline, run_ffmpeg(&inner.ffmpeg, source, dest, kbps)).await {
        Ok(r) => r,
        Err(_) => Err("transcode timed out".into()),
    }
}

fn file_arg(path: &Path) -> OsString {
    let mut s = OsString::from("file:");
    s.push(path.as_os_str());
    s
}

async fn run_ffmpeg(ffmpeg: &Path, source: &Path, dest: &Path, kbps: u32) -> Result<(), String> {
    let child = Command::new(ffmpeg)
        .args(["-y", "-v", "error", "-nostdin", "-i"])
        .arg(file_arg(source))
        .args(["-vn", "-map", "0:a:0", "-c:a", "libmp3lame", "-b:a"])
        .arg(format!("{kbps}k"))
        .args(["-f", "mp3"])
        .arg(file_arg(dest))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("ffmpeg wait failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let skip = err.chars().count().saturating_sub(500);
        let tail: String = err.chars().skip(skip).collect();
        tracing::warn!(source = %source.display(), "ffmpeg failed: {tail}");
        Err(format!(
            "ffmpeg exited with {}: {}",
            out.status,
            tail.trim()
        ))
    }
}

/// Marks a cache entry as recently used (LRU order is by modification time).
fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// Deletes the least recently used finished entries until the cache fits `max_bytes`.
/// `.part` files are never touched: they belong to running transcodes. Runs are serialised, and a
/// file that is already gone counts as removed.
fn evict(inner: &Inner) {
    let _serial = inner.evict_lock.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(rd) = std::fs::read_dir(&inner.dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".mp3"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, path) in files {
        if total <= inner.max_bytes {
            break;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => total -= len,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => total -= len,
            Err(_) => {}
        }
    }
}
