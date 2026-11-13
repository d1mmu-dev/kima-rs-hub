//! `GET /api/podcasts/preview/{itunesId}[/snippet]` (Task 14, `podcasts.ts:326-433`): a show
//! not followed yet, read from its own feed, with its oldest dated episode playable as a
//! sample before subscribing.

use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use kima_api_types::stats::js_iso;
use kima_net::{Conditional, FeedFetch, Outcome};
use kima_podcast::catalog::{Priority, oldest_episode};
use kima_podcast::feed::FeedEpisode;
use kima_podcast::preview::{self, Remembered, Snippet};
use serde_json::{Value, json};

use super::discovery::apple_failure;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/podcasts/preview/{itunes_id}", get(preview_show))
        .route("/podcasts/preview/{itunes_id}/snippet", get(sample))
}

/// How long opening the sample's audio may take before a busy or gateway answer (mirrors
/// `routes/podcasts/stream.rs`'s own `FIRST_BYTE`).
const FIRST_BYTE: Duration = Duration::from_secs(30);

fn missing_podcast() -> ApiError {
    ApiError::NotFound("Podcast not found")
}

fn no_sample() -> ApiError {
    ApiError::NotFound("This podcast has no episode to sample")
}

/// `router.param("itunesId")`: digits only, else 404 before any Apple or feed call.
fn checked_itunes_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(missing_podcast());
    }
    Ok(())
}

fn header(v: impl AsRef<str>) -> HeaderValue {
    HeaderValue::from_str(v.as_ref()).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// `{title,publishedAt,duration,description}`, without `audioUrl` (used for both a preview
/// episode row and the preview's own `snippet` key).
fn episode_json(ep: &FeedEpisode) -> Value {
    json!({
        "title": ep.title,
        "publishedAt": js_iso(ep.published_at),
        "duration": ep.duration,
        "description": ep.description,
    })
}

async fn categories_and_genres(state: &AppState, ids: &[i32]) -> (Vec<Value>, Vec<Value>) {
    let mut categories = Vec::new();
    if !ids.is_empty()
        && let Ok(names) = state.podcast_catalog.categories().await
    {
        for id in ids {
            if let Some(c) = names.iter().find(|c| c.id == *id) {
                categories.push(json!({"id": c.id, "name": c.name}));
            }
        }
    }
    let genres: Vec<_> = categories.iter().map(|c| c["name"].clone()).collect();
    (categories, genres)
}

async fn preview_show(
    State(state): State<AppState>,
    user: AuthUser,
    Path(itunes_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    checked_itunes_id(&itunes_id)?;
    let mut shows = state
        .podcast_catalog
        .lookup(std::slice::from_ref(&itunes_id), Priority::Interactive)
        .await
        .map_err(apple_failure)?;
    let show = shows.remove(&itunes_id).ok_or_else(missing_podcast)?;

    let stored = kima_db::podcasts::show_by_feed_or_itunes(
        &state.pool,
        show.feed_url.as_deref().unwrap_or(""),
        Some(&itunes_id),
    )
    .await?;
    let (is_subscribed, subscribed_podcast_id) = match &stored {
        Some(s) => (
            kima_db::podcasts::is_subscribed(&state.pool, user.id, s.id).await?,
            Some(s.id),
        ),
        None => (false, None),
    };

    let mut preview_episodes: Vec<Value> = Vec::new();
    let mut snippet_json = Value::Null;
    // The channel's `ai-content` tag as this read found it (`Some(None)`: read, no tag).
    let mut channel_tag = None;
    if let Some(feed_url) = &show.feed_url {
        // A feed that does not answer in time (or at all) is tolerated: Apple's own data still
        // answers (`podcasts.ts:343-347`).
        let fetched = state
            .net
            .get_feed(feed_url, Conditional::default(), state.preview_feed_timeout)
            .await;
        if let Outcome::Answered(FeedFetch::Body { text, .. }) = fetched
            && let Ok(parsed) = kima_podcast::feed::parse(&text, time::OffsetDateTime::now_utc())
        {
            preview_episodes = preview::newest_episodes(&parsed.episodes)
                .into_iter()
                .map(episode_json)
                .collect();
            let oldest = oldest_episode(&parsed.episodes);
            let snippet = oldest.map(Snippet::from_episode);
            let _ = preview::remember_snippet(&state.pool, &itunes_id, snippet.as_ref()).await;
            snippet_json = oldest.map_or(Value::Null, episode_json);
            channel_tag = Some(parsed.show.ai_feed);
            // Remembered so discovery items can carry it (spec S3); a failure only costs a label.
            let _ = kima_db::podcasts::remember_feed_label(
                &state.pool,
                &itunes_id,
                parsed.show.ai_feed,
            )
            .await;
        }
    }

    // A stored show's own label (with its admin override) first, then this read's channel tag,
    // then one remembered from an earlier read; else unknown. A preview is a direct read: it
    // is labelled, never hidden.
    use kima_api_types::podcasts::{AiLabel, show_label};
    let label = if let Some(s) = &stored {
        show_label(s.ai_override.as_deref(), s.ai_feed)
    } else if let Some(tag) = channel_tag {
        show_label(None, tag)
    } else {
        kima_db::podcasts::labels_for_itunes(&state.pool, std::slice::from_ref(&itunes_id))
            .await?
            .remove(&itunes_id)
            .map(|l| super::ai::from_db(&l.ai_label, l.ai_label_source.as_deref()))
            .unwrap_or((AiLabel::Unknown, None))
    };
    let (categories, genres) = categories_and_genres(&state, &show.category_ids).await;
    let mut body = json!({
        "itunesId": show.itunes_id,
        "title": show.title,
        "author": show.author,
        "description": stored.as_ref().and_then(|s| s.description.clone()),
        "coverUrl": show.cover_url,
        "feedUrl": show.feed_url,
        "genres": genres,
        "categories": categories,
        "episodeCount": show.episode_count,
        "previewEpisodes": preview_episodes,
        "snippet": snippet_json,
        "isSubscribed": is_subscribed,
        "subscribedPodcastId": subscribed_podcast_id,
    });
    super::ai::put_label(&mut body, label);
    Ok(Json(body))
}

/// `GET /preview/:itunesId/snippet`: the remembered sample, or a fresh feed read when the show
/// has never been previewed.
async fn sample(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(itunes_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    checked_itunes_id(&itunes_id)?;
    let snippet = match preview::remembered_snippet(&state.pool, &itunes_id).await? {
        Some(Remembered::Some(s)) => s,
        Some(Remembered::None) => return Err(no_sample()),
        None => read_fresh_sample(&state, &itunes_id).await?,
    };

    if let Err(why) = state.net.validate_feed_url(&snippet.audio_url).await {
        return Err(ApiError::BadRequest(format!(
            "Invalid episode audio URL: {why}"
        )));
    }

    let forwarded = headers.get(RANGE).and_then(|v| v.to_str().ok());
    let res = match state
        .net
        .open_audio(&snippet.audio_url, forwarded, FIRST_BYTE)
        .await
    {
        Outcome::Answered(res) => res,
        Outcome::Absent(_) => return Err(ApiError::NotFound("Episode audio not found")),
        _ => {
            return Err(ApiError::BadGateway(
                "The preview's host did not answer; try again shortly",
            ));
        }
    };

    let status = StatusCode::from_u16(res.status).unwrap_or(StatusCode::OK);
    let mut builder = Response::builder()
        .status(status)
        .header(
            CONTENT_TYPE,
            header(res.content_type.as_deref().unwrap_or(&snippet.mime_type)),
        )
        .header(ACCEPT_RANGES, "bytes")
        .header(CACHE_CONTROL, "no-store");
    if let Some(len) = &res.content_length {
        builder = builder.header(CONTENT_LENGTH, header(len));
    }
    if let Some(range) = &res.content_range {
        builder = builder.header(CONTENT_RANGE, header(range));
    }
    builder
        .body(Body::from_stream(res.body))
        .map_err(|e| ApiError::Internal(e.into()))
}

/// A show never previewed yet: looked up, its feed read once, the pick remembered either way.
async fn read_fresh_sample(state: &AppState, itunes_id: &str) -> Result<Snippet, ApiError> {
    let mut shows = state
        .podcast_catalog
        .lookup(
            std::slice::from_ref(&itunes_id.to_string()),
            Priority::Interactive,
        )
        .await
        .map_err(apple_failure)?;
    let show = shows.remove(itunes_id).ok_or_else(missing_podcast)?;
    let Some(feed_url) = show.feed_url else {
        let _ = preview::remember_snippet(&state.pool, itunes_id, None).await;
        return Err(no_sample());
    };
    let fetched = state
        .net
        .get_feed(
            &feed_url,
            Conditional::default(),
            state.preview_feed_timeout,
        )
        .await;
    let text = match fetched {
        Outcome::Answered(FeedFetch::Body { text, .. }) => text,
        _ => {
            return Err(ApiError::BadGateway(
                "The show's feed did not answer; try again shortly",
            ));
        }
    };
    let parsed = kima_podcast::feed::parse(&text, time::OffsetDateTime::now_utc())
        .map_err(|_| no_sample())?;
    let snippet = oldest_episode(&parsed.episodes).map(Snippet::from_episode);
    let _ = preview::remember_snippet(&state.pool, itunes_id, snippet.as_ref()).await;
    snippet.ok_or_else(no_sample)
}
