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

    fn key(ip: IpAddr, username: &str) -> Key {
        (ip, username.to_lowercase())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether an attempt may go ahead. Never extends a block.
    pub fn check(&self, ip: IpAddr, username: &str, now: Instant) -> Result<(), Blocked> {
        let key = Self::key(ip, username);
        let mut entries = self.lock();
        let Some(entry) = entries.get(&key) else {
            return Ok(());
        };
        if now.saturating_duration_since(entry.last_failure) >= FORGET_AFTER {
            entries.remove(&key);
            return Ok(());
        }
        match entry.blocked_until {
            Some(until) if until > now => Err(Blocked {
                retry_after: until - now,
            }),
            _ => Ok(()),
        }
    }

    pub fn record_failure(&self, ip: IpAddr, username: &str, now: Instant) {
        let key = Self::key(ip, username);
        let mut entries = self.lock();
        if let Some(entry) = entries.get(&key)
            && now.saturating_duration_since(entry.last_failure) >= FORGET_AFTER
        {
            entries.remove(&key);
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
        let entry = entries.entry(key).or_insert(Entry {
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
    }

    pub fn record_success(&self, ip: IpAddr, username: &str) {
        self.lock().remove(&Self::key(ip, username));
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    const OTHER_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

    fn fail(limiter: &LoginLimiter, user: &str, times: u32, now: Instant) {
        for _ in 0..times {
            limiter.record_failure(IP, user, now);
        }
    }

    fn secs(limiter: &LoginLimiter, user: &str, now: Instant) -> Option<u64> {
        limiter
            .check(IP, user, now)
            .err()
            .map(|blocked| blocked.retry_after.as_secs())
    }

    #[test]
    fn five_failures_are_free() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        for _ in 0..4 {
            limiter.record_failure(IP, "alice", now);
            assert!(limiter.check(IP, "alice", now).is_ok());
        }
        limiter.record_failure(IP, "alice", now);
        let blocked = limiter.check(IP, "alice", now).unwrap_err();
        assert_eq!(blocked.retry_after, Duration::from_secs(2));
        assert_eq!(blocked.retry_after_secs(), 2);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let now = Instant::now();
        for (failures, expected) in [(5, 2), (6, 4), (7, 8), (13, 512), (14, 900), (40, 900)] {
            let limiter = LoginLimiter::new();
            fail(&limiter, "alice", failures, now);
            assert_eq!(secs(&limiter, "alice", now), Some(expected), "f={failures}");
        }
    }

    #[test]
    fn block_expires() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "alice", 5, now);
        let blocked = limiter.check(IP, "alice", now).unwrap_err();
        assert!(
            limiter
                .check(IP, "alice", now + blocked.retry_after)
                .is_ok()
        );
    }

    #[test]
    fn blocked_check_does_not_extend() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "alice", 5, now);
        for _ in 0..50 {
            assert!(limiter.check(IP, "alice", now).is_err());
        }
        let later = now + Duration::from_millis(1500);
        let blocked = limiter.check(IP, "alice", later).unwrap_err();
        assert_eq!(blocked.retry_after, Duration::from_millis(500));
        assert_eq!(blocked.retry_after_secs(), 1);
        assert!(
            limiter
                .check(IP, "alice", now + Duration::from_secs(2))
                .is_ok()
        );
    }

    #[test]
    fn success_clears() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "alice", 5, now);
        limiter.record_success(IP, "alice");
        assert!(limiter.check(IP, "alice", now).is_ok());
        fail(&limiter, "alice", 4, now);
        assert!(limiter.check(IP, "alice", now).is_ok());
    }

    #[test]
    fn other_ip_and_other_user_unaffected() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "alice", 5, now);
        assert!(limiter.check(OTHER_IP, "alice", now).is_ok());
        assert!(limiter.check(IP, "bob", now).is_ok());
    }

    #[test]
    fn username_is_case_insensitive() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "Alice", 5, now);
        assert!(limiter.check(IP, "alice", now).is_err());
        assert!(limiter.check(IP, "ALICE", now).is_err());
    }

    #[test]
    fn idle_key_forgotten() {
        let (limiter, now) = (LoginLimiter::new(), Instant::now());
        fail(&limiter, "alice", 5, now);
        let later = now + Duration::from_secs(901);
        assert!(limiter.check(IP, "alice", later).is_ok());
        limiter.record_failure(IP, "alice", later);
        assert!(limiter.check(IP, "alice", later).is_ok(), "count restarted");
    }

    #[test]
    fn full_store_evicts_oldest_failure() {
        let (limiter, now) = (LoginLimiter::with_capacity(3), Instant::now());
        fail(&limiter, "a", 7, now + Duration::from_secs(1));
        fail(&limiter, "b", 7, now);
        fail(&limiter, "c", 7, now + Duration::from_secs(2));
        fail(&limiter, "d", 7, now + Duration::from_secs(3));
        let at = now + Duration::from_secs(3);
        assert!(limiter.check(IP, "b", at).is_ok(), "oldest evicted");
        for kept in ["a", "c", "d"] {
            assert!(limiter.check(IP, kept, at).is_err(), "{kept} kept");
        }
    }
}
