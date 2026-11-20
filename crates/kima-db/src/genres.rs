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
    effective_for_tracks,
    "track_genres_effective",
    "track_id"
);
genre_reader!(
    /// Effective genres per album, by rank.
    effective_for_albums,
    "album_genres_effective",
    "album_id"
);
genre_reader!(
    /// Effective genres per artist, by rank.
    effective_for_artists,
    "artist_genres_effective",
    "artist_id"
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
    musicbrainz_for_albums,
    "album_genres",
    "album_id"
);
musicbrainz_reader!(
    /// MusicBrainz genres per artist (the `tags` alias), by rank.
    musicbrainz_for_artists,
    "artist_genres",
    "artist_id"
);

/// The genres an entity shows (upstream `getMergedGenres`, D-166): a person's own list **is**
/// the result when it has any name, and the enriched `genres` only when it has none. Either way
/// unique by [`genre_key`] (the first spelling wins, in order) with empty names dropped.
pub fn merged_for_display(user: &[String], genres: &[String]) -> Vec<String> {
    let own = unique_by_key(user);
    if own.is_empty() {
        unique_by_key(genres)
    } else {
        own
    }
}

fn unique_by_key(names: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    names
        .iter()
        .filter(|g| {
            let key = genre_key(g);
            !key.is_empty() && seen.insert(key)
        })
        .cloned()
        .collect()
}

/// One artist as the genre list reads it (upstream `GenreCountedArtist`): its name, its genres (a
/// person's own list when it has any name, else the effective genres; D-166) and its present
/// library songs.
#[derive(Debug, Clone, PartialEq)]
pub struct GenreArtist {
    pub name: String,
    pub genres: Vec<String>,
    pub songs: i64,
}

/// Upstream `genreKey` (`utils/genreSql.ts`): NFKC, lower-case, without spaces (any width),
/// hyphens, dashes, the minus sign, underscores, slashes, dots, middle dots, apostrophes and the
/// invisible soft hyphen, zero-width space and byte-order mark. kima groups genres by its own
/// [`genre_key`] (D-166 (2)); this key only tells an artist's tag of its own name, and a name
/// made of nothing else, as upstream does.
pub fn upstream_genre_key(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let stripped = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '\u{a0}'
                    | '\u{202f}'
                    | '\u{ad}'
                    | '\u{200b}'
                    | '\u{feff}'
                    | '_'
                    | '/'
                    | '.'
                    | '\u{b7}'
                    | '\''
                    | '\u{2018}'
                    | '\u{2019}'
                    | '\u{2010}'..='\u{2015}' | '\u{2043}' | '\u{2212}' | '-'
            )
    };
    name.nfkc()
        .collect::<String>()
        .to_lowercase()
        .chars()
        .filter(|c| !stripped(*c))
        .collect()
}

/// JavaScript's `<` on strings: UTF-16 code units.
fn js_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Upstream `computeGenreTotals` + `libraryGenres` (`services/mixes/radioRow.ts:40-57`): each
/// artist's songs count once under every genre it carries; a tag naming the artist itself (by
/// upstream's `genreKey`), a name starting with `_` and a name with no letters are no genre;
/// spellings of one genre (kima's [`genre_key`], D-166 (2)) are listed under the one carrying
/// the most songs (ties: the first in UTF-16 order); a genre needs [`LIBRARY_GENRES_MIN_TRACKS`]
/// songs; names lower-cased, most songs first then by name, at most [`LIBRARY_GENRES_LIMIT`].
pub fn genre_totals(artists: &[GenreArtist]) -> Vec<(String, i64)> {
    // Per genre key: its songs and its spellings with theirs, in first-seen order.
    let mut totals: Vec<(i64, Vec<(String, i64)>)> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for artist in artists {
        let own = upstream_genre_key(&artist.name);
        let mut seen = HashSet::new();
        for raw in &artist.genres {
            let name = raw.trim();
            if name.is_empty() || name.starts_with('_') {
                continue;
            }
            let loose = upstream_genre_key(name);
            if loose.is_empty() || loose == own {
                continue;
            }
            let key = genre_key(name);
            let i = *at.entry(key.clone()).or_insert_with(|| {
                totals.push((0, Vec::new()));
                totals.len() - 1
            });
            let entry = &mut totals[i];
            if seen.insert(key) {
                entry.0 += artist.songs;
            }
            match entry.1.iter_mut().find(|(n, _)| n == name) {
                Some((_, songs)) => *songs += artist.songs,
                None => entry.1.push((name.to_string(), artist.songs)),
            }
        }
    }
    let mut out: Vec<(String, i64)> = totals
        .into_iter()
        .filter(|(songs, _)| *songs >= LIBRARY_GENRES_MIN_TRACKS)
        .map(|(songs, mut spellings)| {
            spellings.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| js_cmp(&x.0, &y.0)));
            (spellings[0].0.to_lowercase(), songs)
        })
        .collect();
    out.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| js_cmp(&x.0, &y.0)));
    out.truncate(usize::try_from(LIBRARY_GENRES_LIMIT).unwrap_or(usize::MAX));
    out
}

/// The artists with present tracks and their genres, for [`genre_totals`] (spec §13.2): a
/// person's own list when it has any name (D-166), else the effective genres. Songs are the
/// present tracks of the artist's albums (D-114).
pub async fn library_genre_artists(pool: &PgPool) -> sqlx::Result<Vec<GenreArtist>> {
    let rows: Vec<(Uuid, String, Vec<String>, i64)> = sqlx::query_as(
        "SELECT ar.id, ar.name, COALESCE(ar.user_genres, '{}'), count(t.id) AS n
         FROM artists ar
         JOIN albums b ON b.artist_id = ar.id
         JOIN tracks t ON t.album_id = b.id AND t.missing_since IS NULL
         GROUP BY ar.id
         ORDER BY ar.id",
    )
    .fetch_all(pool)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let mut effective = effective_for_artists(pool, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, own, songs)| GenreArtist {
            genres: merged_for_display(&own, &effective.remove(&id).unwrap_or_default()),
            name,
            songs,
        })
        .collect())
}

/// `GET /library/genres`' list: [`genre_totals`] over [`library_genre_artists`].
pub async fn library_genres(pool: &PgPool) -> sqlx::Result<Vec<(String, i64)>> {
    Ok(genre_totals(&library_genre_artists(pool).await?))
}

/// Candidates for "this genre is an artist's name": `(genre, artist name)` pairs where the two
/// agree under lower-casing, accent folding, `&` as "and" and collapsed spaces. The caller
/// confirms with its own normalisation. One query for all genres.
///
/// Each genre is first narrowed through the artists' search index (`artists_search_idx`) by
/// its words without "and" (an artist may write it `&`, which the vector drops), so the
/// comparison runs on those artists only, not on every artist for every genre. Equal names
/// always share those words, so the narrowing loses no pair.
pub async fn artist_name_candidates(
    pool: &PgPool,
    genres: &[String],
) -> sqlx::Result<Vec<(String, String)>> {
    if genres.is_empty() {
        return Ok(Vec::new());
    }
    // `tq` is the genre's words as a tsquery. The lateral (with `OFFSET 0`, so it is not
    // flattened back into one hash join over every artist) asks the index per genre. A genre with no words at all (punctuation only)
    // cannot be narrowed, and only compares against the artists that have none either.
    sqlx::query_as(
        r"SELECT u.genre, a.name
          FROM UNNEST($1::text[]) AS u(genre)
          CROSS JOIN LATERAL (
              SELECT string_agg('(' || v.q || ')', ' | ')::tsquery AS q
              FROM (VALUES (plainto_tsquery('simple', unaccent(u.genre))::text),
                           (plainto_tsquery('simple', unaccent(replace(lower(u.genre), ' and ', ' ')))::text)
                   ) AS v(q)
              WHERE v.q <> '') AS tq
          CROSS JOIN LATERAL (
              SELECT a.name FROM artists a
              WHERE a.search @@ tq.q
                AND btrim(regexp_replace(regexp_replace(lower(unaccent(a.name)), '\s*&\s*', ' and ', 'g'), '\s+', ' ', 'g'))
                  = btrim(regexp_replace(regexp_replace(lower(unaccent(u.genre)), '\s*&\s*', ' and ', 'g'), '\s+', ' ', 'g'))
              OFFSET 0) AS a
          UNION ALL
          SELECT u.genre, a.name
          FROM UNNEST($1::text[]) AS u(genre)
          JOIN artists a ON length(a.search) = 0
                        AND btrim(regexp_replace(regexp_replace(lower(unaccent(a.name)), '\s*&\s*', ' and ', 'g'), '\s+', ' ', 'g'))
                          = btrim(regexp_replace(regexp_replace(lower(unaccent(u.genre)), '\s*&\s*', ' and ', 'g'), '\s+', ' ', 'g'))
          WHERE length(to_tsvector('simple', unaccent(u.genre))) = 0",
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

    fn artist(name: &str, songs: i64, genres: &[&str]) -> GenreArtist {
        GenreArtist {
            name: name.into(),
            genres: genres.iter().map(|g| g.to_string()).collect(),
            songs,
        }
    }

    #[test]
    fn upstream_key_folds_spellings() {
        for s in [
            "Hip Hop",
            "hip-hop",
            "HipHop",
            "HIP_HOP",
            "hip\u{2010}hop",
            "hip\u{a0}hop",
            "hip\u{ad}hop",
            "hip\u{2212}hop",
        ] {
            assert_eq!(upstream_genre_key(s), "hiphop", "{s}");
        }
        assert_eq!(
            upstream_genre_key("e\u{301}lectro"),
            upstream_genre_key("\u{e9}lectro")
        );
        assert_eq!(upstream_genre_key(" / "), "");
        assert_eq!(upstream_genre_key("ロック"), "ロック");
    }

    #[test]
    fn totals_count_an_artist_once_per_genre_and_drop_its_own_name() {
        let got = genre_totals(&[
            artist("A", 10, &["Rock", "rock"]),
            artist("B", 5, &["rock"]),
            artist("Synth-Wave", 30, &["synth wave", "pop"]),
            artist("C", 14, &["solo"]),
        ]);
        // "Rock" carries 10 songs, "rock" 15: the genre is named by the latter.
        assert_eq!(got, vec![("pop".into(), 30), ("rock".into(), 15)]);
    }

    #[test]
    fn keys_collapse_whitespace_and_case() {
        assert_eq!(genre_key("  Hip   Hop\t"), "hip hop");
        assert_eq!(genre_key("Drum\u{a0}and Bass"), "drum and bass");
        assert_eq!(genre_key(""), "");
    }
}
