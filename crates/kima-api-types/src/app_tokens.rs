//! `/api/api-keys` contracts from the server handlers: list never contains a secret.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppToken {
    pub id: Uuid,
    pub name: String,
    pub created_at: String,
    pub last_used: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppTokensResponse {
    pub api_keys: Vec<AppToken>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAppToken {
    pub device_name: String,
}

// Deliberately no Debug/Serialize: the create result is ephemeral UI state, not a log/storage DTO.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedAppToken {
    pub api_key: String,
    pub name: String,
    pub created_at: String,
    pub message: String,
}
