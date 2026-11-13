//! `GET /api/search?q=&type=&limit=`: library search, shaped as upstream `routes/search.ts`
//! (library part only). Artists, albums and tracks reuse the browse serializers.

use axum::extract::{Query, RawQuery, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::search;
use serde::Deserialize;
use serde_json::{Value, json};

use super::library::artists::artist_json;
use super::library::{album_json, parse_int, track_json};
use super::podcasts::ai::{AiFilter, from_db, put_label, retain_visible};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Longer queries are cut on a character boundary.
const MAX_QUERY_CHARS: usize = 500;

pub fn routes() -> Router<AppState> {
    Router::new().route("/search", get(search))
}

#[derive(Deserialize)]
struct Params {
    q: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<String>,
}

async fn search(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<Params>,
    RawQuery(raw): RawQuery,
) -> Result<(HeaderMap, Json<Value>), ApiError> {
    let q = params.q.as_deref().unwrap_or("").trim();
    let q: String = q.chars().take(MAX_QUERY_CHARS).collect();
    let limit = params
        .limit
        .as_deref()
        .and_then(parse_int)
        .unwrap_or(20)
        .clamp(1, 100);
    let kind = params.kind.as_deref().unwrap_or("all");
    let want = |name: &str| kind == "all" || kind == name;
    // Only the podcast groups are filtered (D-207): a music-only search neither reads the
    // setting nor answers `X-Kima-Ai-Hidden`.
    let filter = if want("podcasts") || want("episodes") {
        Some(AiFilter::resolve(&state, user.id, raw.as_deref()).await?)
    } else {
        None
    };

    let (mut artists, mut albums, mut tracks) = (Vec::new(), Vec::new(), Vec::new());
    let (mut podcasts, mut episodes) = (Vec::new(), Vec::new());
    if let Some(tsq) = search::prefix_query(&state.pool, &q).await? {
        if want("artists") {
            artists = search::artists(&state.pool, &tsq, limit, 0).await?;
        }
        if want("albums") {
            albums = search::albums(&state.pool, &tsq, limit, 0).await?;
        }
        if want("tracks") {
            tracks = search::tracks(&state.pool, &tsq, limit, 0).await?;
        }
        if want("podcasts") {
            podcasts = search::podcasts(&state.pool, user.id, &tsq, limit, 0).await?;
        }
        if want("episodes") {
            episodes = search::episodes(&state.pool, user.id, &tsq, limit, 0).await?;
        }
    }
    // Followed shows and their episodes are filtered like every podcast list (D-207).
    let headers = match filter {
        Some(filter) => {
            let hidden = retain_visible(filter, &mut podcasts, |p| from_db(&p.ai_label, None).0)
                + retain_visible(filter, &mut episodes, |e| from_db(&e.ai_label, None).0);
            filter.headers(hidden)
        }
        None => HeaderMap::new(),
    };
    Ok((
        headers,
        Json(json!({
            "artists": artists.iter().map(artist_json).collect::<Vec<_>>(),
            "albums": albums.iter().map(album_json).collect::<Vec<_>>(),
            "tracks": tracks.iter().map(track_json).collect::<Vec<_>>(),
            "audiobooks": [],
            "podcasts": podcasts.iter().map(podcast_json).collect::<Vec<_>>(),
            "episodes": episodes.iter().map(episode_json).collect::<Vec<_>>(),
        })),
    ))
}

/// A followed show found by search, with its AI label (D-207).
fn podcast_json(p: &search::PodcastSearchRow) -> Value {
    let mut v = json!({
        "id": p.id,
        "title": p.title,
        "author": p.author,
        "description": p.description,
        "imageUrl": p.image_url,
        "episodeCount": p.episode_count,
    });
    put_label(&mut v, from_db(&p.ai_label, p.ai_label_source.as_deref()));
    v
}

fn episode_json(e: &search::EpisodeSearchRow) -> Value {
    let mut v = json!({
        "id": e.id,
        "title": e.title,
        "description": e.description,
        "podcastId": e.podcast_id,
        "podcastTitle": e.podcast_title,
        "publishedAt": kima_api_types::stats::js_iso(e.published_at),
        "duration": e.duration,
        "audioUrl": e.audio_url,
    });
    put_label(&mut v, from_db(&e.ai_label, e.ai_label_source.as_deref()));
    v
}
