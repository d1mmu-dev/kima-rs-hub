//! Subsonic "now playing", kept in memory (spec §6.3): `scrobble` with `submission=false` records
//! `(user, player) -> track`, and `getNowPlaying` reads it next to the web players' state.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// How long past the track's length an entry stays.
const GRACE: Duration = Duration::from_secs(60);

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
        let length = Duration::from_secs(u64::try_from(duration_secs).unwrap_or(0));
        let mut slots = self.lock();
        slots.retain(|_, slot| slot.expires > now);
        slots.insert(
            (user, player.to_owned()),
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
    fn player_id_is_fnv1a() {
        // Reference values of 32-bit FNV-1a.
        assert_eq!(player_id(""), 2_166_136_261);
        assert_eq!(player_id("a"), 0xe40c_292c);
        assert_eq!(player_id("foobar"), 0xbf9c_f968);
    }
}
