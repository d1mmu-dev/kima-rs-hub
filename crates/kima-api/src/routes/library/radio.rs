//! The radio's library lists (M5b Task 11; upstream `routes/library/tracks.ts:582-596`): `GET /library/decades` and
//! `GET /library/radio/stations`. Replies are serialised from structs so upstream's key order holds.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::radio;
use kima_mixes::stations::mood_station_list;
use serde::Serialize;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/library/decades", get(decades))
        .route("/library/radio/stations", get(stations))
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
