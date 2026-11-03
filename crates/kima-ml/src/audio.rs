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

/// Bounded maximum number of audio frames allowed to be decoded across a single track.
/// Prevents memory and CPU exhaustion on infinite/stream inputs (e.g. 4 hours at 192 kHz).
pub const MAX_TOTAL_DECODE_FRAMES: usize = 192_000 * 3600 * 4;

/// Minimum supported sample rate (8 kHz).
pub const MIN_SAMPLE_RATE: u32 = 8_000;

/// Maximum supported sample rate (192 kHz).
pub const MAX_SAMPLE_RATE: u32 = 192_000;

/// Maximum supported channels (up to 8 channels, e.g. 7.1 surround).
pub const MAX_CHANNELS: u32 = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    /// Mono 16 kHz audio samples (normalized f32 in [-1.0, 1.0], for MusiCNN and DSP).
    pub mono_16k: Vec<f32>,
    /// Mono 48 kHz audio samples (normalized f32 in [-1.0, 1.0], for CLAP).
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
    let mut total_decoded_frames: usize = 0;

    loop {
        let packet_opt = match format.next_packet() {
            Ok(Some(pkg)) => Some(pkg),
            Ok(None) => None,
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                None
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(SymphoniaError::Unsupported(msg)) => {
                return Err(AudioError::UnsupportedFormat(msg.to_string()));
            }
            Err(_) => None,
        };

        let Some(packet) = packet_opt else {
            break;
        };

        if packet.track_id != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                let num_channels = audio_buf.spec().channels().count();
                if num_channels == 0 || num_channels > MAX_CHANNELS as usize {
                    return Err(AudioError::UnsupportedChannels(num_channels as u32));
                }

                interleaved.resize(audio_buf.samples_interleaved(), f32::MID);
                audio_buf.copy_to_slice_interleaved(&mut interleaved);

                let frames = interleaved.len() / num_channels;
                total_decoded_frames += frames;

                if total_decoded_frames > MAX_TOTAL_DECODE_FRAMES {
                    return Err(AudioError::BufferOverflow);
                }

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
                tracing::warn!("audio decode error in packet: {msg}");
                continue;
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
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

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

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
