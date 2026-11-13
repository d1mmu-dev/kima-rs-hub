//! Refresh scheduling for followed podcast feeds.

use kima_jobs::queue;

use crate::catalog::{Catalog, Priority};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// A feed endpoint explicitly reported that the requested resource is gone.
#[derive(Debug, thiserror::Error)]
pub enum FeedFailure {
    #[error("feed is gone: {0:?}")]
    Gone(kima_net::Evidence),
    #[error("feed is down: {0:?}")]
    Down(kima_net::Failure),
    #[error("feed address is blocked: {0:?}")]
    Blocked(kima_net::Evidence),
    #[error("feed unavailable: {0:?}")]
    Absent(kima_net::Evidence),
    #[error("permanent feed fetch failure: {0:?}")]
    Permanent(kima_net::Failure),
    #[error("feed rate limited until {until}")]
    RateLimited { until: time::OffsetDateTime },
}

/// Database errors remain distinct from feed errors, including failures to record one.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Feed(#[from] FeedFailure),
    #[error("feed processing failed: {0:#}")]
    Processing(anyhow::Error),
    #[error("feed refresh failed: {original}; recording failure failed: {report}")]
    Reporting {
        original: Box<RefreshError>,
        report: sqlx::Error,
    },
}

#[derive(Debug)]
pub struct RefreshResult {
    pub new_episodes: u64,
    pub total_episodes: i32,
    /// Set when the show had no categories: its Apple id and its feed's category names, for
    /// [`refresh`] to resolve (upstream assigns them only when there are none).
    pub uncategorized: Option<(Option<String>, Vec<String>)>,
}

#[derive(Debug)]
pub enum RefreshOutcome {
    Applied(RefreshResult),
    Superseded,
    Missing,
}

impl RefreshError {
    /// What the show row records as its `refresh_error` (D-205).
    fn record_text(&self) -> &'static str {
        match self {
            RefreshError::Feed(FeedFailure::Gone(_)) => "The show's feed is gone",
            RefreshError::Feed(FeedFailure::Down(_) | FeedFailure::RateLimited { .. }) => {
                "The show's feed did not answer"
            }
            RefreshError::Feed(FeedFailure::Blocked(_)) => "The show's feed address is not public",
            RefreshError::Feed(FeedFailure::Absent(_)) => "The show's feed refused the request",
            RefreshError::Feed(FeedFailure::Permanent(_)) => "The show's feed could not be fetched",
            RefreshError::Processing(_) => "The show's feed could not be read",
            RefreshError::Database(_) | RefreshError::Reporting { .. } => "Feed refresh failed",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FenceState {
    Current,
    Superseded,
    Missing,
}

#[derive(sqlx::FromRow)]
struct RefreshStart {
    feed_url: String,
    feed_etag: Option<String>,
    feed_last_modified: Option<String>,
    itunes_id: Option<String>,
    category_ids: Vec<i32>,
    episode_count: i32,
}

async fn lock_current(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    token: Uuid,
) -> sqlx::Result<FenceState> {
    let stored: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT refresh_token FROM podcasts WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(match stored {
        None => FenceState::Missing,
        Some(Some(current)) if current == token => FenceState::Current,
        _ => FenceState::Superseded,
    })
}

/// A refresh as people and jobs ask for it: [`refresh_one`], then, when the show had no
/// categories, the ones Apple or its feed names, in the background lane (upstream
/// `refreshPodcastFeed` -> `assignCategories(..., "background")`). Category trouble never
/// fails the refresh (`Catalog::assign_categories` logs it).
pub async fn refresh(catalog: &Catalog, id: Uuid) -> Result<RefreshOutcome, RefreshError> {
    let outcome = refresh_one(catalog.pool(), catalog.net(), id).await?;
    if let RefreshOutcome::Applied(RefreshResult {
        uncategorized: Some((itunes, names)),
        ..
    }) = &outcome
    {
        catalog
            .assign_categories(id, itunes.as_deref(), names, Priority::Background)
            .await;
    }
    Ok(outcome)
}

/// The queue worker's entry point for one `podcasts` job. A job whose show succeeded a refresh
/// after the job was queued (`last_refreshed > jobs.created_at`) is stale intent, retries
/// included (they keep the job's row), and answers `Superseded` without asking the feed. An
/// unknown job or show is `Missing`. Otherwise it is [`refresh`].
pub async fn refresh_job(
    catalog: &Catalog,
    id: Uuid,
    job: Uuid,
) -> Result<RefreshOutcome, RefreshError> {
    let overtaken: Option<bool> = sqlx::query_scalar(
        "SELECT p.last_refreshed > j.created_at FROM podcasts p, jobs j
         WHERE p.id = $1 AND j.id = $2",
    )
    .bind(id)
    .bind(job)
    .fetch_optional(catalog.pool())
    .await?;
    match overtaken {
        None => Ok(RefreshOutcome::Missing),
        Some(true) => Ok(RefreshOutcome::Superseded),
        Some(false) => refresh(catalog, id).await,
    }
}

pub async fn refresh_one(
    pool: &PgPool,
    net: &kima_net::Net,
    id: Uuid,
) -> Result<RefreshOutcome, RefreshError> {
    let token = Uuid::now_v7();
    let show: Option<RefreshStart> = sqlx::query_as("UPDATE podcasts SET refresh_token = $2 WHERE id = $1 RETURNING feed_url, feed_etag, feed_last_modified, itunes_id, category_ids, episode_count")
        .bind(id).bind(token).fetch_optional(pool).await?;
    let Some(show) = show else {
        return Ok(RefreshOutcome::Missing);
    };
    let result = refresh_inner(pool, net, id, token, &show).await;
    // Every feed failure is checked against the fence: an attempt a newer refresh has
    // overtaken answers `Superseded`, whatever its own answer was. The current attempt's
    // failure, `Gone` included, is recorded on the show with a text per kind (D-205).
    if let Some(text) = result
        .as_ref()
        .err()
        .filter(|e| !matches!(e, RefreshError::Database(_)))
        .map(RefreshError::record_text)
    {
        let report = async {
            let mut tx = pool.begin().await?;
            let state = lock_current(&mut tx, id, token).await?;
            if state == FenceState::Current {
                sqlx::query("UPDATE podcasts SET refresh_error = $2, refresh_failed_at = now(), refresh_failures = refresh_failures + 1 WHERE id = $1").bind(id).bind(text).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok::<_, sqlx::Error>(state)
        }.await;
        match report {
            Ok(FenceState::Superseded) => return Ok(RefreshOutcome::Superseded),
            Ok(FenceState::Missing) => return Ok(RefreshOutcome::Missing),
            Ok(FenceState::Current) => {}
            Err(report_error) => {
                return match result {
                    Err(original) => Err(RefreshError::Reporting {
                        original: Box::new(original),
                        report: report_error,
                    }),
                    Ok(_) => unreachable!("reporting is only attempted after failure"),
                };
            }
        }
    }
    result
}

async fn refresh_inner(
    pool: &PgPool,
    net: &kima_net::Net,
    id: Uuid,
    token: Uuid,
    show: &RefreshStart,
) -> Result<RefreshOutcome, RefreshError> {
    use kima_db::podcasts::{self, NewEpisode, NewShow};
    use kima_net::{Conditional, FeedFetch, Outcome};
    let response = net
        .get_feed(
            &show.feed_url,
            Conditional {
                etag: show.feed_etag.as_deref(),
                last_modified: show.feed_last_modified.as_deref(),
            },
            std::time::Duration::from_secs(30),
        )
        .await;
    let body = match response {
        Outcome::Answered(body) => body,
        Outcome::Absent(evidence)
            if evidence.0.get("refused").and_then(|s| s.as_str()) == Some("NOT_PUBLIC") =>
        {
            return Err(FeedFailure::Blocked(evidence).into());
        }
        Outcome::Absent(evidence)
            if evidence.0.get("status").and_then(|s| s.as_u64()) == Some(404) =>
        {
            return Err(FeedFailure::Gone(evidence).into());
        }
        Outcome::Absent(evidence) => return Err(FeedFailure::Absent(evidence).into()),
        Outcome::Transient(failure) => return Err(FeedFailure::Down(failure).into()),
        Outcome::Permanent(failure) => return Err(FeedFailure::Permanent(failure).into()),
        Outcome::RateLimited { until } => return Err(FeedFailure::RateLimited { until }.into()),
    };
    match body {
        FeedFetch::NotModified => {
            let mut tx = pool.begin().await?;
            match lock_current(&mut tx, id, token).await? {
                FenceState::Current => {}
                FenceState::Superseded => return Ok(RefreshOutcome::Superseded),
                FenceState::Missing => return Ok(RefreshOutcome::Missing),
            }
            sqlx::query("UPDATE podcasts SET last_refreshed = now(), updated_at = now(), refresh_error = NULL, refresh_failed_at = NULL, refresh_failures = 0 WHERE id = $1")
                .bind(id).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(RefreshOutcome::Applied(RefreshResult {
                new_episodes: 0,
                total_episodes: show.episode_count,
                uncategorized: None,
            }))
        }
        FeedFetch::Body {
            text,
            etag,
            last_modified,
        } => {
            let parsed = crate::feed::parse(&text, time::OffsetDateTime::now_utc())
                .map_err(|error| RefreshError::Processing(error.into()))?;
            let episodes: Vec<_> = parsed
                .episodes
                .into_iter()
                .map(|e| NewEpisode {
                    guid: e.guid,
                    title: e.title,
                    description: e.description,
                    audio_url: e.audio_url,
                    duration: e.duration,
                    published_at: e.published_at,
                    episode_number: e.episode_number,
                    season: e.season,
                    image_url: e.image_url,
                    file_size: e.file_size,
                    mime_type: Some(e.mime_type),
                })
                .collect();
            let episode_count = i32::try_from(episodes.len())
                .map_err(|error| RefreshError::Processing(error.into()))?;
            let input = NewShow {
                feed_url: &show.feed_url,
                title: &parsed.show.title,
                author: parsed.show.author.as_deref(),
                description: parsed.show.description.as_deref(),
                image_url: parsed.show.image_url.as_deref(),
                itunes_id: show.itunes_id.as_deref(),
                category_ids: &show.category_ids,
                language: parsed.show.language.as_deref(),
                explicit: parsed.show.explicit,
                episode_count,
                feed_etag: etag.as_deref(),
                feed_last_modified: last_modified.as_deref(),
            };
            let mut tx = pool.begin().await?;
            match lock_current(&mut tx, id, token).await? {
                FenceState::Current => {}
                FenceState::Superseded => return Ok(RefreshOutcome::Superseded),
                FenceState::Missing => return Ok(RefreshOutcome::Missing),
            }
            let stored = podcasts::insert_episodes_on(&mut tx, id, &episodes).await?;
            sqlx::query("UPDATE podcasts SET title = $2, author = $3, description = $4, image_url = $5, language = $6, explicit = $7, episode_count = $8, feed_etag = $9, feed_last_modified = $10, last_refreshed = now(), updated_at = now(), refresh_error = NULL, refresh_failed_at = NULL, refresh_failures = 0 WHERE id = $1")
                .bind(id).bind(input.title).bind(input.author).bind(input.description)
                .bind(input.image_url).bind(input.language).bind(input.explicit)
                .bind(input.episode_count).bind(input.feed_etag).bind(input.feed_last_modified)
                .execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(RefreshOutcome::Applied(RefreshResult {
                new_episodes: stored,
                total_episodes: episode_count,
                uncategorized: show
                    .category_ids
                    .is_empty()
                    .then(|| (show.itunes_id.clone(), parsed.show.category_names.clone())),
            }))
        }
    }
}

/// Requeues every followed show with a recorded refresh failure right now, ignoring the
/// sweep's hourly dispatch backoff (an explicit admin "retry failed and blocked" action, not a
/// background tick). An unfollowed show's failure is left for the owner to unsubscribe or
/// resubscribe, same as [`sweep`]. A show already queued or running is left alone.
pub async fn retry_failed(pool: &PgPool) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let shows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT p.id FROM podcasts p\n         WHERE p.refresh_error IS NOT NULL\n         AND EXISTS (SELECT 1 FROM podcast_subscriptions s WHERE s.podcast_id = p.id)\n         ORDER BY p.id FOR UPDATE OF p SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut queued = 0;
    for id in shows {
        let key = format!("podcast-{id}");
        let active: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM jobs WHERE queue = 'podcasts' AND state IN ('queued', 'running') AND payload ->> 'key' = $1)")
            .bind(&key).fetch_one(&mut *tx).await?;
        if active {
            continue;
        }
        if queue::enqueue_unless_queued_with_key_tx(
            &mut tx,
            "podcasts",
            json!({"podcast_id": id, "key": key}),
            3,
            "key",
        )
        .await?
        .is_some()
        {
            sqlx::query("UPDATE podcasts SET refresh_attempted_at = now() WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            queued += 1;
        }
    }
    tx.commit().await?;
    Ok(queued)
}

/// Atomically claim stale followed shows and enqueue each refresh. A queued duplicate is kept;
/// failed jobs do not block later sweeps. A crash cannot leave a claimed show without a job.
pub async fn sweep(pool: &PgPool) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    // Backoff on the LATER of the last confirmed success and the last dispatch attempt: a
    // feed whose last attempt failed keeps an old `last_refreshed` forever, so gating on that
    // column alone would re-dispatch it on every sweep tick instead of waiting an hour between
    // attempts. `GREATEST` ignores a NULL `refresh_attempted_at` (never attempted).
    let shows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT p.id FROM podcasts p\n         WHERE GREATEST(p.last_refreshed, p.refresh_attempted_at) < now() - interval '1 hour'\n         AND EXISTS (SELECT 1 FROM podcast_subscriptions s WHERE s.podcast_id = p.id)\n         ORDER BY p.id FOR UPDATE OF p SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut queued = 0;
    for id in shows {
        let key = format!("podcast-{id}");
        // The podcast row lock serializes competing sweeps across processes. Both queued
        // and running jobs count, while failed jobs remain eligible for retry.
        let active: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM jobs WHERE queue = 'podcasts' AND state IN ('queued', 'running') AND payload ->> 'key' = $1)")
            .bind(&key).fetch_one(&mut *tx).await?;
        if active {
            continue;
        }
        if queue::enqueue_unless_queued_with_key_tx(
            &mut tx,
            "podcasts",
            json!({"podcast_id": id, "key": key}),
            3,
            "key",
        )
        .await?
        .is_some()
        {
            // Record the attempt now, independent of its eventual outcome: this is what the
            // backoff above reads, so a failed dispatch still waits an hour before retrying.
            sqlx::query("UPDATE podcasts SET refresh_attempted_at = now() WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            queued += 1;
        }
    }
    tx.commit().await?;
    Ok(queued)
}
