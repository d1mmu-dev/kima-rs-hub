//! `/api/auth/*` request and response shapes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::profiles::Profile;

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
    /// The profile the tokens act as: the account's main profile (kima-only, D-242).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
}

/// `POST /onboarding/register` (no refresh token is issued).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterResponse {
    pub token: String,
    pub user: UserSummary,
    /// The new account's main profile (kima-only, D-242).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
}

/// `GET /auth/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    pub id: Uuid,
    pub username: String,
    pub role: String,
    pub onboarding_complete: bool,
    /// RFC 3339 timestamp.
    pub created_at: String,
    /// The profile the session acts as (kima-only, D-242). `id`/`username` stay the account's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
}

/// Body of `POST /auth/change-password`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePassword {
    #[serde(default)]
    pub current_password: String,
    #[serde(default)]
    pub new_password: String,
}

/// Answer of `POST /auth/change-password`: the old tokens are revoked, these replace them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePasswordResponse {
    pub message: String,
    pub token: String,
    pub refresh_token: String,
}
