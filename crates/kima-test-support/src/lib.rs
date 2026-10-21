//! Helpers shared by the workspace's tests. Only ever a dev-dependency.

use std::process::{Command, Stdio};

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The skip rule for ffmpeg-gated tests: `available` outside CI, and a panic in CI when ffmpeg is
/// missing, so a test cannot pass there by doing nothing.
pub fn ffmpeg_guard(available: bool, ci: bool) -> bool {
    assert!(available || !ci, "ffmpeg is required in CI");
    if !available {
        eprintln!("ffmpeg not found; skipping test");
    }
    available
}

/// Returns false (after printing a note) when ffmpeg is missing; tests should return early.
/// Under `CI` a missing ffmpeg panics instead.
pub fn require_ffmpeg() -> bool {
    ffmpeg_guard(ffmpeg_available(), std::env::var_os("CI").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "ffmpeg is required in CI")]
    fn ffmpeg_guard_panics_in_ci_without_ffmpeg() {
        ffmpeg_guard(false, true);
    }

    #[test]
    fn ffmpeg_guard_skips_outside_ci() {
        assert!(!ffmpeg_guard(false, false));
        assert!(ffmpeg_guard(true, false));
        assert!(ffmpeg_guard(true, true));
    }
}
