//! `/api/playback-state` (upstream `routes/playbackState.ts`): the signed-in user's resume point
//! and queue, plus `GET /limit` and `POST /live`.

use axum::extract::{DefaultBodyLimit, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use kima_db::playback::{self, PlaybackState, QUEUE_LIMIT};
use serde_json::{Map, Value, json};

use super::plays::iso;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/playback-state/limit", get(limit))
        .route("/playback-state", get(read).post(save).delete(clear))
        .route("/playback-state/live", post(live))
        // Queues are big: upstream raises the JSON limit to 5 MB for this path.
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024))
}

fn state_json(s: &PlaybackState) -> Value {
    json!({
        "userId": s.user_id,
        "playbackType": s.playback_type,
        "trackId": s.track_id,
        "audiobookId": s.audiobook_id,
        "podcastId": s.podcast_id,
        "queue": s.queue,
        "currentIndex": s.current_index,
        "isShuffle": s.is_shuffle,
        "position": s.position,
        "changedBy": s.changed_by,
        "createdAt": iso(s.created_at),
        "updatedAt": iso(s.updated_at),
    })
}

async fn limit(_user: AuthUser) -> Json<Value> {
    Json(json!({ "maxQueueLength": QUEUE_LIMIT }))
}

async fn read(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    Ok(Json(match playback::get(&state.pool, user.id).await? {
        Some(s) => state_json(&s),
        None => Value::Null,
    }))
}

/// JavaScript's `String(v)` for the scalar ids a client may send; objects and arrays are junk.
fn stringish(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(_) => true,
    }
}

fn clip(v: Option<&Value>, fallback: &str, max: usize) -> String {
    let s = v
        .filter(|v| truthy(Some(v)))
        .and_then(stringish)
        .unwrap_or_else(|| fallback.to_owned());
    s.chars().take(max).collect()
}

/// An id field: absent leaves the stored value, null or empty clears it.
fn id_field(body: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    body.get(key)
        .map(|v| stringish(v).filter(|s| !s.is_empty()))
}

/// Cuts the queue to the limit and rebuilds each entry from the fields the player reads, so a
/// client cannot stuff arbitrary data (or huge strings) into the row.
fn sanitize_queue(items: &[Value]) -> Option<Value> {
    let safe: Vec<Value> = items
        .iter()
        .take(QUEUE_LIMIT)
        .filter(|item| truthy(item.get("id")))
        .map(|item| {
            let artist = item.get("artist").filter(|a| truthy(Some(a))).map(|a| {
                json!({
                    "id": a.get("id").and_then(stringish).unwrap_or_default(),
                    "name": clip(a.get("name"), "Unknown", 200),
                })
            });
            let album = item.get("album").filter(|a| truthy(Some(a))).map(|a| {
                let cover = a
                    .get("coverArt")
                    .filter(|c| truthy(Some(c)))
                    .and_then(stringish)
                    .map(|c| c.chars().take(1000).collect::<String>());
                json!({
                    "id": a.get("id").and_then(stringish).unwrap_or_default(),
                    "title": clip(a.get("title"), "Unknown", 500),
                    "coverArt": cover,
                })
            });
            let duration = item
                .get("duration")
                .and_then(Value::as_f64)
                .filter(|d| d.is_finite())
                .unwrap_or(0.0);
            json!({
                "id": item.get("id").and_then(stringish).unwrap_or_default(),
                "title": clip(item.get("title"), "Unknown", 500),
                "duration": duration,
                "artist": artist,
                "album": album,
            })
        })
        .collect();
    (!safe.is_empty()).then_some(Value::Array(safe))
}

async fn save(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let body = body.as_object().cloned().unwrap_or_default();
    if !truthy(body.get("playbackType")) {
        return Err(ApiError::BadRequest("playbackType is required".into()));
    }
    let playback_type = match body.get("playbackType").and_then(Value::as_str) {
        Some(t @ ("track" | "audiobook" | "podcast")) => t.to_owned(),
        _ => return Err(ApiError::BadRequest("Invalid playbackType".into())),
    };

    // A request without a `queue` is a partial update (a shuffle toggle, a track change within
    // the queue the client already has): it must not clear the stored queue.
    let queue = body.get("queue").and_then(Value::as_array);
    let safe_queue = queue.and_then(|q| sanitize_queue(q));
    let requested = body
        .get("currentIndex")
        .and_then(Value::as_f64)
        .filter(|f| f.is_finite())
        .map_or(0, |f| f.clamp(0.0, f64::from(i32::MAX)) as i32);
    let last = safe_queue
        .as_ref()
        .and_then(Value::as_array)
        .map_or(0, |q| q.len() as i32 - 1);
    let current_index = if queue.is_some() {
        Some(requested.min(last.max(0)))
    } else if body.get("currentIndex").is_some_and(Value::is_number) {
        Some(requested)
    } else {
        None
    };

    let patch = playback::Patch {
        playback_type,
        track_id: id_field(&body, "trackId"),
        audiobook_id: id_field(&body, "audiobookId"),
        podcast_id: id_field(&body, "podcastId"),
        queue: queue.map(|_| safe_queue),
        current_index,
        is_shuffle: body.get("isShuffle").and_then(Value::as_bool),
        // Upstream's web save never writes these; only the Subsonic queue does.
        position: None,
        changed_by: None,
    };
    let saved = playback::upsert(&state.pool, user.id, patch).await?;
    Ok(Json(state_json(&saved)))
}

/// Upstream answers 404 for everyone but the public "live trip" user, and the web player stops
/// reporting after a 404. kima-rs has no live trip (M5), so every caller gets that 404.
async fn live(_user: AuthUser) -> ApiError {
    ApiError::NotFound("Not available")
}

async fn clear(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let removed = playback::delete(&state.pool, user.id).await?;
    Ok(Json(json!({ "success": true, "cleared": removed })))
}
