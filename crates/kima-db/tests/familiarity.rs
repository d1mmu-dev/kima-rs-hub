//! The familiarity boost (M5b Task 8; port of the familiarity cases of upstream
//! `services/mixes/__tests__/recipes.integration.test.ts` at the pin `c7527577`, the ones Task 4 left with a handed-in
//! boost): the seeds (the listener's most-heard recent songs), the scan through the matching engine, the boost by rank,
//! the kept result (3 h, 60 s for no boost) and the scan shared between callers, over the in-process store (D-260).

mod mix_seed;

use std::sync::Arc;
use std::time::{Duration, Instant};

use kima_core::profile::ProfileId;
use kima_db::PgPool;
use kima_db::mixes::familiar::{
    FAMILIAR_LOOKBACK_DAYS, FAMILIAR_SEEDS, familiar_boost, seeds_of, signature_of,
};
use kima_mixes::familiar::FamiliarBoost;
use kima_mixes::store::{FamiliarTiming, ListenerStore};
use mix_seed::*;
use uuid::Uuid;

fn timing() -> FamiliarTiming {
    FamiliarTiming {
        lock: Duration::from_secs(60),
        wait: Duration::from_millis(400),
        poll: Duration::from_millis(10),
    }
}

async fn heard(pool: &PgPool, who: ProfileId, track: Uuid, times: usize, days_ago: u32) {
    for i in 0..times {
        play_at(
            pool,
            who,
            track,
            &format!("now() - interval '{days_ago} days' - interval '{i} minutes'"),
            true,
        )
        .await;
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn familiar_seeds_200_lookback_365(pool: PgPool) {
    assert_eq!((FAMILIAR_SEEDS, FAMILIAR_LOOKBACK_DAYS), (200, 365));
    let me = listener(&pool, "me").await;
    let you = listener(&pool, "you").await;
    let s = flat(&library(&pool, 6, 1).await);
    heard(&pool, me, s[0], 3, 10).await;
    heard(&pool, me, s[1], 2, 20).await;
    // Equally heard: the most recent listen first.
    heard(&pool, me, s[2], 2, 5).await;
    // Not heard, older than the year, another listener's.
    play_at(&pool, me, s[3], "now() - interval '1 day'", false).await;
    heard(&pool, me, s[4], 5, 366).await;
    heard(&pool, you, s[5], 9, 1).await;
    let seeds = seeds_of(&pool, me).await.unwrap();
    assert_eq!(seeds, vec![(s[0], 3), (s[2], 2), (s[1], 2)]);
    // The signature names which songs, not their weights or order.
    let shuffled = vec![(s[1], 7), (s[0], 1), (s[2], 4)];
    assert_eq!(signature_of(&seeds), signature_of(&shuffled));
    assert_ne!(signature_of(&seeds), signature_of(&seeds[..2]));
    // At most 200 seeds.
    let many = flat(&library(&pool, 205, 1).await);
    for t in &many {
        heard(&pool, me, *t, 1, 30).await;
    }
    assert_eq!(seeds_of(&pool, me).await.unwrap().len(), 200);
}

/// 40 embedded songs: 0-9 in cluster 0 (the listener plays 0 and 1), 10-39 elsewhere.
async fn familiar_library(
    pool: &PgPool,
    me: ProfileId,
) -> (Vec<Uuid>, kima_vibe::feel_probe::FeelProbe) {
    let lib = flat(&library(pool, 20, 2).await);
    for (i, t) in lib.iter().enumerate() {
        embed(pool, *t, &clustered(if i < 10 { 0 } else { 1 + i % 7 }, i)).await;
    }
    let probe = build_matching(pool).await;
    heard(pool, me, lib[0], 3, 1).await;
    heard(pool, me, lib[1], 1, 1).await;
    (lib, probe)
}

fn boost_of(b: &FamiliarBoost, id: Uuid) -> Option<f64> {
    b.ids.iter().position(|x| *x == id).map(|i| b.boosts[i])
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_boost_gives_seeds_full_and_the_closest_by_rank(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let (lib, probe) = familiar_library(&pool, me).await;
    let store = ListenerStore::new();
    let boost = familiar_boost(&pool, &store, Some(&probe), me, timing())
        .await
        .expect("a boost");
    // The seeds take the full boost; ceil(40 * 0.1) = 4 closest others take 1, 0.75, 0.5, 0.25.
    assert_eq!(boost_of(&boost, lib[0]), Some(1.0));
    assert_eq!(boost_of(&boost, lib[1]), Some(1.0));
    assert_eq!(boost.ids.len(), 6);
    let mut others: Vec<f64> = boost
        .ids
        .iter()
        .zip(&boost.boosts)
        .filter(|(id, _)| **id != lib[0] && **id != lib[1])
        .map(|(_, b)| *b)
        .collect();
    others.sort_by(|a, b| b.total_cmp(a));
    assert_eq!(others, vec![1.0, 0.75, 0.5, 0.25]);
    // The closest are the songs that sound like what the listener plays (cluster 0).
    assert!(
        boost.ids[..4].iter().all(|id| lib[2..10].contains(id)),
        "{boost:?}"
    );
    // A listener with no plays has no boost.
    let you = listener(&pool, "you").await;
    assert_eq!(
        familiar_boost(&pool, &store, Some(&probe), you, timing()).await,
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn familiar_cache_3h_empty_60s(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let (lib, probe) = familiar_library(&pool, me).await;
    let store = ListenerStore::new();
    // No similarity data to scan with (no probe): no boost, kept a minute.
    assert_eq!(
        familiar_boost(&pool, &store, None, me, timing()).await,
        None
    );
    let seeds = seeds_of(&pool, me).await.unwrap();
    let sig = signature_of(&seeds);
    let now = Instant::now();
    // Kept "no boost" (its minute is the store's: `kima-mixes` store tests).
    assert_eq!(store.familiar(me.into_uuid(), &sig, now), Some(None));
    // Within that minute the kept "no boost" answers, even where a scan could now run.
    assert_eq!(
        familiar_boost(&pool, &store, Some(&probe), me, timing()).await,
        None
    );
    // Once the minute is over (dropped here) it scans; the boost is kept for the same seeds and answers a probe-less
    // call too.
    store.drop_all();
    let first = familiar_boost(&pool, &store, Some(&probe), me, timing())
        .await
        .unwrap();
    assert_eq!(
        familiar_boost(&pool, &store, None, me, timing()).await,
        Some(first.clone())
    );
    // Playing a seed again keeps the seeds: the kept boost. A new seed song: scanned again.
    heard(&pool, me, lib[0], 1, 1).await;
    assert_eq!(
        familiar_boost(&pool, &store, None, me, timing()).await,
        Some(first.clone())
    );
    heard(&pool, me, lib[20], 1, 1).await;
    let second = familiar_boost(&pool, &store, Some(&probe), me, timing())
        .await
        .unwrap();
    assert_eq!(boost_of(&second, lib[20]), Some(1.0));
    // No boost is never written over a boost kept for the same seeds.
    assert_eq!(
        familiar_boost(&pool, &store, None, me, timing()).await,
        Some(second)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_held_scan_lock_is_waited_for_and_its_result_used(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let (_lib, probe) = familiar_library(&pool, me).await;
    let store = Arc::new(ListenerStore::new());
    let sig = signature_of(&seeds_of(&pool, me).await.unwrap());
    // Another caller holds the lock and writes its result while this one waits: this one uses it without scanning.
    let token = store
        .take_familiar_lock(me.into_uuid(), Instant::now(), Duration::from_secs(60))
        .unwrap();
    let theirs = FamiliarBoost {
        ids: vec![Uuid::from_u128(7)],
        boosts: vec![1.0],
    };
    let writer = {
        let store = store.clone();
        let theirs = theirs.clone();
        let sig = sig.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            store.put_familiar(me.into_uuid(), &sig, Some(theirs), Instant::now());
            store.release_familiar_lock(me.into_uuid(), token);
        })
    };
    assert_eq!(
        familiar_boost(&pool, &store, Some(&probe), me, timing()).await,
        Some(theirs)
    );
    writer.await.unwrap();
    // A holder that never answers: after the wait this caller scans itself, then frees no lock it does not hold.
    store.drop_all();
    let held = store
        .take_familiar_lock(me.into_uuid(), Instant::now(), Duration::from_secs(60))
        .unwrap();
    let started = Instant::now();
    let own = familiar_boost(&pool, &store, Some(&probe), me, timing()).await;
    assert!(started.elapsed() >= timing().wait);
    assert!(own.is_some_and(|b| b.ids.len() == 6));
    assert!(store.familiar_lock_held(me.into_uuid(), Instant::now()));
    store.release_familiar_lock(me.into_uuid(), held);
    // Free lock: taken and released by the scan.
    store.drop_all();
    familiar_boost(&pool, &store, Some(&probe), me, timing())
        .await
        .unwrap();
    assert!(!store.familiar_lock_held(me.into_uuid(), Instant::now()));
}
