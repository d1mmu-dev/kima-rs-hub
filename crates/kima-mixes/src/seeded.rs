//! Seeded draws (upstream `services/mixes/helpers.ts`) and the daily pick of mix types (upstream
//! `services/mixes/index.ts` `generateAllMixes`). Each function reproduces upstream's JavaScript arithmetic exactly, so
//! the same inputs give the same mix ids, seeds and draws.

use md5::{Digest as _, Md5};
use sha2::Sha256;
use std::cmp::Ordering;
use std::fmt::Display;

/// Upstream `getSeededRandom`: the 32-bit string hash over UTF-16 code units (`hash << 5` wraps, `hash & hash`
/// truncates to 32 bits), then `Math.abs`. `i32::MIN` becomes 2^31, so the result is a `u32`.
pub fn seeded_random(seed: &str) -> u32 {
    let mut hash: i32 = 0;
    for unit in seed.encode_utf16() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit));
    }
    hash.unsigned_abs()
}

/// The `count` distinct indexes into `generators` mix types a day plays: upstream's linear congruential walk
/// `seed = (seed * 9301 + 49297) % 233280`, index `seed % generators`, a repeat skipped. Exact in `u64` as it is in
/// JavaScript's doubles (`seed * 9301` stays below 2^53).
///
/// Panics when `count` exceeds `generators` (no walk could find that many; upstream would loop forever).
pub fn daily_pick(date_seed: u32, generators: usize, count: usize) -> Vec<usize> {
    assert!(count <= generators, "cannot pick {count} of {generators}");
    let mut picked = Vec::with_capacity(count);
    let mut seed = u64::from(date_seed);
    while picked.len() < count {
        seed = (seed * 9301 + 49297) % 233_280;
        let index = (seed % generators as u64) as usize;
        if !picked.contains(&index) {
            picked.push(index);
        }
    }
    picked
}

/// Upstream `seededKey`: the MD5 of `"<seed>:<id>"` as lower-case hex. A number seed and its decimal text give the
/// same key, as in JavaScript's template string. In SQL: `md5(seed || ':' || id) COLLATE "C"`.
pub fn seeded_key(seed: impl Display, id: &str) -> String {
    hex::encode(Md5::digest(format!("{seed}:{id}").as_bytes()))
}

/// Upstream `seededOrder`: `items` sorted by their [`seeded_key`] (the hex compared byte by byte, as JavaScript
/// compares the strings), stable for equal keys.
pub fn seeded_order<T>(items: Vec<T>, seed: impl Display, id_of: impl Fn(&T) -> &str) -> Vec<T> {
    let seed = seed.to_string();
    let mut keyed: Vec<(String, T)> = items
        .into_iter()
        .map(|item| (seeded_key(&seed, id_of(&item)), item))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, item)| item).collect()
}

/// Upstream `seededTake`: the first `take` of [`seeded_order`] (upstream keeps a heap of `take` instead of sorting;
/// the picks and their order are the same).
pub fn seeded_take<T>(
    items: Vec<T>,
    seed: impl Display,
    id_of: impl Fn(&T) -> &str,
    take: usize,
) -> Vec<T> {
    let mut order = seeded_order(items, seed, id_of);
    order.truncate(take);
    order
}

/// Upstream `weightedOrder` with `seeded`: every item with a positive, finite weight, highest key `ln(u) / weight`
/// first (Efraimidis and Spirakis), `u = (first 13 hex digits of SHA-256("<seed>\0<id>") + 0.5) / 2^52`, ties by id
/// (UTF-16 order, as JavaScript compares strings). `ln` is the fdlibm logarithm JavaScript's `Math.log` uses
/// (`kima_vibe::exact_math::log`), so the keys match bit for bit.
pub fn weighted_order_seeded<T>(
    items: Vec<T>,
    weight_of: impl Fn(&T) -> f64,
    seed: &str,
    id_of: impl Fn(&T) -> &str,
) -> Vec<T> {
    let mut keyed: Vec<(f64, String, T)> = Vec::with_capacity(items.len());
    for item in items {
        let weight = weight_of(&item);
        if !weight.is_finite() || weight <= 0.0 {
            continue;
        }
        let id = id_of(&item).to_string();
        let digest = hex::encode(Sha256::digest(format!("{seed}\0{id}").as_bytes()));
        let bits = u64::from_str_radix(&digest[..13], 16).expect("13 hex digits");
        let u = (bits as f64 + 0.5) / 4_503_599_627_370_496.0; // 2^52
        keyed.push((kima_vibe::exact_math::log(u) / weight, id, item));
    }
    keyed.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.1.encode_utf16().cmp(b.1.encode_utf16()))
    });
    keyed.into_iter().map(|(_, _, item)| item).collect()
}
