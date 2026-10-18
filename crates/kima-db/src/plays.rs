//! Listening history. What counts as a play follows upstream's `playRecording.ts` (Last.fm's
//! rule): the track is longer than 30 seconds and was heard for half its length or four minutes,
//! whichever comes first.

use sqlx::{FromRow, PgPool};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::browse::{ArtistRow, TRACK_SELECT, TrackRow};

/// A track at or below this length can never be scrobbled.
pub const MIN_TRACK_DURATION_SECONDS: f64 = 30.0;
/// Nobody has to finish a long track. Four minutes is always enough.
pub const MAX_REQUIRED_SECONDS: f64 = 240.0;
/// The shortest window a repeat is ever suppressed for, whatever the track's length.
const MIN_DEDUP_WINDOW_MS: f64 = 30_000.0;

/// How long a repeat of the same track is the same listen: the track's own length (at least 30 s;
/// four minutes when the length is unknown), applied on both sides of the reported play.
pub fn dedup_window_ms(duration_secs: Option<f64>) -> f64 {
    match duration_secs {
        Some(d) if d.is_finite() && d > 0.0 => MIN_DEDUP_WINDOW_MS.max(d * 1000.0),
        _ => MAX_REQUIRED_SECONDS * 1000.0,
    }
}

/// Seconds of a track that must be heard before it counts, or `None` when it can never qualify.
/// An unknown length (0) demands the four-minute bound rather than waving the play through.
pub fn required_listen_seconds(duration_secs: Option<f64>) -> Option<f64> {
    match duration_secs {
        Some(d) if d.is_finite() && d > 0.0 => {
            if d <= MIN_TRACK_DURATION_SECONDS {
                None
            } else {
                Some((d / 2.0).min(MAX_REQUIRED_SECONDS))
            }
        }
        _ => Some(MAX_REQUIRED_SECONDS),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayOutcome {
    Recorded(Uuid),
    TooShort,
    NotEnoughListened,
    Duplicate,
    UnknownTrack,
}

/// How many tracks the library holds right now (those not flagged missing).
pub async fn count_present_tracks(pool: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM tracks WHERE missing_since IS NULL")
        .fetch_one(pool)
        .await
}

/// Records a play if the listening earns it. The duration comes from the database, never the
/// request. The duplicate check and the insert run in one transaction behind a per-(user, track)
/// advisory lock, so concurrent reports of one play cannot both write.
pub async fn record(
    pool: &PgPool,
    user: Uuid,
    track: Uuid,
    seconds_listened: f64,
    played_at: Option<OffsetDateTime>,
) -> sqlx::Result<PlayOutcome> {
    let Some(duration) =
        sqlx::query_scalar::<_, i32>("SELECT duration_secs FROM tracks WHERE id = $1")
            .bind(track)
            .fetch_optional(pool)
            .await?
    else {
        return Ok(PlayOutcome::UnknownTrack);
    };
    let duration = Some(f64::from(duration));
    let Some(required) = required_listen_seconds(duration) else {
        return Ok(PlayOutcome::TooShort);
    };
    if !seconds_listened.is_finite() || seconds_listened < required {
        return Ok(PlayOutcome::NotEnoughListened);
    }

    let anchor = played_at.unwrap_or_else(OffsetDateTime::now_utc);
    let window = Duration::milliseconds(dedup_window_ms(duration).round() as i64);

    let mut tx = pool.begin().await?;
    // Namespace 7001 plus the user/track hash. READ COMMITTED (the default) is what lets the
    // waiting writer see the committed row.
    sqlx::query("SELECT pg_advisory_xact_lock(7001, hashtext($1))")
        .bind(format!("{user}:{track}"))
        .execute(&mut *tx)
        .await?;
    // Strict bounds on both sides: exactly one duration away, the song has finished and that
    // play is a genuine replay.
    let recent: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM plays WHERE user_id = $1 AND track_id = $2
           AND played_at > $3 AND played_at < $4 LIMIT 1",
    )
    .bind(user)
    .bind(track)
    .bind(anchor - window)
    .bind(anchor + window)
    .fetch_optional(&mut *tx)
    .await?;
    if recent.is_some() {
        return Ok(PlayOutcome::Duplicate);
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO plays (id, user_id, track_id, played_at, heard) VALUES ($1, $2, $3, $4, true)",
    )
    .bind(id)
    .bind(user)
    .bind(track)
    .bind(anchor)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(PlayOutcome::Recorded(id))
}

#[derive(Debug, Clone, FromRow)]
pub struct PlayRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub track_id: Uuid,
    pub played_at: OffsetDateTime,
    pub heard: bool,
}

/// A user's plays, newest first, whose tracks are still present.
pub async fn recent(
    pool: &PgPool,
    user: Uuid,
    limit: i64,
) -> sqlx::Result<Vec<(PlayRow, TrackRow)>> {
    let plays: Vec<PlayRow> = sqlx::query_as(
        "SELECT p.id, p.user_id, p.track_id, p.played_at, p.heard
         FROM plays p JOIN tracks t ON t.id = p.track_id AND t.missing_since IS NULL
         WHERE p.user_id = $1 ORDER BY p.played_at DESC, p.id DESC LIMIT $2",
    )
    .bind(user)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let ids: Vec<Uuid> = plays.iter().map(|p| p.track_id).collect();
    let sql = format!("{TRACK_SELECT} AND t.id = ANY($1)");
    let tracks: Vec<TrackRow> = sqlx::query_as(&sql).bind(&ids).fetch_all(pool).await?;
    Ok(plays
        .into_iter()
        .filter_map(|p| {
            let t = tracks.iter().find(|t| t.id == p.track_id)?.clone();
            Some((p, t))
        })
        .collect())
}

#[derive(Debug, Clone, FromRow)]
pub struct RecentArtist {
    #[sqlx(flatten)]
    pub artist: ArtistRow,
    pub last_played_at: OffsetDateTime,
}

/// Album artists the user played most recently (one row each, newest first). Plays of tracks
/// flagged missing do not count.
pub async fn recent_artists(
    pool: &PgPool,
    user: Uuid,
    limit: i64,
) -> sqlx::Result<Vec<RecentArtist>> {
    sqlx::query_as(
        "WITH last AS (
             SELECT ar.id, max(p.played_at) AS last_played_at
             FROM plays p
             JOIN tracks t ON t.id = p.track_id AND t.missing_since IS NULL
             JOIN albums al ON al.id = t.album_id
             JOIN artists ar ON ar.id = al.artist_id
             WHERE p.user_id = $1 GROUP BY ar.id
             ORDER BY max(p.played_at) DESC LIMIT $2)
         SELECT ar.id, ar.name,
             (SELECT count(DISTINCT al.id) FROM albums al JOIN tracks t ON t.album_id = al.id
                 AND t.missing_since IS NULL WHERE al.artist_id = ar.id) AS album_count,
             (SELECT count(*) FROM albums al JOIN tracks t ON t.album_id = al.id
                 AND t.missing_since IS NULL WHERE al.artist_id = ar.id) AS track_count,
             last.last_played_at
         FROM last JOIN artists ar ON ar.id = last.id
         ORDER BY last.last_played_at DESC",
    )
    .bind(user)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_a_track_at_or_under_30_seconds() {
        assert_eq!(required_listen_seconds(Some(30.0)), None);
        assert_eq!(required_listen_seconds(Some(12.0)), None);
    }

    #[test]
    fn asks_for_half_the_length_just_past_the_floor() {
        assert_eq!(required_listen_seconds(Some(31.0)), Some(15.5));
        assert_eq!(required_listen_seconds(Some(200.0)), Some(100.0));
    }

    #[test]
    fn never_asks_for_more_than_four_minutes() {
        assert_eq!(required_listen_seconds(Some(600.0)), Some(240.0));
        assert_eq!(required_listen_seconds(Some(3600.0)), Some(240.0));
    }

    #[test]
    fn unknown_length_demands_the_four_minute_bound() {
        for d in [Some(0.0), None, Some(f64::NAN), Some(-5.0)] {
            assert_eq!(required_listen_seconds(d), Some(240.0));
        }
    }

    #[test]
    fn the_floor_is_on_the_track_not_the_listening() {
        assert_eq!(required_listen_seconds(Some(40.0)), Some(20.0));
    }

    #[test]
    fn dedup_window_is_the_track_length_with_a_floor() {
        assert_eq!(dedup_window_ms(Some(200.0)), 200_000.0);
        assert_eq!(dedup_window_ms(Some(10.0)), 30_000.0);
        assert_eq!(dedup_window_ms(Some(0.0)), 240_000.0);
        assert_eq!(dedup_window_ms(None), 240_000.0);
        assert_eq!(dedup_window_ms(Some(f64::NAN)), 240_000.0);
        assert_eq!(dedup_window_ms(Some(-1.0)), 240_000.0);
    }
}
