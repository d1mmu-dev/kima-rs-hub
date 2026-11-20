//! A station's play order, handed out a refill at a time (M5b Task 12; port of upstream
//! `services/mixes/stationOrder.ts` at the pin `c7527577`, unchanged at the reference checkout `9450c94b`): every song of
//! the pool, spread by artist ([`spread_by_artist`]). A listening session (one `seed`) builds the order once and keeps it
//! in the listener store ([`ListenerStore`], upstream's Redis keys `station:<station>:<listener>`, D-260), so refills read
//! only their page and a song joining or leaving the pool cannot make the session skip or repeat; when it runs out the
//! pool starts over in a new order that does not open with the artist or the songs heard last. Positions count up
//! through every pass.
//!
//! One session per listener (the profile) and station, the [`STATIONS_KEPT`] most recently used per listener, each kept
//! [`SESSION_TTL`] after its last refill (the store's rules). A session the store does not hold (forgotten, pushed out,
//! or lost with a restart) is rebuilt from the pool ([`replay_passes`]): the same order for the same seed while the pool
//! is unchanged. A refill from an older tab's session is rebuilt the same way and served without being kept.
//!
//! Upstream's Redis-failure paths (a read or write that throws) have no counterpart: the store is in process and does
//! not fail. Its only "cannot serve" case is a session another tab replaced between reading it and reading its ids,
//! which takes the same rebuild path upstream takes for both (D-277).

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use uuid::Uuid;

use crate::spread::{Follows, Recent, random_from_seed, spread_by_artist};
use crate::store::{ListenerStore, SaveMode, SessionMeta, StationSession};

pub use crate::store::{SESSION_TTL, STATIONS_KEPT};

/// When the pool starts over, the songs from the last this many places stay out of as many first places: a refill, and
/// at most a third of the pool (at a half, the artist rule could not keep them all out).
pub const SEAM_WINDOW: usize = 100;
/// Positions read at a time after a page with nothing playable (upstream's Redis chunk of ids): a run of songs that
/// went missing after the session read the pool costs one `playable` check per this many songs.
pub const CHUNK: usize = 1000;
/// How deep [`replay_passes`] goes to rebuild a later pass: at most this many passes after the first ...
pub const REPLAY_PASSES: u64 = 1000;
/// ... and this many songs in all. Past either, the pass is rebuilt alone, without the seam rules.
pub const REPLAY_SONGS: u64 = 200_000;
/// What a seed may be (upstream `SEED_PATTERN`): it names a session, so only plain characters.
pub const SEED_PATTERN: &str = "^[A-Za-z0-9_-]{1,128}$";

fn seam_window(songs: usize) -> usize {
    SEAM_WINDOW.min(songs / 3)
}

/// Whether `seed` matches [`SEED_PATTERN`].
pub fn seed_is_valid(seed: &str) -> bool {
    (1..=128).contains(&seed.len())
        && seed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A song of a station's pool: the track and the artist the mixes' cap counts it toward (upstream `artistSql`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationSong {
    pub id: Uuid,
    pub artist_id: String,
}

/// What a station's refill asks for (upstream `StationRequest`): `limit` songs. With a `seed`, from position `offset`
/// of that session's order, kept for `listener`; without one, the start of a fresh order. `now` is the store's clock.
#[derive(Debug, Clone, Copy)]
pub struct StationRequest<'a> {
    pub limit: usize,
    pub seed: Option<&'a str>,
    pub offset: u64,
    pub listener: Option<Uuid>,
    pub now: Instant,
}

impl<'a> StationRequest<'a> {
    /// A fresh order's first `limit` songs: no seed, no listener.
    pub fn fresh(limit: usize) -> Self {
        Self {
            limit,
            seed: None,
            offset: 0,
            listener: None,
            now: Instant::now(),
        }
    }

    /// `limit` songs of `seed`'s session from `offset`, kept for `listener`.
    pub fn session(limit: usize, seed: &'a str, offset: u64, listener: Uuid) -> Self {
        Self {
            limit,
            seed: Some(seed),
            offset,
            listener: Some(listener),
            now: Instant::now(),
        }
    }
}

/// A refill (upstream `StationPage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationPage {
    pub track_ids: Vec<Uuid>,
    /// The position to ask from for the next refill; only for a request with a seed.
    pub next: Option<u64>,
}

/// Why a refill has no answer: a request upstream refuses with a `RangeError`, or the pool's or `playable`'s error.
#[derive(Debug)]
pub enum StationError<E> {
    Invalid(String),
    Source(E),
}

/// A page with no songs, with `next` only for a request with a seed (upstream `emptyPage`).
pub fn empty_page(request: &StationRequest<'_>) -> StationPage {
    StationPage {
        track_ids: Vec::new(),
        next: request.seed.map(|_| request.offset),
    }
}

/// A `playable` that keeps every id.
pub fn all_playable<E>(ids: Vec<Uuid>) -> std::future::Ready<Result<HashSet<Uuid>, E>> {
    std::future::ready(Ok(ids.into_iter().collect()))
}

/// One pass of a session's order (upstream `Pass`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass {
    /// The position its first song has.
    pub start: u64,
    pub pass: u64,
    pub length: u64,
    pub last_artist: String,
    /// The ids in order, when this refill built the pass; otherwise they are read from the store.
    pub ids: Option<Arc<Vec<Uuid>>>,
    /// Whether the store holds this pass for the session.
    kept: bool,
}

impl Pass {
    fn held(meta: SessionMeta) -> Self {
        Pass {
            start: meta.start,
            pass: meta.pass,
            length: meta.length,
            last_artist: meta.last_artist,
            ids: None,
            kept: true,
        }
    }

    fn end(&self) -> u64 {
        self.start + self.length
    }
}

fn check_request(request: &StationRequest<'_>) -> Result<(), String> {
    if request.limit < 1 {
        return Err(format!(
            "limit must be a positive integer, got {}",
            request.limit
        ));
    }
    if request.seed.is_none() && request.offset > 0 {
        return Err("an offset needs the seed of the order it counts into".into());
    }
    if let Some(seed) = request.seed {
        if !seed_is_valid(seed) {
            return Err("a seed is 1 to 128 letters, digits, - or _".into());
        }
    }
    Ok(())
}

/// The first pass [`replay_passes`] builds for `target` over `songs` songs: 0, or `target` itself (built alone) when
/// replaying would go past [`REPLAY_PASSES`] passes or [`REPLAY_SONGS`] songs.
pub fn replay_first_pass(target: u64, songs: usize) -> u64 {
    let ordered = u128::from(target + 1) * songs as u128;
    if target > REPLAY_PASSES || ordered > u128::from(REPLAY_SONGS) {
        target
    } else {
        0
    }
}

/// Upstream `replayPasses`: pass `target` of a seed's session over `songs`, rebuilt as it was built: each pass from its
/// own seed, after the pass before it (its last artist and last songs), so with the pool unchanged it comes back song
/// for song. Deeper than [`REPLAY_PASSES`] or [`REPLAY_SONGS`], pass `target` is built alone. `songs` is not empty.
pub fn replay_passes(seed: &str, songs: &[StationSong], target: u64) -> Pass {
    let first = replay_first_pass(target, songs.len());
    let mut pass = order_pass(seed, songs, first, None);
    for p in first + 1..=target {
        let ids = pass.ids.clone().expect("a built pass has its ids");
        pass = order_pass(seed, songs, p, Some((&pass.last_artist, &ids)));
    }
    pass
}

/// Upstream `orderPass`: pass `pass` of a seed's session over `songs`; after `done` (its last artist and its ids, or
/// the last of them), it does not open with that artist or those songs.
fn order_pass(seed: &str, songs: &[StationSong], pass: u64, done: Option<(&str, &[Uuid])>) -> Pass {
    let seed_of_pass = format!("{seed}\0{pass}");
    let mut random = random_from_seed(&seed_of_pass);
    let artist_of = |s: &StationSong| s.artist_id.clone();
    let order = match done {
        None => spread_by_artist(songs, artist_of, &mut random, &Follows::default()),
        Some((last_artist, ids)) => {
            let window = seam_window(songs.len()).min(ids.len());
            let heard: HashSet<Uuid> = ids[ids.len() - window..].iter().copied().collect();
            let is_recent = |s: &StationSong| heard.contains(&s.id);
            spread_by_artist(
                songs,
                artist_of,
                &mut random,
                &Follows {
                    previous_artist: (!last_artist.is_empty()).then_some(last_artist),
                    recent: Some(Recent {
                        is_recent: &is_recent,
                        window,
                    }),
                },
            )
        }
    };
    Pass {
        start: pass * songs.len() as u64,
        pass,
        length: songs.len() as u64,
        last_artist: order
            .last()
            .map(|s| s.artist_id.clone())
            .unwrap_or_default(),
        ids: Some(Arc::new(order.iter().map(|s| s.id).collect())),
        kept: false,
    }
}

/// Where a refill keeps what it builds: the store, the station's name and the listener.
struct Keeper<'s> {
    store: &'s ListenerStore,
    station: &'s str,
    listener: Option<Uuid>,
    seed: &'s str,
    now: Instant,
}

impl Keeper<'_> {
    /// Upstream `save`: keeps a pass for the session as `mode` allows.
    fn save(&self, pass: &mut Pass, mode: SaveMode) {
        let Some(listener) = self.listener else {
            return;
        };
        let session = StationSession {
            seed: self.seed.to_string(),
            start: pass.start,
            pass: pass.pass,
            length: pass.length,
            last_artist: pass.last_artist.clone(),
            ids: pass.ids.clone().expect("a built pass has its ids"),
        };
        pass.kept = self
            .store
            .save_session(self.station, listener, session, mode, self.now);
    }

    /// Upstream `readIds`: positions `from` to `to` of the session's pass starting at `start`; `None` when the store
    /// no longer holds that pass of this seed.
    fn read_ids(&self, start: u64, from: usize, to: usize) -> Option<Vec<Uuid>> {
        let listener = self.listener?;
        self.store
            .session_ids(self.station, listener, self.seed, start, from, to, self.now)
    }
}

async fn read_pool<E, P, PF>(pool: &mut P) -> Result<Option<Vec<StationSong>>, StationError<E>>
where
    P: FnMut() -> PF,
    PF: Future<Output = Result<Option<Vec<StationSong>>, E>>,
{
    Ok(pool()
        .await
        .map_err(StationError::Source)?
        .filter(|songs| !songs.is_empty()))
}

/// Upstream `rebuild`: the pass holding `offset` for a session the store does not hold, kept as `keep` says.
async fn rebuild<E, P, PF>(
    keeper: &Keeper<'_>,
    pool: &mut P,
    offset: u64,
    keep: Option<SaveMode>,
) -> Result<Option<Pass>, StationError<E>>
where
    P: FnMut() -> PF,
    PF: Future<Output = Result<Option<Vec<StationSong>>, E>>,
{
    let Some(songs) = read_pool(pool).await? else {
        return Ok(None);
    };
    let mut pass = replay_passes(keeper.seed, &songs, offset / songs.len() as u64);
    if let Some(mode) = keep {
        keeper.save(&mut pass, mode);
    }
    Ok(Some(pass))
}

/// Upstream `startOver`: the next pass after `done`, over the pool as it is now, kept for the session when `done` was
/// and the session still holds `done`. When the end of `done` cannot be read, the next pass is replayed and not kept.
async fn start_over<E, P, PF>(
    keeper: &Keeper<'_>,
    pool: &mut P,
    done: &Pass,
) -> Result<Option<Pass>, StationError<E>>
where
    P: FnMut() -> PF,
    PF: Future<Output = Result<Option<Vec<StationSong>>, E>>,
{
    let Some(songs) = read_pool(pool).await? else {
        return Ok(None);
    };
    let length = done.length as usize;
    let window = seam_window(songs.len()).min(length);
    let tail = match &done.ids {
        Some(ids) => Some(ids[length - window..].to_vec()),
        None => keeper.read_ids(done.start, length - window, length),
    };
    let Some(tail) = tail else {
        let mut replayed = replay_passes(keeper.seed, &songs, done.pass + 1);
        replayed.start = done.end();
        return Ok(Some(replayed));
    };
    // The tail stands in for the whole pass: only its last songs and last artist are read.
    let mut next = order_pass(
        keeper.seed,
        &songs,
        done.pass + 1,
        Some((&done.last_artist, &tail)),
    );
    next.start = done.end();
    if done.kept {
        keeper.save(
            &mut next,
            SaveMode::After {
                seed: keeper.seed.to_string(),
                start: done.start,
            },
        );
    }
    Ok(Some(next))
}

/// Upstream `stationPage`: a refill of `station` (a name unique to the station) from `pool`, which reads its songs
/// (`None` when it has none yet). The pool is read only when a session starts, starts over, or is not held. A page
/// stops at the end of a pass; the next starts the pool over, so a station ends only when its pool is empty.
/// `playable` keeps the ids that can still play; a page left with none is passed over.
pub async fn station_page<E, P, PF, K, KF>(
    store: &ListenerStore,
    station: &str,
    mut pool: P,
    mut playable: K,
    request: &StationRequest<'_>,
) -> Result<StationPage, StationError<E>>
where
    P: FnMut() -> PF,
    PF: Future<Output = Result<Option<Vec<StationSong>>, E>>,
    K: FnMut(Vec<Uuid>) -> KF,
    KF: Future<Output = Result<HashSet<Uuid>, E>>,
{
    check_request(request).map_err(StationError::Invalid)?;
    let limit = request.limit;
    let Some(seed) = request.seed else {
        let songs = read_pool(&mut pool).await?.unwrap_or_default();
        // A fresh order each call (upstream: `Math.random`).
        let fresh = Uuid::new_v4().to_string();
        let mut random = random_from_seed(&fresh);
        let order = spread_by_artist(
            &songs,
            |s: &StationSong| s.artist_id.clone(),
            &mut random,
            &Follows::default(),
        );
        let ids: Vec<Uuid> = order.iter().take(limit).map(|s| s.id).collect();
        let keep = playable(ids.clone()).await.map_err(StationError::Source)?;
        return Ok(StationPage {
            track_ids: ids.into_iter().filter(|id| keep.contains(id)).collect(),
            next: None,
        });
    };
    let empty = |at: u64| StationPage {
        track_ids: Vec::new(),
        next: Some(at),
    };
    let offset = request.offset;
    let keeper = Keeper {
        store,
        station,
        listener: request.listener,
        seed,
        now: request.now,
    };

    let held = request
        .listener
        .and_then(|listener| store.touch_session(station, listener, request.now));
    let current = match held {
        Some(meta) if meta.seed == seed => Some(Pass::held(meta)),
        held => {
            // Kept when this refill starts the session (replacing any other), or the listener holds none for this
            // station (only if still none by the time it is written).
            let keep = match (request.listener, offset, held) {
                (None, _, _) => None,
                (Some(_), 0, _) => Some(SaveMode::Replace),
                (Some(_), _, None) => Some(SaveMode::Absent),
                (Some(_), _, Some(_)) => None,
            };
            rebuild(&keeper, &mut pool, offset, keep).await?
        }
    };
    let Some(mut current) = current else {
        return Ok(empty(offset));
    };

    let mut at = offset.max(current.start);
    // A new pass is read from the pool, which holds only playable songs, so a second one with nothing playable means
    // the pool and `playable` disagree: give up rather than go round again.
    let mut passes_started = 0;
    // Positions read at a time: a page, or after a page with nothing playable a chunk.
    let mut window = limit;
    loop {
        if at >= current.end() {
            passes_started += 1;
            if passes_started > 2 {
                return Ok(empty(at));
            }
            let Some(next) = start_over(&keeper, &mut pool, &current).await? else {
                return Ok(empty(at));
            };
            current = next;
            if at >= current.end() {
                // The client is past this pass too: the pass holding its position, kept in place of the one just
                // written when that was kept.
                let keep = current.kept.then(|| SaveMode::After {
                    seed: seed.to_string(),
                    start: current.start,
                });
                match rebuild(&keeper, &mut pool, at, keep).await? {
                    Some(pass) => current = pass,
                    None => return Ok(empty(at)),
                }
            }
        }
        let from = (at - current.start) as usize;
        let to = (from + window).min(current.length as usize);
        let read = match &current.ids {
            Some(ids) => Some(ids[from..to].to_vec()),
            None => keeper.read_ids(current.start, from, to),
        };
        let ids = match read {
            Some(ids) => ids,
            None => {
                // Another tab replaced the session since it was read: this refill builds its own.
                match rebuild(&keeper, &mut pool, at, None).await? {
                    Some(pass) => current = pass,
                    None => return Ok(empty(at)),
                }
                let ids = current.ids.as_ref().expect("a built pass has its ids");
                let from = (at - current.start) as usize;
                ids[from..(from + window).min(ids.len())].to_vec()
            }
        };
        let keep = playable(ids.clone()).await.map_err(StationError::Source)?;
        let Some(first) = ids.iter().position(|id| keep.contains(id)) else {
            // Nothing here can play any more: on through the same order, a chunk at a time.
            at += ids.len() as u64;
            window = limit.max(CHUNK);
            continue;
        };
        let page: Vec<Uuid> = ids[first..(first + limit).min(ids.len())].to_vec();
        at += (first + page.len()) as u64;
        return Ok(StationPage {
            track_ids: page.into_iter().filter(|id| keep.contains(id)).collect(),
            next: Some(at),
        });
    }
}
