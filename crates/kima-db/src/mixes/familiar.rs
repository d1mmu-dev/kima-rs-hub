//! The familiarity boost (M5b Task 8; port of upstream `services/mixes/familiarity.ts`): "more like what this listener
//! already plays", for recipes with a familiarity share. The matching engine's closest songs to the profile's
//! most-heard recent songs get a boost from 1 down to just above 0, added after the recipe standardises, so it only
//! lifts. The scan is kept in the in-process store (D-260) while the seeds are unchanged and shared by overlapping
//! callers through the store's scan lock; a failed scan gives no boost and never fails the mix.

use kima_core::profile::ProfileId;
use kima_mixes::familiar::FamiliarBoost;
use kima_mixes::store::{FamiliarTiming, ListenerStore};
use kima_vibe::feel_probe::FeelProbe;
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;
use std::time::Instant;
use uuid::Uuid;

use crate::vibe::rank::{WeightedInput, matching_context, top_scores};

/// Only plays this recent count.
pub const FAMILIAR_LOOKBACK_DAYS: i32 = 365;
/// The most seeds used.
pub const FAMILIAR_SEEDS: usize = 200;
/// How many closest songs receive a boost, at most ...
pub const FAMILIAR_MATCHES: usize = 1000;
/// ... and at most this share of the library.
pub const FAMILIAR_SHARE: f64 = 0.1;

/// Upstream `seedsOf`: the profile's most-heard songs of the lookback (heard plays only, any track of the library), most
/// heard first; equally heard by the most recent listen, then id.
pub async fn seeds_of(pool: &PgPool, profile: ProfileId) -> sqlx::Result<Vec<(Uuid, i64)>> {
    sqlx::query_as(
        "SELECT p.track_id, count(*) AS plays
         FROM plays p JOIN tracks t ON t.id = p.track_id
         WHERE p.profile_id = $1 AND p.played_at >= now() - make_interval(days => $2) AND p.heard
         GROUP BY p.track_id
         ORDER BY plays DESC, max(p.played_at) DESC, p.track_id
         LIMIT $3",
    )
    .bind(profile)
    .bind(FAMILIAR_LOOKBACK_DAYS)
    .bind(FAMILIAR_SEEDS as i64)
    .fetch_all(pool)
    .await
}

/// Upstream `signatureOf`: which songs are the seeds, whatever their weights (SHA-256 of the sorted ids as JSON).
pub fn signature_of(seeds: &[(Uuid, i64)]) -> String {
    let mut ids: Vec<String> = seeds.iter().map(|(id, _)| id.to_string()).collect();
    ids.sort();
    let json = serde_json::to_string(&ids).expect("strings serialise");
    hex::encode(Sha256::digest(json.as_bytes()))
}

/// Upstream `closestToSeeds`: the seeds take the full boost; the closest songs that are not seeds take it by rank. `None`
/// without matching data (no probe, no state, or no seed has a vector).
async fn closest_to_seeds(
    pool: &PgPool,
    probe: Option<&FeelProbe>,
    seeds: &[(Uuid, i64)],
) -> Result<Option<FamiliarBoost>, String> {
    let Some(probe) = probe else {
        return Ok(None);
    };
    let ids: Vec<Uuid> = seeds.iter().map(|(id, _)| *id).collect();
    let Some(ctx) = matching_context(pool, probe, &ids)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let with_data: Vec<&(Uuid, i64)> = seeds
        .iter()
        .filter(|(id, _)| ctx.tracks.contains_key(id))
        .collect();
    if with_data.is_empty() {
        return Ok(None);
    }
    let total: i64 = with_data.iter().map(|(_, p)| p).sum();
    let matches = FAMILIAR_MATCHES
        .min(((f64::from(ctx.state.track_count) * FAMILIAR_SHARE).ceil() as usize).max(1));
    let inputs: Vec<WeightedInput> = with_data
        .iter()
        .map(|(id, p)| WeightedInput {
            track_id: *id,
            weight: *p as f64 / total as f64,
        })
        .collect();
    let exclude: Vec<Uuid> = with_data.iter().map(|(id, _)| *id).collect();
    let closest = top_scores(pool, &ctx, &inputs, matches as i64, &exclude)
        .await
        .map_err(|e| e.to_string())?;
    let mut ids: Vec<Uuid> = Vec::new();
    let mut boosts: Vec<f64> = Vec::new();
    for (rank, (id, _)) in closest.into_iter().enumerate() {
        ids.push(id);
        boosts.push(1.0 - rank as f64 / matches as f64);
    }
    for id in exclude {
        match ids.iter().position(|x| *x == id) {
            Some(i) => boosts[i] = 1.0,
            None => {
                ids.push(id);
                boosts.push(1.0);
            }
        }
    }
    Ok(Some(FamiliarBoost { ids, boosts }))
}

/// Upstream `familiarBoost`: the boost for the songs closest to this profile's seeds, or `None` when there is none to
/// give. A kept result for the same seeds answers; otherwise the scan lock is taken (a caller that finds it held waits
/// for the holder's result, then scans itself), the scan runs and its result is kept.
pub async fn familiar_boost(
    pool: &PgPool,
    store: &ListenerStore,
    probe: Option<&FeelProbe>,
    profile: ProfileId,
    timing: FamiliarTiming,
) -> Option<FamiliarBoost> {
    let seeds = match seeds_of(pool, profile).await {
        Ok(seeds) => seeds,
        Err(e) => {
            tracing::debug!("[Mixes] familiarity boost lookup failed: {e}");
            return None;
        }
    };
    if seeds.is_empty() {
        return None;
    }
    let key = profile.into_uuid();
    let signature = signature_of(&seeds);
    if let Some(kept) = store.familiar(key, &signature, Instant::now()) {
        return kept;
    }
    let token = store.take_familiar_lock(key, Instant::now(), timing.lock);
    if token.is_none()
        && let Some(kept) = store.wait_for_familiar(key, &signature, timing).await
    {
        return kept;
    }
    let boost = match closest_to_seeds(pool, probe, &seeds).await {
        Ok(boost) => boost,
        Err(e) => {
            tracing::debug!("[Mixes] familiarity scan failed: {e}");
            None
        }
    };
    let kept = store.put_familiar(key, &signature, boost, Instant::now());
    if let Some(token) = token {
        store.release_familiar_lock(key, token);
    }
    kept
}
