//! `/api/auth/*` request and response shapes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Body of `POST /auth/login` and `POST /onboarding/register`. Absent fields read as empty
/// strings; the server rejects those itself with its own message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// The `user` object inside login and register responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSummary {
    pub id: Uuid,
    pub username: String,
    pub role: String,
    pub onboarding_complete: bool,
}

/// `POST /auth/login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    pub token: String,
    pub refresh_token: String,
    pub user: UserSummary,
}

/// `POST /onboarding/register` (no refresh token is issued).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterResponse {
    pub token: String,
    pub user: UserSummary,
}

/// `GET /auth/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    pub id: Uuid,
    pub username: String,
    pub role: String,
    pub onboarding_complete: bool,
    /// Always `null` today; kept so the shape round-trips.
    pub enrichment_settings: Option<serde_json::Value>,
    /// RFC 3339 timestamp.
    pub created_at: String,
}
