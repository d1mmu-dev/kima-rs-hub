//! Cached mp3 transcoding via ffmpeg.
//!
//! One ffmpeg process runs per cache key (track, quality, source mtime). Concurrent requests for
//! the same key join it through an in-process map. Output is written to a uniquely named
//! `.part` file and renamed to `<key>.mp3` on success, so a cache entry is never half-written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use tokio::process::Command;
use tokio::sync::watch;
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
    /// ffmpeg failed or could not start; carries the reason. The partial file is already removed.
    Failed(String),
}

#[derive(Debug)]
pub enum TranscodeOutput {
    /// A finished file (the source itself for `Quality::Original`).
    Cached(PathBuf),
    /// A transcode that is still running. `path` is the growing `.part` file. Open it right away:
    /// when the transcode finishes the file is renamed, so an open handle keeps working but the
    /// path stops existing. Wait on `done` for the outcome; on `Done` call `get` again for the
    /// final `Cached` path.
    InProgress {
        path: PathBuf,
        done: watch::Receiver<TranscodeState>,
    },
}

struct Running {
    path: PathBuf,
    done: watch::Receiver<TranscodeState>,
}

struct Inner {
    dir: PathBuf,
    max_bytes: u64,
    running: Mutex<HashMap<String, Running>>,
    spawned: AtomicUsize,
}

#[derive(Clone)]
pub struct Transcoder {
    inner: Arc<Inner>,
}

impl Transcoder {
    /// Creates the cache. Leftover `.part` files from an earlier run are removed.
    pub fn new(cache_dir: impl Into<PathBuf>, max_bytes: u64) -> Transcoder {
        let dir = cache_dir.into();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(".part") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Transcoder {
            inner: Arc::new(Inner {
                dir,
                max_bytes,
                running: Mutex::new(HashMap::new()),
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

    /// How many ffmpeg processes this transcoder has started (a test hook).
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
        let mtime = tokio::fs::metadata(source)
            .await?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let key = format!("{track_id}-{}-{mtime}", quality.name());
        let final_path = self.inner.dir.join(format!("{key}.mp3"));

        // Lookup and registration happen under one lock, so two callers cannot both start ffmpeg
        // and a finished file is never missed between "task done" and "entry removed".
        let (part, tx) = {
            let mut running = self.inner.running.lock().unwrap();
            if let Some(r) = running.get(&key) {
                return Ok(TranscodeOutput::InProgress {
                    path: r.path.clone(),
                    done: r.done.clone(),
                });
            }
            if final_path.is_file() {
                touch(&final_path);
                return Ok(TranscodeOutput::Cached(final_path));
            }
            std::fs::create_dir_all(&self.inner.dir)?;
            let part = tempfile::Builder::new()
                .prefix(&format!("{key}-"))
                .suffix(".part")
                .tempfile_in(&self.inner.dir)?;
            let (tx, rx) = watch::channel(TranscodeState::Running);
            running.insert(
                key.clone(),
                Running {
                    path: part.path().to_path_buf(),
                    done: rx,
                },
            );
            self.inner.spawned.fetch_add(1, Ordering::SeqCst);
            (part, tx)
        };

        let out = TranscodeOutput::InProgress {
            path: part.path().to_path_buf(),
            done: tx.subscribe(),
        };
        let inner = self.inner.clone();
        let source = source.to_path_buf();
        tokio::spawn(async move {
            let state = match run_ffmpeg(&source, part.path(), kbps).await {
                Ok(()) => match part.persist(&final_path) {
                    Ok(_) => TranscodeState::Done,
                    Err(e) => TranscodeState::Failed(format!("could not store result: {e}")),
                },
                // Dropping `part` removes the partial file.
                Err(msg) => TranscodeState::Failed(msg),
            };
            let ok = state == TranscodeState::Done;
            inner.running.lock().unwrap().remove(&key);
            let _ = tx.send(state);
            if ok {
                let _ = tokio::task::spawn_blocking(move || evict(&inner)).await;
            }
        });
        Ok(out)
    }
}

async fn run_ffmpeg(source: &Path, dest: &Path, kbps: u32) -> Result<(), String> {
    let child = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-nostdin", "-i"])
        .arg(source)
        .args(["-vn", "-map", "0:a:0", "-c:a", "libmp3lame", "-b:a"])
        .arg(format!("{kbps}k"))
        .args(["-f", "mp3"])
        .arg(dest)
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
/// `.part` files are never touched: they belong to running transcodes.
fn evict(inner: &Inner) {
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
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}
