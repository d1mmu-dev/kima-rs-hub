//! Bookmarks (spec §4.5 #39-#41), on Task 9's `bookmarks` layer. A bookmark of a track flagged
//! missing is kept and left out of the list.

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use kima_api::AppState;
use kima_db::{bookmarks, subsonic};

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids;
use crate::map;
use crate::params::Params;
use crate::response::{Answer, Node};

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getBookmarks", get_bookmarks);
    let router = add(router, "createBookmark", create_bookmark);
    add(router, "deleteBookmark", delete_bookmark)
}

fn unknown() -> SubsonicError {
    SubsonicError::not_found("Song not found")
}

async fn get_bookmarks(
    user: SubsonicUser,
    State(app): State<AppState>,
    _params: Params,
) -> Result<Answer, SubsonicError> {
    let me = user.user.id;
    let list = bookmarks::list_for_user(&app.pool, me).await?;
    let track_ids: Vec<_> = list.iter().map(|b| b.track_id).collect();
    let songs: HashMap<_, _> = subsonic::tracks_by_ids(&app.pool, me, &track_ids)
        .await?
        .into_iter()
        .map(|s| (s.track.id, s))
        .collect();
    let nodes = list
        .iter()
        .filter_map(|b| {
            // A track purged since the list was read has no song; its bookmark is gone too.
            let song = songs.get(&b.track_id)?;
            Some(
                Node::new("bookmark")
                    .attr("position", b.position_ms)
                    .attr("username", &user.user.username)
                    .attr_opt("comment", b.comment.as_deref())
                    .attr("created", map::iso(b.created_at))
                    .attr("changed", map::iso(b.changed_at))
                    .child(map::song(song).renamed("entry")),
            )
        })
        .collect();
    Ok(Answer::node(Node::new("bookmarks").list("bookmark", nodes)))
}

async fn create_bookmark(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let position: i64 = params
        .int("position")?
        .ok_or_else(|| SubsonicError::missing("position"))?;
    if position < 0 {
        return Err(SubsonicError::invalid("position"));
    }
    let track = ids::parse(raw, &[]).ok_or_else(unknown)?;
    let comment = params.get("comment").filter(|c| !c.is_empty());
    if !bookmarks::upsert(&app.pool, user.user.id, track, position, comment).await? {
        return Err(unknown());
    }
    Ok(Answer::ok())
}

async fn delete_bookmark(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let track = ids::parse(params.required("id")?, &[]).ok_or_else(unknown)?;
    bookmarks::delete(&app.pool, user.user.id, track).await?;
    Ok(Answer::ok())
}
