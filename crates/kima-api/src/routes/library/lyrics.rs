//! `GET /api/library/tracks/{id}/lyrics` (user; spec §14.2): the stored lyrics, else LRCLIB.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use kima_ident::lyrics;
use serde_json::{Value, json};

use super::parse_id;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/tracks/{id}/lyrics", get(track_lyrics))
}

async fn track_lyrics(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Track not found");
    let id = parse_id(&id).ok_or_else(not_found)?;
    let answer = lyrics::for_track(&state.pool, &state.net, &state.identify, id)
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(json!({
        "plainLyrics": answer.plain,
        "syncedLyrics": answer.synced,
        "source": answer.source,
    })))
}
