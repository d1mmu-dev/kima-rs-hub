//! ffmpeg-made audio for the media tests.

use std::path::Path;
use std::process::Command;

#[allow(unused_imports)]
pub use kima_test_support::require_ffmpeg;

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
