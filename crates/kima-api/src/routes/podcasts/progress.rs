use axum::Json;
use axum::extract::{Path, State};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::routes::stats::args::js_number;
use crate::state::AppState;
use kima_api_types::stats::js_iso;
use kima_db::podcasts::{clear_progress, save_progress};

/// Coerce a JSON value the way zod's `z.coerce.number()` does (JS `Number(x)`).
fn number_from_json(raw: &Value) -> f64 {
    match raw {
        Value::Null => 0.0,
        Value::Array(a) if a.is_empty() => 0.0,
        Value::Array(_) => f64::NAN,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::String(s) => js_number(s),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Object(_) => f64::NAN,
    }
}

/// Coerce a "currentTime"/"duration" value to a finite, non-negative number.
fn coerce_number(raw: &Value) -> Result<f64, ApiError> {
    let n = number_from_json(raw);
    if n.is_nan() || !n.is_finite() || n < 0.0 {
        Err(ApiError::BadRequest(
            "currentTime (seconds) is required".into(),
        ))
    } else {
        Ok(n)
    }
}

/// Render a number the way `JSON.stringify` does: no `.0` on whole numbers, so a round-tripped
/// value compares equal to `json!(650)` rather than only `json!(650.0)`.
fn js_number_value(n: f64) -> Value {
    if n.is_finite() && n == n.trunc() && n.abs() < 2f64.powi(53) {
        json!(n as i64)
    } else {
        json!(n)
    }
}

fn progress_of(current_time: f64, duration: f64, is_finished: bool) -> Value {
    let progress = if duration > 0.0 {
        ((current_time / duration) * 100.0).min(100.0)
    } else {
        0.0
    };
    json!({
        "currentTime": js_number_value(current_time),
        "progress": js_number_value(progress),
        "isFinished": is_finished,
        "lastPlayedAt": js_iso(OffsetDateTime::now_utc()),
    })
}

/// POST /api/podcasts/{podcast}/episodes/{episode}/progress
///
/// Upstream ignores the `podcast` id in the path: progress is keyed by episode and user only.
pub async fn progress(
    State(state): State<AppState>,
    user: AuthUser,
    Path((_podcast, episode)): Path<(String, String)>,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let episode = Uuid::parse_str(&episode).map_err(|_| ApiError::NotFound("Episode not found"))?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM podcast_episodes WHERE id = $1)")
            .bind(episode)
            .fetch_one(&state.pool)
            .await?;
    if !exists {
        return Err(ApiError::NotFound("Episode not found"));
    }

    let current_time = match body.get("currentTime") {
        Some(v) => coerce_number(v)?,
        None => {
            return Err(ApiError::BadRequest(
                "currentTime (seconds) is required".into(),
            ));
        }
    };
    let provided_duration = match body.get("duration") {
        None => None,
        Some(raw) => Some(coerce_number(raw)?),
    };
    let is_finished_override = match body.get("isFinished") {
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => {
            return Err(ApiError::BadRequest("isFinished must be a boolean".into()));
        }
        None => None,
    };

    let stored = kima_db::podcasts::progress(&state.pool, user.id, episode).await?;
    let duration = provided_duration.unwrap_or_else(|| stored.map(|s| s.1).unwrap_or(0.0));
    // Finished is decided from THIS request's duration only (upstream: "a save without one is
    // finished only when the client says so"), never a stored length from an earlier save.
    let is_finished = is_finished_override.unwrap_or(false)
        || provided_duration.is_some_and(|d| d > 0.0 && current_time >= 0.95 * d);

    save_progress(
        &state.pool,
        user.id,
        episode,
        current_time,
        duration,
        is_finished,
    )
    .await?;

    Ok(Json(json!({
        "success": true,
        "progress": progress_of(current_time, duration, is_finished),
    })))
}

/// DELETE /api/podcasts/{podcast}/episodes/{episode}/progress — idempotent, never 404s.
pub async fn clear(
    State(state): State<AppState>,
    user: AuthUser,
    Path((_podcast, episode)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let episode = Uuid::parse_str(&episode).map_err(|_| ApiError::NotFound("Episode not found"))?;
    clear_progress(&state.pool, user.id, episode).await?;
    Ok(Json(json!({
        "success": true,
        "message": "Progress removed",
    })))
}
