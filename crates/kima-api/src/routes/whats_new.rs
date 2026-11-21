//! `/api/whats-new` (M5b Task 15; upstream `routes/whatsNew.ts` at the M5b reference `9450c94b`): the release
//! window. The server decides what is pending, never the client. kima-rs ships its own catalog, empty at the start
//! (owner Q3 = B, D-267); acknowledgements and choices are per profile, admin-only items for an admin account's main
//! profile (owner Q9 = A, D-269); no release asks the GPU choice, so `gpu.accelerate` is an unknown choice (owner
//! Q5 = A, D-268). `requireAuth` upstream: a signed-in session (`AuthUser`).

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use kima_api_types::releases::{
    ChangelogReply, ChoiceReply, CurrentReply, DismissReply, OpenReply, PendingReply,
};
use kima_db::releases::{self, ChoiceError, Viewer};
use kima_mixes::catalog::find_release;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/whats-new/pending", get(pending))
        .route("/whats-new/current", get(current))
        .route("/whats-new/changelog", get(changelog))
        .route("/whats-new/open", get(open))
        .route("/whats-new/{version}/choice", post(choice))
        .route("/whats-new/{version}/dismiss", post(dismiss))
}

fn viewer(user: &AuthUser) -> Viewer {
    Viewer {
        profile: user.profile,
        is_admin: user.is_admin(),
    }
}

fn failure(message: &'static str, error: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %error, "[WhatsNew] {message}");
    ApiError::Failed(message)
}

const UNKNOWN_RELEASE: &str = "Unknown release";

/// GET /whats-new/pending: the newest release, until this profile closes it.
async fn pending(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<PendingReply>, ApiError> {
    let releases = releases::pending(&state.pool, &state.releases, viewer(&user))
        .await
        .map_err(|e| failure("Failed to load releases", e))?;
    Ok(Json(PendingReply { releases }))
}

/// GET /whats-new/current: the newest release with what this profile already settled.
async fn current(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<CurrentReply>, ApiError> {
    releases::current(&state.pool, &state.releases, viewer(&user))
        .await
        .map_err(|e| failure("Failed to load release", e))?
        .map(Json)
        .ok_or(ApiError::NotFound("No release"))
}

/// GET /whats-new/changelog: every release, newest first.
async fn changelog(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<ChangelogReply>, ApiError> {
    releases::changelog(&state.pool, &state.releases, viewer(&user))
        .await
        .map(Json)
        .map_err(|e| failure("Failed to load the changelog", e))
}

/// GET /whats-new/open: what this profile was told to do and the server cannot yet see done.
async fn open(State(state): State<AppState>, user: AuthUser) -> Result<Json<OpenReply>, ApiError> {
    releases::open_requirements(&state.pool, &state.releases, viewer(&user))
        .await
        .map(Json)
        .map_err(|e| failure("Failed to load open requirements", e))
}

/// The JSON body as `express.json()` leaves it: an empty body is `{}`; a malformed one is a 400.
pub(crate) fn json_body(body: &Bytes) -> Result<Value, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    serde_json::from_slice(body).map_err(|e| ApiError::Invalid(e.to_string()))
}

/// The type name zod reports as `received`.
pub(crate) fn received(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// A zod `invalid_type` issue.
pub(crate) fn invalid_type(path: &[&str], expected: &str, got: Option<&Value>) -> Value {
    let message = if got.is_none() {
        "Required".to_string()
    } else {
        format!("Expected {expected}, received {}", received(got))
    };
    json!({ "code": "invalid_type", "expected": expected, "received": received(got), "path": path, "message": message })
}

/// zod's `z.object({ key: z.string().min(1), enabled: z.boolean() })`, every issue reported.
fn parse_choice(body: &Value) -> Result<(String, bool), ApiError> {
    let Some(obj) = body.as_object() else {
        return Err(ApiError::InvalidWithDetails {
            message: "Invalid request".into(),
            details: vec![invalid_type(&[], "object", Some(body))],
        });
    };
    let mut issues = Vec::new();
    let key = match obj.get("key") {
        Some(Value::String(k)) if !k.is_empty() => Some(k.clone()),
        Some(Value::String(_)) => {
            issues.push(json!({ "code": "too_small", "minimum": 1, "type": "string", "inclusive": true,
                "exact": false, "message": "String must contain at least 1 character(s)", "path": ["key"] }));
            None
        }
        other => {
            issues.push(invalid_type(&["key"], "string", other));
            None
        }
    };
    let enabled = match obj.get("enabled") {
        Some(Value::Bool(b)) => Some(*b),
        other => {
            issues.push(invalid_type(&["enabled"], "boolean", other));
            None
        }
    };
    match (key, enabled) {
        (Some(k), Some(e)) => Ok((k, e)),
        _ => Err(ApiError::InvalidWithDetails {
            message: "Invalid request".into(),
            details: issues,
        }),
    }
}

/// POST /whats-new/{version}/choice: an explicit answer. The release is looked up before the body is read.
async fn choice(
    State(state): State<AppState>,
    user: AuthUser,
    Path(version): Path<String>,
    body: Bytes,
) -> Result<Json<ChoiceReply>, ApiError> {
    const ERROR: &str = "Failed to record choice";
    if find_release(&state.releases, &version).is_none() {
        return Err(ApiError::NotFound(UNKNOWN_RELEASE));
    }
    let (key, enabled) = parse_choice(&json_body(&body)?)?;
    match releases::set_choice(
        &state.pool,
        &state.releases,
        user.profile,
        &version,
        &key,
        enabled,
    )
    .await
    {
        Ok(()) => Ok(Json(ChoiceReply {
            key,
            enabled,
            decided_by: releases::EXPLICIT.into(),
        })),
        Err(ChoiceError::Unknown) => Err(ApiError::BadRequest("Unknown choice".into())),
        Err(ChoiceError::Db(e)) => Err(failure(ERROR, e)),
    }
}

/// POST /whats-new/{version}/dismiss: close it; unanswered choices are settled at their default.
async fn dismiss(
    State(state): State<AppState>,
    user: AuthUser,
    Path(version): Path<String>,
) -> Result<Json<DismissReply>, ApiError> {
    const ERROR: &str = "Failed to dismiss release";
    if find_release(&state.releases, &version).is_none() {
        return Err(ApiError::NotFound(UNKNOWN_RELEASE));
    }
    let recorded_defaults =
        releases::dismiss(&state.pool, &state.releases, viewer(&user), &version)
            .await
            .map_err(|e| failure(ERROR, e))?
            .unwrap_or_default();
    Ok(Json(DismissReply {
        version,
        dismissed: true,
        recorded_defaults,
    }))
}
