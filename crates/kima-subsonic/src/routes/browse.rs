//! Browsing and album lists (spec §4.2): `getArtists`, `getIndexes`, `getArtist`,
//! `getMusicDirectory`, `getAlbum`, `getSong`, `getAlbumList2` and `getAlbumList`.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use kima_api::AppState;
use kima_db::subsonic::{self, AlbumItem, AlbumListKind};
use uuid::Uuid;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids::{self, Prefix};
use crate::map;
use crate::params::Params;
use crate::response::{Answer, Node};

/// The most items an album list returns.
const MAX_PAGE: i64 = 500;
const DEFAULT_PAGE: i64 = 10;

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getArtists", get_artists);
    let router = add(router, "getIndexes", get_indexes);
    let router = add(router, "getArtist", get_artist);
    let router = add(router, "getMusicDirectory", get_music_directory);
    let router = add(router, "getAlbum", get_album);
    let router = add(router, "getSong", get_song);
    let router = add(router, "getAlbumList2", get_album_list2);
    add(router, "getAlbumList", get_album_list)
}

/// The artist index, bucketed by the first letter of the sort name. Buckets come in letter
/// order (`#` first), each in sort-name order.
async fn index(
    app: &AppState,
    user: &SubsonicUser,
    name: &'static str,
) -> Result<Node, SubsonicError> {
    let artists = subsonic::artists_sorted(&app.pool, user.user.id).await?;
    let mut buckets: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for artist in &artists {
        buckets
            .entry(map::index_letter(&artist.sort_name))
            .or_default()
            .push(map::artist_id3(artist));
    }
    let indexes = buckets
        .into_iter()
        .map(|(letter, nodes)| {
            Node::new("index")
                .attr("name", letter)
                .list("artist", nodes)
        })
        .collect();
    Ok(Node::new(name)
        .attr("ignoredArticles", map::IGNORED_ARTICLES)
        .list("index", indexes))
}

async fn get_artists(
    user: SubsonicUser,
    State(app): State<AppState>,
) -> Result<Answer, SubsonicError> {
    Ok(Answer::node(index(&app, &user, "artists").await?))
}

/// The same data as `getArtists`. `ifModifiedSince` is ignored: no reliable "library changed"
/// watermark exists, so the full index is always sent, stamped with the current time (D-088).
async fn get_indexes(
    user: SubsonicUser,
    State(app): State<AppState>,
) -> Result<Answer, SubsonicError> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Ok(Answer::node(
        index(&app, &user, "indexes")
            .await?
            .attr("lastModified", now_ms),
    ))
}

async fn get_artist(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let not_found = || SubsonicError::not_found("Artist not found");
    let id = ids::parse(raw, &[]).ok_or_else(not_found)?;
    let (artist, albums) = subsonic::artist_with_albums(&app.pool, user.user.id, id)
        .await?
        .ok_or_else(not_found)?;
    let node = map::artist_id3(&artist).list("album", albums.iter().map(map::album_id3).collect());
    Ok(Answer::node(node))
}

async fn get_music_directory(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let not_found = || SubsonicError::not_found("Directory not found");
    let me = user.user.id;
    if raw == "1" {
        let artists = subsonic::artists_sorted(&app.pool, me).await?;
        return Ok(Answer::node(
            Node::new("directory")
                .attr("id", "1")
                .attr("name", "Music")
                .list("child", artists.iter().map(map::artist_dir).collect()),
        ));
    }
    if let Some(id) = ids::parse(raw, &[Prefix::Ar, Prefix::ArtistColon])
        && let Some((artist, albums)) = subsonic::artist_with_albums(&app.pool, me, id).await?
    {
        return Ok(Answer::node(
            Node::new("directory")
                .attr("id", artist.id.to_string())
                .attr("parent", "1")
                .attr("name", &artist.name)
                .list(
                    "child",
                    albums
                        .iter()
                        .map(|a| map::album_child(a).renamed("child"))
                        .collect(),
                ),
        ));
    }
    let id = ids::parse(raw, &[Prefix::Al, Prefix::AlbumColon]).ok_or_else(not_found)?;
    let (album, songs) = subsonic::album_with_tracks(&app.pool, me, id)
        .await?
        .ok_or_else(not_found)?;
    Ok(Answer::node(
        Node::new("directory")
            .attr("id", album.id.to_string())
            .attr("parent", album.artist_id.to_string())
            .attr("name", &album.title)
            .list(
                "child",
                songs
                    .iter()
                    .map(|s| map::song(s).renamed("child"))
                    .collect(),
            ),
    ))
}

async fn get_album(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let not_found = || SubsonicError::not_found("Album not found");
    let id = ids::parse(raw, &[]).ok_or_else(not_found)?;
    let (album, songs) = subsonic::album_with_tracks(&app.pool, user.user.id, id)
        .await?
        .ok_or_else(not_found)?;
    Ok(Answer::node(
        map::album_id3(&album).list("song", songs.iter().map(map::song).collect()),
    ))
}

async fn get_song(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let not_found = || SubsonicError::not_found("Song not found");
    let id: Uuid = ids::parse(raw, &[]).ok_or_else(not_found)?;
    let song = subsonic::track(&app.pool, user.user.id, id)
        .await?
        .ok_or_else(not_found)?;
    Ok(Answer::node(map::song(&song)))
}

/// The albums for `type`, `size` and `offset`. A missing or unknown `type` acts as `newest`.
async fn album_list(
    app: &AppState,
    user: &SubsonicUser,
    params: &Params,
) -> Result<Vec<AlbumItem>, SubsonicError> {
    let kind = match params.get("type").unwrap_or("newest") {
        "random" => Some(AlbumListKind::Random),
        "highest" => Some(AlbumListKind::Highest),
        "frequent" => Some(AlbumListKind::Frequent),
        "recent" => Some(AlbumListKind::Recent),
        "alphabeticalByName" => Some(AlbumListKind::AlphabeticalByName),
        "alphabeticalByArtist" => Some(AlbumListKind::AlphabeticalByArtist),
        "starred" => Some(AlbumListKind::Starred),
        "byYear" => {
            let year = |key: &str| {
                params
                    .int::<i32>(key)?
                    .ok_or_else(|| SubsonicError::missing(key))
            };
            Some(AlbumListKind::ByYear {
                from: year("fromYear")?,
                to: year("toYear")?,
            })
        }
        // Genres arrive in M3; until then no album has one.
        "byGenre" => None,
        _ => Some(AlbumListKind::Newest),
    };
    let size = params
        .int::<i64>("size")?
        .unwrap_or(DEFAULT_PAGE)
        .clamp(0, MAX_PAGE);
    let offset = params.int::<i64>("offset")?.unwrap_or(0).max(0);
    let Some(kind) = kind else {
        return Ok(Vec::new());
    };
    Ok(subsonic::album_list(&app.pool, user.user.id, kind, size, offset).await?)
}

async fn get_album_list2(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let albums = album_list(&app, &user, &params).await?;
    Ok(Answer::node(Node::new("albumList2").list(
        "album",
        albums.iter().map(map::album_id3).collect(),
    )))
}

/// The legacy folder variant: albums in the `Child` shape (D-083).
async fn get_album_list(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let albums = album_list(&app, &user, &params).await?;
    Ok(Answer::node(Node::new("albumList").list(
        "album",
        albums.iter().map(map::album_child).collect(),
    )))
}
