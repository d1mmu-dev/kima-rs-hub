//! `/api/playlists` (upstream `routes/playlists.ts` and `middleware/playlistOwner.ts`). Pending
//! (Spotify import) tracks and mixes do not exist in M1: their fields are empty or null and
//! `/pending/*` is not routed.

use std::collections::HashSet;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use kima_core::events::Event;
use kima_db::playlists::{self, ItemRow, MoveError, Playlist, PlaylistSummary};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::library::{parse_int, track_json};
use super::plays::iso;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/playlists", get(list).post(create))
        .route("/playlists/{id}", get(detail).put(update).delete(remove))
        .route("/playlists/{id}/hide", post(hide).delete(unhide))
        .route(
            "/playlists/{id}/items",
            post(add_items).delete(remove_items),
        )
        .route("/playlists/{id}/items/batch", post(add_batch))
        .route("/playlists/{id}/items/{track_id}", delete(remove_item))
        .route("/playlists/{id}/items/{item_id}/move", patch(move_item))
}

const NOT_FOUND: &str = "Playlist not found";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    Reader,
    Owner,
}

/// Upstream's `requirePlaylistReader` / `requirePlaylistOwner`. A private playlist of someone else
/// is 404 (its existence is not revealed); a public one is readable but a write is 403.
async fn guard(
    state: &AppState,
    user: &AuthUser,
    id: &str,
    need: Need,
) -> Result<Playlist, ApiError> {
    let not_found = || ApiError::NotFound(NOT_FOUND);
    let id = id.parse::<Uuid>().map_err(|_| not_found())?;
    let playlist = playlists::find(&state.pool, id)
        .await?
        .ok_or_else(not_found)?;
    let owner = playlist.user_id == user.id;
    if !owner && !playlist.is_public {
        return Err(not_found());
    }
    if need == Need::Owner && !owner {
        return Err(ApiError::Forbidden("Access denied"));
    }
    Ok(playlist)
}

fn playlist_json(p: &Playlist) -> Value {
    json!({
        "id": p.id,
        "userId": p.user_id,
        "mixId": null,
        "name": p.name,
        "isPublic": p.is_public,
        "createdAt": iso(p.created_at),
        "updatedAt": iso(p.updated_at),
        "spotifyPlaylistId": null,
        "spotifyPlaylistUrl": null,
    })
}

/// Fractional ranks are not used; the string is the zero-padded position, so comparing ranks as
/// strings orders items as upstream's clients expect.
fn rank_of(sort: i32) -> String {
    format!("{sort:010}")
}

fn item_json(i: &ItemRow) -> Value {
    json!({
        "id": i.item_id,
        "playlistId": i.playlist_id,
        "trackId": i.track.id,
        "sort": i.sort,
        "rank": rank_of(i.sort),
        "track": track_json(&i.track),
    })
}

fn tagged_item_json(i: &ItemRow) -> Value {
    let mut v = item_json(i);
    v["type"] = json!("track");
    v
}

fn summary_json(s: &PlaylistSummary, viewer: Uuid) -> Value {
    let mut v = playlist_json(&s.playlist);
    v["user"] = json!({ "username": s.username });
    v["trackCount"] = json!(s.track_count);
    v["_count"] = json!({ "items": s.track_count });
    v["isOwner"] = json!(s.playlist.user_id == viewer);
    v["isHidden"] = json!(s.hidden);
    v
}

#[derive(Deserialize)]
struct ListParams {
    page: Option<String>,
    limit: Option<String>,
}

async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, ApiError> {
    let page = params
        .page
        .as_deref()
        .and_then(parse_int)
        .filter(|n| *n != 0)
        .unwrap_or(1)
        .max(1);
    let limit = params
        .limit
        .as_deref()
        .and_then(parse_int)
        .filter(|n| *n != 0)
        .unwrap_or(50)
        .clamp(1, 100);
    let rows = playlists::list(
        &state.pool,
        user.id,
        limit,
        (page - 1).saturating_mul(limit),
    )
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.playlist.id).collect();
    let firsts = playlists::first_items(&state.pool, &ids, 4).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|s| {
            let mut v = summary_json(s, user.id);
            v["items"] = Value::Array(
                firsts
                    .iter()
                    .filter(|i| i.playlist_id == s.playlist.id)
                    .map(|i| json!({ "id": i.item_id, "track": track_json(&i.track) }))
                    .collect(),
            );
            v
        })
        .collect();
    Ok(Json(Value::Array(out)))
}

fn check_name(name: &str) -> Result<(), ApiError> {
    let n = name.chars().count();
    if (1..=200).contains(&n) {
        Ok(())
    } else {
        Err(ApiError::Invalid("name must be 1 to 200 characters".into()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBody {
    name: String,
    #[serde(default)]
    is_public: bool,
}

async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<CreateBody>,
) -> Result<Json<Value>, ApiError> {
    check_name(&body.name)?;
    let p = playlists::create(
        &state.pool,
        Uuid::now_v7(),
        user.id,
        &body.name,
        body.is_public,
    )
    .await?;
    Ok(Json(playlist_json(&p)))
}

async fn detail(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = guard(&state, &user, &id, Need::Reader).await?;
    let summary = playlists::summary(&state.pool, p.id, user.id)
        .await?
        .ok_or(ApiError::NotFound(NOT_FOUND))?;
    let items = playlists::items(&state.pool, p.id).await?;
    let items: Vec<Value> = items.iter().map(tagged_item_json).collect();
    let mut v = summary_json(&summary, user.id);
    v["pendingCount"] = json!(0);
    v["pendingTracks"] = json!([]);
    v["mergedItems"] = Value::Array(items.clone());
    v["items"] = Value::Array(items);
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBody {
    name: Option<String>,
    is_public: Option<bool>,
}

async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> Result<Json<Value>, ApiError> {
    let p = guard(&state, &user, &id, Need::Owner).await?;
    if let Some(name) = &body.name {
        check_name(name)?;
    }
    let updated = playlists::update(&state.pool, p.id, body.name.as_deref(), body.is_public)
        .await?
        .ok_or(ApiError::NotFound(NOT_FOUND))?;
    Ok(Json(playlist_json(&updated)))
}

async fn remove(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = guard(&state, &user, &id, Need::Owner).await?;
    playlists::delete(&state.pool, p.id).await?;
    Ok(Json(json!({ "message": "Playlist deleted" })))
}

async fn hide(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = guard(&state, &user, &id, Need::Reader).await?;
    playlists::hide(&state.pool, user.id, p.id).await?;
    Ok(Json(
        json!({ "message": "Playlist hidden", "isHidden": true }),
    ))
}

async fn unhide(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if let Ok(p) = id.parse::<Uuid>() {
        playlists::unhide(&state.pool, user.id, p).await?;
    }
    Ok(Json(
        json!({ "message": "Playlist unhidden", "isHidden": false }),
    ))
}

/// Splits requested ids into UUIDs and strings that can match no track.
fn split_ids(raw: &[String]) -> (Vec<Uuid>, Vec<String>) {
    let mut seen = HashSet::new();
    let (mut ids, mut bad) = (Vec::new(), Vec::new());
    for s in raw.iter().filter(|s| !s.is_empty()) {
        if !seen.insert(s.as_str()) {
            continue;
        }
        match s.parse::<Uuid>() {
            Ok(u) => ids.push(u),
            Err(_) => bad.push(s.clone()),
        }
    }
    (ids, bad)
}

fn too_many(n: usize, max: usize) -> Result<(), ApiError> {
    if (1..=max).contains(&n) {
        Ok(())
    } else {
        Err(ApiError::Invalid(format!(
            "trackIds must hold 1 to {max} ids"
        )))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddBody {
    track_id: Option<String>,
    track_ids: Option<Vec<String>>,
}

async fn add_items(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<AddBody>,
) -> Result<Response, ApiError> {
    let p = guard(&state, &user, &id, Need::Owner).await?;
    let single = body.track_id.as_deref().filter(|s| !s.is_empty());
    let raw = match (single, body.track_ids) {
        (Some(one), None) => vec![one.to_owned()],
        (None, Some(many)) => {
            too_many(many.len(), 1000)?;
            many
        }
        _ => {
            return Err(ApiError::Invalid(
                "Provide exactly one of trackId or trackIds".into(),
            ));
        }
    };
    let (ids, mut rejected) = split_ids(&raw);
    let result = playlists::add_tracks(&state.pool, p.id, &ids)
        .await?
        .ok_or(ApiError::NotFound(NOT_FOUND))?;
    rejected.extend(result.rejected.iter().map(Uuid::to_string));
    if result.added == 0 && result.duplicates == 0 {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "No matching tracks in request", "rejected": rejected })),
        )
            .into_response());
    }
    Ok(Json(json!({
        "added": result.added,
        "duplicated": result.duplicates > 0,
        "duplicates": result.duplicates,
        "rejected": rejected,
    }))
    .into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackIdsBody {
    track_ids: Vec<String>,
}

async fn remove_items(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<TrackIdsBody>,
) -> Result<Json<Value>, ApiError> {
    let p = guard(&state, &user, &id, Need::Owner).await?;
    too_many(body.track_ids.len(), 1000)?;
    let (ids, _) = split_ids(&body.track_ids);
    let removed = playlists::remove_tracks(&state.pool, p.id, &ids).await?;
    if removed == 0 {
        return Err(ApiError::NotFound("No matching tracks in playlist"));
    }
    Ok(Json(json!({ "removed": removed })))
}

async fn add_batch(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<TrackIdsBody>,
) -> Result<Json<Value>, ApiError> {
    too_many(body.track_ids.len(), 500)?;
    let p = guard(&state, &user, &id, Need::Owner).await?;
    let (ids, bad) = split_ids(&body.track_ids);
    let result = playlists::add_tracks(&state.pool, p.id, &ids)
        .await?
        .ok_or(ApiError::NotFound(NOT_FOUND))?;
    let items = playlists::items_for_tracks(&state.pool, p.id, &ids).await?;
    Ok(Json(json!({
        "added": result.added,
        "skippedExisting": result.duplicates,
        "skippedInvalid": result.rejected.len() + bad.len(),
        "items": items.iter().map(item_json).collect::<Vec<_>>(),
    })))
}

async fn remove_item(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, track_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let p = guard(&state, &user, &id, Need::Owner).await?;
    let not_in = || ApiError::NotFound("Track not in playlist");
    let track = track_id.parse::<Uuid>().map_err(|_| not_in())?;
    let removed = playlists::remove_tracks(&state.pool, p.id, &[track]).await?;
    if removed == 0 {
        return Err(not_in());
    }
    Ok(Json(json!({ "success": true, "removed": removed })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveBody {
    after_item_id: Option<String>,
}

async fn move_item(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, item_id)): Path<(String, String)>,
    ApiJson(body): ApiJson<MoveBody>,
) -> Result<Json<Value>, ApiError> {
    let p = guard(&state, &user, &id, Need::Owner).await?;
    let no_item = || ApiError::NotFound("Item not in playlist");
    let no_anchor = || ApiError::BadRequest("Anchor item not in playlist".into());
    let item = item_id.parse::<Uuid>().map_err(|_| no_item())?;
    let after = match body.after_item_id.as_deref() {
        None => None,
        Some(a) => Some(a.parse::<Uuid>().map_err(|_| no_anchor())?),
    };
    match playlists::move_item(&state.pool, p.id, item, after).await? {
        Ok(sort) => {
            state.events.publish(Event {
                user_id: Some(user.id),
                data: json!({ "type": "playlist:updated", "playlistId": p.id }),
            });
            Ok(Json(json!({ "success": true, "rank": rank_of(sort) })))
        }
        Err(MoveError::ItemNotFound) => Err(no_item()),
        Err(MoveError::AnchorNotFound) => Err(no_anchor()),
    }
}
