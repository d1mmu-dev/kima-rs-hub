//! Failed-login limiter (spec §5.7), keyed by (TCP peer IP, lower-cased username).
//!
//! Failures 1-4 are free. From the 5th consecutive failure on, the key is blocked for
//! `min(2^(f-4), 900)` seconds (2 s, 4 s, 8 s, ... up to 15 minutes). A success clears the key,
//! and a key with no failure for 15 minutes is forgotten. All methods take an explicit `now`
//! where time matters, so tests need no sleeping.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const FREE_FAILURES: u32 = 5;
pub const MAX_BLOCK: Duration = Duration::from_secs(900);
pub const FORGET_AFTER: Duration = Duration::from_secs(900);
pub const MAX_KEYS: usize = 100_000;

/// A refused attempt: try again after `retry_after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocked {
    pub retry_after: Duration,
}

impl Blocked {
    /// Whole seconds, rounded up, at least 1.
    pub fn retry_after_secs(&self) -> u64 {
        let secs = self.retry_after.as_secs() + u64::from(self.retry_after.subsec_nanos() > 0);
        secs.max(1)
    }
}

struct Entry {
    failures: u32,
    last_failure: Instant,
    blocked_until: Option<Instant>,
}

impl Entry {
    /// Idle for `FORGET_AFTER`, counted from the later of the last failure and the end of the
    /// block, so a maximum block is always served in full before the count can restart.
    fn forgotten(&self, now: Instant) -> bool {
        let since = self
            .blocked_until
            .map_or(self.last_failure, |until| until.max(self.last_failure));
        now.saturating_duration_since(since) >= FORGET_AFTER
    }
}

type Key = (IpAddr, String);

pub struct LoginLimiter {
    max_keys: usize,
    entries: Mutex<HashMap<Key, Entry>>,
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::with_capacity(MAX_KEYS)
    }

    pub fn with_capacity(max_keys: usize) -> Self {
        Self {
            max_keys: max_keys.max(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// IPv6 peers share one key per /64 (a host can rotate through its whole prefix), and an
    /// IPv4-mapped IPv6 address is its IPv4 form.
    fn network(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V4(_) => ip,
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => {
                    let s = v6.segments();
                    IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
                }
            },
        }
    }

    fn key(ip: IpAddr, username: &str) -> Key {
        (Self::network(ip), username.to_lowercase())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether an attempt may go ahead. Never extends a block.
    pub fn check(&self, ip: IpAddr, username: &str, now: Instant) -> Result<(), Blocked> {
        self.check_key(&Self::key(ip, username), now)
    }

    fn check_key(&self, key: &Key, now: Instant) -> Result<(), Blocked> {
        let mut entries = self.lock();
        let Some(entry) = entries.get(key) else {
            return Ok(());
        };
        if entry.forgotten(now) {
            entries.remove(key);
            return Ok(());
        }
        match entry.blocked_until {
            Some(until) if until > now => Err(Blocked {
                retry_after: until - now,
            }),
            _ => Ok(()),
        }
    }

    /// Starts one password attempt. Under a single lock it refuses a blocked key, and otherwise
    /// counts the attempt as a failure right away, so parallel or abandoned guesses cannot
    /// slip past the backoff. Call [`Attempt::success`] when the password was right; dropping
    /// the attempt (wrong password, client hang-up, error) leaves the failure counted.
    ///
    /// A blocked key may still be evicted when the store is full (oldest failure first): making
    /// blocked keys permanent would let junk keys fill the store and disable the limiter.
    pub fn begin(&self, ip: IpAddr, username: &str, now: Instant) -> Result<Attempt<'_>, Blocked> {
        self.begin_key(Self::key(ip, username), now)
    }

    fn begin_key(&self, key: Key, now: Instant) -> Result<Attempt<'_>, Blocked> {
        let mut entries = self.lock();
        if let Some(entry) = entries.get(&key) {
            if entry.forgotten(now) {
                entries.remove(&key);
            } else if let Some(until) = entry.blocked_until
                && until > now
            {
                return Err(Blocked {
                    retry_after: until - now,
                });
            }
        }
        if !entries.contains_key(&key) && entries.len() >= self.max_keys {
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_failure)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        let entry = entries.entry(key.clone()).or_insert(Entry {
            failures: 0,
            last_failure: now,
            blocked_until: None,
        });
        entry.failures = entry.failures.saturating_add(1);
        entry.last_failure = now;
        if entry.failures >= FREE_FAILURES {
            let exponent = (entry.failures - (FREE_FAILURES - 1)).min(20);
            let block = Duration::from_secs(1u64 << exponent).min(MAX_BLOCK);
            entry.blocked_until = Some(now + block);
        }
        Ok(Attempt { limiter: self, key })
    }

    /// Starts one attempt on the typed name plus further keys (the database's fold of it).
    /// Every distinct key is checked first; when any is blocked the longest block is returned
    /// and nothing is charged. Otherwise each distinct key is charged once.
    pub fn begin_all(
        &self,
        ip: IpAddr,
        names: &[&str],
        now: Instant,
    ) -> Result<Vec<Attempt<'_>>, Blocked> {
        let mut keys: Vec<Key> = Vec::with_capacity(names.len());
        for name in names {
            let key = Self::key(ip, name);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let mut longest: Option<Blocked> = None;
        for key in &keys {
            if let Err(blocked) = self.check_key(key, now)
                && longest.is_none_or(|l| blocked.retry_after > l.retry_after)
            {
                longest = Some(blocked);
            }
        }
        if let Some(blocked) = longest {
            return Err(blocked);
        }
        keys.iter()
            .map(|key| self.begin_key(key.clone(), now))
            .collect()
    }

    fn clear(&self, key: &Key) {
        self.lock().remove(key);
    }
}

/// One counted password attempt (see [`LoginLimiter::begin`]).
#[must_use = "call success() when the password was right"]
pub struct Attempt<'a> {
    limiter: &'a LoginLimiter,
    key: Key,
}

impl Attempt<'_> {
    /// The password was right: forget the key.
    pub fn success(self) {
        self.limiter.clear(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    const OTHER_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

    /// Fails `times` attempts, waiting out any block in between. Returns the time of the last.
    fn fail(limiter: &LoginLimiter, user: &str, times: u32, mut now: Instant) -> Instant {
        for _ in 0..times {
            if let Err(blocked) = limiter.check(IP, user, now) {
                now += blocked.retry_after;
            }
            drop(limiter.begin(IP, user, now).unwrap());
        }
        now
    }

    fn secs(limiter: &LoginLimiter, user: &str, now: Instant) -> Option<u64> {
        limiter
            .check(IP, user, now)
            .err()
            .map(|blocked| blocked.retry_after.as_secs())
    }

    fn blocked_five(limiter: &LoginLimiter, user: &str, now: Instant) {
        fail(limiter, user, 5, now);
        assert!(limiter.check(IP, user, now).is_err());
    }

    #[test]
    fn begin_all_blocked_second_key_charges_nothing() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        blocked_five(&limiter, "b", now);
        for _ in 0..10 {
            assert!(limiter.begin_all(IP, &["a", "b"], now).is_err());
        }
        fail(&limiter, "a", 4, now);
        assert!(limiter.check(IP, "a", now).is_ok());
    }

    #[test]
    fn begin_all_blocked_first_key_charges_nothing() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        blocked_five(&limiter, "b", now);
        for _ in 0..10 {
            assert!(limiter.begin_all(IP, &["b", "a"], now).is_err());
        }
        fail(&limiter, "a", 4, now);
        assert!(limiter.check(IP, "a", now).is_ok());
    }

    #[test]
    fn begin_all_charges_each_distinct_key_once() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        drop(limiter.begin_all(IP, &["a", "a"], now).unwrap());
        // One failure counted: three more stay free, the fifth blocks.
        fail(&limiter, "a", 3, now);
        assert!(limiter.check(IP, "a", now).is_ok());
        drop(limiter.begin(IP, "a", now).unwrap());
        assert!(limiter.check(IP, "a", now).is_err());
    }

    #[test]
    fn five_failures_are_free() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        for _ in 0..4 {
            drop(limiter.begin(IP, "alice", now).unwrap());
            assert!(limiter.check(IP, "alice", now).is_ok());
        }
        // The 5th attempt still goes ahead, but the key is blocked from then on.
        drop(limiter.begin(IP, "alice", now).unwrap());
        let blocked = limiter.check(IP, "alice", now).unwrap_err();
        assert_eq!(blocked.retry_after, Duration::from_secs(2));
        assert_eq!(blocked.retry_after_secs(), 2);
        assert!(limiter.begin(IP, "alice", now).is_err(), "6th is refused");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let now = Instant::now();
        for (failures, expected) in [(5, 2), (6, 4), (7, 8), (13, 512), (14, 900), (40, 900)] {
            let limiter = LoginLimiter::new();
            let last = fail(&limiter, "alice", failures, now);
            assert_eq!(
                secs(&limiter, "alice", last),
                Some(expected),
                "f={failures}"
            );
        }
    }

    #[test]
    fn block_expires() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        let now = fail(&limiter, "alice", 5, now);
        let blocked = limiter.check(IP, "alice", now).unwrap_err();
        assert!(
            limiter
                .check(IP, "alice", now + blocked.retry_after)
                .is_ok()
        );
    }

    #[test]
    fn blocked_begin_does_not_extend_or_count() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        let now = fail(&limiter, "alice", 5, now);
        for _ in 0..50 {
            assert!(limiter.begin(IP, "alice", now).is_err());
            assert!(limiter.check(IP, "alice", now).is_err());
        }
        let later = now + Duration::from_millis(1500);
        let blocked = limiter.check(IP, "alice", later).unwrap_err();
        assert_eq!(blocked.retry_after, Duration::from_millis(500));
        assert_eq!(blocked.retry_after_secs(), 1);
        // The next attempt is the 6th failure (4 s), so the 50 refused ones were not counted.
        let after = now + Duration::from_secs(2);
        drop(limiter.begin(IP, "alice", after).unwrap());
        assert_eq!(secs(&limiter, "alice", after), Some(4));
    }

    #[test]
    fn cap_holds_when_retried_right_after_the_block() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        let last = fail(&limiter, "alice", 14, now);
        assert_eq!(secs(&limiter, "alice", last), Some(900));
        // Retrying the instant the block ends: not reset, and the next block is the cap again.
        let retry = last + MAX_BLOCK;
        assert!(limiter.check(IP, "alice", retry).is_ok());
        drop(limiter.begin(IP, "alice", retry).unwrap());
        assert_eq!(secs(&limiter, "alice", retry), Some(900));
        // Forgotten only 900 s after the block ends.
        assert!(
            limiter
                .check(IP, "alice", retry + MAX_BLOCK + FORGET_AFTER)
                .is_ok()
        );
        drop(
            limiter
                .begin(IP, "alice", retry + MAX_BLOCK + FORGET_AFTER)
                .unwrap(),
        );
        assert!(
            limiter
                .check(IP, "alice", retry + MAX_BLOCK + FORGET_AFTER)
                .is_ok()
        );
    }

    #[test]
    fn success_clears() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "alice", 4, now);
        limiter.begin(IP, "alice", now).unwrap().success();
        assert!(limiter.check(IP, "alice", now).is_ok());
        fail(&limiter, "alice", 4, now);
        assert!(limiter.check(IP, "alice", now).is_ok());
    }

    #[test]
    fn other_ip_and_other_user_unaffected() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "alice", 5, now);
        assert!(limiter.begin(OTHER_IP, "alice", now).is_ok());
        assert!(limiter.begin(IP, "bob", now).is_ok());
    }

    #[test]
    fn ipv6_peers_share_a_key_per_64() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let same_64: IpAddr = "2001:db8:1:2:ffff:eeee:dddd:cccc".parse().unwrap();
        let other_64: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        for _ in 0..5 {
            drop(limiter.begin(a, "alice", now).unwrap());
        }
        assert!(limiter.begin(same_64, "alice", now).is_err());
        assert!(limiter.begin(other_64, "alice", now).is_ok());
    }

    #[test]
    fn mapped_ipv4_equals_plain_ipv4() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        let mapped: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        fail(&limiter, "alice", 5, now);
        assert!(limiter.begin(mapped, "alice", now).is_err());
        let other_mapped: IpAddr = "::ffff:10.0.0.2".parse().unwrap();
        assert!(limiter.begin(other_mapped, "alice", now).is_ok());
    }

    #[test]
    fn username_is_case_insensitive() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "Alice", 5, now);
        assert!(limiter.begin(IP, "alice", now).is_err());
        assert!(limiter.begin(IP, "ALICE", now).is_err());
    }

    #[test]
    fn idle_key_forgotten() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "alice", 5, now);
        let later = now + Duration::from_secs(903);
        assert!(limiter.check(IP, "alice", later).is_ok());
        drop(limiter.begin(IP, "alice", later).unwrap());
        assert!(limiter.check(IP, "alice", later).is_ok(), "count restarted");
    }

    #[test]
    fn full_store_evicts_oldest_failure() {
        let (limiter, now) = (LoginLimiter::with_capacity(3), Instant::now());
        let ms = Duration::from_millis;
        fail(&limiter, "a", 5, now + ms(100));
        fail(&limiter, "b", 5, now);
        fail(&limiter, "c", 5, now + ms(200));
        fail(&limiter, "d", 5, now + ms(300));
        let at = now + ms(300);
        assert!(limiter.check(IP, "b", at).is_ok(), "oldest evicted");
        for kept in ["a", "c", "d"] {
            assert!(limiter.check(IP, kept, at).is_err(), "{kept} kept");
        }
    }
}
