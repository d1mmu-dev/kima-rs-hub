use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use kima_core::auth::hash_password;
use kima_db::users::{self, Registration};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::{AuthUser, authenticate, bearer_token};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/onboarding/status", get(status))
        .route("/onboarding/register", post(register))
        .route("/onboarding/lidarr", post(lidarr))
        .route("/onboarding/audiobookshelf", post(audiobookshelf))
        .route("/onboarding/soulseek", post(soulseek))
        .route("/onboarding/complete", post(complete))
}

#[derive(Deserialize)]
pub struct Credentials {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let has_account = users::count(&state.pool).await? > 0;
    let registration_open = !has_account || users::registration_open(&state.pool).await?;

    let user = match bearer_token(&headers) {
        Some(token) => authenticate(&state, token, "/api/onboarding/status").await?,
        None => None,
    };
    let needs_onboarding = match user {
        Some(user) => !users::find_by_id(&state.pool, user.id)
            .await?
            .is_some_and(|u| u.onboarding_complete),
        None => !has_account,
    };
    Ok(Json(json!({
        "needsOnboarding": needs_onboarding,
        "hasAccount": has_account,
        "registrationOpen": registration_open,
    })))
}

async fn register(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<Credentials>,
) -> Result<Json<Value>, ApiError> {
    let username = body.username.trim().to_string();
    if !(3..=50).contains(&username.chars().count()) {
        return Err(ApiError::Invalid(
            "Username must be 3 to 50 characters".into(),
        ));
    }
    if body.password.chars().count() < 6 {
        return Err(ApiError::Invalid(
            "Password must be at least 6 characters".into(),
        ));
    }

    // Cheap refusal before the expensive hash; `users::register` re-checks under its lock.
    if users::count(&state.pool).await? > 0 && !users::registration_open(&state.pool).await? {
        return Err(ApiError::Forbidden("Registration is closed"));
    }

    let password = body.password;
    let permit = state.password_permit().await?;
    let hash = tokio::task::spawn_blocking(move || {
        // Released when the hash ends, even if the client hung up and the handler was dropped.
        let _permit = permit;
        hash_password(&password)
    })
    .await
    .map_err(anyhow::Error::from)?;

    match users::register(&state.pool, Uuid::now_v7(), &username, &hash).await? {
        Registration::Closed => Err(ApiError::Forbidden("Registration is closed")),
        Registration::UsernameTaken => Err(ApiError::BadRequest("Username already taken".into())),
        Registration::Created(user) => {
            let token =
                state
                    .jwt
                    .issue_access(user.id, &user.username, &user.role, user.token_version);
            Ok(Json(json!({
                "token": token,
                "user": {
                    "id": user.id,
                    "username": user.username,
                    "role": user.role,
                    "onboardingComplete": false,
                },
            })))
        }
    }
}

/// The integration steps' common field; the url/apiKey or username/password fields are
/// accepted and ignored until the integrations exist (D-025).
#[derive(Deserialize)]
struct IntegrationStep {
    enabled: bool,
}

/// Stub for an integration step: "disabled" succeeds as upstream does; enabling one is refused.
fn integration_step(
    user: &AuthUser,
    step: &IntegrationStep,
    name: &str,
) -> Result<Json<Value>, ApiError> {
    if user.role != users::ROLE_ADMIN {
        return Err(ApiError::Forbidden("Admin access required"));
    }
    if step.enabled {
        return Err(ApiError::BadRequest(format!(
            "{name} integration is not available yet"
        )));
    }
    Ok(Json(json!({ "success": true, "tested": false })))
}

async fn lidarr(
    user: AuthUser,
    ApiJson(step): ApiJson<IntegrationStep>,
) -> Result<Json<Value>, ApiError> {
    integration_step(&user, &step, "Lidarr")
}

async fn audiobookshelf(
    user: AuthUser,
    ApiJson(step): ApiJson<IntegrationStep>,
) -> Result<Json<Value>, ApiError> {
    integration_step(&user, &step, "Audiobookshelf")
}

async fn soulseek(
    user: AuthUser,
    ApiJson(step): ApiJson<IntegrationStep>,
) -> Result<Json<Value>, ApiError> {
    integration_step(&user, &step, "Soulseek")
}

async fn complete(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    users::set_onboarding_complete(&state.pool, user.id).await?;
    Ok(Json(json!({ "success": true })))
}
