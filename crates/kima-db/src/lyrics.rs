//! `track_lyrics`: embedded lyrics written by the scanner (spec §8.1, §14) and the LRCLIB
//! answers and absences the lyrics route stores (spec §14.2).

use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

/// Each side of stored lyrics is cut to this many bytes (at a character boundary). Tag data and
/// LRCLIB answers are untrusted; real lyrics, even LRC with timestamps, are a few kilobytes.
pub const MAX_LYRICS_BYTES: usize = 256 * 1024;

/// A track's stored lyrics row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lyrics {
    pub plain: Option<String>,
    pub synced: Option<String>,
    /// `embedded`, `lrclib` or `none`.
    pub source: String,
    pub fetched_at: OffsetDateTime,
}

/// `s` cut to [`MAX_LYRICS_BYTES`] at a character boundary.
pub fn capped(s: Option<&str>) -> Option<&str> {
    let s = s?;
    if s.len() <= MAX_LYRICS_BYTES {
        return Some(s);
    }
    let mut end = MAX_LYRICS_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    Some(&s[..end])
}

/// Stores the file's lyrics with source `embedded`, replacing a row of any source.
pub async fn upsert_embedded(
    conn: &mut PgConnection,
    track: Uuid,
    plain: Option<&str>,
    synced: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO track_lyrics (track_id, plain_lyrics, synced_lyrics, source)
         VALUES ($1, $2, $3, 'embedded')
         ON CONFLICT (track_id) DO UPDATE
         SET plain_lyrics = EXCLUDED.plain_lyrics, synced_lyrics = EXCLUDED.synced_lyrics,
             source = 'embedded', lrclib_id = NULL, fetched_at = now(), updated_at = now()",
        track,
        capped(plain),
        capped(synced)
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Deletes the track's lyrics row only if its source is `embedded` (D-115).
pub async fn delete_embedded(conn: &mut PgConnection, track: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM track_lyrics WHERE track_id = $1 AND source = 'embedded'",
        track
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The track's lyrics row, if it has one.
pub async fn get(pool: &PgPool, track: Uuid) -> sqlx::Result<Option<Lyrics>> {
    sqlx::query_as!(
        Lyrics,
        r#"SELECT plain_lyrics AS plain, synced_lyrics AS synced, source, fetched_at
           FROM track_lyrics WHERE track_id = $1"#,
        track
    )
    .fetch_optional(pool)
    .await
}

/// What LRCLIB is asked for a track (spec §14.1): its title, its album artist's name, its
/// album's title and its duration in seconds. `None` for no such track; a track flagged missing
/// has them like a present one.
pub async fn lookup_fields(
    pool: &PgPool,
    track: Uuid,
) -> sqlx::Result<Option<(String, String, String, i32)>> {
    let row = sqlx::query!(
        "SELECT t.title, ar.name AS artist, al.title AS album, t.duration_secs
         FROM tracks t
         JOIN albums al ON al.id = t.album_id
         JOIN artists ar ON ar.id = al.artist_id
         WHERE t.id = $1",
        track
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.title, r.artist, r.album, r.duration_secs)))
}

/// Stores an LRCLIB answer: source `lrclib`, `fetched_at = now()`. Replaces a `none` or an older
/// `lrclib` row, never an `embedded` one (the file wins; the scanner may have written it while
/// LRCLIB was asked). False when nothing was written: an `embedded` row, or the track is gone.
pub async fn upsert_lrclib(
    pool: &PgPool,
    track: Uuid,
    plain: Option<&str>,
    synced: Option<&str>,
    lrclib_id: i32,
) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "INSERT INTO track_lyrics (track_id, plain_lyrics, synced_lyrics, source, lrclib_id)
         SELECT id, $2, $3, 'lrclib', $4 FROM tracks WHERE id = $1
         ON CONFLICT (track_id) DO UPDATE
         SET plain_lyrics = EXCLUDED.plain_lyrics, synced_lyrics = EXCLUDED.synced_lyrics,
             source = 'lrclib', lrclib_id = EXCLUDED.lrclib_id, fetched_at = now(),
             updated_at = now()
         WHERE track_lyrics.source IN ('lrclib', 'none')",
        track,
        capped(plain),
        capped(synced),
        lrclib_id
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Stores LRCLIB's "no such track": source `none`, no text, `fetched_at = now()` (the 30-day
/// negative cache). Refreshes only a `none` row: an answer or the file's lyrics, however they
/// got there meanwhile, are kept. False when nothing was written.
pub async fn upsert_none(pool: &PgPool, track: Uuid) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "INSERT INTO track_lyrics (track_id, source)
         SELECT id, 'none' FROM tracks WHERE id = $1
         ON CONFLICT (track_id) DO UPDATE
         SET plain_lyrics = NULL, synced_lyrics = NULL, lrclib_id = NULL, fetched_at = now(),
             updated_at = now()
         WHERE track_lyrics.source = 'none'",
        track
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_cuts_at_a_char_boundary() {
        let long = "é".repeat(MAX_LYRICS_BYTES); // 2 bytes each
        let c = capped(Some(&long)).unwrap();
        assert!(c.len() <= MAX_LYRICS_BYTES);
        assert_eq!(c.len(), MAX_LYRICS_BYTES);
        let odd = format!("a{long}");
        assert_eq!(capped(Some(&odd)).unwrap().len(), MAX_LYRICS_BYTES - 1);
        assert_eq!(capped(Some("short")), Some("short"));
        assert_eq!(capped(None), None);
    }
}
