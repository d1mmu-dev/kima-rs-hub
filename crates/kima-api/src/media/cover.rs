//! Album cover art and stored album and artist images, resized and cached. Shared by `/api`
//! and `/rest`.

use std::path::{Component, Path, PathBuf};

use kima_db::identity::CoverOwner;
use kima_library::artwork;
use uuid::Uuid;

use super::MediaError;
use crate::state::AppState;

/// How many of an album's tracks to probe for embedded art before giving up.
const PROBE_TRACKS: usize = 5;

pub struct CoverImage {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

/// The cover for `id` at `size` (snapped to a cache bucket), or `None` when there is none.
///
/// For an album: a user image first, then embedded and folder art (M1), then the stored
/// fallback cover (spec §12.7). For an artist (an id that is no album): its stored image
/// (D-113). Stored images are read only from `<data_dir>/{album,artist}-images/<id>.jpg`,
/// the name built from the parsed id, never from stored or requested text.
pub async fn cover(
    state: &AppState,
    id: Uuid,
    size: Option<u32>,
) -> Result<Option<CoverImage>, MediaError> {
    let size = size.map(artwork::bucket);
    let Some(data_dir) = state.data_dir.clone() else {
        return Ok(None);
    };
    let jpeg = |bytes: Option<Vec<u8>>| {
        bytes.map(|bytes| CoverImage {
            bytes,
            content_type: "image/jpeg",
        })
    };
    let image_source = match kima_db::identity::cover_owner(&state.pool, id).await? {
        CoverOwner::None | CoverOwner::Artist { has_image: false } => return Ok(None),
        CoverOwner::Artist { has_image: true } => {
            let source = artwork::artist_image_path(&data_dir, id);
            return Ok(jpeg(stored(state, data_dir, id, source, size).await?));
        }
        CoverOwner::Album { image_source } => image_source,
    };
    let album_image = artwork::album_image_path(&data_dir, id);
    if image_source.as_deref() == Some("user")
        && let Some(bytes) = stored(state, data_dir.clone(), id, album_image.clone(), size).await?
    {
        return Ok(jpeg(Some(bytes)));
    }
    if let Some(root) = state.library_root() {
        let library = library_cover(state, data_dir.clone(), root, id, size).await?;
        if library.is_some() {
            return Ok(jpeg(library));
        }
    }
    if image_source.is_some_and(|s| s != "user") {
        return Ok(jpeg(stored(state, data_dir, id, album_image, size).await?));
    }
    Ok(None)
}

/// A variant of a stored image (spec §12.6), cached under `<data_dir>/artwork/<id>/`.
async fn stored(
    state: &AppState,
    data_dir: PathBuf,
    id: Uuid,
    source: PathBuf,
    size: Option<u32>,
) -> Result<Option<Vec<u8>>, MediaError> {
    let cap = state.artwork_cache_bytes;
    tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<u8>>> {
        let build = || artwork::stored_variant(&data_dir, id, &source, size);
        // Another request can evict this entry between our steps; build once more.
        let bytes = match build() {
            Err(artwork::ArtworkError::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => {
                build()?
            }
            other => other?,
        };
        if bytes.is_some()
            && let Err(e) = artwork::evict_to(&data_dir, cap)
        {
            tracing::warn!(error = %e, "could not trim the cover cache");
        }
        Ok(bytes)
    })
    .await
    .map_err(|e| MediaError::Internal(e.into()))?
    .map_err(MediaError::Internal)
}

/// The album's embedded or folder art (M1), resized and cached.
async fn library_cover(
    state: &AppState,
    data_dir: PathBuf,
    root: PathBuf,
    album_id: Uuid,
    size: Option<u32>,
) -> Result<Option<Vec<u8>>, MediaError> {
    let paths = kima_db::library::album_track_paths(&state.pool, album_id).await?;
    let key = album_id.to_string();
    let cap = state.artwork_cache_bytes;
    let job = Job {
        data_dir,
        root,
        key,
        album_id,
        size,
        cap,
        paths,
    };
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<u8>>> {
        // Another request can evict or remove this entry between our steps; build once more.
        match job.run() {
            Err(e) if vanished(&e) => job.run(),
            other => other,
        }
    })
    .await
    .map_err(|e| MediaError::Internal(e.into()))??;
    Ok(bytes)
}

struct Job {
    data_dir: PathBuf,
    root: PathBuf,
    key: String,
    album_id: Uuid,
    size: Option<u32>,
    cap: u64,
    paths: Vec<String>,
}

/// True when the error is a file or folder that disappeared under us.
fn vanished(e: &anyhow::Error) -> bool {
    let not_found = |io: &std::io::Error| io.kind() == std::io::ErrorKind::NotFound;
    e.downcast_ref::<std::io::Error>().is_some_and(not_found)
        || matches!(e.downcast_ref::<artwork::ArtworkError>(), Some(artwork::ArtworkError::Io(io)) if not_found(io))
}

impl Job {
    fn run(&self) -> anyhow::Result<Option<Vec<u8>>> {
        let Self {
            data_dir,
            root,
            key,
            album_id,
            size,
            cap,
            paths,
        } = self;
        let (album_id, size, cap) = (*album_id, *size, *cap);
        let entry = data_dir.join("artwork").join(key);
        if entry.is_dir() {
            if artwork::stamp_is_fresh(root, &entry) {
                let cached = artwork::cache_path(data_dir, key, size);
                if cached.is_file() {
                    artwork::touch_stamp(&entry);
                    return Ok(Some(std::fs::read(cached)?));
                }
            } else {
                // The source or its folder changed (or the entry predates stamps): start over,
                // keeping the variants of a stored image that share the folder.
                artwork::clear_cover_entry(data_dir, album_id)?;
            }
        }
        let mut failed = 0;
        for rel in paths.iter().take(PROBE_TRACKS) {
            let rel = Path::new(rel);
            // Track paths come from our own scanner, but never walk out of the root.
            if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
                continue;
            }
            for (source, origin) in artwork::candidates_with_origin(&root.join(rel)) {
                // Stamped before decoding, so a change during the build shows up as stale.
                let Ok(stamp) = artwork::SourceStamp::capture(root, &origin) else {
                    continue;
                };
                let (file, bad) = artwork::cached_from(data_dir, key, size, [source])?;
                let Some(file) = file else {
                    failed += bad;
                    continue;
                };
                artwork::write_stamp(&entry, &stamp)?;
                // Read before trimming: a tiny cap may evict this very entry.
                let bytes = std::fs::read(&file)?;
                if let Err(e) = artwork::evict_to(data_dir, cap) {
                    tracing::warn!(error = %e, "could not trim the cover cache");
                }
                return Ok(Some(bytes));
            }
        }
        if failed > 0 {
            tracing::warn!(album = %key, failed, "cover art found but none of it could be decoded");
        }
        Ok(None)
    }
}
