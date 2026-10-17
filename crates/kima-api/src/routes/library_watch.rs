use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::users;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/watch-status", get(watch_status))
}

/// `{mode, active, lastError}`: which watch mode is in effect and the last thing that went wrong.
async fn watch_status(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<kima_library::watch::WatchStatus>, ApiError> {
    if user.role != users::ROLE_ADMIN {
        return Err(ApiError::Forbidden("Admin access required"));
    }
    let status = state
        .library
        .watch_status
        .read()
        .map(|s| s.clone())
        .unwrap_or_default();
    Ok(Json(status))
}
