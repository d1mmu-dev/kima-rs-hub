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
use tokio::time::Instant;
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

#[derive(Default)]
pub struct Scheduler {
    entries: Vec<Entry>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(mut self, name: &'static str, cron: Cron, f: EnqueueFn) -> Self {
        self.entries.push(Entry { name, cron, f });
        self
    }

    /// Runs every entry until `shutdown` is cancelled. An `Err` from an entry is logged with
    /// its name and the entry keeps its schedule.
    pub fn spawn(self, pool: PgPool, shutdown: CancellationToken) -> JoinHandle<()> {
        // Wall time at start plus tokio's (pausable) monotonic clock, so sleeps and fire
        // times agree.
        let wall_start = OffsetDateTime::now_utc();
        let mono_start = Instant::now();
        tokio::spawn(async move {
            let loops = self.entries.into_iter().map(|entry| {
                run_entry(
                    entry,
                    pool.clone(),
                    shutdown.clone(),
                    wall_start,
                    mono_start,
                )
            });
            futures::future::join_all(loops).await;
        })
    }
}

async fn run_entry(
    entry: Entry,
    pool: PgPool,
    shutdown: CancellationToken,
    wall_start: OffsetDateTime,
    mono_start: Instant,
) {
    let now = || wall_start + mono_start.elapsed();
    let mut last_fire: Option<OffsetDateTime> = None;
    loop {
        // Never pick the fire time we just handled again.
        let from = last_fire.map_or_else(now, |f| f.max(now()));
        let Some(next) = entry.cron.next_after(from) else {
            info!(job = entry.name, "schedule has no further fire times");
            return;
        };
        let wait = (next - now()).max(time::Duration::ZERO);
        let wait = Duration::try_from(wait).unwrap_or_default();
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
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
