use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/system/features", get(features))
}

/// The four upstream flags stay off until their subsystems land. The rest say which sections
/// of the app exist: `library` is on from M1, everything not yet ported is off, so the frontend
/// hides those sections (D-030).
async fn features(_user: AuthUser) -> Json<Value> {
    Json(json!({
        "musicCNN": false,
        "vibeEmbeddings": false,
        "audiobookshelfEnabled": false,
        "fpcalc": false,
        "library": true,
        "podcasts": false,
        "audiobooks": false,
        "discover": false,
        "mixes": false,
        "downloads": false,
        "imports": false,
    }))
}
