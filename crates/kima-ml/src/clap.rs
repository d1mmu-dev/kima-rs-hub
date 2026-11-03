//! Default CLAP window selection, preparation, and model-independent log-mel.
//!
//! Window selection ports `compute_window_offsets` in [upstream analyzer.py](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer-clap/analyzer.py).
//! Only the default five-window, ten-second configuration is implemented.
//! Model inference and source verification are outside this module.

/// Prepare one native-rate mono window for the default ONNX CLAP frontend.
///
/// Resamples independently with libsoxr HQ, fixes the length using librosa's
/// ceiling rule, cuts to ten seconds, then tiles whole copies and zero-pads
/// the remainder, as in upstream `onnx_backend.py` and `clap_mel.py` at
/// commit 12faf1cd34ff53054221818b0ccde58b8e9b0020. Empty input is an error.
/// Only 8–192 kHz and up to ten seconds of input are accepted.
pub fn prepare_window(samples: &[f32], rate: u32) -> Result<Vec<f32>, crate::audio::AudioError> {
    use crate::audio::{AudioError, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE};
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&rate) {
        return Err(AudioError::UnsupportedSampleRate(rate));
    }
    if samples.len() > rate as usize * 10 {
        return Err(AudioError::BufferOverflow);
    }
    crate::audio::validate_samples(samples)?;
    if samples.is_empty() {
        return Err(crate::audio::AudioError::EmptyAudio);
    }
    let mut output = if rate == 48_000 {
        samples.to_vec()
    } else {
        soxr_resample(samples, rate)?
    };
    output.truncate(480_000);
    let length = output.len();
    let repeated = (480_000 / length) * length;
    output.reserve(480_000 - length);
    while output.len() < repeated {
        let count = (repeated - output.len()).min(length);
        output.extend_from_within(..count);
    }
    output.resize(480_000, 0.0);
    Ok(output)
}

// C ABI is defined by libsoxr's public soxr.h. The library remains dynamically linked.
#[repr(C)]
struct SoxrQuality {
    precision: f64,
    phase_response: f64,
    passband_end: f64,
    stopband_begin: f64,
    reserved: *mut std::ffi::c_void,
    flags: std::ffi::c_ulong,
}

#[link(name = "soxr")]
unsafe extern "C" {
    fn soxr_quality_spec(recipe: std::ffi::c_ulong, flags: std::ffi::c_ulong) -> SoxrQuality;
    fn soxr_oneshot(
        input_rate: f64,
        output_rate: f64,
        channels: std::ffi::c_uint,
        input: *const std::ffi::c_void,
        input_len: usize,
        input_done: *mut usize,
        output: *mut std::ffi::c_void,
        output_len: usize,
        output_done: *mut usize,
        io_spec: *const std::ffi::c_void,
        quality: *const SoxrQuality,
        runtime: *const std::ffi::c_void,
    ) -> *const std::ffi::c_char;
}

fn soxr_resample(samples: &[f32], rate: u32) -> Result<Vec<f32>, crate::audio::AudioError> {
    use crate::audio::AudioError;
    let length = ((48_000_f64 / f64::from(rate)) * samples.len() as f64).ceil() as usize;
    let mut output = vec![0.0_f32; length];
    let (mut consumed, mut produced) = (0, 0);
    // SAFETY: bounded slices outlive the call, sizes are in frames, null requests defaults.
    let error = unsafe {
        let quality = soxr_quality_spec(4, 0); // SOXR_HQ, not oneshot's LQ default.
        soxr_oneshot(
            f64::from(rate),
            48_000.0,
            1,
            samples.as_ptr().cast(),
            samples.len(),
            &mut consumed,
            output.as_mut_ptr().cast(),
            output.len(),
            &mut produced,
            std::ptr::null(),
            &quality,
            std::ptr::null(),
        )
    };
    if !error.is_null() {
        // SAFETY: libsoxr returns a static NUL-terminated error string.
        let message = unsafe { std::ffi::CStr::from_ptr(error) }.to_string_lossy();
        return Err(AudioError::Resample(message.into_owned()));
    }
    if consumed != samples.len() || produced > length || produced.saturating_add(1) < length {
        return Err(AudioError::Resample("incomplete soxr conversion".into()));
    }
    // soxr rounds its output count; librosa pads the final ceil sample with zero.
    crate::audio::validate_samples(&output)?;
    Ok(output)
}

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

/// Derive half-open source-frame ranges from a complete decoded frame count.
///
/// Uses the upstream binary64 offset, start clamp, and separate truncation of
/// offset and take at the native sample rate, capped at the observed frame count.
/// This does not establish librosa waveform or compressed-container parity.
/// Empty input is refused.
pub fn source_window_ranges(
    expected: usize,
    rate: u32,
) -> Result<Vec<std::ops::Range<usize>>, WindowCollectorError> {
    if expected == 0 || rate == 0 || rate > crate::audio::MAX_SAMPLE_RATE {
        return Err(WindowCollectorError::InvalidRanges);
    }
    let duration = expected as f64 / f64::from(rate);
    let offsets = window_offsets(duration).ok_or(WindowCollectorError::InvalidRanges)?;
    let mut ranges = Vec::with_capacity(offsets.len());
    for offset in offsets {
        let start_seconds = offset.min((duration - 0.001).max(0.0)).max(0.0);
        let take = 10.0_f64.min((duration - start_seconds).max(0.0));
        let start = ((start_seconds * f64::from(rate)) as usize).min(expected);
        // A zero take is passed to librosa as None (read to EOF).
        let length = if take > 0.0 {
            (take * f64::from(rate)) as usize
        } else {
            expected - start
        };
        let end = start.saturating_add(length).min(expected);
        ranges.push(start..end);
    }
    Ok(ranges)
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
    /// Collect default CLAP ranges, retaining at most 50 seconds at 192 kHz.
    /// Caller must establish source identity and clean EOF independently.
    pub fn for_clap(expected: usize, rate: u32) -> Result<Self, WindowCollectorError> {
        let ranges = source_window_ranges(expected, rate)?;
        Self::new(
            expected,
            &ranges,
            5 * 10 * crate::audio::MAX_SAMPLE_RATE as usize,
        )
    }

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

/// Default model-independent CLAP log-mel frontend for a mono 48 kHz window.
/// Output is 1001 frames of 64 dB values in frame-major order.
pub struct ClapMelFrontend {
    fft: std::sync::Arc<dyn rustfft::Fft<f64>>,
    filters: [[f64; 513]; 64],
    window: [f64; 1024],
}

impl Default for ClapMelFrontend {
    fn default() -> Self {
        Self::new()
    }
}

impl ClapMelFrontend {
    pub fn new() -> Self {
        let fft = rustfft::FftPlanner::new().plan_fft_forward(1024);
        let step = 6.4_f64.ln() / 27.0;
        let low = 0.75;
        let high = 15.0 + 14.0_f64.ln() / step;
        let hz: [f64; 66] = std::array::from_fn(|i| {
            let mel = low + (high - low) * i as f64 / 65.0;
            if mel < 15.0 {
                200.0 * mel / 3.0
            } else {
                1000.0 * (step * (mel - 15.0)).exp()
            }
        });
        let filters = std::array::from_fn(|b| {
            std::array::from_fn(|k| {
                let f = k as f64 * 46.875;
                ((f - hz[b]) / (hz[b + 1] - hz[b]))
                    .min((hz[b + 2] - f) / (hz[b + 2] - hz[b + 1]))
                    .max(0.0)
                    * 2.0
                    / (hz[b + 2] - hz[b])
            })
        });
        let window = std::array::from_fn(|i| {
            0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / 1024.0).cos()
        });
        Self {
            fft,
            filters,
            window,
        }
    }

    /// Repeat whole copies and zero-fill the remainder, matching upstream.
    /// Empty, nonfinite and over-ten-second inputs fail closed.
    pub fn compute(&self, samples: &[f32]) -> Result<Vec<[f32; 64]>, &'static str> {
        const LEN: usize = 480_000;
        if samples.is_empty() {
            return Err("empty input");
        }
        if samples.len() > LEN {
            return Err("input exceeds ten seconds");
        }
        if samples.iter().any(|x| !x.is_finite()) {
            return Err("non-finite input");
        }
        let repeated = LEN / samples.len() * samples.len();
        let sample = |index: usize| {
            if index < repeated {
                samples[index % samples.len()] as f64
            } else {
                0.0
            }
        };
        let mut out = Vec::with_capacity(1001);
        let mut fft = vec![rustfft::num_complex::Complex64::default(); 1024];
        for frame in 0..1001 {
            for (i, v) in fft.iter_mut().enumerate() {
                let pos = frame * 480 + i;
                let index = if pos < 512 {
                    512 - pos
                } else if pos >= LEN + 512 {
                    2 * LEN + 510 - pos
                } else {
                    pos - 512
                };
                *v = rustfft::num_complex::Complex64::new(sample(index) * self.window[i], 0.0);
            }
            self.fft.process(&mut fft);
            let power: [f64; 513] = std::array::from_fn(|i| {
                let re = fft[i].re as f32 as f64;
                let im = fft[i].im as f32 as f64;
                re * re + im * im
            });
            if power.iter().any(|p| !p.is_finite()) {
                return Err("non-finite frontend output");
            }
            let bands = std::array::from_fn(|b| {
                let sum: f64 = power.iter().zip(self.filters[b]).map(|(p, w)| p * w).sum();
                (10.0 * sum.max(1e-10).log10()) as f32
            });
            if bands.iter().any(|v: &f32| !v.is_finite()) {
                return Err("non-finite frontend output");
            }
            out.push(bands);
        }
        Ok(out)
    }
}

/// Fixed dimensionality of CLAP audio and text embeddings (512 dimensions).
pub const CLAP_EMBEDDING_DIM: usize = 512;

/// Maximum number of windows aggregated for a single track (5 windows).
pub const MAX_CLAP_WINDOWS: usize = 5;

/// Validated 512-dimensional CLAP embedding vector with unit L2 norm.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClapEmbedding([f32; CLAP_EMBEDDING_DIM]);

impl ClapEmbedding {
    /// Construct a ClapEmbedding from an already-normalized 512-vector.
    ///
    /// Validates that all elements are finite and that the L2 norm satisfies
    /// upstream's unit-norm guard: `|norm - 1.0| <= 1e-3` (from `analyzer.py` line 853).
    pub fn from_normalized(
        vector: [f32; CLAP_EMBEDDING_DIM],
    ) -> Result<Self, ClapAggregationError> {
        let norm = l2_norm(&vector)?;
        if (norm - 1.0).abs() > 1e-3 {
            return Err(ClapAggregationError::UnitNormGuardFailed { norm });
        }
        Ok(Self(vector))
    }

    /// Return the embedding as a fixed-size array reference of 512 floats.
    pub fn as_array(&self) -> &[f32; CLAP_EMBEDDING_DIM] {
        &self.0
    }

    /// Return the embedding as a slice of 512 floats.
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// Unwrap the inner 512-element array.
    pub fn into_inner(self) -> [f32; CLAP_EMBEDDING_DIM] {
        self.0
    }
}

impl AsRef<[f32]> for ClapEmbedding {
    fn as_ref(&self) -> &[f32] {
        &self.0
    }
}

impl std::ops::Deref for ClapEmbedding {
    type Target = [f32; CLAP_EMBEDDING_DIM];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Errors during CLAP embedding normalization, aggregation, or validation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClapAggregationError {
    #[error("window collection is empty; at least one window is required")]
    EmptyWindows,
    #[error("too many windows: {count} exceeds maximum of {max}")]
    TooManyWindows { count: usize, max: usize },
    #[error("window embedding contains non-finite values (NaN or infinity)")]
    NonFiniteInput,
    #[error(
        "embedding vector has zero or near-zero norm (norm = {norm:.6e}); cannot be normalized"
    )]
    ZeroNorm { norm: f64 },
    #[error("unit-norm guard failed: norm {norm:.6} deviates from 1.0 by more than 1e-3")]
    UnitNormGuardFailed { norm: f64 },
}

/// Compute the L2 norm of a 512-dimensional vector using double-precision accumulation.
/// Returns error if any element is non-finite.
pub fn l2_norm(vector: &[f32; CLAP_EMBEDDING_DIM]) -> Result<f64, ClapAggregationError> {
    let mut sum_sq = 0.0_f64;
    for &x in vector {
        if !x.is_finite() {
            return Err(ClapAggregationError::NonFiniteInput);
        }
        let v = f64::from(x);
        sum_sq += v * v;
    }
    Ok(sum_sq.sqrt())
}

/// L2-normalize a 512-dimensional vector.
///
/// Refuses non-finite input (`NonFiniteInput`) and vectors whose norm is zero
/// or below 1e-12 (`ZeroNorm`).
pub fn normalize_embedding(
    vector: &[f32; CLAP_EMBEDDING_DIM],
) -> Result<[f32; CLAP_EMBEDDING_DIM], ClapAggregationError> {
    let norm = l2_norm(vector)?;
    if norm <= 1e-12 {
        return Err(ClapAggregationError::ZeroNorm { norm });
    }
    let mut out = [0.0_f32; CLAP_EMBEDDING_DIM];
    for (i, &x) in vector.iter().enumerate() {
        out[i] = (f64::from(x) / norm) as f32;
    }
    Ok(out)
}

/// Compute the arithmetic mean of 1 to 5 window embeddings across each of the 512 dimensions.
///
/// Uses double precision accumulation to prevent precision loss.
/// Refuses empty slices (`EmptyWindows`), window counts exceeding 5 (`TooManyWindows`),
/// and non-finite values (`NonFiniteInput`).
pub fn mean_embeddings(
    windows: &[[f32; CLAP_EMBEDDING_DIM]],
) -> Result<[f32; CLAP_EMBEDDING_DIM], ClapAggregationError> {
    if windows.is_empty() {
        return Err(ClapAggregationError::EmptyWindows);
    }
    if windows.len() > MAX_CLAP_WINDOWS {
        return Err(ClapAggregationError::TooManyWindows {
            count: windows.len(),
            max: MAX_CLAP_WINDOWS,
        });
    }
    let mut sums = [0.0_f64; CLAP_EMBEDDING_DIM];
    for window in windows {
        for (i, &x) in window.iter().enumerate() {
            if !x.is_finite() {
                return Err(ClapAggregationError::NonFiniteInput);
            }
            sums[i] += f64::from(x);
        }
    }
    let n = windows.len() as f64;
    let mut out = [0.0_f32; CLAP_EMBEDDING_DIM];
    for (i, &sum) in sums.iter().enumerate() {
        out[i] = (sum / n) as f32;
    }
    Ok(out)
}

/// Aggregate 1 to 5 window embeddings into a single 512-dimensional track embedding.
///
/// Expects each input window embedding to already be unit-normalized (as produced
/// by [`normalize_embedding`]). If starting from raw model outputs, prefer
/// [`postprocess_clap_model_outputs`], which performs per-window normalization before
/// aggregation.
///
/// Computes the arithmetic mean of the unit-norm window vectors and L2-normalizes
/// the resulting aggregate vector.
pub fn aggregate_window_embeddings(
    windows: &[[f32; CLAP_EMBEDDING_DIM]],
) -> Result<ClapEmbedding, ClapAggregationError> {
    let mean = mean_embeddings(windows)?;
    let normalized = normalize_embedding(&mean)?;
    ClapEmbedding::from_normalized(normalized)
}

/// Pure postprocessing of raw model output vectors for 1 to 5 windows.
///
/// Ports the complete upstream CLAP embedding pipeline:
/// 1. Each raw window output vector is L2-normalized to unit norm (as in `onnx_backend.embed_clips`).
/// 2. The unit-norm window vectors are aggregated by taking their arithmetic mean across windows
///    (as in `analyzer.py:366`).
/// 3. The aggregate vector is L2-normalized to unit norm (as in `analyzer.py:368`).
/// 4. Validates that the resulting vector satisfies the unit-norm guard: `|norm - 1.0| <= 1e-3`.
///
/// Refuses:
/// - Empty windows (`EmptyWindows`)
/// - More than 5 windows (`TooManyWindows`)
/// - Non-finite values (`NonFiniteInput`)
/// - Zero or near-zero norms or complete cancellation (`ZeroNorm`)
pub fn postprocess_clap_model_outputs(
    raw_windows: &[[f32; CLAP_EMBEDDING_DIM]],
) -> Result<ClapEmbedding, ClapAggregationError> {
    if raw_windows.is_empty() {
        return Err(ClapAggregationError::EmptyWindows);
    }
    if raw_windows.len() > MAX_CLAP_WINDOWS {
        return Err(ClapAggregationError::TooManyWindows {
            count: raw_windows.len(),
            max: MAX_CLAP_WINDOWS,
        });
    }
    let mut normalized_windows = Vec::with_capacity(raw_windows.len());
    for raw in raw_windows {
        normalized_windows.push(normalize_embedding(raw)?);
    }
    aggregate_window_embeddings(&normalized_windows)
}
