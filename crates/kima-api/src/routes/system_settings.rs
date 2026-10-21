use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::settings::{self, SettingsPatch, SystemSettings};
use kima_db::settings_kv::{self, Key, KvSettings};
use serde_json::{Map, Value, json};

use crate::auth::{AdminUser, AuthUser};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

/// The longest secret (characters, after trimming).
const MAX_SECRET_CHARS: usize = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/system-settings", get(show).post(update))
        .route("/system-settings/public-config", get(public_config))
}

/// Any signed-in user. `KIMA_PUBLIC_URL`, empty when unset (D-096).
async fn public_config(State(state): State<AppState>, _user: AuthUser) -> Json<Value> {
    Json(json!({ "publicUrl": state.public_url.as_deref().unwrap_or("") }))
}

fn body(settings: SystemSettings) -> Json<Value> {
    Json(json!({
        "registrationOpen": settings.registration_open,
        "libraryRoot": settings.library_root,
    }))
}

/// M1's keys plus the key-value settings. Secrets are never returned (D-110).
async fn show(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Value>, ApiError> {
    let system = settings::get(&state.pool).await?;
    let kv = state.settings.get().await?;
    Ok(Json(json!({
        "registrationOpen": system.registration_open,
        "libraryRoot": system.library_root,
        "fanartEnabled": kv.fanart_enabled,
        "fanartApiKey": "",
        "hasFanartApiKey": kv.fanart_api_key.is_some(),
        "acoustidApiKey": "",
        "hasAcoustidApiKey": kv.acoustid_api_key.is_some(),
        "autoEnrichMetadata": state.identify.enabled(),
        "autoEnrichMetadataLocked": state.identify.locked(),
        "enrichmentConcurrency": kv.enrichment_concurrency,
        "autotagAutoApply": kv.autotag_auto_apply.as_str(),
    })))
}

/// One validated change.
enum Change {
    Secret(Key, Option<String>),
    Plain(Key, Value),
}

fn invalid(detail: String) -> ApiError {
    ApiError::InvalidSettings(detail)
}

fn secret_change(key: Key, value: &Value) -> Result<Option<Change>, ApiError> {
    match value {
        Value::Null => Ok(Some(Change::Secret(key, None))),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Ok(None);
            }
            if s.chars().count() > MAX_SECRET_CHARS {
                return Err(invalid(format!("{} is too long", key.name())));
            }
            Ok(Some(Change::Secret(key, Some(s.to_string()))))
        }
        _ => Err(invalid(format!("{} must be a string or null", key.name()))),
    }
}

fn changes(fields: &Map<String, Value>) -> Result<(Option<bool>, Vec<Change>), ApiError> {
    let mut registration_open = None;
    if let Some(v) = fields.get("registrationOpen") {
        registration_open = Some(
            v.as_bool()
                .ok_or_else(|| invalid("registrationOpen must be a boolean".into()))?,
        );
    }
    let mut out = Vec::new();
    for key in Key::ALL {
        let Some(value) = fields.get(key.name()) else {
            continue;
        };
        match key {
            Key::AcoustidApiKey | Key::FanartApiKey => out.extend(secret_change(key, value)?),
            Key::FanartEnabled | Key::AutoEnrichMetadata => {
                if !value.is_boolean() {
                    return Err(invalid(format!("{} must be a boolean", key.name())));
                }
                out.push(Change::Plain(key, value.clone()));
            }
            Key::EnrichmentConcurrency => match value.as_i64() {
                Some(1..=5) => out.push(Change::Plain(key, value.clone())),
                _ => {
                    return Err(invalid(
                        "enrichmentConcurrency must be a whole number from 1 to 5".into(),
                    ));
                }
            },
            Key::AutotagAutoApply => match value.as_str() {
                Some("off" | "db" | "files") => out.push(Change::Plain(key, value.clone())),
                _ => {
                    return Err(invalid(
                        "autotagAutoApply must be one of off, db, files".into(),
                    ));
                }
            },
        }
    }
    Ok((registration_open, out))
}

async fn update(
    State(state): State<AppState>,
    _admin: AdminUser,
    ApiJson(fields): ApiJson<Map<String, Value>>,
) -> Result<Json<Value>, ApiError> {
    // Validate everything first, so a rejected body stores nothing.
    let (registration_open, changes) = changes(&fields)?;
    let mut switch = None;
    for change in changes {
        match change {
            Change::Secret(key, value) => {
                settings_kv::set_secret(&state.pool, &state.cipher, key, value.as_deref()).await?;
            }
            Change::Plain(key, value) => {
                settings_kv::set_plain(&state.pool, key, Some(&value)).await?;
                if key == Key::AutoEnrichMetadata {
                    switch = value.as_bool();
                }
            }
        }
    }
    let kv: KvSettings = settings_kv::load(&state.pool, &state.cipher).await?;
    state.settings.replace(kv);
    if let Some(on) = switch {
        state.identify.set_setting(on);
    }
    let patch = SettingsPatch {
        registration_open,
        library_root: None,
    };
    Ok(body(settings::update(&state.pool, patch).await?))
}
