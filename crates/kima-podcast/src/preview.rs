//! Remembering a show's sample episode for `GET /preview/:itunesId/snippet` (Task 14,
//! `podcasts.ts:326-433`): the feed is read for a sample at most once a day; the proxy itself
//! only ever needs the sample's audio URL and mime type, never its display fields (those are
//! built fresh from the feed by the `/preview/:itunesId` handler and never round-tripped
//! through this cache).

use std::time::Duration;

use kima_db::PgPool;
use kima_db::http_cache::{self, CacheOutcome};
use serde::{Deserialize, Serialize};

use crate::feed::FeedEpisode;

/// `PREVIEW_EPISODES` (`podcasts.ts:398`): the newest episodes shown in a preview.
pub const PREVIEW_EPISODES: usize = 20;
/// `PREVIEW_RSS_TIMEOUT_MS` (`podcasts.ts:43`): how long a preview's feed read may take.
pub const PREVIEW_RSS_TIMEOUT: Duration = Duration::from_secs(8);
/// How long a show's sample pick is remembered (`podcasts.ts:411-433`: "remembered 1 day").
const SNIPPET_TTL: Duration = Duration::from_secs(86_400);

/// What proxying the sample episode's audio needs, remembered a day
/// (`podcasts:snippet:v1:<itunesId>` in `http_cache`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snippet {
    pub audio_url: String,
    pub mime_type: String,
}

impl Snippet {
    pub fn from_episode(ep: &FeedEpisode) -> Snippet {
        Snippet {
            audio_url: ep.audio_url.clone(),
            mime_type: ep.mime_type.clone(),
        }
    }
}

/// Whether a show's feed was already read for a sample.
#[derive(Debug, Clone, PartialEq)]
pub enum Remembered {
    /// Read, and it has a sample.
    Some(Snippet),
    /// Read, and it has none.
    None,
}

fn snippet_key(itunes_id: &str) -> String {
    format!("podcasts:snippet:v1:{itunes_id}")
}

/// `None` when the feed has not been read for a sample yet (a fresh read is needed); a row of
/// a shape this build no longer reads is treated the same way.
pub async fn remembered_snippet(
    pool: &PgPool,
    itunes_id: &str,
) -> sqlx::Result<Option<Remembered>> {
    let Some((outcome, body)) = http_cache::get(pool, &snippet_key(itunes_id)).await? else {
        return Ok(None);
    };
    Ok(match outcome {
        CacheOutcome::Absent => Some(Remembered::None),
        CacheOutcome::Answered => serde_json::from_value(body).ok().map(Remembered::Some),
    })
}

/// Remembers a show's sample episode (or that it has none) for a day.
pub async fn remember_snippet(
    pool: &PgPool,
    itunes_id: &str,
    snippet: Option<&Snippet>,
) -> sqlx::Result<()> {
    let (outcome, body) = match snippet {
        Some(s) => (
            CacheOutcome::Answered,
            serde_json::to_value(s).unwrap_or(serde_json::Value::Null),
        ),
        None => (CacheOutcome::Absent, serde_json::Value::Null),
    };
    http_cache::put(
        pool,
        &snippet_key(itunes_id),
        "podcasts",
        &format!("snippet {itunes_id}"),
        outcome,
        &body,
        SNIPPET_TTL,
    )
    .await
}

/// The newest episodes of a feed, newest first, trimmed to [`PREVIEW_EPISODES`].
pub fn newest_episodes(episodes: &[FeedEpisode]) -> Vec<&FeedEpisode> {
    let mut sorted: Vec<&FeedEpisode> = episodes.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.published_at));
    sorted.truncate(PREVIEW_EPISODES);
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn ep(title: &str, published_at: time::OffsetDateTime) -> FeedEpisode {
        FeedEpisode {
            guid: title.into(),
            title: title.into(),
            description: None,
            audio_url: format!("https://example.invalid/{title}.mp3"),
            duration: 0,
            published_at,
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: "audio/mpeg".into(),
        }
    }

    #[test]
    fn newest_episodes_sorts_descending_and_trims() {
        let episodes = vec![
            ep("old", datetime!(2026 - 01 - 01 0:00 UTC)),
            ep("new", datetime!(2026 - 03 - 01 0:00 UTC)),
            ep("mid", datetime!(2026 - 02 - 01 0:00 UTC)),
        ];
        let titles: Vec<&str> = newest_episodes(&episodes)
            .into_iter()
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(titles, vec!["new", "mid", "old"]);
    }

    #[test]
    fn newest_episodes_keeps_at_most_the_preview_limit() {
        let many: Vec<FeedEpisode> = (0..(PREVIEW_EPISODES + 5))
            .map(|i| {
                ep(
                    &i.to_string(),
                    datetime!(2026 - 01 - 01 0:00 UTC) + time::Duration::days(i as i64),
                )
            })
            .collect();
        assert_eq!(newest_episodes(&many).len(), PREVIEW_EPISODES);
    }

    #[test]
    fn snippet_key_is_versioned_per_show() {
        assert_eq!(snippet_key("123"), "podcasts:snippet:v1:123");
        assert_ne!(snippet_key("123"), snippet_key("456"));
    }
}
