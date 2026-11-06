//! The album vote's storage (migration 0032; M-SYNC Task 13): which untagged albums are due a
//! vote, their songs, the remembered verdicts, and the writes an assigned album needs.

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// An untagged album due a vote (no release-group id, no remembered verdict that still holds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub artist: String,
    /// Hash of the present songs' ids, fingerprints and file times.
    pub signature: String,
}

/// Albums with no release-group id and a present song, whose remembered verdict (if any) is
/// stale, expired or missing. With `fingerprints_settled` every present song's fingerprint
/// question must be closed (`done`, or failed for good), as the recording stage's claim requires.
///
/// `in_window`: only the albums in `window`, in that order (the albums of the songs about to go
/// out); else every other album, by id.
pub async fn candidates(
    pool: &PgPool,
    window: &[Uuid],
    in_window: bool,
    fingerprints_settled: bool,
    limit: i64,
) -> sqlx::Result<Vec<Candidate>> {
    let rows = sqlx::query!(
        r#"WITH c AS (
             SELECT a.id, a.title, a.artist_id, ar.name AS artist,
                    (SELECT encode(sha256(convert_to(string_agg(
                                t.id::text || ':'
                                || COALESCE(encode(sha256(convert_to(ta.fingerprint, 'UTF8')), 'hex'), '-')
                                || ':' || (extract(epoch FROM t.mtime) * 1000)::bigint::text,
                                '|' ORDER BY t.id), 'UTF8')), 'hex')
                     FROM tracks t LEFT JOIN track_acoustid ta ON ta.track_id = t.id
                     WHERE t.album_id = a.id AND t.missing_since IS NULL) AS signature
             FROM albums a JOIN artists ar ON ar.id = a.artist_id
             WHERE a.release_group_mbid IS NULL AND a.release_group_mbid_source IS NULL
               AND (a.id = ANY($1)) = $2
               AND EXISTS (SELECT 1 FROM tracks t
                           WHERE t.album_id = a.id AND t.missing_since IS NULL)
               AND (NOT $3 OR NOT EXISTS (
                     SELECT 1 FROM tracks t
                     WHERE t.album_id = a.id AND t.missing_since IS NULL
                       AND NOT EXISTS (SELECT 1 FROM track_stages f
                                       WHERE f.track_id = t.id AND f.stage = 'fingerprint'
                                         AND (f.status = 'done'
                                              OR (f.status = 'failed' AND f.next_attempt_at IS NULL))))))
           SELECT c.id, c.title, c.artist_id, c.artist, c.signature AS "signature!"
           FROM c LEFT JOIN album_votes v ON v.album_id = c.id
           WHERE v.album_id IS NULL OR v.signature IS DISTINCT FROM c.signature
                 OR v.expires_at <= now()
           ORDER BY CASE WHEN $2 THEN array_position($1::uuid[], c.id) END, c.id
           LIMIT $4"#,
        window,
        in_window,
        fingerprints_settled,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Candidate {
            id: r.id,
            title: r.title,
            artist_id: r.artist_id,
            artist: r.artist,
            signature: r.signature,
        })
        .collect())
}

/// A present song of an album and its stored fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    pub id: Uuid,
    pub title: String,
    pub duration_secs: i32,
    pub fingerprint: Option<String>,
}

/// The album's present songs, by id.
pub async fn songs(pool: &PgPool, album: Uuid) -> sqlx::Result<Vec<Song>> {
    let rows = sqlx::query!(
        r#"SELECT t.id, t.title, t.duration_secs, ta.fingerprint AS "fingerprint?"
           FROM tracks t LEFT JOIN track_acoustid ta ON ta.track_id = t.id
           WHERE t.album_id = $1 AND t.missing_since IS NULL
           ORDER BY t.id"#,
        album
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Song {
            id: r.id,
            title: r.title,
            duration_secs: r.duration_secs,
            fingerprint: r.fingerprint,
        })
        .collect())
}

/// Remembers what was decided for the album for `ttl_secs`.
pub async fn remember(
    pool: &PgPool,
    album: Uuid,
    signature: &str,
    verdict: &Value,
    ttl_secs: i64,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO album_votes (album_id, signature, verdict, decided_at, expires_at)
         VALUES ($1, $2, $3, now(), now() + make_interval(secs => $4::float8))
         ON CONFLICT (album_id) DO UPDATE SET
             signature = EXCLUDED.signature, verdict = EXCLUDED.verdict,
             decided_at = EXCLUDED.decided_at, expires_at = EXCLUDED.expires_at",
        album,
        signature,
        verdict,
        ttl_secs as f64
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Forgets every remembered verdict, so a re-run of identification votes every untagged album
/// again. The saved AcoustID replies stay: they answer the vote without a request.
pub async fn forget_all(conn: &mut PgConnection) -> sqlx::Result<u64> {
    Ok(sqlx::query!("DELETE FROM album_votes")
        .execute(&mut *conn)
        .await?
        .rows_affected())
}

/// Whether another album already holds this release-group id.
pub async fn id_is_held(conn: &mut PgConnection, group: Uuid, except: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM albums WHERE release_group_mbid = $1 AND id <> $2) AS "held!""#,
        group,
        except
    )
    .fetch_one(&mut *conn)
    .await
}

/// The titles of the artist's other albums.
pub async fn sibling_titles(
    pool: &PgPool,
    artist: Uuid,
    except: Uuid,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT title FROM albums WHERE artist_id = $1 AND id <> $2",
        artist,
        except
    )
    .fetch_all(pool)
    .await
}

/// Songs of the album that found no recording before can be named from its tracklist now: their
/// `recording` row, closed `done`, goes back to `pending`, due now. Returns rows reopened.
pub async fn reopen_unnamed_songs(pool: &PgPool, album: Uuid) -> sqlx::Result<u64> {
    Ok(sqlx::query!(
        r#"UPDATE track_stages s
           SET status = 'pending', next_attempt_at = NULL, started_at = NULL, finished_at = NULL,
               evidence = '{"reason":"album identified"}'::jsonb, updated_at = now()
           FROM tracks t
           WHERE s.track_id = t.id AND t.album_id = $1
             AND s.stage = 'recording' AND s.status = 'done' AND t.recording_mbid IS NULL"#,
        album
    )
    .execute(pool)
    .await?
    .rows_affected())
}
