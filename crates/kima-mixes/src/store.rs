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
//!
//! Every method takes the time it acts at, so lifetimes are tested without waiting. Entries past their life are never
//! answered and are swept from a map whenever that map is written, so memory stays bounded by the live entries.
//! `drop_mood_dependent` and `drop_all` are upstream's `dropCachedMixes(moodsOnly)` and `dropCachedMixes()` over the
//! `mixes:` keys: station sessions and radio covers are not under `mixes:` and neither drop touches them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
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
    pub ids: Arc<Vec<String>>,
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
    sessions: HashMap<String, HashMap<String, Session>>,
    familiar: HashMap<Uuid, Familiar>,
    familiar_locks: HashMap<Uuid, Lock>,
    energy_band: Option<(Option<(f64, f64)>, Instant)>,
    /// (listener, day, station) -> (cover, kept until).
    covers: HashMap<(String, String, String), (Option<String>, Instant)>,
}

/// The store; one per process, shared through `AppState`.
#[derive(Default)]
pub struct ListenerStore {
    inner: Mutex<Inner>,
    tokens: AtomicU64,
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
        let inner = self.lock();
        inner
            .mix_lists
            .get(&profile)?
            .iter()
            .find(|(z, _)| z == zone)
            .filter(|(_, entry)| now.saturating_duration_since(entry.at) < MIXES_CACHE_TTL)
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
        inner.mix_lists.retain(|_, zones| {
            zones
                .iter()
                .any(|(_, e)| now.saturating_duration_since(e.at) < MIXES_CACHE_TTL)
        });
        let zones = inner.mix_lists.entry(profile).or_default();
        zones.retain(|(z, e)| z != zone && now.saturating_duration_since(e.at) < MIXES_CACHE_TTL);
        zones.push((zone.to_string(), Zone { mixes, at: now }));
        let excess = zones.len().saturating_sub(MAX_CACHED_ZONES);
        zones.drain(..excess);
    }

    /// Drops the profile's lists (upstream `redis.del("mixes:<user>")` after a saved or removed mood mix).
    pub fn drop_mix_lists(&self, profile: Uuid) {
        self.lock().mix_lists.remove(&profile);
    }

    // ---- station sessions --------------------------------------------------------------------------------------

    /// Upstream `TOUCH`: the session's meta, its twelve hours set again and the station marked the listener's most
    /// recently used; `None` when the listener holds no live session for it.
    pub fn touch_session(
        &self,
        station: &str,
        listener: &str,
        now: Instant,
    ) -> Option<SessionMeta> {
        let mut inner = self.lock();
        let entry = inner.sessions.get_mut(listener)?.get_mut(station)?;
        if now.saturating_duration_since(entry.used) >= SESSION_TTL {
            return None;
        }
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
    pub fn session_ids(
        &self,
        station: &str,
        listener: &str,
        seed: &str,
        start: u64,
        from: usize,
        to: usize,
    ) -> Option<Vec<String>> {
        if to <= from {
            return Some(Vec::new());
        }
        let inner = self.lock();
        let entry = inner.sessions.get(listener)?.get(station)?;
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
        listener: &str,
        session: StationSession,
        mode: SaveMode,
        now: Instant,
    ) -> bool {
        let mut inner = self.lock();
        inner.sessions.retain(|_, stations| {
            stations.retain(|_, s| now.saturating_duration_since(s.used) < SESSION_TTL);
            !stations.is_empty()
        });
        let stations = inner.sessions.entry(listener.to_string()).or_default();
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
        match self.lock().energy_band {
            Some((band, until)) if now < until => Some(band),
            _ => None,
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
        let inner = self.lock();
        let kept = inner.familiar.get(&profile)?;
        (kept.signature == signature && now < kept.until).then(|| kept.boost.clone())
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
        inner.familiar.retain(|_, f| now < f.until);
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
        inner.familiar_locks.retain(|_, l| now < l.until);
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
        self.lock()
            .familiar_locks
            .get(&profile)
            .is_some_and(|l| now < l.until)
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
        listener: &str,
        day: &str,
        station: &str,
        now: Instant,
    ) -> Option<Option<String>> {
        let inner = self.lock();
        let (cover, until) =
            inner
                .covers
                .get(&(listener.to_string(), day.to_string(), station.to_string()))?;
        (now < *until).then(|| cover.clone())
    }

    /// Upstream `dailyCover`'s `SET … NX`: keeps `cover` until the day ends (`day_left`), a "no cover" at most
    /// [`NO_COVER`]; when a live pick is already kept, that one is the answer.
    pub fn put_cover_if_absent(
        &self,
        listener: &str,
        day: &str,
        station: &str,
        cover: Option<String>,
        day_left: Duration,
        now: Instant,
    ) -> Option<String> {
        let mut inner = self.lock();
        inner.covers.retain(|_, (_, until)| now < *until);
        let key = (listener.to_string(), day.to_string(), station.to_string());
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
/// until the sender is gone. A lagging receiver missed events, so it drops as well.
pub fn spawn_mood_drop(
    store: Arc<ListenerStore>,
    mut changes: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match changes.recv().await {
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => store.drop_mood_dependent(),
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}
