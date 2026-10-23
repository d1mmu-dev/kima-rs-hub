//! Read models for the Subsonic endpoints (spec §7.5). Each carries the caller's star time and
//! rating, joined in the same query. Tracks flagged missing are excluded everywhere, and so are
//! albums and artists left with no present track. Orders end in the id, so paging is stable.

use std::collections::HashMap;

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::browse::TrackRow;
use crate::genres;
use crate::stars::StarKind;

#[derive(Debug, Clone, FromRow)]
pub struct ArtistItem {
    pub id: Uuid,
    pub name: String,
    pub sort_name: String,
    pub album_count: i64,
    pub starred_at: Option<OffsetDateTime>,
    pub rating: Option<i16>,
    pub mbid: Option<Uuid>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AlbumItem {
    pub id: Uuid,
    pub title: String,
    pub year: Option<i32>,
    pub created_at: OffsetDateTime,
    pub artist_id: Uuid,
    pub artist_name: String,
    pub song_count: i64,
    pub duration_secs: i64,
    pub starred_at: Option<OffsetDateTime>,
    pub rating: Option<i16>,
    pub release_mbid: Option<Uuid>,
    /// The album's first effective genre, else the album artist's.
    pub genre: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct SongItem {
    #[sqlx(flatten)]
    pub track: TrackRow,
    /// Library-relative path. Only the suffix and the download name come from it; never sent.
    pub path: String,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub starred_at: Option<OffsetDateTime>,
    pub rating: Option<i16>,
    /// The track's first effective genre, else the album artist's. (The recording id is
    /// `track.recording_mbid`.)
    pub genre: Option<String>,
}

/// `$1` is the calling user in every query below.
const ARTIST_SELECT: &str = "SELECT ar.id, ar.name, ar.sort_name,
        count(DISTINCT al.id) AS album_count, s.starred_at, r.rating, ar.mbid
    FROM artists ar
    JOIN albums al ON al.artist_id = ar.id
    JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
    LEFT JOIN stars s ON s.user_id = $1 AND s.artist_id = ar.id
    LEFT JOIN ratings r ON r.user_id = $1 AND r.artist_id = ar.id";
const ARTIST_GROUP: &str = "GROUP BY ar.id, s.starred_at, r.rating";

const ALBUM_SELECT: &str = "SELECT al.id, al.title, al.year, al.created_at,
        ar.id AS artist_id, ar.name AS artist_name,
        count(t.id) AS song_count, COALESCE(sum(t.duration_secs), 0)::bigint AS duration_secs,
        s.starred_at, r.rating, al.release_mbid,
        COALESCE(
            (SELECT g.name FROM album_genres_effective e JOIN genres g ON g.id = e.genre_id
             WHERE e.album_id = al.id ORDER BY e.rank, g.name, g.id LIMIT 1),
            (SELECT g.name FROM artist_genres_effective e JOIN genres g ON g.id = e.genre_id
             WHERE e.artist_id = ar.id ORDER BY e.rank, g.name, g.id LIMIT 1)) AS genre
    FROM albums al
    JOIN artists ar ON ar.id = al.artist_id
    JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
    LEFT JOIN stars s ON s.user_id = $1 AND s.album_id = al.id
    LEFT JOIN ratings r ON r.user_id = $1 AND r.album_id = al.id";
const ALBUM_GROUP: &str = "GROUP BY al.id, ar.id, s.starred_at, r.rating";

const SONG_SELECT: &str = "SELECT t.id, t.title, t.artist_name, t.duration_secs, t.disc, t.track_no,
        t.codec, t.bitrate, t.year, al.id AS album_id, al.title AS album_title, al.year AS album_year,
        ar.id AS album_artist_id, ar.name AS album_artist_name, t.recording_mbid, t.isrc,
        t.path, t.size, t.created_at, s.starred_at, r.rating,
        COALESCE(
            (SELECT g.name FROM track_genres_effective e JOIN genres g ON g.id = e.genre_id
             WHERE e.track_id = t.id ORDER BY e.rank, g.name, g.id LIMIT 1),
            (SELECT g.name FROM artist_genres_effective e JOIN genres g ON g.id = e.genre_id
             WHERE e.artist_id = ar.id ORDER BY e.rank, g.name, g.id LIMIT 1)) AS genre
    FROM tracks t
    JOIN albums al ON al.id = t.album_id
    JOIN artists ar ON ar.id = al.artist_id
    LEFT JOIN stars s ON s.user_id = $1 AND s.track_id = t.id
    LEFT JOIN ratings r ON r.user_id = $1 AND r.track_id = t.id";

/// Artists with a present track, by sort name.
pub async fn artists_sorted(pool: &PgPool, user: Uuid) -> sqlx::Result<Vec<ArtistItem>> {
    let sql = format!("{ARTIST_SELECT} {ARTIST_GROUP} ORDER BY ar.sort_name ASC, ar.id ASC");
    sqlx::query_as(&sql).bind(user).fetch_all(pool).await
}

/// The artist and their albums (newest year first, undated last). `None` for an unknown artist
/// or one with no present track.
pub async fn artist_with_albums(
    pool: &PgPool,
    user: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<(ArtistItem, Vec<AlbumItem>)>> {
    let sql = format!("{ARTIST_SELECT} WHERE ar.id = $2 {ARTIST_GROUP}");
    let Some(artist) = sqlx::query_as::<_, ArtistItem>(&sql)
        .bind(user)
        .bind(id)
        .fetch_optional(pool)
        .await?
    else {
        return Ok(None);
    };
    let sql = format!(
        "{ALBUM_SELECT} WHERE al.artist_id = $2 {ALBUM_GROUP}
         ORDER BY al.year DESC NULLS LAST, al.sort_name ASC, al.id ASC"
    );
    let albums = sqlx::query_as(&sql)
        .bind(user)
        .bind(id)
        .fetch_all(pool)
        .await?;
    Ok(Some((artist, albums)))
}

/// The album and its present tracks by disc, track number, title. `None` for an unknown album
/// or one with no present track.
pub async fn album_with_tracks(
    pool: &PgPool,
    user: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<(AlbumItem, Vec<SongItem>)>> {
    let sql = format!("{ALBUM_SELECT} WHERE al.id = $2 {ALBUM_GROUP}");
    let Some(album) = sqlx::query_as::<_, AlbumItem>(&sql)
        .bind(user)
        .bind(id)
        .fetch_optional(pool)
        .await?
    else {
        return Ok(None);
    };
    let sql = format!(
        "{SONG_SELECT} WHERE t.missing_since IS NULL AND t.album_id = $2
         ORDER BY t.disc ASC, t.track_no ASC, lower(t.title) ASC, t.id ASC"
    );
    let tracks = sqlx::query_as(&sql)
        .bind(user)
        .bind(id)
        .fetch_all(pool)
        .await?;
    Ok(Some((album, tracks)))
}

pub async fn track(pool: &PgPool, user: Uuid, id: Uuid) -> sqlx::Result<Option<SongItem>> {
    let sql = format!("{SONG_SELECT} WHERE t.missing_since IS NULL AND t.id = $2");
    sqlx::query_as(&sql)
        .bind(user)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// `AND`-able condition: the entity (`$col`) has the genre whose key is bound at `$n`, among its
/// effective genres of `$view`.
fn has_genre(view: &str, col: &str, entity: &str, n: u8) -> String {
    format!(
        "EXISTS (SELECT 1 FROM {view} e JOIN genres g ON g.id = e.genre_id
                 WHERE e.{col} = {entity} AND g.key = ${n})"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlbumListKind {
    Random,
    Newest,
    /// The caller's rated albums, best rating first.
    Highest,
    /// Albums the caller played, most plays first.
    Frequent,
    /// Albums the caller played, latest play first.
    Recent,
    AlphabeticalByName,
    AlphabeticalByArtist,
    /// The caller's starred albums, newest star first.
    Starred,
    /// Albums dated between the years, in the direction given (`from > to` is descending).
    ByYear {
        from: i32,
        to: i32,
    },
    /// Albums with the genre among their effective genres (case-insensitive exact), by sort name.
    ByGenre(String),
}

/// A page of albums for `getAlbumList2`.
pub async fn album_list(
    pool: &PgPool,
    user: Uuid,
    kind: AlbumListKind,
    size: i64,
    offset: i64,
) -> sqlx::Result<Vec<AlbumItem>> {
    const PLAYED: &str =
        "JOIN (SELECT pt.album_id, count(*) AS plays, max(p.played_at) AS last_played
            FROM plays p JOIN tracks pt ON pt.id = p.track_id AND pt.missing_since IS NULL
            WHERE p.user_id = $1 GROUP BY pt.album_id) pc ON pc.album_id = al.id";
    let genre = match &kind {
        AlbumListKind::ByGenre(name) => Some(genres::genre_key(name)),
        _ => None,
    };
    // (extra join, where, extra group-by columns, order)
    let (join, filter, group, order) = match kind {
        AlbumListKind::Random => (String::new(), String::new(), "", "random()".to_owned()),
        AlbumListKind::Newest => (
            String::new(),
            String::new(),
            "",
            "al.created_at DESC, al.id ASC".to_owned(),
        ),
        AlbumListKind::Highest => (
            String::new(),
            "WHERE r.rating IS NOT NULL".to_owned(),
            "",
            "r.rating DESC, al.sort_name ASC, al.id ASC".to_owned(),
        ),
        AlbumListKind::Frequent => (
            PLAYED.to_owned(),
            String::new(),
            ", pc.plays, pc.last_played",
            "pc.plays DESC, pc.last_played DESC, al.id ASC".to_owned(),
        ),
        AlbumListKind::Recent => (
            PLAYED.to_owned(),
            String::new(),
            ", pc.plays, pc.last_played",
            "pc.last_played DESC, al.id ASC".to_owned(),
        ),
        AlbumListKind::AlphabeticalByName => (
            String::new(),
            String::new(),
            "",
            "al.sort_name ASC, al.id ASC".to_owned(),
        ),
        AlbumListKind::AlphabeticalByArtist => (
            String::new(),
            String::new(),
            "",
            "ar.sort_name ASC, al.sort_name ASC, al.id ASC".to_owned(),
        ),
        AlbumListKind::Starred => (
            String::new(),
            "WHERE s.starred_at IS NOT NULL".to_owned(),
            "",
            "s.starred_at DESC, al.id ASC".to_owned(),
        ),
        AlbumListKind::ByYear { from, to } => {
            let (lo, hi) = (from.min(to), from.max(to));
            let dir = if from > to { "DESC" } else { "ASC" };
            (
                String::new(),
                // Integers formatted in, so there is nothing to escape.
                format!("WHERE al.year BETWEEN {lo} AND {hi}"),
                "",
                format!("al.year {dir}, al.sort_name ASC, al.id ASC"),
            )
        }
        AlbumListKind::ByGenre(_) => (
            String::new(),
            format!(
                "WHERE {}",
                has_genre("album_genres_effective", "album_id", "al.id", 4)
            ),
            "",
            "al.sort_name ASC, al.id ASC".to_owned(),
        ),
    };
    // The joins have to come before WHERE, and ALBUM_SELECT ends in its last LEFT JOIN.
    let sql = format!(
        "{ALBUM_SELECT} {join} {filter} {ALBUM_GROUP}{group} ORDER BY {order} LIMIT $2 OFFSET $3"
    );
    let query = sqlx::query_as(&sql).bind(user).bind(size).bind(offset);
    match genre {
        Some(key) => query.bind(key).fetch_all(pool).await,
        None => query.fetch_all(pool).await,
    }
}

/// Up to `size` random present tracks, optionally only those dated within the years
/// (`coalesce(track year, album year)`) and those with the genre among their effective track
/// genres (case-insensitive exact).
pub async fn random_tracks(
    pool: &PgPool,
    user: Uuid,
    size: i64,
    years: Option<(i32, i32)>,
    genre: Option<&str>,
) -> sqlx::Result<Vec<SongItem>> {
    let years = years
        .map(|(a, b)| {
            format!(
                "AND coalesce(t.year, al.year) BETWEEN {} AND {}",
                a.min(b),
                a.max(b)
            )
        })
        .unwrap_or_default();
    let by_genre = genre
        .map(|_| {
            format!(
                "AND {}",
                has_genre("track_genres_effective", "track_id", "t.id", 3)
            )
        })
        .unwrap_or_default();
    let sql = format!(
        "{SONG_SELECT} WHERE t.missing_since IS NULL {years} {by_genre} ORDER BY random() LIMIT $2"
    );
    let query = sqlx::query_as(&sql).bind(user).bind(size);
    match genre {
        Some(name) => query.bind(genres::genre_key(name)).fetch_all(pool).await,
        None => query.fetch_all(pool).await,
    }
}

/// A page of present tracks with the genre among their effective track genres
/// (case-insensitive exact on `genres.key`), by lower-cased title, title, id.
pub async fn songs_by_genre(
    pool: &PgPool,
    user: Uuid,
    genre: &str,
    count: i64,
    offset: i64,
) -> sqlx::Result<Vec<SongItem>> {
    let sql = format!(
        "{SONG_SELECT} WHERE t.missing_since IS NULL AND {}
         ORDER BY lower(t.title) ASC, t.title ASC, t.id ASC LIMIT $3 OFFSET $4",
        has_genre("track_genres_effective", "track_id", "t.id", 2)
    );
    sqlx::query_as(&sql)
        .bind(user)
        .bind(genres::genre_key(genre))
        .bind(count)
        .bind(offset)
        .fetch_all(pool)
        .await
}

/// Every genre with a present track among its effective track genres, with the distinct present
/// tracks and the distinct albums (with a present track) that carry it among their effective
/// genres, by name.
pub async fn genre_counts(pool: &PgPool) -> sqlx::Result<Vec<(String, i64, i64)>> {
    let rows = sqlx::query!(
        r#"SELECT g.name,
               (SELECT count(DISTINCT t.id) FROM track_genres_effective e
                 JOIN tracks t ON t.id = e.track_id AND t.missing_since IS NULL
                 WHERE e.genre_id = g.id) AS "songs!",
               (SELECT count(DISTINCT e.album_id) FROM album_genres_effective e
                 WHERE e.genre_id = g.id
                   AND EXISTS (SELECT 1 FROM tracks t
                               WHERE t.album_id = e.album_id AND t.missing_since IS NULL)) AS "albums!"
           FROM genres g
           WHERE EXISTS (SELECT 1 FROM track_genres_effective e
                         JOIN tracks t ON t.id = e.track_id AND t.missing_since IS NULL
                         WHERE e.genre_id = g.id)
           ORDER BY lower(g.name), g.name, g.id"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.name, r.songs, r.albums))
        .collect())
}

/// The artist's songs, most played first (plays of all users), then by title. The name is matched
/// case-insensitively against the album artist or the track artist: exact matches when there
/// are any, else substring matches.
pub async fn top_tracks(
    pool: &PgPool,
    user: Uuid,
    artist: &str,
    count: i64,
) -> sqlx::Result<Vec<SongItem>> {
    let tail = "ORDER BY COALESCE(pc.plays, 0) DESC, lower(t.title) ASC, t.id ASC LIMIT $3";
    let base = format!(
        "{SONG_SELECT} LEFT JOIN (SELECT track_id, count(*) AS plays FROM plays GROUP BY track_id) pc
         ON pc.track_id = t.id WHERE t.missing_since IS NULL AND "
    );
    let exact =
        format!("{base} (lower(ar.name) = lower($2) OR lower(t.artist_name) = lower($2)) {tail}");
    let songs: Vec<SongItem> = sqlx::query_as(&exact)
        .bind(user)
        .bind(artist)
        .bind(count)
        .fetch_all(pool)
        .await?;
    if !songs.is_empty() {
        return Ok(songs);
    }
    let fuzzy = format!("{base} (ar.name ILIKE $2 OR t.artist_name ILIKE $2) {tail}");
    sqlx::query_as(&fuzzy)
        .bind(user)
        .bind(crate::browse::like_pattern(artist))
        .bind(count)
        .fetch_all(pool)
        .await
}

/// Present tracks for the ids, in the order given; unknown and missing ones are dropped.
pub async fn tracks_by_ids(pool: &PgPool, user: Uuid, ids: &[Uuid]) -> sqlx::Result<Vec<SongItem>> {
    let sql = format!(
        "{SONG_SELECT} JOIN unnest($2::uuid[]) WITH ORDINALITY u(id, ord) ON u.id = t.id
         WHERE t.missing_since IS NULL ORDER BY u.ord ASC"
    );
    sqlx::query_as(&sql)
        .bind(user)
        .bind(ids)
        .fetch_all(pool)
        .await
}

pub async fn albums_by_ids(
    pool: &PgPool,
    user: Uuid,
    ids: &[Uuid],
) -> sqlx::Result<Vec<AlbumItem>> {
    let sql = format!(
        "{ALBUM_SELECT} JOIN unnest($2::uuid[]) WITH ORDINALITY u(id, ord) ON u.id = al.id
         {ALBUM_GROUP}, u.ord ORDER BY u.ord ASC"
    );
    sqlx::query_as(&sql)
        .bind(user)
        .bind(ids)
        .fetch_all(pool)
        .await
}

pub async fn artists_by_ids(
    pool: &PgPool,
    user: Uuid,
    ids: &[Uuid],
) -> sqlx::Result<Vec<ArtistItem>> {
    let sql = format!(
        "{ARTIST_SELECT} JOIN unnest($2::uuid[]) WITH ORDINALITY u(id, ord) ON u.id = ar.id
         {ARTIST_GROUP}, u.ord ORDER BY u.ord ASC"
    );
    sqlx::query_as(&sql)
        .bind(user)
        .bind(ids)
        .fetch_all(pool)
        .await
}

/// Counts and offsets for `search_all`, one pair per result type.
#[derive(Debug, Clone, Copy)]
pub struct SearchPage {
    pub artist_count: i64,
    pub artist_offset: i64,
    pub album_count: i64,
    pub album_offset: i64,
    pub song_count: i64,
    pub song_offset: i64,
}

/// The whole library, paged per type: artists and albums by sort name, songs by title, ties by
/// id. This is what `search3` with an empty query returns.
pub async fn search_all(
    pool: &PgPool,
    user: Uuid,
    page: SearchPage,
) -> sqlx::Result<(Vec<ArtistItem>, Vec<AlbumItem>, Vec<SongItem>)> {
    // A zero count runs no SQL: the aggregates would otherwise be built and sorted for nothing.
    let artists = if page.artist_count > 0 {
        sqlx::query_as(&format!(
            "{ARTIST_SELECT} {ARTIST_GROUP} ORDER BY ar.sort_name ASC, ar.id ASC LIMIT $2 OFFSET $3"
        ))
        .bind(user)
        .bind(page.artist_count)
        .bind(page.artist_offset)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };
    let albums = if page.album_count > 0 {
        sqlx::query_as(&format!(
            "{ALBUM_SELECT} {ALBUM_GROUP} ORDER BY al.sort_name ASC, al.id ASC LIMIT $2 OFFSET $3"
        ))
        .bind(user)
        .bind(page.album_count)
        .bind(page.album_offset)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };
    let songs = if page.song_count > 0 {
        sqlx::query_as(&format!(
            "{SONG_SELECT} WHERE t.missing_since IS NULL ORDER BY lower(t.title) ASC, t.id ASC
             LIMIT $2 OFFSET $3"
        ))
        .bind(user)
        .bind(page.song_count)
        .bind(page.song_offset)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };
    Ok((artists, albums, songs))
}

/// What an id refers to, trying tracks, then albums, then artists. Missing tracks still count:
/// a client can star something the next scan flags as gone.
pub async fn kind_of(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<StarKind>> {
    let kind: Option<String> = sqlx::query_scalar(
        "SELECT CASE
            WHEN EXISTS (SELECT 1 FROM tracks WHERE id = $1) THEN 'track'
            WHEN EXISTS (SELECT 1 FROM albums WHERE id = $1) THEN 'album'
            WHEN EXISTS (SELECT 1 FROM artists WHERE id = $1) THEN 'artist'
         END",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(kind.and_then(|k| match k.as_str() {
        "track" => Some(StarKind::Track),
        "album" => Some(StarKind::Album),
        "artist" => Some(StarKind::Artist),
        _ => None,
    }))
}

/// What each id refers to, in one query over tracks, albums and artists. Ids that name nothing
/// are absent from the map. Like `kind_of`, a missing track still counts.
pub async fn kinds_of(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, StarKind>> {
    let rows = sqlx::query!(
        r#"SELECT id AS "id!", kind AS "kind!" FROM (
               SELECT id, 1 AS kind FROM tracks WHERE id = ANY($1)
               UNION ALL SELECT id, 2 FROM albums WHERE id = ANY($1)
               UNION ALL SELECT id, 3 FROM artists WHERE id = ANY($1)
           ) found ORDER BY kind DESC"#,
        ids
    )
    .fetch_all(pool)
    .await?;
    // Ordered last-priority first, so a track overwrites an album overwrites an artist.
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let kind = match r.kind {
                1 => StarKind::Track,
                2 => StarKind::Album,
                3 => StarKind::Artist,
                _ => return None,
            };
            Some((r.id, kind))
        })
        .collect())
}

/// Whether the artist is visible: it has at least one present track (the rule of
/// `artist_with_albums`).
pub async fn artist_exists(pool: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    let found = sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM albums al
               JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
               WHERE al.artist_id = $1
           ) AS "found!""#,
        id
    )
    .fetch_one(pool)
    .await?;
    Ok(found)
}

/// What the web players are playing: users whose `playback_state` is of type `track`, was
/// updated since `since`, and names a track that is still present. Newest first, at most 50.
/// Returns user, username, track and `updated_at`.
pub async fn web_now_playing(
    pool: &PgPool,
    since: OffsetDateTime,
) -> sqlx::Result<Vec<(Uuid, String, Uuid, OffsetDateTime)>> {
    sqlx::query_as(
        "SELECT ps.user_id, u.username, t.id, ps.updated_at
         FROM playback_state ps
         JOIN users u ON u.id = ps.user_id
         JOIN tracks t ON t.missing_since IS NULL AND t.id = CASE
             WHEN ps.track_id ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
             THEN ps.track_id::uuid END
         WHERE ps.playback_type = 'track' AND ps.updated_at >= $1
         ORDER BY ps.updated_at DESC, ps.user_id LIMIT 50",
    )
    .bind(since)
    .fetch_all(pool)
    .await
}
