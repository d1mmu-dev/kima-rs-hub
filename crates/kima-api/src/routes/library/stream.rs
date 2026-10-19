//! `GET /api/library/tracks/:id/stream?quality=`: the audio itself, with byte ranges.
//!
//! A thin wrapper over [`crate::media`], which `/rest` shares.

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use kima_library::transcode::Quality;
use serde::Deserialize;

use super::parse_id;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::media;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/tracks/{id}/stream", get(stream))
}

#[derive(Deserialize)]
struct Params {
    quality: Option<String>,
}

async fn stream(
    State(state): State<AppState>,
    user: AuthUser,
    UrlPath(id): UrlPath<String>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let quality = match params.quality.as_deref() {
        // Without `quality` the caller's `playbackQuality` setting decides, as upstream.
        None => {
            let saved = kima_db::user_settings::get(&state.pool, user.id).await?;
            Quality::parse(&saved.playback_quality).unwrap_or(Quality::Original)
        }
        Some(q) => Quality::parse(q).ok_or_else(|| {
            ApiError::BadRequest("quality must be original, high, medium or low".into())
        })?,
    };
    let track_id = parse_id(&id).ok_or_else(|| ApiError::NotFound("Track not found"))?;
    let track = media::resolve_track(&state, track_id).await?;
    Ok(media::serve_track(&state, &track, quality, &headers).await?)
}
