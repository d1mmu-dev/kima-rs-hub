//! Authenticated library-global vibe reads. No personal/profile data is read or cached here.
use crate::{auth::AuthUser, error::ApiError, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kima_api_types::vibe::{Computing, MapResponse, MapTrack, MoodShare, MoodSummary, TrackMood};
use kima_db::vibe::{moods::MoodSnapshot, read};
use kima_ident::stages::{embedding, moods, vibe_assets};
use kima_vibe::moods::{SummaryTrack, mood_output, summarise};
use std::sync::Arc;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/vibe/map", get(map))
        .route("/vibe/mood-summary", get(summary))
        .route("/vibe/mood/{track_id}", get(mood))
        .route("/vibe/search", post(search))
}

async fn search(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    let query = body
        .get("query")
        .and_then(|q| q.as_str())
        .unwrap_or_default();
    if query.trim().encode_utf16().count() < 2 {
        return Err(ApiError::BadRequest(
            "Query must be at least 2 characters".into(),
        ));
    }
    if query.encode_utf16().count() > 500 {
        return Err(ApiError::BadRequest(
            "Query must be at most 500 characters".into(),
        ));
    }
    let ctx = state.ident_ctx();
    let vector = match embedding::vibe_embed_text(&ctx,query.trim(),state.vibe_text_embedder.as_ref()).await {
        Ok(v) => v,
        Err(embedding::TextEmbeddingError::Unavailable(_)) => return Ok((StatusCode::SERVICE_UNAVAILABLE,Json(serde_json::json!({"error":"Text embedding service unavailable","message":"The CLAP text model is not available"}))).into_response()),
        Err(embedding::TextEmbeddingError::Timeout) => return Ok((StatusCode::GATEWAY_TIMEOUT,Json(serde_json::json!({"error":"Text embedding service unavailable","message":"The CLAP analyzer service did not respond in time"}))).into_response()),
        Err(e) => return Err(failure("Failed to search tracks by vibe",e)),
    };
    let rows = kima_db::vibe::search::search_tracks_by_embedding(
        &state.pool,
        &vector,
        parse_limit(body.get("limit")),
        ctx.config.vibe.exact_max,
    )
    .await
    .map_err(|e| failure("Failed to search tracks by vibe", e))?;
    let titles = read::edited_titles(&state.pool, &rows.iter().map(|r| r.id).collect::<Vec<_>>())
        .await
        .map_err(|e| failure("Failed to search tracks by vibe", e))?;
    let tracks = rows
        .into_iter()
        .map(|r| kima_api_types::vibe::SearchTrack {
            id: r.id.to_string(),
            title: r.title,
            display_title: titles.get(&r.id).cloned().flatten(),
            duration: r.duration,
            track_no: r.track_no,
            distance: r.distance,
            similarity: (1.0 - r.distance).max(0.0),
            album: kima_api_types::vibe::SearchAlbum {
                id: r.album_id.to_string(),
                title: r.album_title,
                cover_url: r.album_has_cover.then(|| r.album_id.to_string()),
            },
            artist: kima_api_types::vibe::SearchArtist {
                id: r.artist_id.to_string(),
                name: r.artist_name,
            },
        })
        .collect();
    Ok(Json(kima_api_types::vibe::SearchResponse {
        query: query.trim().to_owned(),
        tracks,
    })
    .into_response())
}

/// Upstream parseInt(raw, 10) || 20, clamped to [1,100].
fn parse_limit(raw: Option<&serde_json::Value>) -> usize {
    let text = match raw {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => return 20,
    };
    let text = text.trim_start();
    let negative = text.starts_with('-');
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    let digits: String = unsigned
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return 20;
    }
    let n = digits.parse::<u64>().unwrap_or(u64::MAX);
    if n == 0 {
        20
    } else if negative {
        1
    } else {
        n.min(100) as usize
    }
}

fn failure(message: &'static str, error: impl std::fmt::Debug) -> ApiError {
    tracing::error!(?error, message, "Vibe read failed");
    ApiError::Failed(message)
}

async fn snapshot(state: &AppState) -> sqlx::Result<Option<Arc<MoodSnapshot>>> {
    let Some(ctx) = &state.ident else {
        return Ok(None);
    };
    let Some(rt) = moods::runtime(ctx).await else {
        return Ok(None);
    };
    rt.snapshots
        .current(&state.pool, rt.heads())
        .await
        .map(Some)
}

async fn pending(state: &AppState) -> sqlx::Result<bool> {
    let Some(ctx) = &state.ident else {
        return Ok(false);
    };
    if ctx.config.embedding.disabled {
        return Ok(false);
    }
    let assets = vibe_assets::initialize(ctx).await;
    if !assets.as_ref().is_ok_and(|a| a.probe.is_some()) {
        return Ok(false);
    }
    let sound_available = embedding::cached_status(ctx).is_some_and(|s| {
        s.capabilities.get(&kima_ml::Capability::ClapAudioEmbedding)
            == Some(&kima_ml::CapabilityStatus::Available)
    });
    read::layout_pending(&state.pool, sound_available).await
}

fn computing() -> Response {
    (StatusCode::ACCEPTED, Json(Computing { computing: true })).into_response()
}

async fn map(
    State(state): State<AppState>,
    _user: AuthUser,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to read the vibe map";
    let read = read::read_map(&state.pool)
        .await
        .map_err(|e| failure(ERROR, e))?;
    if read.rows.is_empty() && pending(&state).await.map_err(|e| failure(ERROR, e))? {
        return Ok(computing());
    }
    let snap = snapshot(&state).await.map_err(|e| failure(ERROR, e))?;
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for r in &read.rows {
        for (d, v) in [r.x, r.y, r.z].into_iter().enumerate() {
            min[d] = min[d].min(v);
            max[d] = max[d].max(v);
        }
    }
    let range = (0..3).map(|d| max[d] - min[d]).fold(0.0, f64::max);
    let range = if range > 0.0 { range } else { 1.0 };
    let centre: [f64; 3] = std::array::from_fn(|d| (min[d] + max[d]) / 2.0);
    let tracks: Vec<_> = read
        .rows
        .into_iter()
        .map(|r| {
            let mood = mood_output(snap.as_ref().and_then(|s| s.views.get(&r.id)));
            MapTrack {
                id: r.id.to_string(),
                x: 0.5 + (r.x - centre[0]) / range,
                y: 0.5 + (r.y - centre[1]) / range,
                z: 0.5 + (r.z - centre[2]) / range,
                colour: r.colour,
                title: r.title,
                artist: r.artist,
                artist_id: r.artist_id.to_string(),
                album_id: r.album_id.to_string(),
                cover_url: r.has_cover.then(|| r.album_id.to_string()),
                dominant_mood: mood.dominant_mood.into(),
                mood_score: mood.mood_score,
                moods: mood
                    .moods
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v))
                    .collect(),
                energy: None,
                valence: r.valence,
            }
        })
        .collect();
    let at = read
        .computed_at
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(time::UtcOffset::UTC);
    let computed_at = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    );
    let data = MapResponse {
        track_count: tracks.len(),
        tracks,
        computed_at,
        mood_signature: snap.map_or_else(
            || kima_db::vibe::moods::NO_STATISTICS.to_owned(),
            |s| s.signature.clone(),
        ),
    };
    let json = tokio::task::spawn_blocking(move || serde_json::to_vec(&data))
        .await
        .map_err(|e| failure(ERROR, e))?
        .map_err(|e| failure(ERROR, e))?;
    let accepts_gzip = match super::vibe_encoding::gzip(&headers) {
        Ok(gzip) => gzip,
        Err(status) => return Ok((status, Json(serde_json::json!({"error": if status == StatusCode::NOT_ACCEPTABLE { "No acceptable map encoding" } else { "Invalid Accept-Encoding" }}))).into_response()),
    };
    let body = if accepts_gzip {
        state
            .vibe_map_cache
            .gzip(json)
            .await
            .map_err(|e| failure(ERROR, e))?
    } else {
        json.into()
    };
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/json; charset=utf-8"
            .parse()
            .map_err(|e| failure(ERROR, e))?,
    );
    response.headers_mut().insert(
        header::VARY,
        "Accept-Encoding".parse().map_err(|e| failure(ERROR, e))?,
    );
    if accepts_gzip {
        response.headers_mut().insert(
            header::CONTENT_ENCODING,
            "gzip".parse().map_err(|e| failure(ERROR, e))?,
        );
    }
    Ok(response)
}

async fn summary(State(state): State<AppState>, _user: AuthUser) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to read the mood summary";
    let read = read::read_map(&state.pool)
        .await
        .map_err(|e| failure(ERROR, e))?;
    if read.rows.is_empty() && pending(&state).await.map_err(|e| failure(ERROR, e))? {
        return Ok(computing());
    }
    let snap = snapshot(&state).await.map_err(|e| failure(ERROR, e))?;
    let ids: Vec<_> = read
        .rows
        .iter()
        .map(|r| (r.album_id.to_string(), r.artist_id.to_string()))
        .collect();
    let tracks: Vec<_> = read
        .rows
        .iter()
        .zip(&ids)
        .map(|(r, (album, artist))| SummaryTrack {
            dominant: mood_output(snap.as_ref().and_then(|s| s.views.get(&r.id))).dominant_mood,
            album_id: album,
            artist_id: artist,
        })
        .collect();
    let result = summarise(&tracks);
    Ok(Json(MoodSummary {
        shares: result
            .shares
            .into_iter()
            .map(|s| MoodShare {
                key: s.key,
                fraction: s.fraction,
            })
            .collect(),
        albums: result.albums,
        artists: result.artists,
    })
    .into_response())
}

async fn mood(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<TrackMood>, ApiError> {
    let snap = snapshot(&state)
        .await
        .map_err(|e| failure("Failed to read the track's mood", e))?;
    let id = id.parse::<uuid::Uuid>().ok();
    let view = snap
        .as_ref()
        .and_then(|s| id.and_then(|id| s.views.get(&id)));
    Ok(Json(TrackMood {
        dominant_mood: mood_output(view).dominant_mood.into(),
    }))
}
