//! Endpoints the frontend polls for subsystems that are not ported yet. They answer with
//! upstream's empty shape so the polls do not 404 (D-070); later milestones replace them.

use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/notifications/downloads/active", get(active_downloads))
        .route("/discover/batch-status", get(batch_status))
}

async fn active_downloads(_user: AuthUser) -> Json<Value> {
    Json(json!([]))
}

async fn batch_status(_user: AuthUser) -> Json<Value> {
    Json(json!({ "active": false, "status": null }))
}
