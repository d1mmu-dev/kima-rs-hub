mod common;

use common::{Picard, Tags, make_audio, require_ffmpeg, tag_picard};
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

// ---- MusicBrainz ids, ISRC, totals, genres and lyrics (M3a Task 9) ----

const REC: &str = "a8b4c3d2-1e0f-4a9b-8c7d-6e5f4a3b2c1d";
const RTRACK: &str = "b1b2b3b4-c1c2-4d1d-9e1e-f1f2f3f4f5f6";
const REL: &str = "c0ffee00-1234-4abc-8def-0123456789ab";
const RG: &str = "d00dfeed-5678-4cde-9f01-23456789abcd";
const A1: &str = "e1e2e3e4-0000-4000-8000-000000000001";
const A2: &str = "e1e2e3e4-0000-4000-8000-000000000002";

fn uuid(s: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(s).unwrap()
}

/// A fresh fixture with the plain tags, then `p` written over it.
fn picard_file(dir: &std::path::Path, name: &str, p: &Picard<'_>) -> std::path::PathBuf {
    let path = make_audio(dir, name, Tags::full());
    tag_picard(&path, p);
    path
}

/// Replaces (or adds) raw Vorbis comments on a FLAC file.
fn set_vorbis(path: &std::path::Path, items: &[(&str, &str)]) {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile;
    use lofty::flac::FlacFile;
    use lofty::tag::TagExt;
    let mut f = std::fs::File::open(path).unwrap();
    let mut vc = FlacFile::read_from(&mut f, ParseOptions::new())
        .unwrap()
        .vorbis_comments()
        .cloned()
        .unwrap_or_default();
    drop(f);
    for (k, v) in items {
        vc.insert(k.to_string(), v.to_string());
    }
    vc.save_to_path(path, WriteOptions::default()).unwrap();
}

fn full_picard() -> Picard<'static> {
    Picard {
        recording: Some(REC),
        release_track: Some(RTRACK),
        release: Some(REL),
        release_group: Some(RG),
        artists: vec![A1, A2],
        album_artists: vec![A1],
        isrc: Some("USRC17607839"),
        genres: vec!["Rock", "Pop"],
        disc: Some((1, 2)),
        track: Some((3, 12)),
        uslt: Some("Plain words\nSecond line"),
        sylt: vec![(62500, "line"), (125_000, "next")],
        acoustid_id: None,
    }
}

fn check_picard(ext: &str) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = picard_file(dir.path(), &format!("p.{ext}"), &full_picard());
    let t = tags::read(&p).unwrap_or_else(|e| panic!("{ext}: {e}"));
    assert_eq!(t.recording_mbid, Some(uuid(REC)), "{ext}");
    assert_eq!(t.release_track_mbid, Some(uuid(RTRACK)), "{ext}");
    assert_eq!(t.release_mbid, Some(uuid(REL)), "{ext}");
    assert_eq!(t.release_group_mbid, Some(uuid(RG)), "{ext}");
    assert_eq!(t.artist_mbids, vec![uuid(A1), uuid(A2)], "{ext}");
    assert_eq!(t.album_artist_mbids, vec![uuid(A1)], "{ext}");
    assert_eq!(t.isrc.as_deref(), Some("USRC17607839"), "{ext}");
    assert_eq!(
        t.genres,
        vec!["Rock".to_string(), "Pop".to_string()],
        "{ext}"
    );
    assert_eq!((t.track, t.track_total), (Some(3), Some(12)), "{ext}");
    assert_eq!((t.disc, t.disc_total), (Some(1), Some(2)), "{ext}");
    let lyrics = t.lyrics.unwrap_or_else(|| panic!("{ext}: no lyrics"));
    assert_eq!(
        lyrics.plain.as_deref(),
        Some("Plain words\nSecond line"),
        "{ext}"
    );
    let synced = (ext == "mp3").then(|| "[01:02.50] line\n[02:05.00] next".to_string());
    assert_eq!(lyrics.synced, synced, "{ext}");
    // The plain tags written by ffmpeg survive.
    assert_eq!(t.title.as_deref(), Some("The Title"), "{ext}");
}

#[test]
fn reads_picard_ids_mp3() {
    check_picard("mp3");
}

#[test]
fn reads_picard_ids_flac() {
    check_picard("flac");
}

#[test]
fn reads_picard_ids_ogg() {
    check_picard("ogg");
}

#[test]
fn reads_picard_ids_opus() {
    check_picard("opus");
}

#[test]
fn reads_picard_ids_m4a() {
    check_picard("m4a");
}

#[test]
fn mbid_junk_dropped() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let joined = format!("{REC}; {A1}");
    let junk = [
        "{a8b4c3d2-1e0f-4a9b-8c7d-6e5f4a3b2c1d}",
        "urn:uuid:a8b4c3d2-1e0f-4a9b-8c7d-6e5f4a3b2c1d",
        "a8b4c3d21e0f4a9b8c7d6e5f4a3b2c1d",
        "00000000-0000-0000-0000-000000000000",
        joined.as_str(),
    ];
    for ext in ["mp3", "flac", "m4a"] {
        for (i, j) in junk.iter().enumerate() {
            let p = picard_file(
                dir.path(),
                &format!("j{i}.{ext}"),
                &Picard {
                    recording: Some(j),
                    release_group: Some(j),
                    ..Default::default()
                },
            );
            let t = tags::read(&p).unwrap();
            assert_eq!(t.recording_mbid, None, "{ext}: {j}");
            assert_eq!(t.release_group_mbid, None, "{ext}: {j}");
        }
        let upper = REC.to_ascii_uppercase();
        let p = picard_file(
            dir.path(),
            &format!("u.{ext}"),
            &Picard {
                recording: Some(&format!("  {upper} ")),
                ..Default::default()
            },
        );
        assert_eq!(
            tags::read(&p).unwrap().recording_mbid,
            Some(uuid(REC)),
            "{ext}"
        );
        // Multi-valued artist ids: split per value and on `;` and `/`, junk values dropped.
        let semi = format!("{A1}; {A2}");
        let slash = format!("{A1}/{A2}");
        for artists in [
            vec![semi.as_str()],
            vec![slash.as_str()],
            vec!["junk", A1, A2],
        ] {
            let p = picard_file(
                dir.path(),
                &format!("a.{ext}"),
                &Picard {
                    artists: artists.clone(),
                    album_artists: artists.clone(),
                    ..Default::default()
                },
            );
            let t = tags::read(&p).unwrap();
            assert_eq!(
                t.artist_mbids,
                vec![uuid(A1), uuid(A2)],
                "{ext}: {artists:?}"
            );
            assert_eq!(
                t.album_artist_mbids,
                vec![uuid(A1), uuid(A2)],
                "{ext}: {artists:?}"
            );
        }
    }
}

#[test]
fn acoustid_id_never_read_as_recording() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for ext in ["mp3", "flac", "ogg", "m4a"] {
        let p = picard_file(
            dir.path(),
            &format!("a.{ext}"),
            &Picard {
                acoustid_id: Some(REC),
                ..Default::default()
            },
        );
        assert_eq!(tags::read(&p).unwrap().recording_mbid, None, "{ext}");
    }
}

#[test]
fn isrc_first_value_normalised() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for ext in ["mp3", "flac", "m4a"] {
        for (i, (raw, want)) in [
            ("us-rc1-76-07839; GBAYE0601498", Some("USRC17607839")),
            ("PMEDIA-01", None),
        ]
        .into_iter()
        .enumerate()
        {
            let p = picard_file(
                dir.path(),
                &format!("i{i}.{ext}"),
                &Picard {
                    isrc: Some(raw),
                    ..Default::default()
                },
            );
            assert_eq!(
                tags::read(&p).unwrap().isrc.as_deref(),
                want,
                "{ext}: {raw}"
            );
        }
    }
    // Upstream recordingMatch.test.ts "ISRC form".
    assert_eq!(
        tags::normalize_isrc("US-RC1-76-07839").as_deref(),
        Some("USRC17607839")
    );
    assert_eq!(
        tags::normalize_isrc("gbaye0601498").as_deref(),
        Some("GBAYE0601498")
    );
    assert_eq!(tags::normalize_isrc("PMEDIA"), None);
    assert_eq!(tags::normalize_isrc("P.M.E.D.I.A"), None);
    assert_eq!(tags::normalize_isrc(""), None);
}

#[test]
fn genres_split_and_deduped() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for ext in ["mp3", "flac", "m4a"] {
        let p = picard_file(
            dir.path(),
            &format!("g.{ext}"),
            &Picard {
                genres: vec!["Rock; Indie", "rock", "Pop", "_hidden"],
                ..Default::default()
            },
        );
        assert_eq!(
            tags::read(&p).unwrap().genres,
            vec!["Rock".to_string(), "Indie".into(), "Pop".into()],
            "{ext}"
        );
    }
}

#[test]
fn totals_from_n_of_t() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = picard_file(
        dir.path(),
        "t.mp3",
        &Picard {
            track: Some((3, 12)),
            disc: Some((1, 2)),
            ..Default::default()
        },
    );
    let t = tags::read(&p).unwrap();
    assert_eq!((t.track, t.track_total), (Some(3), Some(12)));
    assert_eq!((t.disc, t.disc_total), (Some(1), Some(2)));
}

#[test]
fn vorbis_totals() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = make_audio(dir.path(), "a.flac", Tags::full());
    set_vorbis(&p, &[("TRACKTOTAL", "12"), ("DISCTOTAL", "2")]);
    let t = tags::read(&p).unwrap();
    assert_eq!((t.track_total, t.disc_total), (Some(12), Some(2)));

    let p = make_audio(dir.path(), "b.flac", Tags::full());
    set_vorbis(&p, &[("TOTALTRACKS", "9"), ("TOTALDISCS", "3")]);
    let t = tags::read(&p).unwrap();
    assert_eq!((t.track_total, t.disc_total), (Some(9), Some(3)));

    let p = make_audio(dir.path(), "c.flac", Tags::full());
    let t = tags::read(&p).unwrap();
    assert_eq!((t.track_total, t.disc_total), (None, None));
}

#[test]
fn sylt_becomes_lrc() {
    assert_eq!(
        tags::sylt_to_lrc(&[(62500, "line".to_string())]),
        "[01:02.50] line"
    );
    assert_eq!(
        tags::sylt_to_lrc(&[(0, "a".to_string()), (3_723_456, "b".to_string())]),
        "[00:00.00] a\n[62:03.45] b"
    );
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = picard_file(
        dir.path(),
        "s.mp3",
        &Picard {
            sylt: vec![(62500, "line")],
            ..Default::default()
        },
    );
    let l = tags::read(&p).unwrap().lyrics.unwrap();
    assert_eq!(l.synced.as_deref(), Some("[01:02.50] line"));
    assert_eq!(l.plain, None);
}

#[test]
fn uslt_with_timestamps_is_synced() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let lrc = "\n[00:01.00] one\n[00:02.00] two";
    let p = picard_file(
        dir.path(),
        "l.mp3",
        &Picard {
            uslt: Some(lrc),
            ..Default::default()
        },
    );
    let l = tags::read(&p).unwrap().lyrics.unwrap();
    assert_eq!(l.synced.as_deref(), Some("[00:01.00] one\n[00:02.00] two"));
    assert_eq!(l.plain, None);
}

#[test]
fn vorbis_plain_lyrics() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = picard_file(
        dir.path(),
        "v.flac",
        &Picard {
            uslt: Some("Just words"),
            ..Default::default()
        },
    );
    let l = tags::read(&p).unwrap().lyrics.unwrap();
    assert_eq!(l.plain.as_deref(), Some("Just words"));
    assert_eq!(l.synced, None);

    let p = make_audio(dir.path(), "u.flac", Tags::full());
    set_vorbis(&p, &[("UNSYNCEDLYRICS", "Other words")]);
    let l = tags::read(&p).unwrap().lyrics.unwrap();
    assert_eq!(l.plain.as_deref(), Some("Other words"));

    let p = make_audio(dir.path(), "n.flac", Tags::full());
    assert_eq!(tags::read(&p).unwrap().lyrics, None);
}

#[test]
fn m4a_lyr_read() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = picard_file(
        dir.path(),
        "m.m4a",
        &Picard {
            uslt: Some("[00:05.10] sung"),
            ..Default::default()
        },
    );
    let l = tags::read(&p).unwrap().lyrics.unwrap();
    assert_eq!(l.synced.as_deref(), Some("[00:05.10] sung"));
    assert_eq!(l.plain, None);
    let p = picard_file(
        dir.path(),
        "n.m4a",
        &Picard {
            uslt: Some("words"),
            ..Default::default()
        },
    );
    let l = tags::read(&p).unwrap().lyrics.unwrap();
    assert_eq!(l.plain.as_deref(), Some("words"));
}
