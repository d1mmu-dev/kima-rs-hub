use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts, Query};
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

/// The `?token=` value, only on media paths (`.../stream`, `/api/library/cover-art...`), where
/// `<audio>`/`<img>` cannot send headers.
fn query_token(parts: &Parts) -> Option<String> {
    // Inside `nest("/api", ..)` the request URI has the prefix stripped; the original keeps it.
    let path = parts
        .extensions
        .get::<axum::extract::OriginalUri>()
        .map_or_else(|| parts.uri.path(), |original| original.0.path());
    if !(path.ends_with("/stream") || path.starts_with("/api/library/cover-art")) {
        return None;
    }
    let Query(mut params) = Query::<HashMap<String, String>>::try_from_uri(&parts.uri).ok()?;
    params.remove("token").filter(|token| !token.is_empty())
}

/// The request's token: the `Authorization: Bearer` header first, then `?token=` on media paths.
pub fn token_from_request(parts: &Parts) -> Option<String> {
    bearer_token(&parts.headers)
        .map(str::to_owned)
        .or_else(|| query_token(parts))
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

/// An authenticated admin. A missing or bad token is 401 (from `AuthUser`), a non-admin is 403.
#[derive(Debug, Clone, PartialEq)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.role != users::ROLE_ADMIN {
            return Err(ApiError::Forbidden("Admin access required"));
        }
        Ok(Self(user))
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let path = parts.uri.path();
        // A bad header falls through to a valid `?token=` on media paths, as upstream does.
        if let Some(token) = bearer_token(&parts.headers)
            && let Some(user) = authenticate(state, token, path).await?
        {
            return Ok(user);
        }
        let token = query_token(parts).ok_or(ApiError::Unauthorized("Not authenticated"))?;
        authenticate(state, &token, path)
            .await?
            .ok_or(ApiError::Unauthorized("Not authenticated"))
    }
}

/// The TCP peer address. `X-Forwarded-For` is deliberately never read: anyone can forge it.
/// Without connect info (in-process tests) it is `0.0.0.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // The extractor also honours `MockConnectInfo`, which tests layer on.
        let ip = ConnectInfo::<SocketAddr>::from_request_parts(parts, state)
            .await
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip());
        Ok(Self(ip))
    }
}
