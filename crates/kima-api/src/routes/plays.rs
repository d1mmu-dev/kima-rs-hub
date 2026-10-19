//! `POST /api/plays`, `GET /api/plays` (upstream `routes/plays.ts`) and
//! `GET /api/library/recently-listened` (`routes/library/tracks.ts`, artists only: M1 has no
//! audiobooks or podcasts).

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use kima_db::plays::{self, Listen, PlayOutcome, Source};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::library::artists::artist_json;
use super::library::{parse_int, track_json};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/plays", post(create).get(list))
        .route("/library/recently-listened", get(recently_listened))
}

pub(super) fn iso(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayBody {
    track_id: String,
    /// Seconds actually heard. A client that does not send it records no play.
    #[serde(default)]
    seconds_listened: f64,
}

async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<PlayBody>,
) -> Result<Json<Value>, ApiError> {
    if !(0.0..=86_400.0).contains(&body.seconds_listened) {
        return Err(ApiError::Invalid("secondsListened must be 0..86400".into()));
    }
    let Ok(track) = body.track_id.parse::<Uuid>() else {
        return Err(ApiError::NotFound("Track not found"));
    };
    let outcome = plays::record(
        &state.pool,
        user.id,
        track,
        Listen::Seconds(body.seconds_listened),
        Source::Library,
        None,
    )
    .await?;
    let skipped = |reason: &str| Json(json!({ "recorded": false, "reason": reason }));
    Ok(match outcome {
        PlayOutcome::Recorded(id) => Json(json!({ "recorded": true, "playId": id })),
        PlayOutcome::TooShort => skipped("too-short"),
        PlayOutcome::NotEnoughListened => skipped("not-enough-listened"),
        PlayOutcome::Duplicate => skipped("duplicate"),
        PlayOutcome::UnknownTrack => return Err(ApiError::NotFound("Track not found")),
    })
}

#[derive(Deserialize)]
struct LimitParams {
    limit: Option<String>,
}

fn limit_of(params: &LimitParams, fallback: i64, max: i64) -> i64 {
    params
        .limit
        .as_deref()
        .and_then(parse_int)
        .filter(|n| *n != 0)
        .unwrap_or(fallback)
        .clamp(1, max)
}

async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<LimitParams>,
) -> Result<Json<Value>, ApiError> {
    let rows = plays::recent(&state.pool, user.id, limit_of(&params, 50, 200)).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|(p, t)| {
            json!({
                "id": p.id,
                "userId": p.user_id,
                "trackId": p.track_id,
                "playedAt": iso(p.played_at),
                "source": p.source,
                "heard": p.heard,
                "track": track_json(t),
            })
        })
        .collect();
    Ok(Json(Value::Array(out)))
}

async fn recently_listened(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<LimitParams>,
) -> Result<Json<Value>, ApiError> {
    let rows = plays::recent_artists(&state.pool, user.id, limit_of(&params, 10, 100)).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = artist_json(&r.artist);
            v["userHeroUrl"] = Value::Null;
            v["type"] = json!("artist");
            v["lastPlayedAt"] = json!(iso(r.last_played_at));
            v
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}
