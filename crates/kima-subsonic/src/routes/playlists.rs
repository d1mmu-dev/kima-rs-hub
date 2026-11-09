//! Playlists (spec §4.6 #42-#46), on M1's playlist layer. Every change goes through its row-locked
//! helpers, so concurrent edits keep the order dense.

use axum::Router;
use axum::extract::State;
use kima_core::events::Event;
use kima_db::playlists::{self, Playlist, PlaylistSummary, SubsonicWrite};
use kima_db::subsonic;
use serde_json::json;
use uuid::Uuid;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids;
use crate::map;
use crate::params::Params;
use crate::response::{Answer, Node};

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getPlaylists", get_playlists);
    let router = add(router, "getPlaylist", get_playlist);
    let router = add(router, "createPlaylist", create_playlist);
    let router = add(router, "updatePlaylist", update_playlist);
    add(router, "deletePlaylist", delete_playlist)
}

fn not_found() -> SubsonicError {
    SubsonicError::not_found("Playlist not found")
}

fn denied() -> SubsonicError {
    SubsonicError::not_authorized("Access denied")
}

/// The playlist behind a raw id, for writing: 70 when unknown or not a UUID, 50 when it is not
/// the caller's.
async fn owned(
    ctx: &SubsonicCtx,
    user: &SubsonicUser,
    raw: &str,
) -> Result<Playlist, SubsonicError> {
    let id = ids::parse(raw, &[]).ok_or_else(not_found)?;
    let playlist = playlists::find(&ctx.app.pool, id)
        .await?
        .ok_or_else(not_found)?;
    if playlist.user_id != user.user.id {
        return Err(denied());
    }
    Ok(playlist)
}

/// The playlist with its `entry` songs, as `getPlaylist` and `createPlaylist` answer.
async fn full(ctx: &SubsonicCtx, user: &SubsonicUser, id: Uuid) -> Result<Answer, SubsonicError> {
    let pool = &ctx.app.pool;
    let summary = playlists::summary(pool, id, user.user.id)
        .await?
        .ok_or_else(not_found)?;
    let items = playlists::items(pool, id).await?;
    let duration = items.iter().map(|i| i64::from(i.track.duration_secs)).sum();
    let track_ids: Vec<Uuid> = items.iter().map(|i| i.track.id).collect();
    let songs = subsonic::tracks_by_ids(pool, user.user.id, &track_ids).await?;
    let entries: Vec<Node> = songs
        .iter()
        .map(|s| map::song(s).renamed("entry"))
        .collect();
    // An empty playlist has no `entry`, as upstream.
    let node = map::playlist(&summary, duration);
    Ok(Answer::node(if entries.is_empty() {
        node
    } else {
        node.list("entry", entries)
    }))
}

async fn get_playlists(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    _params: Params,
) -> Result<Answer, SubsonicError> {
    let pool = &ctx.app.pool;
    let all: Vec<PlaylistSummary> = playlists::list(pool, user.user.id, i64::MAX, 0)
        .await?
        .into_iter()
        .filter(|p| !p.hidden)
        .collect();
    let ids: Vec<Uuid> = all.iter().map(|p| p.playlist.id).collect();
    let durations = playlists::durations(pool, &ids).await?;
    let nodes = all
        .iter()
        .map(|p| map::playlist(p, durations.get(&p.playlist.id).copied().unwrap_or(0)))
        .collect();
    Ok(Answer::node(Node::new("playlists").list("playlist", nodes)))
}

async fn get_playlist(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let id = ids::parse(raw, &[]).ok_or_else(not_found)?;
    full(&ctx, &user, id).await
}

/// Every supplied song id must parse; missing library songs are checked inside the DB transaction.
fn song_ids(params: &Params, key: &str) -> Result<Vec<Uuid>, SubsonicError> {
    params
        .all(key)
        .iter()
        .map(|raw| ids::parse(raw, &[]).ok_or_else(|| SubsonicError::not_found("Song not found")))
        .collect()
}

fn announce(ctx: &SubsonicCtx, who: Uuid, playlist: Uuid, deleted: bool) {
    ctx.app.events.publish(Event {
        user_id: Some(who),
        data: json!({"type": "playlist:updated", "playlistId": playlist, "deleted": deleted}),
    });
}

fn write_result(result: SubsonicWrite) -> Result<bool, SubsonicError> {
    match result {
        SubsonicWrite::MissingPlaylist => Err(not_found()),
        SubsonicWrite::UnknownSong => Err(SubsonicError::not_found("Song not found")),
        SubsonicWrite::Unchanged => Ok(false),
        SubsonicWrite::Changed => Ok(true),
    }
}

async fn create_playlist(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let pool = &ctx.app.pool;
    let id = if let Some(raw) = params.get("playlistId").filter(|p| !p.is_empty()) {
        let playlist = owned(&ctx, &user, raw).await?;
        let songs = song_ids(&params, "songId")?;
        let changed = write_result(
            playlists::subsonic_edit(pool, playlist.id, None, None, Some(&songs), &[], &[]).await?,
        )?;
        if changed {
            announce(&ctx, user.user.id, playlist.id, false);
        }
        playlist.id
    } else if let Some(name) = params.get("name").filter(|n| !n.is_empty()) {
        let songs = song_ids(&params, "songId")?;
        let id = Uuid::now_v7();
        if !playlists::subsonic_create(pool, id, user.user.id, name, &songs).await? {
            return Err(SubsonicError::not_found("Song not found"));
        }
        announce(&ctx, user.user.id, id, false);
        id
    } else {
        return Err(SubsonicError::new(
            10,
            "Required parameter is missing: playlistId or name",
        ));
    };
    full(&ctx, &user, id).await
}

async fn update_playlist(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let pool = &ctx.app.pool;
    let playlist = owned(&ctx, &user, params.required("playlistId")?).await?;
    let name = params.get("name");
    let public = params.bool("public")?;
    // Upstream skips non-numbers and out-of-range indexes alike.
    let remove: Vec<usize> = params
        .all("songIndexToRemove")
        .iter()
        .filter_map(|v| v.trim().parse::<usize>().ok())
        .collect();
    let add = song_ids(&params, "songIdToAdd")?;
    let changed = write_result(
        playlists::subsonic_edit(pool, playlist.id, name, public, None, &remove, &add).await?,
    )?;
    if changed {
        announce(&ctx, user.user.id, playlist.id, false);
    }
    Ok(Answer::ok())
}

async fn delete_playlist(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let playlist = owned(&ctx, &user, params.required("id")?).await?;
    if playlists::delete(&ctx.app.pool, playlist.id).await? {
        announce(&ctx, user.user.id, playlist.id, true);
    }
    Ok(Answer::ok())
}
