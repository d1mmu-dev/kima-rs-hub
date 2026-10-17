//! The library organizer (spec §5), admin only, under `/api/library/organizer/*`. Upstream has
//! no such feature; its `POST /library/organize` (Soulseek singles, M7) is a different thing.
//!
//! Previews, applies, undos and root switches run as `library` jobs; these endpoints validate,
//! move the run to its next state and queue the job.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use kima_db::attention;
use kima_db::organizer::{self as db, RowView, Run};
use kima_db::users;
use kima_library::organize::apply::{self, Action};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Paging, parse_id};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library/organizer/capabilities", get(capabilities))
        .route("/library/organizer/preview", post(preview))
        .route("/library/organizer/runs", get(list))
        .route("/library/organizer/runs/{id}", get(detail))
        .route("/library/organizer/runs/{id}/apply", post(apply_run))
        .route("/library/organizer/runs/{id}/undo", post(undo_run))
        .route(
            "/library/organizer/runs/{id}/switch-root",
            post(switch_root),
        )
}

const READ_ONLY: &str = "Library folder is read-only; use copy mode";

fn admin(user: &AuthUser) -> Result<(), ApiError> {
    if user.role != users::ROLE_ADMIN {
        return Err(ApiError::Forbidden("Admin access required"));
    }
    Ok(())
}

fn rfc3339(t: time::OffsetDateTime) -> Option<String> {
    t.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

fn run_json(r: &Run) -> Value {
    json!({
        "id": r.id,
        "mode": r.mode,
        "state": r.state,
        "sourceRoot": r.source_root,
        "targetRoot": r.target_root,
        "createdAt": rfc3339(r.created_at),
        "finishedAt": r.finished_at.and_then(rfc3339),
        "switchedAt": r.switched_at.and_then(rfc3339),
        "summary": r.summary,
        "error": r.error,
    })
}

/// A journal row as the preview table shows it: paths, state, and for a track its album and
/// the attention codes it raises by itself (`guessed`). Sidecars carry `null` album fields.
fn row_json(v: &RowView) -> Value {
    let r = &v.row;
    let guessed: Vec<&str> = match (&v.guessed, v.track_no) {
        (Some(g), Some(no)) => attention::track_codes(&g.0, v.album_year, no),
        _ => Vec::new(),
    };
    // A rename that only changes letter case (kept in step with the planner).
    let case_only =
        r.from_path != r.to_path && r.from_path.to_lowercase() == r.to_path.to_lowercase();
    json!({
        "id": r.id,
        "trackId": r.track_id,
        "from": r.from_path,
        "to": r.to_path,
        "caseOnly": case_only,
        "state": r.state,
        "note": r.note,
        "collision": r.collision,
        "albumId": v.album_id,
        "album": v.album,
        "artist": v.artist,
        "guessed": guessed,
    })
}

/// `a` lies inside `b` or the other way round.
fn overlaps(a: &FsPath, b: &FsPath) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// How long a capabilities probe result is reused.
const PROBE_TTL: Duration = Duration::from_secs(60);

type ProbeCache = Mutex<HashMap<PathBuf, (Instant, bool)>>;

/// Probe results per folder, so polling the capabilities does not write a probe file per call.
static PROBES: LazyLock<ProbeCache> = LazyLock::new(|| Mutex::new(HashMap::new()));

async fn cached(dir: PathBuf, probe: impl FnOnce(PathBuf) -> bool + Send + 'static) -> bool {
    if let Ok(cache) = PROBES.lock()
        && let Some((at, ok)) = cache.get(&dir)
        && at.elapsed() < PROBE_TTL
    {
        return *ok;
    }
    let d = dir.clone();
    let ok = tokio::task::spawn_blocking(move || probe(d))
        .await
        .unwrap_or(false);
    if let Ok(mut cache) = PROBES.lock() {
        cache.insert(dir, (Instant::now(), ok));
    }
    ok
}

async fn writable(dir: PathBuf) -> bool {
    tokio::task::spawn_blocking(move || dir.is_dir() && apply::writable(&dir))
        .await
        .unwrap_or(false)
}

/// The copy target, created if needed, when it can be written to.
async fn copy_target(state: &AppState) -> Option<PathBuf> {
    let target = state.organize_target.clone()?;
    let t = target.clone();
    let ok = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&t).is_ok() && apply::writable(&t)
    })
    .await
    .unwrap_or(false);
    ok.then_some(target)
}

/// `{copyAvailable, moveAvailable, libraryRoot, targetRoot}`. Move needs a writable library
/// folder, tested with a probe file (a read-only mount keeps its mode bits).
async fn capabilities(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Value>, ApiError> {
    admin(&user)?;
    let root = state.library_root();
    let move_ok = match &root {
        Some(r) => cached(r.clone(), |d| d.is_dir() && apply::writable(&d)).await,
        None => false,
    };
    let copy_ok = match (&root, &state.organize_target) {
        (Some(_), Some(t)) => {
            cached(t.clone(), |d| {
                std::fs::create_dir_all(&d).is_ok() && apply::writable(&d)
            })
            .await
        }
        _ => false,
    };
    Ok(Json(json!({
        "copyAvailable": copy_ok,
        "moveAvailable": move_ok,
        "libraryRoot": root.map(|p| p.display().to_string()),
        "targetRoot": state.organize_target.as_ref().map(|p| p.display().to_string()),
    })))
}

#[derive(Debug, Deserialize)]
struct PreviewBody {
    mode: String,
}

async fn busy(state: &AppState) -> Result<bool, ApiError> {
    Ok(!db::interrupted_runs(&state.pool).await?.is_empty())
}

/// Creates a run and queues its planning. `202 {runId, jobId}`.
async fn preview(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<PreviewBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&user)?;
    let root = state
        .library_root()
        .ok_or(ApiError::Conflict("Library folder is not configured"))?;
    let target = match body.mode.as_str() {
        db::MODE_COPY => {
            let target = copy_target(&state).await.ok_or(ApiError::Conflict(
                "Copy mode needs a writable KIMA_ORGANIZE_TARGET folder",
            ))?;
            if overlaps(&target, &root) {
                return Err(ApiError::Conflict(
                    "The organize target must be outside the library folder",
                ));
            }
            target
        }
        db::MODE_MOVE => {
            if !writable(root.clone()).await {
                return Err(ApiError::Conflict(READ_ONLY));
            }
            root.clone()
        }
        _ => return Err(ApiError::BadRequest("mode must be copy or move".into())),
    };
    if busy(&state).await? {
        return Err(ApiError::Conflict("Another organizer run is in progress"));
    }
    let run = db::create_run(
        &state.pool,
        Uuid::now_v7(),
        &body.mode,
        &root.display().to_string(),
        &target.display().to_string(),
        Some(user.id),
    )
    .await?;
    let job = apply::enqueue(&state.pool, run.id, Action::Preview).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "runId": run.id, "jobId": job })),
    ))
}

/// The newest 20 runs.
async fn list(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    admin(&user)?;
    let runs = db::list_runs(&state.pool, 20).await?;
    Ok(Json(
        json!({ "runs": runs.iter().map(run_json).collect::<Vec<_>>() }),
    ))
}

async fn load(state: &AppState, id: &str) -> Result<Run, ApiError> {
    let not_found = || ApiError::NotFound("Organizer run not found");
    db::get_run(&state.pool, parse_id(id).ok_or_else(not_found)?)
        .await?
        .ok_or_else(not_found)
}

/// The run, plus one page of its journal (`rows`, `total`, `offset`, `limit`).
async fn detail(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(paging): Query<Paging>,
) -> Result<Json<Value>, ApiError> {
    admin(&user)?;
    let run = load(&state, &id).await?;
    let (offset, limit) = (paging.offset(), paging.limit(200));
    let (rows, total) = db::rows(&state.pool, run.id, offset, limit).await?;
    let mut v = run_json(&run);
    v["lastAction"] = json!(if db::has_undo_rows(&state.pool, run.id).await? {
        "undo"
    } else {
        "apply"
    });
    v["rows"] = rows.iter().map(row_json).collect();
    v["total"] = json!(total);
    v["offset"] = json!(offset);
    v["limit"] = json!(limit);
    Ok(Json(v))
}

fn accepted(run: Uuid, job: Uuid) -> (StatusCode, Json<Value>) {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "runId": run, "jobId": job })),
    )
}

/// Starts (or, for a failed run, resumes) applying a preview.
async fn apply_run(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&user)?;
    let run = load(&state, &id).await?;
    if run.switched_at.is_some() {
        return Err(ApiError::Conflict("This run was already switched to"));
    }
    if run.mode == db::MODE_MOVE && !writable(PathBuf::from(&run.source_root)).await {
        return Err(ApiError::Conflict(READ_ONLY));
    }
    if state.library_root().as_deref() != Some(FsPath::new(&run.source_root)) {
        return Err(ApiError::Conflict(
            "The library folder changed since this preview; preview again",
        ));
    }
    if !db::start_apply(&state.pool, run.id).await? {
        // Nothing was updated; read once to say why.
        if db::has_undo_rows(&state.pool, run.id).await? {
            return Err(ApiError::Conflict(
                "This run was partly undone; undo it fully or start a new preview",
            ));
        }
        return Err(ApiError::Conflict("This run cannot be applied now"));
    }
    let job = apply::enqueue(&state.pool, run.id, Action::Apply).await?;
    Ok(accepted(run.id, job))
}

/// Undoes an applied (or partly applied) run.
async fn undo_run(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&user)?;
    let run = load(&state, &id).await?;
    if run.switched_at.is_some() {
        return Err(ApiError::Conflict(
            "The library was switched to this copy; it cannot be undone",
        ));
    }
    let current = state.library_root();
    match run.mode.as_str() {
        db::MODE_MOVE => {
            if current.as_deref() != Some(FsPath::new(&run.source_root)) {
                return Err(ApiError::Conflict(
                    "The library folder changed since this run; it cannot be undone",
                ));
            }
            if !writable(PathBuf::from(&run.source_root)).await {
                return Err(ApiError::Conflict(READ_ONLY));
            }
        }
        _ => {
            if current.is_some_and(|c| overlaps(&c, FsPath::new(&run.target_root))) {
                return Err(ApiError::Conflict(
                    "The library uses this copy; it cannot be undone",
                ));
            }
        }
    }
    if !db::transition(&state.pool, run.id, &["done", "failed"], "undoing").await? {
        return Err(ApiError::Conflict("This run cannot be undone now"));
    }
    let job = apply::enqueue(&state.pool, run.id, Action::Undo).await?;
    Ok(accepted(run.id, job))
}

/// Switches the library to a finished copy run's target.
async fn switch_root(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&user)?;
    let run = load(&state, &id).await?;
    if run.mode != db::MODE_COPY {
        return Err(ApiError::Conflict("Only a copy run can become the library"));
    }
    if run.state != "done" {
        return Err(ApiError::Conflict("The copy is not finished"));
    }
    if run.switched_at.is_some() {
        return Err(ApiError::Conflict("The library already uses this copy"));
    }
    let not_copied = db::tracks_not_done(&state.pool, run.id).await?;
    if not_copied > 0 {
        return Err(ApiError::Conflict(
            "Some tracks were not copied; switching would leave them missing",
        ));
    }
    let job = apply::enqueue(&state.pool, run.id, Action::SwitchRoot).await?;
    Ok(accepted(run.id, job))
}
