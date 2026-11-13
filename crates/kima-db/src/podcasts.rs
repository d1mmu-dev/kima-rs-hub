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
    /// How many episodes the feed listed when the show was stored.
    pub episode_count: i32,
    /// The feed's validators, for the next conditional refresh.
    pub feed_etag: Option<&'a str>,
    pub feed_last_modified: Option<&'a str>,
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
    let mut connection = pool.acquire().await?;
    insert_show_on(&mut connection, id, s).await
}

pub async fn insert_show_on(
    connection: &mut sqlx::PgConnection,
    id: Uuid,
    s: &NewShow<'_>,
) -> sqlx::Result<Show> {
    sqlx::query_as!(
        Show,
        "INSERT INTO podcasts (id, feed_url, title, author, description, image_url, itunes_id,
                               category_ids, language, explicit, episode_count, feed_etag,
                               feed_last_modified)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
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
        s.explicit,
        s.episode_count,
        s.feed_etag,
        s.feed_last_modified
    )
    .fetch_one(connection)
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
    let stored = insert_episodes_on(&mut tx, podcast, episodes).await?;
    tx.commit().await?;
    Ok(stored)
}

pub async fn insert_episodes_on(
    connection: &mut sqlx::PgConnection,
    podcast: Uuid,
    episodes: &[NewEpisode],
) -> sqlx::Result<u64> {
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
        .execute(&mut *connection)
        .await?
        .rows_affected();
    }
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
    let mut tx = pool.begin().await?;
    let done =
        sqlx::query("DELETE FROM podcast_subscriptions WHERE user_id = $1 AND podcast_id = $2")
            .bind(user)
            .bind(podcast)
            .execute(&mut *tx)
            .await?;
    if done.rows_affected() == 0 {
        tx.commit().await?;
        return Ok(false);
    }
    for table in ["podcast_progress", "podcast_downloads"] {
        let query = format!(
            "DELETE FROM {table} WHERE user_id = $1 AND episode_id IN             (SELECT id FROM podcast_episodes WHERE podcast_id = $2)"
        );
        sqlx::query(&query)
            .bind(user)
            .bind(podcast)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM playback_state WHERE user_id = $1 AND playback_type = 'podcast'                  AND left(podcast_id, length($2)) = $2")
        .bind(user).bind(format!("{podcast}:")).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
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

/// Replaces a show's Apple category ids (upstream `podcast.updateMany`, `podcastCatalog.ts:285`).
pub async fn set_category_ids(pool: &PgPool, podcast: Uuid, ids: &[i32]) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE podcasts SET category_ids = $2, updated_at = now() WHERE id = $1",
        podcast,
        ids
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Shows with an Apple id and no categories (`podcastCatalog.ts:298-301`): `(id, itunes_id)`.
pub async fn missing_categories(pool: &PgPool) -> sqlx::Result<Vec<(Uuid, String)>> {
    let rows = sqlx::query!(
        r#"SELECT id, itunes_id AS "itunes_id!" FROM podcasts
           WHERE category_ids = '{}' AND itunes_id IS NOT NULL ORDER BY id"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.itunes_id)).collect())
}

/// A followed show as "for you" weighs it (`podcastCatalog.ts:361-364`).
#[derive(Debug, Clone, PartialEq)]
pub struct Followed {
    pub title: String,
    pub itunes_id: Option<String>,
    pub category_ids: Vec<i32>,
}

/// The shows `user` follows, oldest follow first.
pub async fn followed(pool: &PgPool, user: Uuid) -> sqlx::Result<Vec<Followed>> {
    sqlx::query_as!(
        Followed,
        "SELECT p.title, p.itunes_id, p.category_ids
         FROM podcast_subscriptions s JOIN podcasts p ON p.id = s.podcast_id
         WHERE s.user_id = $1 ORDER BY s.subscribed_at, p.id",
        user
    )
    .fetch_all(pool)
    .await
}

/// The stored `podcastChartCountry` setting as text, if it is a string
/// (`SystemSettings.podcastChartCountry`; the settings route lands in M6 Task 17).
pub async fn chart_country(pool: &PgPool) -> sqlx::Result<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT value #>> '{}' AS "value" FROM settings_kv
           WHERE key = 'podcastChartCountry' AND jsonb_typeof(value) = 'string'"#
    )
    .fetch_optional(pool)
    .await?
    .flatten())
}

/// One in-progress podcast row for `GET /library/recently-listened` (upstream
/// `routes/library/tracks.ts:90-125`): the caller's most recently played episode of a show
/// they have not finished, at most one per show.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InProgressPodcast {
    pub podcast_id: Uuid,
    pub episode_id: Uuid,
    pub name: String,
    pub cover_art: Option<String>,
    pub author: Option<String>,
    /// Rounded percent (0 when the stored duration is not yet known).
    pub progress: i32,
    pub last_played_at: OffsetDateTime,
}

/// `user`'s in-progress episodes, newest `lastPlayedAt` first, deduplicated to the most recent
/// episode of each show (upstream takes `overall_limit * 2` progress rows, dedupes keeping the
/// first/most recent per show, then slices to `ceil(overall_limit / 3)`; `tracks.ts:90-125`).
pub async fn recent_in_progress(
    pool: &PgPool,
    user: Uuid,
    overall_limit: i64,
) -> sqlx::Result<Vec<InProgressPodcast>> {
    let take_rows = overall_limit.saturating_mul(2).max(1);
    let take_shows = overall_limit.saturating_add(2) / 3;
    if take_shows <= 0 {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        "WITH recent AS (
             SELECT p.episode_id, e.podcast_id, p.last_played_at, p.\"current_time\", p.duration
             FROM podcast_progress p JOIN podcast_episodes e ON e.id = p.episode_id
             WHERE p.user_id = $1 AND NOT p.is_finished AND p.\"current_time\" > 0
             ORDER BY p.last_played_at DESC LIMIT $2
         ),
         deduped AS (
             SELECT DISTINCT ON (podcast_id) episode_id, podcast_id, last_played_at,
                    \"current_time\", duration
             FROM recent ORDER BY podcast_id, last_played_at DESC
         )
         SELECT d.podcast_id, d.episode_id, s.title AS name, s.image_url AS cover_art, s.author,
                (CASE WHEN d.duration > 0 THEN round(d.\"current_time\" / d.duration * 100)
                      ELSE 0 END)::int AS progress,
                d.last_played_at
         FROM deduped d JOIN podcasts s ON s.id = d.podcast_id
         ORDER BY d.last_played_at DESC LIMIT $3",
    )
    .bind(user)
    .bind(take_rows)
    .bind(take_shows)
    .fetch_all(pool)
    .await
}

/// A subscribed show in newest-subscription-first order.
#[derive(sqlx::FromRow)]
pub struct FollowedShowRow {
    pub id: Uuid,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub episode_count: i32,
}

/// The caller's most recent episodes of one show with only their own progress.
#[derive(sqlx::FromRow)]
pub struct SummaryRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub duration: i32,
    pub published_at: OffsetDateTime,
    pub image_url: Option<String>,
    pub current_time: Option<f64>,
    pub progress_duration: Option<f64>,
    pub is_finished: Option<bool>,
    pub last_played_at: Option<OffsetDateTime>,
}

pub async fn followed_shows(pool: &PgPool, user: Uuid) -> sqlx::Result<Vec<FollowedShowRow>> {
    sqlx::query_as("SELECT p.id, p.title, p.author, p.description, p.image_url, p.episode_count         FROM podcast_subscriptions s JOIN podcasts p ON p.id = s.podcast_id         WHERE s.user_id = $1 ORDER BY s.subscribed_at DESC, p.id DESC")
        .bind(user).fetch_all(pool).await
}

pub async fn recent_episodes(
    pool: &PgPool,
    user: Uuid,
    show: Uuid,
) -> sqlx::Result<Vec<SummaryRow>> {
    sqlx::query_as("SELECT e.id, e.title, e.description, e.duration, e.published_at, e.image_url,         p.\"current_time\", p.duration AS progress_duration, p.is_finished, p.last_played_at         FROM podcast_episodes e LEFT JOIN podcast_progress p ON p.episode_id = e.id AND p.user_id = $1         WHERE e.podcast_id = $2 ORDER BY e.published_at DESC, e.id DESC LIMIT 5")
        .bind(user).bind(show).fetch_all(pool).await
}

#[derive(sqlx::FromRow)]
pub struct EpisodePageRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub duration: i32,
    pub published_at: OffsetDateTime,
    pub episode_number: Option<i32>,
    pub season: Option<i32>,
    pub image_url: Option<String>,
    pub is_downloaded: bool,
    pub current_time: Option<f64>,
    pub progress_duration: Option<f64>,
    pub is_finished: Option<bool>,
    pub last_played_at: Option<OffsetDateTime>,
}

pub async fn episode_page(
    pool: &PgPool,
    user: Uuid,
    show: Uuid,
    limit: i64,
    offset: i64,
    oldest: bool,
) -> sqlx::Result<(Vec<EpisodePageRow>, i64)> {
    let order = if oldest { "ASC" } else { "DESC" };
    let query = format!(
        "SELECT e.id, e.title, e.description, e.duration, e.published_at, e.episode_number, e.season, e.image_url,         EXISTS(SELECT 1 FROM podcast_downloads d WHERE d.episode_id=e.id AND d.user_id=$1) AS is_downloaded,         p.\"current_time\", p.duration AS progress_duration, p.is_finished, p.last_played_at         FROM podcast_episodes e LEFT JOIN podcast_progress p ON p.episode_id=e.id AND p.user_id=$1         WHERE e.podcast_id=$2 ORDER BY e.published_at {order}, e.id {order} LIMIT $3 OFFSET $4"
    );
    let rows = sqlx::query_as(&query)
        .bind(user)
        .bind(show)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let total = sqlx::query_scalar("SELECT count(*) FROM podcast_episodes WHERE podcast_id=$1")
        .bind(show)
        .fetch_one(pool)
        .await?;
    Ok((rows, total))
}

/// Next by release date and id, skipping episodes this listener finished. The outer option
/// distinguishes an unknown current episode from the end of the show.
pub async fn next_episode(
    pool: &PgPool,
    user: Uuid,
    show: Uuid,
    current: Uuid,
) -> sqlx::Result<Option<Option<SummaryRow>>> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM podcast_episodes WHERE id=$1 AND podcast_id=$2)",
    )
    .bind(current)
    .bind(show)
    .fetch_one(pool)
    .await?;
    if !exists {
        return Ok(None);
    }
    let next = sqlx::query_as("SELECT e.id, e.title, e.description, e.duration, e.published_at, e.image_url,         p.\"current_time\", p.duration AS progress_duration, p.is_finished, p.last_played_at         FROM podcast_episodes e JOIN podcast_episodes cur ON cur.id=$3 AND cur.podcast_id=$2         LEFT JOIN podcast_progress p ON p.episode_id=e.id AND p.user_id=$1         WHERE e.podcast_id=$2 AND (e.published_at,e.id) > (cur.published_at,cur.id)         AND NOT COALESCE(p.is_finished, false) ORDER BY e.published_at,e.id LIMIT 1")
        .bind(user).bind(show).bind(current).fetch_optional(pool).await?;
    Ok(Some(next))
}

#[derive(sqlx::FromRow)]
pub struct ListeningRow {
    pub podcast_id: Uuid,
    pub podcast_title: String,
    pub podcast_image: Option<String>,
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub duration: i32,
    pub published_at: OffsetDateTime,
    pub image_url: Option<String>,
    pub current_time: Option<f64>,
    pub progress_duration: Option<f64>,
    pub is_finished: Option<bool>,
    pub last_played_at: Option<OffsetDateTime>,
}

pub async fn listening(
    pool: &PgPool,
    user: Uuid,
) -> sqlx::Result<(Vec<ListeningRow>, Vec<ListeningRow>)> {
    let columns = "s.id AS podcast_id,s.title AS podcast_title,s.image_url AS podcast_image,         e.id,e.title,e.description,e.duration,e.published_at,e.image_url,         p.\"current_time\",p.duration AS progress_duration,p.is_finished,p.last_played_at".to_string();
    let query = format!(
        "SELECT {columns} FROM podcast_progress p         JOIN podcast_episodes e ON e.id=p.episode_id         JOIN podcasts s ON s.id=e.podcast_id JOIN podcast_subscriptions sub ON sub.podcast_id=s.id AND sub.user_id=$1         WHERE p.user_id=$1 AND NOT p.is_finished AND p.\"current_time\">0 ORDER BY p.last_played_at DESC LIMIT 12"
    );
    let in_progress = sqlx::query_as(&query).bind(user).fetch_all(pool).await?;
    let query = format!(
        "WITH newest AS (SELECT DISTINCT ON (e.podcast_id) e.id FROM podcast_episodes e         JOIN podcast_subscriptions sub ON sub.podcast_id=e.podcast_id AND sub.user_id=$1         ORDER BY e.podcast_id,e.published_at DESC,e.id)         SELECT {columns} FROM newest n JOIN podcast_episodes e ON e.id=n.id JOIN podcasts s ON s.id=e.podcast_id         LEFT JOIN podcast_progress p ON p.episode_id=e.id AND p.user_id=$1         WHERE p.user_id IS NULL ORDER BY e.published_at DESC LIMIT 20"
    );
    let new_episodes = sqlx::query_as(&query).bind(user).fetch_all(pool).await?;
    Ok((in_progress, new_episodes))
}

/// Current playback position and duration for one episode of one show.
/// `None` when the user has no stored progress for this episode yet.
pub async fn progress(
    pool: &PgPool,
    user: Uuid,
    episode: Uuid,
) -> sqlx::Result<Option<(f64, f64)>> {
    sqlx::query_as("SELECT \"current_time\", duration FROM podcast_progress WHERE user_id = $1 AND episode_id = $2")
        .bind(user)
        .bind(episode)
        .fetch_optional(pool)
        .await
}

/// Store or update playback progress for one episode of one show.
pub async fn save_progress(
    pool: &PgPool,
    user: Uuid,
    episode: Uuid,
    current_time: f64,
    duration: f64,
    is_finished: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO podcast_progress (user_id, episode_id, \"current_time\", duration, is_finished, last_played_at) VALUES ($1, $2, $3, $4, $5, now()) ON CONFLICT (user_id, episode_id) DO UPDATE SET \"current_time\" = $3, duration = $4, is_finished = $5, last_played_at = now()",
    )
    .bind(user)
    .bind(episode)
    .bind(current_time)
    .bind(duration)
    .bind(is_finished)
    .execute(pool)
    .await?;
    Ok(())
}

/// Remove playback progress for one episode of one show.
pub async fn clear_progress(pool: &PgPool, user: Uuid, episode: Uuid) -> sqlx::Result<u64> {
    let done = sqlx::query("DELETE FROM podcast_progress WHERE user_id = $1 AND episode_id = $2")
        .bind(user)
        .bind(episode)
        .execute(pool)
        .await?;
    Ok(done.rows_affected())
}

/// Where an episode's audio lives and what to tell the browser it is (`podcasts.ts:916-1084`:
/// `Content-Type: mimeType || audio/mpeg`, independent of what the host's own response says).
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeAudio {
    pub audio_url: String,
    pub mime_type: Option<String>,
}

pub async fn episode_audio(pool: &PgPool, episode: Uuid) -> sqlx::Result<Option<EpisodeAudio>> {
    sqlx::query_as!(
        EpisodeAudio,
        "SELECT audio_url, mime_type FROM podcast_episodes WHERE id = $1",
        episode
    )
    .fetch_optional(pool)
    .await
}

/// The episode's own `image_url` (Task 15, `routes/podcasts/cover.rs`); `None` when the
/// episode does not exist or has none, same as upstream's `episode?.imageUrl`.
pub async fn episode_cover_url(pool: &PgPool, episode: Uuid) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT image_url FROM podcast_episodes WHERE id = $1",
        episode
    )
    .fetch_optional(pool)
    .await
    .map(Option::flatten)
}

// M6 Task 12: the episode cache (`kima_podcast::cache`). The cached file on disk is shared by
// every follower of the show, but `podcast_downloads` keeps one row per (user, episode), as
// upstream's `PodcastDownload` does; helpers below that validate or expire the cache act on
// every row for the episode, not just one user's.

/// A download record's stored size, to validate the cached file against (D-204).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DownloadRecord {
    pub file_size_mb: f64,
}

/// Any one download record for `episode` (upstream's `findFirst`): the file is shared, so one
/// row's recorded size validates it for everyone. `None` when nobody has downloaded it yet.
pub async fn download_record(pool: &PgPool, episode: Uuid) -> sqlx::Result<Option<DownloadRecord>> {
    sqlx::query_as!(
        DownloadRecord,
        "SELECT file_size_mb FROM podcast_downloads WHERE episode_id = $1
         ORDER BY downloaded_at LIMIT 1",
        episode
    )
    .fetch_optional(pool)
    .await
}

/// Creates or refreshes `user`'s download record for `episode` with the file it now holds.
pub async fn upsert_download(
    pool: &PgPool,
    user: Uuid,
    episode: Uuid,
    local_path: &str,
    file_size_mb: f64,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO podcast_downloads (user_id, episode_id, local_path, file_size_mb, downloaded_at, last_accessed_at)
         VALUES ($1, $2, $3, $4, now(), now())
         ON CONFLICT (user_id, episode_id) DO UPDATE
         SET local_path = $3, file_size_mb = $4, downloaded_at = now(), last_accessed_at = now()",
        user,
        episode,
        local_path,
        file_size_mb
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks `episode`'s cached file accessed now, for every follower's download record (the file
/// is shared, so a read by any of them keeps it alive for all).
pub async fn touch_download(pool: &PgPool, episode: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE podcast_downloads SET last_accessed_at = now() WHERE episode_id = $1",
        episode
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Deletes every download record for `episode` (its cached file no longer validates).
/// Returns how many rows were removed.
pub async fn delete_downloads_for_episode(pool: &PgPool, episode: Uuid) -> sqlx::Result<u64> {
    let done = sqlx::query!(
        "DELETE FROM podcast_downloads WHERE episode_id = $1",
        episode
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// Removes one (user, episode) download record (cleanup, after its file is deleted).
pub async fn delete_download_row(pool: &PgPool, user: Uuid, episode: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM podcast_downloads WHERE user_id = $1 AND episode_id = $2",
        user,
        episode
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Stores the audio file's own byte count on its episode row (the cached download's size,
/// which can differ from a feed's declared enclosure length).
pub async fn set_episode_file_size(pool: &PgPool, episode: Uuid, bytes: i64) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE podcast_episodes SET file_size = $2 WHERE id = $1",
        episode,
        bytes
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Stores the audio's measured length on its episode row (`recordMeasuredLength`). Returns
/// whether the episode still exists.
pub async fn set_episode_duration(
    pool: &PgPool,
    episode: Uuid,
    seconds: i32,
) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "UPDATE podcast_episodes SET duration = $2 WHERE id = $1",
        episode,
        seconds
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// A download record whose cached file [`cleanup`] is about to delete.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpiredDownload {
    pub user_id: Uuid,
    pub episode_id: Uuid,
    pub local_path: String,
    pub file_size_mb: f64,
}

/// Every download record not accessed since before `cutoff` (`cleanupExpiredCache`'s 30 days).
pub async fn expired_downloads(
    pool: &PgPool,
    cutoff: OffsetDateTime,
) -> sqlx::Result<Vec<ExpiredDownload>> {
    sqlx::query_as!(
        ExpiredDownload,
        "SELECT user_id, episode_id, local_path, file_size_mb FROM podcast_downloads
         WHERE last_accessed_at < $1",
        cutoff
    )
    .fetch_all(pool)
    .await
}

// M6 Task 18: Subsonic podcast views (`crates/kima-subsonic/src/routes/podcasts.rs`). These
// helpers are scoped to what Subsonic needs; Task 9's eventual job-queued refresh/sweep is a
// separate, larger piece (D-205) and is not implemented here.

/// A followed show as a Subsonic `channel` needs it (`getPodcasts`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Channel {
    pub id: Uuid,
    pub feed_url: String,
    pub title: String,
    pub description: Option<String>,
    pub image_url: Option<String>,
}

/// The shows `user` follows, as Subsonic channels, oldest follow first (same order as
/// [`followed`]).
pub async fn followed_channels(pool: &PgPool, user: Uuid) -> sqlx::Result<Vec<Channel>> {
    sqlx::query_as!(
        Channel,
        "SELECT p.id, p.feed_url, p.title, p.description, p.image_url
         FROM podcast_subscriptions s JOIN podcasts p ON p.id = s.podcast_id
         WHERE s.user_id = $1 ORDER BY s.subscribed_at, p.id",
        user
    )
    .fetch_all(pool)
    .await
}

/// An episode as a Subsonic `podcastEpisode` needs it: its show's title too
/// (`mapPodcastEpisode`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SubsonicEpisode {
    pub id: Uuid,
    pub podcast_id: Uuid,
    pub podcast_title: String,
    pub title: String,
    pub description: Option<String>,
    pub audio_url: String,
    pub duration: i32,
    pub published_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub file_size: Option<i64>,
    pub mime_type: Option<String>,
}

const SUBSONIC_EPISODE_COLS: &str = "e.id, e.podcast_id, p.title AS podcast_title, e.title, \
     e.description, e.audio_url, e.duration, e.published_at, e.created_at, e.file_size, \
     e.mime_type FROM podcast_episodes e JOIN podcasts p ON p.id = e.podcast_id";

/// Up to `limit` newest episodes of `show` (`getPodcasts`: at most 200).
pub async fn episodes_for_channel(
    pool: &PgPool,
    show: Uuid,
    limit: i64,
) -> sqlx::Result<Vec<SubsonicEpisode>> {
    let query = format!(
        "SELECT {SUBSONIC_EPISODE_COLS} WHERE e.podcast_id = $1 \
         ORDER BY e.published_at DESC, e.id DESC LIMIT $2"
    );
    sqlx::query_as(&query)
        .bind(show)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// `user`'s newest episodes across every followed show (`getNewestPodcasts`).
pub async fn newest_episodes(
    pool: &PgPool,
    user: Uuid,
    limit: i64,
) -> sqlx::Result<Vec<SubsonicEpisode>> {
    let query = format!(
        "SELECT {SUBSONIC_EPISODE_COLS} \
         JOIN podcast_subscriptions s ON s.podcast_id = e.podcast_id AND s.user_id = $1 \
         ORDER BY e.published_at DESC, e.id DESC LIMIT $2"
    );
    sqlx::query_as(&query)
        .bind(user)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// One episode by id, with its show's title (`getPodcastEpisode`, the Subsonic decision and
/// stream branches).
pub async fn subsonic_episode_by_id(
    pool: &PgPool,
    id: Uuid,
) -> sqlx::Result<Option<SubsonicEpisode>> {
    let query = format!("SELECT {SUBSONIC_EPISODE_COLS} WHERE e.id = $1");
    sqlx::query_as(&query).bind(id).fetch_optional(pool).await
}

/// Applies a successful feed refresh to a show: metadata, counts and validators; clears any
/// stored refresh failure (Subsonic `refreshPodcasts`; D-205's job-queued sweep is a separate,
/// later piece and may supersede this).
pub async fn apply_refresh(pool: &PgPool, id: Uuid, s: &NewShow<'_>) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE podcasts SET title = $2, author = $3, description = $4, image_url = $5,
                language = $6, explicit = $7, episode_count = $8, feed_etag = $9,
                feed_last_modified = $10, last_refreshed = now(), updated_at = now(),
                refresh_error = NULL, refresh_failed_at = NULL, refresh_failures = 0
         WHERE id = $1",
        id,
        s.title,
        s.author,
        s.description,
        s.image_url,
        s.language,
        s.explicit,
        s.episode_count,
        s.feed_etag,
        s.feed_last_modified
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A 304 to a conditional refresh: the caller's copy is current, so only the clock moves
/// (upstream `refreshPodcastFeed`'s `{newEpisodesCount:0}` branch).
pub async fn bump_last_refreshed(pool: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE podcasts SET last_refreshed = now(), updated_at = now() WHERE id = $1",
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}
