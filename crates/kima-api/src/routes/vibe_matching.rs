//! HTTP adapters for the existing matching and song-path services.
use super::vibe::{failure, parse_limit};
use crate::{auth::AuthUser, error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use kima_api_types::vibe::{
    AlchemyResponse, MatchTrack, PathMetadata, PathResponse, PathTrack, SearchAlbum, SearchArtist,
    SimilarResponse,
};
use kima_db::vibe::{
    path::{self, PathError, PathOptions},
    rank::{self, LibraryTrack, RankedTrack, WeightedInput},
    read,
};
use kima_ident::stages::similarity;
use kima_vibe::{formula::SELF_SCORE, song_path::Mode};
use rand::Rng;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}

async fn probe(state: &AppState) -> Option<std::sync::Arc<kima_vibe::feel_probe::FeelProbe>> {
    similarity::runtime(&state.ident_ctx())
        .await
        .map(|r| r.probe.clone())
}

async fn match_tracks(
    state: &AppState,
    rows: Vec<RankedTrack>,
    scale: f64,
    error: &'static str,
) -> Result<Vec<MatchTrack>, ApiError> {
    let titles = read::edited_titles(
        &state.pool,
        &rows.iter().map(|r| r.track.id).collect::<Vec<_>>(),
    )
    .await
    .map_err(|e| failure(error, e))?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let t = r.track;
            let m = r.score / scale;
            MatchTrack {
                id: t.id.to_string(),
                title: t.title,
                display_title: titles.get(&t.id).cloned().flatten(),
                duration: t.duration_secs,
                distance: 1.0 - m,
                similarity: m.clamp(0.0, 1.0),
                album: SearchAlbum {
                    id: t.album_id.to_string(),
                    title: t.album_title,
                    cover_url: t.album_has_cover.then(|| t.album_id.to_string()),
                },
                artist: SearchArtist {
                    id: t.artist_id.to_string(),
                    name: t.artist_name,
                },
            }
        })
        .collect())
}

pub async fn similar(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(source): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to find similar tracks";
    let limit = parse_limit(
        params
            .get("limit")
            .map(|s| Value::String(s.clone()))
            .as_ref(),
    );
    let mut rows = Vec::new();
    if let (Ok(id), Some(probe)) = (Uuid::parse_str(&source), probe(&state).await) {
        if let Some(ctx) = rank::matching_context(&state.pool, &probe, &[id])
            .await
            .map_err(|e| failure(ERROR, e))?
        {
            if ctx.tracks.contains_key(&id) {
                rows = rank::rank_matches(
                    &state.pool,
                    &ctx,
                    &[WeightedInput {
                        track_id: id,
                        weight: 1.0,
                    }],
                    limit as i64,
                    &[id],
                )
                .await
                .map_err(|e| failure(ERROR, e))?;
            }
        }
    }
    if rows.is_empty() {
        return Ok((StatusCode::NOT_FOUND,Json(json!({"error":"No similar tracks found","message":"This track may not have been analyzed yet, or no analyzer is running"}))).into_response());
    }
    Ok(Json(SimilarResponse {
        source_track_id: source,
        tracks: match_tracks(&state, rows, SELF_SCORE, ERROR).await?,
    })
    .into_response())
}

// JS truthiness matters for the optional subtract field, before array validation.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn ids(values: &[Value], message: &str) -> Result<Vec<Uuid>, ApiError> {
    values
        .iter()
        .map(|v| {
            v.as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| bad(message))
        })
        .collect()
}

pub async fn alchemy(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to compute alchemy blend";
    let add = body
        .get("add")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad("At least one track ID in 'add' is required"))?;
    if add.len() > 10 {
        return Err(bad("'add' may contain at most 10 tracks"));
    }
    let subtract = body.get("subtract").filter(|v| truthy(v));
    if subtract.is_some_and(|v| !v.is_array() || v.as_array().is_some_and(|a| a.len() > 10)) {
        return Err(bad("'subtract' must be an array of at most 10 tracks"));
    }
    let add = ids(add, "'add' must contain valid track IDs")?;
    let subtract = ids(
        subtract
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default(),
        "'subtract' must contain valid track IDs",
    )?;
    let all: Vec<_> = add.iter().chain(&subtract).copied().collect();
    let ctx = if let Some(p) = probe(&state).await {
        rank::matching_context(&state.pool, &p, &all)
            .await
            .map_err(|e| failure(ERROR, e))?
    } else {
        None
    };
    let missing_add = ctx
        .as_ref()
        .map_or_else(|| add.clone(), |c| c.missing(&add));
    let missing_subtract = ctx
        .as_ref()
        .map_or_else(|| subtract.clone(), |c| c.missing(&subtract));
    let Some(ctx) = ctx.filter(|_| missing_add.is_empty() && missing_subtract.is_empty()) else {
        return Ok((StatusCode::BAD_REQUEST,Json(json!({"error":"Some tracks have not been through the similarity step yet","missingAdd":missing_add,"missingSubtract":missing_subtract}))).into_response());
    };
    let inputs: Vec<_> = add
        .iter()
        .map(|id| WeightedInput {
            track_id: *id,
            weight: 1.0 / add.len() as f64,
        })
        .chain(subtract.iter().map(|id| WeightedInput {
            track_id: *id,
            weight: -1.0 / subtract.len() as f64,
        }))
        .collect();
    let blend = rank::check_blend(&ctx, &inputs).map_err(|e| match e {
        rank::RankError::CancelsOut => {
            bad("The tracks to add and to subtract cancel each other out")
        }
        other => failure(ERROR, other),
    })?;
    let rows = rank::rank_matches(
        &state.pool,
        &ctx,
        &inputs,
        parse_limit(body.get("limit")) as i64,
        &add,
    )
    .await
    .map_err(|e| failure(ERROR, e))?;
    Ok(Json(AlchemyResponse {
        tracks: match_tracks(&state, rows, (blend * SELF_SCORE).sqrt(), ERROR).await?,
    })
    .into_response())
}

fn path_length(v: Option<&Value>) -> Option<usize> {
    let v = v.filter(|v| truthy(v))?;
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let s = s.trim_matches(kima_vibe::ecmascript_whitespace);
            if s.is_empty() {
                Some(0.0)
            } else {
                s.parse::<f64>().ok()
            }
        }
        Value::Bool(b) => Some(u8::from(*b) as f64),
        _ => None,
    }?;
    if n.is_nan() {
        None
    } else {
        Some(n.clamp(5.0, 50.0) as usize)
    }
}
fn path_track(t: LibraryTrack, titles: &HashMap<Uuid, Option<String>>) -> PathTrack {
    PathTrack {
        id: t.id.to_string(),
        title: t.title,
        display_title: titles.get(&t.id).cloned().flatten(),
        duration: t.duration_secs,
        album_id: t.album_id.to_string(),
        album_title: t.album_title,
        album_cover_url: t.album_has_cover.then(|| t.album_id.to_string()),
        artist_id: t.artist_id.to_string(),
        artist_name: t.artist_name,
    }
}

pub async fn song_path(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to generate song path";
    let pair = body
        .get("startTrackId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .zip(
            body.get("endTrackId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        )
        .ok_or_else(|| bad("startTrackId and endTrackId are required strings"))?;
    let start = Uuid::parse_str(pair.0).map_err(|_| bad("Invalid track ID format"))?;
    let end = Uuid::parse_str(pair.1).map_err(|_| bad("Invalid track ID format"))?;
    if start == end {
        return Err(bad("Start and end tracks must be different"));
    }
    let probe = probe(&state)
        .await
        .ok_or_else(|| bad(&PathError::NotMatched.to_string()))?;
    let mode = if body.get("mode").and_then(Value::as_str) == Some("discovery") {
        Mode::Discovery
    } else {
        Mode::Smooth
    };
    let result = path::generate_song_path(
        &state.pool,
        &probe,
        start,
        end,
        PathOptions {
            length: path_length(body.get("length")),
            mode,
        },
        |bound| rand::thread_rng().gen_range(0..bound),
    )
    .await
    .map_err(|e| {
        if e.status() == 400 {
            bad(&e.to_string())
        } else {
            failure(ERROR, e)
        }
    })?;
    let ids: Vec<_> = std::iter::once(result.start.id)
        .chain(result.path.iter().map(|t| t.id))
        .chain(std::iter::once(result.end.id))
        .collect();
    let titles = read::edited_titles(&state.pool, &ids)
        .await
        .map_err(|e| failure(ERROR, e))?;
    Ok(Json(PathResponse {
        start_track: path_track(result.start, &titles),
        end_track: path_track(result.end, &titles),
        path: result
            .path
            .into_iter()
            .map(|t| path_track(t, &titles))
            .collect(),
        metadata: PathMetadata {
            total_tracks: result.total_tracks,
            embedding_distance: result.embedding_distance,
            average_step_size: result.average_step_size,
            mode: if mode == Mode::Discovery {
                "discovery"
            } else {
                "smooth"
            }
            .into(),
        },
    })
    .into_response())
}
