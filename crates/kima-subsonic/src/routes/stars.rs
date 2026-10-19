//! Stars and ratings (spec §4.7 #47-#51), on Task 9's `stars` and `ratings` layers. Songs, albums
//! and artists can all be starred and rated (D-076, D-077).

use axum::Router;
use axum::extract::State;
use kima_api::AppState;
use kima_db::stars::{self, StarKind};
use kima_db::{PgPool, ratings, subsonic};
use uuid::Uuid;

use super::add;
use crate::auth::SubsonicUser;
use crate::error::SubsonicError;
use crate::ids;
use crate::map;
use crate::params::Params;
use crate::response::{Answer, Node};

pub fn add_routes(router: Router<crate::ctx::SubsonicCtx>) -> Router<crate::ctx::SubsonicCtx> {
    let router = add(router, "star", star);
    let router = add(router, "unstar", unstar);
    let router = add(router, "getStarred", get_starred);
    let router = add(router, "getStarred2", get_starred2);
    add(router, "setRating", set_rating)
}

fn unknown() -> SubsonicError {
    SubsonicError::not_found("Item not found")
}

/// The targets named by `id` (any kind, looked up), `albumId` and `artistId` (taken as their
/// kind), in that order. A value that is not a UUID is dropped; `None` for the kind means the
/// id names nothing.
async fn targets(
    pool: &PgPool,
    params: &Params,
) -> Result<Vec<(Option<StarKind>, Uuid)>, SubsonicError> {
    let mut out = Vec::new();
    for raw in params.all("id") {
        if let Some(id) = ids::parse(raw, &[]) {
            out.push((subsonic::kind_of(pool, id).await?, id));
        }
    }
    for (key, kind) in [("albumId", StarKind::Album), ("artistId", StarKind::Artist)] {
        for raw in params.all(key) {
            if let Some(id) = ids::parse(raw, &[]) {
                out.push((Some(kind), id));
            }
        }
    }
    Ok(out)
}

fn sent(params: &Params) -> bool {
    ["id", "albumId", "artistId"]
        .iter()
        .any(|k| params.all(k).iter().any(|v| !v.is_empty()))
}

/// Ok when at least one id was valid or none were sent; 70 when every id sent was unknown.
async fn star(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let mut valid = 0usize;
    for (kind, id) in targets(&app.pool, &params).await? {
        // `set` answers false for a target that does not exist, so an `albumId` that names a
        // song is skipped just as an unknown id is.
        if let Some(kind) = kind
            && stars::set(&app.pool, user.user.id, kind, id).await?
        {
            valid += 1;
        }
    }
    if valid == 0 && sent(&params) {
        return Err(unknown());
    }
    Ok(Answer::ok())
}

async fn unstar(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    for (kind, id) in targets(&app.pool, &params).await? {
        if let Some(kind) = kind {
            stars::clear(&app.pool, user.user.id, kind, id).await?;
        }
    }
    Ok(Answer::ok())
}

struct Starred {
    artists: Vec<subsonic::ArtistItem>,
    albums: Vec<subsonic::AlbumItem>,
    songs: Vec<subsonic::SongItem>,
}

async fn starred(pool: &PgPool, me: Uuid) -> Result<Starred, SubsonicError> {
    let artist_ids = stars::list_for_user(pool, me, StarKind::Artist).await?;
    let album_ids = stars::list_for_user(pool, me, StarKind::Album).await?;
    let song_ids = stars::list_for_user(pool, me, StarKind::Track).await?;
    Ok(Starred {
        artists: subsonic::artists_by_ids(pool, me, &artist_ids).await?,
        albums: subsonic::albums_by_ids(pool, me, &album_ids).await?,
        songs: subsonic::tracks_by_ids(pool, me, &song_ids).await?,
    })
}

async fn get_starred(
    user: SubsonicUser,
    State(app): State<AppState>,
    _params: Params,
) -> Result<Answer, SubsonicError> {
    let s = starred(&app.pool, user.user.id).await?;
    Ok(Answer::node(
        Node::new("starred")
            .list("artist", s.artists.iter().map(map::artist_id3).collect())
            .list("album", s.albums.iter().map(map::album_child).collect())
            .list("song", s.songs.iter().map(map::song).collect()),
    ))
}

async fn get_starred2(
    user: SubsonicUser,
    State(app): State<AppState>,
    _params: Params,
) -> Result<Answer, SubsonicError> {
    let s = starred(&app.pool, user.user.id).await?;
    Ok(Answer::node(
        Node::new("starred2")
            .list("artist", s.artists.iter().map(map::artist_id3).collect())
            .list("album", s.albums.iter().map(map::album_id3).collect())
            .list("song", s.songs.iter().map(map::song).collect()),
    ))
}

async fn set_rating(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let rating: i64 = params
        .int("rating")?
        .ok_or_else(|| SubsonicError::missing("rating"))?;
    if !(0..=5).contains(&rating) {
        return Err(SubsonicError::generic("Rating must be between 0 and 5"));
    }
    let id = ids::parse(raw, &[]).ok_or_else(unknown)?;
    let kind = subsonic::kind_of(&app.pool, id)
        .await?
        .ok_or_else(unknown)?;
    if rating == 0 {
        ratings::clear(&app.pool, user.user.id, kind, id).await?;
    } else if !ratings::set(&app.pool, user.user.id, kind, id, rating as i16).await? {
        return Err(unknown());
    }
    Ok(Answer::ok())
}
