use std::time::Instant;

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use kima_core::auth::{TokenKind, hash_password, verify_dummy, verify_password};
use kima_db::users::{self, Deletion};
use serde::Deserialize;
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::onboarding::Credentials;
use crate::auth::{AdminUser, AuthUser, ClientIp};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::revoke_sessions;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/stream-ticket", post(stream_ticket))
        .route("/auth/change-password", post(change_password))
        .route("/auth/users", get(list_users))
        .route("/auth/create-user", post(create_user))
        .route("/auth/users/{id}", delete(delete_user))
}

/// The longest username registration accepts.
const MAX_USERNAME_CHARS: usize = 50;

fn rate_limited(blocked: crate::login_limit::Blocked) -> ApiError {
    ApiError::RateLimited {
        retry_after_secs: blocked.retry_after_secs(),
    }
}

async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(body): ApiJson<Credentials>,
) -> Result<Json<Value>, ApiError> {
    let username = body.username.trim().to_string();
    if username.is_empty() || body.password.is_empty() {
        return Err(ApiError::Invalid(
            "Username and password are required".into(),
        ));
    }

    // No account can have a name this long (registration allows 50), so refuse it before it
    // becomes a limiter key or reaches the database.
    if username.chars().count() > MAX_USERNAME_CHARS {
        return Err(ApiError::Unauthorized("Invalid credentials"));
    }

    // Before any lookup or hashing, so a blocked guess costs nothing. The attempt is counted as
    // a failure now (parallel or abandoned requests cannot dodge the backoff) and cleared only
    // on success.
    let now = Instant::now();
    let attempt = state
        .login_limit
        .begin(ip, &username, now)
        .map_err(rate_limited)?;

    let found = users::find_by_username(&state.pool, &username).await?;
    // The database folds case its own way (`lower()` under its locale), Rust another. Also
    // count the attempt on the stored name's key, so no spelling gets a separate budget.
    let stored_attempt = match &found {
        Some(user) if user.username.to_lowercase() != username.to_lowercase() => Some(
            state
                .login_limit
                .begin(ip, &user.username, now)
                .map_err(rate_limited)?,
        ),
        _ => None,
    };
    let password = body.password;
    let permit = state.password_permit().await?;
    let (found, valid) = tokio::task::spawn_blocking(move || {
        // Released when the verify ends, even if the client hung up and the handler was dropped.
        let _permit = permit;
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
    attempt.success();
    if let Some(stored) = stored_attempt {
        stored.success();
    }
    Ok(Json(json!({
        "token": state.jwt.issue_access(user.id, &user.username, &user.role, user.token_version),
        "refreshToken": state.jwt.issue_refresh(user.id, user.token_version),
        "user": {
            "id": user.id,
            "username": user.username,
            "role": user.role,
            "onboardingComplete": user.onboarding_complete,
        },
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

/// A 4 h stream-scoped token for `<audio src>` URLs. `ticket` is upstream's key; `token` is what
/// the kima-rs frontend client reads.
async fn stream_ticket(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Value>, ApiError> {
    let user = users::find_by_id(&state.pool, auth.id)
        .await?
        .ok_or(ApiError::Unauthorized("Not authenticated"))?;
    let token = state.jwt.issue_stream(user.id, user.token_version);
    Ok(Json(json!({
        "token": token,
        "ticket": token,
        "expiresIn": kima_core::auth::STREAM_TTL_SECS,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangePassword {
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(body): ApiJson<ChangePassword>,
) -> Result<Json<Value>, ApiError> {
    if body.current_password.is_empty() || body.new_password.is_empty() {
        return Err(ApiError::BadRequest(
            "Current and new password are required".into(),
        ));
    }
    if body.new_password.chars().count() < 6 {
        return Err(ApiError::BadRequest(
            "New password must be at least 6 characters".into(),
        ));
    }
    let user = users::find_by_id(&state.pool, auth.id)
        .await?
        .ok_or(ApiError::NotFound("User not found"))?;

    let seen_version = user.token_version;
    let permit = state.password_permit().await?;
    let (current, new) = (body.current_password, body.new_password);
    let hash = tokio::task::spawn_blocking(move || {
        // Released when the work ends, even if the client hung up and the handler was dropped.
        let _permit = permit;
        verify_password(&current, &user.password_hash).then(|| hash_password(&new))
    })
    .await
    .map_err(anyhow::Error::from)?;
    let Some(hash) = hash else {
        // 401 means "not signed in"; a wrong password from a signed-in user is a bad request.
        return Err(ApiError::BadRequest("Current password is incorrect".into()));
    };

    let version = users::set_password(&state.pool, auth.id, &hash, seen_version)
        .await?
        .ok_or_else(|| ApiError::Conflict("Password was changed concurrently, please retry"))?;
    revoke_sessions(&state, auth.id);
    Ok(Json(json!({
        "message": "Password changed successfully",
        "token": state.jwt.issue_access(auth.id, &auth.username, &auth.role, version),
        "refreshToken": state.jwt.issue_refresh(auth.id, version),
    })))
}

async fn list_users(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    let rows = users::list(&state.pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for user in rows {
        out.push(json!({
            "id": user.id,
            "username": user.username,
            "role": user.role,
            "onboardingComplete": user.onboarding_complete,
            "createdAt": user.created_at.format(&Rfc3339).map_err(anyhow::Error::from)?,
        }));
    }
    Ok(Json(Value::Array(out)))
}

#[derive(Deserialize)]
struct CreateUser {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    role: Option<String>,
}

async fn create_user(
    State(state): State<AppState>,
    _admin: AdminUser,
    ApiJson(body): ApiJson<CreateUser>,
) -> Result<Json<Value>, ApiError> {
    let username = body.username.trim().to_string();
    if username.is_empty() || body.password.is_empty() {
        return Err(ApiError::BadRequest(
            "Username and password are required".into(),
        ));
    }
    if body.password.chars().count() < 6 {
        return Err(ApiError::BadRequest(
            "Password must be at least 6 characters".into(),
        ));
    }
    let role = match body.role.as_deref() {
        None | Some("") | Some(users::ROLE_USER) => users::ROLE_USER,
        Some(users::ROLE_ADMIN) => users::ROLE_ADMIN,
        Some(_) => return Err(ApiError::BadRequest("Invalid role".into())),
    };

    let permit = state.password_permit().await?;
    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(&password)
    })
    .await
    .map_err(anyhow::Error::from)?;

    let user = users::create_by_admin(&state.pool, Uuid::now_v7(), &username, &hash, role)
        .await?
        .map_err(|_| ApiError::BadRequest("Username already taken".into()))?;
    Ok(Json(json!({
        "id": user.id,
        "username": user.username,
        "role": user.role,
        "createdAt": user.created_at.format(&Rfc3339).map_err(anyhow::Error::from)?,
    })))
}

async fn delete_user(
    State(state): State<AppState>,
    AdminUser(auth): AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id: Uuid = id
        .parse()
        .map_err(|_| ApiError::NotFound("User not found"))?;
    if id == auth.id {
        return Err(ApiError::BadRequest(
            "Cannot delete your own account".into(),
        ));
    }
    match users::delete_unless_last_admin(&state.pool, id).await? {
        Deletion::NotFound => Err(ApiError::NotFound("User not found")),
        Deletion::LastAdmin => Err(ApiError::BadRequest("Cannot delete the last admin".into())),
        Deletion::Deleted => {
            revoke_sessions(&state, id);
            Ok(Json(json!({ "message": "User deleted successfully" })))
        }
    }
}
