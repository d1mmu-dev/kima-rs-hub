//! Album cover art, resized and cached. Shared by `/api` and `/rest`.

use std::path::{Component, Path};

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

/// The album's cover at `size` (snapped to a cache bucket), or `None` when it has none.
pub async fn cover(
    state: &AppState,
    album_id: Uuid,
    size: Option<u32>,
) -> Result<Option<CoverImage>, MediaError> {
    let size = size.map(artwork::bucket);
    let (Some(data_dir), Some(root)) = (state.data_dir.clone(), state.library_root()) else {
        return Ok(None);
    };

    let paths = kima_db::library::album_track_paths(&state.pool, album_id).await?;
    let key = album_id.to_string();
    let cap = state.artwork_cache_bytes;
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<u8>>> {
        let entry = data_dir.join("artwork").join(&key);
        if entry.is_dir() {
            if artwork::stamp_is_fresh(&root, &entry) {
                let cached = artwork::cache_path(&data_dir, &key, size);
                if cached.is_file() {
                    artwork::touch_stamp(&entry);
                    return Ok(Some(std::fs::read(cached)?));
                }
            } else {
                // The source or its folder changed (or the entry predates stamps): start over.
                artwork::remove_album(&data_dir, album_id)?;
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
                let Ok(stamp) = artwork::SourceStamp::capture(&root, &origin) else {
                    continue;
                };
                let (file, bad) = artwork::cached_from(&data_dir, &key, size, [source])?;
                let Some(file) = file else {
                    failed += bad;
                    continue;
                };
                artwork::write_stamp(&entry, &stamp)?;
                // Read before trimming: a tiny cap may evict this very entry.
                let bytes = std::fs::read(&file)?;
                if let Err(e) = artwork::evict_to(&data_dir, cap) {
                    tracing::warn!(error = %e, "could not trim the cover cache");
                }
                return Ok(Some(bytes));
            }
        }
        if failed > 0 {
            tracing::warn!(album = %key, failed, "cover art found but none of it could be decoded");
        }
        Ok(None)
    })
    .await
    .map_err(|e| MediaError::Internal(e.into()))??;

    Ok(bytes.map(|bytes| CoverImage {
        bytes,
        content_type: "image/jpeg",
    }))
}
