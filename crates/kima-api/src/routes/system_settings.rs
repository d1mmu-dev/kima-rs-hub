use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::settings::{self, SettingsPatch, SystemSettings};
use kima_db::users;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/system-settings", get(show).post(update))
}

fn require_admin(user: &AuthUser) -> Result<(), ApiError> {
    if user.role != users::ROLE_ADMIN {
        return Err(ApiError::Forbidden("Admin access required"));
    }
    Ok(())
}

fn body(settings: SystemSettings) -> Json<Value> {
    Json(json!({
        "registrationOpen": settings.registration_open,
        "libraryRoot": settings.library_root,
    }))
}

async fn show(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;
    Ok(body(settings::get(&state.pool).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Patch {
    registration_open: Option<bool>,
}

async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(patch): ApiJson<Patch>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;
    let patch = SettingsPatch {
        registration_open: patch.registration_open,
        library_root: None,
    };
    Ok(body(settings::update(&state.pool, patch).await?))
}
