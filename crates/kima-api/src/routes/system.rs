use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/system/features", get(features))
}

/// Nothing optional exists yet; later milestones turn these on as the subsystems land.
async fn features(_user: AuthUser) -> Json<Value> {
    Json(json!({
        "musicCNN": false,
        "vibeEmbeddings": false,
        "audiobookshelfEnabled": false,
        "fpcalc": false,
    }))
}
