mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{Tags, make_audio, require_ffmpeg};
use kima_library::transcode::{
    Quality, TranscodeOptions, TranscodeOutput, TranscodeState, Transcoder,
};
use lofty::file::{FileType, TaggedFileExt};
use lofty::prelude::AudioFile;
use lofty::probe::Probe;
use tokio::sync::{Semaphore, watch};
use uuid::Uuid;

async fn wait_terminal(done: &mut watch::Receiver<TranscodeState>) -> TranscodeState {
    done.wait_for(|s| *s != TranscodeState::Running)
        .await
        .unwrap()
        .clone()
}

/// Waits for a transcode to finish and returns the final cached path.
async fn get_final(t: &Transcoder, id: Uuid, src: &Path, q: Quality) -> PathBuf {
    match t.get(id, src, q).await.unwrap() {
        TranscodeOutput::Cached(p) => p,
        TranscodeOutput::InProgress { mut done, .. } => {
            assert_eq!(wait_terminal(&mut done).await, TranscodeState::Done);
            match t.get(id, src, q).await.unwrap() {
                TranscodeOutput::Cached(p) => p,
                _ => panic!("expected cached after completion"),
            }
        }
    }
}

fn part_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect()
}

/// Writes an executable shell script that stands in for ffmpeg.
fn fake_ffmpeg(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn fake_source(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, b"fake").unwrap();
    p
}

#[test]
fn quality_parse_and_bitrate() {
    assert_eq!(Quality::parse("original"), Some(Quality::Original));
    assert_eq!(Quality::parse("high").unwrap().bitrate_kbps(), Some(320));
    assert_eq!(Quality::parse("medium").unwrap().bitrate_kbps(), Some(192));
    assert_eq!(Quality::parse("low").unwrap().bitrate_kbps(), Some(128));
    assert_eq!(Quality::Original.bitrate_kbps(), None);
    assert_eq!(Quality::parse("bogus"), None);
}

#[tokio::test]
async fn transcode_produces_mp3_at_bitrate() {
    if !require_ffmpeg() {
        return;
    }
    let src_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = make_audio(src_dir.path(), "a.flac", Tags::full());
    let t = Transcoder::new(cache.path(), 1 << 30);
    let out = get_final(&t, Uuid::now_v7(), &src, Quality::Medium).await;
    let tagged = Probe::open(&out).unwrap().read().unwrap();
    let kbps = tagged.properties().audio_bitrate().unwrap();
    assert!((180..=200).contains(&kbps), "bitrate {kbps}");
    assert_eq!(tagged.file_type(), FileType::Mpeg);
}

#[tokio::test]
async fn second_request_hits_cache() {
    if !require_ffmpeg() {
        return;
    }
    let src_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = make_audio(src_dir.path(), "a.flac", Tags::full());
    let t = Transcoder::new(cache.path(), 1 << 30);
    let id = Uuid::now_v7();
    let first = get_final(&t, id, &src, Quality::Low).await;
    match t.get(id, &src, Quality::Low).await.unwrap() {
        TranscodeOutput::Cached(p) => assert_eq!(p, first),
        _ => panic!("expected cache hit"),
    }
    assert_eq!(t.spawn_count(), 1);
}

#[tokio::test]
async fn original_is_passthrough() {
    let cache = tempfile::tempdir().unwrap();
    let t = Transcoder::new(cache.path(), 1 << 30);
    let src = Path::new("/nonexistent/x.flac");
    match t.get(Uuid::now_v7(), src, Quality::Original).await.unwrap() {
        TranscodeOutput::Cached(p) => assert_eq!(p, src),
        _ => panic!("original should not transcode"),
    }
    assert_eq!(t.spawn_count(), 0);
}

#[tokio::test]
async fn concurrent_requests_share_one_process() {
    if !require_ffmpeg() {
        return;
    }
    let src_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = make_audio(src_dir.path(), "a.flac", Tags::full());
    // The gate holds the transcode back, so every request below arrives while it is running.
    let gate = Arc::new(Semaphore::new(0));
    let t = Transcoder::with_options(
        cache.path(),
        1 << 30,
        TranscodeOptions {
            start_gate: Some(gate.clone()),
            ..Default::default()
        },
    );
    let id = Uuid::now_v7();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let t = t.clone();
        let src = src.clone();
        handles.push(tokio::spawn(async move {
            t.get(id, &src, Quality::High).await.unwrap()
        }));
    }
    let mut paths = Vec::new();
    let mut dones = Vec::new();
    for h in handles {
        let TranscodeOutput::InProgress { path, done } = h.await.unwrap() else {
            panic!("all requests must join the running transcode");
        };
        paths.push(path);
        dones.push(done);
    }
    assert_eq!(t.spawn_count(), 1);
    assert!(paths.iter().all(|p| p == &paths[0]));
    assert_eq!(part_files(cache.path()).len(), 1);
    gate.add_permits(1);
    for mut d in dones {
        assert_eq!(wait_terminal(&mut d).await, TranscodeState::Done);
    }
    assert_eq!(t.spawn_count(), 1);
    assert!(part_files(cache.path()).is_empty());
}

#[tokio::test]
async fn cache_evicts_lru_over_cap() {
    if !require_ffmpeg() {
        return;
    }
    let src_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = make_audio(src_dir.path(), "a.flac", Tags::full());
    // Learn the size of one transcode, then cap the cache at 2.5 of them.
    let probe_cache = tempfile::tempdir().unwrap();
    let probe = Transcoder::new(probe_cache.path(), 1 << 30);
    let one = std::fs::metadata(get_final(&probe, Uuid::now_v7(), &src, Quality::Low).await)
        .unwrap()
        .len();

    let t = Transcoder::new(cache.path(), one * 5 / 2);
    let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let pa = get_final(&t, a, &src, Quality::Low).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let pb = get_final(&t, b, &src, Quality::Low).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Touch `a` so `b` is the least recently used.
    assert!(matches!(
        t.get(a, &src, Quality::Low).await.unwrap(),
        TranscodeOutput::Cached(_)
    ));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let pc = get_final(&t, c, &src, Quality::Low).await;
    assert!(pa.exists(), "recently used entry kept");
    assert!(!pb.exists(), "LRU entry evicted");
    assert!(pc.exists(), "newest entry kept");
}

#[tokio::test]
async fn source_change_invalidates() {
    if !require_ffmpeg() {
        return;
    }
    let src_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = make_audio(src_dir.path(), "a.flac", Tags::full());
    let t = Transcoder::new(cache.path(), 1 << 30);
    let id = Uuid::now_v7();
    let first = get_final(&t, id, &src, Quality::Low).await;
    let f = std::fs::OpenOptions::new().write(true).open(&src).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    drop(f);
    let second = get_final(&t, id, &src, Quality::Low).await;
    assert_ne!(first, second);
    assert_eq!(t.spawn_count(), 2);
}

#[tokio::test]
async fn failed_ffmpeg_cleans_part_file() {
    if !require_ffmpeg() {
        return;
    }
    let src_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = src_dir.path().join("bad.flac");
    std::fs::write(&src, b"this is not audio").unwrap();
    let t = Transcoder::new(cache.path(), 1 << 30);
    let TranscodeOutput::InProgress { mut done, .. } =
        t.get(Uuid::now_v7(), &src, Quality::Low).await.unwrap()
    else {
        panic!("expected a fresh transcode");
    };
    let state = wait_terminal(&mut done).await;
    assert!(matches!(state, TranscodeState::Failed(_)), "{state:?}");
    assert!(part_files(cache.path()).is_empty());
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn hung_ffmpeg_times_out_and_cleans_up() {
    let work = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let hang = fake_ffmpeg(work.path(), "hang.sh", "exec sleep 1000");
    let src = fake_source(work.path(), "a.flac");
    let t = Transcoder::with_options(
        cache.path(),
        1 << 30,
        TranscodeOptions {
            ffmpeg: Some(hang),
            timeout: Some(Duration::from_millis(300)),
            ..Default::default()
        },
    );
    let id = Uuid::now_v7();
    let TranscodeOutput::InProgress { mut done, .. } = t.get(id, &src, Quality::Low).await.unwrap()
    else {
        panic!("expected a fresh transcode");
    };
    assert_eq!(
        wait_terminal(&mut done).await,
        TranscodeState::Failed("transcode timed out".into())
    );
    assert!(part_files(cache.path()).is_empty());
    // The map entry is gone: the next request starts a new run.
    let _ = t.get(id, &src, Quality::Low).await.unwrap();
    assert_eq!(t.spawn_count(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn part_file_is_gone_when_failure_is_observed() {
    if !require_ffmpeg() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = fake_source(work.path(), "bad.flac");
    let t = Transcoder::new(cache.path(), 1 << 30);
    for _ in 0..20 {
        let TranscodeOutput::InProgress { mut done, .. } =
            t.get(Uuid::now_v7(), &src, Quality::Low).await.unwrap()
        else {
            panic!("expected a fresh transcode");
        };
        assert!(matches!(
            wait_terminal(&mut done).await,
            TranscodeState::Failed(_)
        ));
        assert!(part_files(cache.path()).is_empty());
    }
}

#[tokio::test]
async fn failed_key_can_be_retried() {
    let work = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let marker = work.path().join("fail");
    std::fs::write(&marker, b"").unwrap();
    let script = format!(
        "if [ -e {m} ]; then echo boom >&2; exit 1; fi\nfor a; do last=$a; done\necho data > \"${{last#file:}}\"",
        m = marker.display()
    );
    let fake = fake_ffmpeg(work.path(), "ff.sh", &script);
    let src = fake_source(work.path(), "a.flac");
    let t = Transcoder::with_options(
        cache.path(),
        1 << 30,
        TranscodeOptions {
            ffmpeg: Some(fake),
            ..Default::default()
        },
    );
    let id = Uuid::now_v7();
    let TranscodeOutput::InProgress { mut done, .. } = t.get(id, &src, Quality::Low).await.unwrap()
    else {
        panic!("expected a fresh transcode");
    };
    assert!(matches!(
        wait_terminal(&mut done).await,
        TranscodeState::Failed(_)
    ));
    std::fs::remove_file(&marker).unwrap();
    let p = get_final(&t, id, &src, Quality::Low).await;
    assert!(p.exists());
    assert_eq!(t.spawn_count(), 2);
}

#[tokio::test]
async fn global_limit_serialises_different_keys() {
    let work = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let log = work.path().join("log");
    let script = format!(
        "echo start >> {l}\nsleep 0.4\necho end >> {l}\nfor a; do last=$a; done\necho data > \"${{last#file:}}\"",
        l = log.display()
    );
    let fake = fake_ffmpeg(work.path(), "ff.sh", &script);
    let src = fake_source(work.path(), "a.flac");
    let t = Transcoder::with_options(
        cache.path(),
        1 << 30,
        TranscodeOptions {
            ffmpeg: Some(fake),
            max_concurrent: Some(1),
            ..Default::default()
        },
    );
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let TranscodeOutput::InProgress { done: mut da, .. } =
        t.get(a, &src, Quality::Low).await.unwrap()
    else {
        panic!("expected a fresh transcode");
    };
    let TranscodeOutput::InProgress { done: mut db, .. } =
        t.get(b, &src, Quality::Low).await.unwrap()
    else {
        panic!("expected a fresh transcode");
    };
    assert_eq!(wait_terminal(&mut da).await, TranscodeState::Done);
    assert_eq!(wait_terminal(&mut db).await, TranscodeState::Done);
    let lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(lines, ["start", "end", "start", "end"]);
}

/// A reader that follows the growing file from the start, using the documented protocol, ends up
/// with the final file's bytes. The first 4096 bytes are excluded from the comparison: ffmpeg
/// rewrites the Xing/Info header frame at offset 0 when it finishes.
#[tokio::test]
async fn reader_following_growing_file_gets_final_bytes() {
    if !require_ffmpeg() {
        return;
    }
    use std::io::Read;
    let src_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = make_audio(src_dir.path(), "a.flac", Tags::full());
    let gate = Arc::new(Semaphore::new(0));
    let t = Transcoder::with_options(
        cache.path(),
        1 << 30,
        TranscodeOptions {
            start_gate: Some(gate.clone()),
            ..Default::default()
        },
    );
    let id = Uuid::now_v7();
    let TranscodeOutput::InProgress { path, done } = t.get(id, &src, Quality::Low).await.unwrap()
    else {
        panic!("expected in progress");
    };
    let mut f = std::fs::File::open(&path).expect("open before the transcode is released");
    gate.add_permits(1);
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n > 0 {
            got.extend_from_slice(&buf[..n]);
            continue;
        }
        let state = done.borrow().clone();
        match state {
            TranscodeState::Running => tokio::time::sleep(Duration::from_millis(5)).await,
            TranscodeState::Done => {
                // Drain once more: data may have landed between the read and the state check.
                let n = f.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            TranscodeState::Failed(e) => panic!("transcode failed: {e}"),
        }
    }
    let TranscodeOutput::Cached(final_path) = t.get(id, &src, Quality::Low).await.unwrap() else {
        panic!("expected cached");
    };
    let fin = std::fs::read(final_path).unwrap();
    assert_eq!(got.len(), fin.len());
    assert_eq!(got[4096..], fin[4096..]);
}
