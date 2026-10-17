//! `/api/api-keys` (upstream `routes/apiKeys.ts`): per-app tokens for the Subsonic API. The
//! plaintext token is returned once, by `POST`, and is never stored in the clear or logged.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use kima_core::app_token;
use kima_db::api_keys::{self, TooMany};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::plays::iso;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api-keys", get(list).post(create))
        .route("/api-keys/{id}", delete(revoke))
}

const NOT_FOUND: &str = "API key not found or already deleted";
const MAX_NAME_CHARS: usize = 100;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBody {
    #[serde(default)]
    device_name: String,
}

async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(body): ApiJson<CreateBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = body.device_name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("Device name is required".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::BadRequest(
            "Device name must be at most 100 characters".into(),
        ));
    }

    let token = app_token::generate();
    let id = Uuid::now_v7();
    let secret_enc = app_token::seal(&state.cipher, id, &token);
    let key = api_keys::create(
        &state.pool,
        id,
        auth.id,
        name,
        &app_token::hash(&token),
        &secret_enc,
    )
    .await?
    .map_err(|TooMany| ApiError::BadRequest("Too many app tokens; revoke one first".into()))?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "apiKey": token,
            "name": key.name,
            "createdAt": iso(key.created_at),
            "message": "API key created successfully. Save this key - you won't see it again!",
        })),
    ))
}

async fn list(State(state): State<AppState>, auth: AuthUser) -> Result<Json<Value>, ApiError> {
    let keys = api_keys::list_for_user(&state.pool, auth.id).await?;
    let keys: Vec<Value> = keys
        .into_iter()
        .map(|key| {
            json!({
                "id": key.id,
                "name": key.name,
                "createdAt": iso(key.created_at),
                "lastUsed": key.last_used_at.map(iso),
            })
        })
        .collect();
    Ok(Json(json!({ "apiKeys": keys })))
}

async fn revoke(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id: Uuid = id.parse().map_err(|_| ApiError::NotFound(NOT_FOUND))?;
    if !api_keys::delete_owned(&state.pool, id, auth.id).await? {
        return Err(ApiError::NotFound(NOT_FOUND));
    }
    Ok(Json(json!({ "message": "API key revoked successfully" })))
}
