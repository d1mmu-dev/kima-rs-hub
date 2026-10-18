//! `GET /api/search?q=&type=&limit=`: library search, shaped as upstream `routes/search.ts`
//! (library part only). Artists, albums and tracks reuse the browse serializers.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use kima_db::search;
use serde::Deserialize;
use serde_json::{Value, json};

use super::library::artists::artist_json;
use super::library::{album_json, parse_int, track_json};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Longer queries are cut on a character boundary.
const MAX_QUERY_CHARS: usize = 500;

pub fn routes() -> Router<AppState> {
    Router::new().route("/search", get(search))
}

#[derive(Deserialize)]
struct Params {
    q: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<String>,
}

async fn search(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<Params>,
) -> Result<Json<Value>, ApiError> {
    let q = params.q.as_deref().unwrap_or("").trim();
    let q: String = q.chars().take(MAX_QUERY_CHARS).collect();
    let limit = params
        .limit
        .as_deref()
        .and_then(parse_int)
        .unwrap_or(20)
        .clamp(1, 100);
    let kind = params.kind.as_deref().unwrap_or("all");
    let want = |name: &str| kind == "all" || kind == name;

    let (mut artists, mut albums, mut tracks) = (Vec::new(), Vec::new(), Vec::new());
    if let Some(tsq) = search::prefix_query(&state.pool, &q).await? {
        if want("artists") {
            artists = search::artists(&state.pool, &tsq, limit, 0).await?;
        }
        if want("albums") {
            albums = search::albums(&state.pool, &tsq, limit, 0).await?;
        }
        if want("tracks") {
            tracks = search::tracks(&state.pool, &tsq, limit, 0).await?;
        }
    }
    Ok(Json(json!({
        "artists": artists.iter().map(artist_json).collect::<Vec<_>>(),
        "albums": albums.iter().map(album_json).collect::<Vec<_>>(),
        "tracks": tracks.iter().map(track_json).collect::<Vec<_>>(),
        "audiobooks": [],
        "podcasts": [],
        "episodes": [],
    })))
}
