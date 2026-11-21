//! The radio (M5b Tasks 11 and 13; upstream `routes/library/tracks.ts:582-686`): `GET /library/decades`,
//! `GET /library/radio/stations` and `GET /library/radio`. Replies are serialised from structs so upstream's key
//! order holds.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use kima_api_types::radio::{RadioAlbum, RadioArtist, RadioReply, RadioTrack};
use kima_db::radio::{self, RadioRow};
use kima_mixes::covers::RowScope;
use kima_mixes::radio::{parse_limit, shuffle};
use kima_mixes::spread::{Follows, spread_by_artist};
use kima_mixes::stations::mood_station_list;
use serde::Serialize;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::radio::{Param, RadioQuery, RowStationReply, Selection, select_radio_tracks};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library/decades", get(decades))
        .route("/library/radio/stations", get(stations))
        .route("/library/radio/row", get(radio_row))
        .route("/library/radio", get(radio_tracks))
}

#[derive(Serialize)]
struct DecadeRow {
    decade: i32,
    count: i64,
}

#[derive(Serialize)]
struct Decades {
    decades: Vec<DecadeRow>,
}

/// Upstream `libraryDecades` (`services/mixes/radioRow.ts:59-76`): `{decades: [{decade, count}]}`, newest first;
/// 500 `Failed to get decades`.
async fn decades(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Decades>, ApiError> {
    const ERROR: &str = "Failed to get decades";
    let found = radio::library_decades(&state.pool).await.map_err(|e| {
        tracing::error!(error = %e, "{ERROR}");
        ApiError::Failed(ERROR)
    })?;
    Ok(Json(Decades {
        decades: found
            .into_iter()
            .map(|d| DecadeRow {
                decade: d.decade,
                count: d.count,
            })
            .collect(),
    }))
}

#[derive(Serialize)]
struct StationRow {
    value: &'static str,
    name: &'static str,
    description: String,
}

#[derive(Serialize)]
struct Stations {
    stations: Vec<StationRow>,
}

/// Upstream `moodStationList` (`services/mixes/stations.ts:111`): the 13 mood stations and Wildcard with their
/// subtitles, for the apps' radio pages.
async fn stations(_user: AuthUser) -> Json<Stations> {
    Json(Stations {
        stations: mood_station_list()
            .into_iter()
            .map(|s| StationRow {
                value: s.value,
                name: s.name,
                description: s.description,
            })
            .collect(),
    })
}

fn radio_track(r: RadioRow) -> RadioTrack {
    RadioTrack {
        id: r.id.to_string(),
        title: r.title,
        display_title: r.display_title,
        duration: r.duration_secs,
        track_no: r.track_no,
        disc_number: r.disc,
        disc_subtitle: None,
        file_path: r.path,
        artist: RadioArtist {
            id: r.artist_id.to_string(),
            name: r.artist_name,
        },
        album: RadioAlbum {
            cover_art: r.album_has_image.then(|| r.album_id.to_string()),
            id: r.album_id.to_string(),
            title: r.album_title,
        },
    }
}

fn reply(tracks: Vec<RadioTrack>, next: Option<u64>) -> Json<RadioReply> {
    Json(RadioReply {
        tracks,
        message: None,
        next,
    })
}

/// Upstream `GET /radio`: `type` is required (400 `Radio type is required`); `limit` reads as `parseLimit(_, 50, 100)`.
/// The stations' songs come in their own order; the other radios' songs are the playable ones, a uniform draw of
/// `limit`, spread by artist. 500 `Failed to get radio tracks`.
async fn radio_tracks(
    State(state): State<AppState>,
    user: AuthUser,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<RadioReply>, ApiError> {
    const ERROR: &str = "Failed to get radio tracks";
    let fail = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "Radio endpoint error");
        ApiError::Failed(ERROR)
    };
    let query = RadioQuery::new(pairs);
    let limit = parse_limit(query.text("limit").as_deref(), 50, 100);
    // An array `type` is truthy and matches no case: the `all` radio.
    let kind = match query.param("type") {
        Param::Absent | Param::One("") => {
            return Err(ApiError::BadRequest("Radio type is required".into()));
        }
        Param::One(kind) => kind.to_string(),
        Param::Many => "all".to_string(),
    };
    let selection = select_radio_tracks(&state, user.profile, &kind, &query, limit)
        .await
        .map_err(|e| fail(&e))?;
    let (track_ids, station, next) = match selection {
        Selection::BadRequest(message) => return Err(ApiError::BadRequest(message)),
        Selection::Empty => return Ok(reply(Vec::new(), None)),
        Selection::Retired(value) => {
            return Ok(Json(RadioReply {
                tracks: Vec::new(),
                message: Some(format!("The {value} station has been retired")),
                next: None,
            }));
        }
        Selection::Picked {
            track_ids,
            station,
            next,
        } => (track_ids, station, next),
    };
    let mut random = || rand::random::<f64>();
    let final_ids: Vec<Uuid> = if station {
        track_ids
    } else {
        let keep = radio::playable_ids(&state.pool, &track_ids)
            .await
            .map_err(|e| fail(&e))?;
        let mut kept: Vec<Uuid> = keep.into_iter().collect();
        kept.sort();
        let mut drawn = shuffle(kept, &mut random);
        drawn.truncate(limit);
        drawn
    };
    if final_ids.is_empty() {
        return Ok(reply(Vec::new(), next));
    }
    let rows = radio::radio_rows(&state.pool, &final_ids)
        .await
        .map_err(|e| fail(&e))?;
    let ordered: Vec<RadioRow> = if station {
        let rank: HashMap<Uuid, usize> = final_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();
        let mut rows = rows;
        rows.sort_by_key(|r| rank[&r.id]);
        rows
    } else {
        spread_by_artist(
            &rows,
            |r: &RadioRow| r.artist_key.clone(),
            &mut random,
            &Follows::default(),
        )
    };
    Ok(reply(ordered.into_iter().map(radio_track).collect(), next))
}

#[derive(Serialize)]
struct RadioRowReply {
    stations: Vec<RowStationReply>,
}

/// Upstream `GET /radio/row`: a page's stations, each with this listener's cover for the local day, in one reply;
/// `?scope=home` (default) or `all` (400 `scope must be home or all`), `?tz=` the listener's zone. 500 `Failed to get
/// radio stations`.
async fn radio_row(
    State(state): State<AppState>,
    user: AuthUser,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<RadioRowReply>, ApiError> {
    const ERROR: &str = "Failed to get radio stations";
    let query = RadioQuery::new(pairs);
    let scope = match query.param("scope") {
        Param::Absent => RowScope::Home,
        Param::One(s) => RowScope::parse(Some(s))
            .ok_or_else(|| ApiError::BadRequest("scope must be home or all".into()))?,
        Param::Many => return Err(ApiError::BadRequest("scope must be home or all".into())),
    };
    let tz = match query.param("tz") {
        Param::One(tz) => Some(tz),
        _ => None,
    };
    let stations = crate::radio::radio_row(&state, user.profile, tz, scope)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Radio row endpoint error");
            ApiError::Failed(ERROR)
        })?;
    Ok(Json(RadioRowReply { stations }))
}
