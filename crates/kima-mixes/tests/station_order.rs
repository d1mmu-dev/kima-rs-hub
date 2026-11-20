//! M5b Task 12: a station's play order (`kima_mixes::station_order`), the port of upstream
//! `services/mixes/__tests__/stationOrder.test.ts` (20 cases) on the in-process listener store (D-260), and
//! `replayPasses` against goldens computed by upstream's own `stationOrder.ts` at the pin
//! (`tools/mix-goldens/upstream.mjs`, key `stationOrder`).
//!
//! Not ported as such (no counterpart, D-277): "never keeps a session on a failed read" and "orders a listener's
//! stations by Redis's clock" (the store does not fail and has one clock). "serves from the pool when Redis fails" is
//! `lost_session_rebuilt_by_replay_passes` (a store that lost the session); "replays the next pass when the end of the
//! kept one cannot be read" and "never mixes two sessions in a page" are driven by another tab replacing the session,
//! the one way the store cannot serve a read.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::future::{Ready, ready};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kima_mixes::spread::{Follows, random_from_seed, spread_by_artist};
use kima_mixes::station_order::{
    CHUNK, REPLAY_PASSES, REPLAY_SONGS, SEED_PATTERN, SESSION_TTL, STATIONS_KEPT, StationError,
    StationPage, StationRequest, StationSong, all_playable, replay_first_pass, replay_passes,
    seed_is_valid, station_page,
};
use kima_mixes::store::{ListenerStore, SaveMode, StationSession};
use serde_json::Value;
use uuid::Uuid;

const PIN: &str = "c7527577d925b20f5adf410c7fd4e0c8ac250f69";

type Songs = Result<Option<Vec<StationSong>>, Infallible>;
type Keep = Result<HashSet<Uuid>, Infallible>;

fn u1() -> Uuid {
    Uuid::from_u128(0xa1)
}

fn u2() -> Uuid {
    Uuid::from_u128(0xa2)
}

/// `n` songs, `per_artist` to an artist, ids in list order.
fn pool(n: usize, per_artist: usize) -> Vec<StationSong> {
    (0..n)
        .map(|i| StationSong {
            id: Uuid::from_u128(i as u128 + 1),
            artist_id: format!("a{}", i / per_artist),
        })
        .collect()
}

/// A pool reader that counts its reads.
#[derive(Clone)]
struct Reader {
    songs: Arc<Vec<StationSong>>,
    reads: Arc<AtomicUsize>,
}

impl Reader {
    fn new(songs: Vec<StationSong>) -> Self {
        Self {
            songs: Arc::new(songs),
            reads: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn read(&self) -> impl FnMut() -> Ready<Songs> + '_ {
        move || {
            self.reads.fetch_add(1, Ordering::SeqCst);
            ready(Ok(Some(self.songs.as_ref().clone())))
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

fn request(
    limit: usize,
    seed: &str,
    offset: u64,
    listener: Uuid,
    now: Instant,
) -> StationRequest<'_> {
    StationRequest {
        limit,
        seed: Some(seed),
        offset,
        listener: Some(listener),
        now,
    }
}

async fn page(
    store: &ListenerStore,
    station: &str,
    reader: &Reader,
    r: StationRequest<'_>,
) -> StationPage {
    station_page(
        store,
        station,
        reader.read(),
        all_playable::<Infallible>,
        &r,
    )
    .await
    .unwrap()
}

/// Every page of `limit` from `offset` until `count` songs, for listener u1.
#[allow(clippy::too_many_arguments)]
async fn listen(
    store: &ListenerStore,
    station: &str,
    reader: &Reader,
    seed: &str,
    offset: u64,
    count: usize,
    limit: usize,
    now: Instant,
) -> Vec<Uuid> {
    let mut heard = Vec::new();
    let mut at = offset;
    while heard.len() < count {
        let p = page(store, station, reader, request(limit, seed, at, u1(), now)).await;
        assert!(!p.track_ids.is_empty(), "empty page at {at}");
        heard.extend(p.track_ids);
        at = p.next.unwrap();
    }
    heard
}

fn held_seed(store: &ListenerStore, station: &str, listener: Uuid, now: Instant) -> Option<String> {
    store.touch_session(station, listener, now).map(|m| m.seed)
}

fn held_pass(store: &ListenerStore, station: &str, now: Instant) -> Option<u64> {
    store.touch_session(station, u1(), now).map(|m| m.pass)
}

/// Starts `seed`'s session of `station` for `listener` the way another tab would: a pass written in place of any.
fn other_tab_starts(
    store: &ListenerStore,
    station: &str,
    listener: Uuid,
    seed: &str,
    songs: &[StationSong],
    now: Instant,
) {
    let pass = replay_passes(seed, songs, 0);
    store.save_session(
        station,
        listener,
        StationSession {
            seed: seed.into(),
            start: pass.start,
            pass: pass.pass,
            length: pass.length,
            last_artist: pass.last_artist.clone(),
            ids: pass.ids.clone().unwrap(),
        },
        SaveMode::Replace,
        now,
    );
}

#[tokio::test]
async fn same_seed_same_order_while_pool_unchanged() {
    // Upstream: "reads the pool once per pass, then pages from Redis, across its stored chunks".
    let store = ListenerStore::new();
    let now = Instant::now();
    let songs = pool(2_500, 3);
    let reader = Reader::new(songs.clone());
    let whole = page(&store, "t", &reader, request(100, "x", 0, u1(), now))
        .await
        .track_ids;
    assert_eq!(reader.reads(), 1);
    let all = listen(&store, "t", &reader, "x", 0, 2_500, 70, now).await;
    assert_eq!(reader.reads(), 1);
    assert_eq!(all[..100], whole[..]);
    assert_eq!(
        all.iter().copied().collect::<HashSet<_>>(),
        songs.iter().map(|s| s.id).collect::<HashSet<_>>()
    );
    assert_eq!(all.len(), 2_500);
    // A page across the first 1,000 ids (upstream's chunk boundary).
    let across = page(&store, "t", &reader, request(100, "x", 950, u1(), now)).await;
    assert_eq!(across.track_ids, all[950..1050]);
    assert_eq!(reader.reads(), 1);
    // Another store, the same seed and pool: the same order.
    let again = listen(&ListenerStore::new(), "t", &reader, "x", 0, 2_500, 100, now).await;
    assert_eq!(again, all);
}

#[tokio::test]
async fn offset_pages_and_next() {
    // Upstream: "stops a page at the end of a pass and starts the pool over at the next, reading it again".
    let store = ListenerStore::new();
    let now = Instant::now();
    let reader = Reader::new(pool(250, 3));
    let at = |offset: u64, limit: usize| {
        page(&store, "t", &reader, request(limit, "x", offset, u1(), now))
    };
    at(0, 100).await;
    let last = at(200, 100).await;
    assert_eq!(last.track_ids.len(), 50);
    assert_eq!(last.next, Some(250));
    let next = at(250, 100).await;
    assert_eq!(next.track_ids.len(), 100);
    assert_eq!(next.next, Some(350));
    assert_eq!(reader.reads(), 2);
    // Asking from before the kept pass starts at it.
    assert_eq!(at(100, 10).await.track_ids, next.track_ids[..10]);
}

#[tokio::test]
async fn pass_restarts_without_last_artist_or_seam_window_songs() {
    // 30 songs by one artist and 15 by 15 others: the seam rules over three passes (recipes.integration
    // "never ends ..." on the pure order).
    let songs: Vec<StationSong> = (0..45)
        .map(|i| StationSong {
            id: Uuid::from_u128(i + 1),
            artist_id: if i < 30 {
                "big".into()
            } else {
                format!("o{i}")
            },
        })
        .collect();
    let artist: HashMap<Uuid, String> = songs.iter().map(|s| (s.id, s.artist_id.clone())).collect();
    let reader = Reader::new(songs);
    for t in 0..5 {
        let store = ListenerStore::new();
        let heard = listen(
            &store,
            "t",
            &reader,
            &format!("cycle-{t}"),
            0,
            135,
            10,
            Instant::now(),
        )
        .await;
        let passes: Vec<&[Uuid]> = heard.chunks(45).collect();
        for p in &passes {
            assert_eq!(p.iter().collect::<HashSet<_>>().len(), 45);
        }
        assert_ne!(passes[0], passes[1]);
        for pair in passes.windows(2) {
            let (done, next) = (pair[0], pair[1]);
            assert_ne!(artist[&next[0]], artist[&done[44]]);
            // The last 15 songs (a third of the pool) stay out of the next pass's first 15.
            let last: HashSet<&Uuid> = done[30..].iter().collect();
            assert!(next[..15].iter().all(|id| !last.contains(id)));
        }
    }
}

#[tokio::test]
async fn one_session_per_listener_and_station() {
    // Upstream: "keeps one session per listener and station: a new start replaces the old one and its ids".
    let store = ListenerStore::new();
    let now = Instant::now();
    let reader = Reader::new(pool(3_000, 3));
    page(&store, "t", &reader, request(100, "first", 0, u1(), now)).await;
    page(&store, "t", &reader, request(100, "other", 0, u2(), now)).await;
    page(&store, "t2", &reader, request(100, "first", 0, u1(), now)).await;
    assert_eq!(store.held_stations(u1(), now), ["t", "t2"]);
    assert_eq!(store.held_stations(u2(), now), ["t"]);
    page(&store, "t", &reader, request(100, "second", 0, u1(), now)).await;
    assert_eq!(held_seed(&store, "t", u1(), now).as_deref(), Some("second"));
    assert_eq!(store.held_stations(u1(), now), ["t", "t2"]);
    assert_eq!(store.held_entries(), 3);
}

#[tokio::test]
async fn older_tab_served_in_its_own_order_without_pushing_out_the_newer() {
    let store = ListenerStore::new();
    let now = Instant::now();
    let reader = Reader::new(pool(400, 3));
    let first = listen(&store, "t", &reader, "old", 0, 200, 100, now).await;
    page(&store, "t", &reader, request(100, "new", 0, u1(), now)).await;
    let rest = listen(&store, "t", &reader, "old", 200, 300, 100, now).await;
    assert_eq!(held_seed(&store, "t", u1(), now).as_deref(), Some("new"));
    let own = listen(&store, "t2", &reader, "old", 0, 500, 100, now).await;
    assert_eq!([first, rest].concat(), own);
}

#[tokio::test]
async fn passes_over_a_page_with_nothing_playable() {
    let store = ListenerStore::new();
    let now = Instant::now();
    let reader = Reader::new(pool(500, 3));
    let all = listen(&store, "t", &reader, "x", 0, 500, 100, now).await;
    assert_eq!(reader.reads(), 1);
    let gone: HashSet<Uuid> = all[100..300].iter().copied().collect();
    let playable = |ids: Vec<Uuid>| -> Ready<Keep> {
        ready(Ok(ids
            .into_iter()
            .filter(|id| !gone.contains(id))
            .collect()))
    };
    let p = station_page(
        &store,
        "t",
        reader.read(),
        playable,
        &request(100, "x", 100, u1(), now),
    )
    .await
    .unwrap();
    assert_eq!(p.track_ids, all[300..400]);
    assert_eq!(p.next, Some(400));
    assert_eq!(reader.reads(), 1);
    // Part of a page gone: the rest of it comes back.
    let partly = station_page(
        &store,
        "t",
        reader.read(),
        playable,
        &request(100, "x", 250, u1(), now),
    )
    .await
    .unwrap();
    assert_eq!(partly.track_ids, all[300..350]);
    assert_eq!(partly.next, Some(350));
}

#[tokio::test]
async fn passes_over_a_long_missing_run_a_chunk_at_a_time() {
    let store = ListenerStore::new();
    let now = Instant::now();
    let reader = Reader::new(pool(5_000, 3));
    let first = listen(&store, "t", &reader, "x", 0, 100, 100, now).await;
    let order = listen(&store, "t2", &reader, "x", 0, 5_000, 100, now).await;
    assert_eq!(first, order[..100]);
    let gone: HashSet<Uuid> = order[100..4_100].iter().copied().collect();
    let checks = AtomicUsize::new(0);
    let playable = |ids: Vec<Uuid>| -> Ready<Keep> {
        checks.fetch_add(1, Ordering::SeqCst);
        ready(Ok(ids
            .into_iter()
            .filter(|id| !gone.contains(id))
            .collect()))
    };
    let p = station_page(
        &store,
        "t",
        reader.read(),
        playable,
        &request(100, "x", 100, u1(), now),
    )
    .await
    .unwrap();
    assert_eq!(p.track_ids, order[4_100..4_200]);
    assert_eq!(p.next, Some(4_200));
    // One page, then CHUNK songs a check: 4,000 missing songs take 5 checks, not 40.
    assert_eq!(CHUNK, 1000);
    assert!(checks.load(Ordering::SeqCst) <= 6);
}

#[tokio::test]
async fn rebuilds_at_the_clients_position_more_than_a_pass_past_the_kept_session() {
    let store = ListenerStore::new();
    let now = Instant::now();
    let reader = Reader::new(pool(300, 3));
    let own = listen(&store, "t2", &reader, "x", 0, 1_200, 100, now).await;
    listen(&store, "t", &reader, "x", 0, 300, 100, now).await;
    assert_eq!(held_pass(&store, "t", now), Some(0));
    let p = page(&store, "t", &reader, request(100, "x", 1_000, u1(), now)).await;
    assert_eq!(p.track_ids, own[1_000..1_100]);
    assert_eq!(p.next, Some(1_100));
    assert_eq!(held_pass(&store, "t", now), Some(3));
    let after = page(&store, "t", &reader, request(100, "x", 1_100, u1(), now)).await;
    assert_eq!(after.track_ids, own[1_100..1_200]);
}

#[tokio::test]
async fn gives_up_when_no_song_of_a_new_pass_can_play() {
    let store = ListenerStore::new();
    let reader = Reader::new(pool(30, 3));
    let nothing = |_: Vec<Uuid>| -> Ready<Keep> { ready(Ok(HashSet::new())) };
    let p = station_page(
        &store,
        "t",
        reader.read(),
        nothing,
        &request(10, "x", 0, u1(), Instant::now()),
    )
    .await
    .unwrap();
    assert_eq!(
        p,
        StationPage {
            track_ids: vec![],
            next: Some(90)
        }
    );
    assert_eq!(reader.reads(), 3);
}

#[tokio::test]
async fn nothing_for_an_empty_pool_and_a_session_whose_pool_emptied_ends() {
    let store = ListenerStore::new();
    let now = Instant::now();
    let none = || -> Ready<Songs> { ready(Ok(None)) };
    let empty = || -> Ready<Songs> { ready(Ok(Some(vec![]))) };
    let p = station_page(
        &store,
        "t",
        none,
        all_playable::<Infallible>,
        &request(10, "x", 0, u1(), now),
    )
    .await
    .unwrap();
    assert_eq!(
        p,
        StationPage {
            track_ids: vec![],
            next: Some(0)
        }
    );
    let p = station_page(
        &store,
        "t",
        empty,
        all_playable::<Infallible>,
        &StationRequest::fresh(10),
    )
    .await
    .unwrap();
    assert_eq!(
        p,
        StationPage {
            track_ids: vec![],
            next: None
        }
    );
    let five = Reader::new(pool(5, 1));
    page(&store, "t", &five, request(10, "y", 0, u1(), now)).await;
    let p = station_page(
        &store,
        "t",
        empty,
        all_playable::<Infallible>,
        &request(10, "y", 5, u1(), now),
    )
    .await
    .unwrap();
    assert_eq!(
        p,
        StationPage {
            track_ids: vec![],
            next: Some(5)
        }
    );
}

#[tokio::test]
async fn no_session_and_no_next_without_a_seed_none_kept_without_a_listener() {
    let store = ListenerStore::new();
    let reader = Reader::new(pool(300, 3));
    let p = station_page(
        &store,
        "t",
        reader.read(),
        all_playable::<Infallible>,
        &StationRequest::fresh(100),
    )
    .await
    .unwrap();
    assert_eq!(p.track_ids.len(), 100);
    assert_eq!(p.next, None);
    // Without a seed each refill is a fresh order.
    let q = station_page(
        &store,
        "t",
        reader.read(),
        all_playable::<Infallible>,
        &StationRequest::fresh(300),
    )
    .await
    .unwrap();
    let r = station_page(
        &store,
        "t",
        reader.read(),
        all_playable::<Infallible>,
        &StationRequest::fresh(300),
    )
    .await
    .unwrap();
    assert_ne!(q.track_ids, r.track_ids);
    let anonymous = StationRequest {
        listener: None,
        ..request(100, "x", 0, u1(), Instant::now())
    };
    let p = station_page(
        &store,
        "t",
        reader.read(),
        all_playable::<Infallible>,
        &anonymous,
    )
    .await
    .unwrap();
    assert_eq!(p.next, Some(100));
    assert_eq!(store.held_entries(), 0);
}

#[tokio::test]
async fn seed_pattern_rejected() {
    assert_eq!(SEED_PATTERN, "^[A-Za-z0-9_-]{1,128}$");
    let store = ListenerStore::new();
    let reader = Reader::new(pool(5, 1));
    let long = "x".repeat(129);
    for seed in ["abc:ids", "a b", "", long.as_str(), "é"] {
        assert!(!seed_is_valid(seed));
        let r = station_page(
            &store,
            "t",
            reader.read(),
            all_playable::<Infallible>,
            &request(1, seed, 0, u1(), Instant::now()),
        )
        .await;
        assert!(matches!(r, Err(StationError::Invalid(_))), "{seed:?}");
    }
    assert!(seed_is_valid(&"x".repeat(128)));
    assert!(seed_is_valid("Az09_-"));
    // A limit under 1 and an offset without a seed are refused as well.
    let zero = request(0, "x", 0, u1(), Instant::now());
    assert!(matches!(
        station_page(
            &store,
            "t",
            reader.read(),
            all_playable::<Infallible>,
            &zero
        )
        .await,
        Err(StationError::Invalid(_))
    ));
    let unseeded = StationRequest {
        offset: 10,
        ..StationRequest::fresh(10)
    };
    assert!(matches!(
        station_page(
            &store,
            "t",
            reader.read(),
            all_playable::<Infallible>,
            &unseeded
        )
        .await,
        Err(StationError::Invalid(_))
    ));
    assert_eq!(reader.reads(), 0);
}

#[tokio::test]
async fn lost_session_rebuilt_by_replay_passes() {
    // Upstream: "serves from the pool when Redis fails, the same order for the same seed while the pool is unchanged";
    // here the store lost the session (a restart, or pushed out).
    let now = Instant::now();
    let reader = Reader::new(pool(400, 3));
    let whole = listen(&ListenerStore::new(), "t", &reader, "x", 0, 1_000, 100, now).await;
    assert_eq!(
        listen(&ListenerStore::new(), "t", &reader, "x", 150, 100, 100, now).await,
        whole[150..250]
    );
    // A later pass too: it is replayed after the passes before it.
    assert_eq!(
        listen(&ListenerStore::new(), "t", &reader, "x", 850, 100, 100, now).await,
        whole[850..950]
    );
}

#[tokio::test]
async fn session_forgotten_twelve_hours_after_its_last_refill() {
    assert_eq!(SESSION_TTL, Duration::from_secs(12 * 3600));
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let reader = Reader::new(pool(50, 3));
    page(&store, "t", &reader, request(10, "x", 0, u1(), t0)).await;
    let refill = t0 + Duration::from_secs(11 * 3600);
    page(&store, "t", &reader, request(10, "x", 10, u1(), refill)).await;
    assert_eq!(reader.reads(), 1);
    assert_eq!(
        store.held_stations(u1(), refill + SESSION_TTL - Duration::from_secs(1)),
        ["t"]
    );
    assert!(
        store
            .held_stations(u1(), refill + SESSION_TTL + Duration::from_secs(1))
            .is_empty()
    );
}

#[tokio::test]
async fn three_stations_kept_per_profile() {
    assert_eq!(STATIONS_KEPT, 3);
    let store = ListenerStore::new();
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let songs = pool(300, 3);
    let reader = Reader::new(songs.clone());
    for (i, station) in ["a", "b", "c"].into_iter().enumerate() {
        page(
            &store,
            station,
            &reader,
            request(10, "x", 0, u1(), at(i as u64 + 1)),
        )
        .await;
    }
    page(&store, "a2", &reader, request(10, "x", 0, u2(), at(4))).await;
    // Using "a" again makes "b" the least recent, so a fourth station pushes out "b".
    page(&store, "a", &reader, request(10, "x", 10, u1(), at(5))).await;
    page(&store, "d", &reader, request(10, "x", 0, u1(), at(6))).await;
    assert_eq!(store.held_stations(u1(), at(7)), ["a", "c", "d"]);
    assert_eq!(store.held_stations(u2(), at(7)), ["a2"]);
    // A station pushed out keeps playing in its order, rebuilt from the pool.
    let pushed_out = page(&store, "b", &reader, request(10, "x", 10, u1(), at(8))).await;
    let other = listen(
        &ListenerStore::new(),
        "other",
        &reader,
        "x",
        0,
        20,
        10,
        at(8),
    )
    .await;
    assert_eq!(pushed_out.track_ids, other[10..]);
}

#[tokio::test]
async fn never_mixes_two_sessions_in_a_page_when_another_tab_starts_over() {
    // Another tab starts the station over between this refill's first read of its pass and its next one: the
    // refill rebuilds its own order instead of reading the other session's ids.
    let now = Instant::now();
    let songs = pool(400, 3);
    let reader = Reader::new(songs.clone());
    let own = listen(
        &ListenerStore::new(),
        "t2",
        &reader,
        "old",
        0,
        400,
        100,
        now,
    )
    .await;
    let store = ListenerStore::new();
    listen(&store, "t", &reader, "old", 0, 100, 100, now).await;
    let calls = AtomicUsize::new(0);
    let playable = |ids: Vec<Uuid>| -> Ready<Keep> {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            // The first look: the other tab starts over now, and nothing of this page can play any more.
            other_tab_starts(&store, "t", u1(), "new", &songs, now);
            return ready(Ok(HashSet::new()));
        }
        ready(Ok(ids.into_iter().collect()))
    };
    let p = station_page(
        &store,
        "t",
        reader.read(),
        playable,
        &request(100, "old", 100, u1(), now),
    )
    .await
    .unwrap();
    assert_eq!(p.track_ids, own[200..300]);
    assert_eq!(p.next, Some(300));
    assert_eq!(held_seed(&store, "t", u1(), now).as_deref(), Some("new"));
}

#[tokio::test]
async fn replays_the_next_pass_when_the_end_of_the_kept_one_cannot_be_read() {
    let now = Instant::now();
    let songs = pool(300, 3);
    let reader = Reader::new(songs.clone());
    let own = listen(&ListenerStore::new(), "t2", &reader, "x", 0, 500, 100, now).await;
    let store = ListenerStore::new();
    listen(&store, "t", &reader, "x", 0, 300, 100, now).await;
    // While this refill reads the pool to start over, another tab replaces the session: the end of the kept pass is
    // gone, so the next pass is replayed and not kept.
    let replaced = AtomicUsize::new(0);
    let read = || -> Ready<Songs> {
        if replaced.fetch_add(1, Ordering::SeqCst) == 0 {
            other_tab_starts(&store, "t", u1(), "other", &songs, now);
        }
        ready(Ok(Some(songs.clone())))
    };
    let p = station_page(
        &store,
        "t",
        read,
        all_playable::<Infallible>,
        &request(100, "x", 300, u1(), now),
    )
    .await
    .unwrap();
    assert_eq!(p.track_ids, own[300..400]);
    assert_eq!(p.next, Some(400));
    assert_eq!(held_seed(&store, "t", u1(), now).as_deref(), Some("other"));
    // The next refill goes on from 400 in the same order.
    let after = page(&store, "t", &reader, request(100, "x", 400, u1(), now)).await;
    assert_eq!(after.track_ids, own[400..500]);
}

#[tokio::test]
async fn keeps_a_found_missing_session_only_if_no_other_tab_started_one_meanwhile() {
    let now = Instant::now();
    let songs = pool(400, 3);
    let store = ListenerStore::new();
    let read = || -> Ready<Songs> {
        // The other tab starts the station while this refill reads the pool.
        if store.held_stations(u1(), now).is_empty() {
            other_tab_starts(&store, "t", u1(), "new", &songs, now);
        }
        ready(Ok(Some(songs.clone())))
    };
    let p = station_page(
        &store,
        "t",
        read,
        all_playable::<Infallible>,
        &request(100, "old", 100, u1(), now),
    )
    .await
    .unwrap();
    let reader = Reader::new(songs.clone());
    let own = listen(
        &ListenerStore::new(),
        "t2",
        &reader,
        "old",
        0,
        200,
        100,
        now,
    )
    .await;
    assert_eq!(p.track_ids, own[100..]);
    assert_eq!(held_seed(&store, "t", u1(), now).as_deref(), Some("new"));
}

#[test]
fn replay_bounds_1000_passes_200k_songs() {
    assert_eq!((REPLAY_PASSES, REPLAY_SONGS), (1000, 200_000));
    // A one-song pool: every pass up to REPLAY_PASSES is replayed from the first; deeper, the pass is built alone.
    assert_eq!(replay_first_pass(REPLAY_PASSES, 1), 0);
    assert_eq!(replay_first_pass(REPLAY_PASSES + 1, 1), REPLAY_PASSES + 1);
    assert_eq!(replay_first_pass(999_999, 1), 999_999);
    let tiny = pool(1, 1);
    assert_eq!(replay_passes("x", &tiny, 999_999).start, 999_999);
    // 50,000 songs: pass 3 replays 4 passes (200,000 songs); pass 4 would order 250,000, so it is built alone.
    let quarter = (REPLAY_SONGS / 4) as usize;
    assert_eq!(replay_first_pass(3, quarter), 0);
    assert_eq!(replay_first_pass(4, quarter), 4);
    // A pass built alone is the pass's own seed order, without the seam rules.
    let songs = pool(30, 3);
    let own_seed = format!("x\0{}", REPLAY_PASSES + 1);
    let alone = spread_by_artist(
        &songs,
        |s: &StationSong| s.artist_id.clone(),
        &mut random_from_seed(&own_seed),
        &Follows::default(),
    );
    let deep = replay_passes("x", &songs, REPLAY_PASSES + 1);
    assert_eq!(
        *deep.ids.unwrap(),
        alone.iter().map(|s| s.id).collect::<Vec<_>>()
    );
    assert_eq!(deep.start, (REPLAY_PASSES + 1) * 30);
}

#[test]
fn replay_passes_match_upstream_goldens() {
    let g: Value = serde_json::from_str(include_str!("fixtures/draw_goldens.json")).unwrap();
    assert_eq!(g["provenance"]["commit"], PIN);
    let s = &g["stationOrder"];
    assert_eq!(s["sessionTtlS"].as_u64(), Some(SESSION_TTL.as_secs()));
    assert_eq!(s["stationsKept"].as_u64(), Some(STATIONS_KEPT as u64));
    assert_eq!(s["replayPasses"].as_u64(), Some(REPLAY_PASSES));
    assert_eq!(s["replaySongs"].as_u64(), Some(REPLAY_SONGS));
    assert_eq!(s["seedPattern"], SEED_PATTERN);
    let pools: HashMap<String, Vec<StationSong>> = s["pools"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, songs)| {
            let songs = songs
                .as_array()
                .unwrap()
                .iter()
                .map(|p| StationSong {
                    id: Uuid::parse_str(p[0].as_str().unwrap()).unwrap(),
                    artist_id: p[1].as_str().unwrap().into(),
                })
                .collect();
            (name.clone(), songs)
        })
        .collect();
    let cases = s["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 50);
    for case in cases {
        let songs = &pools[case["pool"].as_str().unwrap()];
        let seed = case["seed"].as_str().unwrap();
        let target = case["target"].as_u64().unwrap();
        let pass = replay_passes(seed, songs, target);
        let label = format!("{} {seed} {target}", case["pool"]);
        assert_eq!(pass.start, case["start"].as_u64().unwrap(), "{label}");
        assert_eq!(pass.pass, case["pass"].as_u64().unwrap(), "{label}");
        assert_eq!(pass.length, case["length"].as_u64().unwrap(), "{label}");
        assert_eq!(
            pass.last_artist,
            case["lastArtist"].as_str().unwrap(),
            "{label}"
        );
        let ids: Vec<Uuid> = case["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| Uuid::parse_str(id.as_str().unwrap()).unwrap())
            .collect();
        assert_eq!(*pass.ids.unwrap(), ids, "{label}");
    }
}
