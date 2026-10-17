//! `POST /api/library/scan` (admin) and `GET /api/library/scan/status/:jobId`.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use kima_db::{browse, users};
use serde_json::{Value, json};

use super::parse_id;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library/scan", post(start))
        .route("/library/scan/status/{job_id}", get(status))
}

/// Queues a reconcile, or returns the id of the one already queued or running.
async fn start(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    if user.role != users::ROLE_ADMIN {
        return Err(ApiError::Forbidden("Admin access required"));
    }
    let job_id = kima_library::jobs::request_reconcile(&state.pool).await?;
    Ok(Json(json!({
        "message": "Library scan started",
        "jobId": job_id,
        "musicPath": state.library_root().map(|p| p.display().to_string()),
    })))
}

/// `{status, progress, result}` with the status names the frontend polls for:
/// `waiting`, `active`, `completed` or `failed`.
async fn status(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Job not found");
    let job = browse::job_status(&state.pool, parse_id(&job_id).ok_or_else(not_found)?)
        .await?
        .ok_or_else(not_found)?;
    let (status, progress) = match job.state.as_str() {
        "queued" => ("waiting", 0),
        "running" => ("active", 0),
        "done" => ("completed", 100),
        _ => ("failed", 0),
    };
    Ok(Json(json!({
        "status": status,
        "progress": progress,
        "result": null,
        "error": job.last_error,
    })))
}
