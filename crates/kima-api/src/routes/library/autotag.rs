//! Admin-only, offline reads of stored autotag proposals and history (spec §16.5).
use std::collections::BTreeSet;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use kima_db::{attention, autotag as db};
use kima_ident::{
    autotag::{
        proposal,
        score::{self, Mapping, Scored},
        start,
    },
    musicbrainz::Release,
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{Paging, parse_id};
use crate::{auth::AdminUser, error::ApiError, state::AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library/autotag/runs/{id}/undo", post(undo))
        .route("/library/autotag/albums/{id}/skip", post(skip))
        .route("/library/autotag/albums/{id}/apply", post(apply))
        .route("/library/autotag/search", post(search))
        .route("/library/autotag/capabilities", get(capabilities))
        .route("/library/autotag/albums", get(albums))
        .route("/library/autotag/albums/{id}", get(album))
        .route("/library/autotag/runs", get(runs))
        .route("/library/autotag/runs/{id}", get(run))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchRequest {
    album_ids: Option<Vec<uuid::Uuid>>,
    all: Option<bool>,
    query: Option<SearchQuery>,
    release_mbid: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchQuery {
    artist: String,
    album: String,
}

async fn search(
    _: AdminUser,
    State(s): State<AppState>,
    crate::extract::ApiJson(q): crate::extract::ApiJson<SearchRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    use kima_ident::autotag::search::{ManualQuery, SearchJob, enqueue_search, parse_release_ref};
    if q.all == Some(true) && q.album_ids.is_some() {
        return Err(ApiError::BadRequest(
            "Give albumIds or all, not both".into(),
        ));
    }
    if q.all != Some(true) && q.album_ids.as_ref().is_none_or(Vec::is_empty) {
        return Err(ApiError::BadRequest("Give albumIds or all".into()));
    }
    if q.query.is_some() && q.release_mbid.is_some() {
        return Err(ApiError::BadRequest(
            "Give query or releaseMbid, not both".into(),
        ));
    }
    let ids = q
        .album_ids
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeSet<_>>();
    if (q.query.is_some() || q.release_mbid.is_some()) && (q.all == Some(true) || ids.len() != 1) {
        return Err(ApiError::BadRequest(
            "query and releaseMbid need exactly one album".into(),
        ));
    }
    let query = q
        .query
        .map(|q| {
            if q.artist.trim().is_empty() || q.album.trim().is_empty() {
                return Err(ApiError::BadRequest("query needs artist and album".into()));
            }
            Ok(ManualQuery {
                artist: q.artist,
                album: q.album,
            })
        })
        .transpose()?;
    let release_mbid = q
        .release_mbid
        .map(|s| {
            parse_release_ref(&s)
                .ok_or_else(|| ApiError::BadRequest("Not a MusicBrainz release id or URL".into()))
        })
        .transpose()?;
    if !s.identify.enabled() {
        return Err(ApiError::Conflict("Outbound lookups are turned off"));
    }
    let ids = if q.all == Some(true) {
        db::unmatched_albums(&s.pool).await?.into_iter().collect()
    } else {
        ids
    };
    let mut queued = 0;
    for album_id in ids {
        let job = SearchJob {
            album_id,
            query: query.clone(),
            release_mbid,
        };
        queued += usize::from(enqueue_search(&s.pool, &job).await?.is_some());
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"queued":queued}))))
}

fn library_ctx(s: &AppState) -> Result<kima_library::scan::LibraryCtx, ApiError> {
    Ok(kima_library::scan::LibraryCtx {
        pool: s.pool.clone(),
        root: s
            .library_root
            .clone()
            .ok_or_else(|| ApiError::BadRequest("Library root is not configured".into()))?,
        events: s.events.clone(),
        data_dir: s.data_dir.clone().unwrap_or_default(),
        ignore: s.library.ignore.clone(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyBody {
    candidate_id: String,
    target: db::Target,
    #[serde(default)]
    reorganize: bool,
    #[serde(default)]
    mbids_only: bool,
}
async fn apply(
    AdminUser(user): AdminUser,
    State(s): State<AppState>,
    Path(id): Path<String>,
    crate::extract::ApiJson(body): crate::extract::ApiJson<ApplyBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let album_id = parse_id(&id).ok_or(ApiError::NotFound("Album not found"))?;
    let candidate_id = parse_id(&body.candidate_id).ok_or(ApiError::NotFound(start::NOT_FOUND))?;
    let started = start::start(
        &s.pool,
        &library_ctx(&s)?,
        &start::ApplyRequest {
            album_id,
            candidate_id,
            target: body.target,
            reorganize: body.reorganize,
            mbids_only: body.mbids_only,
            created_by: Some(user.id),
        },
    )
    .await
    .map_err(|e| match e {
        start::StartError::NotFound => ApiError::NotFound(start::NOT_FOUND),
        start::StartError::Db(e) => e.into(),
        start::StartError::Busy => ApiError::Conflict(kima_library::autotag::apply::ALBUM_BUSY),
        start::StartError::Collision(c) => collision(c),
        start::StartError::Invalid(m) | start::StartError::Unavailable(m) => {
            ApiError::BadRequest(m)
        }
    })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"runId":started.run_id,"jobId":started.job_id})),
    ))
}

async fn undo(
    _: AdminUser,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    use kima_library::autotag::{
        apply::AutotagError,
        undo::{RequestUndoError, request_undo},
    };
    let id = parse_id(&id).ok_or(ApiError::NotFound("Run not found"))?;
    let job = request_undo(&library_ctx(&s)?, id)
        .await
        .map_err(|e| match e {
            RequestUndoError::NotFound => ApiError::NotFound("Run not found"),
            RequestUndoError::Collision(c) => collision(c),
            RequestUndoError::Other(AutotagError::Refused(m)) => ApiError::ConflictMessage(m),
            RequestUndoError::Other(AutotagError::Db(e)) => e.into(),
            e => ApiError::Internal(anyhow::anyhow!(e)),
        })?;
    Ok((StatusCode::ACCEPTED, Json(json!({"jobId":job}))))
}

async fn skip(
    _: AdminUser,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id).ok_or(ApiError::NotFound("Album not found"))?;
    let mut tx = s.pool.begin().await?;
    if db::lock_album_tracks(&mut tx, id)
        .await?
        .is_none_or(|t| t.is_empty())
    {
        return Err(ApiError::NotFound("Album not found"));
    }
    if !db::lock_search_eligible(&mut tx, id).await? {
        return Err(ApiError::Conflict(
            kima_library::autotag::APPLIED_MATCH_EXISTS,
        ));
    }
    db::set_state_locked(&mut tx, id, db::AlbumState::Skipped, None).await?;
    tx.commit().await?;
    Ok(Json(json!({"state":"skipped"})))
}

fn collision(c: db::ConflictAlbum) -> ApiError {
    ApiError::ConflictWith {
        message: c.message(),
        extra: json!({"conflictAlbumId":c.album_id}),
    }
}

fn rfc3339(t: OffsetDateTime) -> Option<String> {
    t.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

async fn capabilities(_: AdminUser, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let (files, copy, copy_run_id) = if let Some(root) = s.library_root.clone() {
        let ctx = kima_library::scan::LibraryCtx {
            pool: s.pool.clone(),
            root,
            events: s.events.clone(),
            data_dir: s.data_dir.clone().unwrap_or_default(),
            ignore: s.library.ignore.clone(),
        };
        let c = kima_library::autotag::capability::capabilities(&ctx).await?;
        (json!(c.files), json!(c.copy), c.copy_run_id)
    } else {
        (
            json!({"available":false,"reason":kima_library::autotag::FILES_READ_ONLY}),
            json!({"available":false,"reason":kima_library::autotag::COPY_FIRST}),
            None,
        )
    };
    Ok(Json(
        json!({"identifyEnabled":s.identify.enabled(),"files":files,"copy":{"available":copy["available"],"reason":copy["reason"],"copyRunId":copy_run_id},"db":{"available":true}}),
    ))
}

#[derive(Default, Deserialize)]
struct AlbumQuery {
    state: Option<String>,
    recommendation: Option<String>,
    q: Option<String>,
    #[serde(flatten)]
    paging: Paging,
}
async fn albums(
    _: AdminUser,
    State(s): State<AppState>,
    Query(q): Query<AlbumQuery>,
) -> Result<Json<Value>, ApiError> {
    let f = db::AlbumFilter {
        state: q.state,
        recommendation: q.recommendation,
        q: q.q,
    };
    let (rows, total) =
        db::list_albums(&s.pool, &f, q.paging.offset(), q.paging.limit(50).min(200)).await?;
    let ids = rows.iter().map(|r| r.album_id).collect::<Vec<_>>();
    let issues = attention::report_for(&s.pool, &ids).await?;
    let albums = rows
        .iter()
        .map(|r| {
            json!({
                "albumId": r.album_id,
                "artist": r.artist,
                "album": r.album,
                "year": r.year,
                "trackCount": r.track_count,
                "state": r.state,
                "recommendation": r.recommendation,
                "distance": r.distance,
                "releaseMbid": r.release_mbid,
                "searchedAt": r.searched_at.and_then(rfc3339),
                "appliedAt": r.applied_at.and_then(rfc3339),
                "issues": issues.get(&r.album_id).cloned().unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"albums":albums,"total":total})))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Selection {
    candidate_id: Option<String>,
}

fn decode<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, ApiError> {
    serde_json::from_value(v.clone()).map_err(|e| ApiError::Internal(e.into()))
}

fn candidate_json(c: &db::CandidateRow, r: &Release, now: OffsetDateTime) -> Value {
    let artist = r
        .artist_credit
        .iter()
        .map(|c| format!("{}{}", c.name, c.joinphrase))
        .collect::<String>();
    let media = r.media.iter().map(|m| m.format.clone()).collect::<Vec<_>>();
    let label = r
        .label_info
        .iter()
        .find_map(|l| l.label.as_ref().map(|l| &l.name));
    let catalog = r.label_info.iter().find_map(|l| l.catalog_number.as_ref());
    json!({
        "id": c.id,
        "releaseMbid": c.release_mbid,
        "releaseGroupMbid": c.release_group_mbid,
        "title": r.title,
        "artist": artist,
        "date": r.date,
        "country": r.country,
        "media": media,
        "label": label,
        "catalogNumber": catalog,
        "barcode": r.barcode,
        "trackCount": r.media.iter().map(|m| u64::from(m.track_count)).sum::<u64>(),
        "distance": c.distance,
        "recommendation": c.recommendation,
        "penalties": c.penalties,
        "origin": c.origin,
        "fetchedAt": rfc3339(c.fetched_at),
        "stale": now - c.fetched_at > time::Duration::days(30),
    })
}

async fn album(
    _: AdminUser,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<Selection>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id).ok_or(ApiError::NotFound("Album not found"))?;
    let local = db::local_album(&s.pool, id)
        .await?
        .ok_or(ApiError::NotFound("Album not found"))?;
    let state = db::review_state(&s.pool, id).await?;
    let mut candidates = Vec::new();
    for c in db::candidates(&s.pool, id).await? {
        let release: Release = decode(&c.release)?;
        let scored = Scored {
            distance: c.distance,
            recommendation: decode(&json!(c.recommendation))?,
            penalties: decode(&c.penalties)?,
            mapping: Mapping {
                pairs: decode(&c.mapping)?,
                extra: decode(&c.extra_tracks)?,
                missing: decode(&c.missing_tracks)?,
            },
        };
        candidates.push((c, release, scored));
    }
    // Same rounded-distance and release tie-breakers as score::order, without rescoring
    // a stored proposal against a potentially changed local album.
    candidates.sort_by_cached_key(|(_, r, score)| score::order_key(r, score));
    let chosen = match q.candidate_id {
        Some(id) => Some(parse_id(&id).ok_or(ApiError::NotFound("Candidate not found"))?),
        None => state
            .as_ref()
            .and_then(|s| s.chosen_candidate.or(s.best_candidate)),
    };
    let selected_candidate = chosen
        .map(|id| {
            candidates
                .iter()
                .find(|(c, _, _)| c.id == id)
                .ok_or(ApiError::NotFound("Candidate not found"))
        })
        .transpose()?;
    let mut conflict = Value::Null;
    let mut selected = Value::Null;
    if let Some((c, r, scored)) = selected_candidate {
        let applied =
            start::applied_values(c, &local, false).map_err(|e| ApiError::Internal(e.into()))?;
        let keys = db::IdentityKeys {
            artist_key: kima_library::names::key(&applied.album.artist),
            title_key: kima_library::names::key(&applied.album.title),
        };
        let mut conn = s.pool.acquire().await?;
        if let Some(c) = db::find_collision(&mut conn, id, &keys, applied.album.year).await? {
            conflict =
                json!({"albumId":c.album_id,"artist":c.artist,"title":c.title,"year":c.year});
        }
        let mapping = scored.mapping.pairs.iter().filter_map(|p| {
            let l = local.tracks.iter().find(|l| l.id == p.track_id)?;
            let t = r.media.iter().find(|m| m.position == p.medium)?
                .tracks.iter().find(|t| t.position == p.position)?;
            Some(json!({
                "trackId": p.track_id,
                "local": {"disc": l.disc, "trackNo": l.track_no, "title": l.title, "duration": l.duration_secs},
                "release": {"medium": p.medium, "position": p.position, "title": t.title, "length": t.length, "recordingMbid": t.recording.id},
                "distance": p.distance,
            }))
        }).collect::<Vec<_>>();
        selected = json!({
            "candidateId": c.id,
            "changes": proposal::changes(&local, &applied),
            "mapping": mapping,
            "missing": c.missing_tracks,
            "extra": c.extra_tracks,
        });
    }
    let tracks = local
        .tracks
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "path": t.path,
                "disc": t.disc,
                "trackNo": t.track_no,
                "title": t.title,
                "artist": t.artist_name,
                "duration": t.duration_secs,
                "recordingMbid": t.recording_mbid,
            })
        })
        .collect::<Vec<_>>();
    let now = OffsetDateTime::now_utc();
    Ok(Json(json!({
        "album": {"id": local.id, "artist": local.artist_name, "title": local.title, "year": local.year, "tracks": tracks},
        "state": state.as_ref().map_or("new", |s| s.state.as_str()),
        "recommendation": state.as_ref().and_then(|s| s.recommendation.as_ref()),
        "error": state.as_ref().and_then(|s| s.error.as_ref()),
        "candidates": candidates.iter().map(|(c, r, _)| candidate_json(c, r, now)).collect::<Vec<_>>(),
        "conflict": conflict,
        "selected": selected,
        "userMbidKept": local.release_mbid_source.as_deref() == Some("user")
            || local.release_group_mbid_source.as_deref() == Some("user"),
    })))
}

fn run_json(r: &db::RunListRow) -> Value {
    json!({"id":r.id,"albumId":r.album_id,"album":r.album,"artist":r.artist,"target":r.target,"reorganize":r.reorganize,"state":r.state,"createdAt":rfc3339(r.created_at),"finishedAt":r.finished_at.and_then(rfc3339),"summary":r.summary,"error":r.error})
}
async fn runs(
    _: AdminUser,
    State(s): State<AppState>,
    Query(q): Query<Paging>,
) -> Result<Json<Value>, ApiError> {
    let runs = db::list_runs(&s.pool, q.offset(), q.limit(20).min(200)).await?;
    Ok(Json(
        json!({"runs":runs.iter().map(run_json).collect::<Vec<_>>()}),
    ))
}
fn tag_changes(old: &Value, new: &Value) -> Vec<Value> {
    let keys: BTreeSet<_> = old
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys())
        .chain(new.as_object().into_iter().flat_map(|m| m.keys()))
        .collect();
    keys.into_iter()
        .filter(|k| old[*k] != new[*k])
        .map(|k| json!({"field":k,"old":old[k],"new":new[k]}))
        .collect()
}
async fn run(
    _: AdminUser,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<Paging>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_id(&id).ok_or(ApiError::NotFound("Run not found"))?;
    let r = db::run_view(&s.pool, id)
        .await?
        .ok_or(ApiError::NotFound("Run not found"))?;
    let (rows, total) = db::run_rows(&s.pool, id, q.offset(), q.limit(200).min(200)).await?;
    let mut v = run_json(&r);
    v["rows"]=json!(rows.iter().map(|r|json!({"id":r.id,"trackId":r.track_id,"path":r.path,"state":r.state,"note":r.note,"changes":tag_changes(&r.old_tags,&r.new_tags)})).collect::<Vec<_>>());
    // Link only the public organizer run id; never serialize the private apply snapshot.
    v["organizerRunId"] = json!(
        db::get_run(&s.pool, id)
            .await?
            .and_then(|r| r.organizer_run_id)
    );
    v["total"] = json!(total);
    Ok(Json(v))
}
