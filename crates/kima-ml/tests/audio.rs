use kima_ml::audio::{AudioError, decode_and_resample, decode_and_resample_bytes};
use std::io::Write;
use tempfile::NamedTempFile;

/// Creates a standard 16-bit PCM RIFF WAV file in memory.
#[allow(clippy::needless_range_loop)]
fn create_pcm_wav(channels: u16, sample_rate: u32, channel_samples: &[Vec<f32>]) -> Vec<u8> {
    assert_eq!(channels as usize, channel_samples.len());
    let num_frames = channel_samples[0].len();
    for ch in channel_samples {
        assert_eq!(ch.len(), num_frames);
    }

    let bytes_per_sample: u16 = 2; // 16-bit PCM
    let block_align = channels * bytes_per_sample;
    let byte_rate = sample_rate * block_align as u32;
    let subchunk2_size = (num_frames as u32) * (block_align as u32);
    let chunk_size = 36 + subchunk2_size;

    let mut out = Vec::with_capacity(44 + subchunk2_size as usize);
    // RIFF chunk descriptor
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");

    // "fmt " subchunk
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // Subchunk1Size for PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // AudioFormat 1 = PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes()); // BitsPerSample = 16

    // "data" subchunk
    out.extend_from_slice(b"data");
    out.extend_from_slice(&subchunk2_size.to_le_bytes());

    for frame in 0..num_frames {
        for ch in 0..channels as usize {
            let s = channel_samples[ch][frame];
            let clamped = s.clamp(-1.0, 1.0);
            let val = (clamped * 32767.0).round() as i16;
            out.extend_from_slice(&val.to_le_bytes());
        }
    }

    out
}

#[test]
fn float_wav_nonfinite_is_rejected() {
    let mut wav = create_pcm_wav(1, 16_000, &[vec![0.0; 4096]]);
    wav[20..22].copy_from_slice(&3u16.to_le_bytes());
    wav[28..32].copy_from_slice(&64_000u32.to_le_bytes());
    wav[32..34].copy_from_slice(&4u16.to_le_bytes());
    wav[34..36].copy_from_slice(&32u16.to_le_bytes());
    wav[4..8].copy_from_slice(&(36u32 + 4096 * 4).to_le_bytes());
    wav[40..44].copy_from_slice(&(4096u32 * 4).to_le_bytes());
    wav.truncate(44);
    for i in 0..4096 {
        wav.extend_from_slice(&(if i == 42 { f32::NAN } else { 0.5 }).to_le_bytes());
    }
    assert!(matches!(
        decode_and_resample_bytes(&wav, Some("wav")),
        Err(AudioError::CorruptAudio(_))
    ));
}

#[test]
fn test_decode_short_mono_track() {
    let sample_rate = 44100;
    let duration_sec = 2.0;
    let num_samples = (sample_rate as f64 * duration_sec) as usize;

    let mut samples = Vec::with_capacity(num_samples);
    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        // 440 Hz sine wave
        samples.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5);
    }

    let wav = create_pcm_wav(1, sample_rate, &[samples]);
    let decoded = decode_and_resample_bytes(&wav, Some("wav")).expect("decode failed");

    // Full track should be preserved when <= 90s
    let expected_16k_len = (16000.0 * duration_sec) as usize;
    let expected_48k_len = (48000.0 * duration_sec) as usize;

    // Resampler output should match expected length within ±1% (due to filter padding)
    let diff_16k = (decoded.mono_16k.len() as isize - expected_16k_len as isize).abs();
    assert!(
        diff_16k < (expected_16k_len as isize / 50).max(100),
        "16k len {} vs expected {}",
        decoded.mono_16k.len(),
        expected_16k_len
    );

    let diff_48k = (decoded.mono_48k.len() as isize - expected_48k_len as isize).abs();
    assert!(
        diff_48k < (expected_48k_len as isize / 50).max(100),
        "48k len {} vs expected {}",
        decoded.mono_48k.len(),
        expected_48k_len
    );

    // Verify non-silent samples and within [-1.0, 1.0]
    let max_amp = decoded.mono_16k.iter().cloned().fold(0.0f32, f32::max);
    assert!(
        max_amp > 0.4 && max_amp <= 1.0,
        "unexpected amplitude: {max_amp}"
    );
}

#[test]
fn test_decode_stereo_downmix_to_mono_averages_channels() {
    let sample_rate = 48000;
    let duration_sec = 0.5;
    let num_samples = (sample_rate as f64 * duration_sec) as usize;

    // Left channel = constant 0.8, Right channel = constant 0.2
    let left = vec![0.8f32; num_samples];
    let right = vec![0.2f32; num_samples];

    let wav = create_pcm_wav(2, sample_rate, &[left, right]);
    let decoded = decode_and_resample_bytes(&wav, Some("wav")).expect("decode failed");

    // Average of 0.8 and 0.2 is 0.5
    // For 48kHz input, mono_48k should directly be the downmixed mono signal (~0.5)
    for (i, &s) in decoded.mono_48k.iter().take(1000).enumerate() {
        assert!(
            (s - 0.5).abs() < 0.01,
            "sample {i} expected ~0.5 but got {s}"
        );
    }

    // mono_16k resampled from 0.5 DC offset should also be ~0.5 away from filter boundaries
    let mid_start = decoded.mono_16k.len() / 4;
    let mid_end = (decoded.mono_16k.len() * 3) / 4;
    for &s in &decoded.mono_16k[mid_start..mid_end] {
        assert!(
            (s - 0.5).abs() < 0.05,
            "resampled 16k sample expected ~0.5 but got {s}"
        );
    }
}

#[test]
fn test_decode_long_track_windowing_rule_last_90_seconds() {
    let sample_rate = 16000;
    let duration_sec = 100.0; // > 90s
    let total_frames = (sample_rate as f64 * duration_sec) as usize;

    // Create a track where first 10s is silent (0.0), and last 90s has constant 0.5 amplitude
    let mut samples = Vec::with_capacity(total_frames);
    let silent_frames = (10.0 * sample_rate as f64) as usize;
    samples.extend(vec![0.0f32; silent_frames]);
    samples.extend(vec![0.5f32; total_frames - silent_frames]);

    let wav = create_pcm_wav(1, sample_rate, &[samples]);
    let decoded = decode_and_resample_bytes(&wav, Some("wav")).expect("decode failed");

    // Exactly last 90s should be kept: 90 * 16000 = 1,440,000 samples
    let expected_16k_len = 90 * 16000;
    assert_eq!(
        decoded.mono_16k.len(),
        expected_16k_len,
        "expected exactly 90s of 16kHz audio"
    );

    // mono_48k should have 90 * 48000 = 4,320,000 samples
    let expected_48k_len = 90 * 48000;
    let diff_48k = (decoded.mono_48k.len() as isize - expected_48k_len as isize).abs();
    assert!(
        diff_48k < 500,
        "48k len {} vs expected {}",
        decoded.mono_48k.len(),
        expected_48k_len
    );

    // The entire window should be from the last 90s, so samples should be ~0.5 (not 0.0)
    let mid_sample = decoded.mono_16k[1000];
    assert!(
        (mid_sample - 0.5).abs() < 0.02,
        "sample from beginning of 90s window was {mid_sample}, expected ~0.5"
    );
}

#[test]
fn long_44100hz_track_keeps_exact_tail_at_both_output_rates() {
    let rate = 44_100usize;
    let mut samples = vec![0.0; rate];
    samples.extend(vec![0.25; rate * 89]);
    samples.extend(vec![0.75; rate]);
    let wav = create_pcm_wav(1, rate as u32, &[samples]);
    let decoded = decode_and_resample_bytes(&wav, Some("wav")).unwrap();
    assert_eq!(decoded.duration_seconds, 90.0);
    for (output, output_rate) in [
        (&decoded.mono_16k, 16_000usize),
        (&decoded.mono_48k, 48_000),
    ] {
        assert_eq!(output.len(), 90 * output_rate);
        assert!((output[output_rate / 2] - 0.25).abs() < 0.01);
        assert!((output[output.len() - output_rate / 2] - 0.75).abs() < 0.01);
    }
}

#[test]
fn test_decode_from_file_path() {
    let sample_rate = 16000;
    let duration_sec = 1.0;
    let num_samples = (sample_rate as f64 * duration_sec) as usize;
    let samples = vec![0.3f32; num_samples];

    let wav = create_pcm_wav(1, sample_rate, &[samples]);

    let mut tmp = NamedTempFile::new().expect("create tempfile");
    tmp.write_all(&wav).expect("write tempfile");

    let decoded = decode_and_resample(tmp.path()).expect("decode from path");
    assert_eq!(decoded.mono_16k.len(), 16000);
}

#[test]
fn test_truncated_wav_is_not_a_clean_end() {
    let mut wav = create_pcm_wav(1, 16_000, &[vec![0.5; 100_000]]);
    wav.truncate(120_000);
    assert!(decode_and_resample_bytes(&wav, Some("wav")).is_err());
}

#[test]
fn test_reject_unsupported_format() {
    let junk = b"THIS IS NOT A VALID AUDIO CONTAINER OR FORMAT AT ALL!";
    let res = decode_and_resample_bytes(junk, None);
    assert!(res.is_err());
    match res.unwrap_err() {
        AudioError::UnsupportedFormat(_) | AudioError::CorruptAudio(_) => {}
        other => panic!("expected UnsupportedFormat or CorruptAudio, got: {other:?}"),
    }
}

#[test]
fn test_reject_zero_length_file() {
    let res = decode_and_resample_bytes(&[], None);
    assert!(res.is_err());
}

#[test]
fn test_reject_zero_audio_samples() {
    // Valid WAV header with 0 data frames
    let wav = create_pcm_wav(1, 44100, &[vec![]]);
    let res = decode_and_resample_bytes(&wav, Some("wav"));
    assert!(res.is_err());
    match res.unwrap_err() {
        AudioError::EmptyAudio => {}
        other => panic!("expected EmptyAudio, got: {other:?}"),
    }
}
