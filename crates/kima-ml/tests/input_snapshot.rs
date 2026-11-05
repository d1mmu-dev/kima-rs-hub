#![cfg(target_os = "linux")]
mod snapshot_support;
use kima_ml::input_snapshot::{SnapshotLimits, SnapshotStore};
use snapshot_support::{private_dir, require_reflink};
use std::io::Read;

#[test]
fn reflink_retains_original_bytes_and_reopens_at_start() {
    let (lib, snap) = (private_dir(), private_dir());
    if !require_reflink(snap.path()) {
        return;
    }
    let store = SnapshotStore::open(snap.path(), SnapshotLimits::default()).unwrap();
    std::fs::write(lib.path().join("audio.wav"), b"original").unwrap();
    let snapshot = store.create(lib.path(), "audio.wav".as_ref()).unwrap();
    std::fs::write(lib.path().join("audio.wav"), b"changed!").unwrap();
    for _ in 0..2 {
        let mut bytes = Vec::new();
        snapshot.open().unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
    }
}

#[test]
fn decodes_real_audio_from_snapshot_after_source_replacement() {
    let (lib, snap) = (private_dir(), private_dir());
    if !require_reflink(snap.path()) {
        return;
    }
    let store = SnapshotStore::open(snap.path(), SnapshotLimits::default()).unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/fixtures/chromaprint/tone.wav");
    std::fs::copy(fixture, lib.path().join("tone.wav")).unwrap();
    let snapshot = store.create(lib.path(), "tone.wav".as_ref()).unwrap();
    std::fs::write(lib.path().join("tone.wav"), b"not audio").unwrap();
    let mut hint = symphonia::core::formats::probe::Hint::new();
    hint.with_extension("wav");
    let (_, windows, rate) =
        kima_ml::audio::decode_clap_auto(|| Ok(Box::new(snapshot.open()?)), &hint, None).unwrap();
    assert!(!windows.is_empty());
    assert!(rate > 0);
}
