//! `/api/settings` (upstream `routes/settings.ts`): the signed-in user's own settings.
//!
//! `POST /settings/cleanup-stale-jobs` is not ported (no job-queue maintenance yet).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::user_settings::{self, Patch, UserSettings};
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/settings", get(read).post(save))
}

/// `hasListenbrainzToken` and `liveSharingAvailable` are always false in kima-rs (D-103).
fn settings_json(s: &UserSettings) -> Value {
    json!({
        "userId": s.user_id,
        "playbackQuality": s.playback_quality,
        "wifiOnly": s.wifi_only,
        "offlineEnabled": s.offline_enabled,
        "maxCacheSizeMb": s.max_cache_size_mb,
        "shareLiveListening": s.share_live_listening,
        "hasListenbrainzToken": false,
        "liveSharingAvailable": false,
    })
}

async fn read(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let s = user_settings::get(&state.pool, user.id).await?;
    Ok(Json(settings_json(&s)))
}

fn issue(path: &str, message: &str) -> Value {
    json!({ "path": [path], "message": message })
}

/// Validates like upstream's zod schema: a field that is present must have the right type
/// (`null` counts as present for everything but the ignored `listenbrainzToken`); unknown keys
/// are ignored. Returns every failure, not just the first.
fn parse(body: &Value) -> Result<Patch, Vec<Value>> {
    let Some(obj) = body.as_object() else {
        return Err(vec![json!({ "path": [], "message": "Expected object" })]);
    };
    let mut patch = Patch::default();
    let mut issues = Vec::new();

    match obj.get("playbackQuality") {
        None => {}
        Some(Value::String(q)) if matches!(q.as_str(), "original" | "high" | "medium" | "low") => {
            patch.playback_quality = Some(q.clone());
        }
        Some(_) => issues.push(issue(
            "playbackQuality",
            "Invalid enum value. Expected 'original' | 'high' | 'medium' | 'low'",
        )),
    }
    for (key, slot) in [
        ("wifiOnly", &mut patch.wifi_only),
        ("offlineEnabled", &mut patch.offline_enabled),
        ("shareLiveListening", &mut patch.share_live_listening),
    ] {
        match obj.get(key) {
            None => {}
            Some(Value::Bool(b)) => *slot = Some(*b),
            Some(_) => issues.push(issue(key, "Expected boolean")),
        }
    }
    match obj.get("maxCacheSizeMb") {
        None => {}
        Some(Value::Number(n)) => {
            let whole = n
                .as_i64()
                .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64));
            match whole {
                Some(v) if (0..=i64::from(i32::MAX)).contains(&v) => {
                    patch.max_cache_size_mb = Some(v as i32);
                }
                Some(v) if v < 0 => issues.push(issue(
                    "maxCacheSizeMb",
                    "Number must be greater than or equal to 0",
                )),
                Some(_) => issues.push(issue("maxCacheSizeMb", "Number is too large")),
                None => issues.push(issue("maxCacheSizeMb", "Expected integer, received float")),
            }
        }
        Some(_) => issues.push(issue("maxCacheSizeMb", "Expected number")),
    }
    // `listenbrainzToken` is accepted (string or null) and ignored (D-103); anything else is
    // still a type error, as upstream.
    match obj.get("listenbrainzToken") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if !s.is_empty() => {}
        Some(Value::String(_)) => issues.push(issue(
            "listenbrainzToken",
            "String must contain at least 1 character(s)",
        )),
        Some(_) => issues.push(issue("listenbrainzToken", "Expected string")),
    }

    if issues.is_empty() {
        Ok(patch)
    } else {
        Err(issues)
    }
}

async fn save(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let patch = parse(&body).map_err(|details| ApiError::InvalidWithDetails {
        message: "Invalid settings".into(),
        details,
    })?;
    let s = user_settings::upsert(&state.pool, user.id, patch).await?;
    Ok(Json(settings_json(&s)))
}
