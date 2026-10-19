//! A small cron scheduler: each entry sleeps until its next fire time and calls its function.
//!
//! Missed fires (the process was down) are not caught up; the startup reconcile covers that.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[derive(Debug, PartialEq, Eq)]
pub struct ScheduleError(String);

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid cron expression: {}", self.0)
    }
}

impl std::error::Error for ScheduleError {}

/// A standard 5-field cron expression (minute, hour, day of month, month, day of week),
/// evaluated in UTC.
#[derive(Debug, Clone)]
pub struct Cron(croner::Cron);

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, ScheduleError> {
        let expr = expr.trim();
        if expr.split_whitespace().count() != 5 {
            return Err(ScheduleError(format!(
                "{expr:?}: expected 5 fields (minute hour day-of-month month day-of-week)"
            )));
        }
        croner::Cron::new(expr)
            .parse()
            .map(Cron)
            .map_err(|e| ScheduleError(format!("{expr:?}: {e}")))
    }

    /// The first fire time strictly after `t`, or `None` when the expression never fires again.
    pub fn next_after(&self, t: OffsetDateTime) -> Option<OffsetDateTime> {
        // Fire times are whole seconds; drop the sub-second part so "strictly after" is exact.
        let start: DateTime<Utc> = DateTime::from_timestamp(t.unix_timestamp(), 0)?;
        let next = self.0.find_next_occurrence(&start, false).ok()?;
        OffsetDateTime::from_unix_timestamp(next.timestamp()).ok()
    }
}

pub type EnqueueFn = Arc<dyn Fn(PgPool) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

struct Entry {
    name: &'static str,
    cron: Cron,
    f: EnqueueFn,
}

/// Source of "now"; production reads the real wall clock, tests inject their own.
pub type Clock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// Longest single sleep. The wall clock is re-read after each chunk, so a suspend or a clock
/// step delays a fire by at most this much.
const MAX_CHUNK: Duration = Duration::from_secs(60);

pub struct Scheduler {
    entries: Vec<Entry>,
    clock: Clock,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            clock: Arc::new(OffsetDateTime::now_utc),
        }
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the wall clock (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn add(mut self, name: &'static str, cron: Cron, f: EnqueueFn) -> Self {
        self.entries.push(Entry { name, cron, f });
        self
    }

    /// Runs every entry until `shutdown` is cancelled. An `Err` from an entry is logged with
    /// its name and the entry keeps its schedule.
    pub fn spawn(self, pool: PgPool, shutdown: CancellationToken) -> JoinHandle<()> {
        let clock = self.clock;
        tokio::spawn(async move {
            let loops = self
                .entries
                .into_iter()
                .map(|entry| run_entry(entry, pool.clone(), shutdown.clone(), clock.clone()));
            futures::future::join_all(loops).await;
        })
    }
}

async fn run_entry(entry: Entry, pool: PgPool, shutdown: CancellationToken, clock: Clock) {
    let mut last_fire: Option<OffsetDateTime> = None;
    loop {
        // Fires missed while asleep or suspended are not caught up: the next fire is always
        // the first one after the current time, and never the boundary that just fired, even
        // if the clock was stepped back across it.
        let from = last_fire.map_or_else(|| clock(), |f| f.max(clock()));
        let Some(next) = entry.cron.next_after(from) else {
            info!(job = entry.name, "schedule has no further fire times");
            return;
        };
        loop {
            let now = clock();
            if now >= next {
                break;
            }
            let wait = Duration::try_from(next - now).unwrap_or_default();
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(wait.min(MAX_CHUNK)) => {}
            }
        }
        last_fire = Some(next);
        if let Err(e) = (entry.f)(pool.clone()).await {
            warn!(job = entry.name, error = %e, "scheduled job failed to enqueue");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn next_fire_daily_0400_utc() {
        let cron = Cron::parse("0 4 * * *").unwrap();
        assert_eq!(
            cron.next_after(datetime!(2026-01-01 03:59:59 UTC)),
            Some(datetime!(2026-01-01 04:00:00 UTC))
        );
        assert_eq!(
            cron.next_after(datetime!(2026-01-01 04:00:00 UTC)),
            Some(datetime!(2026-01-02 04:00:00 UTC))
        );
        // A non-UTC offset is still read as the same instant.
        assert_eq!(
            cron.next_after(datetime!(2026-01-01 05:30:00 +2)),
            Some(datetime!(2026-01-01 04:00:00 UTC))
        );
    }

    #[test]
    fn invalid_expression_errors() {
        for bad in [
            "",
            "nope",
            "* * * *",
            "61 * * * *",
            "* * * * * *",
            "0 25 * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad:?} should not parse");
        }
    }
}
