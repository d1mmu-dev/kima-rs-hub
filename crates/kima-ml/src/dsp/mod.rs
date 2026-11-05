//! Pure DSP analysis: integrated LUFS and loudness range (LRA).
//!
//! DB-free, no persistence/scheduler/`attempt.rs` wiring. Consumes the same
//! 16 kHz mono last-90-seconds (or whole shorter track) tail that musicnn
//! consumes, per the owner's clarification that "musicnn and DSP analysis
//! retain the last 90 seconds" (`docs/superpowers/specs/2026-11-03-m4-ml-analysis-design.md`
//! line 13) — this is **not** the same window CLAP uses (whole track), and is
//! verified against the pinned upstream analyzer rather than assumed.
//!
//! ## Grounding (pinned upstream, loudness only)
//!
//! Pinned upstream [`analyzer.py`](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer/analyzer.py)
//! (commit `12faf1cd34ff53054221818b0ccde58b8e9b0020`) instantiates, in
//! `AudioAnalyzer.__init__`:
//!
//! ```python
//! # EBU R128: integrated loudness (LUFS) and loudness range (LU).
//! self.loudness_ebu = es.LoudnessEBUR128(sampleRate=44100)
//! ```
//!
//! `essentia.standard.LoudnessEBUR128` is Essentia's implementation of the
//! **EBU R128** loudness recommendation (integrated loudness / "M", "S", "I"
//! modes per EBU Tech 3341, and loudness range per EBU Tech 3342 — the same
//! public broadcast standard built on ITU-R BS.1770). Upstream runs this
//! analyzer on its own 44.1 kHz decode path; it is not run on musicnn's
//! resampled 16 kHz tail in `analyzer.py` (musicnn gets its own 16 kHz
//! resample via `self.resampler = es.Resample(inputSampleRate=44100,
//! outputSampleRate=16000)`, called only before the musicnn/mel extraction,
//! not before `self.loudness_ebu`).
//!
//! The parent binding Rust design
//! (`docs/superpowers/specs/2026-10-16-kima-rust-design.md`, section "DSP
//! features (diverges from Essentia)") already resolves the loudness side of
//! this for kima-rs: **"Loudness: `ebur128` (integrated LUFS, loudness
//! range), exact."** — i.e. not a divergence in *algorithm* (same EBU R128
//! standard as `LoudnessEBUR128`), only in *implementation library*: the
//! `ebur128` crate (sdroege, MIT, a Rust port of the reference `libebur128`
//! C library, "produces the same results as the C library") instead of
//! Essentia's C++ implementation. §6 also already fixes kima-rs's DSP input
//! sample rate to 16 kHz mono (parent design doc line 137: "16 kHz mono
//! (musicnn, DSP)"), which differs from upstream's 44.1 kHz DSP input; EBU
//! R128 is defined to be correct at any sample rate (the `ebur128` crate
//! documents "Supports all samplerates by recalculation of the filter
//! coefficients"), so this sample-rate difference does not change the
//! computed LUFS/LRA value for a given signal and is not a new divergence
//! beyond the already-accepted D-009 shared-decode / 16 kHz DSP input design.
//!
//! Licensing: `ebur128` 0.1.11 is MIT-licensed (pure Rust, no bundled/linked
//! C library by default — the C API is only built via the optional `capi`
//! feature with `cargo-c`, not used here); MIT is compatible with this
//! project's GPL-3.0-only license (permissive dependency, not a derivative
//! that changes project licensing obligations beyond carrying the MIT notice
//! in distribution).
//!
//! ## Key and tempo (owner decision 2026-11-04): grounded, implemented
//!
//! Key (tonality) and tempo (BPM) are implemented as pure, DB-free Rust
//! compute functions in the [`key`] and [`tempo`] submodules: chroma +
//! Krumhansl–Schmuckler for key, onset-strength + autocorrelation/comb for
//! tempo, per the owner's explicit 2026-11-04 decision rejecting `aubio`
//! FFI (no C/FFI dependency for this). Every parameter used (chroma
//! frame/hop size, key profile table, onset-envelope FFT/hop size, tempo
//! search range, comb-filter resolution) is grounded and cited in each
//! submodule's own docs, either from the pinned upstream analyzer's own
//! `essentia.standard.KeyExtractor()`/`RhythmExtractor2013()` default
//! parameters (same effective parameters, different engine), or from
//! well-established published values (Krumhansl & Kessler 1982 profile
//! table; `librosa.onset.onset_strength` default FFT/hop size) — see
//! `docs/DIVERGENCES.md` D-008 (now `accepted-with-params`) for the full
//! citation trail. No algorithm parameter here was invented ad hoc.

pub mod key;
pub mod tempo;

pub use key::{KeyAnalysis, KeyError, KeyMode, analyze_key};
pub use tempo::{TempoAnalysis, TempoError, analyze_tempo};

use ebur128::{EbuR128, Error as EbuR128Error, Mode};

/// Integrated loudness (LUFS) and loudness range (LU) of a mono PCM signal,
/// computed per the EBU R128 recommendation (EBU Tech 3341/3342), matching
/// the algorithm pinned upstream's `essentia.standard.LoudnessEBUR128` also
/// implements.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnessAnalysis {
    /// Integrated program loudness, in LUFS (an absolute unit; EBU R128's
    /// recommended target level of -23 LUFS is a separate normalization
    /// convention, not part of this measurement).
    pub integrated_lufs: f64,
    /// Loudness range, in LU, per EBU Tech 3342.
    pub loudness_range_lu: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum DspError {
    #[error("empty input: at least one sample is required")]
    EmptyInput,
    #[error("non-finite sample encountered in DSP input")]
    NonFiniteSample,
    #[error("sample rate must be nonzero")]
    ZeroSampleRate,
    #[error("ebur128 analyzer error: {0:?}")]
    Loudness(EbuR128Error),
    /// Integrated loudness is undefined (signal entirely below the absolute
    /// gating threshold, e.g. digital silence); upstream's `LoudnessEBUR128`
    /// algorithm family produces `-inf` in this case rather than a numeric
    /// LUFS value. This is reported as an explicit error rather than
    /// silently returning `-inf` or fabricating a floor value.
    #[error("integrated loudness is undefined for this input (below absolute gating threshold)")]
    UndefinedLoudness,
}

/// Compute integrated LUFS and loudness range (LRA) for a mono PCM signal
/// using the EBU R128 algorithm (EBU Tech 3341 "M/S/I" modes, EBU Tech 3342
/// loudness range), matching the same public standard pinned upstream's
/// `essentia.standard.LoudnessEBUR128` implements.
///
/// `samples` must be finite mono PCM in `[-1.0, 1.0]`-scale float samples
/// (the existing shared-decode/final-tail convention used by musicnn); this
/// function performs no resampling, windowing or tail selection itself —
/// callers pass in the already-selected 16 kHz mono tail.
pub fn analyze_loudness(samples: &[f32], sample_rate: u32) -> Result<LoudnessAnalysis, DspError> {
    if sample_rate == 0 {
        return Err(DspError::ZeroSampleRate);
    }
    if samples.is_empty() {
        return Err(DspError::EmptyInput);
    }
    if samples.iter().any(|s| !s.is_finite()) {
        return Err(DspError::NonFiniteSample);
    }

    let mut analyzer =
        EbuR128::new(1, sample_rate, Mode::I | Mode::LRA).map_err(DspError::Loudness)?;
    analyzer
        .add_frames_f32(samples)
        .map_err(DspError::Loudness)?;

    let integrated_lufs = analyzer.loudness_global().map_err(DspError::Loudness)?;
    if !integrated_lufs.is_finite() {
        return Err(DspError::UndefinedLoudness);
    }
    let loudness_range_lu = analyzer.loudness_range().map_err(DspError::Loudness)?;

    Ok(LoudnessAnalysis {
        integrated_lufs,
        loudness_range_lu,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// Generates a mono sine wave at `freq_hz`, `amplitude` (linear, peak),
    /// `duration_s` seconds at `sample_rate` Hz.
    fn sine(freq_hz: f64, amplitude: f64, duration_s: f64, sample_rate: u32) -> Vec<f32> {
        let n = (duration_s * sample_rate as f64).round() as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                (amplitude * (2.0 * PI * freq_hz * t).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn empty_input_is_rejected() {
        let err = analyze_loudness(&[], 16_000).unwrap_err();
        assert!(matches!(err, DspError::EmptyInput));
    }

    #[test]
    fn zero_sample_rate_is_rejected() {
        let err = analyze_loudness(&[0.0_f32; 16_000], 0).unwrap_err();
        assert!(matches!(err, DspError::ZeroSampleRate));
    }

    #[test]
    fn non_finite_sample_is_rejected() {
        let mut samples = vec![0.0_f32; 1000];
        samples[500] = f32::NAN;
        let err = analyze_loudness(&samples, 16_000).unwrap_err();
        assert!(matches!(err, DspError::NonFiniteSample));

        let mut samples = vec![0.0_f32; 1000];
        samples[10] = f32::INFINITY;
        let err = analyze_loudness(&samples, 16_000).unwrap_err();
        assert!(matches!(err, DspError::NonFiniteSample));
    }

    #[test]
    fn digital_silence_is_undefined_loudness() {
        let samples = vec![0.0_f32; 16_000 * 2];
        let err = analyze_loudness(&samples, 16_000).unwrap_err();
        assert!(matches!(err, DspError::UndefinedLoudness));
    }

    /// Known-level calibration point from the ITU-R BS.1770-4 (the standard
    /// EBU R128 is built on) conformance reference: a mono 997 Hz sine wave
    /// at -20 dBFS peak measures integrated loudness of -23.0 LUFS (within
    /// the ITU conformance tolerance of ±0.1 LU). This is a cited standard
    /// calibration constant, not a value derived from running this code and
    /// asserting its own output.
    #[test]
    fn known_level_sine_matches_itu_calibration_point() {
        let amplitude = 10f64.powf(-20.0 / 20.0); // -20 dBFS peak amplitude
        let samples = sine(997.0, amplitude, 5.0, 16_000);
        let result = analyze_loudness(&samples, 16_000).unwrap();
        assert!(
            (result.integrated_lufs - (-23.0)).abs() < 0.1,
            "expected -23.0 LUFS +/- 0.1, got {}",
            result.integrated_lufs
        );
    }

    /// Loudness is scale-invariant in the sample rate for a signal expressed
    /// in seconds (same calibration point, resampled analysis rate as used
    /// in production: 16 kHz).
    #[test]
    fn known_level_sine_is_sample_rate_independent() {
        let amplitude = 10f64.powf(-20.0 / 20.0);
        let at_16k = analyze_loudness(&sine(997.0, amplitude, 5.0, 16_000), 16_000).unwrap();
        let at_44_1k = analyze_loudness(&sine(997.0, amplitude, 5.0, 44_100), 44_100).unwrap();
        assert!(
            (at_16k.integrated_lufs - at_44_1k.integrated_lufs).abs() < 0.05,
            "16kHz={} 44.1kHz={}",
            at_16k.integrated_lufs,
            at_44_1k.integrated_lufs
        );
    }

    /// Known-gap loudness range: two equal-duration constant-level segments
    /// 10 dB apart produce a gated loudness-range reading of ~10 LU. This is
    /// not an invented value: for a pure sustained tone, K-weighted gated
    /// block loudness differs by exactly the dB difference between the two
    /// segments' linear amplitudes (both segments are far above the -70 LUFS
    /// absolute gate and within the -20 LU relative gate of each other), so
    /// the 95th/10th percentile loudness-range reading is mathematically
    /// bound to the constructed 10 dB amplitude gap, independent of any
    /// external reference document.
    #[test]
    fn known_level_gap_matches_expected_loudness_range() {
        let sample_rate = 16_000;
        let loud = sine(997.0, 10f64.powf(-20.0 / 20.0), 6.0, sample_rate);
        let quiet = sine(997.0, 10f64.powf(-30.0 / 20.0), 6.0, sample_rate);
        let mut samples = loud;
        samples.extend(quiet);
        let result = analyze_loudness(&samples, sample_rate).unwrap();
        assert!(
            (result.loudness_range_lu - 10.0).abs() < 0.5,
            "expected ~10.0 LU, got {}",
            result.loudness_range_lu
        );
    }

    /// Constant level across the whole signal: loudness range should be ~0
    /// (no gated variation), regardless of integrated level.
    #[test]
    fn constant_level_has_near_zero_loudness_range() {
        let samples = sine(997.0, 10f64.powf(-20.0 / 20.0), 8.0, 16_000);
        let result = analyze_loudness(&samples, 16_000).unwrap();
        assert!(
            result.loudness_range_lu.abs() < 0.3,
            "expected near-zero LRA, got {}",
            result.loudness_range_lu
        );
    }

    /// A single short (far below gating block length) impulse-like input
    /// still runs without panicking and returns either a numeric result or
    /// an explicit `UndefinedLoudness`/`ebur128` error, never a silent wrong
    /// value; this guards against assuming a minimum 90-second tail length.
    #[test]
    fn very_short_input_fails_closed_or_succeeds_explicitly() {
        let samples = sine(997.0, 0.5, 0.05, 16_000); // 50ms, below 400ms gating block
        match analyze_loudness(&samples, 16_000) {
            Ok(result) => assert!(result.integrated_lufs.is_finite()),
            Err(DspError::UndefinedLoudness) | Err(DspError::Loudness(_)) => {}
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }
}
