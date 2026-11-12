use crate::routes::podcasts::progress::{clear, progress};
use axum::extract::{Path, RawQuery, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

mod discovery;
mod preview;
mod progress;
mod stream;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/podcasts", get(list))
        .route("/podcasts/subscribe", post(subscribe))
        .route("/podcasts/{id}/unsubscribe", delete(unsubscribe))
        .route("/podcasts/listening", get(listening))
        .route("/podcasts/{id}", get(detail))
        .route("/podcasts/{id}/episodes", get(episodes))
        .route("/podcasts/{podcast}/episodes/{episode}/next", get(next))
        .route(
            "/podcasts/{podcast}/episodes/{episode}/progress",
            post(progress).delete(clear),
        )
        .merge(stream::routes())
        .merge(discovery::routes())
        .merge(preview::routes())
}

async fn subscribe(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let feed = body
        .get("feedUrl")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let apple = match body.get("itunesId") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    if feed.is_none() && apple.is_none() {
        return Err(ApiError::BadRequest(
            "feedUrl or itunesId is required".into(),
        ));
    }
    let show = kima_db::podcasts::show_by_feed_or_itunes(
        &state.pool,
        feed.unwrap_or(""),
        apple.as_deref(),
    )
    .await?;
    let resolved_feed = if show.is_none() && feed.is_none() {
        let id = apple.as_deref().unwrap_or("");
        state
            .podcast_catalog
            .lookup(
                &[id.to_string()],
                kima_podcast::catalog::Priority::Interactive,
            )
            .await
            .map_err(|e| match e {
                kima_podcast::upstream::Upstream::Missing(_) => {
                    ApiError::NotFound("Nothing was found at that feed address")
                }
                kima_podcast::upstream::Upstream::Down(reason)
                    if reason.starts_with("BAD_RESPONSE:") =>
                {
                    ApiError::BadGateway("Apple Podcasts did not answer; try again shortly")
                }
                kima_podcast::upstream::Upstream::Down(_) => ApiError::BadGateway(
                    "The show's feed or Apple Podcasts did not answer; try again shortly",
                ),
                kima_podcast::upstream::Upstream::Internal(e) => ApiError::Internal(e),
            })?
            .remove(id)
            .and_then(|s| s.feed_url)
    } else {
        None
    };
    let feed = feed.or(resolved_feed.as_deref());
    let show = if let Some(show) = show {
        show
    } else if let Some(feed_url) = feed {
        if let Some(existing) =
            kima_db::podcasts::show_by_feed_or_itunes(&state.pool, feed_url, apple.as_deref())
                .await?
        {
            let inserted = kima_db::podcasts::subscribe(&state.pool, user.id, existing.id).await?;
            return Ok(Json(
                json!({"success": true, "podcast": {"id": existing.id, "title": existing.title},
                "message": if inserted {"Subscribed successfully"} else {"Already subscribed"}}),
            ));
        }
        state
            .net
            .validate_feed_url(feed_url)
            .await
            .map_err(|why| ApiError::BadRequest(format!("Invalid feed URL: {why}")))?;
        use kima_net::{Conditional, FeedFetch, Outcome};
        let fetched = state
            .net
            .get_feed(
                feed_url,
                Conditional::default(),
                std::time::Duration::from_secs(30),
            )
            .await;
        let (text, etag, modified) = match fetched {
            Outcome::Answered(FeedFetch::Body {
                text,
                etag,
                last_modified,
            }) => (text, etag, last_modified),
            Outcome::Absent(_) => {
                return Err(ApiError::NotFound("Nothing was found at that feed address"));
            }
            _ => {
                return Err(ApiError::BadGateway(
                    "The show's feed or Apple Podcasts did not answer; try again shortly",
                ));
            }
        };
        let parsed = kima_podcast::feed::parse(&text, time::OffsetDateTime::now_utc())
            .map_err(|e| ApiError::BadFeed(e.to_string()))?;
        let mut verified = None;
        for candidate in [apple.as_deref(), parsed.show.itunes_id.as_deref()]
            .into_iter()
            .flatten()
        {
            if candidate.is_empty() || !candidate.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            if let Ok(mut shows) = state
                .podcast_catalog
                .lookup(
                    &[candidate.to_string()],
                    kima_podcast::catalog::Priority::Interactive,
                )
                .await
            {
                if shows
                    .remove(candidate)
                    .and_then(|s| s.feed_url)
                    .is_some_and(|url| kima_net::same_feed(&url, feed_url))
                {
                    verified = Some(candidate.to_string());
                    break;
                }
            }
        }
        let categories = state
            .podcast_catalog
            .resolve_categories(
                verified.as_deref(),
                &parsed.show.category_names,
                kima_podcast::catalog::Priority::Interactive,
            )
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "could not assign podcast categories");
                Vec::new()
            });
        let input = kima_db::podcasts::NewShow {
            feed_url,
            title: &parsed.show.title,
            author: parsed.show.author.as_deref(),
            description: parsed.show.description.as_deref(),
            image_url: parsed.show.image_url.as_deref(),
            itunes_id: verified.as_deref(),
            category_ids: &categories,
            language: parsed.show.language.as_deref(),
            explicit: parsed.show.explicit,
            episode_count: parsed.episodes.len() as i32,
            feed_etag: etag.as_deref(),
            feed_last_modified: modified.as_deref(),
        };
        let mut tx = state.pool.begin().await?;
        match kima_db::podcasts::insert_show_on(&mut tx, uuid::Uuid::now_v7(), &input).await {
            Ok(show) => {
                let episodes: Vec<_> = parsed
                    .episodes
                    .into_iter()
                    .map(|e| kima_db::podcasts::NewEpisode {
                        guid: e.guid,
                        title: e.title,
                        description: e.description,
                        audio_url: e.audio_url,
                        duration: e.duration,
                        published_at: e.published_at,
                        episode_number: e.episode_number,
                        season: e.season,
                        image_url: e.image_url,
                        file_size: e.file_size,
                        mime_type: Some(e.mime_type),
                    })
                    .collect();
                kima_db::podcasts::insert_episodes_on(&mut tx, show.id, &episodes).await?;
                tx.commit().await?;
                show
            }
            Err(e)
                if e.as_database_error()
                    .is_some_and(|d| d.is_unique_violation()) =>
            {
                // Release the aborted transaction's connection before acquiring from the pool.
                tx.rollback().await?;
                kima_db::podcasts::show_by_feed_or_itunes(
                    &state.pool,
                    feed_url,
                    verified.as_deref(),
                )
                .await?
                .ok_or(e)?
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        return Err(ApiError::NotFound("Podcast not found in iTunes"));
    };
    let inserted = kima_db::podcasts::subscribe(&state.pool, user.id, show.id).await?;
    Ok(Json(
        json!({"success": true, "podcast": {"id": show.id, "title": show.title},
        "message": if inserted {"Subscribed successfully"} else {"Already subscribed"}}),
    ))
}

async fn list(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<kima_api_types::podcasts::FollowedShow>>, ApiError> {
    use kima_api_types::podcasts::{EpisodeSummary, FollowedShow};
    use kima_api_types::stats::js_iso;
    let mut out = Vec::new();
    for show in kima_db::podcasts::followed_shows(&state.pool, user.id).await? {
        let rows = kima_db::podcasts::recent_episodes(&state.pool, user.id, show.id).await?;
        let episodes: Vec<_> = rows
            .into_iter()
            .map(|e| {
                let progress = progress_of(
                    e.current_time,
                    e.progress_duration,
                    e.is_finished,
                    e.last_played_at,
                );
                EpisodeSummary {
                    id: e.id,
                    title: e.title,
                    description: e.description,
                    duration: e.duration,
                    published_at: js_iso(e.published_at),
                    cover_url: e.image_url,
                    progress,
                }
            })
            .collect();
        out.push(FollowedShow {
            id: show.id,
            title: show.title,
            author: show.author,
            description: show.description,
            cover_url: show.image_url,
            episode_count: show.episode_count,
            auto_download_episodes: false,
            latest_episode_at: episodes.first().map(|e| e.published_at.clone()),
            episodes,
        });
    }
    Ok(Json(out))
}

async fn unsubscribe(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::NotFound("Not subscribed to this podcast"))?;
    if !kima_db::podcasts::unsubscribe(&state.pool, user.id, id).await? {
        return Err(ApiError::NotFound("Not subscribed to this podcast"));
    }
    Ok(Json(
        json!({"success": true, "message": "Unsubscribed successfully"}),
    ))
}

struct Page {
    limit: i64,
    offset: i64,
    oldest: bool,
}
fn page(raw: Option<String>) -> Page {
    let args = super::stats::args::Args(
        form_urlencoded::parse(raw.as_deref().unwrap_or("").as_bytes())
            .into_owned()
            .collect(),
    );
    let l = args.get("limit").number();
    let o = args.get("offset").number();
    Page {
        limit: if l.is_finite() && l > 0.0 {
            l.floor().min(200.0) as i64
        } else {
            50
        },
        offset: if o.is_finite() && o > 0.0 {
            o.floor() as i64
        } else {
            0
        },
        oldest: args.get("order").string() == Some("oldest"),
    }
}

async fn checked_page(
    state: &AppState,
    user: uuid::Uuid,
    id: &str,
    raw: Option<String>,
) -> Result<
    (
        uuid::Uuid,
        Page,
        Vec<kima_api_types::podcasts::EpisodeRow>,
        i64,
    ),
    ApiError,
> {
    use kima_api_types::podcasts::EpisodeRow;
    use kima_api_types::stats::js_iso;
    let missing = || ApiError::NotFound("Podcast not found or not subscribed");
    let id = uuid::Uuid::parse_str(id).map_err(|_| missing())?;
    if !kima_db::podcasts::is_subscribed(&state.pool, user, id).await? {
        return Err(missing());
    }
    let page = page(raw);
    let (rows, total) = kima_db::podcasts::episode_page(
        &state.pool,
        user,
        id,
        page.limit,
        page.offset,
        page.oldest,
    )
    .await?;
    let rows = rows
        .into_iter()
        .map(|e| {
            let progress = progress_of(
                e.current_time,
                e.progress_duration,
                e.is_finished,
                e.last_played_at,
            );
            EpisodeRow {
                id: e.id,
                title: e.title,
                description: e.description,
                duration: e.duration,
                published_at: js_iso(e.published_at),
                episode_number: e.episode_number,
                season: e.season,
                image_url: e.image_url,
                is_downloaded: e.is_downloaded,
                progress,
            }
        })
        .collect();
    Ok((id, page, rows, total))
}

async fn episodes(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let (_, page, episodes, total) = checked_page(&state, user.id, &id, raw).await?;
    Ok(Json(
        json!({"episodes": episodes, "total": total, "offset": page.offset, "limit": page.limit}),
    ))
}

async fn detail(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let (id, page, episodes, total) = checked_page(&state, user.id, &id, raw).await?;
    let show = kima_db::podcasts::show_by_id(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("Podcast not found"))?;
    let mut categories = Vec::new();
    if !show.category_ids.is_empty() {
        if let Ok(names) = state.podcast_catalog.categories().await {
            for id in &show.category_ids {
                if let Some(c) = names.iter().find(|c| c.id == *id) {
                    categories.push(json!({"id": c.id, "name": c.name}));
                }
            }
        }
    }
    let genres: Vec<_> = categories.iter().map(|c| c["name"].clone()).collect();
    Ok(Json(
        json!({"id": show.id, "title": show.title, "author": show.author, "description": show.description,
        "coverUrl": show.image_url, "feedUrl": show.feed_url, "genres": genres, "categories": categories,
        "itunesId": show.itunes_id, "autoDownloadEpisodes": false, "episodes": episodes,
        "episodeTotal": total, "episodeOffset": page.offset, "episodeLimit": page.limit, "isSubscribed": true}),
    ))
}

async fn next(
    State(state): State<AppState>,
    user: AuthUser,
    Path((show, episode)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    use kima_api_types::stats::js_iso;
    let missing = || ApiError::NotFound("Episode not found");
    let show = uuid::Uuid::parse_str(&show).map_err(|_| missing())?;
    let episode = uuid::Uuid::parse_str(&episode).map_err(|_| missing())?;
    let next = kima_db::podcasts::next_episode(&state.pool, user.id, show, episode)
        .await?
        .ok_or_else(missing)?;
    let Some(e) = next else {
        return Ok(Json(json!({"episode": null})));
    };
    let show = kima_db::podcasts::show_by_id(&state.pool, show)
        .await?
        .ok_or_else(missing)?;
    let progress = progress_of(
        e.current_time,
        e.progress_duration,
        e.is_finished,
        e.last_played_at,
    );
    Ok(Json(
        json!({"podcast": {"id": show.id, "title": show.title, "coverUrl": show.image_url},
        "episode": {"id": e.id, "title": e.title, "duration": e.duration, "publishedAt": js_iso(e.published_at), "coverUrl": e.image_url, "progress": progress}}),
    ))
}

async fn listening(State(state): State<AppState>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    use kima_api_types::stats::js_iso;
    let (in_progress, new_episodes) = kima_db::podcasts::listening(&state.pool, user.id).await?;
    let convert = |e: kima_db::podcasts::ListeningRow| {
        let progress = progress_of(
            e.current_time,
            e.progress_duration,
            e.is_finished,
            e.last_played_at,
        );
        json!({"podcast": {"id": e.podcast_id, "title": e.podcast_title, "coverUrl": e.podcast_image},
            "episode": {"id": e.id, "title": e.title, "description": e.description, "duration": e.duration,
            "publishedAt": js_iso(e.published_at), "coverUrl": e.image_url, "progress": progress}})
    };
    Ok(Json(
        json!({"inProgress": in_progress.into_iter().map(convert).collect::<Vec<_>>(),
        "newEpisodes": new_episodes.into_iter().map(convert).collect::<Vec<_>>()}),
    ))
}

fn progress_of(
    current: Option<f64>,
    duration: Option<f64>,
    finished: Option<bool>,
    last: Option<time::OffsetDateTime>,
) -> Option<kima_api_types::podcasts::Progress> {
    let (current_time, duration, is_finished, last_played_at) =
        (current?, duration?, finished?, last?);
    Some(kima_api_types::podcasts::Progress {
        current_time,
        progress: if duration > 0.0 {
            ((current_time / duration) * 100.0).min(100.0)
        } else {
            0.0
        },
        is_finished,
        last_played_at: kima_api_types::stats::js_iso(last_played_at),
    })
}
