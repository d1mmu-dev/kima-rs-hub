use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/watch-status", get(watch_status))
}

/// `{mode, active, lastError, nextSyncAt}`: which watch mode is in effect, the last thing that
/// went wrong, and when the next safety-net reconcile fires (null when scheduled sync is off).
async fn watch_status(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<WatchStatusBody>, ApiError> {
    let status = state
        .library
        .watch_status
        .read()
        .map(|s| s.clone())
        .unwrap_or_default();
    let next_sync_at = state
        .library
        .sync_cron
        .as_ref()
        .and_then(|c| c.next_after(OffsetDateTime::now_utc()))
        .and_then(|t| t.format(&Rfc3339).ok());
    Ok(Json(WatchStatusBody {
        status,
        next_sync_at,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WatchStatusBody {
    #[serde(flatten)]
    status: kima_library::watch::WatchStatus,
    next_sync_at: Option<String>,
}
