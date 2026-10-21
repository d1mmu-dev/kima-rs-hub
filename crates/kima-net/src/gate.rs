//! Per-source admission (spec §7.3): a resizable concurrency cap, then a GCRA token bucket.
//!
//! This stands in for the `governor` crate the spec names: governor's clock does not follow
//! tokio's paused time, and spec §24.3 asks for paused-clock limiter tests. The behaviour is
//! governor's: `burst` cells at once, then one every `period / burst`. Both halves run on
//! `tokio::time`, so a paused test clock drives them.

use std::pin::pin;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

use crate::source::Limits;

/// No slot or cell within the allowed wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

#[derive(Debug)]
struct Slots {
    in_flight: u32,
    cap: u32,
}

/// One source's limiter: at most `cap` requests in flight, `burst` per `period`.
#[derive(Debug)]
pub struct Gate {
    /// Time between cells once the burst is spent: `period / burst`.
    interval: Duration,
    /// How far ahead of now the theoretical arrival time may run: `interval × (burst − 1)`.
    tolerance: Duration,
    /// GCRA's theoretical arrival time of the next cell; `None` before the first.
    tat: Mutex<Option<Instant>>,
    slots: Mutex<Slots>,
    freed: Notify,
}

/// A held concurrency slot (and a spent cell). Dropping it frees the slot.
#[derive(Debug)]
pub struct GatePermit<'a> {
    gate: &'a Gate,
}

impl Drop for GatePermit<'_> {
    fn drop(&mut self) {
        {
            let mut slots = lock(&self.gate.slots);
            slots.in_flight = slots.in_flight.saturating_sub(1);
        }
        self.gate.freed.notify_waiters();
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
            tat: Mutex::new(None),
            slots: Mutex::new(Slots {
                in_flight: 0,
                cap: concurrency.max(1),
            }),
            freed: Notify::new(),
        }
    }

    pub fn for_limits(limits: &Limits) -> Gate {
        Gate::new(limits.period, limits.burst, limits.concurrency)
    }

    /// Changes the concurrency cap. Lowering it lets holders finish; nobody new gets in until
    /// fewer than `cap` are in flight.
    pub fn set_concurrency(&self, cap: u32) {
        lock(&self.slots).cap = cap.max(1);
        self.freed.notify_waiters();
    }

    /// The concurrency cap now.
    pub fn concurrency(&self) -> u32 {
        lock(&self.slots).cap
    }

    /// Waits for a slot, then a cell. With `max_wait`, gives up with [`Busy`] when either is not
    /// to be had within it (a cell further away than that is refused at once).
    pub async fn acquire(&self, max_wait: Option<Duration>) -> Result<GatePermit<'_>, Busy> {
        let deadline = max_wait.map(|w| Instant::now() + w);
        let permit = self.slot(deadline).await?;

        let now = Instant::now();
        let ready_at = {
            let mut tat = lock(&self.tat);
            let base = tat.map_or(now, |t| t.max(now));
            let ready_at = base.checked_sub(self.tolerance).map_or(now, |t| t.max(now));
            if deadline.is_some_and(|d| ready_at > d) {
                return Err(Busy); // `permit` drops: the slot is freed
            }
            *tat = Some(base + self.interval);
            ready_at
        };
        if ready_at > now {
            tokio::time::sleep_until(ready_at).await;
        }
        Ok(permit)
    }

    async fn slot(&self, deadline: Option<Instant>) -> Result<GatePermit<'_>, Busy> {
        loop {
            let mut freed = pin!(self.freed.notified());
            freed.as_mut().enable();
            {
                let mut slots = lock(&self.slots);
                if slots.in_flight < slots.cap {
                    slots.in_flight += 1;
                    return Ok(GatePermit { gate: self });
                }
            }
            match deadline {
                None => freed.await,
                Some(d) => {
                    if tokio::time::timeout_at(d, freed).await.is_err() {
                        return Err(Busy);
                    }
                }
            }
        }
    }
}
