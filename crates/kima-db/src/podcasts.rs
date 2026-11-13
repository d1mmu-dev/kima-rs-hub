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
    /// The channel's `ai-content` tag (D-200); `None` is no signal.
    pub ai_feed: Option<bool>,
    /// An admin's `ai`/`human` decision for the show.
    pub ai_override: Option<String>,
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

// M6 Task 8 (D-200): AI-content labels. The resolution mirrors
// `kima_api_types::podcasts::{show_label, episode_label}` (a test compares them over every input
// combination) so filtering and counting happen in the query.

/// A show's effective label (`ai`/`human`/`unknown`) over the `podcasts` row aliased `p`: the
/// admin override, else the channel tag, else `unknown`.
pub fn show_label_sql(p: &str) -> String {
    format!(
        "(CASE WHEN {p}.ai_override IS NOT NULL THEN {p}.ai_override \
         WHEN {p}.ai_feed THEN 'ai' WHEN NOT {p}.ai_feed THEN 'human' ELSE 'unknown' END)"
    )
}

/// The source of [`show_label_sql`]: `admin`, `feed-show`, or NULL.
pub fn show_source_sql(p: &str) -> String {
    format!(
        "(CASE WHEN {p}.ai_override IS NOT NULL THEN 'admin' \
         WHEN {p}.ai_feed IS NOT NULL THEN 'feed-show' END)"
    )
}

/// An episode's effective label over `podcast_episodes` aliased `e` and its show aliased `p`:
/// the episode's admin override, else its item tag, else the show's label.
pub fn episode_label_sql(e: &str, p: &str) -> String {
    format!(
        "(CASE WHEN {e}.ai_override IS NOT NULL THEN {e}.ai_override \
         WHEN {e}.ai_feed THEN 'ai' WHEN NOT {e}.ai_feed THEN 'human' ELSE {} END)",
        show_label_sql(p)
    )
}

/// The source of [`episode_label_sql`]: `admin`, `feed-episode`, the show's source, or NULL.
pub fn episode_source_sql(e: &str, p: &str) -> String {
    format!(
        "(CASE WHEN {e}.ai_override IS NOT NULL THEN 'admin' \
         WHEN {e}.ai_feed IS NOT NULL THEN 'feed-episode' ELSE {} END)",
        show_source_sql(p)
    )
}

/// `<label> AS ai_label, <source> AS ai_label_source` for a show.
fn show_label_cols(p: &str) -> String {
    format!(
        "{} AS ai_label, {} AS ai_label_source",
        show_label_sql(p),
        show_source_sql(p)
    )
}

/// `<label> AS ai_label, <source> AS ai_label_source` for an episode.
fn episode_label_cols(e: &str, p: &str) -> String {
    format!(
        "{} AS ai_label, {} AS ai_label_source",
        episode_label_sql(e, p),
        episode_source_sql(e, p)
    )
}

/// Stores the `ai-content` tags a feed read carries (subscribe and every non-304 refresh,
/// D-206): the channel tag on the show, and each listed episode's item tag by guid (including
/// `None` when the tag is gone). An episode no longer in the feed keeps its last tag. The first
/// item of a duplicated guid wins, as in [`insert_episodes_on`].
pub async fn set_feed_labels_on(
    connection: &mut sqlx::PgConnection,
    podcast: Uuid,
    channel: Option<bool>,
    items: &[(String, Option<bool>)],
) -> sqlx::Result<()> {
    sqlx::query("UPDATE podcasts SET ai_feed = $2 WHERE id = $1 AND ai_feed IS DISTINCT FROM $2")
        .bind(podcast)
        .bind(channel)
        .execute(&mut *connection)
        .await?;
    let mut seen = std::collections::HashSet::new();
    let (mut guids, mut tags) = (Vec::new(), Vec::new());
    for (guid, tag) in items {
        if seen.insert(guid.as_str()) {
            guids.push(guid.clone());
            tags.push(*tag);
        }
    }
    sqlx::query(
        "UPDATE podcast_episodes e SET ai_feed = v.ai
         FROM unnest($2::text[], $3::bool[]) AS v(guid, ai)
         WHERE e.podcast_id = $1 AND e.guid = v.guid AND e.ai_feed IS DISTINCT FROM v.ai",
    )
    .bind(podcast)
    .bind(&guids)
    .bind(&tags)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// A resolved label as the queries answer it: `(label, source)`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LabelRow {
    pub ai_label: String,
    pub ai_label_source: Option<String>,
}

/// Sets or clears (`None`) an admin's label for a show; `None` when there is no such show.
pub async fn set_show_override(
    pool: &PgPool,
    podcast: Uuid,
    label: Option<&str>,
    by: Uuid,
) -> sqlx::Result<Option<LabelRow>> {
    let query = format!(
        "UPDATE podcasts p SET ai_override = $2,
                ai_override_by = CASE WHEN $2::text IS NULL THEN NULL ELSE $3 END,
                ai_override_at = CASE WHEN $2::text IS NULL THEN NULL ELSE now() END
         WHERE p.id = $1 RETURNING {}",
        show_label_cols("p")
    );
    sqlx::query_as(&query)
        .bind(podcast)
        .bind(label)
        .bind(by)
        .fetch_optional(pool)
        .await
}

/// Sets or clears an admin's label for one episode of `podcast`; `None` when the episode is
/// not one of that show's.
pub async fn set_episode_override(
    pool: &PgPool,
    podcast: Uuid,
    episode: Uuid,
    label: Option<&str>,
    by: Uuid,
) -> sqlx::Result<Option<LabelRow>> {
    let query = format!(
        "UPDATE podcast_episodes e SET ai_override = $3,
                ai_override_by = CASE WHEN $3::text IS NULL THEN NULL ELSE $4 END,
                ai_override_at = CASE WHEN $3::text IS NULL THEN NULL ELSE now() END
         FROM podcasts p
         WHERE e.id = $2 AND e.podcast_id = $1 AND p.id = e.podcast_id
         RETURNING {}",
        episode_label_cols("e", "p")
    );
    sqlx::query_as(&query)
        .bind(podcast)
        .bind(episode)
        .bind(label)
        .bind(by)
        .fetch_optional(pool)
        .await
}

/// Remembers the channel tag a preview read for an Apple show (spec S3).
pub async fn remember_feed_label(
    pool: &PgPool,
    itunes_id: &str,
    ai_feed: Option<bool>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO podcast_feed_labels (itunes_id, ai_feed) VALUES ($1, $2)
         ON CONFLICT (itunes_id) DO UPDATE SET ai_feed = $2, read_at = now()",
    )
    .bind(itunes_id)
    .bind(ai_feed)
    .execute(pool)
    .await?;
    Ok(())
}

/// Labels for Apple shows by id (spec S3): a stored show's label, else the channel tag a
/// preview remembered, else nothing (the caller answers `unknown`).
pub async fn labels_for_itunes(
    pool: &PgPool,
    itunes_ids: &[String],
) -> sqlx::Result<std::collections::HashMap<String, LabelRow>> {
    if itunes_ids.is_empty() {
        return Ok(Default::default());
    }
    let stored = show_label_sql("p");
    let query = format!(
        "SELECT ids.id AS itunes_id,
                COALESCE(CASE WHEN p.id IS NOT NULL THEN {stored} END,
                         CASE WHEN f.ai_feed THEN 'ai' WHEN NOT f.ai_feed THEN 'human' END,
                         'unknown') AS ai_label,
                CASE WHEN p.id IS NOT NULL THEN {ss}
                     WHEN f.ai_feed IS NOT NULL THEN 'feed-show' END AS ai_label_source
         FROM unnest($1::text[]) AS ids(id)
         LEFT JOIN podcasts p ON p.itunes_id = ids.id
         LEFT JOIN podcast_feed_labels f ON f.itunes_id = ids.id",
        ss = show_source_sql("p"),
    );
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(&query)
        .bind(itunes_ids)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(id, ai_label, ai_label_source)| {
            (
                id,
                LabelRow {
                    ai_label,
                    ai_label_source,
                },
            )
        })
        .collect())
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
                   created_at, updated_at, ai_feed, ai_override",
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
                created_at, updated_at, ai_feed, ai_override
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
                created_at, updated_at, ai_feed, ai_override
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
    /// The in-progress episode's label (D-200).
    pub ai_label: String,
    pub ai_label_source: Option<String>,
}

/// `user`'s in-progress episodes, newest `lastPlayedAt` first, deduplicated to the most recent
/// episode of each show (upstream takes `overall_limit * 2` progress rows, dedupes keeping the
/// first/most recent per show, then slices to `ceil(overall_limit / 3)`; `tracks.ts:90-125`).
///
/// With `hide_ai` the AI-labelled episodes are left out before the row limit and the per-show
/// deduplication, so a show whose latest played episode is AI shows its latest visible one
/// (D-207). The second value is how many AI-labelled rows the unfiltered answer has (0 when not
/// hiding).
pub async fn recent_in_progress(
    pool: &PgPool,
    user: Uuid,
    overall_limit: i64,
    hide_ai: bool,
) -> sqlx::Result<(Vec<InProgressPodcast>, i64)> {
    let take_rows = overall_limit.saturating_mul(2).max(1);
    let take_shows = overall_limit.saturating_add(2) / 3;
    if take_shows <= 0 {
        return Ok((Vec::new(), 0));
    }
    let query = format!(
        "WITH recent AS (
             SELECT p.episode_id, e.podcast_id, p.last_played_at, p.\"current_time\", p.duration
             FROM podcast_progress p JOIN podcast_episodes e ON e.id = p.episode_id
             JOIN podcasts s ON s.id = e.podcast_id
             WHERE p.user_id = $1 AND NOT p.is_finished AND p.\"current_time\" > 0
               AND (NOT $4 OR {label} <> 'ai')
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
                d.last_played_at, {cols}
         FROM deduped d JOIN podcasts s ON s.id = d.podcast_id
         JOIN podcast_episodes e ON e.id = d.episode_id
         ORDER BY d.last_played_at DESC LIMIT $3",
        label = episode_label_sql("e", "s"),
        cols = episode_label_cols("e", "s")
    );
    let run = |hide: bool| {
        sqlx::query_as::<_, InProgressPodcast>(&query)
            .bind(user)
            .bind(take_rows)
            .bind(take_shows)
            .bind(hide)
            .fetch_all(pool)
    };
    if !hide_ai {
        return Ok((run(false).await?, 0));
    }
    let hidden = run(false)
        .await?
        .iter()
        .filter(|r| r.ai_label == "ai")
        .count() as i64;
    Ok((run(true).await?, hidden))
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
    pub ai_label: String,
    pub ai_label_source: Option<String>,
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
    pub ai_label: String,
    pub ai_label_source: Option<String>,
}

pub async fn followed_shows(pool: &PgPool, user: Uuid) -> sqlx::Result<Vec<FollowedShowRow>> {
    let query = format!(
        "SELECT p.id, p.title, p.author, p.description, p.image_url, p.episode_count, {}
         FROM podcast_subscriptions s JOIN podcasts p ON p.id = s.podcast_id
         WHERE s.user_id = $1 ORDER BY s.subscribed_at DESC, p.id DESC",
        show_label_cols("p")
    );
    sqlx::query_as(&query).bind(user).fetch_all(pool).await
}

pub async fn recent_episodes(
    pool: &PgPool,
    user: Uuid,
    show: Uuid,
) -> sqlx::Result<Vec<SummaryRow>> {
    let query = format!(
        "SELECT e.id, e.title, e.description, e.duration, e.published_at, e.image_url,
                p.\"current_time\", p.duration AS progress_duration, p.is_finished,
                p.last_played_at, {}
         FROM podcast_episodes e JOIN podcasts s ON s.id = e.podcast_id
         LEFT JOIN podcast_progress p ON p.episode_id = e.id AND p.user_id = $1
         WHERE e.podcast_id = $2 ORDER BY e.published_at DESC, e.id DESC LIMIT 5",
        episode_label_cols("e", "s")
    );
    sqlx::query_as(&query)
        .bind(user)
        .bind(show)
        .fetch_all(pool)
        .await
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
    pub ai_label: String,
    pub ai_label_source: Option<String>,
}

/// One page of a show's episodes (Task 7, filtered by Task 8).
pub struct EpisodePage {
    pub rows: Vec<EpisodePageRow>,
    /// Episodes the page draws from: the visible ones when `hide_ai`.
    pub total: i64,
    /// AI-labelled episodes `hide_ai` removed before paging (0 when not hiding).
    pub hidden: i64,
}

pub async fn episode_page(
    pool: &PgPool,
    user: Uuid,
    show: Uuid,
    limit: i64,
    offset: i64,
    oldest: bool,
    hide_ai: bool,
) -> sqlx::Result<EpisodePage> {
    let order = if oldest { "ASC" } else { "DESC" };
    let label = episode_label_sql("e", "s");
    let query = format!(
        "SELECT e.id, e.title, e.description, e.duration, e.published_at, e.episode_number,
                e.season, e.image_url,
                EXISTS(SELECT 1 FROM podcast_downloads d WHERE d.episode_id=e.id AND d.user_id=$1)
                    AS is_downloaded,
                p.\"current_time\", p.duration AS progress_duration, p.is_finished,
                p.last_played_at, {cols}
         FROM podcast_episodes e JOIN podcasts s ON s.id = e.podcast_id
         LEFT JOIN podcast_progress p ON p.episode_id=e.id AND p.user_id=$1
         WHERE e.podcast_id=$2 AND (NOT $5 OR {label} <> 'ai')
         ORDER BY e.published_at {order}, e.id {order} LIMIT $3 OFFSET $4",
        cols = episode_label_cols("e", "s"),
    );
    let rows = sqlx::query_as(&query)
        .bind(user)
        .bind(show)
        .bind(limit)
        .bind(offset)
        .bind(hide_ai)
        .fetch_all(pool)
        .await?;
    let counts = format!(
        "SELECT count(*), count(*) FILTER (WHERE {label} = 'ai')
         FROM podcast_episodes e JOIN podcasts s ON s.id = e.podcast_id WHERE e.podcast_id=$1"
    );
    let (all, ai): (i64, i64) = sqlx::query_as(&counts).bind(show).fetch_one(pool).await?;
    Ok(if hide_ai {
        EpisodePage {
            rows,
            total: all - ai,
            hidden: ai,
        }
    } else {
        EpisodePage {
            rows,
            total: all,
            hidden: 0,
        }
    })
}

/// Next by release date and id, skipping episodes this listener finished. The outer option
/// distinguishes an unknown current episode from the end of the show.
pub async fn next_episode(
    pool: &PgPool,
    user: Uuid,
    show: Uuid,
    current: Uuid,
    hide_ai: bool,
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
    // With `hide_ai`, AI-labelled episodes are skipped like finished ones (spec S4).
    let query = format!(
        "SELECT e.id, e.title, e.description, e.duration, e.published_at, e.image_url,
                p.\"current_time\", p.duration AS progress_duration, p.is_finished,
                p.last_played_at, {cols}
         FROM podcast_episodes e JOIN podcasts s ON s.id = e.podcast_id
         JOIN podcast_episodes cur ON cur.id=$3 AND cur.podcast_id=$2
         LEFT JOIN podcast_progress p ON p.episode_id=e.id AND p.user_id=$1
         WHERE e.podcast_id=$2 AND (e.published_at,e.id) > (cur.published_at,cur.id)
         AND NOT COALESCE(p.is_finished, false) AND (NOT $4 OR {label} <> 'ai')
         ORDER BY e.published_at,e.id LIMIT 1",
        cols = episode_label_cols("e", "s"),
        label = episode_label_sql("e", "s"),
    );
    let next = sqlx::query_as(&query)
        .bind(user)
        .bind(show)
        .bind(current)
        .bind(hide_ai)
        .fetch_optional(pool)
        .await?;
    Ok(Some(next))
}

#[derive(sqlx::FromRow)]
pub struct ListeningRow {
    pub podcast_id: Uuid,
    pub podcast_title: String,
    pub podcast_image: Option<String>,
    pub podcast_ai_label: String,
    pub podcast_ai_label_source: Option<String>,
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
    pub ai_label: String,
    pub ai_label_source: Option<String>,
}

/// `/podcasts/listening`: the followed shows' in-progress episodes (12 most recently played)
/// and each followed show's newest episode the caller has not started (20 newest).
///
/// With `hide_ai` the AI-labelled episodes are left out before the limits and before picking a
/// show's newest episode, so a show whose newest episode is AI offers its newest visible one
/// (D-207). The third value is how many AI-labelled rows the unfiltered lists have (0 when not
/// hiding).
pub async fn listening(
    pool: &PgPool,
    user: Uuid,
    hide_ai: bool,
) -> sqlx::Result<(Vec<ListeningRow>, Vec<ListeningRow>, i64)> {
    let columns = format!(
        "s.id AS podcast_id, s.title AS podcast_title, s.image_url AS podcast_image,
         {} AS podcast_ai_label, {} AS podcast_ai_label_source,
         e.id, e.title, e.description, e.duration, e.published_at, e.image_url,
         p.\"current_time\", p.duration AS progress_duration, p.is_finished, p.last_played_at,
         {}",
        show_label_sql("s"),
        show_source_sql("s"),
        episode_label_cols("e", "s")
    );
    let label = episode_label_sql("e", "s");
    let in_progress = format!(
        "SELECT {columns} FROM podcast_progress p \
         JOIN podcast_episodes e ON e.id=p.episode_id \
         JOIN podcasts s ON s.id=e.podcast_id \
         JOIN podcast_subscriptions sub ON sub.podcast_id=s.id AND sub.user_id=$1 \
         WHERE p.user_id=$1 AND NOT p.is_finished AND p.\"current_time\">0 \
           AND (NOT $2 OR {label} <> 'ai') \
         ORDER BY p.last_played_at DESC LIMIT 12"
    );
    let new_episodes = format!(
        "WITH newest AS (SELECT DISTINCT ON (e.podcast_id) e.id FROM podcast_episodes e \
         JOIN podcasts s ON s.id=e.podcast_id \
         JOIN podcast_subscriptions sub ON sub.podcast_id=e.podcast_id AND sub.user_id=$1 \
         WHERE NOT $2 OR {label} <> 'ai' \
         ORDER BY e.podcast_id,e.published_at DESC,e.id) \
         SELECT {columns} FROM newest n JOIN podcast_episodes e ON e.id=n.id \
         JOIN podcasts s ON s.id=e.podcast_id \
         LEFT JOIN podcast_progress p ON p.episode_id=e.id AND p.user_id=$1 \
         WHERE p.user_id IS NULL ORDER BY e.published_at DESC LIMIT 20"
    );
    async fn run(
        pool: &PgPool,
        query: &str,
        user: Uuid,
        hide: bool,
    ) -> sqlx::Result<Vec<ListeningRow>> {
        sqlx::query_as(query)
            .bind(user)
            .bind(hide)
            .fetch_all(pool)
            .await
    }
    let hidden = if hide_ai {
        let ai =
            |rows: Vec<ListeningRow>| rows.iter().filter(|r| r.ai_label == "ai").count() as i64;
        ai(run(pool, &in_progress, user, false).await?)
            + ai(run(pool, &new_episodes, user, false).await?)
    } else {
        0
    };
    Ok((
        run(pool, &in_progress, user, hide_ai).await?,
        run(pool, &new_episodes, user, hide_ai).await?,
        hidden,
    ))
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
    pub ai_label: String,
}

/// The shows `user` follows, as Subsonic channels, oldest follow first (same order as
/// [`followed`]); AI-labelled shows left out when `hide_ai`.
pub async fn followed_channels(
    pool: &PgPool,
    user: Uuid,
    hide_ai: bool,
) -> sqlx::Result<Vec<Channel>> {
    let label = show_label_sql("p");
    let query = format!(
        "SELECT p.id, p.feed_url, p.title, p.description, p.image_url, {label} AS ai_label
         FROM podcast_subscriptions s JOIN podcasts p ON p.id = s.podcast_id
         WHERE s.user_id = $1 AND (NOT $2 OR {label} <> 'ai')
         ORDER BY s.subscribed_at, p.id"
    );
    sqlx::query_as(&query)
        .bind(user)
        .bind(hide_ai)
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
    pub ai_label: String,
}

fn subsonic_episode_cols() -> String {
    format!(
        "e.id, e.podcast_id, p.title AS podcast_title, e.title, e.description, e.audio_url, \
         e.duration, e.published_at, e.created_at, e.file_size, e.mime_type, {} AS ai_label \
         FROM podcast_episodes e JOIN podcasts p ON p.id = e.podcast_id",
        episode_label_sql("e", "p")
    )
}

/// Up to `limit` newest episodes of `show` (`getPodcasts`: at most 200). With `hide_ai` the
/// AI-labelled ones are left out before the limit, so the answer stays full (D-208).
pub async fn episodes_for_channel(
    pool: &PgPool,
    show: Uuid,
    limit: i64,
    hide_ai: bool,
) -> sqlx::Result<Vec<SubsonicEpisode>> {
    let query = format!(
        "SELECT {} WHERE e.podcast_id = $1 AND (NOT $3 OR {} <> 'ai') \
         ORDER BY e.published_at DESC, e.id DESC LIMIT $2",
        subsonic_episode_cols(),
        episode_label_sql("e", "p")
    );
    sqlx::query_as(&query)
        .bind(show)
        .bind(limit)
        .bind(hide_ai)
        .fetch_all(pool)
        .await
}

/// `user`'s newest episodes across every followed show (`getNewestPodcasts`), AI-labelled ones
/// left out before the limit when `hide_ai` (D-208).
pub async fn newest_episodes(
    pool: &PgPool,
    user: Uuid,
    limit: i64,
    hide_ai: bool,
) -> sqlx::Result<Vec<SubsonicEpisode>> {
    let query = format!(
        "SELECT {} \
         JOIN podcast_subscriptions s ON s.podcast_id = e.podcast_id AND s.user_id = $1 \
         WHERE NOT $3 OR {} <> 'ai' \
         ORDER BY e.published_at DESC, e.id DESC LIMIT $2",
        subsonic_episode_cols(),
        episode_label_sql("e", "p")
    );
    sqlx::query_as(&query)
        .bind(user)
        .bind(limit)
        .bind(hide_ai)
        .fetch_all(pool)
        .await
}

/// One episode by id, with its show's title (`getPodcastEpisode`, the Subsonic decision and
/// stream branches).
pub async fn subsonic_episode_by_id(
    pool: &PgPool,
    id: Uuid,
) -> sqlx::Result<Option<SubsonicEpisode>> {
    let query = format!("SELECT {} WHERE e.id = $1", subsonic_episode_cols());
    sqlx::query_as(&query).bind(id).fetch_optional(pool).await
}
