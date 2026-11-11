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
        .route("/stats", get(stats))
        .route("/stats/track/{id}/plays", get(track_plays))
        .route("/stats/hour", get(hour_detail))
}

async fn stats(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(query): RawQuery,
) -> Result<Json<kima_api_types::stats::StatsResponse>, ApiError> {
    use kima_api_types::stats as dto;
    use kima_db::stats::{
        listening::listening_stats,
        listens::{Period, safe_zone},
        overview::overview,
    };
    let args = args::Args(
        form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes())
            .into_owned()
            .collect(),
    );
    let result = async {
        let tz = args.get("tz");
        let zone = safe_zone(&state.pool, tz.string()).await?;
        let period_arg = args.get("period");
        let period = Period::parse(period_arg.string());
        let (l, o) = tokio::try_join!(
            listening_stats(&state.pool, user.id, &zone, period),
            overview(&state.pool, user.id, period)
        )?;
        let listening = dto::ListeningStats {
            plays: l.plays,
            estimated_seconds: l.estimated_seconds,
            distinct_tracks: l.distinct_tracks,
            distinct_artists: l.distinct_artists,
            distinct_albums: l.distinct_albums,
            first_play: l.first_play.map(js_iso),
            last_play: l.last_play.map(js_iso),
            by_source: l.by_source.into_iter().collect(),
            clock: l.clock,
            peak: l.peak.map(|p| dto::Peak {
                weekday: p.weekday,
                hour: p.hour,
                plays: p.plays,
            }),
            shape: vec![],
            drift: l
                .drift
                .into_iter()
                .map(|d| dto::DriftMonth {
                    month: d.month,
                    energy: None,
                    valence: None,
                    bpm: None,
                    plays: d.plays,
                })
                .collect(),
            top_artists: l
                .top_artists
                .into_iter()
                .map(|a| dto::TopArtist {
                    id: a.id.to_string(),
                    name: a.name,
                    plays: a.plays,
                    estimated_seconds: a.estimated_seconds,
                })
                .collect(),
            top_tracks: l
                .top_tracks
                .into_iter()
                .map(|t| dto::TopTrack {
                    id: t.id.to_string(),
                    title: t.title,
                    artist: t.artist,
                    plays: t.plays,
                })
                .collect(),
        };
        let total = o.total.tracks;
        let era_total: i64 = o.era.iter().map(|e| e.plays).sum();
        let era = o.era.first().map(|first| dto::Era {
            median_year: first.median,
            buckets: o
                .era
                .iter()
                .map(|e| dto::EraBucket {
                    label: e.label.clone(),
                    plays: e.plays,
                    percent: ((e.plays as f64 / era_total as f64) * 1000.0).round() / 10.0,
                })
                .collect(),
        });
        let concentration = match (
            o.concentration.for_half,
            o.concentration.for_eighty,
            o.concentration.distinct,
        ) {
            (Some(for_half), Some(for_eighty), Some(distinct)) => Some(dto::Concentration {
                for_half,
                for_eighty,
                distinct,
            }),
            _ => None,
        };
        let habit = (o.habit.moves > 0).then_some(dto::AlbumHabit {
            moves: o.habit.moves,
            same_album_pct: o.habit.same_album,
            in_order_pct: o.habit.in_order,
        });
        Ok::<_, sqlx::Error>(dto::StatsResponse {
            period: period.as_str().to_owned(),
            listening,
            shelf: dto::ShelfStats {
                tracks: dto::Tally {
                    total,
                    heard: o.heard.tracks,
                },
                albums: dto::Tally {
                    total: o.total.albums,
                    heard: o.heard.albums,
                },
                artists: dto::Tally {
                    total: o.total.artists,
                    heard: o.heard.artists,
                },
                analysis: dto::AnalysisTally {
                    done: 0,
                    failed: 0,
                    pending: total,
                    total,
                },
                never_played: (total - o.heard.tracks).max(0),
                decades: o
                    .decades
                    .into_iter()
                    .map(|d| dto::Decade {
                        decade: d.decade,
                        albums: d.albums,
                        heard: d.heard,
                    })
                    .collect(),
            },
            spoken: dto::SpokenStats {
                audiobooks: dto::AudiobookStats {
                    seconds: 0,
                    finished: 0,
                    in_progress: 0,
                    set_aside: 0,
                    top_authors: vec![],
                    current: None,
                },
                podcasts: dto::PodcastStats {
                    seconds: 0,
                    finished: 0,
                    in_progress: 0,
                    subscriptions: 0,
                    backlog: 0,
                },
            },
            signature: dto::Signature {
                concentration,
                album_habit: habit,
                era,
                key: None,
                owned_vs_played: vec![],
                hourly: vec![],
            },
        })
    }
    .await
    .map_err(|error| {
        tracing::error!(?error, "stats failed");
        if kima_db::is_retryable_conflict(&error) {
            ApiError::Busy
        } else {
            ApiError::Failed("Failed to load stats")
        }
    })?;
    Ok(Json(result))
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
