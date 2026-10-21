mod common;

use common::{Tags, make_audio, require_ffmpeg};
use kima_library::{fingerprint, tags};
use std::io::Write;

#[test]
fn reads_mp3_flac_ogg_m4a_tags() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for ext in ["mp3", "flac", "ogg", "opus", "m4a", "wav"] {
        let p = make_audio(dir.path(), &format!("a.{ext}"), Tags::full());
        let t = tags::read(&p).unwrap_or_else(|e| panic!("{ext}: {e}"));
        // WAV (RIFF INFO) cannot carry album artist, disc or compilation.
        if ext != "wav" {
            assert_eq!(t.album_artist.as_deref(), Some("The Album Artist"), "{ext}");
            assert_eq!(t.disc, Some(2), "{ext}");
            assert!(t.compilation, "{ext} compilation");
        }
        assert_eq!(t.artist.as_deref(), Some("The Track Artist"), "{ext}");
        assert_eq!(t.album.as_deref(), Some("The Album"), "{ext}");
        assert_eq!(t.title.as_deref(), Some("The Title"), "{ext}");
        assert_eq!(t.track, Some(5), "{ext}");
        assert_eq!(t.year, Some(1997), "{ext}");
        assert!(t.duration_secs > 1.5 && t.duration_secs < 2.5, "{ext}");
        assert!(t.codec.is_some(), "{ext}");
        assert!(tags::is_audio(&p));
    }
}

#[test]
fn untagged_file_gives_nones() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = make_audio(dir.path(), "x.flac", Tags::default());
    let t = tags::read(&p).unwrap();
    assert!(t.album_artist.is_none() && t.artist.is_none() && t.album.is_none());
    assert!(t.title.is_none() && t.year.is_none() && t.date.is_none());
    assert!(t.disc.is_none() && t.track.is_none() && !t.compilation);
}

#[test]
fn year_from_date_tag() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tg = Tags {
        date: Some("1997-05-21".into()),
        ..Default::default()
    };
    for ext in ["mp3", "flac", "m4a"] {
        let p = make_audio(dir.path(), &format!("y.{ext}"), tg.clone());
        assert_eq!(tags::read(&p).unwrap().year, Some(1997), "{ext}");
    }
}

#[test]
fn is_audio_by_extension() {
    assert!(tags::is_audio("a/b.FLAC".as_ref()));
    assert!(!tags::is_audio("a/b.jpg".as_ref()));
    assert!(!tags::is_audio("a/noext".as_ref()));
}

#[test]
fn fingerprint_stable_and_size_sensitive() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&a, &data).unwrap();
    let f1 = fingerprint::of(&a).unwrap();
    assert_eq!(f1, fingerprint::of(&a).unwrap());
    let b = dir.path().join("sub/copy.bin");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    std::fs::copy(&a, &b).unwrap();
    assert_eq!(f1, fingerprint::of(&b).unwrap());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&a)
        .unwrap()
        .write_all(&[0])
        .unwrap();
    assert_ne!(f1, fingerprint::of(&a).unwrap());
}

#[test]
fn fingerprint_small_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("s.bin");
    std::fs::write(&a, b"tiny").unwrap();
    let f = fingerprint::of(&a).unwrap();
    let e = dir.path().join("e.bin");
    std::fs::write(&e, b"").unwrap();
    assert_ne!(f, fingerprint::of(&e).unwrap());
    let b = dir.path().join("t.bin");
    std::fs::write(&b, b"tiny").unwrap();
    assert_eq!(f, fingerprint::of(&b).unwrap());
    let c = dir.path().join("c.bin");
    std::fs::write(&c, b"tinz").unwrap();
    assert_ne!(f, fingerprint::of(&c).unwrap());
}

fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 253) as u8).collect()
}

fn manual(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    const C: usize = 64 * 1024;
    let head = data.len().min(C);
    let tail_start = data.len().saturating_sub(C).max(head);
    let mut h = Sha256::new();
    h.update((data.len() as u64).to_le_bytes());
    h.update(&data[..head]);
    h.update(&data[tail_start..]);
    h.finalize().into()
}

#[test]
fn fingerprint_known_answer() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("k.bin");
    for n in [200 * 1024, 100 * 1024, 64 * 1024, 128 * 1024, 70_000] {
        let data = pattern(n);
        std::fs::write(&p, &data).unwrap();
        assert_eq!(fingerprint::of(&p).unwrap(), manual(&data), "size {n}");
    }
}

#[test]
fn fingerprint_last_byte_matters_middle_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m.bin");
    let mut data = pattern(300 * 1024);
    std::fs::write(&p, &data).unwrap();
    let base = fingerprint::of(&p).unwrap();

    data[100 * 1024] ^= 0xff; // middle: not hashed, by design
    std::fs::write(&p, &data).unwrap();
    assert_eq!(base, fingerprint::of(&p).unwrap());

    let last = data.len() - 1;
    data[last] ^= 0xff;
    std::fs::write(&p, &data).unwrap();
    assert_ne!(base, fingerprint::of(&p).unwrap());
}

#[test]
fn read_never_panics_on_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty.mp3");
    std::fs::write(&empty, b"").unwrap();
    let noise = dir.path().join("noise.flac");
    let bytes: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    std::fs::write(&noise, bytes).unwrap();
    let mut cases = vec![
        empty,
        noise,
        dir.path().to_path_buf(),
        dir.path().join("missing.mp3"),
    ];
    if require_ffmpeg() {
        let good = make_audio(dir.path(), "good.flac", Tags::full());
        let trunc = dir.path().join("trunc.flac");
        std::fs::write(&trunc, &std::fs::read(good).unwrap()[..1000]).unwrap();
        cases.push(trunc);
    }
    for p in cases {
        match tags::read(&p) {
            Err(_) => {}
            Ok(t) => assert!(
                t.artist.is_none() && t.album.is_none() && t.title.is_none(),
                "{p:?}: {t:?}"
            ),
        }
    }
}
