#[allow(dead_code)] // Used by later stats routes.
pub mod args;

use axum::{
    Json, Router,
    extract::{Path, RawQuery, State},
    routing::get,
};
use kima_api_types::stats::{HourDetail, HourTrack, NamedListens, TrackPlays, js_iso};
use uuid::Uuid;

use crate::{auth::AuthUser, error::ApiError, state::AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/stats/track/{id}/plays", get(track_plays))
        .route("/stats/hour", get(hour_detail))
}

async fn track_plays(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<TrackPlays>, ApiError> {
    if id.is_empty() || id.encode_utf16().count() > 64 {
        return Err(ApiError::BadRequest("Bad request".into()));
    }
    let plays = if let Ok(track) = id.parse::<Uuid>() {
        kima_db::stats::listening::track_play_count(&state.pool, user.id, track)
            .await
            .map_err(|error| {
                if kima_db::is_retryable_conflict(&error) {
                    ApiError::Busy
                } else {
                    tracing::error!(?error, "track play count failed");
                    ApiError::Failed("Couldn't count that track")
                }
            })?
    } else {
        0
    };
    Ok(Json(TrackPlays { plays }))
}

async fn hour_detail(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(query): RawQuery,
) -> Result<Json<HourDetail>, ApiError> {
    let args = args::Args(
        form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes())
            .into_owned()
            .collect(),
    );
    let dow = args.get("dow").number();
    let hour = args.get("hour").number();
    if !dow.is_finite()
        || dow.fract() != 0.0
        || !(0.0..=6.0).contains(&dow)
        || !hour.is_finite()
        || hour.fract() != 0.0
        || !(0.0..=23.0).contains(&hour)
    {
        return Err(ApiError::BadRequest("Bad request".into()));
    }
    let result = async {
        let tz = args.get("tz");
        let zone = kima_db::stats::listens::safe_zone(&state.pool, tz.string()).await?;
        let period_arg = args.get("period");
        let period = kima_db::stats::listens::Period::parse(period_arg.string());
        kima_db::stats::listening::hour_detail(
            &state.pool,
            user.id,
            dow as i32,
            hour as i32,
            &zone,
            period,
        )
        .await
    }
    .await
    .map_err(|error| {
        tracing::error!(?error, "hour detail failed");
        ApiError::Failed("Failed to read that hour")
    })?;
    Ok(Json(HourDetail {
        weekday: dow as i32,
        hour: hour as i32,
        listens: result.listens,
        tracks: result
            .tracks
            .into_iter()
            .map(|r| HourTrack {
                title: r.title,
                artist: r.artist,
                listens: r.listens,
            })
            .collect(),
        artists: result
            .artists
            .into_iter()
            .map(|r| NamedListens {
                name: r.name,
                listens: r.listens,
            })
            .collect(),
        last_at: result.last_at.map(js_iso),
    }))
}
