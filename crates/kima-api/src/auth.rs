use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts, Query};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use kima_core::auth::TokenKind;
use kima_core::profile::{AccountId, ProfileId, ProfileKind};
use kima_db::users;

use crate::error::ApiError;
use crate::state::AppState;

/// The caller of a request: an account and the profile it acts as (D-241). Per-profile data
/// is keyed by `profile`, credentials and account state by `account`; on the main profile the
/// two ids are equal, so the types keep them apart.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthUser {
    pub account: AccountId,
    pub profile: ProfileId,
    pub profile_kind: ProfileKind,
    /// The account's username.
    pub username: String,
    /// The account's role.
    pub role: String,
    /// The account's `token_version` that this session was checked against.
    pub token_version: i32,
}

impl AuthUser {
    pub fn is_main_profile(&self) -> bool {
        self.profile_kind == ProfileKind::Main
    }

    /// Admin rights: an admin account acting as its main profile (D-241). A kids or standard
    /// profile of an admin account is not an admin.
    pub fn is_admin(&self) -> bool {
        self.role == users::ROLE_ADMIN && self.is_main_profile()
    }
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

/// Whether `path` is a media path where `<audio>`/`<img>` cannot send headers: a stream, a
/// podcast preview sample, the cover-art proxy, or a legacy podcast cover redirect link
/// (Task 15) that itself carries a `?token=` on to the proxy.
fn is_media_path(path: &str) -> bool {
    path.ends_with("/stream")
        || path.ends_with("/snippet")
        || path.ends_with("/cover")
        || path.starts_with("/api/library/cover-art")
}

/// The `?token=` value, only on media paths ([`is_media_path`]).
fn query_token(parts: &Parts) -> Option<String> {
    // Inside `nest("/api", ..)` the request URI has the prefix stripped; the original keeps it.
    let path = parts
        .extensions
        .get::<axum::extract::OriginalUri>()
        .map_or_else(|| parts.uri.path(), |original| original.0.path());
    if !is_media_path(path) {
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

/// The session a token names, if it is a live access token (or a stream token on a media
/// path): its account's `token_version` still matches and its profile still belongs to that
/// account. A token without `profileId` is the main profile's.
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
        TokenKind::Stream if !is_media_path(path) => return Ok(None),
        TokenKind::Access | TokenKind::Stream => {}
    }
    let Some(session) =
        users::find_session(&state.pool, claims.account(), claims.profile()).await?
    else {
        return Ok(None);
    };
    if session.token_version != claims.token_version {
        return Ok(None);
    }
    Ok(Some(AuthUser {
        account: session.account,
        profile: session.profile,
        profile_kind: session.profile_kind,
        username: session.username,
        role: session.role,
        token_version: session.token_version,
    }))
}

/// An authenticated admin ([`AuthUser::is_admin`]: role admin on the main profile). A missing
/// or bad token is 401 (from `AuthUser`), anyone else is 403.
#[derive(Debug, Clone, PartialEq)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !user.is_admin() {
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
        let ip = match ConnectInfo::<SocketAddr>::from_request_parts(parts, state).await {
            Ok(info) => info.0.ip(),
            Err(_) => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    tracing::warn!(
                        "no connect info: every client shares one address; serve with \
                         into_make_service_with_connect_info"
                    );
                });
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            }
        };
        Ok(Self(ip))
    }
}
