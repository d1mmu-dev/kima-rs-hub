//! Subsonic podcast views (spec §4.9-ish, D-086 amended, M6 Task 18): `getPodcasts`,
//! `getNewestPodcasts`, `getPodcastEpisode` and `refreshPodcasts`. The decision/stream branches
//! for `mediaType=podcast` live in `routes/media.rs` instead, next to the song ones they share a
//! transport with.
//!
//! `refreshPodcasts` here is a synchronous, best-effort refresh of every show the caller
//! follows (upstream's `Promise.allSettled`): one show's feed failing never stops the others,
//! and the answer is always empty `ok`, as upstream. This is scoped to what the Subsonic
//! endpoint needs; it is distinct from Task 9's job-queued sweep (`kima_podcast::refresh::sweep`,
//! `podcast-refresh-sweep` cron in `kima-server`), which runs independently on its own 15-minute
//! schedule and does not replace the per-request orchestration below (D-205).

use axum::Router;
use axum::extract::State;
use kima_api::AppState;
use kima_db::podcasts::SubsonicEpisode;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids;
use crate::map;
use crate::params::Params;
use crate::response::{Answer, Node};

/// `getPodcasts`' episode cap per channel (upstream: "at most 200 newest").
const EPISODE_LIMIT: i64 = 200;
/// `getNewestPodcasts`' `count`: default and ceiling.
const DEFAULT_NEWEST: i64 = 20;
const MAX_NEWEST: i64 = 500;

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getPodcasts", podcasts);
    let router = add(router, "getNewestPodcasts", newest_podcasts);
    let router = add(router, "getPodcastEpisode", podcast_episode);
    add(router, "refreshPodcasts", refresh_podcasts)
}

/// An episode as upstream's `mapPodcastEpisode`. `suffix` is the content type's subtype
/// (episodes carry no file extension); `status` is always `"completed"` (kima-rs streams every
/// episode directly from its host or cache, D-204/D-167, never partially).
fn episode_node(e: &SubsonicEpisode) -> Node {
    let mime = e.mime_type.as_deref().unwrap_or("audio/mpeg");
    let suffix = mime.rsplit('/').next().unwrap_or("mpeg");
    Node::new("episode")
        .attr("id", e.id.to_string())
        .attr("parent", e.podcast_id.to_string())
        .attr("isDir", false)
        .attr("channelId", e.podcast_id.to_string())
        .attr("title", &e.title)
        .attr("album", &e.podcast_title)
        .attr("artist", "Podcast")
        .attr("coverArt", e.podcast_id.to_string())
        .attr_opt("description", e.description.clone())
        .attr("streamUrl", &e.audio_url)
        .attr("streamId", e.id.to_string())
        .attr("publishDate", map::iso(e.published_at))
        .attr("created", map::iso(e.created_at))
        .attr("duration", e.duration)
        .attr("status", "completed")
        .attr_opt("size", e.file_size)
        .attr("contentType", mime)
        .attr("suffix", suffix)
        .attr("isVideo", false)
        .attr("type", "podcast")
}

fn channel_node(c: &kima_db::podcasts::Channel, episodes: Vec<Node>) -> Node {
    Node::new("channel")
        .attr("id", c.id.to_string())
        .attr("url", &c.feed_url)
        .attr("title", &c.title)
        .attr_opt("description", c.description.clone())
        .attr("coverArt", c.id.to_string())
        .attr_opt("originalImageUrl", c.image_url.clone())
        .list("episode", episodes)
}

/// `includeEpisodes` unless the literal text `"false"`; an optional `id` narrows the list to
/// that one followed channel (an id that is not among them, or not a UUID at all, answers an
/// empty `podcasts` element, as upstream's "not found among the caller's shows" case).
async fn podcasts(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let include_episodes = params.get("includeEpisodes") != Some("false");
    let raw_id = params.get("id");
    let mut channels = kima_db::podcasts::followed_channels(&app.pool, user.user.id).await?;
    if let Some(raw) = raw_id {
        let wanted = ids::parse(raw, &[]);
        channels.retain(|c| wanted == Some(c.id));
    }
    let mut nodes = Vec::with_capacity(channels.len());
    for c in channels {
        let episodes = if include_episodes {
            kima_db::podcasts::episodes_for_channel(&app.pool, c.id, EPISODE_LIMIT)
                .await?
                .iter()
                .map(episode_node)
                .collect()
        } else {
            Vec::new()
        };
        nodes.push(channel_node(&c, episodes));
    }
    Ok(Answer::node(Node::new("podcasts").list("channel", nodes)))
}

/// `count` default 20, clamped 1-500.
async fn newest_podcasts(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let count = params
        .paging::<i64>("count")
        .unwrap_or(DEFAULT_NEWEST)
        .clamp(1, MAX_NEWEST);
    let episodes = kima_db::podcasts::newest_episodes(&app.pool, user.user.id, count).await?;
    Ok(Answer::node(Node::new("newestPodcasts").list(
        "episode",
        episodes.iter().map(episode_node).collect(),
    )))
}

const EPISODE_NOT_FOUND: &str = "Podcast episode not found";

/// Missing `id` is error 10; an id that is not a UUID, does not exist, or belongs to a show the
/// caller does not follow, is 70 (upstream: "only followed shows").
async fn podcast_episode(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let not_found = || SubsonicError::not_found(EPISODE_NOT_FOUND);
    let id = ids::parse(raw, &[]).ok_or_else(not_found)?;
    let episode = kima_db::podcasts::subsonic_episode_by_id(&app.pool, id)
        .await?
        .ok_or_else(not_found)?;
    if !kima_db::podcasts::is_subscribed(&app.pool, user.user.id, episode.podcast_id).await? {
        return Err(not_found());
    }
    Ok(Answer::node(episode_node(&episode)))
}

async fn refresh_podcasts(
    user: SubsonicUser,
    State(app): State<AppState>,
) -> Result<Answer, SubsonicError> {
    let shows = kima_db::podcasts::followed_shows(&app.pool, user.user.id).await?;
    for row in shows {
        let _ = kima_podcast::refresh::refresh(&app.podcast_catalog, row.id).await;
    }
    Ok(Answer::ok())
}
