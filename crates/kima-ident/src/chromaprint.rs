//! Chromaprint fingerprints computed in-process (spec §9, D-106).
//!
//! ffmpeg decodes the first 120 s of the file to 11025 Hz mono s16le on stdout (chromaprint's
//! own rate, so `rusty-chromaprint` never resamples), and the samples go through
//! `rusty_chromaprint` with the TEST2 preset, the algorithm `fpcalc` and AcoustID use.
//!
//! ffmpeg runs on untrusted files: the input is always `file:<absolute path>` (so a name
//! starting with `-` or holding `:` is neither an option nor a protocol) with
//! `-protocol_whitelist file` (a playlist cannot reach the network), never through a shell;
//! stdout is bounded, stderr keeps its first 2000 bytes, a timeout kills and reaps the child,
//! and stdout is only fingerprinted after a clean exit.

use std::ffi::OsString;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Chromaprint's internal sample rate; ffmpeg resamples to it.
const SAMPLE_RATE: u32 = 11025;
/// Seconds decoded, as `fpcalc -length 120`.
const MAX_SECONDS: u32 = 120;
/// Bytes of stderr kept for the error message.
const STDERR_CAP: usize = 2000;
/// Bytes fed to the fingerprinter at a time.
const CHUNK: usize = 64 * 1024;
/// The most PCM kept: 120 s of s16 mono plus a chunk of slack. ffmpeg's `-t 120` stops well
/// before; anything past this is drained and dropped.
const PCM_CAP: usize = (MAX_SECONDS * SAMPLE_RATE * 2) as usize + CHUNK;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// The raw sub-fingerprints.
    pub raw: Vec<u32>,
    /// Compressed and base64 encoded, as AcoustID and `fpcalc` give it.
    pub encoded: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The file is not there (or cannot be read).
    #[error("file missing: {0}")]
    Missing(std::io::Error),
    /// ffmpeg did not finish in time; it was killed.
    #[error("timed out")]
    Timeout,
    /// ffmpeg exited cleanly but gave too little audio for a fingerprint.
    #[error("incomplete decoder output")]
    NoAudio,
    /// ffmpeg could not start (`code` None) or exited non-zero.
    #[error("ffmpeg exited with {code:?}: {stderr}")]
    Ffmpeg { code: Option<i32>, stderr: String },
}

impl DecodeError {
    /// The text the ledger classifies (spec §10.4).
    pub fn message(&self) -> String {
        match self {
            Self::Missing(e) => e.to_string(),
            Self::Timeout => "timed out".into(),
            Self::NoAudio => "incomplete decoder output".into(),
            Self::Ffmpeg { code, stderr } => {
                let text = stderr.trim();
                if !text.is_empty() {
                    text.to_string()
                } else {
                    match code {
                        Some(c) => format!("ffmpeg exited with code {c}"),
                        None => "ffmpeg was killed by a signal".into(),
                    }
                }
            }
        }
    }
}

/// The TEST2 preset (algorithm id 1), chromaprint's default.
fn config() -> Configuration {
    Configuration::preset_test2()
}

/// Chromaprint's `FingerprintCompressor` format for `raw`, algorithm 1 (TEST2).
pub fn compress(raw: &[u32]) -> Vec<u8> {
    let config = config();
    FingerprintCompressor::from(&config).compress(raw)
}

/// `compress`, base64 URL-safe without padding: the string AcoustID takes.
pub fn encode(raw: &[u32]) -> String {
    URL_SAFE_NO_PAD.encode(compress(raw))
}

fn file_arg(path: &Path) -> OsString {
    let mut s = OsString::from("file:");
    s.push(path.as_os_str());
    s
}

/// Reads all of `r`, keeping at most `cap` bytes.
async fn read_capped(mut r: impl AsyncRead + Unpin, cap: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    kept
}

/// `bytes` as text of at most `STDERR_CAP` bytes, cut on a character boundary.
fn stderr_text(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if text.len() > STDERR_CAP {
        let mut end = STDERR_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Decodes `path` with ffmpeg and fingerprints the first 120 s. ffmpeg gets `timeout`; on
/// expiry it is killed and reaped before this returns.
pub async fn fingerprint_file(path: &Path, timeout: Duration) -> Result<Fingerprint, DecodeError> {
    let path = std::path::absolute(path).map_err(DecodeError::Missing)?;
    // ENOENT is FILE_MISSING (spec §9.1), told apart from a file ffmpeg cannot decode.
    tokio::fs::metadata(&path)
        .await
        .map_err(DecodeError::Missing)?;

    let mut child = Command::new("ffmpeg")
        // Only local files, even for inputs a renamed playlist would open (defence in depth).
        .args([
            "-nostdin",
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(file_arg(&path))
        .args(["-map", "0:a:0", "-t"])
        .arg(MAX_SECONDS.to_string())
        .args(["-ac", "1", "-ar"])
        .arg(SAMPLE_RATE.to_string())
        .args(["-f", "s16le", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| DecodeError::Ffmpeg {
            code: None,
            stderr: format!("could not start ffmpeg: {e}"),
        })?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(DecodeError::Ffmpeg {
            code: None,
            stderr: "ffmpeg pipes unavailable".into(),
        });
    };

    let run = async {
        let (pcm, err, status) = tokio::join!(
            read_capped(stdout, PCM_CAP),
            read_capped(stderr, STDERR_CAP),
            child.wait(),
        );
        (pcm, err, status)
    };
    let (pcm, err, status): (Vec<u8>, Vec<u8>, std::io::Result<ExitStatus>) =
        match tokio::time::timeout(timeout, run).await {
            Ok(done) => done,
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(DecodeError::Timeout);
            }
        };

    let status = status.map_err(|e| DecodeError::Ffmpeg {
        code: None,
        stderr: format!("waiting for ffmpeg failed: {e}"),
    })?;
    if !status.success() {
        // stdout is never looked at after a failed exit.
        return Err(DecodeError::Ffmpeg {
            code: status.code(),
            stderr: stderr_text(&err),
        });
    }
    if pcm.len() < 2 {
        return Err(DecodeError::NoAudio);
    }

    let raw = tokio::task::spawn_blocking(move || fingerprint_pcm(&pcm))
        .await
        .map_err(|e| DecodeError::Ffmpeg {
            code: None,
            stderr: format!("fingerprinting failed: {e}"),
        })?;
    if raw.is_empty() {
        return Err(DecodeError::NoAudio);
    }
    let encoded = encode(&raw);
    Ok(Fingerprint { raw, encoded })
}

/// Raw TEST2 fingerprint of s16le mono PCM at 11025 Hz, fed in 64 KiB chunks. A trailing odd
/// byte is dropped.
fn fingerprint_pcm(pcm: &[u8]) -> Vec<u32> {
    let mut printer = Fingerprinter::new(&config());
    if printer.start(SAMPLE_RATE, 1).is_err() {
        return Vec::new();
    }
    let mut samples = Vec::with_capacity(CHUNK / 2);
    for chunk in pcm.chunks(CHUNK) {
        samples.clear();
        samples.extend(
            chunk
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]])),
        );
        printer.consume(&samples);
    }
    printer.finish();
    printer.fingerprint().to_vec()
}

/// Whether `ffmpeg -version` runs and succeeds within 5 s.
pub async fn ffmpeg_available() -> bool {
    let child = Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            false
        }
    }
}
