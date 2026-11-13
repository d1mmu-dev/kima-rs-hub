//! `/api/enrichment` (spec §16.2, upstream `routes/enrichment.ts`): progress, status, the
//! operator controls, the failures list and the concurrency setting. Reads need a signed-in
//! user, writes an admin. Also the MetadataEditor's override and reset routes (spec §16.3).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use kima_db::enrichment::{self, FailureFilter, State as Pipeline};
use kima_ident::coordinator::{self, control, control::ControlError};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::auth::{AdminUser, AuthUser};
use crate::error::ApiError;
use crate::state::AppState;

mod editor;

/// How long a progress answer is reused (upstream memoises it too).
const PROGRESS_TTL: Duration = Duration::from_secs(5);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/enrichment/progress", get(progress))
        .route("/enrichment/status", get(status))
        .route("/enrichment/pause", post(pause))
        .route("/enrichment/resume", post(resume))
        .route("/enrichment/stop", post(stop))
        .route("/enrichment/sync", post(sync))
        .route(
            "/enrichment/retry-failed-and-blocked",
            post(retry_failed_and_blocked),
        )
        .route(
            "/enrichment/re-decide-everything",
            post(redecide_everything),
        )
        .route("/enrichment/wipe-and-rerun", post(wipe_and_rerun))
        .route("/enrichment/rerun", post(rerun))
        .route("/enrichment/failures", get(failures))
        .route("/enrichment/failures/counts", get(failure_counts))
        .route(
            "/enrichment/concurrency",
            get(get_concurrency).put(put_concurrency),
        )
        .route("/enrichment/artists/{id}/metadata", put(editor::put_artist))
        .route("/enrichment/albums/{id}/metadata", put(editor::put_album))
        .route("/enrichment/tracks/{id}/metadata", put(editor::put_track))
        .route("/enrichment/artists/{id}/reset", post(editor::reset_artist))
        .route("/enrichment/albums/{id}/reset", post(editor::reset_album))
        .route("/enrichment/tracks/{id}/reset", post(editor::reset_track))
}

fn time_json(t: Option<OffsetDateTime>) -> Value {
    t.and_then(|t| t.format(&Rfc3339).ok())
        .map_or(Value::Null, Value::String)
}

/// The `enrichment_state` row as clients see it.
fn state_json(s: &Pipeline) -> Value {
    json!({
        "status": s.status.as_str(),
        "currentPhase": s.current_phase.map(|p| p.as_str()),
        "startedAt": time_json(s.started_at),
        "pausedAt": time_json(s.paused_at),
        "stoppedAt": time_json(s.stopped_at),
        "lastError": s.last_error,
        "lastActivity": time_json(Some(s.last_activity)),
    })
}

fn control_error(e: ControlError) -> ApiError {
    match e {
        ControlError::WrongState(message) => ApiError::BadRequest(message),
        ControlError::Db(e) => e.into(),
    }
}

async fn progress(State(state): State<AppState>, _user: AuthUser) -> Result<Json<Value>, ApiError> {
    {
        let memo = state
            .progress_memo
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((at, answer)) = memo.as_ref()
            && at.elapsed() < PROGRESS_TTL
        {
            return Ok(Json(answer.clone()));
        }
    }
    let p = enrichment::progress(&state.pool, state.fpcalc).await?;
    let answer = json!({
        "source": p.source,
        "artists": p.artists,
        "trackTags": p.track_tags,
        "audioAnalysis": p.audio_analysis,
        "clapEmbeddings": p.clap_embeddings,
        "fingerprints": p.fingerprints,
        "similarity": p.similarity,
        "coreComplete": p.core_complete,
        "isFullyComplete": p.is_fully_complete,
    });
    *state
        .progress_memo
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), answer.clone()));
    Ok(Json(answer))
}

async fn status(State(state): State<AppState>, _user: AuthUser) -> Result<Json<Value>, ApiError> {
    let s = enrichment::state(&state.pool).await?;
    let counts = enrichment::status_counts(&state.pool).await?;
    let mut out = state_json(&s);
    out["hung"] = json!(coordinator::hung(&s, OffsetDateTime::now_utc()));
    out["artists"] = json!(counts.artists);
    out["tracks"] = json!(counts.tracks);
    out["audio"] = json!(counts.audio);
    out["mlCapabilities"] = json!(
        state
            .ident
            .as_ref()
            .and_then(kima_ident::stages::embedding::cached_status)
    );
    Ok(Json(out))
}

async fn pause(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Value>, ApiError> {
    let s = control::pause(&state.pool).await.map_err(control_error)?;
    control::announce_state(&state.progress_events, &s);
    Ok(Json(
        json!({"message": "Enrichment paused", "state": state_json(&s)}),
    ))
}

async fn resume(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Value>, ApiError> {
    let s = control::resume(&state.pool).await.map_err(control_error)?;
    control::announce_state(&state.progress_events, &s);
    Ok(Json(
        json!({"message": "Enrichment resumed", "state": state_json(&s)}),
    ))
}

async fn stop(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Value>, ApiError> {
    let s = control::stop(&state.pool).await?;
    control::announce_state(&state.progress_events, &s);
    Ok(Json(
        json!({"message": "Enrichment stopping...", "state": state_json(&s)}),
    ))
}

async fn sync(State(state): State<AppState>, _admin: AdminUser) -> Result<Json<Value>, ApiError> {
    let counts = control::sync(&state.pool).await?;
    control::announce(&state.pool, &state.progress_events).await;
    Ok(Json(json!({
        "message": "Incremental sync started",
        "description": "Processing new and pending items only",
        "result": {"artists": counts.artists, "tracks": counts.tracks, "audioQueued": 0},
    })))
}

async fn retry_failed_and_blocked(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    let enrichment_queued = control::retry_failed_and_blocked(&state.ident_ctx()).await?;
    // Feed refreshes are not identification steps: this control-plane action also re-queues
    // every followed show with a recorded refresh failure right now, ignoring the sweep's
    // hourly dispatch backoff. A show already queued or running is not queued again.
    let podcast_queued = kima_podcast::refresh::retry_failed(&state.pool).await?;
    state.invalidate_progress();
    control::announce(&state.pool, &state.progress_events).await;
    Ok(Json(json!({
        "message": "Failed and blocked work re-queued",
        "description": format!("{enrichment_queued} stage questions handed to the queue"),
        "queued": enrichment_queued + podcast_queued,
    })))
}

async fn redecide_everything(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    let reopened = control::redecide_everything(&state.ident_ctx()).await?;
    state.invalidate_progress();
    control::announce(&state.pool, &state.progress_events).await;
    Ok(Json(json!({
        "message": "Every stage question re-opened",
        "description": format!("{reopened} stage records reopened"),
        "reopened": reopened,
    })))
}

async fn wipe_and_rerun(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    let wiped = control::wipe_and_rerun(&state.pool).await?;
    state.invalidate_progress();
    control::announce(&state.pool, &state.progress_events).await;
    Ok(Json(json!({
        "artists": wiped.artists,
        "tracks": wiped.tracks,
        "vectors": 0,
    })))
}

/// `POST /enrichment/rerun {step}` (upstream `routes/enrichment.ts`, `5d348720`): re-run the
/// enrichment from one of its six steps. Admin only. A name outside the six is a 400 naming them;
/// a step whose subsystem has no reset here yet (`analysis`, `vectors`, `moods`, `similarity`) is
/// a 409 that changes nothing (M-SYNC Task 14, D-112).
async fn rerun(
    State(state): State<AppState>,
    _admin: AdminUser,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let step = body
        .ok()
        .and_then(|Json(b)| b.get("step").and_then(Value::as_str).map(str::to_owned))
        .and_then(|s| control::Step::parse(&s))
        .ok_or_else(|| {
            ApiError::BadRequest(format!("step must be one of: {}", control::Step::list()))
        })?;
    let wiped = match control::rerun(&state.pool, step).await {
        Ok(w) => w,
        Err(control::RerunError::Unavailable(step)) => {
            return Err(ApiError::ConflictMessage(format!(
                "The {} step is not available yet",
                step.id()
            )));
        }
        Err(control::RerunError::Db(error)) => {
            if kima_db::is_retryable_conflict(&error) {
                return Err(ApiError::Busy);
            }
            tracing::error!(%error, "re-run enrichment failed");
            return Err(ApiError::Failed("Failed to re-run enrichment"));
        }
    };
    state.invalidate_progress();
    control::announce(&state.pool, &state.progress_events).await;
    Ok(Json(json!({
        "artists": wiped.artists,
        "tracks": wiped.tracks,
        "vectors": 0,
        "message": format!("Re-running from {}", step.name()),
    })))
}

/// `limit` and `offset` as upstream's `parseInt`: an unreadable value is as if absent.
async fn failures(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let number = |key: &str, default: i64| {
        q.get(key)
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(default)
    };
    let filter = FailureFilter {
        entity_type: q.get("entityType").filter(|v| !v.is_empty()).cloned(),
        limit: number("limit", 100),
        offset: number("offset", 0),
    };
    let (failures, total) = enrichment::failures(&state.pool, &filter).await?;
    Ok(Json(json!({"failures": failures, "total": total})))
}

async fn failure_counts(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(enrichment::failure_counts(&state.pool).await?)))
}

/// The concurrency answer: `10c` artists and `60c` tracks a minute.
fn concurrency_json(c: u64) -> Value {
    let (artists, tracks) = (10 * c, 60 * c);
    json!({
        "concurrency": c,
        "estimatedSpeed": format!("~{artists} artists/min, ~{tracks} tracks/min"),
        "artistsPerMin": artists,
        "tracksPerMin": tracks,
    })
}

async fn get_concurrency(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Value>, ApiError> {
    let settings = state.settings.get().await?;
    Ok(Json(concurrency_json(u64::from(
        settings.enrichment_concurrency.max(1),
    ))))
}

async fn put_concurrency(
    State(state): State<AppState>,
    _admin: AdminUser,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let invalid = || ApiError::BadRequest("Missing or invalid 'concurrency' parameter".into());
    let Json(body) = body.map_err(|_| invalid())?;
    // Upstream: `!concurrency || typeof concurrency !== "number"`; zero is refused too.
    let n = body
        .get("concurrency")
        .and_then(Value::as_f64)
        .filter(|n| *n != 0.0 && n.is_finite())
        .ok_or_else(invalid)?;
    let stored = control::set_concurrency(&state.ident_ctx(), n.floor() as i64).await?;
    Ok(Json(concurrency_json(u64::from(stored))))
}
