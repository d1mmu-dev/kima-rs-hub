//! `GET /api/library/tracks`, `/tracks/shuffle` and `/tracks/:id`.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use kima_db::browse::{self, Sort};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Paging, cover, parse_id, track_json};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library/tracks", get(list))
        .route("/library/tracks/shuffle", get(shuffle))
        .route("/library/tracks/{id}", get(detail))
}

#[derive(Deserialize)]
struct ListParams {
    #[serde(rename = "albumId")]
    album_id: Option<String>,
    #[serde(rename = "sortBy")]
    sort_by: Option<String>,
    #[serde(flatten)]
    paging: Paging,
}

async fn list(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, ApiError> {
    let limit = params.paging.limit(100);
    let offset = params.paging.offset();
    let album: Option<Option<Uuid>> = params
        .album_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(parse_id);
    let (rows, total) = if album == Some(None) {
        (Vec::new(), 0)
    } else {
        let sort = match params.sort_by.as_deref() {
            Some("name-desc") => Sort::NameDesc,
            _ => Sort::NameAsc,
        };
        browse::tracks(&state.pool, album.flatten(), sort, limit, offset).await?
    };
    Ok(Json(json!({
        "tracks": rows.iter().map(track_json).collect::<Vec<_>>(),
        "total": total,
        "offset": offset,
        "limit": limit,
    })))
}

#[derive(Deserialize)]
struct ShuffleParams {
    #[serde(flatten)]
    paging: Paging,
}

async fn shuffle(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<ShuffleParams>,
) -> Result<Json<Value>, ApiError> {
    let (rows, total) = browse::shuffle(&state.pool, params.paging.limit(100)).await?;
    Ok(Json(json!({
        "tracks": rows.iter().map(track_json).collect::<Vec<_>>(),
        "total": total,
    })))
}

/// Shaped as upstream's single-track handler: `{id, title, artist, album, duration}`.
async fn detail(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Track not found");
    let t = browse::track(&state.pool, parse_id(&id).ok_or_else(not_found)?)
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!({
        "id": t.id,
        "title": t.title,
        "artist": { "id": t.album_artist_id, "name": t.artist_name },
        "album": {
            "id": t.album_id,
            "title": t.album_title,
            "coverArt": cover(t.album_id),
        },
        "duration": t.duration_secs,
        "audioFeatures": null,
    })))
}
