//! AcoustID asked in batches, and its replies kept (M-SYNC Task 12; upstream
//! `acoustidService.lookupMany` / `lookup` and `acoustidIdentification.prefetchAcoustidReplies`).
//!
//! The rate limit is per application key, shared by every install, so the recording stage asks
//! about the songs coming up [`PREFETCH_AHEAD`] at a time, [`acoustid::BATCH_SIZE`] fingerprints
//! per request, and every reply is kept per fingerprint and length in `acoustid_replies`: a song
//! is asked once. Nothing here throws: a failure stops the batch and each song can still ask on
//! its own ([`answer`]). The client key is only ever handed to [`acoustid`], which keeps it in the
//! request body; no reply, log line or error here holds it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kima_db::acoustid_replies::{self as replies, Status};
use kima_db::identity;
use kima_net::{Evidence, Failure, Outcome, Source};
use serde_json::json;

use crate::IdentCtx;
use crate::acoustid::{self, AcoustidResult, BATCH_SIZE, Picked};

/// After a batch request fails (a rate limit pauses the source instead) batches wait this long;
/// each song can still ask on its own (`BATCH_HOLD_MS`).
pub const HOLD: Duration = Duration::from_secs(60);
/// How far ahead of the recording stage AcoustID is asked, so its batches fill up.
pub const PREFETCH_AHEAD: i64 = 100;

/// Until when batches wait. Shared by the clones of one [`IdentCtx`].
#[derive(Clone, Default)]
pub struct BatchHold(Arc<Mutex<Option<Instant>>>);

impl BatchHold {
    /// Whether a failed batch still holds the next ones.
    pub fn held(&self) -> bool {
        let until = *self.0.lock().unwrap_or_else(PoisonError::into_inner);
        until.is_some_and(|u| Instant::now() < u)
    }

    fn start(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now() + HOLD);
    }
}

/// Whether a batch may be asked now: AcoustID not disabled, the network switch on, no batch
/// failure in the last minute, a client key, and the `acoustid` source not paused.
pub async fn may_ask_in_batches(ctx: &IdentCtx) -> bool {
    if ctx.config.acoustid_disabled || !ctx.net.enabled() || ctx.acoustid_hold.held() {
        return false;
    }
    if ctx.net.paused_until(Source::AcoustId).is_some() {
        return false;
    }
    ctx.acoustid_key().await.is_some()
}

/// Saves what AcoustID said about one question. A failed save never un-answers it.
async fn save(
    ctx: &IdentCtx,
    fingerprint: &str,
    secs: i32,
    status: Status,
    results: &[AcoustidResult],
) {
    let hash = replies::fingerprint_hash(fingerprint);
    let value = serde_json::to_value(results).unwrap_or_else(|_| json!([]));
    if let Err(error) = replies::put(&ctx.pool, &hash, secs, status, &value).await {
        tracing::warn!(%error, "could not save an AcoustID reply");
    }
}

async fn save_results(ctx: &IdentCtx, fingerprint: &str, secs: i32, results: &[AcoustidResult]) {
    let status = if results.is_empty() {
        Status::Unmatched
    } else {
        Status::Matched
    };
    save(ctx, fingerprint, secs, status, results).await;
}

/// A question still to ask: the trimmed fingerprint, its length and its reply key.
struct Owed<'a> {
    fingerprint: &'a str,
    secs: i32,
    key: (Vec<u8>, i32),
}

/// Asks AcoustID about many songs at once, [`BATCH_SIZE`] fingerprints per request, and saves
/// each reply. Questions already answered are not asked again; a question with no fingerprint or
/// no length is never asked. A rate limit pauses the source and stops; any other failure holds
/// batches for [`HOLD`] and stops. A fingerprint missing from a reply (the server skips an
/// invalid one without a word) and every fingerprint of a request the server refuses are saved
/// `rejected`. Returns how many songs were asked.
pub async fn prefetch(ctx: &IdentCtx, questions: &[(String, i32)]) -> usize {
    if !may_ask_in_batches(ctx).await {
        return 0;
    }
    let Some(key) = ctx.acoustid_key().await else {
        return 0;
    };

    let mut seen = HashSet::new();
    let mut owed: Vec<Owed<'_>> = Vec::new();
    for (fingerprint, secs) in questions {
        let fingerprint = fingerprint.trim();
        if fingerprint.is_empty() || *secs < 1 {
            continue;
        }
        let k = (replies::fingerprint_hash(fingerprint), *secs);
        if seen.insert(k.clone()) {
            owed.push(Owed {
                fingerprint,
                secs: *secs,
                key: k,
            });
        }
    }
    let keys: Vec<(Vec<u8>, i32)> = owed.iter().map(|o| o.key.clone()).collect();
    let saved = match replies::saved_among(&ctx.pool, &keys).await {
        Ok(s) => s,
        Err(error) => {
            // Unreadable saved replies are asked again (upstream).
            tracing::debug!(%error, "AcoustID saved replies unreadable");
            HashSet::new()
        }
    };
    owed.retain(|o| !saved.contains(&o.key));

    let mut asked = 0;
    for chunk in owed.chunks(BATCH_SIZE) {
        let qs: Vec<(&str, i32)> = chunk.iter().map(|o| (o.fingerprint, o.secs)).collect();
        match acoustid::lookup_batch(&ctx.net, &key, &qs).await {
            Outcome::Answered(items) => {
                let mut answered = vec![false; chunk.len()];
                for (index, results) in items {
                    answered[index] = true;
                    save_results(ctx, qs[index].0, qs[index].1, &results).await;
                }
                for (i, (fp, secs)) in qs.iter().enumerate() {
                    if !answered[i] {
                        save(ctx, fp, *secs, Status::Rejected, &[]).await;
                    }
                }
                asked += chunk.len();
            }
            // Every fingerprint in this request was refused: none will ever be answered.
            Outcome::Permanent(f) if acoustid::refused_input(&f) => {
                for (fp, secs) in &qs {
                    save(ctx, fp, *secs, Status::Rejected, &[]).await;
                }
                asked += chunk.len();
            }
            Outcome::RateLimited { until } => {
                tracing::warn!(%until, "AcoustID rate limited during a batch lookup; source paused");
                return asked;
            }
            other => {
                tracing::debug!(code = other.code(), "AcoustID batch lookup failed");
                ctx.acoustid_hold.start();
                return asked;
            }
        }
    }
    asked
}

/// [`prefetch`] for the next `limit` songs the recording stage will claim. Reads nothing when
/// no batch may be asked now.
pub async fn prefetch_ahead(ctx: &IdentCtx, limit: i64) -> sqlx::Result<usize> {
    if !may_ask_in_batches(ctx).await {
        return Ok(0);
    }
    let questions = identity::upcoming_acoustid_questions(&ctx.pool, limit).await?;
    Ok(prefetch(ctx, &questions).await)
}

/// The saved results for this question; `None` when AcoustID must be asked (nothing saved, or
/// what is saved is unreadable).
pub async fn saved_results(
    ctx: &IdentCtx,
    fingerprint: &str,
    duration_secs: i32,
) -> Option<Vec<AcoustidResult>> {
    let hash = replies::fingerprint_hash(fingerprint);
    let saved = match replies::get(&ctx.pool, &hash, duration_secs).await {
        Ok(s) => s?,
        Err(error) => {
            tracing::debug!(%error, "AcoustID saved reply unreadable");
            return None;
        }
    };
    serde_json::from_value(saved.results).ok()
}

/// What AcoustID says about one fingerprint, for the ladder: the saved reply when there is one
/// (no request), else one lookup of its own, saved. Errors as [`acoustid::lookup`], and a request
/// the server refuses as invalid is saved `rejected` and answers `Absent`.
pub async fn answer(
    ctx: &IdentCtx,
    key: &str,
    fingerprint: &str,
    duration_secs: i32,
    title: Option<&str>,
) -> Outcome<Picked> {
    let (key, fingerprint) = (key.trim(), fingerprint.trim());
    if key.is_empty() || fingerprint.is_empty() || duration_secs < 1 {
        // The validation of a lookup: no key, or nothing to ask with, without a request.
        return acoustid::identify(&ctx.net, key, fingerprint, duration_secs, title).await;
    }
    if let Some(results) = saved_results(ctx, fingerprint, duration_secs).await {
        return acoustid::pick_outcome(&results, duration_secs, title);
    }
    match acoustid::lookup(&ctx.net, key, fingerprint, duration_secs).await {
        Outcome::Answered(results) => {
            save_results(ctx, fingerprint, duration_secs, &results).await;
            acoustid::pick_outcome(&results, duration_secs, title)
        }
        Outcome::Absent(_) => {
            save(ctx, fingerprint, duration_secs, Status::Unmatched, &[]).await;
            Outcome::Absent(acoustid::no_match(&[], duration_secs))
        }
        Outcome::Permanent(f) if acoustid::refused_input(&f) => {
            save(ctx, fingerprint, duration_secs, Status::Rejected, &[]).await;
            Outcome::Absent(Evidence(json!({
                "askedDuration": duration_secs,
                "topScore": null,
                "reason": "AcoustID refused the fingerprint or length as invalid",
            })))
        }
        Outcome::RateLimited { until } => Outcome::RateLimited { until },
        Outcome::Transient(f) => Outcome::Transient(f),
        Outcome::Permanent(f) => Outcome::Permanent(Failure::new(f.code, f.message)),
    }
}
