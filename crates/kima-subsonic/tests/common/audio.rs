//! ffmpeg-made audio for the media tests.

use std::path::Path;
use std::process::{Command, Stdio};

/// True when ffmpeg runs. Without it a test is skipped locally, but under `CI` a missing ffmpeg
/// is a failure, so the test cannot pass by doing nothing.
pub fn require_ffmpeg() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        assert!(std::env::var_os("CI").is_none(), "ffmpeg is required in CI");
        eprintln!("ffmpeg not found; skipping test");
    }
    ok
}

/// Writes a 3 second sine tone at `dest`; the codec follows the extension (`flac` or `mp3`).
pub fn sine(dest: &Path) {
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let codec = match dest.extension().and_then(|e| e.to_str()) {
        Some("flac") => "flac",
        Some("mp3") => "libmp3lame",
        other => panic!("unsupported test audio extension {other:?}"),
    };
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=duration=3"])
        .args(["-c:a", codec])
        .arg(dest)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed for {}", dest.display());
}
