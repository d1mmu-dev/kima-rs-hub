//! Default CLAP window offsets in seconds, independent of decoding and inference.
//!
//! Port of `compute_window_offsets` in [upstream analyzer.py](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer-clap/analyzer.py).
//! This is only the default five-window, ten-second configuration. It does not
//! perform sample indexing, repeat padding, feature extraction, or inference.

/// Select default CLAP window start times for a finite, nonnegative track duration.
/// Returns `None` for invalid duration. Zero follows upstream's five zero offsets;
/// callers must still handle empty audio when loading a window.
pub fn window_offsets(duration: f64) -> Option<Vec<f64>> {
    if !duration.is_finite() || duration < 0.0 {
        return None;
    }
    if duration == 0.0 {
        return Some(vec![0.0; 5]);
    }
    let n = ((duration / 10.0).floor() as usize).clamp(1, 5);
    if n == 1 {
        return Some(vec![0.0]);
    }
    if n < 5 {
        let step = (duration - 10.0) / (n - 1) as f64;
        return Some((0..n).map(|i| i as f64 * step).collect());
    }
    let mut starts: Vec<f64> = (0..5).map(|i| (i as f64 + 0.5) / 5.0 * duration).collect();
    starts[4] = starts[4].min((duration - 10.0).max(0.0));
    if starts[4] < starts[3] + 10.0 {
        let step = (duration - 10.0) / 4.0;
        starts = (0..5).map(|i| i as f64 * step).collect();
    }
    Some(starts)
}
