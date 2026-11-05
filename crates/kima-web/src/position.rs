//! The in-track resume position, kept client-side.
//!
//! `POST /api/playback-state` stores the queue, the index, the track id and shuffle, but the web save
//! never writes `position` (only the Subsonic queue does, see `kima-api::routes::playback_state`), so
//! `GET /api/playback-state` always reports `position: 0` for a web session. Rather than change the
//! backend, the player keeps `{ id, t }` under its own `localStorage` key and applies it on restore
//! only when `id` is the track the restored queue points at.

use serde::{Deserialize, Serialize};

pub const STORAGE_KEY: &str = "kima-web.position";

/// A saved point is ignored this close to the end (the track would end at once) and below one second.
pub const EDGE_SECS: f64 = 1.0;
/// The position is written again only after it moved by this much.
pub const WRITE_EVERY_SECS: f64 = 1.0;

#[derive(Serialize, Deserialize)]
struct Saved {
    id: String,
    t: f64,
}

pub fn encode(track_id: &str, secs: f64) -> String {
    serde_json::to_string(&Saved {
        id: track_id.to_owned(),
        t: secs,
    })
    .unwrap_or_default()
}

/// Where to resume `track_id` (whose length is `duration` seconds, 0 when unknown), if `raw` (the stored
/// value) is a point of that very track and worth resuming.
pub fn restore_point(raw: Option<&str>, track_id: &str, duration: f64) -> Option<f64> {
    let saved: Saved = serde_json::from_str(raw?).ok()?;
    let usable = saved.id == track_id
        && saved.t.is_finite()
        && saved.t >= EDGE_SECS
        && !(duration > 0.0 && saved.t >= duration - EDGE_SECS);
    usable.then_some(saved.t)
}

/// Whether `now` is worth writing: past the first second (a fresh element can still report 0 before
/// its start position is applied, which must not overwrite the stored point) and far enough from the
/// last written position.
pub fn should_write(last_written: f64, now: f64) -> bool {
    now >= EDGE_SECS && (now - last_written).abs() >= WRITE_EVERY_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_of_the_same_track_comes_back() {
        let raw = encode("abc", 12.5);
        assert_eq!(restore_point(Some(&raw), "abc", 20.0), Some(12.5));
    }

    #[test]
    fn a_point_of_another_track_is_ignored() {
        let raw = encode("abc", 12.5);
        assert_eq!(restore_point(Some(&raw), "other", 20.0), None);
    }

    #[test]
    fn the_very_start_and_the_very_end_are_not_worth_resuming() {
        assert_eq!(restore_point(Some(&encode("a", 0.4)), "a", 20.0), None);
        assert_eq!(restore_point(Some(&encode("a", 19.5)), "a", 20.0), None);
        // Unknown duration: only the lower bound applies.
        assert_eq!(
            restore_point(Some(&encode("a", 19.5)), "a", 0.0),
            Some(19.5)
        );
    }

    #[test]
    fn junk_is_ignored() {
        assert_eq!(restore_point(None, "a", 20.0), None);
        assert_eq!(restore_point(Some("not json"), "a", 20.0), None);
        assert_eq!(restore_point(Some(r#"{"id":"a"}"#), "a", 20.0), None);
    }

    #[test]
    fn writes_are_throttled() {
        assert!(!should_write(10.0, 10.4));
        assert!(should_write(10.0, 11.0));
        assert!(should_write(10.0, 3.0));
        assert!(should_write(-1000.0, 5.0));
        assert!(!should_write(-1000.0, 0.0));
        assert!(!should_write(10.0, 0.2));
    }
}
