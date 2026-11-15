//! Subsonic "now playing", kept in memory (spec §6.3): `scrobble` with `submission=false` records
//! `(profile, player) -> track`, and `getNowPlaying` reads it next to the web players' state.
//! Slots are keyed by profile (D-244): two profiles of one account playing on players of the
//! same name are two entries.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kima_core::profile::ProfileId;
use uuid::Uuid;

/// How long past the track's length an entry stays.
const GRACE: Duration = Duration::from_secs(60);

/// Most slots one profile holds; a new one past this evicts that profile's oldest-started.
pub const MAX_SLOTS_PER_PROFILE: usize = 10;
/// Most slots overall; at the cap the oldest-started one anywhere is evicted.
pub const MAX_SLOTS: usize = 10_000;
/// A track's length counts for at most this long when computing the expiry.
pub const MAX_TRACK_SECS: u64 = 2 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub profile: ProfileId,
    pub player: String,
    pub track: Uuid,
    pub started: Instant,
}

#[derive(Debug)]
struct Slot {
    entry: Entry,
    expires: Instant,
}

type Key = (ProfileId, String);

/// One slot per (profile, player).
#[derive(Debug, Default)]
pub struct NowPlaying {
    slots: Mutex<HashMap<Key, Slot>>,
}

/// A stable small id for a player name: the 32-bit FNV-1a hash of its bytes, masked to 31 bits so it fits the signed
/// 32-bit `int` the Subsonic schema gives `playerId`.
pub fn player_id(player: &str) -> i64 {
    let mut hash: u32 = 2_166_136_261;
    for byte in player.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    i64::from(hash & 0x7fff_ffff)
}

impl NowPlaying {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Slot>> {
        // Nothing here can leave the map half-updated, so a poisoned lock is still good.
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records that `player` of `profile` started `track` now. Entries that have expired are
    /// dropped on the way, which keeps the map from growing with player names.
    pub fn report(
        &self,
        profile: ProfileId,
        player: &str,
        track: Uuid,
        duration_secs: i32,
        now: Instant,
    ) {
        let length = Duration::from_secs(
            u64::try_from(duration_secs)
                .unwrap_or(0)
                .min(MAX_TRACK_SECS),
        );
        let mut slots = self.lock();
        slots.retain(|_, slot| slot.expires > now);
        let key = (profile, player.to_owned());
        slots.remove(&key);
        let oldest = |slots: &HashMap<Key, Slot>, mine: Option<ProfileId>| {
            slots
                .iter()
                .filter(|(k, _)| mine.is_none_or(|u| k.0 == u))
                .min_by(|a, b| (a.1.entry.started, a.0).cmp(&(b.1.entry.started, b.0)))
                .map(|(k, _)| k.clone())
        };
        if slots.keys().filter(|k| k.0 == profile).count() >= MAX_SLOTS_PER_PROFILE
            && let Some(k) = oldest(&slots, Some(profile))
        {
            slots.remove(&k);
        }
        if slots.len() >= MAX_SLOTS
            && let Some(k) = oldest(&slots, None)
        {
            slots.remove(&k);
        }
        slots.insert(
            key,
            Slot {
                entry: Entry {
                    profile,
                    player: player.to_owned(),
                    track,
                    started: now,
                },
                expires: now + length + GRACE,
            },
        );
    }

    /// The player scrobbled a play: it is no longer "now playing".
    pub fn finish(&self, profile: ProfileId, player: &str) {
        self.lock().remove(&(profile, player.to_owned()));
    }

    /// Drops every slot of these profiles (deleted ones: `getNowPlaying` finds no owner).
    pub fn drop_profiles(&self, gone: &[ProfileId]) {
        if !gone.is_empty() {
            self.lock()
                .retain(|(profile, _), _| !gone.contains(profile));
        }
    }

    /// Live entries, newest first.
    pub fn entries(&self, now: Instant) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .lock()
            .values()
            .filter(|slot| slot.expires > now)
            .map(|slot| slot.entry.clone())
            .collect();
        out.sort_by(|a, b| b.started.cmp(&a.started).then(a.player.cmp(&b.player)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid() -> ProfileId {
        ProfileId::new(Uuid::now_v7())
    }

    #[test]
    fn two_profiles_on_one_player_name_are_two_slots_and_drop_apart() {
        let np = NowPlaying::new();
        let (main, kids, track) = (pid(), pid(), Uuid::now_v7());
        let t0 = Instant::now();
        np.report(main, "Symfonium", track, 200, t0);
        np.report(kids, "Symfonium", track, 200, t0);
        assert_eq!(np.entries(t0).len(), 2);
        np.finish(kids, "Symfonium");
        let left: Vec<ProfileId> = np.entries(t0).iter().map(|e| e.profile).collect();
        assert_eq!(left, vec![main]);
        np.report(kids, "Feishin", track, 200, t0);
        np.drop_profiles(&[kids]);
        let left: Vec<ProfileId> = np.entries(t0).iter().map(|e| e.profile).collect();
        assert_eq!(left, vec![main]);
    }

    #[test]
    fn a_profile_holds_at_most_ten_slots_oldest_evicted() {
        let np = NowPlaying::new();
        let (user, other, track) = (pid(), pid(), Uuid::now_v7());
        let t0 = Instant::now();
        np.report(other, "Phone", track, 200, t0);
        for i in 0..=MAX_SLOTS_PER_PROFILE {
            let at = t0 + Duration::from_secs(i as u64);
            np.report(user, &format!("p{i}"), track, 200, at);
        }
        let now = t0 + Duration::from_secs(60);
        let mine: Vec<String> = np
            .entries(now)
            .into_iter()
            .filter(|e| e.profile == user)
            .map(|e| e.player)
            .collect();
        assert_eq!(mine.len(), MAX_SLOTS_PER_PROFILE);
        assert!(!mine.contains(&"p0".to_owned()));
        assert!(mine.contains(&"p10".to_owned()));
        // Another profile's slot is untouched.
        assert!(np.entries(now).iter().any(|e| e.profile == other));
        // Re-reporting an existing player is not a new slot.
        np.report(user, "p5", track, 200, t0 + Duration::from_secs(30));
        assert_eq!(
            np.entries(now).iter().filter(|e| e.profile == user).count(),
            10
        );
    }

    #[test]
    fn the_global_cap_evicts_the_oldest_started() {
        let np = NowPlaying::new();
        let track = Uuid::now_v7();
        let t0 = Instant::now();
        let first = pid();
        np.report(first, "p", track, 200, t0);
        for i in 1..MAX_SLOTS {
            let at = t0 + Duration::from_millis(i as u64);
            np.report(pid(), "p", track, 200, at);
        }
        let now = t0 + Duration::from_secs(1);
        assert_eq!(np.entries(now).len(), MAX_SLOTS);
        let late = pid();
        np.report(late, "p", track, 200, t0 + Duration::from_secs(1));
        let all = np.entries(now);
        assert_eq!(all.len(), MAX_SLOTS);
        assert!(all.iter().all(|e| e.profile != first));
        assert!(all.iter().any(|e| e.profile == late));
    }

    #[test]
    fn expiry_is_capped_for_absurd_durations() {
        let np = NowPlaying::new();
        let (user, track) = (pid(), Uuid::now_v7());
        let t0 = Instant::now();
        np.report(user, "p", track, i32::MAX, t0);
        let cap = Duration::from_secs(MAX_TRACK_SECS) + GRACE;
        assert_eq!(np.entries(t0 + cap - Duration::from_secs(1)).len(), 1);
        assert!(np.entries(t0 + cap).is_empty());
    }

    #[test]
    fn player_id_is_fnv1a() {
        // Reference values of 32-bit FNV-1a.
        assert_eq!(player_id(""), 2_166_136_261 & 0x7fff_ffff);
        assert_eq!(player_id("a"), 0xe40c_292c & 0x7fff_ffff);
        assert_eq!(player_id("foobar"), 0xbf9c_f968 & 0x7fff_ffff);
        assert_eq!(player_id("Symfonium"), 3_152_842_248 & 0x7fff_ffff);
    }
}
