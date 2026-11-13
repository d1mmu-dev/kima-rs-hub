//! Per-source admission: a resizable concurrency cap and a GCRA token bucket.
//!
//! Both limits use tokio's clock, including in paused-clock tests. A waiter does not
//! reserve either a slot or a future cell: both are committed together on admission.

use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

use crate::source::Limits;

/// No slot or cell within the allowed wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

#[derive(Debug)]
struct State {
    in_flight: u32,
    cap: u32,
    tat: Option<Instant>,
    next_id: u64,
    queue: Vec<(u64, Arc<AtomicBool>)>,
}

/// One source's limiter: at most `cap` requests in flight, `burst` per `period`.
#[derive(Debug)]
pub struct Gate {
    interval: Duration,
    tolerance: Duration,
    core: Arc<Core>,
}

#[derive(Debug)]
struct Core {
    state: Mutex<State>,
    changed: Notify,
}

/// A request's priority, shared with later joiners of the same flight.
#[derive(Debug)]
pub struct Priority {
    interactive: Arc<AtomicBool>,
    gates: Mutex<Vec<Weak<Core>>>,
}

impl Priority {
    pub fn background() -> Self {
        Self {
            interactive: Arc::new(AtomicBool::new(false)),
            gates: Mutex::new(Vec::new()),
        }
    }
    pub fn interactive() -> Self {
        Self {
            interactive: Arc::new(AtomicBool::new(true)),
            gates: Mutex::new(Vec::new()),
        }
    }
    pub fn raise(&self) {
        let mut gates = lock(&self.gates);
        let mut cores: Vec<_> = gates.iter().filter_map(Weak::upgrade).collect();
        cores.sort_unstable_by_key(Arc::as_ptr);
        cores.dedup_by(|a, b| Arc::ptr_eq(a, b));
        gates.retain(|weak| weak.strong_count() != 0);
        // Promotion and admission have one linearization point, even if a caller
        // deliberately shares this priority across multiple source gates.
        let guards: Vec<_> = cores.iter().map(|core| lock(&core.state)).collect();
        self.interactive.store(true, Ordering::SeqCst);
        drop(guards);
        for core in &cores {
            core.changed.notify_waiters();
        }
    }
}

struct Waiter {
    core: Arc<Core>,
    id: u64,
}
impl Drop for Waiter {
    fn drop(&mut self) {
        let mut state = lock(&self.core.state);
        state.queue.retain(|(id, _)| *id != self.id);
        drop(state);
        self.core.changed.notify_waiters();
    }
}

/// A held concurrency slot (and a spent cell). Dropping it frees the slot.
#[derive(Debug)]
pub struct GatePermit {
    core: Arc<Core>,
}

impl Drop for GatePermit {
    fn drop(&mut self) {
        let mut state = lock(&self.core.state);
        state.in_flight -= 1;
        drop(state);
        self.core.changed.notify_waiters();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Gate {
    /// `burst` cells per `period`, `concurrency` in flight.
    pub fn new(period: Duration, burst: u32, concurrency: u32) -> Gate {
        let burst = burst.max(1);
        let interval = period / burst;
        Gate {
            interval,
            tolerance: interval * (burst - 1),
            core: Arc::new(Core {
                state: Mutex::new(State {
                    in_flight: 0,
                    cap: concurrency.max(1),
                    tat: None,
                    next_id: 0,
                    queue: Vec::new(),
                }),
                changed: Notify::new(),
            }),
        }
    }

    pub fn for_limits(limits: &Limits) -> Gate {
        Gate::new(limits.period, limits.burst, limits.concurrency)
    }

    /// Changes the concurrency cap. Existing holders are allowed to finish.
    pub fn set_concurrency(&self, cap: u32) {
        lock(&self.core.state).cap = cap.max(1);
        self.core.changed.notify_waiters();
    }

    /// The concurrency cap now.
    pub fn concurrency(&self) -> u32 {
        lock(&self.core.state).cap
    }

    /// Number of registered waiting requests.
    pub fn queued(&self) -> usize {
        lock(&self.core.state).queue.len()
    }

    /// Snapshot of registered waiter priorities, in registration order.
    #[doc(hidden)]
    pub fn queued_priorities(&self) -> Vec<bool> {
        lock(&self.core.state)
            .queue
            .iter()
            .map(|(_, p)| p.load(Ordering::SeqCst))
            .collect()
    }

    pub async fn acquire(&self, max_wait: Option<Duration>) -> Result<GatePermit, Busy> {
        self.acquire_with_priority(max_wait, &Priority::background())
            .await
    }

    pub async fn acquire_with_priority(
        &self,
        max_wait: Option<Duration>,
        priority: &Priority,
    ) -> Result<GatePermit, Busy> {
        let deadline = max_wait.map(|w| Instant::now() + w);
        let waiter = {
            let mut gates = lock(&priority.gates);
            let mut state = lock(&self.core.state);
            let id = state.next_id;
            state.next_id += 1;
            state.queue.push((id, priority.interactive.clone()));
            gates.retain(|weak| weak.strong_count() != 0);
            if !gates
                .iter()
                .any(|weak| weak.ptr_eq(&Arc::downgrade(&self.core)))
            {
                gates.push(Arc::downgrade(&self.core));
            }
            Waiter {
                core: self.core.clone(),
                id,
            }
        };
        let mut first = true;
        loop {
            let mut changed = pin!(self.core.changed.notified());
            changed.as_mut().enable();
            let now = Instant::now();
            let ready_at = {
                let mut state = lock(&self.core.state);
                let base = state.tat.map_or(now, |t| t.max(now));
                let ready_at = base.checked_sub(self.tolerance).map_or(now, |t| t.max(now));
                if !first && deadline.is_some_and(|d| now > d) {
                    return Err(Busy);
                }
                let selected = state
                    .queue
                    .iter()
                    .find(|(_, p)| p.load(Ordering::SeqCst))
                    .or_else(|| state.queue.first())
                    .is_some_and(|(id, _)| *id == waiter.id);
                if selected && state.in_flight < state.cap && ready_at <= now {
                    state.in_flight += 1;
                    state.tat = Some(base + self.interval);
                    drop(state);
                    drop(waiter);
                    return Ok(GatePermit {
                        core: self.core.clone(),
                    });
                }
                if deadline.is_some_and(|d| ready_at > d || now >= d) {
                    return Err(Busy);
                }
                if state.in_flight < state.cap && ready_at > now {
                    Some(ready_at)
                } else {
                    None
                }
            };
            first = false;
            match (ready_at, deadline) {
                (Some(ready), Some(end)) => {
                    tokio::select! { _ = &mut changed => {}, _ = tokio::time::sleep_until(ready.min(end)) => {}, }
                }
                (Some(ready), None) => {
                    tokio::select! { _ = &mut changed => {}, _ = tokio::time::sleep_until(ready) => {}, }
                }
                (None, Some(end)) => {
                    tokio::select! { _ = &mut changed => {}, _ = tokio::time::sleep_until(end) => {}, }
                }
                (None, None) => changed.await,
            }
        }
    }
}
