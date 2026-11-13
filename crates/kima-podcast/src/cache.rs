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
/// Upstream's 600 s whole-download timeout; kima-net's own per-chunk idle timeout (30 s) also
/// applies and is not overridden here (D-204).
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const EXPIRE_AFTER_DAYS: i64 = 30;
const MARKER: &str = ".lengths-measured";

/// Where cached episode audio lives, relative to `data_dir`.
pub fn cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("podcast-audio")
}

fn episode_path(data_dir: &Path, episode: Uuid) -> PathBuf {
    cache_dir(data_dir).join(format!("{episode}.audio"))
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
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

/// Queues `episode`'s download, unless one is already queued (upstream's in-process `Set`,
/// now the job queue itself, D-204). Returns the new job's id, or `None` for a duplicate.
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
    kima_jobs::queue::enqueue_unless_queued_with_key(pool, QUEUE, payload, MAX_ATTEMPTS, "key")
        .await
}

/// Downloads the episode's audio to the cache: a temp file, checked against the host's own
/// `Content-Length` (never a feed's declared length), renamed in, its measured length and
/// download record stored. Does nothing if a valid cached file already exists.
pub async fn download(ctx: &PodcastCtx, episode: Uuid, user: Uuid) -> Result<(), CacheError> {
    if validate_cached_file(ctx, episode).await.is_some() {
        return Ok(());
    }
    let audio = kima_db::podcasts::episode_audio(&ctx.pool, episode)
        .await?
        .ok_or(CacheError::NoEpisode)?;

    let dir = cache_dir(&ctx.data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let final_path = episode_path(&ctx.data_dir, episode);
    let temp_path = dir.join(format!("{episode}.tmp"));
    let _ = tokio::fs::remove_file(&temp_path).await;

    let response = match ctx
        .net
        .open_audio(&audio.audio_url, None, DOWNLOAD_TIMEOUT)
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

    let write_result = write_body(&temp_path, response.body, expected_bytes).await;
    let written = match write_result {
        Ok(n) => n,
        Err(e) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e);
        }
    };

    tokio::fs::rename(&temp_path, &final_path).await?;
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

/// Streams `body` into `path`, bailing out (and leaving nothing usable) on an empty body or
/// one that ends short of `expected`.
async fn write_body(
    path: &Path,
    mut body: futures::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
    expected: Option<u64>,
) -> Result<u64, CacheError> {
    let mut file = tokio::fs::File::create(path).await?;
    let mut written: u64 = 0;
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
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

/// Deletes cached episodes nobody played for 30 days (upstream's `cleanupExpiredCache`).
/// Returns how many download records were removed and how many megabytes were freed.
pub async fn cleanup(ctx: &PodcastCtx) -> sqlx::Result<(u64, f64)> {
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

#[cfg(test)]
mod probe_tests {
    use super::*;

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
