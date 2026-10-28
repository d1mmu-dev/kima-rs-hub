//! `autotag:progress` events (spec §17): the search, apply and undo jobs announce their
//! progress on the M0 bus to every user, at most four times a second. A progress call inside
//! [`MIN_GAP`] of the last event is dropped (the next one carries newer counts); the final
//! event of a job is always sent.
//!
//! One [`AutotagEvents`] belongs to one running job, so the window is plain state behind
//! `&mut self`: no lock, no task.

use std::time::Duration;

use kima_core::events::{Event, EventBus};
use serde_json::{Map, Value, json};
use tokio::time::Instant;
use uuid::Uuid;

/// The event's `type`.
pub const TYPE: &str = "autotag:progress";

/// The shortest gap between two progress events (four a second).
pub const MIN_GAP: Duration = Duration::from_millis(250);

/// Which job is reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutotagJob {
    Search,
    Apply,
    Undo,
}

impl AutotagJob {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Apply => "apply",
            Self::Undo => "undo",
        }
    }
}

/// One job's throttled publisher.
pub struct AutotagEvents {
    bus: EventBus,
    last: Option<Instant>,
}

impl AutotagEvents {
    pub fn new(bus: EventBus) -> Self {
        Self { bus, last: None }
    }

    /// Publishes the job's progress unless the last event is younger than [`MIN_GAP`].
    pub fn progress(
        &mut self,
        job: AutotagJob,
        album: Option<Uuid>,
        run: Option<Uuid>,
        done: usize,
        total: usize,
        state: Option<&str>,
    ) {
        let now = Instant::now();
        if self.last.is_some_and(|last| now < last + MIN_GAP) {
            return;
        }
        self.last = Some(now);
        self.publish(job, album, run, done, total, state);
    }

    /// Publishes the job's final event, whatever the window says.
    pub fn finish(
        &mut self,
        job: AutotagJob,
        album: Option<Uuid>,
        run: Option<Uuid>,
        done: usize,
        total: usize,
        state: Option<&str>,
    ) {
        self.last = Some(Instant::now());
        self.publish(job, album, run, done, total, state);
    }

    /// `{type, job, albumId?, runId?, done, total, state?}` to every user: ids, counts and a
    /// state name only, never an error text, a path or a secret.
    fn publish(
        &self,
        job: AutotagJob,
        album: Option<Uuid>,
        run: Option<Uuid>,
        done: usize,
        total: usize,
        state: Option<&str>,
    ) {
        let mut data = Map::new();
        data.insert("type".into(), json!(TYPE));
        data.insert("job".into(), json!(job.as_str()));
        if let Some(album) = album {
            data.insert("albumId".into(), json!(album));
        }
        if let Some(run) = run {
            data.insert("runId".into(), json!(run));
        }
        data.insert("done".into(), json!(done));
        data.insert("total".into(), json!(total));
        if let Some(state) = state {
            data.insert("state".into(), json!(state));
        }
        self.bus.publish(Event {
            user_id: None,
            data: Value::Object(data),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    #[tokio::test(start_paused = true)]
    async fn events_throttled_and_final_sent() {
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let album = Uuid::now_v7();
        let mut ev = AutotagEvents::new(bus);

        ev.progress(
            AutotagJob::Search,
            Some(album),
            None,
            0,
            3,
            Some("searching"),
        );
        ev.progress(
            AutotagJob::Search,
            Some(album),
            None,
            1,
            3,
            Some("searching"),
        );
        tokio::time::advance(Duration::from_millis(249)).await;
        ev.progress(
            AutotagJob::Search,
            Some(album),
            None,
            2,
            3,
            Some("searching"),
        );
        let sent = drain(&mut rx);
        assert_eq!(sent.len(), 1, "calls inside 250 ms are dropped");
        assert_eq!(
            sent[0].data,
            json!({"type": TYPE, "job": "search", "albumId": album, "done": 0, "total": 3,
                   "state": "searching"})
        );
        assert_eq!(sent[0].user_id, None);

        tokio::time::advance(Duration::from_millis(1)).await;
        ev.progress(AutotagJob::Search, Some(album), None, 2, 3, None);
        let sent = drain(&mut rx);
        assert_eq!(
            sent.len(),
            1,
            "250 ms after the last event one is sent again"
        );
        assert_eq!(
            sent[0].data,
            json!({"type": TYPE, "job": "search", "albumId": album, "done": 2, "total": 3})
        );

        // The final event is never dropped, even right after another.
        let run = Uuid::now_v7();
        ev.finish(AutotagJob::Apply, None, Some(run), 3, 3, Some("proposed"));
        let sent = drain(&mut rx);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].data,
            json!({"type": TYPE, "job": "apply", "runId": run, "done": 3, "total": 3,
                   "state": "proposed"})
        );
        // And it opens a new window.
        ev.progress(AutotagJob::Undo, None, Some(run), 0, 1, None);
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn job_names() {
        assert_eq!(AutotagJob::Search.as_str(), "search");
        assert_eq!(AutotagJob::Apply.as_str(), "apply");
        assert_eq!(AutotagJob::Undo.as_str(), "undo");
    }
}
