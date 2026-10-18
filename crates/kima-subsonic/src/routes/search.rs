//! Search, random and top songs, and the genre and similar-song stubs (spec §4.3 #16-#24).

use axum::Router;
use axum::extract::State;
use kima_api::AppState;
use kima_db::subsonic::{self, AlbumItem, ArtistItem, SearchPage, SongItem};
use kima_db::{PgPool, search};
use uuid::Uuid;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::map;
use crate::params::Params;
use crate::response::{Answer, Node};

const MAX_PAGE: i64 = 500;

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "search3", search3);
    let router = add(router, "search2", search2);
    let router = add(router, "search", legacy_search);
    let router = add(router, "getRandomSongs", get_random_songs);
    let router = add(router, "getSongsByGenre", get_songs_by_genre);
    let router = add(router, "getTopSongs", get_top_songs);
    let router = add(router, "getSimilarSongs", get_similar_songs);
    let router = add(router, "getSimilarSongs2", get_similar_songs2);
    add(router, "getGenres", get_genres)
}

/// A paging count: the default when absent, then clamped to `0..=500`.
fn count(params: &Params, key: &str, default: i64) -> Result<i64, SubsonicError> {
    Ok(params
        .int::<i64>(key)?
        .unwrap_or(default)
        .clamp(0, MAX_PAGE))
}

fn offset(params: &Params, key: &str) -> Result<i64, SubsonicError> {
    Ok(params.int::<i64>(key)?.unwrap_or(0).max(0))
}

/// `""`, `''` and `""` (two double quotes) count as empty, as clients send them for "everything".
fn is_empty_query(q: &str) -> bool {
    matches!(q.trim(), "" | "\"\"" | "''")
}

#[derive(Default)]
struct Found {
    artists: Vec<ArtistItem>,
    albums: Vec<AlbumItem>,
    songs: Vec<SongItem>,
}

async fn find(
    pool: &PgPool,
    user: Uuid,
    query: &str,
    page: SearchPage,
) -> Result<Found, SubsonicError> {
    if is_empty_query(query) {
        let (artists, albums, songs) = subsonic::search_all(pool, user, page).await?;
        return Ok(Found {
            artists,
            albums,
            songs,
        });
    }
    let Some(q) = search::prefix_query(pool, query).await? else {
        return Ok(Found::default());
    };
    // The search rows carry no star or rating; the `*_by_ids` reads add the caller's.
    // A zero count runs no SQL.
    let artist_ids: Vec<Uuid> = if page.artist_count > 0 {
        search::artists(pool, &q, page.artist_count, page.artist_offset)
            .await?
            .iter()
            .map(|a| a.id)
            .collect()
    } else {
        Vec::new()
    };
    let album_ids: Vec<Uuid> = if page.album_count > 0 {
        search::albums(pool, &q, page.album_count, page.album_offset)
            .await?
            .iter()
            .map(|a| a.id)
            .collect()
    } else {
        Vec::new()
    };
    let track_ids: Vec<Uuid> = if page.song_count > 0 {
        search::tracks(pool, &q, page.song_count, page.song_offset)
            .await?
            .iter()
            .map(|t| t.id)
            .collect()
    } else {
        Vec::new()
    };
    Ok(Found {
        artists: subsonic::artists_by_ids(pool, user, &artist_ids).await?,
        albums: subsonic::albums_by_ids(pool, user, &album_ids).await?,
        songs: subsonic::tracks_by_ids(pool, user, &track_ids).await?,
    })
}

async fn search_result(
    name: &'static str,
    user: SubsonicUser,
    app: AppState,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let page = SearchPage {
        artist_count: count(&params, "artistCount", 20)?,
        artist_offset: offset(&params, "artistOffset")?,
        album_count: count(&params, "albumCount", 20)?,
        album_offset: offset(&params, "albumOffset")?,
        song_count: count(&params, "songCount", 20)?,
        song_offset: offset(&params, "songOffset")?,
    };
    let query = params.get("query").unwrap_or("");
    let found = find(&app.pool, user.user.id, query, page).await?;
    Ok(Answer::node(
        Node::new(name)
            .list(
                "artist",
                found.artists.iter().map(map::artist_id3).collect(),
            )
            .list("album", found.albums.iter().map(map::album_id3).collect())
            .list("song", found.songs.iter().map(map::song).collect()),
    ))
}

async fn search3(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    search_result("searchResult3", user, app, params).await
}

async fn search2(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    search_result("searchResult2", user, app, params).await
}

/// The legacy `search`: songs only, as `match` entries. The text comes from `query`, else `any`,
/// `title`, `album` or `artist` (D-080). One row more than `count` is fetched: `totalHits` is the
/// offset plus the page, plus one when that extra row exists (a lower bound, D-100).
async fn legacy_search(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let query = ["query", "any", "title", "album", "artist"]
        .iter()
        .filter_map(|k| params.get(k))
        .find(|v| !v.trim().is_empty())
        .unwrap_or("");
    let song_offset = offset(&params, "offset")?;
    let wanted = count(&params, "count", 20)?;
    let page = SearchPage {
        artist_count: 0,
        artist_offset: 0,
        album_count: 0,
        album_offset: 0,
        song_count: wanted + 1,
        song_offset,
    };
    let mut found = find(&app.pool, user.user.id, query, page).await?;
    let more = i64::try_from(found.songs.len()).unwrap_or(i64::MAX) > wanted;
    found
        .songs
        .truncate(usize::try_from(wanted).unwrap_or_default());
    let hits = song_offset + i64::try_from(found.songs.len()).unwrap_or(0) + i64::from(more);
    Ok(Answer::node(
        Node::new("searchResult")
            .attr("offset", song_offset)
            .attr("totalHits", hits)
            .list(
                "match",
                found
                    .songs
                    .iter()
                    .map(|s| map::song(s).renamed("match"))
                    .collect(),
            ),
    ))
}

async fn get_random_songs(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let size = params.int::<i64>("size")?.unwrap_or(10).clamp(1, MAX_PAGE);
    let from = params.int::<i32>("fromYear")?;
    let to = params.int::<i32>("toYear")?;
    let years = match (from, to) {
        (None, None) => None,
        (from, to) => Some((from.unwrap_or(i32::MIN), to.unwrap_or(i32::MAX))),
    };
    // Genres arrive in M3; until then no track has one.
    let songs = if params.get("genre").is_some_and(|g| !g.is_empty()) {
        Vec::new()
    } else {
        subsonic::random_tracks(&app.pool, user.user.id, size, years).await?
    };
    Ok(Answer::node(
        Node::new("randomSongs").list("song", songs.iter().map(map::song).collect()),
    ))
}

async fn get_songs_by_genre(_user: SubsonicUser, params: Params) -> Result<Answer, SubsonicError> {
    params.required("genre")?;
    Ok(Answer::node(
        Node::new("songsByGenre").list("song", Vec::new()),
    ))
}

async fn get_top_songs(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let artist = params.required("artist")?;
    let count = params.int::<i64>("count")?.unwrap_or(50).clamp(1, MAX_PAGE);
    let songs = subsonic::top_tracks(&app.pool, user.user.id, artist, count).await?;
    Ok(Answer::node(
        Node::new("topSongs").list("song", songs.iter().map(map::song).collect()),
    ))
}

async fn get_similar_songs(_user: SubsonicUser, params: Params) -> Result<Answer, SubsonicError> {
    params.required("id")?;
    Ok(Answer::node(
        Node::new("similarSongs").list("song", Vec::new()),
    ))
}

async fn get_similar_songs2(_user: SubsonicUser, params: Params) -> Result<Answer, SubsonicError> {
    params.required("id")?;
    Ok(Answer::node(
        Node::new("similarSongs2").list("song", Vec::new()),
    ))
}

async fn get_genres(_user: SubsonicUser) -> Result<Answer, SubsonicError> {
    Ok(Answer::node(Node::new("genres").list("genre", Vec::new())))
}
