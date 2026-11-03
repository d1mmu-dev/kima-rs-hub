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

/// Collects caller-selected source-frame ranges without retaining the whole track.
///
/// Ranges are half-open, in mono source frames, and are supplied in output order.
/// This component does not derive upstream offsets, resample, pad, decode, or
/// establish that two passes read the same source. The caller must feed every
/// frame through clean EOF and enforce those separate contracts.
pub struct WindowCollector {
    expected: usize,
    seen: usize,
    failed: bool,
    windows: Vec<(std::ops::Range<usize>, Vec<f32>)>,
}

/// Invalid collector configuration, allocation failure, or a frame-count mismatch.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WindowCollectorError {
    #[error("invalid window ranges or sample budget")]
    InvalidRanges,
    #[error("could not allocate window buffers")]
    Allocation,
    #[error("decoded frame count does not match expected count")]
    FrameCount,
}

impl WindowCollector {
    /// Limit requested window samples to `sample_budget` across at most five windows.
    /// Allocator overhead and input/decoder buffers are outside this bound.
    pub fn new(
        expected: usize,
        ranges: &[std::ops::Range<usize>],
        sample_budget: usize,
    ) -> Result<Self, WindowCollectorError> {
        if ranges.len() > 5 {
            return Err(WindowCollectorError::InvalidRanges);
        }
        let mut total = 0usize;
        for range in ranges {
            if range.start > range.end || range.end > expected {
                return Err(WindowCollectorError::InvalidRanges);
            }
            total = total
                .checked_add(range.end - range.start)
                .ok_or(WindowCollectorError::InvalidRanges)?;
        }
        if total > sample_budget {
            return Err(WindowCollectorError::InvalidRanges);
        }
        let mut windows = Vec::new();
        windows
            .try_reserve_exact(ranges.len())
            .map_err(|_| WindowCollectorError::Allocation)?;
        for range in ranges {
            let mut samples = Vec::new();
            samples
                .try_reserve_exact(range.end - range.start)
                .map_err(|_| WindowCollectorError::Allocation)?;
            windows.push((range.clone(), samples));
        }
        Ok(Self {
            expected,
            seen: 0,
            failed: false,
            windows,
        })
    }

    /// Feed consecutive source frames, including those outside selected ranges.
    pub fn push(&mut self, samples: &[f32]) -> Result<(), WindowCollectorError> {
        let end = self
            .seen
            .checked_add(samples.len())
            .filter(|&end| end <= self.expected);
        let Some(end) = end.filter(|_| !self.failed) else {
            self.failed = true;
            return Err(WindowCollectorError::FrameCount);
        };
        for (range, window) in &mut self.windows {
            let start = range.start.max(self.seen);
            let stop = range.end.min(end);
            if start < stop {
                window.extend_from_slice(&samples[start - self.seen..stop - self.seen]);
            }
        }
        self.seen = end;
        Ok(())
    }

    /// Return windows only after all expected frames have been supplied.
    pub fn finish(self) -> Result<Vec<Vec<f32>>, WindowCollectorError> {
        if self.failed || self.seen != self.expected {
            return Err(WindowCollectorError::FrameCount);
        }
        Ok(self
            .windows
            .into_iter()
            .map(|(_, samples)| samples)
            .collect())
    }
}
