use std::collections::VecDeque;
use std::fs::File;
use std::io::Cursor;
use std::path::Path;

use rubato::{FftFixedInOut, Resampler};
use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

/// Maximum duration of the decoded audio window (90 seconds).
pub const MAX_WINDOW_SECONDS: f64 = 90.0;

/// Minimum supported sample rate (8 kHz).
pub const MIN_SAMPLE_RATE: u32 = 8_000;

/// Maximum supported sample rate (192 kHz).
pub const MAX_SAMPLE_RATE: u32 = 192_000;

/// Maximum supported channels (up to 8 channels, e.g. 7.1 surround).
pub const MAX_CHANNELS: u32 = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    /// Finite mono 16 kHz samples for MusiCNN and DSP. Not clipped: floating-point
    /// input and resampling may exceed the nominal PCM range [-1, 1].
    pub mono_16k: Vec<f32>,
    /// Finite mono 48 kHz samples for CLAP, with the same unclipped policy.
    pub mono_48k: Vec<f32>,
    /// Duration in seconds of the retained audio slice.
    pub duration_seconds: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unsupported audio format or container: {0}")]
    UnsupportedFormat(String),

    #[error("corrupt or invalid audio: {0}")]
    CorruptAudio(String),

    #[error("no audio track found in media container")]
    NoAudioTrack,

    #[error("audio stream contains no samples")]
    EmptyAudio,

    #[error("unsupported sample rate: {0} Hz")]
    UnsupportedSampleRate(u32),

    #[error("unsupported channel count: {0}")]
    UnsupportedChannels(u32),

    #[error("resampling error: {0}")]
    Resample(String),

    #[error("audio exceeds allocation bounds")]
    BufferOverflow,
}

/// Decodes an audio file from `path`, windowing to the last 90 seconds (or full track if <= 90s),
/// downmixing to mono by averaging channels, and resampling to mono 16 kHz and mono 48 kHz.
pub fn decode_and_resample(path: &Path) -> Result<DecodedAudio, AudioError> {
    let file = File::open(path)?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    decode_and_resample_media_source(Box::new(file), &hint)
}

/// Decodes an in-memory audio byte slice, windowing to the last 90 seconds (or full track if <= 90s),
/// downmixing to mono by averaging channels, and resampling to mono 16 kHz and mono 48 kHz.
pub fn decode_and_resample_bytes(
    bytes: &[u8],
    format_hint: Option<&str>,
) -> Result<DecodedAudio, AudioError> {
    if bytes.is_empty() {
        return Err(AudioError::CorruptAudio("empty audio input".to_string()));
    }
    let mut hint = Hint::new();
    if let Some(ext) = format_hint {
        hint.with_extension(ext);
    }
    let cursor = Cursor::new(bytes.to_vec());
    decode_and_resample_media_source(Box::new(cursor), &hint)
}

fn decode_and_resample_media_source(
    source: Box<dyn MediaSource>,
    hint: &Hint,
) -> Result<DecodedAudio, AudioError> {
    let mss = MediaSourceStream::new(source, Default::default());

    let mut format = symphonia::default::get_probe()
        .probe(
            hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| match e {
            SymphoniaError::Unsupported(msg) => AudioError::UnsupportedFormat(msg.to_string()),
            SymphoniaError::IoError(e) => AudioError::Io(e),
            other => AudioError::CorruptAudio(other.to_string()),
        })?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or(AudioError::NoAudioTrack)?;

    let audio_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or(AudioError::NoAudioTrack)?;

    let sample_rate = audio_params.sample_rate.ok_or(AudioError::NoAudioTrack)?;

    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Err(AudioError::UnsupportedSampleRate(sample_rate));
    }

    let channels = audio_params
        .channels
        .as_ref()
        .map(|c| c.count() as u32)
        .unwrap_or(1);

    if channels == 0 || channels > MAX_CHANNELS {
        return Err(AudioError::UnsupportedChannels(channels));
    }

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(audio_params, &AudioDecoderOptions::default())
        .map_err(|e| AudioError::CorruptAudio(e.to_string()))?;

    let window_capacity = (MAX_WINDOW_SECONDS * sample_rate as f64).round() as usize;
    let mut ring_buffer = VecDeque::with_capacity(window_capacity);

    let track_id = track.id;
    let mut interleaved: Vec<f32> = Vec::new();

    loop {
        #[cfg(test)]
        let next_packet = if INJECT_DEMUX_RESET.with(|count| {
            let n = count.get();
            if n > 0 {
                count.set(n - 1);
            }
            n == 1
        }) {
            Err(SymphoniaError::ResetRequired)
        } else {
            format.next_packet()
        };
        #[cfg(not(test))]
        let next_packet = format.next_packet();
        let packet_opt = match next_packet {
            Ok(Some(pkg)) => Some(pkg),
            Ok(None) => None,
            Err(SymphoniaError::IoError(err)) => return Err(AudioError::Io(err)),
            Err(SymphoniaError::ResetRequired) => {
                // The demuxer may have changed tracks and codec parameters;
                // resetting only the old decoder would use stale settings.
                return Err(AudioError::CorruptAudio("demuxer reset required".into()));
            }
            Err(SymphoniaError::Unsupported(msg)) => {
                return Err(AudioError::UnsupportedFormat(msg.to_string()));
            }
            Err(err) => return Err(AudioError::CorruptAudio(err.to_string())),
        };

        let Some(packet) = packet_opt else {
            break;
        };

        if packet.track_id != track_id {
            continue;
        }

        #[cfg(test)]
        let decoded = if INJECT_DECODE_ERROR.with(|count| {
            let n = count.get();
            if n > 0 {
                count.set(n - 1);
            }
            n == 1
        }) {
            if INJECT_RESET.with(|flag| flag.get()) {
                Err(SymphoniaError::ResetRequired)
            } else {
                Err(SymphoniaError::DecodeError("injected packet failure"))
            }
        } else {
            decoder.decode(&packet)
        };
        #[cfg(not(test))]
        let decoded = decoder.decode(&packet);
        match decoded {
            Ok(audio_buf) => {
                let num_channels = audio_buf.spec().channels().count();
                if num_channels == 0 || num_channels > MAX_CHANNELS as usize {
                    return Err(AudioError::UnsupportedChannels(num_channels as u32));
                }

                interleaved.resize(audio_buf.samples_interleaved(), f32::MID);
                audio_buf.copy_to_slice_interleaved(&mut interleaved);

                let frames = interleaved.len() / num_channels;

                let inv_ch = 1.0 / (num_channels as f32);
                for f in 0..frames {
                    let mut mono = 0.0f32;
                    let offset = f * num_channels;
                    for c in 0..num_channels {
                        mono += interleaved[offset + c];
                    }
                    mono *= inv_ch;

                    if ring_buffer.len() == window_capacity {
                        ring_buffer.pop_front();
                    }
                    ring_buffer.push_back(mono);
                }
            }
            Err(SymphoniaError::DecodeError(msg)) => {
                return Err(AudioError::CorruptAudio(msg.to_string()));
            }
            Err(e) => {
                return Err(AudioError::CorruptAudio(e.to_string()));
            }
        }
    }

    if ring_buffer.is_empty() {
        return Err(AudioError::EmptyAudio);
    }

    let mono_raw: Vec<f32> = ring_buffer.into();
    let duration_seconds = mono_raw.len() as f64 / sample_rate as f64;

    let mono_16k = resample_mono(&mono_raw, sample_rate, 16_000)?;
    let mono_48k = resample_mono(&mono_raw, sample_rate, 48_000)?;

    Ok(DecodedAudio {
        mono_16k,
        mono_48k,
        duration_seconds,
    })
}

/// Resamples a single-channel (mono) slice of `f32` audio from `in_rate` to `out_rate`.
pub fn resample_mono(samples: &[f32], in_rate: u32, out_rate: u32) -> Result<Vec<f32>, AudioError> {
    // Keep FFT intermediate arithmetic far below f32 overflow; ordinary normalized
    // PCM and filter overshoots are unaffected, and nothing is silently clipped.
    if samples.iter().any(|s| !s.is_finite() || s.abs() > 1.0e12) {
        return Err(AudioError::CorruptAudio(
            "non-finite or excessive audio sample".into(),
        ));
    }
    if in_rate == out_rate {
        return Ok(samples.to_vec());
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let chunk_size = 1024;
    let mut resampler =
        FftFixedInOut::<f32>::new(in_rate as usize, out_rate as usize, chunk_size, 1)
            .map_err(|e| AudioError::Resample(e.to_string()))?;

    let expected_len =
        ((samples.len() as f64) * (out_rate as f64) / (in_rate as f64)).round() as usize;

    let delay = resampler.output_delay();
    let mut output = Vec::with_capacity(expected_len + delay + chunk_size * 2);

    let mut pos = 0;
    while pos < samples.len() {
        let needed = resampler.input_frames_next();
        if pos + needed <= samples.len() {
            let chunk = &samples[pos..pos + needed];
            let res = resampler
                .process(&[chunk], None)
                .map_err(|e| AudioError::Resample(e.to_string()))?;
            output.extend_from_slice(&res[0]);
            pos += needed;
        } else {
            let remaining = &samples[pos..];
            let res = resampler
                .process_partial(Some(&[remaining]), None)
                .map_err(|e| AudioError::Resample(e.to_string()))?;
            output.extend_from_slice(&res[0]);
            pos = samples.len();
        }
    }

    let flush = resampler
        .process_partial::<Vec<f32>>(None, None)
        .map_err(|e| AudioError::Resample(e.to_string()))?;
    output.extend_from_slice(&flush[0]);

    // Trim initial filter delay to align time with input
    let start = delay.min(output.len());
    let end = (start + expected_len).min(output.len());
    let mut result = output[start..end].to_vec();

    // If slightly short due to block alignment/delay edge, pad to expected_len
    if result.len() < expected_len {
        result.resize(expected_len, 0.0);
    }

    if result.iter().any(|s| !s.is_finite()) {
        return Err(AudioError::CorruptAudio(
            "non-finite resampler output".into(),
        ));
    }
    Ok(result)
}

#[cfg(test)]
thread_local! {
    static INJECT_DECODE_ERROR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static INJECT_RESET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static INJECT_DEMUX_RESET: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demux_reset_after_valid_packet_fails_closed() {
        let frames = 100_000u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(frames * 2).to_le_bytes());
        wav.resize(44 + frames as usize * 2, 1);
        INJECT_DEMUX_RESET.with(|count| count.set(2));
        let result = decode_and_resample_bytes(&wav, Some("wav"));
        let remaining = INJECT_DEMUX_RESET.with(|count| count.replace(0));
        assert_eq!(
            remaining, 0,
            "reset must be injected after the first packet"
        );
        assert!(
            matches!(result, Err(AudioError::CorruptAudio(_))),
            "demux reset must reject all audio"
        );
    }

    #[test]
    fn decode_has_no_total_duration_cap() {
        // Source-level guard: exercising the old >2.7 billion-frame boundary
        // by decoding real audio would be prohibitively slow and resource-heavy.
        let source = include_str!("audio.rs");
        let production = source.split("#[cfg(test)]\nthread_local!").next().unwrap();
        assert!(!production.contains(concat!("MAX_TOTAL", "_DECODE_FRAMES")));
    }

    #[test]
    fn decoder_error_after_valid_packet_fails_closed() {
        let frames = 100_000u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(frames * 2).to_le_bytes());
        wav.resize(44 + frames as usize * 2, 1);
        for reset in [false, true] {
            INJECT_RESET.with(|flag| flag.set(reset));
            INJECT_DECODE_ERROR.with(|count| count.set(2));
            let result = decode_and_resample_bytes(&wav, Some("wav"));
            INJECT_DECODE_ERROR.with(|count| count.set(0));
            INJECT_RESET.with(|flag| flag.set(false));
            assert!(
                matches!(result, Err(AudioError::CorruptAudio(_))),
                "decoder error must reject the entire input (reset={reset})"
            );
        }
    }

    #[test]
    fn read_failure_after_audio_packets_is_not_eof() {
        use std::io::{Read, Seek, SeekFrom};

        struct FailingSource(Cursor<Vec<u8>>);
        impl Read for FailingSource {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let remaining = 1_048_576u64.saturating_sub(self.0.position()) as usize;
                if remaining == 0 {
                    return Err(std::io::Error::other("injected read failure"));
                }
                let len = buf.len().min(remaining);
                self.0.read(&mut buf[..len])
            }
        }
        impl Seek for FailingSource {
            fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
                self.0.seek(pos)
            }
        }
        impl MediaSource for FailingSource {
            fn is_seekable(&self) -> bool {
                false
            }
            fn byte_len(&self) -> Option<u64> {
                Some(self.0.get_ref().len() as u64)
            }
        }
        let frames = 1_000_000u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(frames * 2).to_le_bytes());
        wav.resize(44 + frames as usize * 2, 1);
        let mut hint = Hint::new();
        hint.with_extension("wav");
        let result =
            decode_and_resample_media_source(Box::new(FailingSource(Cursor::new(wav))), &hint);
        assert!(
            matches!(result, Err(AudioError::Io(ref e)) if e.kind() == std::io::ErrorKind::Other)
        );
    }

    #[test]
    fn resampler_rejects_nonfinite_input_even_without_rate_change() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(matches!(
                resample_mono(&[0.1, value], 16_000, 16_000),
                Err(AudioError::CorruptAudio(_))
            ));
            assert!(matches!(
                resample_mono(&[0.1, value], 16_000, 48_000),
                Err(AudioError::CorruptAudio(_))
            ));
        }
    }

    #[test]
    fn resampler_rejects_overflow_from_finite_input() {
        assert!(matches!(
            resample_mono(&vec![f32::MAX; 4096], 16_000, 48_000),
            Err(AudioError::CorruptAudio(_))
        ));
    }

    #[test]
    fn resampling_preserves_finite_overshoot_without_clipping() {
        let mut input = vec![0.0; 4096];
        input[1024..3072].fill(1.0);
        let output = resample_mono(&input, 16_000, 48_000).unwrap();
        assert!(output.iter().all(|x| x.is_finite()));
        assert!(output.iter().any(|x| *x > 1.0));
        assert_eq!(resample_mono(&[1.25], 16_000, 16_000).unwrap(), vec![1.25]);
    }

    #[test]
    fn test_resample_rates_supported() {
        let sample_rates = [8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000];
        let dummy = vec![0.5f32; 4096];

        for &rate in &sample_rates {
            let to_16k = resample_mono(&dummy, rate, 16000).expect("resample to 16k");
            assert!(!to_16k.is_empty());
            let to_48k = resample_mono(&dummy, rate, 48000).expect("resample to 48k");
            assert!(!to_48k.is_empty());
        }
    }
}
