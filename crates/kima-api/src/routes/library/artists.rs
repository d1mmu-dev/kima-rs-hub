//! `GET /api/library/artists`, `/artists/:id` and `/recently-added`.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use kima_db::browse::{self, ArtistRow, Sort};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Paging, album_with_tracks, parse_id};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library/artists", get(list))
        .route("/library/artists/{id}", get(detail))
        .route("/library/recently-added", get(recently_added))
}

#[derive(Deserialize)]
struct ListParams {
    query: Option<String>,
    filter: Option<String>,
    #[serde(rename = "sortBy")]
    sort_by: Option<String>,
    #[serde(flatten)]
    paging: Paging,
}

/// The fields the artist cards read. There are no artist images or MusicBrainz ids in M1.
pub(in crate::routes) fn artist_json(a: &ArtistRow) -> Value {
    json!({
        "id": a.id,
        "mbid": null,
        "name": a.name,
        "heroUrl": null,
        "coverArt": null,
        "albumCount": a.album_count,
        "trackCount": a.track_count,
    })
}

async fn list(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, ApiError> {
    let limit = params.paging.limit(50);
    let offset = params.paging.offset();
    // Every library artist is "owned"; there is nothing in discovery.
    let (rows, total) = if params.filter.as_deref() == Some("discovery") {
        (Vec::new(), 0)
    } else {
        let sort = match params.sort_by.as_deref() {
            Some("name-desc") => Sort::NameDesc,
            Some("tracks") => Sort::Recent,
            _ => Sort::NameAsc,
        };
        browse::artists(&state.pool, params.query.as_deref(), sort, limit, offset).await?
    };
    let next_cursor = if rows.len() as i64 == limit {
        rows.last().map(|a| a.id)
    } else {
        None
    };
    Ok(Json(json!({
        "artists": rows.iter().map(artist_json).collect::<Vec<_>>(),
        "total": total,
        "offset": offset,
        "limit": limit,
        "nextCursor": next_cursor,
    })))
}

async fn detail(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Artist not found");
    let artist = browse::artist(&state.pool, parse_id(&id).ok_or_else(not_found)?)
        .await?
        .ok_or_else(not_found)?;
    let albums = browse::artist_albums(&state.pool, artist.id).await?;
    let ids: Vec<Uuid> = albums.iter().map(|a| a.id).collect();
    let tracks = browse::tracks_of_albums(&state.pool, &ids).await?;

    let mut v = artist_json(&artist);
    v["albums"] = albums
        .iter()
        .map(|a| album_with_tracks(a, &tracks))
        .collect();
    v["image"] = Value::Null;
    v["bio"] = Value::Null;
    v["summary"] = Value::Null;
    v["url"] = Value::Null;
    v["listeners"] = json!(0);
    v["genres"] = json!([]);
    v["tags"] = json!([]);
    v["topTracks"] = json!([]);
    v["similarArtists"] = json!([]);
    v["displayName"] = Value::Null;
    v["userSummary"] = Value::Null;
    v["userHeroUrl"] = Value::Null;
    v["userGenres"] = json!([]);
    v["hasUserOverrides"] = json!(false);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RecentParams {
    limit: Option<String>,
}

/// Artists of the most recently added albums, newest first.
async fn recently_added(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<RecentParams>,
) -> Result<Json<Value>, ApiError> {
    let wanted = params
        .limit
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(10)
        .min(100) as usize;
    // Upstream looks at the 20 newest albums and keeps `limit` distinct artists.
    let albums = browse::recent_albums(&state.pool, 20).await?;
    let mut seen = Vec::new();
    for a in &albums {
        if !seen.contains(&a.artist_id) {
            seen.push(a.artist_id);
        }
        if seen.len() >= wanted {
            break;
        }
    }
    let mut artists = Vec::new();
    for id in seen {
        if let Some(a) = browse::artist(&state.pool, id).await? {
            let mut v = artist_json(&a);
            v["userHeroUrl"] = Value::Null;
            artists.push(v);
        }
    }
    Ok(Json(json!({ "artists": artists })))
}
