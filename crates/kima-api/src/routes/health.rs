use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::state::AppState;

pub async fn health(State(state): State<AppState>) -> Response {
    let db = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();
    let word = if db { "ok" } else { "error" };
    let status = if db {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({ "status": word, "checks": { "db": word } })),
    )
        .into_response()
}
