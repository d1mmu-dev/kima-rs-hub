//! Album cover art, resized and cached. Shared by `/api` and `/rest`.

use std::path::{Component, Path, PathBuf};

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
    let jpeg = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<PathBuf>> {
        let cached = artwork::cache_path(&data_dir, &key, size);
        if cached.is_file() {
            return Ok(Some(cached));
        }
        let mut failed = 0;
        for rel in paths.iter().take(PROBE_TRACKS) {
            let rel = Path::new(rel);
            // Track paths come from our own scanner, but never walk out of the root.
            if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
                continue;
            }
            let candidates = artwork::candidates(&root.join(rel));
            let (file, bad) = artwork::cached_from(&data_dir, &key, size, candidates)?;
            if file.is_some() {
                return Ok(file);
            }
            failed += bad;
        }
        if failed > 0 {
            tracing::warn!(album = %key, failed, "cover art found but none of it could be decoded");
        }
        Ok(None)
    })
    .await
    .map_err(|e| MediaError::Internal(e.into()))??;

    let Some(file) = jpeg else {
        return Ok(None);
    };
    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|e| MediaError::Internal(e.into()))?;
    Ok(Some(CoverImage {
        bytes,
        content_type: "image/jpeg",
    }))
}
