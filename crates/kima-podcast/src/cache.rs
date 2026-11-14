//! The episode audio cache (Task 12): downloaded once per episode when played, kept while it
//! is played, measured on first read. A port of `podcastDownload.ts` (D-204: a jobs-queue
//! download instead of an in-process `Set`, the file named `<data_dir>/podcast-audio/<id>.audio`
//! instead of upstream's per-episode `.mp3`, and `lofty` instead of `music-metadata` for the
//! measured length).

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use kima_db::PgPool;
use kima_net::Outcome;
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::ctx::PodcastCtx;

/// The job queue episode downloads run on.
pub const QUEUE: &str = "podcast-cache";
/// Upstream retries a failed download 3 times, 5 s apart (`queue::backoff`'s first delay).
pub const MAX_ATTEMPTS: i32 = 3;
/// A size difference within this fraction of the expected bytes still counts as complete:
/// some hosts and filesystems are inconsistent at the byte level (`SIZE_TOLERANCE`).
const SIZE_TOLERANCE: f64 = 0.01;
/// How long the host may take to answer with its headers (kima-net's `first_byte` bound).
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(600);
/// Upstream's 600 s download timeout, here a deadline for the whole body once the headers are
/// in; kima-net's own per-chunk idle timeout (30 s) also applies (D-204).
const WHOLE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// The most any download may write before it is abandoned (2 GiB), whatever `Content-Length`
/// the host declares; upstream has no cap (D-217).
pub const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const EXPIRE_AFTER_DAYS: i64 = 30;
/// A temp file this old whose episode has no queued or running download is a leftover of a
/// hard kill; [`cleanup`] removes it. Far above the longest download (headers plus body, 20 min).
const ORPHAN_TEMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const TEMP_SUFFIX: &str = ".tmp";
const MARKER: &str = ".lengths-measured";

/// Where cached episode audio lives, relative to `data_dir`.
pub fn cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("podcast-audio")
}

fn episode_path(data_dir: &Path, episode: Uuid) -> PathBuf {
    cache_dir(data_dir).join(format!("{episode}.audio"))
}

/// A temp file of its own for one download attempt, `<episode>.<random>.tmp`: two attempts at
/// the same episode (a redelivered job beside a worker that lost its lease) never write, delete
/// or rename each other's file.
fn attempt_temp_path(data_dir: &Path, episode: Uuid) -> PathBuf {
    cache_dir(data_dir).join(format!(
        "{episode}.{}{TEMP_SUFFIX}",
        Uuid::new_v4().simple()
    ))
}

/// Removes its file when dropped unless [`TempFile::disarm`]ed: an attempt that fails, times
/// out, is cancelled (its future dropped) or unwinds never leaves its temp file behind. The
/// removal is synchronous, so it has happened by the time the future is gone.
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl TempFile {
    fn new(path: PathBuf) -> Self {
        TempFile { path, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn download_key(episode: Uuid) -> String {
    format!("episode:{episode}")
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("episode not found")]
    NoEpisode,
    #[error("the host refused the download: {0}")]
    HostRefused(String),
    #[error("the host did not answer: {0}")]
    HostDown(String),
    #[error("the downloaded file is empty")]
    Empty,
    #[error("downloaded {got} bytes, expected {expected}")]
    Incomplete { got: u64, expected: u64 },
    #[error(
        "the download passed its {limit}-byte limit (the next chunk would make it {would_write} bytes)"
    )]
    TooLarge { would_write: u64, limit: u64 },
    #[error("the download did not finish within {0:?}")]
    TimedOut(Duration),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl CacheError {
    /// Whether a retry cannot help: a host that sent more than the size limit will send as
    /// much again. Everything else is retried under the queue's policy ([`MAX_ATTEMPTS`]).
    pub fn is_permanent(&self) -> bool {
        matches!(self, CacheError::TooLarge { .. })
    }
}

/// The bounds one download runs under ([`download_with_limits`]); [`Default`] is production's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadLimits {
    /// How long the host may take to answer with its headers.
    pub first_byte: Duration,
    /// The deadline for the whole body, from the headers on.
    pub whole: Duration,
    /// The byte cap for every body: one without a `Content-Length` and one whose declared
    /// length (plus the size tolerance) is larger.
    pub max_bytes: u64,
}

impl Default for DownloadLimits {
    fn default() -> Self {
        DownloadLimits {
            first_byte: FIRST_BYTE_TIMEOUT,
            whole: WHOLE_DOWNLOAD_TIMEOUT,
            max_bytes: MAX_DOWNLOAD_BYTES,
        }
    }
}

/// The cached file's path, when it is complete, valid against its download record, and not
/// being downloaded right now (a queued or running job with key `episode:<id>` on [`QUEUE`]).
/// Touches `last_accessed_at` on a hit.
pub async fn cached_path(ctx: &PodcastCtx, episode: Uuid) -> Option<PathBuf> {
    let downloading = kima_jobs::queue::pending_with_key(&ctx.pool, QUEUE, &download_key(episode))
        .await
        .ok()
        .flatten()
        .is_some();
    if downloading {
        return None;
    }
    validate_cached_file(ctx, episode).await
}

async fn validate_cached_file(ctx: &PodcastCtx, episode: Uuid) -> Option<PathBuf> {
    let path = episode_path(&ctx.data_dir, episode);
    let meta = tokio::fs::metadata(&path).await.ok()?;
    if meta.len() == 0 {
        return None;
    }
    let record = match kima_db::podcasts::download_record(&ctx.pool, episode).await {
        Ok(Some(r)) => r,
        _ => {
            discard(&path, &ctx.pool, episode).await;
            return None;
        }
    };
    let expected = record.file_size_mb * 1024.0 * 1024.0;
    if expected > 0.0 && ((meta.len() as f64 - expected).abs() / expected) > SIZE_TOLERANCE {
        discard(&path, &ctx.pool, episode).await;
        return None;
    }
    let _ = kima_db::podcasts::touch_download(&ctx.pool, episode).await;
    Some(path)
}

async fn discard(path: &Path, pool: &PgPool, episode: Uuid) {
    let _ = tokio::fs::remove_file(path).await;
    let _ = kima_db::podcasts::delete_downloads_for_episode(pool, episode).await;
}

/// Queues `episode`'s download, unless one is already queued or running (upstream's in-process
/// `Set` behind `isDownloading`, now the job queue itself, D-204): a second job beside a running
/// one would download the same file twice. Returns the new job's id, or `None` for a duplicate.
pub async fn enqueue_download(
    pool: &PgPool,
    episode: Uuid,
    user: Uuid,
) -> sqlx::Result<Option<Uuid>> {
    let key = download_key(episode);
    let payload = serde_json::json!({
        "kind": "podcast-download",
        "key": key,
        "episode": episode,
        "user": user,
    });
    kima_jobs::queue::enqueue_unless_pending_with_key(pool, QUEUE, payload, MAX_ATTEMPTS).await
}

/// Downloads the episode's audio to the cache: a temp file, checked against the host's own
/// `Content-Length` (never a feed's declared length), renamed in, its measured length and
/// download record stored. Does nothing if a valid cached file already exists. Runs under the
/// production [`DownloadLimits`].
pub async fn download(ctx: &PodcastCtx, episode: Uuid, user: Uuid) -> Result<(), CacheError> {
    download_with_limits(ctx, episode, user, DownloadLimits::default()).await
}

/// [`download`] under explicit `limits`: abandoned (the temp file deleted, nothing recorded)
/// once the body passes the host's `Content-Length` by more than the size tolerance, or
/// [`DownloadLimits::max_bytes`] (whichever is lower), or runs past [`DownloadLimits::whole`]
/// (D-217). Each attempt writes its own temp file, removed however the attempt ends unless it
/// was renamed in (also when the future is dropped midway); only that file, checked against
/// what this attempt received, is renamed in.
pub async fn download_with_limits(
    ctx: &PodcastCtx,
    episode: Uuid,
    user: Uuid,
    limits: DownloadLimits,
) -> Result<(), CacheError> {
    if validate_cached_file(ctx, episode).await.is_some() {
        return Ok(());
    }
    let audio = kima_db::podcasts::episode_audio(&ctx.pool, episode)
        .await?
        .ok_or(CacheError::NoEpisode)?;

    let dir = cache_dir(&ctx.data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let final_path = episode_path(&ctx.data_dir, episode);

    let response = match ctx
        .net
        .open_audio(&audio.audio_url, None, limits.first_byte)
        .await
    {
        Outcome::Answered(res) => res,
        Outcome::Absent(e) => return Err(CacheError::HostRefused(e.0.to_string())),
        Outcome::Permanent(e) => return Err(CacheError::HostRefused(e.message)),
        Outcome::Transient(e) => return Err(CacheError::HostDown(e.message)),
        Outcome::RateLimited { .. } => {
            return Err(CacheError::HostDown("the audio source is busy".into()));
        }
    };
    let expected_bytes: Option<u64> = response
        .content_length
        .as_deref()
        .and_then(|s| s.parse().ok());

    let temp = TempFile::new(attempt_temp_path(&ctx.data_dir, episode));
    let written = tokio::time::timeout(
        limits.whole,
        write_body(&temp.path, response.body, expected_bytes, limits),
    )
    .await
    .unwrap_or(Err(CacheError::TimedOut(limits.whole)))?;

    // Synchronous, so a cancellation cannot leave the rename running after `temp` has gone.
    std::fs::rename(&temp.path, &final_path)?;
    temp.disarm();
    kima_db::podcasts::set_episode_file_size(&ctx.pool, episode, written as i64).await?;
    let _ = record_measured_length(&ctx.pool, episode, &final_path).await;

    let size_mb = written as f64 / 1024.0 / 1024.0;
    let relative = final_path
        .strip_prefix(&ctx.data_dir)
        .unwrap_or(&final_path)
        .to_string_lossy()
        .into_owned();
    kima_db::podcasts::upsert_download(&ctx.pool, user, episode, &relative, size_mb).await?;
    Ok(())
}

/// The most bytes a body may have: `expected` plus the size tolerance, never more than the cap
/// (the whole cap for a body of unknown length).
fn byte_limit(expected: Option<u64>, limits: DownloadLimits) -> u64 {
    match expected {
        Some(expected) if expected > 0 => expected
            .saturating_add((expected as f64 * SIZE_TOLERANCE).ceil() as u64)
            .min(limits.max_bytes),
        _ => limits.max_bytes,
    }
}

/// Streams `body` into `path`, bailing out (and leaving nothing usable) on an empty body, one
/// that ends short of `expected`, or one that passes [`byte_limit`] (checked before each chunk
/// is written, so the file never grows past it).
async fn write_body(
    path: &Path,
    mut body: futures::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
    expected: Option<u64>,
    limits: DownloadLimits,
) -> Result<u64, CacheError> {
    let limit = byte_limit(expected, limits);
    // Created synchronously: a create still running on the blocking pool after a cancellation
    // could make the file appear after its guard removed it.
    let mut file = tokio::fs::File::from_std(std::fs::File::create(path)?);
    let mut written: u64 = 0;
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        let after = written.saturating_add(chunk.len() as u64);
        if after > limit {
            return Err(CacheError::TooLarge {
                would_write: after,
                limit,
            });
        }
        file.write_all(&chunk).await?;
        written = after;
    }
    file.flush().await?;
    drop(file);
    if written == 0 {
        return Err(CacheError::Empty);
    }
    if let Some(expected) = expected {
        if expected > 0 {
            let variance = (written as f64 - expected as f64).abs() / expected as f64;
            if variance > SIZE_TOLERANCE {
                return Err(CacheError::Incomplete {
                    got: written,
                    expected,
                });
            }
        }
    }
    Ok(written)
}

/// What [`record_measured_length`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measured {
    Stored,
    Unreadable,
    NoEpisode,
}

/// Stores the audio's own length on its episode row (ad networks stitch ads into the file
/// when it is fetched, so the feed's declared length can run minutes short of the real one).
/// Leaves the stored length as it was when the file's length cannot be read.
pub async fn record_measured_length(pool: &PgPool, episode: Uuid, path: &Path) -> Measured {
    let Some(seconds) = probe_duration_seconds(path) else {
        return Measured::Unreadable;
    };
    match kima_db::podcasts::set_episode_duration(pool, episode, seconds.round() as i32).await {
        Ok(true) => Measured::Stored,
        Ok(false) => Measured::NoEpisode,
        Err(_) => Measured::Unreadable,
    }
}

fn probe_duration_seconds(path: &Path) -> Option<f64> {
    use lofty::file::AudioFile;
    use lofty::probe::Probe;
    // The cache names files `<uuid>.audio` (D-204), so the extension says nothing about the
    // real format; sniff it from the content instead of trusting the name.
    let probe = Probe::open(path).ok()?.guess_file_type().ok()?;
    let tagged = probe.read().ok()?;
    let seconds = tagged.properties().duration().as_secs_f64();
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

async fn list_audio_files(dir: &Path) -> Option<Vec<String>> {
    let mut rd = tokio::fs::read_dir(dir).await.ok()?;
    let mut names = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        if let Some(name) = entry.file_name().to_str() {
            if name.ends_with(".audio") {
                names.push(name.to_string());
            }
        }
    }
    Some(names)
}

/// Measures every cached episode once: files cached before downloads measured themselves.
/// A marker lists the files that could not be read, so later starts try only those (upstream's
/// `measureCachedEpisodeLengths`). Returns how many were measured this run.
pub async fn measure_cached_once(ctx: &PodcastCtx) -> usize {
    let dir = cache_dir(&ctx.data_dir);
    let marker = dir.join(MARKER);
    let marked: Option<Vec<String>> = tokio::fs::read_to_string(&marker).await.ok().map(|text| {
        text.lines()
            .filter(|l| l.ends_with(".audio"))
            .map(str::to_string)
            .collect()
    });
    let files = match &marked {
        Some(list) => list.clone(),
        None => match list_audio_files(&dir).await {
            Some(v) => v,
            None => return 0,
        },
    };

    let mut measured = 0usize;
    let mut unreadable = Vec::new();
    for file in &files {
        let path = dir.join(file);
        if tokio::fs::metadata(&path).await.is_err() {
            continue;
        }
        let Some(episode) = file
            .strip_suffix(".audio")
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            continue;
        };
        match record_measured_length(&ctx.pool, episode, &path).await {
            Measured::Stored => measured += 1,
            Measured::Unreadable => unreadable.push(file.clone()),
            Measured::NoEpisode => {}
        }
    }
    let unchanged = marked.as_ref().is_some_and(|m| m.len() == unreadable.len());
    if !unchanged {
        let mut contents = vec![OffsetDateTime::now_utc().to_string()];
        contents.extend(unreadable);
        let _ = tokio::fs::write(&marker, contents.join("\n") + "\n").await;
    }
    measured
}

/// Deletes cached episodes nobody played for 30 days (upstream's `cleanupExpiredCache`), and
/// temp files a hard kill left behind ([`remove_orphaned_temp_files`]). Returns how many
/// download records were removed and how many megabytes were freed.
pub async fn cleanup(ctx: &PodcastCtx) -> sqlx::Result<(u64, f64)> {
    let orphans = remove_orphaned_temp_files(ctx, ORPHAN_TEMP_AGE).await?;
    if orphans > 0 {
        tracing::info!(orphans, "removed leftover podcast download temp files");
    }
    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(EXPIRE_AFTER_DAYS);
    let expired = kima_db::podcasts::expired_downloads(&ctx.pool, cutoff).await?;
    let mut deleted = 0u64;
    let mut freed_mb = 0.0;
    for row in expired {
        let path = ctx.data_dir.join(&row.local_path);
        let _ = tokio::fs::remove_file(&path).await;
        kima_db::podcasts::delete_download_row(&ctx.pool, row.user_id, row.episode_id).await?;
        deleted += 1;
        freed_mb += row.file_size_mb;
    }
    Ok((deleted, freed_mb))
}

/// Removes `*.tmp` files in the cache dir last modified more than `older_than` ago whose
/// episode has no queued or running download: a process killed mid-download (no destructor
/// ran) leaves its temp file. Returns how many were removed.
async fn remove_orphaned_temp_files(ctx: &PodcastCtx, older_than: Duration) -> sqlx::Result<u64> {
    let Ok(mut entries) = tokio::fs::read_dir(cache_dir(&ctx.data_dir)).await else {
        return Ok(0);
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|n| n.ends_with(TEMP_SUFFIX)) else {
            continue;
        };
        let old = entry
            .metadata()
            .await
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > older_than);
        if !old {
            continue;
        }
        let episode = name.split('.').next().and_then(|s| Uuid::parse_str(s).ok());
        if let Some(episode) = episode {
            let key = download_key(episode);
            if kima_jobs::queue::pending_with_key(&ctx.pool, QUEUE, &key)
                .await?
                .is_some()
            {
                continue;
            }
        }
        if tokio::fs::remove_file(entry.path()).await.is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn byte_limit_allows_the_size_tolerance_over_a_known_length_and_caps_every_one() {
        let limits = DownloadLimits::default();
        assert_eq!(byte_limit(Some(1_000_000), limits), 1_010_000);
        assert_eq!(byte_limit(None, limits), MAX_DOWNLOAD_BYTES);
        assert_eq!(byte_limit(Some(0), limits), MAX_DOWNLOAD_BYTES);
        assert_eq!(byte_limit(Some(u64::MAX), limits), MAX_DOWNLOAD_BYTES);
        assert_eq!(
            byte_limit(Some(MAX_DOWNLOAD_BYTES - 1), limits),
            MAX_DOWNLOAD_BYTES
        );
    }

    #[test]
    fn each_attempt_gets_its_own_temp_file() {
        let episode = Uuid::now_v7();
        let (a, b) = (
            attempt_temp_path(Path::new("/d"), episode),
            attempt_temp_path(Path::new("/d"), episode),
        );
        assert_ne!(a, b);
        for p in [a, b] {
            let name = p.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with(&format!("{episode}.")), "{name}");
            assert!(name.ends_with(TEMP_SUFFIX), "{name}");
        }
    }

    /// `probe_duration_seconds` must sniff the format from content, not the `.audio`
    /// extension the cache names its files with (D-204) — a bare `.read()` without
    /// `guess_file_type()` fails on an unrecognised extension.
    #[test]
    fn probes_a_real_small_mp3_named_with_an_unknown_extension() {
        if !kima_test_support::require_ffmpeg() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = kima_test_support::make_audio(tmp.path(), "src.mp3", Default::default());
        let renamed = tmp.path().join("ep.audio");
        std::fs::copy(&src, &renamed).unwrap();
        let seconds = probe_duration_seconds(&renamed).expect("a readable duration");
        assert!((1.9..2.1).contains(&seconds), "{seconds}");
    }
}
