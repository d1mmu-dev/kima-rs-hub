//! `GET /api/library/cover-art/:id?size=N`: an album's cover, resized and cached.

use std::path::{Component, Path};

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::IntoResponse;
use axum::routing::get;
use kima_library::artwork;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        // The frontend also builds `?url=` links for external covers; M1 serves library covers only.
        .route("/library/cover-art", get(no_id))
        .route("/library/cover-art/{id}", get(cover_art))
}

/// How many of an album's tracks to probe for embedded art before giving up.
const PROBE_TRACKS: usize = 5;

#[derive(Deserialize)]
struct Params {
    size: Option<String>,
}

async fn no_id(_user: AuthUser) -> Result<(), ApiError> {
    Err(ApiError::NotFound("Cover art not found"))
}

async fn cover_art(
    State(state): State<AppState>,
    _user: AuthUser,
    UrlPath(id): UrlPath<String>,
    Query(params): Query<Params>,
) -> Result<impl IntoResponse, ApiError> {
    let size = match params.size.as_deref() {
        None | Some("") => None,
        Some(s) => match s.parse::<u32>() {
            Ok(n) if n > 0 => Some(artwork::bucket(n)),
            _ => {
                return Err(ApiError::BadRequest(
                    "size must be a positive integer".into(),
                ));
            }
        },
    };
    let not_found = || ApiError::NotFound("Cover art not found");
    let album: Uuid = id.parse().map_err(|_| not_found())?;
    let (Some(data_dir), Some(root)) = (state.data_dir.clone(), state.library_root.clone()) else {
        return Err(not_found());
    };

    let paths = kima_db::library::album_track_paths(&state.pool, album).await?;
    let key = album.to_string();
    let jpeg =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<std::path::PathBuf>> {
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
        .map_err(|e| ApiError::Internal(e.into()))??;

    let Some(file) = jpeg else {
        return Err(not_found());
    };
    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok((
        [
            (CONTENT_TYPE, "image/jpeg"),
            (CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    ))
}
