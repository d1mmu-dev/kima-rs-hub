//! Album cover fallback (spec §12.7; upstream `enrichAlbumCovers`, `artistEnrichment.ts:393-479`):
//! for an artist's albums with no embedded or folder cover, Deezer, then the Cover Art Archive,
//! then fanart.tv, three albums at a time.
//!
//! Every answer is untrusted. A cover is downloaded through `Net::get_bytes`, decoded under
//! limits and re-encoded ([`artwork::normalize_image`]), staged in a temp file, and put in
//! place at `<data_dir>/album-images/<id>.jpg` only while the album row is locked by the write
//! that records it, after that write was accepted (never over a `user` image). A source that
//! fails records nothing and the next is tried; none of them fails the artist's run. A source
//! waiting on its rate limit makes the fill answer `RateLimited`, so the artist is asked again;
//! an album left without a cover because a source failed transiently makes it answer
//! `Transient`, so the artist is asked again later (final review I1).

use std::path::{Component, Path};

use futures::stream::{self, StreamExt};
use kima_db::identity::{self, AlbumForCover};
use kima_library::artwork;
use kima_net::{Failure, Outcome};
use time::OffsetDateTime;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{IdentCtx, Mbid, coverartarchive, deezer, fanart};

/// Albums filled at once (`enrichAlbumCovers`' batches of 3).
pub const BATCH: usize = 3;

/// How one album's fill ended.
enum Filled {
    Stored,
    Nothing,
    Waiting(OffsetDateTime),
    /// No cover, and a source failed transiently (its code): worth asking again later.
    Retry(String),
}

/// Fills the stored covers of `artist`'s albums that have none (spec §12.7). Answers how many
/// were stored; `RateLimited` when a source waits on its rate limit, otherwise `Transient`
/// when an album got no cover because a source failed transiently (whatever else was stored
/// stays stored). A database error is `Transient("DB")`. Dropping the future
/// (cancellation, the artist's time cap) leaves no temp file and no half-recorded cover.
pub async fn fill_for_artist(ctx: &IdentCtx, artist: Uuid) -> Outcome<usize> {
    let albums = match identity::albums_without_cover(&ctx.pool, artist).await {
        Ok(a) => a,
        Err(e) => return Outcome::Transient(Failure::new("DB", e.to_string())),
    };
    if albums.is_empty() {
        return Outcome::Answered(0);
    }
    let fanart_key = ctx.fanart_key().await;
    let results: Vec<Filled> = stream::iter(albums)
        .map(|album| {
            let key = fanart_key.as_deref();
            async move { fill_one(ctx, &album, key).await }
        })
        .buffer_unordered(BATCH)
        .collect()
        .await;
    let mut stored = 0;
    let mut waiting: Option<OffsetDateTime> = None;
    let mut retry: Option<String> = None;
    let mut retried = 0usize;
    for r in results {
        match r {
            Filled::Stored => stored += 1,
            Filled::Nothing => {}
            Filled::Waiting(until) => {
                waiting = Some(waiting.map_or(until, |w| w.min(until)));
            }
            Filled::Retry(code) => {
                retried += 1;
                retry.get_or_insert(code);
            }
        }
    }
    match (waiting, retry) {
        (Some(until), _) => Outcome::RateLimited { until },
        (None, Some(code)) => Outcome::Transient(Failure::new(
            code,
            format!("{retried} album cover(s) could not be fetched; asked again later"),
        )),
        (None, None) => Outcome::Answered(stored),
    }
}

/// Whether the M1 lookup finds embedded or folder art for the album: `find_cover` on its one
/// probed track. A path that would leave the root counts as no art.
async fn has_library_cover(ctx: &IdentCtx, track_path: Option<&str>) -> bool {
    let Some(rel) = track_path else {
        return false;
    };
    let rel = Path::new(rel);
    if rel.as_os_str().is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    let path = ctx.library.root.get().join(rel);
    tokio::task::spawn_blocking(move || artwork::find_cover(&path).is_some())
        .await
        .unwrap_or(false)
}

/// What one source answered.
enum Asked {
    Url(String),
    None,
    Waiting(OffsetDateTime),
    /// Failed transiently (its code): another try may answer.
    Failed(String),
}

fn asked(source: &str, album: Uuid, o: Outcome<Option<String>>) -> Asked {
    match o {
        Outcome::Answered(Some(url)) => Asked::Url(url),
        Outcome::Answered(None) | Outcome::Absent(_) => Asked::None,
        Outcome::RateLimited { until } => Asked::Waiting(until),
        Outcome::Transient(f) => {
            debug!(%album, source, code = %f.code, "album cover source failed");
            Asked::Failed(f.code)
        }
        Outcome::Permanent(f) => {
            debug!(%album, source, code = %f.code, "album cover source failed");
            Asked::None
        }
    }
}

/// Deezer, the Cover Art Archive, fanart.tv: the first cover that downloads, decodes and is
/// accepted is stored.
async fn fill_one(ctx: &IdentCtx, album: &AlbumForCover, fanart_key: Option<&str>) -> Filled {
    if has_library_cover(ctx, album.track_path.as_deref()).await {
        return Filled::Nothing;
    }
    let net = &ctx.net;
    let rg = album.release_group_mbid.map(Mbid);
    let mut waiting = None;
    let mut failed = None;
    for source in ["deezer", "coverartarchive", "fanart"] {
        let answer = match (source, rg, fanart_key) {
            ("deezer", ..) => deezer::album_cover(net, &album.artist_name, &album.title).await,
            ("coverartarchive", Some(rg), _) => coverartarchive::front(net, rg).await,
            ("fanart", Some(rg), Some(key)) => fanart::album_cover(net, key, rg).await,
            _ => continue,
        };
        match asked(source, album.id, answer) {
            Asked::Url(url) => match store(ctx, album.id, source, &url).await {
                Stored::Yes => return Filled::Stored,
                Stored::No => {}
                Stored::Transient(code) => {
                    failed.get_or_insert(code);
                }
                Stored::Refused => return Filled::Nothing,
            },
            Asked::None => {}
            Asked::Waiting(until) => waiting = Some(until),
            Asked::Failed(code) => {
                failed.get_or_insert(code);
            }
        }
    }
    match (waiting, failed) {
        (Some(until), _) => Filled::Waiting(until),
        (None, Some(code)) => Filled::Retry(code),
        (None, None) => Filled::Nothing,
    }
}

enum Stored {
    Yes,
    /// The image failed or the download failed for good: the next source is tried.
    No,
    /// The download or the write failed transiently (its code): the next source is tried, and
    /// the album is worth asking about again later.
    Transient(String),
    /// The album row refused it (a user image, or the album is gone): nothing more to do.
    Refused,
}

/// Downloads `url`, normalises and stages it, then records it and puts the file in place under
/// the album row's lock.
async fn store(ctx: &IdentCtx, album: Uuid, source: &str, url: &str) -> Stored {
    let bytes = match ctx.net.get_bytes(url).await {
        Outcome::Answered(b) => b,
        other => {
            // The message names the host only (kima-net), never the URL's path or query.
            if let Some(code) = other.code() {
                debug!(%album, source, code, "album cover download failed");
            }
            return match other {
                Outcome::Transient(f) => Stored::Transient(f.code),
                _ => Stored::No,
            };
        }
    };
    let data_dir = ctx.library.data_dir.clone();
    let staged = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let jpeg = artwork::normalize_image(&bytes).map_err(|e| e.to_string())?;
        artwork::stage_album_image(&data_dir, &jpeg).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    let staged = match staged {
        Ok(s) => s,
        Err(error) => {
            debug!(%album, source, %error, "album cover did not decode or could not be staged");
            return Stored::No;
        }
    };
    match record(ctx, album, source, staged).await {
        Ok(true) => Stored::Yes,
        Ok(false) => Stored::Refused,
        Err(error) => {
            warn!(%album, source, %error, "could not store an album cover");
            Stored::Transient("DB".into())
        }
    }
}

/// The precedence check and the file's move, in one transaction: the album row is locked by
/// the update, the staged file is renamed into place only when the update was accepted, and
/// the commit follows. A user image written meanwhile either waits for this lock or was
/// already seen by the update, which then refuses.
async fn record(
    ctx: &IdentCtx,
    album: Uuid,
    source: &str,
    staged: artwork::StagedImage,
) -> Result<bool, String> {
    let name = format!("{}.jpg", album.hyphenated());
    let mut tx = ctx.pool.begin().await.map_err(|e| e.to_string())?;
    let applied = identity::set_album_image(&mut tx, album, &name, source)
        .await
        .map_err(|e| e.to_string())?;
    if !applied {
        // Dropping `staged` removes the temp file.
        tx.rollback().await.map_err(|e| e.to_string())?;
        return Ok(false);
    }
    // A plain blocking rename on this task, not `spawn_blocking`: a detached closure would
    // still run after a cancelled fill's rollback released the row lock.
    let dest = artwork::album_image_path(&ctx.library.data_dir, album);
    if let Err(e) = staged.persist(&dest) {
        tx.rollback().await.map_err(|e| e.to_string())?;
        return Err(e.to_string());
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(true)
}
