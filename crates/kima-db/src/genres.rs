//! Genres per track, album and artist, by source (spec §13). Names are stored as given and
//! compared by [`genre_key`]. Every input list is tag or API data: it is capped and handled
//! in linear time.

use std::collections::{HashMap, HashSet};

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// At most this many genres are kept per entity and source.
pub const MAX_GENRES: usize = 64;
/// File-derived album and artist genres keep the top this many.
pub const TOP_FILE_GENRES: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenreSource {
    MusicBrainz,
    File,
}

impl GenreSource {
    pub fn as_str(self) -> &'static str {
        match self {
            GenreSource::MusicBrainz => "musicbrainz",
            GenreSource::File => "file",
        }
    }
}

/// Lower-case, whitespace collapsed to single spaces, trimmed.
pub fn genre_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Ensures a genre row exists for each usable name and returns the genre ids in input order.
/// Names starting with `_` (upstream internal markers), empty names and repeats of a key are
/// skipped; at most [`MAX_GENRES`] are kept.
async fn genre_ids(conn: &mut PgConnection, names: &[String]) -> sqlx::Result<Vec<Uuid>> {
    let mut seen = HashSet::new();
    let mut new_ids = Vec::new();
    let mut kept_names = Vec::new();
    let mut keys = Vec::new();
    for name in names {
        if keys.len() >= MAX_GENRES {
            break;
        }
        let name = name.trim();
        if name.starts_with('_') {
            continue;
        }
        let key = genre_key(name);
        if key.is_empty() || !seen.insert(key.clone()) {
            continue;
        }
        new_ids.push(Uuid::now_v7());
        kept_names.push(name.to_string());
        keys.push(key);
    }
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    // Shared against `prune_unused`'s exclusive hold until this transaction ends, so a prune
    // never deletes a genre this transaction is about to refer to.
    sqlx::query!("SELECT pg_advisory_xact_lock_shared($1)", GENRE_PRUNE_LOCK)
        .execute(&mut *conn)
        .await?;
    // In key order: two transactions inserting the same new genres wait on each other's
    // unique-index entries in one order, so they queue up instead of deadlocking.
    sqlx::query!(
        "INSERT INTO genres (id, name, key)
         SELECT id, name, key FROM UNNEST($1::uuid[], $2::text[], $3::text[]) AS u(id, name, key)
         ORDER BY key COLLATE \"C\"
         ON CONFLICT (key) DO NOTHING",
        &new_ids,
        &kept_names,
        &keys
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query_scalar!(
        r#"SELECT g.id AS "id!" FROM UNNEST($1::text[]) WITH ORDINALITY AS u(key, ord)
           JOIN genres g ON g.key = u.key
           ORDER BY u.ord"#,
        &keys
    )
    .fetch_all(&mut *conn)
    .await
}

fn ranks(n: usize) -> Vec<i16> {
    (0..n)
        .map(|i| i16::try_from(i).unwrap_or(i16::MAX))
        .collect()
}

/// Replaces the track's genres of `source` with `names`; rank = position among the kept names.
pub async fn replace_track_genres(
    conn: &mut PgConnection,
    track: Uuid,
    source: GenreSource,
    names: &[String],
) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM track_genres WHERE track_id = $1 AND source = $2",
        track,
        source.as_str()
    )
    .execute(&mut *conn)
    .await?;
    let ids = genre_ids(conn, names).await?;
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query!(
        "INSERT INTO track_genres (track_id, genre_id, source, rank)
         SELECT $1, g, $2, r FROM UNNEST($3::uuid[], $4::int2[]) AS u(g, r)",
        track,
        source.as_str(),
        &ids,
        &ranks(ids.len())
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Replaces the album's genres of `source` with `names`.
pub async fn replace_album_genres(
    conn: &mut PgConnection,
    album: Uuid,
    source: GenreSource,
    names: &[String],
) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM album_genres WHERE album_id = $1 AND source = $2",
        album,
        source.as_str()
    )
    .execute(&mut *conn)
    .await?;
    let ids = genre_ids(conn, names).await?;
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query!(
        "INSERT INTO album_genres (album_id, genre_id, source, rank)
         SELECT $1, g, $2, r FROM UNNEST($3::uuid[], $4::int2[]) AS u(g, r)",
        album,
        source.as_str(),
        &ids,
        &ranks(ids.len())
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Replaces the artist's genres of `source` with `names`.
pub async fn replace_artist_genres(
    conn: &mut PgConnection,
    artist: Uuid,
    source: GenreSource,
    names: &[String],
) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM artist_genres WHERE artist_id = $1 AND source = $2",
        artist,
        source.as_str()
    )
    .execute(&mut *conn)
    .await?;
    let ids = genre_ids(conn, names).await?;
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query!(
        "INSERT INTO artist_genres (artist_id, genre_id, source, rank)
         SELECT $1, g, $2, r FROM UNNEST($3::uuid[], $4::int2[]) AS u(g, r)",
        artist,
        source.as_str(),
        &ids,
        &ranks(ids.len())
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Recomputes the file-derived genres of the given albums and artists: the top
/// [`TOP_FILE_GENRES`] by number of the entity's present tracks carrying them (file source),
/// ties by name (spec §13.1).
pub async fn recompute_file_genres(
    conn: &mut PgConnection,
    albums: &[Uuid],
    artists: &[Uuid],
) -> sqlx::Result<()> {
    if albums.is_empty() && artists.is_empty() {
        return Ok(());
    }
    // Writes genre references: shared against `prune_unused` (see `genre_ids`).
    sqlx::query!("SELECT pg_advisory_xact_lock_shared($1)", GENRE_PRUNE_LOCK)
        .execute(&mut *conn)
        .await?;
    if !albums.is_empty() {
        sqlx::query!(
            "DELETE FROM album_genres WHERE album_id = ANY($1) AND source = 'file'",
            albums
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query!(
            r#"INSERT INTO album_genres (album_id, genre_id, source, rank)
               SELECT album_id, genre_id, 'file', (rn - 1)::int2 FROM (
                   SELECT t.album_id, tg.genre_id,
                          row_number() OVER (PARTITION BY t.album_id
                                             ORDER BY count(DISTINCT t.id) DESC, g.name, g.id) AS rn
                   FROM tracks t
                   JOIN track_genres tg ON tg.track_id = t.id AND tg.source = 'file'
                   JOIN genres g ON g.id = tg.genre_id
                   WHERE t.album_id = ANY($1) AND t.missing_since IS NULL
                   GROUP BY t.album_id, tg.genre_id, g.name, g.id
               ) ranked
               WHERE rn <= $2"#,
            albums,
            TOP_FILE_GENRES
        )
        .execute(&mut *conn)
        .await?;
    }
    if !artists.is_empty() {
        sqlx::query!(
            "DELETE FROM artist_genres WHERE artist_id = ANY($1) AND source = 'file'",
            artists
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query!(
            r#"INSERT INTO artist_genres (artist_id, genre_id, source, rank)
               SELECT artist_id, genre_id, 'file', (rn - 1)::int2 FROM (
                   SELECT b.artist_id, tg.genre_id,
                          row_number() OVER (PARTITION BY b.artist_id
                                             ORDER BY count(DISTINCT t.id) DESC, g.name, g.id) AS rn
                   FROM tracks t
                   JOIN albums b ON b.id = t.album_id
                   JOIN track_genres tg ON tg.track_id = t.id AND tg.source = 'file'
                   JOIN genres g ON g.id = tg.genre_id
                   WHERE b.artist_id = ANY($1) AND t.missing_since IS NULL
                   GROUP BY b.artist_id, tg.genre_id, g.name, g.id
               ) ranked
               WHERE rn <= $2"#,
            artists,
            TOP_FILE_GENRES
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Ids per query in the `effective_for_*` and `musicbrainz_for_*` readers.
const ID_CHUNK: usize = 2000;
/// Genres at least this many present tracks carry are listed by [`library_genres`].
pub const LIBRARY_GENRES_MIN_TRACKS: i64 = 15;
/// [`library_genres`] returns at most this many genres.
pub const LIBRARY_GENRES_LIMIT: i64 = 20;

macro_rules! genre_reader {
    ($(#[$doc:meta])* $name:ident, $view:literal, $col:literal) => {
        $(#[$doc])*
        pub async fn $name(
            pool: &PgPool,
            ids: &[Uuid],
        ) -> sqlx::Result<HashMap<Uuid, Vec<String>>> {
            let mut out: HashMap<Uuid, Vec<String>> = HashMap::new();
            for chunk in ids.chunks(ID_CHUNK) {
                let rows: Vec<(Uuid, String)> = sqlx::query_as(concat!(
                    "SELECT e.", $col, ", g.name FROM ", $view, " e JOIN genres g ON g.id = e.genre_id ",
                    "WHERE e.", $col, " = ANY($1) ORDER BY e.", $col, ", e.rank, g.name, g.id"
                ))
                .bind(chunk)
                .fetch_all(pool)
                .await?;
                for (id, name) in rows {
                    out.entry(id).or_default().push(name);
                }
            }
            Ok(out)
        }
    };
}

genre_reader!(
    /// Effective genres (MusicBrainz, else file) per track, by rank. Tracks with none are absent.
    effective_for_tracks, "track_genres_effective", "track_id"
);
genre_reader!(
    /// Effective genres per album, by rank.
    effective_for_albums, "album_genres_effective", "album_id"
);
genre_reader!(
    /// Effective genres per artist, by rank.
    effective_for_artists, "artist_genres_effective", "artist_id"
);

macro_rules! musicbrainz_reader {
    ($(#[$doc:meta])* $name:ident, $table:literal, $col:literal) => {
        $(#[$doc])*
        pub async fn $name(
            pool: &PgPool,
            ids: &[Uuid],
        ) -> sqlx::Result<HashMap<Uuid, Vec<String>>> {
            let mut out: HashMap<Uuid, Vec<String>> = HashMap::new();
            for chunk in ids.chunks(ID_CHUNK) {
                let rows: Vec<(Uuid, String)> = sqlx::query_as(concat!(
                    "SELECT e.", $col, ", g.name FROM ", $table, " e JOIN genres g ON g.id = e.genre_id ",
                    "WHERE e.source = 'musicbrainz' AND e.", $col, " = ANY($1) ",
                    "ORDER BY e.", $col, ", e.rank, g.name, g.id"
                ))
                .bind(chunk)
                .fetch_all(pool)
                .await?;
                for (id, name) in rows {
                    out.entry(id).or_default().push(name);
                }
            }
            Ok(out)
        }
    };
}

musicbrainz_reader!(
    /// MusicBrainz genres per album (the `tags` alias upstream sends), by rank.
    musicbrainz_for_albums, "album_genres", "album_id"
);
musicbrainz_reader!(
    /// MusicBrainz genres per artist (the `tags` alias), by rank.
    musicbrainz_for_artists, "artist_genres", "artist_id"
);

/// `[...user, ...genres]`, unique by [`genre_key`] (first spelling wins), empty names dropped
/// (upstream `getMergedGenres`).
pub fn merged(user: &[String], genres: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    user.iter()
        .chain(genres)
        .filter(|g| seen.insert(genre_key(g)) && !genre_key(g).is_empty())
        .cloned()
        .collect()
}

/// Effective artist genres with the number of distinct present tracks of the artist's albums
/// that sit under them, at least [`LIBRARY_GENRES_MIN_TRACKS`], most first (ties by name),
/// at most [`LIBRARY_GENRES_LIMIT`] (spec §13.2).
pub async fn library_genres(pool: &PgPool) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as(
        "SELECT g.name, count(DISTINCT t.id) AS n
         FROM artist_genres_effective ag
         JOIN genres g ON g.id = ag.genre_id
         JOIN albums b ON b.artist_id = ag.artist_id
         JOIN tracks t ON t.album_id = b.id AND t.missing_since IS NULL
         WHERE g.name NOT LIKE '\\_%'
         GROUP BY g.id, g.name
         HAVING count(DISTINCT t.id) >= $1
         ORDER BY n DESC, g.name, g.id
         LIMIT $2",
    )
    .bind(LIBRARY_GENRES_MIN_TRACKS)
    .bind(LIBRARY_GENRES_LIMIT)
    .fetch_all(pool)
    .await
}

/// Candidates for "this genre is an artist's name": `(genre, artist name)` pairs where the two
/// agree under lower-casing, accent folding, `&` as "and" and collapsed spaces. The caller
/// confirms with its own normalisation. One query for all genres.
pub async fn artist_name_candidates(
    pool: &PgPool,
    genres: &[String],
) -> sqlx::Result<Vec<(String, String)>> {
    if genres.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        r"SELECT u.genre, a.name
          FROM UNNEST($1::text[]) AS u(genre)
          JOIN artists a ON btrim(regexp_replace(regexp_replace(lower(unaccent(a.name)), '\s*&\s*', ' and ', 'g'), '\s+', ' ', 'g'))
                          = btrim(regexp_replace(regexp_replace(lower(unaccent(u.genre)), '\s*&\s*', ' and ', 'g'), '\s+', ' ', 'g'))",
    )
    .bind(genres)
    .fetch_all(pool)
    .await
}

/// Advisory lock key serialising [`prune_unused`] against genre references being written
/// (`hashtext('kima_genres_prune')` would do as well; a constant keeps it obvious).
const GENRE_PRUNE_LOCK: i64 = 0x6b69_6d61_6765_6e72; // "kimagenr"

/// Deletes genres no track, album or artist refers to. Returns how many.
///
/// Holds the genre lock exclusively, so it first waits for every transaction that is writing
/// genre references (they hold it shared until they end); its `DELETE` then starts after they
/// committed and sees their references. Without it, a genre that a concurrent transaction had
/// just looked up could be deleted, taking that transaction's new reference with it.
pub async fn prune_unused(pool: &PgPool) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", GENRE_PRUNE_LOCK)
        .execute(&mut *tx)
        .await?;
    let r = sqlx::query!(
        "DELETE FROM genres g
         WHERE NOT EXISTS (SELECT 1 FROM track_genres x WHERE x.genre_id = g.id)
           AND NOT EXISTS (SELECT 1 FROM album_genres x WHERE x.genre_id = g.id)
           AND NOT EXISTS (SELECT 1 FROM artist_genres x WHERE x.genre_id = g.id)"
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_collapse_whitespace_and_case() {
        assert_eq!(genre_key("  Hip   Hop\t"), "hip hop");
        assert_eq!(genre_key("Drum\u{a0}and Bass"), "drum and bass");
        assert_eq!(genre_key(""), "");
    }
}
