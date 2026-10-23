//! A track's lyrics for `GET /api/library/tracks/{id}/lyrics` (spec §14.2, upstream
//! `routes/library/tracks.ts:401-512`): the stored row, else LRCLIB, with a 30-day cache of
//! LRCLIB's "no such track".
//!
//! No transaction or lock is held across the request. Concurrent asks for one track may both
//! ask LRCLIB; their writes are upserts that never replace an `embedded` row, and a `none` never
//! replaces an answer, so the row stays consistent.

use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::lyrics::{self as db, Lyrics};
use kima_net::{Net, Outcome};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::lrclib;

/// How long LRCLIB's "no such track" is believed (spec §14.2 step 3).
pub const NONE_FRESH_FOR: time::Duration = time::Duration::days(30);

/// What the route answers: `{plainLyrics, syncedLyrics, source}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsAnswer {
    pub plain: Option<String>,
    pub synced: Option<String>,
    /// `embedded`, `lrclib` or `none`.
    pub source: String,
}

impl LyricsAnswer {
    fn none() -> LyricsAnswer {
        LyricsAnswer {
            plain: None,
            synced: None,
            source: "none".into(),
        }
    }

    fn of(row: Option<Lyrics>) -> LyricsAnswer {
        row.map_or_else(LyricsAnswer::none, |r| LyricsAnswer {
            plain: r.plain,
            synced: r.synced,
            source: r.source,
        })
    }
}

/// The track's lyrics (spec §14.2 steps 2-5); `None` when there is no such track. A track flagged
/// missing answers like a present one.
///
/// With identification off (`identify` or the client's own switch), a limiter wait over 5 s, or a
/// failed request, the stored values (or nulls) are answered and nothing is written. Only an
/// answer (`lrclib`) or a 404 (`none`) is stored.
pub async fn for_track(
    pool: &PgPool,
    net: &Net,
    identify: &IdentifySwitch,
    track: Uuid,
) -> sqlx::Result<Option<LyricsAnswer>> {
    let Some((title, artist, album, duration)) = db::lookup_fields(pool, track).await? else {
        return Ok(None);
    };
    let stored = db::get(pool, track).await?;
    if let Some(row) = &stored {
        if row.source != "none" {
            return Ok(Some(LyricsAnswer::of(stored)));
        }
        if row.fetched_at > OffsetDateTime::now_utc() - NONE_FRESH_FOR {
            return Ok(Some(LyricsAnswer::none()));
        }
    }
    if !identify.enabled() || !net.enabled() {
        return Ok(Some(LyricsAnswer::of(stored)));
    }
    let written = match lrclib::get(net, &title, &artist, &album, duration).await {
        Outcome::Answered(a) => {
            let wrote = db::upsert_lrclib(
                pool,
                track,
                a.plain_lyrics.as_deref(),
                a.synced_lyrics.as_deref(),
                a.id,
            )
            .await?;
            wrote.then(|| LyricsAnswer {
                plain: a.plain_lyrics,
                synced: a.synced_lyrics,
                source: "lrclib".into(),
            })
        }
        Outcome::Absent(_) => db::upsert_none(pool, track).await?.then(LyricsAnswer::none),
        other => {
            // The code only: the request URL carries the track's metadata.
            tracing::debug!(%track, code = ?other.code(), "LRCLIB lookup failed; answering stored lyrics");
            return Ok(Some(LyricsAnswer::of(stored)));
        }
    };
    match written {
        Some(answer) => Ok(Some(answer)),
        // Another writer got there first (the scanner's embedded lyrics, a concurrent answer)
        // or the track went away: answer what is stored now.
        None => Ok(Some(LyricsAnswer::of(db::get(pool, track).await?))),
    }
}
