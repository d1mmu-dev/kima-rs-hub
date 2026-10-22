use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kima_db::settings::{self, SettingsPatch, SystemSettings};
use kima_db::settings_kv::{self, Key, KvSettings};
use kima_ident::fanart::{self, TestResult};
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
        .route("/system-settings/test-fanart", post(test_fanart))
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

fn changes(
    fields: &Map<String, Value>,
    locked: bool,
) -> Result<(Option<bool>, Vec<Change>), ApiError> {
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
                // Locked by KIMA_IDENTIFY=off, GET reports false whatever is stored, and the
                // frontend posts that back with every save: ignore it so the stored value lives.
                if key == Key::AutoEnrichMetadata && locked && *value == Value::Bool(false) {
                    continue;
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
    let (registration_open, changes) = changes(&fields, state.identify.locked())?;
    let mut switch = None;
    let mut concurrency = None;
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
                if key == Key::EnrichmentConcurrency {
                    concurrency = value.as_u64();
                }
            }
        }
    }
    let kv: KvSettings = settings_kv::load(&state.pool, &state.cipher).await?;
    state.settings.replace(kv);
    if let Some(on) = switch {
        let was_on = state.identify.enabled();
        state.identify.set_setting(on);
        // Turning identification on wakes the coordinator (spec §15.3). The settings are
        // already saved, so a failed kick is only logged: `enrich-kick` is the backstop.
        if !was_on
            && state.identify.enabled()
            && let Err(error) = kima_ident::coordinator::kick(&state.pool).await
        {
            tracing::warn!(%error, "could not kick the enrichment coordinator");
        }
    }
    if let Some(n) = concurrency {
        // Validated to 1-5 above.
        state.net.set_concurrency(n as u8);
    }
    let patch = SettingsPatch {
        registration_open,
        library_root: None,
    };
    Ok(body(settings::update(&state.pool, patch).await?))
}

/// The key a test-fanart body names: `fanartApiKey` trimmed, or `None` when the body is empty or
/// the field is absent, `null` or blank. A body that is not a JSON object, or a key that is not a
/// string or is too long, is a bare 400: no parser message, which could quote the key, is shown.
fn typed_key(body: &[u8]) -> Result<Option<String>, ApiError> {
    let bad = || ApiError::BadRequest("Invalid request".into());
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(body) else {
        return Err(bad());
    };
    match fields.get(Key::FanartApiKey.name()) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.chars().count() > MAX_SECRET_CHARS {
                return Err(bad());
            }
            Ok((!s.is_empty()).then(|| s.to_string()))
        }
        Some(_) => Err(bad()),
    }
}

/// `POST /system-settings/test-fanart` (spec §15.2, D-110): asks fanart.tv for The Beatles with
/// the body's key, else the stored one (enabled or not). Never cached; the key is never echoed.
async fn test_fanart(
    State(state): State<AppState>,
    _admin: AdminUser,
    body: Bytes,
) -> Result<Response, ApiError> {
    let key = match typed_key(&body)? {
        Some(k) => Some(k),
        None => state
            .settings
            .get()
            .await?
            .fanart_api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string),
    };
    let Some(key) = key else {
        return Err(ApiError::BadRequest("API key is required".into()));
    };
    // Checked here as well as in `Net`, so an off switch asks nothing and says why.
    if !state.identify.enabled() {
        return Err(ApiError::Conflict("Outbound lookups are turned off"));
    }
    Ok(match fanart::test_key(&state.net, &key).await {
        TestResult::Ok => Json(json!({
            "success": true,
            "message": "Fanart.tv connection successful",
        }))
        .into_response(),
        TestResult::Rejected => {
            ApiError::BadRequest("Fanart.tv rejected the API key".into()).into_response()
        }
        TestResult::NoAnswer => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Fanart.tv did not answer" })),
        )
            .into_response(),
    })
}
