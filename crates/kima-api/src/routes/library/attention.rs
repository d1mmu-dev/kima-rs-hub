//! `GET /api/library/attention` (admin): albums the scanner had to guess about.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::{attention, users};
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/attention", get(list))
}

async fn list(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    if user.role != users::ROLE_ADMIN {
        return Err(ApiError::Forbidden("Admin access required"));
    }
    let albums = attention::report(&state.pool).await?;
    Ok(Json(json!(
        albums
            .iter()
            .map(|a| json!({
                "albumId": a.album_id,
                "artist": a.artist,
                "album": a.album,
                "year": a.year,
                "issues": a.issues,
            }))
            .collect::<Vec<_>>()
    )))
}
