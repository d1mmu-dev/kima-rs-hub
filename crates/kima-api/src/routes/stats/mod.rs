#[allow(dead_code)] // Used by later stats routes.
pub mod args;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use kima_api_types::stats::TrackPlays;
use uuid::Uuid;

use crate::{auth::AuthUser, error::ApiError, state::AppState};

pub fn routes() -> Router<AppState> {
    Router::new().route("/stats/track/{id}/plays", get(track_plays))
}

async fn track_plays(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<TrackPlays>, ApiError> {
    if id.is_empty() || id.encode_utf16().count() > 64 {
        return Err(ApiError::BadRequest("Bad request".into()));
    }
    let plays = if let Ok(track) = id.parse::<Uuid>() {
        kima_db::stats::listening::track_play_count(&state.pool, user.id, track)
            .await
            .map_err(|error| {
                if kima_db::is_retryable_conflict(&error) {
                    ApiError::Busy
                } else {
                    tracing::error!(?error, "track play count failed");
                    ApiError::Failed("Couldn't count that track")
                }
            })?
    } else {
        0
    };
    Ok(Json(TrackPlays { plays }))
}
