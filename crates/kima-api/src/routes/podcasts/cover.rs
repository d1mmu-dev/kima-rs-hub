//! `GET /api/podcasts/{id}/cover`, `GET /api/podcasts/episodes/{episodeId}/cover` (Task 15):
//! legacy cover links handed out before covers were served through the proxy go to the show's
//! or episode's own `image_url` through `/api/library/cover-art?url=`; `None` without an image
//! is the same 404 whether the show/episode has none or does not exist (upstream never checks
//! existence separately: a missing row and a null `image_url` both read as `undefined`).

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::LOCATION;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/podcasts/{id}/cover", get(show_cover))
        .route("/podcasts/episodes/{episode_id}/cover", get(episode_cover))
}

fn not_found() -> ApiError {
    ApiError::NotFound("Cover not found")
}

/// A `302` to the external-image proxy for `image_url`, forwarding `?token=` when the caller
/// sent one (so a saved link that carries its own session keeps working).
fn proxy_redirect(image_url: &str, token: Option<&str>) -> Response {
    let mut qs = form_urlencoded::Serializer::new(String::new());
    qs.append_pair("url", image_url);
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        qs.append_pair("token", token);
    }
    (
        StatusCode::FOUND,
        [(LOCATION, format!("/api/library/cover-art?{}", qs.finish()))],
    )
        .into_response()
}

async fn show_cover(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let id = Uuid::parse_str(&id).map_err(|_| not_found())?;
    let image_url = kima_db::podcasts::show_by_id(&state.pool, id)
        .await?
        .and_then(|s| s.image_url)
        .filter(|u| !u.is_empty())
        .ok_or_else(not_found)?;
    Ok(proxy_redirect(
        &image_url,
        params.get("token").map(String::as_str),
    ))
}

async fn episode_cover(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(episode_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let id = Uuid::parse_str(&episode_id).map_err(|_| not_found())?;
    let image_url = kima_db::podcasts::episode_cover_url(&state.pool, id)
        .await?
        .filter(|u| !u.is_empty())
        .ok_or_else(not_found)?;
    Ok(proxy_redirect(
        &image_url,
        params.get("token").map(String::as_str),
    ))
}
