//! `GET /api/library/cover-art/:id?size=N`: an album's cover, resized and cached.

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Deserialize;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::media;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        // The frontend also builds `?url=` links for external covers; M1 serves library covers only.
        .route("/library/cover-art", get(no_id))
        .route("/library/cover-art/{id}", get(cover_art))
}

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
            Ok(n) if n > 0 => Some(n),
            _ => {
                return Err(ApiError::BadRequest(
                    "size must be a positive integer".into(),
                ));
            }
        },
    };
    let not_found = || ApiError::NotFound("Cover art not found");
    let album = id.parse().map_err(|_| not_found())?;
    let img = media::cover(&state, album, size)
        .await?
        .ok_or_else(not_found)?;
    Ok((
        [
            (CONTENT_TYPE, img.content_type),
            (CACHE_CONTROL, "private, max-age=86400"),
        ],
        img.bytes,
    ))
}
