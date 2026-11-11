//! Jobs on the `library` queue (concurrency 1): reconciles, single-file updates, organizer runs
//! and autotag applies and undos. Payloads are `{"kind":"reconcile"}`,
//! `{"kind":"reread_all"}`, `{"kind":"path","path":"<rel>"}`,
//! `{"kind":"organize","runId":"<uuid>","action":"preview"|"apply"|"undo"|"switch-root"}`,
//! `{"kind":"autotag-apply","runId":"<uuid>"}` and `{"kind":"autotag-undo","runId":"<uuid>"}`.

use std::sync::Arc;

use futures::future::BoxFuture;
use kima_db::PgPool;
use kima_jobs::queue;
use kima_jobs::worker::{self, Handler, JobError};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

use crate::autotag::apply as autotag;
use crate::autotag::undo as autotag_undo;
use crate::organize::apply::{self as organize, Action};
use crate::scan::{self, LibraryCtx, PathOutcome};

pub const QUEUE: &str = "library";
/// Library jobs are idempotent; a few retries cover a database hiccup.
pub const MAX_ATTEMPTS: i32 = 3;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LibraryJob {
    Reconcile,
    #[serde(rename = "reread_all")]
    RereadAll,
    Path {
        path: String,
    },
    Organize {
        #[serde(rename = "runId")]
        run_id: Uuid,
        action: Action,
    },
    #[serde(rename = "autotag-apply")]
    AutotagApply {
        #[serde(rename = "runId")]
        run_id: Uuid,
    },
    #[serde(rename = "autotag-undo")]
    AutotagUndo {
        #[serde(rename = "runId")]
        run_id: Uuid,
    },
}

/// Queues a reconcile unless one is already waiting.
pub async fn enqueue_reconcile(pool: &PgPool) -> sqlx::Result<Option<Uuid>> {
    queue::enqueue_unless_pending(
        pool,
        QUEUE,
        serde_json::json!({ "kind": "reconcile" }),
        MAX_ATTEMPTS,
    )
    .await
}

/// Queues a reconcile for the watcher's safety net (overflow, error, root back). Dedupes against
/// queued reconciles only: a running one walked the tree before the lost events, so it still
/// gets one follow-up.
pub async fn enqueue_reconcile_followup(pool: &PgPool) -> sqlx::Result<Option<Uuid>> {
    queue::enqueue_unless_queued_with_key(
        pool,
        QUEUE,
        serde_json::json!({ "kind": "reconcile" }),
        MAX_ATTEMPTS,
        "kind",
    )
    .await
}

/// Queues a reconcile, or returns the id of the one already waiting or running.
pub async fn request_reconcile(pool: &PgPool) -> sqlx::Result<Uuid> {
    queue::enqueue_or_pending(
        pool,
        QUEUE,
        "reconcile",
        serde_json::json!({ "kind": "reconcile" }),
        MAX_ATTEMPTS,
    )
    .await
}

/// Starts exactly one queued/running re-file job; a second request reports a conflict.
/// The queue's advisory enqueue lock makes concurrent requests single-winner.
pub async fn request_refile(pool: &PgPool) -> sqlx::Result<bool> {
    Ok(queue::enqueue_unless_pending(
        pool,
        QUEUE,
        serde_json::json!({ "kind": "reread_all" }),
        MAX_ATTEMPTS,
    )
    .await?
    .is_some())
}

/// Queues a single-file update for `rel` (relative to the library root), unless an update for
/// the same file is already waiting (not yet running). Returns the new id, if one was queued.
pub async fn enqueue_path(pool: &PgPool, rel: &str) -> sqlx::Result<Option<Uuid>> {
    queue::enqueue_unless_queued_with_key(
        pool,
        QUEUE,
        serde_json::json!({ "kind": "path", "path": rel }),
        MAX_ATTEMPTS,
        "path",
    )
    .await
}

/// Runs one library job. Returns whether it added, updated, re-pointed, restored or marked
/// missing any track. A malformed payload fails for good; any other error is retried. Once
/// `cancel` fires, a reconcile stops between folders and an organizer run between rows.
pub async fn run(
    ctx: &LibraryCtx,
    payload: &serde_json::Value,
    cancel: &CancellationToken,
) -> Result<bool, JobError> {
    run_attempt(ctx, payload, cancel, false).await
}

/// [`run`], told whether this is the job's final attempt (`attempts >= max_attempts`): an
/// autotag apply or undo that fails then settles its run instead of leaving it `writing` or
/// `undoing`.
pub async fn run_attempt(
    ctx: &LibraryCtx,
    payload: &serde_json::Value,
    cancel: &CancellationToken,
    final_attempt: bool,
) -> Result<bool, JobError> {
    let job: LibraryJob = serde_json::from_value(payload.clone())
        .map_err(|e| JobError::Permanent(format!("invalid library job payload: {e}")))?;
    match job {
        LibraryJob::Reconcile => {
            let r = scan::reconcile_cancellable(ctx, cancel)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?;
            info!(
                added = r.added,
                updated = r.updated,
                repointed = r.repointed,
                missing = r.missing,
                restored = r.restored,
                purged = r.purged,
                unchanged = r.unchanged,
                failed = r.failed.len(),
                "library reconcile finished"
            );
            Ok(r.added + r.updated + r.repointed + r.restored + r.missing > 0)
        }
        LibraryJob::RereadAll => {
            let r = scan::refile_cancellable(ctx, cancel)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?;
            info!(
                songs_read = r.tags_read,
                files_failed = r.failed.len(),
                added = r.added,
                updated = r.updated,
                repointed = r.repointed,
                missing = r.missing,
                restored = r.restored,
                purged = r.purged,
                "library re-file finished"
            );
            Ok(r.added + r.updated + r.repointed + r.restored + r.missing > 0)
        }
        LibraryJob::Path { path } => {
            let outcome = scan::update_path(ctx, &path)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?;
            match &outcome {
                // A file that cannot be read is reported, not retried: the next change event
                // (or reconcile) picks it up again.
                PathOutcome::Failed(reason) => {
                    info!(%path, %reason, "library file could not be read");
                }
                PathOutcome::RootUnavailable => {
                    tracing::warn!(%path, "library root unavailable (unmounted?); file left as is");
                }
                outcome => tracing::debug!(%path, ?outcome, "library file updated"),
            }
            Ok(matches!(
                outcome,
                PathOutcome::Added
                    | PathOutcome::Updated
                    | PathOutcome::Repointed
                    | PathOutcome::MarkedMissing
            ))
        }
        LibraryJob::Organize { run_id, action } => {
            organize::run_job_cancellable(ctx, run_id, action, cancel)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?;
            Ok(false)
        }
        LibraryJob::AutotagApply { run_id } => {
            autotag::run_job(ctx, run_id, cancel, final_attempt).await
        }
        LibraryJob::AutotagUndo { run_id } => {
            autotag_undo::run_job(ctx, run_id, cancel, final_attempt).await
        }
    }
}

/// Called with the pool after a library job that changed tracks (see [`run`]).
pub type OnChange = Arc<dyn Fn(PgPool) -> BoxFuture<'static, ()> + Send + Sync>;

/// The `library` queue handler, dispatching on the payload's `kind`.
pub fn handler(ctx: LibraryCtx) -> Handler {
    handler_with(ctx, Arc::new(|_pool| Box::pin(async {})))
}

/// [`handler`], calling `on_change` after each job that changed tracks.
pub fn handler_with(ctx: LibraryCtx, on_change: OnChange) -> Handler {
    let ctx = Arc::new(ctx);
    worker::handler_ctx(move |job, job_ctx| {
        let (ctx, on_change) = (ctx.clone(), on_change.clone());
        async move {
            let last = job.attempts >= job.max_attempts;
            if run_attempt(&ctx, &job.payload, &job_ctx.cancel, last).await? {
                on_change(ctx.pool.clone()).await;
            }
            Ok(())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payloads_parse() {
        let parse = |v| serde_json::from_value::<LibraryJob>(v).unwrap();
        assert_eq!(parse(json!({"kind": "reconcile"})), LibraryJob::Reconcile);
        assert_eq!(parse(json!({"kind": "reread_all"})), LibraryJob::RereadAll);
        assert_eq!(
            parse(json!({"kind": "path", "path": "A/b.mp3"})),
            LibraryJob::Path {
                path: "A/b.mp3".into()
            }
        );
        let run = Uuid::nil();
        assert_eq!(
            parse(json!({"kind": "organize", "runId": run, "action": "switch-root"})),
            LibraryJob::Organize {
                run_id: run,
                action: Action::SwitchRoot
            }
        );
        assert!(
            serde_json::from_value::<LibraryJob>(json!({"kind": "organize", "runId": run}))
                .is_err()
        );
        assert_eq!(
            parse(json!({"kind": "autotag-apply", "runId": run})),
            LibraryJob::AutotagApply { run_id: run }
        );
        assert_eq!(
            parse(json!({"kind": "autotag-undo", "runId": run})),
            LibraryJob::AutotagUndo { run_id: run }
        );
        assert!(serde_json::from_value::<LibraryJob>(json!({"kind": "autotag-apply"})).is_err());
        assert!(serde_json::from_value::<LibraryJob>(json!({"kind": "nope"})).is_err());
        assert!(serde_json::from_value::<LibraryJob>(json!({"kind": "path"})).is_err());
    }
}
