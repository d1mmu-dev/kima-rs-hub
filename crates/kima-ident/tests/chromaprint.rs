//! Chromaprint fingerprints from ffmpeg (spec §9). Tests that need ffmpeg skip without it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use kima_ident::chromaprint::{DecodeError, compress, encode, ffmpeg_available, fingerprint_file};
use tokio::sync::RwLock;

/// Every test that runs ffmpeg holds this for reading; `timeout_kills_ffmpeg` holds it for
/// writing, so while it looks for leftover ffmpeg children no other test has any running.
static FFMPEG: RwLock<()> = RwLock::const_new(());

const TIMEOUT: Duration = Duration::from_secs(30);

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/chromaprint")
}

fn have_ffmpeg() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("ffmpeg not found; skipping");
    }
    ok
}

/// The fixture tone as a 44.1 kHz FLAC at `dest`.
fn make_flac(dest: &Path) {
    let mut out = std::ffi::OsString::from("file:");
    out.push(dest.as_os_str());
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(fixture_dir().join("tone.wav"))
        .args(["-ar", "44100", "-c:a", "flac"])
        .arg(out)
        .status()
        .unwrap();
    assert!(status.success());
}

/// `encode` of chromaprint's upstream compressor vectors (`test_fingerprint_compressor.cpp`).
/// Upstream builds them with algorithm 0; `compress` writes algorithm 1 (TEST2), so only the
/// first header byte differs.
#[test]
fn compress_matches_chromaprint_vectors() {
    let cases: [(&[u32], &[u8]); 6] = [
        (&[1], &[1]),
        (&[7], &[73, 0]),
        (&[1 << 6], &[7, 0]),
        (&[1 << 8], &[7, 2]),
        (&[1, 0], &[65, 0]),
        (&[1, 1], &[1, 0]),
    ];
    for (input, body) in cases {
        let mut expected = vec![1u8, 0, 0, input.len() as u8];
        expected.extend_from_slice(body);
        assert_eq!(compress(input), expected, "input {input:?}");
    }
    // base64 URL-safe without padding, as `chromaprint_encode_fingerprint(.., base64 = 1)`.
    assert_eq!(encode(&[1]), "AQAAAQE");
    assert_eq!(encode(&[]), "AQAAAA");
}

#[tokio::test]
async fn known_answer_matches_fpcalc() {
    if !have_ffmpeg() {
        return;
    }
    let _g = FFMPEG.read().await;
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture_dir().join("tone.fpcalc.json")).unwrap())
            .unwrap();
    let expected = json["fingerprint"].as_str().unwrap();
    let fp = fingerprint_file(&fixture_dir().join("tone.wav"), TIMEOUT)
        .await
        .unwrap();
    assert_eq!(fp.encoded, expected);
    assert_eq!(fp.encoded, encode(&fp.raw));
}

#[tokio::test]
async fn flac_44k_bit_error_under_5_percent() {
    if !have_ffmpeg() {
        return;
    }
    let _g = FFMPEG.read().await;
    let dir = tempfile::tempdir().unwrap();
    let flac = dir.path().join("tone.flac");
    make_flac(&flac);
    let wav = fingerprint_file(&fixture_dir().join("tone.wav"), TIMEOUT)
        .await
        .unwrap();
    let resampled = fingerprint_file(&flac, TIMEOUT).await.unwrap();
    let n = wav.raw.len().min(resampled.raw.len());
    assert!(n > 100, "fingerprints too short: {n}");
    let differing: u32 = wav.raw[..n]
        .iter()
        .zip(&resampled.raw[..n])
        .map(|(a, b)| (a ^ b).count_ones())
        .sum();
    let rate = f64::from(differing) / (n as f64 * 32.0);
    eprintln!("bit error rate {rate:.4} over {n} items");
    assert!(rate < 0.05, "bit error rate {rate}");
}

#[tokio::test]
async fn ffmpeg_failure_reports_code_and_stderr() {
    if !have_ffmpeg() {
        return;
    }
    let _g = FFMPEG.read().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.flac");
    std::fs::write(&path, "this is not audio\n".repeat(200)).unwrap();
    match fingerprint_file(&path, TIMEOUT).await {
        Err(DecodeError::Ffmpeg { code, stderr }) => {
            assert!(code.is_some());
            assert!(!stderr.is_empty());
            assert!(stderr.len() <= 2000, "stderr is {} bytes", stderr.len());
        }
        other => panic!("expected Ffmpeg, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_file_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let err = fingerprint_file(&dir.path().join("gone.flac"), TIMEOUT)
        .await
        .unwrap_err();
    match err {
        DecodeError::Missing(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected Missing, got {other:?}"),
    }
}

#[tokio::test]
async fn no_audio_stream_is_no_audio_or_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let _g = FFMPEG.read().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cover.flac");
    let mut out = std::ffi::OsString::from("file:");
    out.push(path.as_os_str());
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=red:s=16x16:d=1")
        .args(["-frames:v", "1", "-c:v", "png", "-f", "image2"])
        .arg(out)
        .status()
        .unwrap();
    assert!(status.success());
    let err = fingerprint_file(&path, TIMEOUT).await.unwrap_err();
    assert!(
        matches!(err, DecodeError::NoAudio | DecodeError::Ffmpeg { .. }),
        "got {err:?}"
    );
    assert!(!err.message().is_empty());
}

#[test]
fn no_audio_message_is_incomplete_decoder_output() {
    assert_eq!(DecodeError::NoAudio.message(), "incomplete decoder output");
    assert_eq!(DecodeError::Timeout.message(), "timed out");
    let e = DecodeError::Ffmpeg {
        code: Some(1),
        stderr: "Invalid data found when processing input".into(),
    };
    assert_eq!(e.message(), "Invalid data found when processing input");
}

/// Our ffmpeg children (any state, zombies included), from `/proc`.
#[cfg(target_os = "linux")]
fn ffmpeg_children() -> Vec<u32> {
    let me = std::process::id();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // pid (comm) state ppid ...
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let comm = stat
            .split_once('(')
            .map(|(_, r)| &r[..r.rfind(')').unwrap()]);
        let ppid = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse::<u32>().ok());
        if ppid == Some(me) && comm == Some("ffmpeg") {
            found.push(entry.file_name().to_string_lossy().parse().unwrap_or(0));
        }
    }
    found
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timeout_kills_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let _g = FFMPEG.write().await;
    let err = fingerprint_file(&fixture_dir().join("tone.wav"), Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(matches!(err, DecodeError::Timeout), "got {err:?}");
    assert_eq!(ffmpeg_children(), Vec::<u32>::new());
}

#[tokio::test]
async fn odd_file_names_fingerprint() {
    if !have_ffmpeg() {
        return;
    }
    let _g = FFMPEG.read().await;
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.flac");
    make_flac(&plain);
    let expected = fingerprint_file(&plain, TIMEOUT).await.unwrap().encoded;

    let mut names: Vec<std::ffi::OsString> = vec!["-01 intro.flac".into(), "a:b.flac".into()];
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        names.push(std::ffi::OsStr::from_bytes(b"\xFFodd.flac").to_owned());
    }
    for name in names {
        let path = dir.path().join(&name);
        std::fs::copy(&plain, &path).unwrap();
        let fp = fingerprint_file(&path, TIMEOUT).await;
        assert_eq!(
            fp.map(|f| f.encoded).ok().as_deref(),
            Some(expected.as_str()),
            "name {name:?}"
        );
    }
}

#[tokio::test]
async fn ffmpeg_available_probe() {
    let expected = have_ffmpeg();
    assert_eq!(ffmpeg_available().await, expected);
}
