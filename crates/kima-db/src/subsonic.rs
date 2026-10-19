//! Read models for the Subsonic endpoints (spec §7.5). Each carries the caller's star time and
//! rating, joined in the same query. Tracks flagged missing are excluded everywhere, and so are
//! albums and artists left with no present track. Orders end in the id, so paging is stable.

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::browse::TrackRow;
use crate::stars::StarKind;

#[derive(Debug, Clone, FromRow)]
pub struct ArtistItem {
    pub id: Uuid,
    pub name: String,
    pub sort_name: String,
    pub album_count: i64,
    pub starred_at: Option<OffsetDateTime>,
    pub rating: Option<i16>,
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
}

/// `$1` is the calling user in every query below.
const ARTIST_SELECT: &str = "SELECT ar.id, ar.name, ar.sort_name,
        count(DISTINCT al.id) AS album_count, s.starred_at, r.rating
    FROM artists ar
    JOIN albums al ON al.artist_id = ar.id
    JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
    LEFT JOIN stars s ON s.user_id = $1 AND s.artist_id = ar.id
    LEFT JOIN ratings r ON r.user_id = $1 AND r.artist_id = ar.id";
const ARTIST_GROUP: &str = "GROUP BY ar.id, s.starred_at, r.rating";

const ALBUM_SELECT: &str = "SELECT al.id, al.title, al.year, al.created_at,
        ar.id AS artist_id, ar.name AS artist_name,
        count(t.id) AS song_count, COALESCE(sum(t.duration_secs), 0)::bigint AS duration_secs,
        s.starred_at, r.rating
    FROM albums al
    JOIN artists ar ON ar.id = al.artist_id
    JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
    LEFT JOIN stars s ON s.user_id = $1 AND s.album_id = al.id
    LEFT JOIN ratings r ON r.user_id = $1 AND r.album_id = al.id";
const ALBUM_GROUP: &str = "GROUP BY al.id, ar.id, s.starred_at, r.rating";

const SONG_SELECT: &str = "SELECT t.id, t.title, t.artist_name, t.duration_secs, t.disc, t.track_no,
        t.codec, t.bitrate, t.year, al.id AS album_id, al.title AS album_title, al.year AS album_year,
        ar.id AS album_artist_id, ar.name AS album_artist_name,
        t.path, t.size, t.created_at, s.starred_at, r.rating
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    };
    // The joins have to come before WHERE, and ALBUM_SELECT ends in its last LEFT JOIN.
    let sql = format!(
        "{ALBUM_SELECT} {join} {filter} {ALBUM_GROUP}{group} ORDER BY {order} LIMIT $2 OFFSET $3"
    );
    sqlx::query_as(&sql)
        .bind(user)
        .bind(size)
        .bind(offset)
        .fetch_all(pool)
        .await
}

/// Up to `size` random present tracks, optionally only those dated within the years
/// (`coalesce(track year, album year)`).
pub async fn random_tracks(
    pool: &PgPool,
    user: Uuid,
    size: i64,
    years: Option<(i32, i32)>,
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
    let sql =
        format!("{SONG_SELECT} WHERE t.missing_since IS NULL {years} ORDER BY random() LIMIT $2");
    sqlx::query_as(&sql)
        .bind(user)
        .bind(size)
        .fetch_all(pool)
        .await
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
