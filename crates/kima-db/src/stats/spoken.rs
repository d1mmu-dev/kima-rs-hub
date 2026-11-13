//! Podcast listening totals for `GET /stats` (upstream `services/stats/spoken.ts:54-85`).
//! Audiobooks stay at zero (D-175) until M6b wires Audiobookshelf progress in.

use sqlx::PgPool;
use uuid::Uuid;

/// `spoken.podcasts` (upstream `spokenStats`'s `casts`/`subs`/`backlog` queries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PodcastSpoken {
    pub seconds: i64,
    pub finished: i64,
    pub in_progress: i64,
    pub subscriptions: i64,
    /// Episodes of followed shows this user has never opened.
    pub backlog: i64,
}

pub async fn podcast_spoken(pool: &PgPool, user: Uuid) -> sqlx::Result<PodcastSpoken> {
    let totals = sqlx::query!(
        r#"SELECT coalesce(sum("current_time"), 0)::bigint AS "seconds!",
                  count(*) FILTER (WHERE is_finished)::bigint AS "finished!",
                  count(*) FILTER (WHERE NOT is_finished)::bigint AS "in_progress!"
           FROM podcast_progress WHERE user_id = $1"#,
        user
    )
    .fetch_one(pool)
    .await?;
    let subscriptions = sqlx::query_scalar!(
        r#"SELECT count(*)::bigint AS "n!" FROM podcast_subscriptions WHERE user_id = $1"#,
        user
    )
    .fetch_one(pool)
    .await?;
    let backlog = sqlx::query_scalar!(
        r#"SELECT count(*)::bigint AS "n!"
           FROM podcast_subscriptions s
           JOIN podcast_episodes e ON e.podcast_id = s.podcast_id
           LEFT JOIN podcast_progress p ON p.episode_id = e.id AND p.user_id = $1
           WHERE s.user_id = $1 AND p.user_id IS NULL"#,
        user
    )
    .fetch_one(pool)
    .await?;
    Ok(PodcastSpoken {
        seconds: totals.seconds,
        finished: totals.finished,
        in_progress: totals.in_progress,
        subscriptions,
        backlog,
    })
}
