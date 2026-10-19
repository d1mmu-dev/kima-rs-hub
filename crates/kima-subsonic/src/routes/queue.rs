//! The shared play queue (spec §4.5 #35-#38, §6.4). Subsonic and the web player use the user's one
//! `playback_state` row, so a queue saved here shows up in the web player and the other way round.

use std::collections::HashSet;

use axum::Router;
use axum::extract::State;
use kima_db::playback::{self, Patch, QUEUE_LIMIT};
use kima_db::subsonic::{self, SongItem};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::add;
use super::scrobble::client_name;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids;
use crate::map;
use crate::params::Params;
use crate::response::{Answer, Node};

const DEFAULT_CHANGED_BY: &str = "Kima";

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getPlayQueue", get_play_queue);
    let router = add(router, "getPlayQueueByIndex", get_play_queue_by_index);
    let router = add(router, "savePlayQueue", save_play_queue);
    add(router, "savePlayQueueByIndex", save_play_queue_by_index)
}

/// The queue's songs from current database rows. `kept[i]` says whether the i-th requested id
/// still names a present track; the songs are in the same order.
async fn hydrate(
    ctx: &SubsonicCtx,
    user: Uuid,
    requested: &[Option<Uuid>],
) -> Result<(Vec<SongItem>, Vec<bool>), SubsonicError> {
    let wanted: Vec<Uuid> = requested.iter().flatten().copied().collect();
    let songs = subsonic::tracks_by_ids(&ctx.app.pool, user, &wanted).await?;
    let present: HashSet<Uuid> = songs.iter().map(|s| s.track.id).collect();
    let kept = requested
        .iter()
        .map(|id| id.is_some_and(|id| present.contains(&id)))
        .collect();
    Ok((songs, kept))
}

/// Where the entry at `at` of the requested list sits once the dropped ones are gone: its own
/// place when it survived, else the next survivor (the last one at the end).
fn reindex(kept: &[bool], at: usize) -> usize {
    let before = kept.iter().take(at).filter(|k| **k).count();
    let total = kept.iter().filter(|k| **k).count();
    before.min(total.saturating_sub(1))
}

/// A song in the item shape the web player stores (`hydrateQueue` upstream).
fn queue_item(s: &SongItem) -> Value {
    let t = &s.track;
    json!({
        "id": t.id,
        "title": t.title,
        "duration": t.duration_secs,
        "artist": { "id": t.album_artist_id, "name": t.album_artist_name },
        "album": { "id": t.album_id, "title": t.album_title, "coverArt": t.album_id.to_string() },
    })
}

fn raw_ids(params: &Params) -> Vec<&str> {
    params
        .all("id")
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Milliseconds: lenient as upstream, so a bad or negative value is 0.
fn position(params: &Params) -> i32 {
    params
        .get("position")
        .and_then(|p| p.trim().parse::<i64>().ok())
        .filter(|p| *p >= 0)
        .map_or(0, |p| i32::try_from(p).unwrap_or(i32::MAX))
}

async fn save(
    ctx: &SubsonicCtx,
    user: &SubsonicUser,
    params: &Params,
    ids: &[&str],
    current_at: usize,
) -> Result<Answer, SubsonicError> {
    let requested: Vec<Option<Uuid>> = ids
        .iter()
        .take(QUEUE_LIMIT)
        .map(|raw| ids::parse(raw, &[]))
        .collect();
    let (songs, kept) = hydrate(ctx, user.user.id, &requested).await?;
    let index = if songs.is_empty() {
        0
    } else {
        reindex(&kept, current_at)
    };
    let queue = (!songs.is_empty()).then(|| Value::Array(songs.iter().map(queue_item).collect()));
    let patch = Patch {
        playback_type: "track".to_owned(),
        track_id: Some(songs.get(index).map(|s| s.track.id.to_string())),
        audiobook_id: Some(None),
        podcast_id: Some(None),
        queue: Some(queue),
        current_index: Some(i32::try_from(index).unwrap_or(0)),
        is_shuffle: Some(false),
        position: Some(position(params)),
        changed_by: Some(Some(client_name(params))),
    };
    playback::upsert(&ctx.app.pool, user.user.id, patch).await?;
    Ok(Answer::ok())
}

async fn save_play_queue(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let ids = raw_ids(&params);
    let current = params.get("current").filter(|c| !c.is_empty());
    let at = match (ids.is_empty(), current) {
        (true, _) => 0,
        (false, Some(c)) => ids.iter().position(|id| *id == c).ok_or_else(not_in_list)?,
        (false, None) => return Err(not_in_list()),
    };
    save(&ctx, &user, &params, &ids, at).await
}

fn not_in_list() -> SubsonicError {
    SubsonicError::new(10, "current must be present and included in id list")
}

async fn save_play_queue_by_index(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let ids = raw_ids(&params);
    let index = params.get("currentIndex");
    if ids.is_empty() {
        if index.is_some() {
            return Err(SubsonicError::new(
                10,
                "currentIndex must not be set when id is not provided",
            ));
        }
        return save(&ctx, &user, &params, &ids, 0).await;
    }
    let Some(index) = index else {
        return Err(SubsonicError::missing("currentIndex"));
    };
    let at = index
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|at| *at < ids.len())
        .ok_or_else(|| {
            SubsonicError::new(10, "currentIndex must be between 0 and queue length - 1")
        })?;
    save(&ctx, &user, &params, &ids, at).await
}

/// The stored queue, rebuilt: its songs now, and where the stored current index points.
struct Stored {
    songs: Vec<SongItem>,
    index: usize,
    position: i32,
    changed: OffsetDateTime,
    changed_by: String,
}

async fn load(ctx: &SubsonicCtx, user: &SubsonicUser) -> Result<Stored, SubsonicError> {
    let state = playback::get(&ctx.app.pool, user.user.id).await?;
    let Some(state) = state else {
        return Ok(Stored {
            songs: Vec::new(),
            index: 0,
            position: 0,
            changed: OffsetDateTime::now_utc(),
            changed_by: DEFAULT_CHANGED_BY.to_owned(),
        });
    };
    let requested: Vec<Option<Uuid>> = state
        .queue
        .as_ref()
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .map(|id| ids::parse(id, &[]))
                .collect()
        })
        .unwrap_or_default();
    let (songs, kept) = hydrate(ctx, user.user.id, &requested).await?;
    let stored = usize::try_from(state.current_index)
        .unwrap_or(0)
        .min(requested.len().saturating_sub(1));
    let index = if songs.is_empty() {
        0
    } else {
        reindex(&kept, stored)
    };
    Ok(Stored {
        songs,
        index,
        position: state.position,
        changed: state.updated_at,
        changed_by: state
            .changed_by
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| DEFAULT_CHANGED_BY.to_owned()),
    })
}

fn entries(songs: &[SongItem]) -> Vec<Node> {
    songs
        .iter()
        .map(|s| map::song(s).renamed("entry"))
        .collect()
}

async fn get_play_queue(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    _params: Params,
) -> Result<Answer, SubsonicError> {
    let q = load(&ctx, &user).await?;
    let node = Node::new("playQueue")
        .attr_opt(
            "current",
            q.songs.get(q.index).map(|s| s.track.id.to_string()),
        )
        .attr("position", q.position)
        .attr("username", &user.user.username)
        .attr("changed", map::iso(q.changed))
        .attr("changedBy", q.changed_by)
        .list("entry", entries(&q.songs));
    Ok(Answer::node(node))
}

async fn get_play_queue_by_index(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    _params: Params,
) -> Result<Answer, SubsonicError> {
    let q = load(&ctx, &user).await?;
    let node = Node::new("playQueueByIndex")
        .attr("currentIndex", q.index)
        .attr("position", q.position)
        .attr("username", &user.user.username)
        .attr("changed", map::iso(q.changed))
        .attr("changedBy", q.changed_by)
        .list("entry", entries(&q.songs));
    Ok(Answer::node(node))
}
