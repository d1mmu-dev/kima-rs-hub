//! Subsonic "now playing", kept in memory (spec §6.3): `scrobble` with `submission=false` records
//! `(user, player) -> track`, and `getNowPlaying` reads it next to the web players' state.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// How long past the track's length an entry stays.
const GRACE: Duration = Duration::from_secs(60);

/// Most slots one user holds; a new one past this evicts that user's oldest-started.
pub const MAX_SLOTS_PER_USER: usize = 10;
/// Most slots overall; at the cap the oldest-started one anywhere is evicted.
pub const MAX_SLOTS: usize = 10_000;
/// A track's length counts for at most this long when computing the expiry.
pub const MAX_TRACK_SECS: u64 = 2 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub user: Uuid,
    pub username: String,
    pub player: String,
    pub track: Uuid,
    pub started: Instant,
}

#[derive(Debug)]
struct Slot {
    entry: Entry,
    expires: Instant,
}

/// One slot per (user, player).
#[derive(Debug, Default)]
pub struct NowPlaying {
    slots: Mutex<HashMap<(Uuid, String), Slot>>,
}

/// A stable small id for a player name: the 32-bit FNV-1a hash of its bytes.
pub fn player_id(player: &str) -> i64 {
    let mut hash: u32 = 2_166_136_261;
    for byte in player.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    i64::from(hash)
}

impl NowPlaying {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(Uuid, String), Slot>> {
        // Nothing here can leave the map half-updated, so a poisoned lock is still good.
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records that `player` of `user` started `track` now. Entries that have expired are
    /// dropped on the way, which keeps the map from growing with player names.
    pub fn report(
        &self,
        user: Uuid,
        username: &str,
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
        let key = (user, player.to_owned());
        slots.remove(&key);
        let oldest = |slots: &HashMap<(Uuid, String), Slot>, mine: Option<Uuid>| {
            slots
                .iter()
                .filter(|(k, _)| mine.is_none_or(|u| k.0 == u))
                .min_by(|a, b| (a.1.entry.started, a.0).cmp(&(b.1.entry.started, b.0)))
                .map(|(k, _)| k.clone())
        };
        if slots.keys().filter(|k| k.0 == user).count() >= MAX_SLOTS_PER_USER
            && let Some(k) = oldest(&slots, Some(user))
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
                    user,
                    username: username.to_owned(),
                    player: player.to_owned(),
                    track,
                    started: now,
                },
                expires: now + length + GRACE,
            },
        );
    }

    /// The player scrobbled a play: it is no longer "now playing".
    pub fn finish(&self, user: Uuid, player: &str) {
        self.lock().remove(&(user, player.to_owned()));
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

    #[test]
    fn a_user_holds_at_most_ten_slots_oldest_evicted() {
        let np = NowPlaying::new();
        let (user, other, track) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let t0 = Instant::now();
        np.report(other, "bob", "Phone", track, 200, t0);
        for i in 0..=MAX_SLOTS_PER_USER {
            let at = t0 + Duration::from_secs(i as u64);
            np.report(user, "alice", &format!("p{i}"), track, 200, at);
        }
        let now = t0 + Duration::from_secs(60);
        let mine: Vec<String> = np
            .entries(now)
            .into_iter()
            .filter(|e| e.user == user)
            .map(|e| e.player)
            .collect();
        assert_eq!(mine.len(), MAX_SLOTS_PER_USER);
        assert!(!mine.contains(&"p0".to_owned()));
        assert!(mine.contains(&"p10".to_owned()));
        // Another user's slot is untouched.
        assert!(np.entries(now).iter().any(|e| e.user == other));
        // Re-reporting an existing player is not a new slot.
        np.report(
            user,
            "alice",
            "p5",
            track,
            200,
            t0 + Duration::from_secs(30),
        );
        assert_eq!(
            np.entries(now).iter().filter(|e| e.user == user).count(),
            10
        );
    }

    #[test]
    fn the_global_cap_evicts_the_oldest_started() {
        let np = NowPlaying::new();
        let track = Uuid::now_v7();
        let t0 = Instant::now();
        let first = Uuid::now_v7();
        np.report(first, "first", "p", track, 200, t0);
        for i in 1..MAX_SLOTS {
            let at = t0 + Duration::from_millis(i as u64);
            np.report(Uuid::now_v7(), "u", "p", track, 200, at);
        }
        let now = t0 + Duration::from_secs(1);
        assert_eq!(np.entries(now).len(), MAX_SLOTS);
        np.report(
            Uuid::now_v7(),
            "late",
            "p",
            track,
            200,
            t0 + Duration::from_secs(1),
        );
        let all = np.entries(now);
        assert_eq!(all.len(), MAX_SLOTS);
        assert!(all.iter().all(|e| e.user != first));
        assert!(all.iter().any(|e| e.username == "late"));
    }

    #[test]
    fn expiry_is_capped_for_absurd_durations() {
        let np = NowPlaying::new();
        let (user, track) = (Uuid::now_v7(), Uuid::now_v7());
        let t0 = Instant::now();
        np.report(user, "alice", "p", track, i32::MAX, t0);
        let cap = Duration::from_secs(MAX_TRACK_SECS) + GRACE;
        assert_eq!(np.entries(t0 + cap - Duration::from_secs(1)).len(), 1);
        assert!(np.entries(t0 + cap).is_empty());
    }

    #[test]
    fn player_id_is_fnv1a() {
        // Reference values of 32-bit FNV-1a.
        assert_eq!(player_id(""), 2_166_136_261);
        assert_eq!(player_id("a"), 0xe40c_292c);
        assert_eq!(player_id("foobar"), 0xbf9c_f968);
    }
}
