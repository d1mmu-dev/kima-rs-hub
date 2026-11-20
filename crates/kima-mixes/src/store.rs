//! The listener state store (M5b Task 6; owner decision Q1 = A, D-260): what upstream keeps in Redis for the mixes and
//! the radio, in process memory, with upstream's lifetimes and limits, lost on restart.
//!
//! | Upstream Redis key | Here | Lifetime |
//! |---|---|---|
//! | `mixes:<user>` (`routes/mixes.ts`): one list per time zone | [`ListenerStore::mix_list`] per profile and zone | 1 h per zone, at most 4 zones |
//! | `station:<station>:<listener>` + `stations:<listener>` (`stationOrder.ts`) | [`ListenerStore::touch_session`] | 12 h after the last refill, 3 stations per listener |
//! | `mixes:familiar:<user>` + its lock (`familiarity.ts`) | [`ListenerStore::familiar`] | 3 h, 60 s for no boost; lock 60 s |
//! | `mixes:energy-band` (`nightPlays.ts`) | [`ListenerStore::energy_band`] | 3 h, 60 s without readings |
//! | `radiocover:<listener>:<day>:<type>:<value>` (`stationCovers.ts`) | [`ListenerStore::cover`] | to the local day's end, 600 s for no cover |
//! | `radiocover:tz:<listener>` (`radioRow.ts`) | [`ListenerStore::cover_zones`] | 7 days after the last radio row |
//! | `radiocover:warm:<listener>:<day>` (`radioRow.ts`) | [`ListenerStore::covers_warmed`] | to the local day's end |
//!
//! Every method takes the time it acts at, so lifetimes are tested without waiting. Entries past their life are never
//! answered and are swept from a map whenever that map is read or written, so memory stays bounded by the live
//! entries. Listeners are profiles (`profile_id`, M-PROF), as every other key here.
//! `drop_mood_dependent` and `drop_all` are upstream's `dropCachedMixes(moodsOnly)` and `dropCachedMixes()` over the
//! `mixes:` keys: station sessions and radio covers are not under `mixes:` and neither drop touches them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::familiar::FamiliarBoost;

/// Upstream `MIXES_CACHE_TTL` (3600 s).
pub const MIXES_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Upstream `MAX_CACHED_ZONES`: a listener on more zones at once drops the oldest list.
pub const MAX_CACHED_ZONES: usize = 4;
/// Upstream `SESSION_TTL_S` (12 h): a refill comes every 100 songs, so 12 h covers that and a pause.
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// Upstream `STATIONS_KEPT`: one station playing on each of two devices and one to switch back to.
pub const STATIONS_KEPT: usize = 3;
/// Upstream `FAMILIAR_CACHE_SECONDS` and `FAMILIAR_EMPTY_CACHE_SECONDS`.
pub const FAMILIAR_CACHE: Duration = Duration::from_secs(3 * 3600);
pub const FAMILIAR_EMPTY_CACHE: Duration = Duration::from_secs(60);
/// Upstream `ENERGY_BAND_CACHE_SECONDS` and `ENERGY_BAND_EMPTY_CACHE_SECONDS`.
pub const ENERGY_BAND_CACHE: Duration = Duration::from_secs(3 * 3600);
pub const ENERGY_BAND_EMPTY_CACHE: Duration = Duration::from_secs(60);
/// Upstream `NO_COVER_SECONDS`: a station without a cover is asked again soon.
pub const NO_COVER: Duration = Duration::from_secs(600);
/// Upstream `ZONE_SECONDS` (`radioRow.ts`): a listener's covers are picked ahead for a week after their last visit.
pub const COVER_ZONE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// Upstream `familiarTiming`: the scan lock's life, and how long (and how often) a caller that finds it held looks for
/// the holder's result before it scans itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamiliarTiming {
    pub lock: Duration,
    pub wait: Duration,
    pub poll: Duration,
}

impl Default for FamiliarTiming {
    fn default() -> Self {
        Self {
            lock: Duration::from_secs(60),
            wait: Duration::from_secs(20),
            poll: Duration::from_millis(100),
        }
    }
}

/// One station session: the pass a listening session (one seed) plays through (upstream's session hash and its ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationSession {
    pub seed: String,
    /// The position this pass starts at; positions count up through every pass.
    pub start: u64,
    pub pass: u64,
    pub length: u64,
    pub last_artist: String,
    pub ids: Arc<Vec<Uuid>>,
}

/// A session without its ids (upstream `readPass`'s meta).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub seed: String,
    pub start: u64,
    pub pass: u64,
    pub length: u64,
    pub last_artist: String,
}

/// When [`ListenerStore::save_session`] writes (upstream `SAVE`'s modes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveMode {
    /// Whatever is there (a start).
    Replace,
    /// Only if the listener holds no session for the station (another tab may have started one meanwhile).
    Absent,
    /// Only if the session still holds the pass it follows, named by seed and start.
    After { seed: String, start: u64 },
}

/// A familiarity scan lock's holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockToken(u64);

struct Zone {
    mixes: Arc<serde_json::Value>,
    at: Instant,
}

struct Session {
    session: StationSession,
    used: Instant,
}

struct Familiar {
    signature: String,
    boost: Option<FamiliarBoost>,
    until: Instant,
}

struct Lock {
    token: u64,
    until: Instant,
}

#[derive(Default)]
struct Inner {
    /// Per profile, its zones in the order written (oldest first).
    mix_lists: HashMap<Uuid, Vec<(String, Zone)>>,
    /// Per listener, its stations' sessions.
    sessions: HashMap<Uuid, HashMap<String, Session>>,
    familiar: HashMap<Uuid, Familiar>,
    familiar_locks: HashMap<Uuid, Lock>,
    energy_band: Option<(Option<(f64, f64)>, Instant)>,
    /// (listener, day, station) -> (cover, kept until).
    covers: HashMap<(Uuid, String, String), (Option<String>, Instant)>,
    /// listener -> (zone of its last radio row, kept until).
    cover_zones: HashMap<Uuid, (String, Instant)>,
    /// (listener, day) whose covers were picked ahead -> kept until.
    warmed: HashMap<(Uuid, String), Instant>,
}

impl Inner {
    fn sweep_mix_lists(&mut self, now: Instant) {
        self.mix_lists.retain(|_, zones| {
            zones.retain(|(_, e)| now.saturating_duration_since(e.at) < MIXES_CACHE_TTL);
            !zones.is_empty()
        });
    }

    fn sweep_sessions(&mut self, now: Instant) {
        self.sessions.retain(|_, stations| {
            stations.retain(|_, s| now.saturating_duration_since(s.used) < SESSION_TTL);
            !stations.is_empty()
        });
    }

    fn sweep_familiar(&mut self, now: Instant) {
        self.familiar.retain(|_, f| now < f.until);
        self.familiar_locks.retain(|_, l| now < l.until);
    }

    fn sweep_covers(&mut self, now: Instant) {
        self.covers.retain(|_, (_, until)| now < *until);
    }

    fn sweep_cover_zones(&mut self, now: Instant) {
        self.cover_zones.retain(|_, (_, until)| now < *until);
        self.warmed.retain(|_, until| now < *until);
    }
}

/// The store; one per process, shared through `AppState`.
#[derive(Default)]
pub struct ListenerStore {
    inner: Mutex<Inner>,
    tokens: AtomicU64,
}

impl std::fmt::Debug for ListenerStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerStore").finish_non_exhaustive()
    }
}

impl ListenerStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // ---- mix lists ---------------------------------------------------------------------------------------------

    /// The profile's mix list built for `zone`, while under an hour old.
    pub fn mix_list(
        &self,
        profile: Uuid,
        zone: &str,
        now: Instant,
    ) -> Option<Arc<serde_json::Value>> {
        let mut inner = self.lock();
        inner.sweep_mix_lists(now);
        inner
            .mix_lists
            .get(&profile)?
            .iter()
            .find(|(z, _)| z == zone)
            .map(|(_, entry)| entry.mixes.clone())
    }

    /// Upstream `cacheMixes`: `mixes` as this zone's list beside the profile's other still-fresh zones, at most
    /// [`MAX_CACHED_ZONES`] (the oldest dropped).
    pub fn put_mix_list(
        &self,
        profile: Uuid,
        zone: &str,
        mixes: Arc<serde_json::Value>,
        now: Instant,
    ) {
        let mut inner = self.lock();
        inner.sweep_mix_lists(now);
        let zones = inner.mix_lists.entry(profile).or_default();
        zones.retain(|(z, _)| z != zone);
        zones.push((zone.to_string(), Zone { mixes, at: now }));
        let excess = zones.len().saturating_sub(MAX_CACHED_ZONES);
        zones.drain(..excess);
    }

    /// Drops the profile's lists (upstream `redis.del("mixes:<user>")` after a saved or removed mood mix).
    pub fn drop_mix_lists(&self, profile: Uuid) {
        self.lock().mix_lists.remove(&profile);
    }

    // ---- station sessions --------------------------------------------------------------------------------------

    /// The stations a listener holds a live session for, by name (sorted), without touching them.
    pub fn held_stations(&self, listener: Uuid, now: Instant) -> Vec<String> {
        let mut inner = self.lock();
        inner.sweep_sessions(now);
        let mut names: Vec<String> = inner
            .sessions
            .get(&listener)
            .map(|stations| stations.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Upstream `TOUCH`: the session's meta, its twelve hours set again and the station marked the listener's most
    /// recently used; `None` when the listener holds no live session for it.
    pub fn touch_session(
        &self,
        station: &str,
        listener: Uuid,
        now: Instant,
    ) -> Option<SessionMeta> {
        let mut inner = self.lock();
        inner.sweep_sessions(now);
        let entry = inner.sessions.get_mut(&listener)?.get_mut(station)?;
        entry.used = now;
        let s = &entry.session;
        Some(SessionMeta {
            seed: s.seed.clone(),
            start: s.start,
            pass: s.pass,
            length: s.length,
            last_artist: s.last_artist.clone(),
        })
    }

    /// Upstream `readIds`: positions `from` to `to` (not included) of the session's pass, only while the session is
    /// still that pass of `seed` (`start`); `None` otherwise or past its ids.
    #[allow(clippy::too_many_arguments)]
    pub fn session_ids(
        &self,
        station: &str,
        listener: Uuid,
        seed: &str,
        start: u64,
        from: usize,
        to: usize,
        now: Instant,
    ) -> Option<Vec<Uuid>> {
        if to <= from {
            return Some(Vec::new());
        }
        let mut inner = self.lock();
        inner.sweep_sessions(now);
        let entry = inner.sessions.get(&listener)?.get(station)?;
        let s = &entry.session;
        if s.seed != seed || s.start != start || to > s.ids.len() {
            return None;
        }
        Some(s.ids[from..to].to_vec())
    }

    /// Upstream `SAVE`: writes a pass for the session as `mode` allows and marks the station the listener's most
    /// recently used, dropping the sessions of all but their [`STATIONS_KEPT`] most recent. True when written.
    pub fn save_session(
        &self,
        station: &str,
        listener: Uuid,
        session: StationSession,
        mode: SaveMode,
        now: Instant,
    ) -> bool {
        let mut inner = self.lock();
        inner.sweep_sessions(now);
        let stations = inner.sessions.entry(listener).or_default();
        match &mode {
            SaveMode::Replace => {}
            SaveMode::Absent => {
                if stations.contains_key(station) {
                    return false;
                }
            }
            SaveMode::After { seed, start } => match stations.get(station) {
                Some(held) if held.session.seed == *seed && held.session.start == *start => {}
                _ => return false,
            },
        }
        stations.insert(station.to_string(), Session { session, used: now });
        if stations.len() > STATIONS_KEPT {
            let mut by_use: Vec<(String, Instant)> = stations
                .iter()
                .filter(|(name, _)| name.as_str() != station)
                .map(|(name, s)| (name.clone(), s.used))
                .collect();
            by_use.sort_by_key(|(_, used)| std::cmp::Reverse(*used));
            for (name, _) in by_use.into_iter().skip(STATIONS_KEPT - 1) {
                stations.remove(&name);
            }
        }
        true
    }

    // ---- energy band -------------------------------------------------------------------------------------------

    /// The library's kept energy band: `Some(None)` is a kept "no readings yet", `None` a miss.
    pub fn energy_band(&self, now: Instant) -> Option<Option<(f64, f64)>> {
        let mut inner = self.lock();
        match inner.energy_band {
            Some((band, until)) if now < until => Some(band),
            Some(_) => {
                inner.energy_band = None;
                None
            }
            None => None,
        }
    }

    pub fn put_energy_band(&self, band: Option<(f64, f64)>, now: Instant) {
        let life = if band.is_some() {
            ENERGY_BAND_CACHE
        } else {
            ENERGY_BAND_EMPTY_CACHE
        };
        self.lock().energy_band = Some((band, now + life));
    }

    // ---- familiarity -------------------------------------------------------------------------------------------

    /// Upstream `readKept`: the scan kept for these seeds (`signature`), `Some(None)` for a kept "no boost".
    pub fn familiar(
        &self,
        profile: Uuid,
        signature: &str,
        now: Instant,
    ) -> Option<Option<FamiliarBoost>> {
        let mut inner = self.lock();
        inner.sweep_familiar(now);
        let kept = inner.familiar.get(&profile)?;
        (kept.signature == signature).then(|| kept.boost.clone())
    }

    /// Upstream `writeKept`: keeps the scan, except that "no boost" is never written over a boost kept for the same
    /// seeds; returns the boost that now stands.
    pub fn put_familiar(
        &self,
        profile: Uuid,
        signature: &str,
        boost: Option<FamiliarBoost>,
        now: Instant,
    ) -> Option<FamiliarBoost> {
        let mut inner = self.lock();
        inner.sweep_familiar(now);
        if boost.is_none()
            && let Some(kept) = inner.familiar.get(&profile)
            && kept.signature == signature
            && kept.boost.is_some()
        {
            return kept.boost.clone();
        }
        let life = if boost.is_some() {
            FAMILIAR_CACHE
        } else {
            FAMILIAR_EMPTY_CACHE
        };
        inner.familiar.insert(
            profile,
            Familiar {
                signature: signature.to_string(),
                boost: boost.clone(),
                until: now + life,
            },
        );
        boost
    }

    /// Upstream `takeLock`: the profile's scan lock for `life`, or `None` while another caller holds it.
    pub fn take_familiar_lock(
        &self,
        profile: Uuid,
        now: Instant,
        life: Duration,
    ) -> Option<LockToken> {
        let mut inner = self.lock();
        inner.sweep_familiar(now);
        if inner.familiar_locks.contains_key(&profile) {
            return None;
        }
        let token = self.tokens.fetch_add(1, Ordering::Relaxed);
        inner.familiar_locks.insert(
            profile,
            Lock {
                token,
                until: now + life,
            },
        );
        Some(LockToken(token))
    }

    pub fn familiar_lock_held(&self, profile: Uuid, now: Instant) -> bool {
        let mut inner = self.lock();
        inner.sweep_familiar(now);
        inner.familiar_locks.contains_key(&profile)
    }

    /// Upstream `RELEASE_LOCK`: frees the lock only if it still holds the caller's token (it may have expired and been
    /// taken by another scan).
    pub fn release_familiar_lock(&self, profile: Uuid, token: LockToken) {
        let mut inner = self.lock();
        if inner
            .familiar_locks
            .get(&profile)
            .is_some_and(|l| l.token == token.0)
        {
            inner.familiar_locks.remove(&profile);
        }
    }

    /// Upstream `waitForKept`: the result the lock's holder writes, looked for every `poll` for up to `wait`; `None`
    /// when it has not come by then or the lock is gone without one (the caller then scans itself). The lock is
    /// looked at before the result: the holder writes the result and then frees the lock.
    pub async fn wait_for_familiar(
        &self,
        profile: Uuid,
        signature: &str,
        timing: FamiliarTiming,
    ) -> Option<Option<FamiliarBoost>> {
        let until = Instant::now() + timing.wait;
        while Instant::now() < until {
            tokio::time::sleep(timing.poll).await;
            let now = Instant::now();
            let held = self.familiar_lock_held(profile, now);
            if let Some(kept) = self.familiar(profile, signature, now) {
                return Some(kept);
            }
            if !held {
                return None;
            }
        }
        None
    }

    // ---- radio covers ------------------------------------------------------------------------------------------

    /// The listener's cover pick for a station on `day`: `Some(None)` is a kept "no cover", `None` a miss.
    pub fn cover(
        &self,
        listener: Uuid,
        day: &str,
        station: &str,
        now: Instant,
    ) -> Option<Option<String>> {
        let mut inner = self.lock();
        inner.sweep_covers(now);
        let (cover, _) = inner
            .covers
            .get(&(listener, day.to_string(), station.to_string()))?;
        Some(cover.clone())
    }

    /// Upstream `dailyCover`'s `SET … NX`: keeps `cover` until the day ends (`day_left`), a "no cover" at most
    /// [`NO_COVER`]; when a live pick is already kept, that one is the answer.
    pub fn put_cover_if_absent(
        &self,
        listener: Uuid,
        day: &str,
        station: &str,
        cover: Option<String>,
        day_left: Duration,
        now: Instant,
    ) -> Option<String> {
        let mut inner = self.lock();
        inner.sweep_covers(now);
        let key = (listener, day.to_string(), station.to_string());
        if let Some((kept, _)) = inner.covers.get(&key) {
            return kept.clone();
        }
        let life = if cover.is_some() {
            day_left
        } else {
            day_left.min(NO_COVER)
        };
        inner.covers.insert(key, (cover.clone(), now + life));
        cover
    }

    /// Upstream `radiocover:tz:<listener>`: the zone the listener asked the radio row from, kept
    /// [`COVER_ZONE_TTL`] from this visit for picking the next day's covers ahead.
    pub fn keep_cover_zone(&self, listener: Uuid, zone: &str, now: Instant) {
        let mut inner = self.lock();
        inner.sweep_cover_zones(now);
        inner
            .cover_zones
            .insert(listener, (zone.to_string(), now + COVER_ZONE_TTL));
    }

    /// Every listener with a live zone, by listener id.
    pub fn cover_zones(&self, now: Instant) -> Vec<(Uuid, String)> {
        let mut inner = self.lock();
        inner.sweep_cover_zones(now);
        let mut zones: Vec<(Uuid, String)> = inner
            .cover_zones
            .iter()
            .map(|(listener, (zone, _))| (*listener, zone.clone()))
            .collect();
        zones.sort();
        zones
    }

    /// Drops a listener that no longer exists from the zones.
    pub fn forget_cover_zone(&self, listener: Uuid) {
        self.lock().cover_zones.remove(&listener);
    }

    /// Upstream `radiocover:warm:<listener>:<day>`: whether this listener's covers were picked ahead for `day`.
    pub fn covers_warmed(&self, listener: Uuid, day: &str, now: Instant) -> bool {
        let mut inner = self.lock();
        inner.sweep_cover_zones(now);
        inner.warmed.contains_key(&(listener, day.to_string()))
    }

    /// Marks this listener's covers picked ahead for `day`, until the day ends (`day_left`).
    pub fn mark_covers_warmed(&self, listener: Uuid, day: &str, day_left: Duration, now: Instant) {
        let mut inner = self.lock();
        inner.sweep_cover_zones(now);
        inner
            .warmed
            .insert((listener, day.to_string()), now + day_left);
    }

    /// How many entries the store holds, live or not yet swept (mix lists per zone, sessions, familiarity scans and
    /// locks, the energy band, covers): what its memory is bounded by.
    pub fn held_entries(&self) -> usize {
        let inner = self.lock();
        inner.mix_lists.values().map(Vec::len).sum::<usize>()
            + inner.sessions.values().map(HashMap::len).sum::<usize>()
            + inner.familiar.len()
            + inner.familiar_locks.len()
            + usize::from(inner.energy_band.is_some())
            + inner.covers.len()
            + inner.cover_zones.len()
            + inner.warmed.len()
    }

    // ---- drops -------------------------------------------------------------------------------------------------

    /// Upstream `dropCachedMixes(true)` after a moods change: the mix lists, not the familiarity scans, their locks or
    /// the energy band (`cacheKeys.ts` `dependsOnMoods`: none of them is worked out from the moods).
    pub fn drop_mood_dependent(&self) {
        self.lock().mix_lists.clear();
    }

    /// Upstream `dropCachedMixes()` (`clearMixCache` after core or full enrichment, a re-run): every `mixes:` entry.
    pub fn drop_all(&self) {
        let mut inner = self.lock();
        inner.mix_lists.clear();
        inner.familiar.clear();
        inner.familiar_locks.clear();
        inner.energy_band = None;
    }
}

/// Drops the mood-dependent entries on every "moods changed" event (`kima_ident::stages::moods::subscribe_changes`),
/// until the sender is gone or `stop` (the server's shutdown token) is cancelled. A lagging receiver missed events,
/// so it drops as well.
pub fn spawn_mood_drop(
    store: Arc<ListenerStore>,
    mut changes: broadcast::Receiver<()>,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => return,
                event = changes.recv() => match event {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => store.drop_mood_dependent(),
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
        }
    })
}
