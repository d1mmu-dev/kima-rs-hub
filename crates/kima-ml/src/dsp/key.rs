//! Chroma + Krumhansl–Schmuckler key (tonality) detection.
//!
//! ## Grounding
//!
//! Frame/hop size, chroma bin count and the whitening frequency range are
//! taken from the pinned upstream analyzer's own
//! `essentia.standard.KeyExtractor()` call (`analyzer.py`, plain default
//! construction, no overriding kwargs) — the task's option (a): the same
//! effective windowing parameters upstream used, on a different (own Rust,
//! no Essentia/aubio) engine. Essentia's `KeyExtractor` declares
//! (`src/algorithms/extractor/keyextractor.h`, `MTG/essentia` commit
//! `b9fa6cb674ca43dfb94d28d293aeda441c6745db`):
//!
//! - `frameSize` default **4096** samples
//! - `hopSize` default **4096** samples (no overlap)
//! - `minFrequency` default **25.0** Hz, `maxFrequency` default **3500.0** Hz
//!   (the band considered for pitch content)
//! - `tuningFrequency` default **440.0** Hz (A4 reference)
//! - `windowType` default `"hann"`
//! - `hpcpSize` default **12** (one bin per pitch class)
//!
//! The owner's 2026-11-04 decision explicitly rejects Essentia's own
//! profile-correlation engine (default `profileType="bgate"`, full HPCP
//! with spectral whitening and peak-picking) in favour of a plain chroma
//! vector (summed per-frame FFT-bin energy folded into 12 pitch classes, no
//! spectral whitening or peak-picking — a documented simplification, not
//! hidden) scored against the **Krumhansl–Schmuckler** key profiles,
//! published in:
//!
//! > Krumhansl, C. L., & Kessler, E. J. (1982). Tracing the dynamic changes
//! > in perceived tonal organization in a spatial representation of musical
//! > keys. *Psychological Review*, 89(4), 334–368.
//!
//! The exact 12 major / 12 minor probe-tone values below are the
//! widely-reproduced Krumhansl–Kessler (1982) table — this is the task's
//! option (b): a well-established published value, explicitly cited.
//! Independently corroborated byte-identical against an unrelated
//! open-source Rust MIR crate's own citation of the same source
//! (`docs.rs/timestretch`, `src/analysis/key.rs`, `KK_MAJOR`/`KK_MINOR`
//! constants).
//!
//! Key-finding method: Pearson correlation of the chroma vector against
//! major/minor profile templates rotated through all 12 roots, highest
//! correlation wins — the standard published Krumhansl–Schmuckler
//! algorithm, also offered (unused here) as Essentia's own
//! `profileType="krumhansl"` option.
//!
//! This module is DB-free, pure compute: no persistence, scheduler or
//! `attempt.rs` wiring. It consumes the same 16 kHz mono last-90-seconds
//! tail as `dsp::analyze_loudness`/musicnn (see `dsp/mod.rs` module docs);
//! callers pass that slice in directly.

use rustfft::{FftPlanner, num_complex::Complex};

/// Chroma analysis frame size, in samples. Matches the pinned upstream
/// `essentia.standard.KeyExtractor()` default `frameSize` (4096).
pub const KEY_FRAME_SIZE: usize = 4096;
/// Chroma analysis hop size, in samples (no overlap). Matches the pinned
/// upstream `KeyExtractor()` default `hopSize` (4096).
pub const KEY_HOP_SIZE: usize = 4096;
/// Lowest frequency considered part of pitch content, in Hz. Matches
/// `KeyExtractor()` default `minFrequency`.
pub const KEY_MIN_FREQUENCY_HZ: f64 = 25.0;
/// Highest frequency considered part of pitch content, in Hz. Matches
/// `KeyExtractor()` default `maxFrequency`.
pub const KEY_MAX_FREQUENCY_HZ: f64 = 3500.0;
/// Tuning reference (A4), in Hz. Matches `KeyExtractor()` default
/// `tuningFrequency`.
pub const TUNING_A4_HZ: f64 = 440.0;

/// Krumhansl & Kessler (1982) major-key probe-tone profile, pitch classes
/// relative to the tonic (index 0 is always the tonic).
pub const KRUMHANSL_MAJOR_PROFILE: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
/// Krumhansl & Kessler (1982) minor-key probe-tone profile, pitch classes
/// relative to the tonic (index 0 is always the tonic).
pub const KRUMHANSL_MINOR_PROFILE: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// Mode of a detected key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMode {
    Major,
    Minor,
}

/// Result of chroma + Krumhansl–Schmuckler key detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyAnalysis {
    /// Tonic pitch class, 0=C, 1=C#, 2=D, ..., 11=B.
    pub tonic: u8,
    pub mode: KeyMode,
    /// Pearson correlation of the chroma vector against the winning
    /// profile, in `[-1.0, 1.0]` — the Krumhansl–Schmuckler "key strength".
    pub strength: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("empty input: at least one sample is required")]
    EmptyInput,
    #[error("sample rate must be nonzero")]
    ZeroSampleRate,
    #[error("non-finite sample encountered in DSP input")]
    NonFiniteSample,
    #[error("input shorter than one analysis frame ({0} samples)")]
    TooShort(usize),
    #[error("no tonal energy found in any analysis frame (silence or out-of-band signal)")]
    NoTonalEnergy,
}

/// Pitch class (0..12, C-based) nearest to `freq_hz`, tuned to
/// [`TUNING_A4_HZ`].
fn pitch_class(freq_hz: f64) -> usize {
    let semitones_from_a4 = 12.0 * (freq_hz / TUNING_A4_HZ).log2();
    // A4 is pitch class 9 (A) in C-based numbering (C=0).
    let pc = (semitones_from_a4.round() as i64 + 9).rem_euclid(12);
    pc as usize
}

/// Computes a 12-bin chroma (pitch-class energy) vector for `samples` at
/// `sample_rate`, summed across non-overlapping [`KEY_FRAME_SIZE`] frames
/// within `[KEY_MIN_FREQUENCY_HZ, KEY_MAX_FREQUENCY_HZ]`. `None` if no
/// in-band energy was found in any frame (silence or no full frame fits).
fn chroma_vector(samples: &[f32], sample_rate: u32) -> Option<[f64; 12]> {
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(KEY_FRAME_SIZE);
    let window: Vec<f64> = (0..KEY_FRAME_SIZE)
        .map(|n| {
            0.5 - 0.5
                * (2.0 * std::f64::consts::PI * n as f64 / (KEY_FRAME_SIZE as f64 - 1.0)).cos()
        })
        .collect();

    let mut chroma = [0.0f64; 12];
    let mut any_energy = false;

    let bin_hz = sample_rate as f64 / KEY_FRAME_SIZE as f64;
    let lo_bin = (KEY_MIN_FREQUENCY_HZ / bin_hz).ceil().max(1.0) as usize;
    let hi_bin = ((KEY_MAX_FREQUENCY_HZ / bin_hz).floor() as usize).min(KEY_FRAME_SIZE / 2 - 1);

    let mut frame_start = 0usize;
    while frame_start + KEY_FRAME_SIZE <= samples.len() {
        let frame = &samples[frame_start..frame_start + KEY_FRAME_SIZE];
        let mut buf: Vec<Complex<f32>> = frame
            .iter()
            .zip(window.iter())
            .map(|(&s, &w)| Complex::new(s * w as f32, 0.0))
            .collect();
        fft.process(&mut buf);

        if lo_bin <= hi_bin {
            for (bin, value) in buf.iter().enumerate().take(hi_bin + 1).skip(lo_bin) {
                let freq = bin as f64 * bin_hz;
                let mag = value.norm() as f64;
                if mag > 0.0 {
                    let pc = pitch_class(freq);
                    chroma[pc] += mag * mag; // energy
                    any_energy = true;
                }
            }
        }
        frame_start += KEY_HOP_SIZE;
    }

    if !any_energy {
        return None;
    }
    Some(chroma)
}

fn pearson_correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let mean_a = a.iter().sum::<f64>() / 12.0;
    let mean_b = b.iter().sum::<f64>() / 12.0;
    let mut cov = 0.0;
    let mut var_a = 0.0;
    let mut var_b = 0.0;
    for i in 0..12 {
        let da = a[i] - mean_a;
        let db = b[i] - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
    }
    if var_a <= 0.0 || var_b <= 0.0 {
        return 0.0;
    }
    cov / (var_a.sqrt() * var_b.sqrt())
}

/// Detects the musical key (tonic + mode) of a mono PCM signal via chroma +
/// Krumhansl–Schmuckler profile correlation. See module docs for grounding
/// of every parameter used.
///
/// `samples` must be finite mono PCM, the same 16 kHz shared-decode tail
/// convention used by `dsp::analyze_loudness`/musicnn; this function
/// performs no resampling or tail selection itself.
pub fn analyze_key(samples: &[f32], sample_rate: u32) -> Result<KeyAnalysis, KeyError> {
    if sample_rate == 0 {
        return Err(KeyError::ZeroSampleRate);
    }
    if samples.is_empty() {
        return Err(KeyError::EmptyInput);
    }
    if samples.iter().any(|s| !s.is_finite()) {
        return Err(KeyError::NonFiniteSample);
    }
    if samples.len() < KEY_FRAME_SIZE {
        return Err(KeyError::TooShort(KEY_FRAME_SIZE));
    }

    let chroma = chroma_vector(samples, sample_rate).ok_or(KeyError::NoTonalEnergy)?;

    let mut best_score = f64::NEG_INFINITY;
    let mut best: Option<(u8, KeyMode)> = None;
    for tonic in 0u8..12 {
        for (mode, profile) in [
            (KeyMode::Major, &KRUMHANSL_MAJOR_PROFILE),
            (KeyMode::Minor, &KRUMHANSL_MINOR_PROFILE),
        ] {
            // Rotate the tonic-relative profile so its index `i` (semitones
            // above the tonic) lands at absolute pitch class `i + tonic`.
            let mut rotated = [0.0f64; 12];
            for (i, value) in profile.iter().enumerate() {
                rotated[(i + tonic as usize) % 12] = *value;
            }
            let score = pearson_correlation(&chroma, &rotated);
            if score > best_score {
                best_score = score;
                best = Some((tonic, mode));
            }
        }
    }

    let (tonic, mode) = best.ok_or(KeyError::NoTonalEnergy)?;
    Ok(KeyAnalysis {
        tonic,
        mode,
        strength: best_score,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Frequency of pitch class `pc` (0=C..11=B) in the octave
    /// `octave_number` relative to A4's octave (0 = same octave as A4;
    /// each unit shifts by exactly 12 semitones so `pitch_class(freq)`
    /// exactly inverts this for any `octave_number`).
    fn freq_for_pc(pc: u8, octave_number: i32) -> f64 {
        let semitones_from_a4 = (pc as i32 - 9) + 12 * octave_number;
        TUNING_A4_HZ * 2f64.powf(semitones_from_a4 as f64 / 12.0)
    }

    fn sine(freq_hz: f64, amplitude: f64, duration_s: f64, sample_rate: u32) -> Vec<f32> {
        let n = (duration_s * sample_rate as f64).round() as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                (amplitude * (2.0 * PI * freq_hz * t).sin()) as f32
            })
            .collect()
    }

    fn mix(components: &[Vec<f32>]) -> Vec<f32> {
        let len = components.iter().map(|c| c.len()).min().unwrap_or(0);
        (0..len)
            .map(|i| components.iter().map(|c| c[i]).sum())
            .collect()
    }

    /// Synthesizes `duration_s` seconds of audio whose chroma vector is, by
    /// construction, exactly proportional to the Krumhansl–Schmuckler
    /// profile for `tonic`/`mode` (one sine per pitch class, amplitude
    /// `sqrt(profile weight)` so summed power matches the profile weight),
    /// all in the single octave starting at `octave_offset_semitones`
    /// above A4, fully inside `[KEY_MIN_FREQUENCY_HZ, KEY_MAX_FREQUENCY_HZ]`.
    fn synth_for_key(tonic: u8, mode: KeyMode, sample_rate: u32, duration_s: f64) -> Vec<f32> {
        let profile = match mode {
            KeyMode::Major => &KRUMHANSL_MAJOR_PROFILE,
            KeyMode::Minor => &KRUMHANSL_MINOR_PROFILE,
        };
        let components: Vec<Vec<f32>> = (0u8..12)
            .map(|i| {
                let pc = (tonic + i) % 12;
                let weight = profile[i as usize];
                let freq = freq_for_pc(pc, -1); // keep all pitches audible/in-band
                sine(freq, (weight / 10.0).sqrt(), duration_s, sample_rate)
            })
            .collect();
        mix(&components)
    }

    #[test]
    fn empty_input_is_rejected() {
        let err = analyze_key(&[], 16_000).unwrap_err();
        assert!(matches!(err, KeyError::EmptyInput));
    }

    #[test]
    fn zero_sample_rate_is_rejected() {
        let err = analyze_key(&[0.0_f32; KEY_FRAME_SIZE], 0).unwrap_err();
        assert!(matches!(err, KeyError::ZeroSampleRate));
    }

    #[test]
    fn non_finite_sample_is_rejected() {
        let mut samples = vec![0.0_f32; KEY_FRAME_SIZE];
        samples[10] = f32::NAN;
        let err = analyze_key(&samples, 16_000).unwrap_err();
        assert!(matches!(err, KeyError::NonFiniteSample));
    }

    #[test]
    fn shorter_than_one_frame_is_rejected() {
        let err = analyze_key(&vec![0.0_f32; KEY_FRAME_SIZE - 1], 16_000).unwrap_err();
        assert!(matches!(err, KeyError::TooShort(KEY_FRAME_SIZE)));
    }

    #[test]
    fn digital_silence_has_no_tonal_energy() {
        let samples = vec![0.0_f32; KEY_FRAME_SIZE * 2];
        let err = analyze_key(&samples, 16_000).unwrap_err();
        assert!(matches!(err, KeyError::NoTonalEnergy));
    }

    #[test]
    fn detects_c_major_from_synthetic_krumhansl_weighted_chroma() {
        let samples = synth_for_key(0, KeyMode::Major, 16_000, 3.0);
        let result = analyze_key(&samples, 16_000).unwrap();
        assert_eq!(
            result.tonic, 0,
            "expected C, got pitch class {}",
            result.tonic
        );
        assert_eq!(result.mode, KeyMode::Major);
        assert!(result.strength > 0.9, "strength {}", result.strength);
    }

    #[test]
    fn detects_a_minor_from_synthetic_krumhansl_weighted_chroma() {
        let samples = synth_for_key(9, KeyMode::Minor, 16_000, 3.0);
        let result = analyze_key(&samples, 16_000).unwrap();
        assert_eq!(
            result.tonic, 9,
            "expected A, got pitch class {}",
            result.tonic
        );
        assert_eq!(result.mode, KeyMode::Minor);
        assert!(result.strength > 0.9, "strength {}", result.strength);
    }

    #[test]
    fn detects_f_sharp_major_across_all_twelve_tonics_sanity() {
        for tonic in 0u8..12 {
            let samples = synth_for_key(tonic, KeyMode::Major, 16_000, 3.0);
            let result = analyze_key(&samples, 16_000).unwrap();
            assert_eq!(
                result.tonic, tonic,
                "tonic {tonic}: detected {} instead",
                result.tonic
            );
            assert_eq!(result.mode, KeyMode::Major);
        }
    }
}
