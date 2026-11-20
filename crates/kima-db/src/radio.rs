//! The radio's library lists (M5b Task 11; upstream `services/mixes/radioRow.ts`) and the song selection of
//! `GET /library/radio` (M5b Task 13; upstream `services/mixes/stationSelection.ts` and `services/mixes/helpers.ts`
//! `sampleTrackIds`/`unplayedTrackIds`). Upstream's in-process uniform draws (`drawIds`, `shuffleArray`) over every
//! matching id are drawn here in the database (`ORDER BY random() LIMIT n`): the same uniform draw without moving
//! every id to the server. A playable song is a present one (`LIBRARY_PLAYABLE`, D-261).

use std::collections::{HashMap, HashSet};

use kima_core::profile::ProfileId;
use kima_mixes::radio::Sound;
use sqlx::PgPool;
use uuid::Uuid;

use crate::genres::{effective_for_artists, genre_key, merged_for_display, upstream_genre_key};
use crate::stats::listens::{Period, listen_rows};
use crate::vibe::ELIGIBLE;

/// A genre or decade becomes a station once it has this many library songs (upstream `MIN_STATION_SONGS`).
pub const MIN_STATION_SONGS: i64 = 15;

/// One decade of `GET /library/decades`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct Decade {
    pub decade: i32,
    pub count: i64,
}

/// Upstream `libraryDecades`: each album's songs count in the decade of its effective year (`getEffectiveYear`:
/// display year, else original year, else year; kima keeps no original year, D-272), a decade needs
/// [`MIN_STATION_SONGS`] songs, newest first. Songs are present tracks (D-276); an album with no year has no decade.
pub async fn library_decades(pool: &PgPool) -> sqlx::Result<Vec<Decade>> {
    sqlx::query_as(
        "SELECT (COALESCE(b.display_year, b.year) / 10 * 10)::int4 AS decade, count(t.id) AS count
         FROM albums b
         JOIN tracks t ON t.album_id = b.id AND t.missing_since IS NULL
         WHERE COALESCE(b.display_year, b.year) IS NOT NULL
         GROUP BY 1
         HAVING count(t.id) >= $1
         ORDER BY 1 DESC",
    )
    .bind(MIN_STATION_SONGS)
    .fetch_all(pool)
    .await
}

/// Upstream `stillPlayable`: the ids of these songs that can still play (present).
pub async fn playable_ids(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<HashSet<Uuid>> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM tracks WHERE id = ANY($1) AND missing_since IS NULL")
            .bind(ids)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

/// Upstream `sampleTrackIds({where: {}, take})`: up to `take` present songs drawn uniformly.
pub async fn sample_track_ids(pool: &PgPool, take: usize) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM tracks WHERE missing_since IS NULL ORDER BY random() LIMIT $1",
    )
    .bind(take as i64)
    .fetch_all(pool)
    .await
}

/// Upstream's decade radio (`getEffectiveDecadeWhereClause`): up to `take` present songs whose album's effective year
/// (display year, else year; kima keeps no original year, D-272) lies in `[start, start + 10)`, drawn uniformly.
pub async fn decade_track_ids(pool: &PgPool, start: i64, take: usize) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT t.id FROM tracks t JOIN albums b ON b.id = t.album_id
         WHERE t.missing_since IS NULL
           AND COALESCE(b.display_year, b.year)::int8 >= $1
           AND COALESCE(b.display_year, b.year)::int8 < $1 + 10
         ORDER BY random() LIMIT $2",
    )
    .bind(start)
    .bind(take as i64)
    .fetch_all(pool)
    .await
}

/// Upstream `libraryArtistIdsForGenre` + the genre radio's draw: up to `take` songs (present or not; the route keeps
/// the playable ones) of the library artists tagged with `value`, drawn uniformly. An artist's genres are its own
/// list when it has one, else its effective genres (`getMergedGenres`, D-166); a tag matches when its kima
/// [`genre_key`] equals the value's (D-114: kima's key, exact, not upstream's `genreKey` *contains*). A value with no
/// letters (upstream's empty `genreKey`) plays nothing.
pub async fn genre_track_ids(pool: &PgPool, value: &str, take: usize) -> sqlx::Result<Vec<Uuid>> {
    if upstream_genre_key(value).is_empty() {
        return Ok(Vec::new());
    }
    let wanted = genre_key(value);
    let artists: Vec<(Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT ar.id, COALESCE(ar.user_genres, '{}')
         FROM artists ar
         WHERE EXISTS (SELECT 1 FROM albums b JOIN tracks t ON t.album_id = b.id WHERE b.artist_id = ar.id)
         ORDER BY ar.id",
    )
    .fetch_all(pool)
    .await?;
    let ids: Vec<Uuid> = artists.iter().map(|a| a.0).collect();
    let mut effective = effective_for_artists(pool, &ids).await?;
    let tagged: Vec<Uuid> = artists
        .into_iter()
        .filter(|(id, own)| {
            merged_for_display(own, &effective.remove(id).unwrap_or_default())
                .iter()
                .any(|g| genre_key(g) == wanted)
        })
        .map(|(id, _)| id)
        .collect();
    if tagged.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(
        "SELECT t.id FROM tracks t JOIN albums b ON b.id = t.album_id
         WHERE b.artist_id = ANY($1) ORDER BY random() LIMIT $2",
    )
    .bind(&tagged)
    .bind(take as i64)
    .fetch_all(pool)
    .await
}

/// Upstream `listensOf`: how many listens (`stats/listens`, repeated rows of one listen folded) this profile has of
/// each of these songs; songs never listened to are absent.
pub async fn listens_of(
    pool: &PgPool,
    profile: ProfileId,
    ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, i64>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!(
        "{} SELECT p.track_id, count(*)::int8 FROM p WHERE p.track_id = ANY($2) GROUP BY p.track_id",
        listen_rows(Period::All, false)
    );
    let rows: Vec<(Uuid, i64)> = sqlx::query_as(&sql)
        .bind(profile)
        .bind(ids)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// The favorites radio's own-plays draw: up to `take` of the present songs this profile has listened to, drawn in
/// proportion to its listens of each (`ORDER BY -ln(1 - random()) / count`). Empty when it has none; the route then
/// plays random songs.
pub async fn favorite_track_ids(
    pool: &PgPool,
    profile: ProfileId,
    take: usize,
) -> sqlx::Result<Vec<Uuid>> {
    let sql = format!(
        "{} SELECT t.id FROM p JOIN tracks t ON t.id = p.track_id
         WHERE t.missing_since IS NULL
         GROUP BY t.id
         ORDER BY -ln(1 - random()) / count(*) ASC
         LIMIT $2",
        listen_rows(Period::All, false)
    );
    sqlx::query_scalar(&sql)
        .bind(profile)
        .bind(take as i64)
        .fetch_all(pool)
        .await
}

/// Upstream's discovery selection for `limit` songs: up to `2 * limit` of the present songs this profile never played
/// (any play row), drawn uniformly; when fewer than `limit`, those and a uniform draw of `10 * limit` present songs,
/// least listened to by this profile first (a stable sort), the first `2 * limit`.
pub async fn discovery_track_ids(
    pool: &PgPool,
    profile: ProfileId,
    limit: usize,
) -> sqlx::Result<Vec<Uuid>> {
    let unplayed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT t.id FROM tracks t
         WHERE t.missing_since IS NULL
           AND NOT EXISTS (SELECT 1 FROM plays p WHERE p.track_id = t.id AND p.profile_id = $1)
         ORDER BY random() LIMIT $2",
    )
    .bind(profile)
    .bind((limit * 2) as i64)
    .fetch_all(pool)
    .await?;
    if unplayed.len() >= limit {
        return Ok(unplayed);
    }
    let mut seen = HashSet::new();
    let mut candidates: Vec<Uuid> = unplayed
        .into_iter()
        .chain(sample_track_ids(pool, limit * 10).await?)
        .filter(|id| seen.insert(*id))
        .collect();
    let plays = listens_of(pool, profile, &candidates).await?;
    candidates.sort_by_key(|id| plays.get(id).copied().unwrap_or(0));
    candidates.truncate(limit * 2);
    Ok(candidates)
}

/// A song as artist radio compares it: bpm (current analysis), the current CLAP valence; no energy or danceability
/// (D-263).
const SOUND_SQL: &str = "ta.tempo_bpm::float8 AS bpm, r.valence_raw::float8 AS valence";

fn sound_joins() -> String {
    format!(
        "LEFT JOIN track_analysis ta ON ta.track_id = t.id AND ta.file_generation = t.ml_generation
         LEFT JOIN (SELECT te.track_id, te.created_at FROM {}) e ON e.track_id = t.id
         LEFT JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at",
        *ELIGIBLE
    )
}

/// One song of artist radio: whether it can play, and its sound.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistSong {
    pub id: Uuid,
    pub present: bool,
    pub sound: Sound,
}

fn artist_song((id, present, bpm, valence): (Uuid, bool, Option<f64>, Option<f64>)) -> ArtistSong {
    ArtistSong {
        id,
        present,
        sound: Sound {
            bpm,
            energy: None,
            valence,
            danceability: None,
        },
    }
}

/// Every song of the artist's albums, missing ones included (upstream's `artistTracks`), by id.
pub async fn artist_songs(pool: &PgPool, artist: Uuid) -> sqlx::Result<Vec<ArtistSong>> {
    let rows: Vec<(Uuid, bool, Option<f64>, Option<f64>)> = sqlx::query_as(&format!(
        "SELECT t.id, t.missing_since IS NULL, {SOUND_SQL}
         FROM tracks t JOIN albums b ON b.id = t.album_id {}
         WHERE b.artist_id = $1 ORDER BY t.id",
        sound_joins()
    ))
    .bind(artist)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(artist_song).collect())
}

/// The present songs of these artists' albums (upstream's `similarTracks`), by id.
pub async fn present_songs_of(pool: &PgPool, artists: &[Uuid]) -> sqlx::Result<Vec<ArtistSong>> {
    if artists.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(Uuid, bool, Option<f64>, Option<f64>)> = sqlx::query_as(&format!(
        "SELECT t.id, true, {SOUND_SQL}
         FROM tracks t JOIN albums b ON b.id = t.album_id {}
         WHERE b.artist_id = ANY($1) AND t.missing_since IS NULL ORDER BY t.id",
        sound_joins()
    ))
    .bind(artists)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(artist_song).collect())
}

/// The library artists (a present song) other than `except`, each with its genres (its own list when it has one,
/// else the effective genres: `getMergedGenres`, D-166), by id; and `except`'s own genres.
pub async fn library_artist_genres(
    pool: &PgPool,
    except: Uuid,
) -> sqlx::Result<(Vec<String>, Vec<(Uuid, Vec<String>)>)> {
    let rows: Vec<(Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT ar.id, COALESCE(ar.user_genres, '{}') FROM artists ar
         WHERE ar.id = $1 OR EXISTS (SELECT 1 FROM albums b JOIN tracks t ON t.album_id = b.id
                                      WHERE b.artist_id = ar.id AND t.missing_since IS NULL)
         ORDER BY ar.id",
    )
    .bind(except)
    .fetch_all(pool)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let mut effective = effective_for_artists(pool, &ids).await?;
    let mut own = Vec::new();
    let mut others = Vec::new();
    for (id, user) in rows {
        let genres = merged_for_display(&user, &effective.remove(&id).unwrap_or_default());
        if id == except {
            own = genres;
        } else {
            others.push((id, genres));
        }
    }
    Ok((own, others))
}

/// Whether the library has an artist (a present song) other than `artist` (upstream's `otherOwnedArtist`).
pub async fn other_library_artist(pool: &PgPool, artist: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tracks t JOIN albums b ON b.id = t.album_id
                        WHERE b.artist_id <> $1 AND t.missing_since IS NULL)",
    )
    .bind(artist)
    .fetch_one(pool)
    .await
}

/// The stored similar-artists value of an artist (`artists."similar"`), `None` for an unknown artist.
pub async fn stored_similar(
    pool: &PgPool,
    artist: Uuid,
) -> sqlx::Result<Option<serde_json::Value>> {
    let row: Option<(Option<serde_json::Value>,)> =
        sqlx::query_as("SELECT \"similar\" FROM artists WHERE id = $1")
            .bind(artist)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|r| r.0))
}

/// One row of `GET /library/radio`: the present song with its album and album artist.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RadioRow {
    pub id: Uuid,
    pub title: String,
    pub display_title: Option<String>,
    pub duration_secs: i32,
    pub track_no: i32,
    pub disc: i32,
    pub path: String,
    pub artist_id: Uuid,
    pub artist_name: String,
    /// The artist the mixes' spread counts the song under (upstream `songArtistKey`).
    pub artist_key: String,
    pub album_id: Uuid,
    pub album_title: String,
    pub album_has_image: bool,
}

/// The present songs among `ids`, in no particular order.
pub async fn radio_rows(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<Vec<RadioRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(&format!(
        "SELECT t.id, t.title, t.display_title, t.duration_secs, t.track_no, t.disc, t.path,
                ar.id AS artist_id, ar.name AS artist_name, {} AS artist_key,
                a.id AS album_id, a.title AS album_title, (a.image_path IS NOT NULL) AS album_has_image
         FROM tracks t JOIN albums a ON a.id = t.album_id JOIN artists ar ON ar.id = a.artist_id
         WHERE t.id = ANY($1) AND t.missing_since IS NULL",
        crate::mixes::recipe::artist_sql()
    ))
    .bind(ids)
    .fetch_all(pool)
    .await
}
