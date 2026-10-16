use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use kima_core::auth::{TokenKind, verify_dummy, verify_password};
use kima_db::users;
use serde::Deserialize;
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;

use super::onboarding::Credentials;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
}

async fn login(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<Credentials>,
) -> Result<Json<Value>, ApiError> {
    let username = body.username.trim().to_string();
    if username.is_empty() || body.password.is_empty() {
        return Err(ApiError::Invalid(
            "Username and password are required".into(),
        ));
    }

    let found = users::find_by_username(&state.pool, &username).await?;
    let password = body.password;
    let (found, valid) = tokio::task::spawn_blocking(move || {
        let valid = match &found {
            Some(user) => verify_password(&password, &user.password_hash),
            None => {
                verify_dummy(&password);
                false
            }
        };
        (found, valid)
    })
    .await
    .map_err(anyhow::Error::from)?;

    let Some(user) = found.filter(|_| valid) else {
        return Err(ApiError::Unauthorized("Invalid credentials"));
    };
    Ok(Json(json!({
        "token": state.jwt.issue_access(user.id, &user.username, &user.role, user.token_version),
        "refreshToken": state.jwt.issue_refresh(user.id, user.token_version),
        "user": { "id": user.id, "username": user.username, "role": user.role },
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshBody {
    refresh_token: Option<String>,
}

async fn refresh(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<RefreshBody>,
) -> Result<Json<Value>, ApiError> {
    let token = body
        .refresh_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ApiError::BadRequest("Refresh token required".into()))?;
    let claims = state
        .jwt
        .decode(&token)
        .ok()
        .filter(|c| c.kind() == TokenKind::Refresh)
        .ok_or(ApiError::Unauthorized("Invalid refresh token"))?;
    let user = users::find_by_id(&state.pool, claims.user_id)
        .await?
        .ok_or(ApiError::Unauthorized("User not found"))?;
    if user.token_version != claims.token_version {
        return Err(ApiError::Unauthorized("Token invalidated"));
    }
    Ok(Json(json!({
        "token": state.jwt.issue_access(user.id, &user.username, &user.role, user.token_version),
        "refreshToken": state.jwt.issue_refresh(user.id, user.token_version),
    })))
}

/// Tokens are stateless; the client forgets them.
async fn logout() -> Json<Value> {
    Json(json!({ "message": "Logged out" }))
}

async fn me(State(state): State<AppState>, auth: AuthUser) -> Result<Json<Value>, ApiError> {
    let user = users::find_by_id(&state.pool, auth.id)
        .await?
        .ok_or(ApiError::NotFound("User not found"))?;
    let created_at = user
        .created_at
        .format(&Rfc3339)
        .map_err(anyhow::Error::from)?;
    Ok(Json(json!({
        "id": user.id,
        "username": user.username,
        "role": user.role,
        "onboardingComplete": user.onboarding_complete,
        "enrichmentSettings": null,
        "createdAt": created_at,
    })))
}
