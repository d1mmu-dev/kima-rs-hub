//! SSE tickets (single-use, short-lived) and the per-user cap on open streams.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

pub const TICKET_TTL: Duration = Duration::from_secs(30);
pub const MAX_STREAMS_PER_USER: usize = 5;

pub struct SseRegistry {
    ttl: Duration,
    tickets: Mutex<HashMap<String, (Uuid, Instant)>>,
    open: Mutex<HashMap<Uuid, usize>>,
}

/// Holds one of a user's stream slots; dropping it (stream closed) frees the slot.
pub struct StreamSlot {
    registry: Arc<SseRegistry>,
    user: Uuid,
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

    pub fn issue_ticket(&self, user: Uuid) -> String {
        let ticket = Uuid::new_v4().simple().to_string();
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap();
        tickets.retain(|_, (_, issued)| now.duration_since(*issued) < self.ttl);
        tickets.insert(ticket.clone(), (user, now));
        ticket
    }

    pub fn redeem_ticket(&self, ticket: &str) -> Option<Uuid> {
        let (user, issued) = self.tickets.lock().unwrap().remove(ticket)?;
        (issued.elapsed() < self.ttl).then_some(user)
    }

    pub fn try_open(self: &Arc<Self>, user: Uuid) -> Option<StreamSlot> {
        let mut open = self.open.lock().unwrap();
        let count = open.entry(user).or_insert(0);
        if *count >= MAX_STREAMS_PER_USER {
            return None;
        }
        *count += 1;
        Some(StreamSlot {
            registry: Arc::clone(self),
            user,
        })
    }

    pub fn open_streams(&self, user: Uuid) -> usize {
        self.open.lock().unwrap().get(&user).copied().unwrap_or(0)
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
        if let Some(count) = open.get_mut(&self.user) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.user);
            }
        }
    }
}
