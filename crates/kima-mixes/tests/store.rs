//! The in-process listener state store (M5b Task 6, owner Q1 = A, D-260): what upstream keeps in Redis for the mixes
//! and radio, with upstream's lifetimes and limits. Ports the store behaviour behind `routes/mixes.ts` (mix lists per
//! zone), `services/mixes/stationOrder.ts` (station sessions), `familiarity.ts` (scan cache and lock),
//! `nightPlays.ts` (energy band), `stationCovers.ts` (daily covers) and `unifiedEnrichment.ts` `dropCachedMixes`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kima_mixes::familiar::FamiliarBoost;
use kima_mixes::store::{
    COVER_ZONE_TTL, FamiliarTiming, ListenerStore, MAX_CACHED_ZONES, MIXES_CACHE_TTL, NO_COVER,
    SESSION_TTL, STATIONS_KEPT, SaveMode, StationSession, spawn_mood_drop,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn profile(n: u128) -> Uuid {
    Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0000 | n)
}

fn list(tag: &str) -> Arc<serde_json::Value> {
    Arc::new(json!([{ "id": tag }]))
}

fn session(seed: &str, start: u64, ids: &[&str]) -> StationSession {
    StationSession {
        seed: seed.into(),
        start,
        pass: 0,
        length: ids.len() as u64,
        last_artist: "a".into(),
        ids: Arc::new(ids.iter().map(|s| tid(s)).collect()),
    }
}

/// A track id named by a short string.
fn tid(name: &str) -> Uuid {
    Uuid::from_u128(name.bytes().fold(0u128, |a, b| (a << 8) | u128::from(b)))
}

const ME: Uuid = Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0011);
const YOU: Uuid = Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0012);

fn boost(ids: &[u128]) -> FamiliarBoost {
    FamiliarBoost {
        ids: ids.iter().map(|n| profile(*n)).collect(),
        boosts: ids.iter().map(|_| 0.5).collect(),
    }
}

const HOUR: Duration = Duration::from_secs(3600);

#[test]
fn mix_list_cache_per_profile_and_zone_ttl_1h_max_4_zones() {
    assert_eq!(MIXES_CACHE_TTL, HOUR);
    assert_eq!(MAX_CACHED_ZONES, 4);
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let (a, b) = (profile(1), profile(2));
    store.put_mix_list(a, "Europe/Warsaw", list("a-waw"), t0);
    // Per profile: a sibling profile sees nothing; per zone: another zone is a miss.
    assert_eq!(store.mix_list(a, "Europe/Warsaw", t0), Some(list("a-waw")));
    assert_eq!(store.mix_list(b, "Europe/Warsaw", t0), None);
    assert_eq!(store.mix_list(a, "UTC", t0), None);
    // An hour later the list is stale.
    assert!(
        store
            .mix_list(a, "Europe/Warsaw", t0 + HOUR - Duration::from_secs(1))
            .is_some()
    );
    assert_eq!(store.mix_list(a, "Europe/Warsaw", t0 + HOUR), None);
    // Five zones: the oldest is dropped.
    let zones = ["z1", "z2", "z3", "z4", "z5"];
    for (i, z) in zones.iter().enumerate() {
        store.put_mix_list(b, z, list(z), t0 + Duration::from_secs(i as u64));
    }
    let now = t0 + Duration::from_secs(10);
    assert_eq!(store.mix_list(b, "z1", now), None);
    for z in &zones[1..] {
        assert_eq!(store.mix_list(b, z, now), Some(list(z)), "{z}");
    }
    // A stale zone is dropped when another is written, not counted against the four.
    store.put_mix_list(
        b,
        "fresh",
        list("fresh"),
        t0 + HOUR + Duration::from_secs(2),
    );
    assert_eq!(
        store.mix_list(b, "z5", t0 + HOUR + Duration::from_secs(2)),
        Some(list("z5"))
    );
    assert_eq!(
        store.mix_list(b, "z2", t0 + HOUR + Duration::from_secs(2)),
        None
    );
    // A saved mood mix drops the listener's lists only.
    let later = t0 + HOUR + Duration::from_secs(3);
    store.put_mix_list(a, "UTC", list("a-utc"), later);
    store.drop_mix_lists(b);
    assert_eq!(store.mix_list(b, "fresh", later), None);
    assert_eq!(store.mix_list(a, "UTC", later), Some(list("a-utc")));
}

#[test]
fn refresh_replaces_only_its_zone() {
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let a = profile(1);
    store.put_mix_list(a, "UTC", list("utc-1"), t0);
    store.put_mix_list(a, "Europe/Warsaw", list("waw-1"), t0);
    // POST /mixes/refresh for one zone writes that zone's list beside the others'.
    store.put_mix_list(a, "UTC", list("utc-2"), t0 + Duration::from_secs(60));
    let now = t0 + Duration::from_secs(61);
    assert_eq!(store.mix_list(a, "UTC", now), Some(list("utc-2")));
    assert_eq!(store.mix_list(a, "Europe/Warsaw", now), Some(list("waw-1")));
}

#[test]
fn station_sessions_3_kept_12h() {
    assert_eq!(SESSION_TTL, 12 * HOUR);
    assert_eq!(STATIONS_KEPT, 3);
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let me = ME;
    assert!(store.save_session(
        "mood:calm",
        me,
        session("s1", 0, &["a", "b", "c"]),
        SaveMode::Replace,
        t0
    ));
    let meta = store.touch_session("mood:calm", me, t0).unwrap();
    assert_eq!((meta.seed.as_str(), meta.start, meta.length), ("s1", 0, 3));
    assert_eq!(
        store.session_ids("mood:calm", me, "s1", 0, 1, 3, t0),
        Some(vec![tid("b"), tid("c")])
    );
    // Another seed or start: not this session's pass.
    assert_eq!(store.session_ids("mood:calm", me, "s2", 0, 0, 1, t0), None);
    assert_eq!(store.session_ids("mood:calm", me, "s1", 3, 0, 1, t0), None);
    // Another listener holds nothing.
    assert!(store.touch_session("mood:calm", YOU, t0).is_none());

    // "absent" writes only when the listener holds no session for the station; "after" only over the pass it follows.
    assert!(!store.save_session(
        "mood:calm",
        me,
        session("s9", 0, &["x"]),
        SaveMode::Absent,
        t0
    ));
    let after = SaveMode::After {
        seed: "s1".into(),
        start: 99,
    };
    assert!(!store.save_session(
        "mood:calm",
        me,
        session("s1", 3, &["c", "a", "b"]),
        after,
        t0
    ));
    let after = SaveMode::After {
        seed: "s1".into(),
        start: 0,
    };
    assert!(store.save_session(
        "mood:calm",
        me,
        session("s1", 3, &["c", "a", "b"]),
        after,
        t0
    ));
    assert_eq!(store.touch_session("mood:calm", me, t0).unwrap().start, 3);

    // Twelve hours after its last refill a session is gone; a touch sets the twelve hours again.
    let t1 = t0 + 11 * HOUR;
    assert!(store.touch_session("mood:calm", me, t1).is_some());
    assert!(
        store
            .touch_session("mood:calm", me, t1 + 11 * HOUR)
            .is_some()
    );
    assert!(
        store
            .touch_session("mood:calm", me, t1 + 23 * HOUR)
            .is_none()
    );

    // At most three stations per listener, the most recently used: a fourth pushes out the least recent.
    let t2 = t0 + 40 * HOUR;
    for (i, station) in ["s:a", "s:b", "s:c"].iter().enumerate() {
        store.save_session(
            station,
            me,
            session("x", 0, &["1"]),
            SaveMode::Replace,
            t2 + Duration::from_secs(i as u64),
        );
    }
    // Touching s:a makes it the most recent, so s:b is the one pushed out.
    store.touch_session("s:a", me, t2 + Duration::from_secs(10));
    store.save_session(
        "s:d",
        me,
        session("x", 0, &["1"]),
        SaveMode::Replace,
        t2 + Duration::from_secs(11),
    );
    let now = t2 + Duration::from_secs(12);
    assert!(store.touch_session("s:a", me, now).is_some());
    assert!(store.touch_session("s:b", me, now).is_none());
    assert!(store.touch_session("s:c", me, now).is_some());
    assert!(store.touch_session("s:d", me, now).is_some());
    // Another listener's stations are their own.
    store.save_session("s:a", YOU, session("y", 0, &["1"]), SaveMode::Replace, now);
    assert!(store.touch_session("s:c", me, now).is_some());
}

#[test]
fn energy_band_survives_mood_drop() {
    let store = ListenerStore::new();
    let t0 = Instant::now();
    assert_eq!(store.energy_band(t0), None);
    store.put_energy_band(Some((0.3, 0.6)), t0);
    assert_eq!(store.energy_band(t0), Some(Some((0.3, 0.6))));
    store.drop_mood_dependent();
    assert_eq!(store.energy_band(t0), Some(Some((0.3, 0.6))));
    // Kept 3 hours; "no readings yet" only a minute.
    assert_eq!(store.energy_band(t0 + 3 * HOUR), None);
    store.put_energy_band(None, t0);
    assert_eq!(store.energy_band(t0 + Duration::from_secs(59)), Some(None));
    assert_eq!(store.energy_band(t0 + Duration::from_secs(60)), None);
}

#[test]
fn drop_all_and_drop_mood_dependent() {
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let a = profile(1);
    let fill = |store: &ListenerStore| {
        store.put_mix_list(a, "UTC", list("l"), t0);
        store.put_energy_band(Some((0.1, 0.2)), t0);
        store.put_familiar(a, "sig", Some(boost(&[7])), t0);
        store.save_session(
            "mood:calm",
            ME,
            session("s", 0, &["x"]),
            SaveMode::Replace,
            t0,
        );
        store.put_cover_if_absent(
            ME,
            "2026-11-19",
            "mood:calm",
            Some("cover".into()),
            HOUR,
            t0,
        );
    };
    fill(&store);
    // A moods change: the mix lists go; the energy band and the familiarity scan stay (upstream `dependsOnMoods`).
    store.drop_mood_dependent();
    assert_eq!(store.mix_list(a, "UTC", t0), None);
    assert!(store.energy_band(t0).is_some());
    assert_eq!(store.familiar(a, "sig", t0), Some(Some(boost(&[7]))));
    // An enrichment clear drops every `mixes:` entry: lists, energy band, familiarity.
    fill(&store);
    store.drop_all();
    assert_eq!(store.mix_list(a, "UTC", t0), None);
    assert_eq!(store.energy_band(t0), None);
    assert_eq!(store.familiar(a, "sig", t0), None);
    // Station sessions and radio covers are not `mixes:` entries: neither drop touches them.
    assert!(store.touch_session("mood:calm", ME, t0).is_some());
    assert_eq!(
        store.cover(ME, "2026-11-19", "mood:calm", t0),
        Some(Some("cover".into()))
    );
}

#[test]
fn familiar_cache_keeps_a_boost_3h_and_no_boost_60s_per_seed_signature() {
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let a = profile(1);
    store.put_familiar(a, "sig", Some(boost(&[1, 2])), t0);
    assert_eq!(store.familiar(a, "sig", t0), Some(Some(boost(&[1, 2]))));
    // Other seeds: a miss. Another profile: a miss.
    assert_eq!(store.familiar(a, "other", t0), None);
    assert_eq!(store.familiar(profile(2), "sig", t0), None);
    // "No boost" is never written over a boost kept for the same seeds: the kept boost is the answer.
    let t1 = t0 + Duration::from_secs(10);
    assert_eq!(store.put_familiar(a, "sig", None, t1), Some(boost(&[1, 2])));
    assert_eq!(store.familiar(a, "sig", t1), Some(Some(boost(&[1, 2]))));
    // For other seeds it is, and lives a minute.
    assert_eq!(store.put_familiar(a, "new", None, t1), None);
    assert_eq!(
        store.familiar(a, "new", t1 + Duration::from_secs(59)),
        Some(None)
    );
    assert_eq!(store.familiar(a, "new", t1 + Duration::from_secs(60)), None);
    // The boost is kept three hours (read last: a read sweeps what has expired by its time).
    assert_eq!(store.familiar(a, "sig", t0 + 3 * HOUR), None);
}

#[tokio::test]
async fn familiar_scan_single_flight_waits_then_scans_itself() {
    let store = Arc::new(ListenerStore::new());
    let a = profile(1);
    let timing = FamiliarTiming {
        lock: Duration::from_secs(60),
        wait: Duration::from_millis(300),
        poll: Duration::from_millis(10),
    };
    let now = Instant::now();
    // The first caller takes the lock; a second finds it held.
    let token = store.take_familiar_lock(a, now, timing.lock).expect("free");
    assert!(store.take_familiar_lock(a, now, timing.lock).is_none());
    // Another profile's scan is its own.
    assert!(
        store
            .take_familiar_lock(profile(2), now, timing.lock)
            .is_some()
    );

    // The holder writes its result while the second waits: the waiter reads it.
    let writer = {
        let store = store.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            store.put_familiar(a, "sig", Some(boost(&[3])), Instant::now());
            store.release_familiar_lock(a, token);
        })
    };
    assert_eq!(
        store.wait_for_familiar(a, "sig", timing).await,
        Some(Some(boost(&[3])))
    );
    writer.await.unwrap();

    // A holder that never writes: the waiter gives up at the end of the wait and scans itself.
    store.drop_all();
    let _held = store
        .take_familiar_lock(a, Instant::now(), timing.lock)
        .unwrap();
    let started = Instant::now();
    assert_eq!(store.wait_for_familiar(a, "sig", timing).await, None);
    assert!(started.elapsed() >= timing.wait);

    // A lock released without a result: the waiter stops at once.
    store.drop_all();
    let token = store
        .take_familiar_lock(a, Instant::now(), timing.lock)
        .unwrap();
    store.release_familiar_lock(a, token);
    let started = Instant::now();
    assert_eq!(store.wait_for_familiar(a, "sig", timing).await, None);
    assert!(started.elapsed() < timing.wait);

    // A lock held past its life (a holder that died) blocks nobody; only the holder's token releases it.
    let t0 = Instant::now();
    let old = store
        .take_familiar_lock(a, t0, Duration::from_secs(1))
        .unwrap();
    let new = store
        .take_familiar_lock(a, t0 + Duration::from_secs(1), timing.lock)
        .expect("expired");
    store.release_familiar_lock(a, old);
    assert!(store.familiar_lock_held(a, t0 + Duration::from_secs(2)));
    store.release_familiar_lock(a, new);
    assert!(!store.familiar_lock_held(a, t0 + Duration::from_secs(2)));
}

#[test]
fn cover_pick_kept_until_local_day_end_no_cover_600s() {
    assert_eq!(NO_COVER, Duration::from_secs(600));
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let left = 2 * HOUR;
    // The first pick of the day is kept to the day's end; a second pick at the same time gets the first one.
    assert_eq!(
        store.put_cover_if_absent(ME, "2026-11-19", "decade:1990", Some("c1".into()), left, t0),
        Some("c1".into())
    );
    assert_eq!(
        store.put_cover_if_absent(ME, "2026-11-19", "decade:1990", Some("c2".into()), left, t0),
        Some("c1".into())
    );
    assert_eq!(
        store.cover(
            ME,
            "2026-11-19",
            "decade:1990",
            t0 + left - Duration::from_secs(1)
        ),
        Some(Some("c1".into()))
    );
    assert_eq!(
        store.cover(ME, "2026-11-19", "decade:1990", t0 + left),
        None
    );
    // Per listener and per day.
    assert_eq!(store.cover(YOU, "2026-11-19", "decade:1990", t0), None);
    assert_eq!(store.cover(ME, "2026-11-20", "decade:1990", t0), None);
    // "No cover" is kept ten minutes at most, and never past the day.
    assert_eq!(
        store.put_cover_if_absent(ME, "2026-11-19", "mood:calm", None, left, t0),
        None
    );
    assert_eq!(
        store.cover(ME, "2026-11-19", "mood:calm", t0 + Duration::from_secs(599)),
        Some(None)
    );
    assert_eq!(
        store.cover(ME, "2026-11-19", "mood:calm", t0 + NO_COVER),
        None
    );
    store.put_cover_if_absent(
        ME,
        "2026-11-19",
        "genre:rock",
        None,
        Duration::from_secs(120),
        t0,
    );
    assert_eq!(
        store.cover(
            ME,
            "2026-11-19",
            "genre:rock",
            t0 + Duration::from_secs(120)
        ),
        None
    );
}

#[tokio::test]
async fn moods_changed_drops_mood_dependent_mix_caches_only() {
    let store = Arc::new(ListenerStore::new());
    let a = profile(1);
    let now = Instant::now();
    store.put_mix_list(a, "UTC", list("l"), now);
    store.put_energy_band(Some((0.1, 0.2)), now);
    store.put_familiar(a, "sig", Some(boost(&[1])), now);
    let (tx, rx) = tokio::sync::broadcast::channel(16);
    let task = spawn_mood_drop(store.clone(), rx, CancellationToken::new());
    tx.send(()).unwrap();
    let mut waited = 0;
    while store.mix_list(a, "UTC", Instant::now()).is_some() {
        assert!(waited < 200, "the drop follows the event");
        tokio::time::sleep(Duration::from_millis(5)).await;
        waited += 1;
    }
    assert!(store.energy_band(Instant::now()).is_some());
    assert_eq!(
        store.familiar(a, "sig", Instant::now()),
        Some(Some(boost(&[1])))
    );
    // The task ends when the sender is gone.
    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("ends")
        .unwrap();
}

#[tokio::test]
async fn the_mood_drop_task_stops_with_the_shutdown_token() {
    let store = Arc::new(ListenerStore::new());
    let (tx, rx) = tokio::sync::broadcast::channel(16);
    let stop = CancellationToken::new();
    let task = spawn_mood_drop(store.clone(), rx, stop.clone());
    // The sender lives on (the moods step holds it for the process): only the token ends the task.
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("stopped by the token")
        .unwrap();
    drop(tx);
}

#[test]
fn expired_entries_are_swept_on_read_too() {
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let a = profile(1);
    store.put_mix_list(a, "UTC", list("l"), t0);
    store.save_session(
        "mood:calm",
        ME,
        session("s", 0, &["x"]),
        SaveMode::Replace,
        t0,
    );
    store.put_familiar(a, "sig", None, t0);
    store.take_familiar_lock(a, t0, Duration::from_secs(60));
    store.put_energy_band(None, t0);
    store.put_cover_if_absent(ME, "2026-11-19", "mood:calm", None, HOUR, t0);
    assert_eq!(store.held_entries(), 6);
    // Nothing is written again; reads past every entry's life leave the store empty.
    let late = t0 + 13 * HOUR;
    assert_eq!(store.mix_list(a, "UTC", late), None);
    assert!(store.touch_session("mood:calm", ME, late).is_none());
    assert_eq!(store.familiar(a, "sig", late), None);
    assert_eq!(store.energy_band(late), None);
    assert_eq!(store.cover(ME, "2026-11-19", "mood:calm", late), None);
    assert_eq!(store.held_entries(), 0);
}

/// `radiocover:tz:<listener>` (`radioRow.ts`): the zone a listener last asked the row from, kept a week after the last
/// visit for the warm job; a forgotten listener is dropped.
#[test]
fn cover_zone_kept_7_days_from_last_visit_and_forgotten() {
    assert_eq!(COVER_ZONE_TTL, Duration::from_secs(7 * 24 * 3600));
    let store = ListenerStore::new();
    let t0 = Instant::now();
    store.keep_cover_zone(ME, "Asia/Tokyo", t0);
    store.keep_cover_zone(YOU, "UTC", t0);
    assert_eq!(
        store.cover_zones(t0),
        sorted(vec![
            (ME, "Asia/Tokyo".to_string()),
            (YOU, "UTC".to_string())
        ])
    );
    store.forget_cover_zone(YOU);
    assert_eq!(store.cover_zones(t0), vec![(ME, "Asia/Tokyo".to_string())]);
    // A later visit moves the zone and the week (the store sweeps at the time asked, so ask in time order).
    let later = t0 + Duration::from_secs(3600);
    store.keep_cover_zone(ME, "Europe/Warsaw", later);
    store.keep_cover_zone(YOU, "UTC", t0);
    assert_eq!(
        store.cover_zones(t0 + COVER_ZONE_TTL),
        vec![(ME, "Europe/Warsaw".to_string())]
    );
    assert!(store.cover_zones(later + COVER_ZONE_TTL).is_empty());
}

fn sorted(mut v: Vec<(Uuid, String)>) -> Vec<(Uuid, String)> {
    v.sort();
    v
}

/// `radiocover:warm:<listener>:<day>`: once a listener's covers were picked ahead for a local day, the warm job leaves
/// them until that day ends.
#[test]
fn covers_warmed_once_per_listener_and_local_day() {
    let store = ListenerStore::new();
    let t0 = Instant::now();
    assert!(!store.covers_warmed(ME, "2026-11-19", t0));
    store.mark_covers_warmed(ME, "2026-11-19", 2 * HOUR, t0);
    assert!(store.covers_warmed(ME, "2026-11-19", t0 + 2 * HOUR - Duration::from_secs(1)));
    assert!(!store.covers_warmed(ME, "2026-11-19", t0 + 2 * HOUR));
    assert!(!store.covers_warmed(ME, "2026-11-20", t0));
    assert!(!store.covers_warmed(YOU, "2026-11-19", t0));
}
