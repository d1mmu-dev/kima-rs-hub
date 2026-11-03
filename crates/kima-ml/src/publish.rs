//! Async fenced publication adapter: bridges the synchronous per-attempt
//! callback boundary (`attempt::run_attempt`'s `embed_and_publish`) to the
//! async Postgres ML claim/publication layer (`kima_db::ml`), and nothing
//! else. This module does not acquire claims, choose eligibility, run
//! inference, or register with any scheduler/job runtime; it only turns one
//! already-computed, already-validated `ClapEmbedding` plus a caller-held
//! claim into one fenced publish call, mapping storage rejection to an error
//! so a stale or superseded claim is never reported as a successful publish.
#![cfg(feature = "pg-publish")]

use kima_db::PgPool;
use kima_db::ml::{EmbeddingClaim, Provenance};

use crate::clap::ClapEmbedding;

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("storage error publishing embedding: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("embedding claim rejected: stale, missing track/generation, or expired lease")]
    Rejected,
}

/// `kima_db::ml::complete_embedding` returning `false` means the claim was
/// stale, the track/generation was gone, or the lease had expired: never a
/// quiet success. Keep this mapping in one place so it cannot drift from the
/// "rejected storage claim must become callback failure" requirement.
fn classify(accepted: bool) -> Result<(), PublishError> {
    if accepted {
        Ok(())
    } else {
        Err(PublishError::Rejected)
    }
}

/// Fenced, atomic publication of an already-computed CLAP embedding. The
/// caller owns the claim, admission and eligibility; this function neither
/// re-checks nor extends them. No transaction or connection is held beyond
/// `kima_db::ml::complete_embedding`'s own short transaction, and none spans
/// decode or inference: call this only after both have finished.
pub async fn publish_clap_embedding(
    pool: &PgPool,
    claim: &EmbeddingClaim,
    provenance: &Provenance,
    embedding: &ClapEmbedding,
) -> Result<(), PublishError> {
    let accepted =
        kima_db::ml::complete_embedding(pool, claim, provenance, embedding.as_slice()).await?;
    classify(accepted)
}

/// Bridge for `attempt::run_attempt`'s synchronous `embed_and_publish`
/// callback, matching this workspace's existing convention of running
/// blocking/sync work from async call sites via `spawn_blocking` (see
/// `kima-library`/`kima-ident`), rather than the other direction. Call this
/// only from inside a `tokio::task::spawn_blocking` closure: that thread is
/// not part of the async worker pool, so blocking it on `handle` here cannot
/// starve other tasks. Calling it directly on a Tokio worker thread risks a
/// panic or deadlock and is not supported.
pub fn publish_clap_embedding_blocking(
    handle: &tokio::runtime::Handle,
    pool: &PgPool,
    claim: &EmbeddingClaim,
    provenance: &Provenance,
    embedding: &ClapEmbedding,
) -> Result<(), PublishError> {
    handle.block_on(publish_clap_embedding(pool, claim, provenance, embedding))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_storage_acceptance_to_ok() {
        assert!(classify(true).is_ok());
    }

    #[test]
    fn classify_maps_storage_rejection_to_rejected_error_not_success() {
        assert!(matches!(classify(false), Err(PublishError::Rejected)));
    }

    /// Exercises the exact bridging pattern `publish_clap_embedding_blocking`
    /// relies on (blocking a dedicated `spawn_blocking` thread on the runtime
    /// handle) without a database, since that pattern is the real integration
    /// risk being added here and has no DB-specific behavior of its own.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_bridge_runs_an_async_future_from_a_dedicated_blocking_task() {
        let handle = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            handle.block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                42
            })
        })
        .await
        .unwrap();
        assert_eq!(result, 42);
    }
}
