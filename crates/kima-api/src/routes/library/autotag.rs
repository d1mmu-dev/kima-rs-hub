//! Admin-only, offline reads of stored autotag proposals and history (spec §16.5).
use std::collections::BTreeSet;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
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
        .route("/library/autotag/capabilities", get(capabilities))
        .route("/library/autotag/albums", get(albums))
        .route("/library/autotag/albums/{id}", get(album))
        .route("/library/autotag/runs", get(runs))
        .route("/library/autotag/runs/{id}", get(run))
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
    v["total"] = json!(total);
    Ok(Json(v))
}
