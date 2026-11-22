//! A station keeps playing by fetching another batch whenever its queue runs out (upstream
//! `lib/station-refill.ts`). The session remembers every track id it has queued, so a refill adds only tracks not
//! already played or queued, and it ends when a fetch brings nothing new. A station the server orders
//! (`server_ordered`: its batches carry `next`) never repeats a song until it has played them all and then starts
//! over on purpose, so its refills are taken whole and it ends only when a fetch brings nothing.
//!
//! The session is the synchronous half of upstream's `refillStation`: [`StationSession::begin`] decides whether a
//! fetch starts, [`StationSession::finish`] / [`StationSession::fail`] take its outcome. The player
//! (`player.rs`) runs the fetch and appends; this module has no browser dependency and is tested natively.

use std::collections::HashSet;

use kima_api_types::playback_state::QueueItem;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationSession {
    server_ordered: bool,
    seen: HashSet<String>,
    exhausted: bool,
    loading: bool,
    /// The queue position a refill was last tried from, so one track start triggers one fetch. A refill that adds
    /// tracks clears it: the queue only ends there again if the listener removes them, and then it needs more.
    triggered_for: Option<String>,
}

impl StationSession {
    /// Upstream `startStationSession`: the first batch's ids count as seen.
    pub fn start<'a>(queued: impl IntoIterator<Item = &'a str>, server_ordered: bool) -> Self {
        Self {
            server_ordered,
            seen: queued.into_iter().map(str::to_owned).collect(),
            exhausted: false,
            loading: false,
            triggered_for: None,
        }
    }

    /// Whether a refill from `trigger` (the queue position: `"<index>:<track id>"`) should fetch now: not when the
    /// session is done, busy, or already tried from this position. Every id in `queued_now` counts as seen, so
    /// tracks the listener added to the queue are not added again.
    pub fn begin<'a>(
        &mut self,
        trigger: &str,
        queued_now: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        if self.exhausted || self.loading || self.triggered_for.as_deref() == Some(trigger) {
            return false;
        }
        self.triggered_for = Some(trigger.to_owned());
        self.loading = true;
        self.seen.extend(queued_now.into_iter().map(str::to_owned));
        true
    }

    /// The fetch answered: the tracks to append (none ends the session for good).
    pub fn finish(&mut self, batch: Vec<QueueItem>) -> Vec<QueueItem> {
        self.loading = false;
        let mut fresh = Vec::new();
        for track in batch {
            let new = self.seen.insert(track.id.clone());
            if new || self.server_ordered {
                fresh.push(track);
            }
        }
        if fresh.is_empty() {
            self.exhausted = true;
        } else {
            self.triggered_for = None;
        }
        fresh
    }

    /// The fetch failed: the same position may try again (on the next player change or a press of play).
    pub fn fail(&mut self) {
        self.loading = false;
        self.triggered_for = None;
    }

    #[cfg(test)]
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }

    #[cfg(test)]
    pub fn loading(&self) -> bool {
        self.loading
    }
}

/// The refill trigger of a queue position (upstream `` `${currentIndex}:${currentTrack.id}` ``).
pub fn trigger_key(index: usize, track_id: &str) -> String {
    format!("{index}:{track_id}")
}

/// Whether the playing entry is the last one in play order (upstream `isLastInPlayOrder`). kima-web's shuffle picks
/// a random next entry instead of keeping a shuffled order, so the queue's own order is the play order (D-282).
pub fn is_last_in_play_order(len: usize, index: usize) -> bool {
    len > 0 && index + 1 >= len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ids: &[&str]) -> Vec<QueueItem> {
        ids.iter()
            .map(|id| QueueItem {
                id: (*id).to_owned(),
                title: (*id).to_owned(),
                duration: 60.0,
                artist: None,
                album: None,
            })
            .collect()
    }

    fn ids(items: &[QueueItem]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    /// Upstream `refillStation` driven through the session with `batch` as the fetch's answer (`Err` = failure).
    fn refill(
        s: &mut StationSession,
        trigger: &str,
        queued_now: &[&str],
        batch: Result<Vec<QueueItem>, ()>,
        fetches: &mut u32,
    ) -> Result<Vec<QueueItem>, ()> {
        if !s.begin(trigger, queued_now.iter().copied()) {
            return Ok(Vec::new());
        }
        *fetches += 1;
        match batch {
            Ok(b) => Ok(s.finish(b)),
            Err(()) => {
                s.fail();
                Err(())
            }
        }
    }

    #[test]
    fn returns_only_tracks_not_already_queued_or_played_and_remembers_them() {
        let mut s = StationSession::start(["a", "b"], false);
        let mut n = 0;
        let got = refill(&mut s, "1", &[], Ok(t(&["b", "c", "c", "d"])), &mut n).unwrap();
        assert_eq!(ids(&got), ["c", "d"]);
        assert!(!s.exhausted());
    }

    #[test]
    fn stops_for_good_when_a_fetch_brings_nothing_new() {
        let mut s = StationSession::start(["a"], false);
        let mut n = 0;
        assert!(
            refill(&mut s, "1", &[], Ok(t(&["a"])), &mut n)
                .unwrap()
                .is_empty()
        );
        assert!(s.exhausted());
        assert!(
            refill(&mut s, "2", &[], Ok(t(&["z"])), &mut n)
                .unwrap()
                .is_empty()
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn never_fetches_twice_at_once() {
        let mut s = StationSession::start(["a"], false);
        assert!(s.begin("1", []));
        assert!(s.loading());
        // An overlapping call while the first fetch is out.
        assert!(!s.begin("1", []));
        assert!(!s.begin("2", []));
        assert_eq!(ids(&s.finish(t(&["b"]))), ["b"]);
        assert!(!s.loading());
    }

    #[test]
    fn one_position_fetches_once_until_its_refill_added_tracks() {
        let mut s = StationSession::start(["a"], false);
        let mut n = 0;
        assert_eq!(
            ids(&refill(&mut s, "0:a", &["a"], Ok(t(&["b"])), &mut n).unwrap()),
            ["b"]
        );
        // The listener removed b and is back at the end: the same position asks again.
        assert_eq!(
            ids(&refill(&mut s, "0:a", &["a"], Ok(t(&["c"])), &mut n).unwrap()),
            ["c"]
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn a_failed_fetch_does_not_end_the_session_and_the_position_retries() {
        let mut s = StationSession::start(["a"], false);
        let mut n = 0;
        assert!(refill(&mut s, "1", &[], Err(()), &mut n).is_err());
        assert!(!s.exhausted());
        assert!(!s.loading());
        assert_eq!(
            ids(&refill(&mut s, "1", &[], Ok(t(&["b"])), &mut n).unwrap()),
            ["b"]
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn skips_tracks_the_listener_added_to_the_queue() {
        let mut s = StationSession::start(["a"], false);
        let mut n = 0;
        let got = refill(&mut s, "1", &["a", "x"], Ok(t(&["x", "b"])), &mut n).unwrap();
        assert_eq!(ids(&got), ["b"]);
    }

    #[test]
    fn a_tried_position_does_not_fetch_again_when_its_refill_was_kept() {
        let mut s = StationSession::start(["a"], false);
        assert!(s.begin("0:a", []));
        s.finish(t(&["b"]));
        // Same trigger after a successful refill is allowed again (upstream clears `triggeredFor` on new tracks) ...
        assert!(s.begin("0:a", []));
        s.finish(t(&["c"]));
        // ... but an empty answer ends the station.
        assert!(s.begin("1:b", []));
        assert!(s.finish(t(&["a", "b", "c"])).is_empty());
        assert!(!s.begin("2:c", []));
    }

    #[test]
    fn a_server_ordered_station_takes_each_refill_whole() {
        let mut s = StationSession::start(["a", "b"], true);
        let mut n = 0;
        assert_eq!(
            ids(&refill(&mut s, "1", &[], Ok(t(&["a", "b"])), &mut n).unwrap()),
            ["a", "b"]
        );
        assert_eq!(
            ids(&refill(&mut s, "2", &[], Ok(t(&["a", "c"])), &mut n).unwrap()),
            ["a", "c"]
        );
        assert!(!s.exhausted());
        assert!(
            refill(&mut s, "3", &[], Ok(Vec::new()), &mut n)
                .unwrap()
                .is_empty()
        );
        assert!(s.exhausted());
    }

    #[test]
    fn last_in_play_order_is_the_queue_end() {
        assert!(!is_last_in_play_order(0, 0));
        assert!(is_last_in_play_order(1, 0));
        assert!(!is_last_in_play_order(3, 1));
        assert!(is_last_in_play_order(3, 2));
        // A stale index past the end counts as the end.
        assert!(is_last_in_play_order(3, 7));
        assert_eq!(trigger_key(4, "x"), "4:x");
    }
}
