//! LRCLIB (spec §14.1, upstream `services/lrclib.ts`): `GET /get` with the track's title, album
//! artist, album and duration. A 404 is an absence; anything else not a 2xx is a failure. Not
//! cached in `http_cache`: `track_lyrics` is its cache (spec §7.4, §14.2).
//!
//! The answer is untrusted: missing or null fields are `None`, an `id` that is not an `i32` is a
//! `BAD_RESPONSE`, and each lyrics side is cut to [`kima_db::lyrics::MAX_LYRICS_BYTES`].

use std::time::Duration;

use kima_db::lyrics::capped;
use kima_net::{Host, Net, Outcome, Req};
use serde::Deserialize;

/// The longest wait for LRCLIB's slot and limiter cell; the request itself then has the source's
/// 5 s timeout (spec §14.2 step 4). A longer wait is `Transient("BUSY")`.
pub const MAX_WAIT: Duration = Duration::from_secs(5);

/// What LRCLIB knows of a track. Both sides may be `None` (an instrumental).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub id: i32,
    #[serde(default)]
    pub plain_lyrics: Option<String>,
    #[serde(default)]
    pub synced_lyrics: Option<String>,
}

fn cap(s: Option<String>) -> Option<String> {
    match s {
        Some(s) if s.len() > kima_db::lyrics::MAX_LYRICS_BYTES => {
            capped(Some(&s)).map(str::to_string)
        }
        other => other,
    }
}

/// `GET /get?track_name=&artist_name=&album_name=&duration=` (only these four, percent-encoded).
pub async fn get(
    net: &Net,
    title: &str,
    artist: &str,
    album: &str,
    duration_secs: i32,
) -> Outcome<Answer> {
    let req = Req::get(Host::Lrclib, "/get")
        .query("track_name", title)
        .query("artist_name", artist)
        .query("album_name", album)
        .query("duration", duration_secs.to_string())
        .absent_on_404()
        .max_wait(MAX_WAIT)
        .no_cache();
    net.get_json::<Answer>(req).await.map(|a| Answer {
        id: a.id,
        plain_lyrics: cap(a.plain_lyrics),
        synced_lyrics: cap(a.synced_lyrics),
    })
}
