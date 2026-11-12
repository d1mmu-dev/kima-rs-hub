//! Podcasts (M6): shows, their episodes and who follows them. Upstream models `Podcast`,
//! `PodcastEpisode`, `PodcastSubscription` (`schema.prisma:471-566`); ids are UUIDv7 (D-016).

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// A stored show.
#[derive(Debug, Clone, PartialEq)]
pub struct Show {
    pub id: Uuid,
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub itunes_id: Option<String>,
    /// Apple category ids, most specific first.
    pub category_ids: Vec<i32>,
    pub language: Option<String>,
    pub explicit: bool,
    pub episode_count: i32,
    pub last_refreshed: OffsetDateTime,
    pub feed_etag: Option<String>,
    pub feed_last_modified: Option<String>,
    pub refresh_error: Option<String>,
    pub refresh_failed_at: Option<OffsetDateTime>,
    pub refresh_failures: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A show to store, as read from its feed (and Apple).
#[derive(Debug, Clone)]
pub struct NewShow<'a> {
    pub feed_url: &'a str,
    pub title: &'a str,
    pub author: Option<&'a str>,
    pub description: Option<&'a str>,
    pub image_url: Option<&'a str>,
    pub itunes_id: Option<&'a str>,
    pub category_ids: &'a [i32],
    pub language: Option<&'a str>,
    pub explicit: bool,
}

/// An episode to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEpisode {
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub audio_url: String,
    pub duration: i32,
    pub published_at: OffsetDateTime,
    pub episode_number: Option<i32>,
    pub season: Option<i32>,
    pub image_url: Option<String>,
    pub file_size: Option<i64>,
    /// `None` stores the column default `audio/mpeg`, as upstream's `@default`.
    pub mime_type: Option<String>,
}

/// Stores a new show. A second show with the same feed URL or Apple id is a unique violation
/// (23505); callers that race resolve it with [`show_by_feed_or_itunes`].
pub async fn insert_show(pool: &PgPool, id: Uuid, s: &NewShow<'_>) -> sqlx::Result<Show> {
    sqlx::query_as!(
        Show,
        "INSERT INTO podcasts (id, feed_url, title, author, description, image_url, itunes_id,
                               category_ids, language, explicit)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING id, feed_url, title, author, description, image_url, itunes_id, category_ids,
                   language, explicit, episode_count, last_refreshed, feed_etag,
                   feed_last_modified, refresh_error, refresh_failed_at, refresh_failures,
                   created_at, updated_at",
        id,
        s.feed_url,
        s.title,
        s.author,
        s.description,
        s.image_url,
        s.itunes_id,
        s.category_ids,
        s.language,
        s.explicit
    )
    .fetch_one(pool)
    .await
}

pub async fn show_by_id(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<Show>> {
    sqlx::query_as!(
        Show,
        "SELECT id, feed_url, title, author, description, image_url, itunes_id, category_ids,
                language, explicit, episode_count, last_refreshed, feed_etag,
                feed_last_modified, refresh_error, refresh_failed_at, refresh_failures,
                created_at, updated_at
         FROM podcasts WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
}

/// The show stored under `feed_url`, else the one with Apple id `itunes_id` (upstream's
/// `findFirst({OR: [{feedUrl}, {itunesId}]})`, `podcasts.ts:739-743`); when both match different
/// shows the feed URL wins (upstream leaves the pick to Postgres).
pub async fn show_by_feed_or_itunes(
    pool: &PgPool,
    feed_url: &str,
    itunes_id: Option<&str>,
) -> sqlx::Result<Option<Show>> {
    sqlx::query_as!(
        Show,
        "SELECT id, feed_url, title, author, description, image_url, itunes_id, category_ids,
                language, explicit, episode_count, last_refreshed, feed_etag,
                feed_last_modified, refresh_error, refresh_failed_at, refresh_failures,
                created_at, updated_at
         FROM podcasts WHERE feed_url = $1 OR ($2::text IS NOT NULL AND itunes_id = $2)
         ORDER BY (feed_url = $1) DESC LIMIT 1",
        feed_url,
        itunes_id
    )
    .fetch_optional(pool)
    .await
}

/// Stores `episodes` of `podcast`; an episode whose guid the show already has is skipped
/// (upstream `createMany({skipDuplicates: true})`). Returns how many were stored.
pub async fn insert_episodes(
    pool: &PgPool,
    podcast: Uuid,
    episodes: &[NewEpisode],
) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let mut stored = 0;
    for e in episodes {
        stored += sqlx::query!(
            "INSERT INTO podcast_episodes (id, podcast_id, guid, title, description, audio_url,
                                           duration, published_at, episode_number, season,
                                           image_url, file_size, mime_type)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     COALESCE($13, 'audio/mpeg'))
             ON CONFLICT (podcast_id, guid) DO NOTHING",
            Uuid::now_v7(),
            podcast,
            e.guid,
            e.title,
            e.description,
            e.audio_url,
            e.duration,
            e.published_at,
            e.episode_number,
            e.season,
            e.image_url,
            e.file_size,
            e.mime_type
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(stored)
}

/// Follows a show; `false` when the user already followed it.
pub async fn subscribe(pool: &PgPool, user: Uuid, podcast: Uuid) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "INSERT INTO podcast_subscriptions (user_id, podcast_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
        user,
        podcast
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Stops following a show; `false` when the user did not follow it.
pub async fn unsubscribe(pool: &PgPool, user: Uuid, podcast: Uuid) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "DELETE FROM podcast_subscriptions WHERE user_id = $1 AND podcast_id = $2",
        user,
        podcast
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn is_subscribed(pool: &PgPool, user: Uuid, podcast: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM podcast_subscriptions
                          WHERE user_id = $1 AND podcast_id = $2) AS "exists!""#,
        user,
        podcast
    )
    .fetch_one(pool)
    .await
}
