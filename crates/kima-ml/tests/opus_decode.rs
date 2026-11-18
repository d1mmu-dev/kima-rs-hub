//! Ogg Opus decoding for analysis and CLAP embedding. Upstream decodes Opus with ffmpeg
//! (analysis) and libsndfile/libopus via librosa (CLAP); both apply the pre-skip and the end
//! trim from the final granule position. The fixture and its reference come from
//! `fixtures/generate_opus_fixture.sh` (synthetic tones, ffmpeg + libopus).

use std::path::PathBuf;

use kima_ml::audio::{decode_and_resample, decode_clap_auto, fingerprint_decoded_audio};
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSource;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone_stereo_3s.opus")
}

/// `(frames, [(index, mono value)])` from `opus_reference.txt`.
fn reference() -> (usize, Vec<(usize, f32)>) {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opus_reference.txt"),
    )
    .unwrap();
    let mut lines = text.lines();
    let frames = lines
        .next()
        .unwrap()
        .strip_prefix("frames ")
        .unwrap()
        .parse()
        .unwrap();
    let points = lines
        .map(|l| {
            let (i, v) = l.split_once(' ').unwrap();
            (i.parse().unwrap(), v.parse().unwrap())
        })
        .collect();
    (frames, points)
}

fn hint() -> Hint {
    let mut h = Hint::new();
    h.with_extension("opus");
    h
}

/// Different libopus builds (float, SIMD paths) need not agree bit for bit; a pre-skip error
/// shifts a 440/660 Hz tone by 312 frames and misses by far more than this.
const TOLERANCE: f32 = 2e-3;

#[test]
fn ogg_opus_decodes_at_48k_with_pre_skip_and_end_trim() {
    let (frames, points) = reference();
    let audio = decode_and_resample(&fixture()).expect("Ogg Opus must decode");
    // 3 s < 90 s: the tail is the whole track, at the native 48 kHz (no resampling).
    assert_eq!(audio.mono_48k.len(), frames);
    assert_eq!(audio.duration_seconds, 3.0);
    assert_eq!(audio.mono_16k.len(), frames / 3);
    for (i, want) in points {
        let got = audio.mono_48k[i];
        assert!(
            (got - want).abs() <= TOLERANCE,
            "frame {i}: decoded {got}, reference {want}"
        );
    }
}

#[test]
fn ogg_opus_fingerprint_counts_every_trimmed_frame_at_48k() {
    let (frames, _) = reference();
    let source: Box<dyn MediaSource> = Box::new(std::fs::File::open(fixture()).unwrap());
    let fp = fingerprint_decoded_audio(source, &hint()).expect("Ogg Opus must decode");
    assert_eq!(fp.sample_rate, 48_000);
    assert_eq!(fp.frames, frames);
}

#[test]
fn ogg_opus_clap_windows_decode_without_a_frame_hint() {
    let (frames, _) = reference();
    let mut opens = 0;
    let (tail, windows, rate) = decode_clap_auto(
        || {
            opens += 1;
            Ok(Box::new(std::fs::File::open(fixture())?))
        },
        &hint(),
        None,
    )
    .expect("Ogg Opus CLAP windows must decode");
    assert_eq!(opens, 2, "no hint: discovery pass then verified pass");
    assert_eq!(rate, 48_000);
    assert_eq!(tail.mono_48k.len(), frames);
    let ranges = kima_ml::clap::source_window_ranges(frames, rate).unwrap();
    assert_eq!(windows.len(), ranges.len());
    for (w, r) in windows.iter().zip(ranges) {
        assert_eq!(w.len(), r.len());
    }
}
