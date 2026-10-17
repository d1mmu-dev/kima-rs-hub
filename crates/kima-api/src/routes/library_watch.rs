use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/watch-status", get(watch_status))
}

/// `{mode, active, lastError}`: which watch mode is in effect and the last thing that went wrong.
async fn watch_status(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<kima_library::watch::WatchStatus>, ApiError> {
    let status = state
        .library
        .watch_status
        .read()
        .map(|s| s.clone())
        .unwrap_or_default();
    Ok(Json(status))
}
