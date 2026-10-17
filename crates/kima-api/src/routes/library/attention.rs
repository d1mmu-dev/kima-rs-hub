//! `GET /api/library/attention` (admin): albums the scanner had to guess about.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::attention;
use serde_json::{Value, json};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/attention", get(list))
}

async fn list(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Value>, ApiError> {
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
