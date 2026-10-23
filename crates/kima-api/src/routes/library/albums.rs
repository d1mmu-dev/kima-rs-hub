//! `GET /api/library/albums` and `/albums/:id`.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use kima_db::browse::{self, Sort};
use kima_db::genres;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Paging, album_json, album_with_tracks, parse_id};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library/albums", get(list))
        .route("/library/albums/{id}", get(detail))
}

#[derive(Deserialize)]
struct ListParams {
    #[serde(rename = "artistId")]
    artist_id: Option<String>,
    filter: Option<String>,
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
    let limit = params.paging.limit(500);
    let offset = params.paging.offset();
    let artist: Option<Option<Uuid>> = params
        .artist_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(parse_id);
    // A discovery filter, or an artist id that cannot exist, matches nothing.
    let (rows, total) = if params.filter.as_deref() == Some("discovery") || artist == Some(None) {
        (Vec::new(), 0)
    } else {
        let sort = match params.sort_by.as_deref() {
            Some("name-desc") => Sort::NameDesc,
            Some("recent") => Sort::Recent,
            _ => Sort::NameAsc,
        };
        browse::albums(&state.pool, artist.flatten(), sort, limit, offset).await?
    };
    Ok(Json(json!({
        "albums": rows.iter().map(album_json).collect::<Vec<_>>(),
        "total": total,
        "offset": offset,
        "limit": limit,
    })))
}

async fn detail(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Album not found");
    let album = browse::album(&state.pool, parse_id(&id).ok_or_else(not_found)?)
        .await?
        .ok_or_else(not_found)?;
    let tracks = browse::tracks_of_albums(&state.pool, &[album.id]).await?;
    let effective = genres::effective_for_albums(&state.pool, &[album.id])
        .await?
        .remove(&album.id)
        .unwrap_or_default();
    let mut v = album_with_tracks(&album, &tracks);
    v["genres"] = json!(genres::merged(&album.user_genres, &effective));
    Ok(Json(v))
}
