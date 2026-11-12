#[allow(dead_code)] // Used by later stats routes.
pub mod args;

use axum::{
    Json, Router,
    extract::{Path, RawQuery, Request, State},
    http::header,
    routing::{get, post},
};
use kima_api_types::stats::{
    HourDetail, HourTrack, Ignored, NamedListens, Removed, RemovedFrom, Restored, TrackPlays,
    js_iso,
};
use kima_db::stats::audit;
use uuid::Uuid;

use crate::{auth::AuthUser, error::ApiError, state::AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/stats", get(stats))
        .route("/stats/tracks", get(tracks))
        .route("/stats/track/{id}/plays", get(track_plays))
        .route("/stats/region", get(region_detail))
        .route("/stats/hour", get(hour_detail))
        .route("/stats/false-starts", get(false_starts))
        .route("/stats/false-starts/collapse", post(collapse))
        .route("/stats/false-starts/ignore", post(ignore))
        .route("/stats/false-starts/remove", post(remove))
        .route("/stats/false-starts/restore", post(restore))
}

/// Upstream's `express.json({ limit: "1mb" })` (`index.ts:104`).
const BODY_LIMIT: usize = 1024 * 1024;

/// The body as upstream's global `express.json()` parser leaves it, in front of
/// every handler: `None` when it does not parse (`Content-Type` other than
/// `application/json`), `{}` for an empty body, and the error handler's answers
/// (`errorHandler.ts:71-79`) for a malformed (strict: object or array only) or
/// oversized one.
async fn express_json(request: Request) -> Result<Option<serde_json::Value>, ApiError> {
    let is_json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return Ok(None);
    }
    let bytes = axum::body::to_bytes(request.into_body(), BODY_LIMIT)
        .await
        // Past the limit (the only read error a client that still waits can see).
        .map_err(|_| ApiError::PayloadTooLarge)?;
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    if text.is_empty() {
        return Ok(Some(serde_json::json!({})));
    }
    let invalid = || ApiError::BadRequest("Invalid request body".into());
    let first = text
        .iter()
        .find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
    if !matches!(first, Some(b'{' | b'[')) {
        return Err(invalid());
    }
    serde_json::from_slice(text)
        .map(Some)
        .map_err(|_| invalid())
}

/// Upstream's `trackListSchema` (`stats.ts:153-155`): an object whose `trackIds`
/// is 1–2000 strings of 1–64 UTF-16 units, other keys ignored. Strings that are
/// no UUID can match no track and are dropped after validation.
fn track_ids(body: Option<&serde_json::Value>) -> Result<Vec<Uuid>, ApiError> {
    let bad = || ApiError::BadRequest("Bad request".into());
    let ids = body
        .filter(|v| v.is_object())
        .and_then(|v| v.get("trackIds"))
        .and_then(|v| v.as_array())
        .ok_or_else(bad)?;
    if ids.is_empty() || ids.len() > 2000 {
        return Err(bad());
    }
    let mut result = Vec::new();
    for id in ids {
        let id = id.as_str().ok_or_else(bad)?;
        if !(1..=64).contains(&id.encode_utf16().count()) {
            return Err(bad());
        }
        if let Ok(id) = Uuid::parse_str(id) {
            result.push(id);
        }
    }
    Ok(result)
}

fn action_failed(action: &'static str) -> impl FnOnce(sqlx::Error) -> ApiError {
    move |error| {
        tracing::error!(?error, action, "false-start action failed");
        ApiError::Failed("That didn't finish")
    }
}

async fn collapse(
    State(state): State<AppState>,
    user: AuthUser,
    request: Request,
) -> Result<Json<Removed>, ApiError> {
    express_json(request).await?;
    let removed = audit::collapse(&state.pool, user.id)
        .await
        .map_err(action_failed("collapse"))?;
    Ok(Json(Removed { removed }))
}

async fn remove(
    State(state): State<AppState>,
    user: AuthUser,
    request: Request,
) -> Result<Json<RemovedFrom>, ApiError> {
    let tracks = track_ids(express_json(request).await?.as_ref())?;
    let (removed, tracks) = audit::remove(&state.pool, user.id, &tracks)
        .await
        .map_err(action_failed("remove"))?;
    Ok(Json(RemovedFrom { removed, tracks }))
}

async fn ignore(
    State(state): State<AppState>,
    user: AuthUser,
    request: Request,
) -> Result<Json<Ignored>, ApiError> {
    let tracks = track_ids(express_json(request).await?.as_ref())?;
    let ignored = audit::ignore(&state.pool, user.id, &tracks)
        .await
        .map_err(action_failed("ignore"))?;
    Ok(Json(Ignored { ignored }))
}

async fn restore(
    State(state): State<AppState>,
    user: AuthUser,
    request: Request,
) -> Result<Json<Restored>, ApiError> {
    let tracks = track_ids(express_json(request).await?.as_ref())?;
    let restored = audit::restore(&state.pool, user.id, &tracks)
        .await
        .map_err(action_failed("restore"))?;
    Ok(Json(Restored { restored }))
}

async fn tracks(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(query): RawQuery,
) -> Result<Json<kima_api_types::stats::TracksResponse>, ApiError> {
    use kima_api_types::stats::{PlayableAlbum, PlayableArtist, PlayableTrack, TracksResponse};
    let args = args::Args(
        form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes())
            .into_owned()
            .collect(),
    );
    let slice = args.get("of").js_string();
    if !["top", "never", "core", "key", "decade"].contains(&slice.as_str()) {
        return Err(ApiError::BadRequest("Bad request".into()));
    }
    let asked = args.get("limit").number();
    let limit = if asked.is_finite() {
        asked.floor().clamp(1.0, 100.0) as i64
    } else {
        40
    };
    let decade = args.get("decade").number();
    let period = kima_db::stats::listens::Period::parse(args.get("period").string());
    let rows = kima_db::stats::playable::playable_slice(
        &state.pool,
        user.id,
        &slice,
        decade,
        limit,
        period,
    )
    .await
    .map_err(|error| {
        tracing::error!(?error, "playable slice failed");
        ApiError::Failed("Couldn't gather those tracks")
    })?;
    Ok(Json(TracksResponse {
        tracks: rows
            .into_iter()
            .map(|row| {
                let album_id = row.album_id.to_string();
                PlayableTrack {
                    id: row.id.to_string(),
                    title: row.title,
                    duration: row.duration,
                    artist: PlayableArtist {
                        id: row.artist_id.to_string(),
                        name: row.artist_name,
                    },
                    album: PlayableAlbum {
                        id: album_id.clone(),
                        title: row.album_title,
                        cover_art: album_id,
                    },
                }
            })
            .collect(),
    }))
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

async fn region_detail(
    _user: AuthUser,
    RawQuery(query): RawQuery,
) -> Result<Json<kima_api_types::stats::RegionDetail>, ApiError> {
    let args = args::Args(
        form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes())
            .into_owned()
            .collect(),
    );
    let x = args.get("x").number();
    let y = args.get("y").number();
    if [x, y]
        .iter()
        .any(|n| !n.is_finite() || n.fract() != 0.0 || !(1.0..=24.0).contains(n))
    {
        return Err(ApiError::BadRequest("Bad request".into()));
    }
    // S1 does not use track analysis; S2 will replace this empty result.
    Ok(Json(kima_api_types::stats::RegionDetail {
        x: x as i32,
        y: y as i32,
        listens: 0,
        tracks: vec![],
        energy: None,
        valence: None,
        bpm: None,
    }))
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

async fn false_starts(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<kima_api_types::stats::FalseStartAudit>, ApiError> {
    use kima_api_types::stats::{FalseStartAudit, FalseStartTrack, IgnoredTrack};
    let (rows, ignored) = tokio::try_join!(
        audit::audit_rows(&state.pool, user.id),
        audit::ignored(&state.pool, user.id)
    )
    .map_err(|error| {
        tracing::error!(?error, "false-start audit failed");
        ApiError::Failed("Failed to read your play history")
    })?;
    let a = audit::summarize(&audit::listens_of(&rows), &ignored);
    Ok(Json(FalseStartAudit {
        total_rows: a.total_rows,
        total_listens: a.total_listens,
        duplicate_rows: a.duplicate_rows,
        collapsible_rows: a.collapsible_rows,
        heard: a.heard,
        skipped: a.skipped,
        too_short: a.too_short,
        unclear: a.unclear,
        earliest: a.earliest.map(js_iso),
        tracks: a
            .tracks
            .into_iter()
            .map(|t| FalseStartTrack {
                track_id: t.track_id.to_string(),
                title: t.title,
                artist: t.artist,
                duration_seconds: t.duration,
                required_seconds: t.required,
                skipped: t.skipped,
                too_short: t.too_short,
                heard: t.heard,
                listens: t.listens,
                rows: t.rows,
                shortest_run_seconds: t.shortest_run,
                last_at: js_iso(t.last_at),
            })
            .collect(),
        ignored: ignored
            .into_iter()
            .map(|r| IgnoredTrack {
                track_id: r.track_id.to_string(),
                title: r.title,
                artist: r.artist,
                ignored_at: js_iso(r.ignored_at),
            })
            .collect(),
    }))
}
