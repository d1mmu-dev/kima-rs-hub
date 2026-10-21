//! `track_lyrics`: embedded lyrics written by the scanner (spec §8.1, §14).

use sqlx::PgConnection;
use uuid::Uuid;

/// Each side of embedded lyrics is cut to this many bytes (at a character boundary). Tag
/// data is untrusted; real lyrics, even LRC with timestamps, are a few kilobytes.
pub const MAX_LYRICS_BYTES: usize = 256 * 1024;

fn capped(s: Option<&str>) -> Option<&str> {
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
