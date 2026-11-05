//! `GET /api/library/removals` (admin): the tracks the reconcile retired, newest first (D-161).

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use kima_db::removal_log;
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;

use super::Paging;
use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/removals", get(list))
}

async fn list(
    State(state): State<AppState>,
    _admin: AdminUser,
    Query(paging): Query<Paging>,
) -> Result<Json<Value>, ApiError> {
    let rows = removal_log::list(&state.pool, paging.limit(100), paging.offset()).await?;
    let count = removal_log::count(&state.pool).await?;
    let removals: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "filePath": r.file_path,
                "title": r.title,
                "artistName": r.artist_name,
                "albumTitle": r.album_title,
                "reason": r.reason,
                "artifacts": r.artifacts,
                "removedAt": r.removed_at.format(&Rfc3339).ok(),
            })
        })
        .collect();
    Ok(Json(json!({ "count": count, "removals": removals })))
}
