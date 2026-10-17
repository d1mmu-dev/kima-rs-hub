use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::settings::{self, SettingsPatch, SystemSettings};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{AdminUser, AuthUser};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/system-settings", get(show).post(update))
        .route("/system-settings/public-config", get(public_config))
}

/// Any signed-in user. `KIMA_PUBLIC_URL`, empty when unset (D-096).
async fn public_config(State(state): State<AppState>, _user: AuthUser) -> Json<Value> {
    Json(json!({ "publicUrl": state.public_url.as_deref().unwrap_or("") }))
}

fn body(settings: SystemSettings) -> Json<Value> {
    Json(json!({
        "registrationOpen": settings.registration_open,
        "libraryRoot": settings.library_root,
    }))
}

async fn show(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Value>, ApiError> {
    Ok(body(settings::get(&state.pool).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Patch {
    registration_open: Option<bool>,
}

async fn update(
    State(state): State<AppState>,
    _admin: AdminUser,
    ApiJson(patch): ApiJson<Patch>,
) -> Result<Json<Value>, ApiError> {
    let patch = SettingsPatch {
        registration_open: patch.registration_open,
        library_root: None,
    };
    Ok(body(settings::update(&state.pool, patch).await?))
}
