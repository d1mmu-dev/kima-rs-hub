//! SSE tickets (single-use, short-lived) and the per-profile cap on open streams (D-245).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kima_core::profile::{AccountId, ProfileId};
use uuid::Uuid;

pub const TICKET_TTL: Duration = Duration::from_secs(30);
/// Open event streams per profile (upstream: per user; each profile is a listener of its own).
pub const MAX_STREAMS_PER_PROFILE: usize = 5;

/// Who a ticket was issued to: a profile, and the account it belongs to (an account-wide
/// revocation reaches the streams of all its profiles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOwner {
    pub account: AccountId,
    pub profile: ProfileId,
}

pub struct SseRegistry {
    ttl: Duration,
    tickets: Mutex<HashMap<String, (StreamOwner, Instant)>>,
    open: Mutex<HashMap<ProfileId, usize>>,
}

/// Holds one of a profile's stream slots; dropping it (stream closed) frees the slot.
pub struct StreamSlot {
    registry: Arc<SseRegistry>,
    profile: ProfileId,
}

impl SseRegistry {
    pub fn new() -> Self {
        Self::with_ttl(TICKET_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            tickets: Mutex::default(),
            open: Mutex::default(),
        }
    }

    pub fn issue_ticket(&self, owner: StreamOwner) -> String {
        let ticket = Uuid::new_v4().simple().to_string();
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap();
        tickets.retain(|_, (_, issued)| now.duration_since(*issued) < self.ttl);
        tickets.insert(ticket.clone(), (owner, now));
        ticket
    }

    /// Forgets every outstanding ticket of every profile of an account (its sessions were
    /// revoked).
    pub fn revoke_account(&self, account: AccountId) {
        self.tickets
            .lock()
            .unwrap()
            .retain(|_, (owner, _)| owner.account != account);
    }

    pub fn redeem_ticket(&self, ticket: &str) -> Option<StreamOwner> {
        let (owner, issued) = self.tickets.lock().unwrap().remove(ticket)?;
        (issued.elapsed() < self.ttl).then_some(owner)
    }

    pub fn try_open(self: &Arc<Self>, profile: ProfileId) -> Option<StreamSlot> {
        let mut open = self.open.lock().unwrap();
        let count = open.entry(profile).or_insert(0);
        if *count >= MAX_STREAMS_PER_PROFILE {
            return None;
        }
        *count += 1;
        Some(StreamSlot {
            registry: Arc::clone(self),
            profile,
        })
    }

    pub fn open_streams(&self, profile: ProfileId) -> usize {
        self.open
            .lock()
            .unwrap()
            .get(&profile)
            .copied()
            .unwrap_or(0)
    }
}

impl Default for SseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut open = self.registry.open.lock().unwrap();
        if let Some(count) = open.get_mut(&self.profile) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.profile);
            }
        }
    }
}
