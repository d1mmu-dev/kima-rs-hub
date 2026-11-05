//! Onset-strength + autocorrelation ("comb") tempo (BPM) estimation.
//!
//! ## Grounding
//!
//! BPM search range (40–208 BPM) matches the pinned upstream analyzer's own
//! `essentia.standard.RhythmExtractor2013(method="multifeature")` call
//! (`analyzer.py`, default `minTempo`/`maxTempo`, no overriding kwargs):
//! `RhythmExtractor2013` declares (`src/algorithms/rhythm/rhythmextractor2013.h`,
//! `MTG/essentia` commit `b9fa6cb674ca43dfb94d28d293aeda441c6745db`):
//! `minTempo` default **40** bpm (declared range `[40,180]`), `maxTempo`
//! default **208** bpm (declared range `[60,250]`) — the task's option
//! (a): the same effective search range upstream used, on a different (own
//! Rust, no Essentia/aubio) engine.
//!
//! `RhythmExtractor2013` is a composite multi-feature beat tracker with no
//! single exposed onset-frame/hop parameter to carry over (its internal
//! network combines several periodicity functions), so the onset
//! envelope's own STFT frame/hop size is grounded separately, under the
//! task's option (b): **2048-sample FFT / 512-sample hop**, the published
//! default of `librosa.onset.onset_strength`/`librosa.stft` (McFee, B., et
//! al. (2015). "librosa: Audio and music signal analysis in python."
//! *Proceedings of the 14th Python in Science Conference*) — a widely used
//! onset-detection convention. At this project's 16 kHz DSP input that is a
//! 128 ms analysis frame / 32 ms hop, inside the 10–50 ms onset-hop range
//! surveyed by Bello, J. P., et al. (2005). "A tutorial on onset detection
//! in music signals." *IEEE Transactions on Speech and Audio Processing*,
//! 13(5).
//!
//! Onset-strength function: rectified spectral flux (sum of positive
//! frame-to-frame magnitude-spectrum increases) — the standard "spectral
//! difference" onset detection function surveyed in Bello et al. (2005)
//! §III-B and Dixon, S. (2006). "Onset detection revisited." *Proc. of the
//! 9th Int. Conference on Digital Audio Effects (DAFx-06)*.
//!
//! Tempo estimation: autocorrelation of the onset envelope restricted to
//! the lag range implied by `[MIN_TEMPO_BPM, MAX_TEMPO_BPM]` at the chosen
//! hop size, picking the lag of maximum *weighted* autocorrelation — the
//! standard onset-autocorrelation ("comb filter") tempo estimate of Ellis,
//! D. P. W. (2007). "Beat tracking by dynamic programming." *Journal of
//! New Music Research*, 36(1), 51–60, including that paper's own tempo
//! preference window (eq. 5/6): raw autocorrelation at lag `tau` is
//! multiplied by `W(tau) = exp(-0.5 * (log2(tau/tau_0) / sigma_tau)^2)`,
//! with the paper's own hand-tuned `tau_0 = 0.5s` (120 BPM centre) and
//! `sigma_tau = 1.4` octaves (the paper's own reported MIREX-tuned best
//! values, not re-derived here). This log-Gaussian weighting is the
//! published, standard fix for the harmonic/subharmonic ambiguity inherent
//! to plain autocorrelation of any periodic onset train (every integer
//! multiple/divisor of the true beat period also autocorrelates
//! strongly) — not an invented tie-break heuristic. The "comb-filter
//! resolution" is exactly one onset-envelope hop (512 samples / 16 kHz =
//! 32 ms per lag step), the same grounded hop size above.
//!
//! This module is DB-free, pure compute: no persistence, scheduler or
//! `attempt.rs` wiring. It consumes the same 16 kHz mono last-90-seconds
//! tail as `dsp::analyze_loudness`/`dsp::key::analyze_key`/musicnn (see
//! `dsp/mod.rs` module docs); callers pass that slice in directly.

use rustfft::{FftPlanner, num_complex::Complex};
use std::f64::consts::PI;

/// Onset-envelope analysis FFT size, in samples. Matches
/// `librosa.onset.onset_strength`'s published default `n_fft` (2048).
pub const ONSET_FFT_SIZE: usize = 2048;
/// Onset-envelope analysis hop size, in samples. Matches
/// `librosa.onset.onset_strength`'s published default `hop_length` (512).
pub const ONSET_HOP_SIZE: usize = 512;
/// Slowest tempo searched, in BPM. Matches the pinned upstream
/// `RhythmExtractor2013(method="multifeature")` default `minTempo`.
pub const MIN_TEMPO_BPM: f64 = 40.0;
/// Fastest tempo searched, in BPM. Matches the pinned upstream
/// `RhythmExtractor2013(method="multifeature")` default `maxTempo`.
pub const MAX_TEMPO_BPM: f64 = 208.0;

/// Result of onset-strength + autocorrelation tempo estimation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoAnalysis {
    pub bpm: f64,
    /// Normalized autocorrelation peak height (peak / mean onset power),
    /// clamped to `[0.0, 1.0]`.
    pub confidence: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum TempoError {
    #[error("empty input: at least one sample is required")]
    EmptyInput,
    #[error("sample rate must be nonzero")]
    ZeroSampleRate,
    #[error("non-finite sample encountered in DSP input")]
    NonFiniteSample,
    #[error("input shorter than one analysis frame ({0} samples)")]
    TooShort(usize),
    #[error("onset envelope too short to search the configured tempo range")]
    InsufficientOnsetFrames,
    #[error("no onset energy detected (silence or a constant signal)")]
    NoOnsetEnergy,
}

/// Rectified spectral-flux onset-strength envelope, one value per
/// [`ONSET_HOP_SIZE`]-sample hop, over non-overlapping-start
/// [`ONSET_FFT_SIZE`]-sample Hann-windowed frames.
fn onset_envelope(samples: &[f32]) -> Vec<f64> {
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(ONSET_FFT_SIZE);
    let window: Vec<f64> = (0..ONSET_FFT_SIZE)
        .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / (ONSET_FFT_SIZE as f64 - 1.0)).cos())
        .collect();

    let mut prev_mag: Option<Vec<f64>> = None;
    let mut envelope = Vec::new();
    let mut start = 0usize;
    while start + ONSET_FFT_SIZE <= samples.len() {
        let frame = &samples[start..start + ONSET_FFT_SIZE];
        let mut buf: Vec<Complex<f32>> = frame
            .iter()
            .zip(window.iter())
            .map(|(&s, &w)| Complex::new(s * w as f32, 0.0))
            .collect();
        fft.process(&mut buf);
        let mag: Vec<f64> = buf[..ONSET_FFT_SIZE / 2]
            .iter()
            .map(|c| c.norm() as f64)
            .collect();

        let flux = match &prev_mag {
            Some(prev) => mag
                .iter()
                .zip(prev.iter())
                .map(|(m, p)| (m - p).max(0.0))
                .sum::<f64>(),
            None => 0.0,
        };
        envelope.push(flux);
        prev_mag = Some(mag);
        start += ONSET_HOP_SIZE;
    }
    envelope
}

/// Estimates tempo (BPM) of a mono PCM signal via onset-strength
/// autocorrelation. See module docs for grounding of every parameter used.
///
/// `samples` must be finite mono PCM, the same 16 kHz shared-decode tail
/// convention used by `dsp::analyze_loudness`/`dsp::key::analyze_key`; this
/// function performs no resampling or tail selection itself.
pub fn analyze_tempo(samples: &[f32], sample_rate: u32) -> Result<TempoAnalysis, TempoError> {
    if sample_rate == 0 {
        return Err(TempoError::ZeroSampleRate);
    }
    if samples.is_empty() {
        return Err(TempoError::EmptyInput);
    }
    if samples.iter().any(|s| !s.is_finite()) {
        return Err(TempoError::NonFiniteSample);
    }
    if samples.len() < ONSET_FFT_SIZE {
        return Err(TempoError::TooShort(ONSET_FFT_SIZE));
    }

    let envelope = onset_envelope(samples);

    let min_lag = ((60.0 * sample_rate as f64) / (MAX_TEMPO_BPM * ONSET_HOP_SIZE as f64))
        .round()
        .max(1.0) as usize;
    let max_lag =
        ((60.0 * sample_rate as f64) / (MIN_TEMPO_BPM * ONSET_HOP_SIZE as f64)).round() as usize;
    if max_lag == 0 || min_lag >= max_lag || envelope.len() <= max_lag {
        return Err(TempoError::InsufficientOnsetFrames);
    }

    let mean_power = envelope.iter().map(|v| v * v).sum::<f64>() / envelope.len() as f64;
    if mean_power <= 0.0 {
        return Err(TempoError::NoOnsetEnergy);
    }

    // Raw autocorrelation at each candidate lag is multiplied by a
    // log-Gaussian "tempo preference" weighting window centered at 120 BPM
    // (0.5s period), width 1.4 octaves — Ellis, D. P. W. (2007), "Beat
    // Tracking by Dynamic Programming", Journal of New Music Research,
    // 36(1), 51-60, eq. (5)/(6): `TPS(tau) = W(tau) * sum_t O(t)O(t-tau)`,
    // `W(tau) = exp(-0.5 * (log2(tau/tau_0)/sigma_tau)^2)`, with the
    // paper's own tuned `tau_0 = 0.5s` (120 BPM) and `sigma_tau = 1.4`
    // octaves. This resolves the harmonic/subharmonic ambiguity inherent
    // to plain autocorrelation of a periodic onset train (every integer
    // multiple/divisor of the true beat period also autocorrelates
    // strongly), the standard, cited fix rather than an invented
    // tie-break heuristic.
    const TAU_0_S: f64 = 0.5;
    const SIGMA_TAU_OCTAVES: f64 = 1.4;

    let mut best_lag = min_lag;
    let mut best_tps = f64::NEG_INFINITY;
    let mut best_raw_corr = 0.0;
    for lag in min_lag..=max_lag {
        let n = envelope.len() - lag;
        if n == 0 {
            continue;
        }
        let mut raw_corr = 0.0;
        for i in 0..n {
            raw_corr += envelope[i] * envelope[i + lag];
        }
        let tau_s = lag as f64 * ONSET_HOP_SIZE as f64 / sample_rate as f64;
        let log_ratio = (tau_s / TAU_0_S).log2() / SIGMA_TAU_OCTAVES;
        let weight = (-0.5 * log_ratio * log_ratio).exp();
        let tps = weight * raw_corr;
        if tps > best_tps {
            best_tps = tps;
            best_lag = lag;
            best_raw_corr = raw_corr / n as f64;
        }
    }

    let bpm = 60.0 * sample_rate as f64 / (best_lag as f64 * ONSET_HOP_SIZE as f64);
    let confidence = (best_raw_corr / mean_power).clamp(0.0, 1.0);

    Ok(TempoAnalysis { bpm, confidence })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesizes a click track at exactly `bpm`: a short decaying burst
    /// of broadband noise-like energy (summed harmonics, not a pure tone,
    /// so it has real spectral flux) once per beat period, for
    /// `duration_s` seconds.
    fn click_track(bpm: f64, duration_s: f64, sample_rate: u32) -> Vec<f32> {
        let period_s = 60.0 / bpm;
        let n = (duration_s * sample_rate as f64).round() as usize;
        let click_len_s = 0.03; // 30ms decaying click
        let click_len_n = (click_len_s * sample_rate as f64).round() as usize;
        let mut out = vec![0.0f32; n];
        let mut beat_t = 0.0;
        while beat_t < duration_s {
            let start = (beat_t * sample_rate as f64).round() as usize;
            for i in 0..click_len_n {
                let idx = start + i;
                if idx >= n {
                    break;
                }
                let t = i as f64 / sample_rate as f64;
                let decay = (-t / (click_len_s / 4.0)).exp();
                // Sum of a few harmonics so the click has broadband
                // spectral content (nonzero spectral flux across bins),
                // not a single pure tone.
                let tone: f64 = [1000.0, 2500.0, 4000.0]
                    .iter()
                    .map(|f| (2.0 * PI * f * t).sin())
                    .sum();
                out[idx] += (0.3 * decay * tone) as f32;
            }
            beat_t += period_s;
        }
        out
    }

    #[test]
    fn empty_input_is_rejected() {
        let err = analyze_tempo(&[], 16_000).unwrap_err();
        assert!(matches!(err, TempoError::EmptyInput));
    }

    #[test]
    fn zero_sample_rate_is_rejected() {
        let err = analyze_tempo(&[0.0_f32; ONSET_FFT_SIZE], 0).unwrap_err();
        assert!(matches!(err, TempoError::ZeroSampleRate));
    }

    #[test]
    fn non_finite_sample_is_rejected() {
        let mut samples = vec![0.0_f32; ONSET_FFT_SIZE];
        samples[5] = f32::INFINITY;
        let err = analyze_tempo(&samples, 16_000).unwrap_err();
        assert!(matches!(err, TempoError::NonFiniteSample));
    }

    #[test]
    fn shorter_than_one_frame_is_rejected() {
        let err = analyze_tempo(&vec![0.0_f32; ONSET_FFT_SIZE - 1], 16_000).unwrap_err();
        assert!(matches!(err, TempoError::TooShort(ONSET_FFT_SIZE)));
    }

    #[test]
    fn digital_silence_has_no_onset_energy() {
        let samples = vec![0.0_f32; 16_000 * 4];
        let err = analyze_tempo(&samples, 16_000).unwrap_err();
        assert!(matches!(err, TempoError::NoOnsetEnergy));
    }

    #[test]
    fn constant_tone_runs_without_panicking_and_yields_a_valid_result() {
        // A steady tone is not within scope of a grounded "this must read
        // as low confidence" threshold (no such threshold is pinned
        // anywhere); plain spectral-flux onset detection on a continuous
        // tone can still show weak, very regular STFT-windowing artifacts
        // that an autocorrelation-based estimator picks up as a spurious
        // periodicity. This is a documented, known limitation of the
        // grounded algorithm as specified, not silently patched over by an
        // invented confidence cutoff. The only thing asserted here is that
        // the function never panics or returns a non-finite/out-of-range
        // result on a toneless signal.
        let sample_rate = 16_000u32;
        let n = sample_rate as usize * 4;
        let samples: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                (0.2 * (2.0 * PI * 440.0 * t).sin()) as f32
            })
            .collect();
        match analyze_tempo(&samples, sample_rate) {
            Err(TempoError::NoOnsetEnergy) => {}
            Ok(result) => {
                assert!(result.bpm.is_finite() && result.bpm > 0.0);
                assert!((0.0..=1.0).contains(&result.confidence));
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    /// Known-tempo click track, checked within the binding spec's own
    /// tempo accuracy tolerance (`2026-11-03-m4-ml-analysis-design.md`:
    /// "tempo ... within ±4% or double/half").
    fn assert_bpm_within_tolerance(expected_bpm: f64, measured_bpm: f64) {
        let ratio = measured_bpm / expected_bpm;
        let within_direct = (measured_bpm - expected_bpm).abs() / expected_bpm <= 0.04;
        let within_double = (ratio - 2.0).abs() / 2.0 <= 0.04;
        let within_half = (ratio - 0.5).abs() / 0.5 <= 0.04;
        assert!(
            within_direct || within_double || within_half,
            "expected ~{expected_bpm} bpm (or double/half, +/-4%), got {measured_bpm}"
        );
    }

    #[test]
    fn detects_120_bpm_click_track() {
        let samples = click_track(120.0, 8.0, 16_000);
        let result = analyze_tempo(&samples, 16_000).unwrap();
        assert_bpm_within_tolerance(120.0, result.bpm);
    }

    #[test]
    fn detects_90_bpm_click_track() {
        let samples = click_track(90.0, 10.0, 16_000);
        let result = analyze_tempo(&samples, 16_000).unwrap();
        assert_bpm_within_tolerance(90.0, result.bpm);
    }

    #[test]
    fn detects_150_bpm_click_track() {
        let samples = click_track(150.0, 8.0, 16_000);
        let result = analyze_tempo(&samples, 16_000).unwrap();
        assert_bpm_within_tolerance(150.0, result.bpm);
    }

    #[test]
    fn detects_60_bpm_click_track_near_min_tempo() {
        let samples = click_track(60.0, 12.0, 16_000);
        let result = analyze_tempo(&samples, 16_000).unwrap();
        assert_bpm_within_tolerance(60.0, result.bpm);
    }
}
