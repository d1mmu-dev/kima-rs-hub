mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Tags, make_audio, require_ffmpeg};
use kima_library::transcode::{Quality, TranscodeOutput, TranscodeState, Transcoder};
use lofty::file::{FileType, TaggedFileExt};
use lofty::prelude::AudioFile;
use lofty::probe::Probe;
use uuid::Uuid;

/// Waits for a transcode to finish and returns the final cached path.
async fn get_final(t: &Transcoder, id: Uuid, src: &Path, q: Quality) -> PathBuf {
    match t.get(id, src, q).await.unwrap() {
        TranscodeOutput::Cached(p) => p,
        TranscodeOutput::InProgress { mut done, .. } => {
            let state = done
                .wait_for(|s| *s != TranscodeState::Running)
                .await
                .unwrap()
                .clone();
            assert_eq!(state, TranscodeState::Done);
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
    let t = Transcoder::new(cache.path(), 1 << 30);
    let id = Uuid::now_v7();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let t = t.clone();
        let src = src.clone();
        handles.push(tokio::spawn(async move {
            if let TranscodeOutput::InProgress { mut done, .. } =
                t.get(id, &src, Quality::High).await.unwrap()
            {
                done.wait_for(|s| *s != TranscodeState::Running)
                    .await
                    .unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
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
    let state = done
        .wait_for(|s| *s != TranscodeState::Running)
        .await
        .unwrap()
        .clone();
    assert!(matches!(state, TranscodeState::Failed(_)), "{state:?}");
    assert!(part_files(cache.path()).is_empty());
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn growing_file_is_readable_while_running() {
    if !require_ffmpeg() {
        return;
    }
    let src_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let src = make_audio(src_dir.path(), "a.flac", Tags::full());
    let t = Transcoder::new(cache.path(), 1 << 30);
    let TranscodeOutput::InProgress { path, mut done } =
        t.get(Uuid::now_v7(), &src, Quality::Low).await.unwrap()
    else {
        panic!("expected in progress");
    };
    let mut f = std::fs::File::open(&path).unwrap();
    done.wait_for(|s| *s != TranscodeState::Running)
        .await
        .unwrap();
    // The open handle still reads the finished data after the rename.
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut f, &mut buf).unwrap();
    assert!(!buf.is_empty());
}
