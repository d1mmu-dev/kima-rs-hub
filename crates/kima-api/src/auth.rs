use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use kima_core::auth::TokenKind;
use kima_db::users;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq)]
pub struct AuthUser {
    pub id: Uuid,
    pub username: String,
    pub role: String,
}

pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// The user a token names, if it is a live access token (or a stream token on a `/stream` path).
pub async fn authenticate(
    state: &AppState,
    token: &str,
    path: &str,
) -> Result<Option<AuthUser>, ApiError> {
    let Ok(claims) = state.jwt.decode(token) else {
        return Ok(None);
    };
    match claims.kind() {
        TokenKind::Refresh => return Ok(None),
        TokenKind::Stream if !path.ends_with("/stream") => return Ok(None),
        TokenKind::Access | TokenKind::Stream => {}
    }
    let Some(user) = users::find_by_id(&state.pool, claims.user_id).await? else {
        return Ok(None);
    };
    if user.token_version != claims.token_version {
        return Ok(None);
    }
    Ok(Some(AuthUser {
        id: user.id,
        username: user.username,
        role: user.role,
    }))
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token =
            bearer_token(&parts.headers).ok_or(ApiError::Unauthorized("Not authenticated"))?;
        authenticate(state, token, parts.uri.path())
            .await?
            .ok_or(ApiError::Unauthorized("Not authenticated"))
    }
}
