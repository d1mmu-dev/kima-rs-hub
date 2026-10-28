mod common;

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use common::{Picard, Tags, embed_cover, make_audio, png_bytes, require_ffmpeg, tag_picard};
use kima_library::organize::apply::IGNORE_GRACE;
use kima_library::tagwrite::{
    Container, Id3Version, NativeTags, NumberPair, TagValues, TagWriteError, WriteHooks, audio_md5,
    container_of, id3_version, native_for_file, read_native, refuse_symlink, safe_write,
};
use kima_library::watch::IgnoreSet;
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::id3::v2::{Frame, FrameId, Id3v2Tag};
use lofty::mpeg::MpegFile;
use lofty::tag::TagExt;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const REC: &str = "11111111-1111-4111-8111-111111111111";
const ARTIST_A: &str = "22222222-2222-4222-8222-222222222222";
const ARTIST_B: &str = "33333333-3333-4333-8333-333333333333";

fn id3(path: &Path) -> Option<Id3v2Tag> {
    let mut f = std::fs::File::open(path).unwrap();
    MpegFile::read_from(&mut f, ParseOptions::new().read_properties(false))
        .unwrap()
        .id3v2()
        .cloned()
}

fn sha(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}

fn user_text(tag: &Id3v2Tag, desc: &str) -> Option<String> {
    tag.get_user_text(desc).map(str::to_string)
}

fn text_frame(tag: &Id3v2Tag, id: &'static str) -> Option<String> {
    tag.get_text(&FrameId::Valid(Cow::Borrowed(id)))
        .map(str::to_string)
}

fn cover(tag: &Id3v2Tag) -> Option<Vec<u8>> {
    tag.iter().find_map(|f| match f {
        Frame::Picture(p) => Some(p.picture.data().to_vec()),
        _ => None,
    })
}

/// A tone with ffmpeg's tags, Picard's MBIDs, a cover, ReplayGain and a custom TXXX.
fn fixture(dir: &Path, rel: &str) -> PathBuf {
    let p = make_audio(dir, rel, Tags::full());
    embed_cover(&p, png_bytes(16, 16, [200, 10, 10]));
    tag_picard(
        &p,
        &Picard {
            recording: Some("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
            release: Some("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
            artists: vec!["cccccccc-cccc-4ccc-8ccc-cccccccccccc"],
            genres: vec!["Jazz"],
            track: Some((5, 9)),
            ..Default::default()
        },
    );
    let mut tag = id3(&p).unwrap();
    tag.insert_user_text("REPLAYGAIN_TRACK_GAIN".into(), "-6.50 dB".into());
    tag.insert_user_text("MY TAG".into(), "keep me".into());
    tag.save_to_path(&p, WriteOptions::default()).unwrap();
    p
}

/// Saves the file's ID3v2 tag again as v2.3.
fn make_v23(path: &Path) {
    let tag = id3(path).unwrap();
    tag.save_to_path(path, WriteOptions::default().use_id3v23(true))
        .unwrap();
    assert_eq!(id3_version(path).unwrap(), Some(Id3Version::V3));
}

fn every_field() -> TagValues {
    TagValues {
        title: Some("New Title".into()),
        artist: Some("New Artist feat. Björk".into()),
        artist_sort: Some("Artist, New".into()),
        album: Some("New Album".into()),
        album_artist: Some("New Album Artist".into()),
        album_artist_sort: Some("Album Artist, New".into()),
        track: Some(NumberPair {
            number: 3,
            total: Some(12),
        }),
        disc: Some(NumberPair {
            number: 1,
            total: Some(2),
        }),
        date: Some("2001-03-04".into()),
        original_date: Some("1999-05-06".into()),
        genres: Some(vec!["Rock".into(), "Pop".into()]),
        compilation: Some(true),
        recording_mbid: Some(Uuid::parse_str(REC).unwrap()),
        release_track_mbid: Some(Uuid::from_u128(0x4444_4444_4444_4444_8444_4444_4444_4444)),
        release_mbid: Some(Uuid::from_u128(0x5555_5555_5555_4555_8555_5555_5555_5555)),
        release_group_mbid: Some(Uuid::from_u128(0x6666_6666_6666_4666_8666_6666_6666_6666)),
        artist_mbids: Some(vec![
            Uuid::parse_str(ARTIST_A).unwrap(),
            Uuid::parse_str(ARTIST_B).unwrap(),
        ]),
        album_artist_mbids: Some(vec![Uuid::parse_str(ARTIST_A).unwrap()]),
    }
}

fn write(
    path: &Path,
    id: i64,
    new: &NativeTags,
) -> Result<kima_library::tagwrite::Written, TagWriteError> {
    let ver = id3_version(path).unwrap();
    safe_write(
        path,
        id,
        Container::Mp3,
        ver,
        new,
        &IgnoreSet::new(),
        &WriteHooks::default(),
    )
}

fn keys(m: &NativeTags) -> Vec<String> {
    m.keys().cloned().collect()
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".kima-tag-"))
        .collect()
}

#[test]
fn mp3_v24_round_trip() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    assert_eq!(container_of(&p).unwrap(), Container::Mp3);
    assert_eq!(id3_version(&p).unwrap(), Some(Id3Version::V4));
    let before = id3(&p).unwrap();
    let md5 = audio_md5(&p).unwrap();

    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let expected: Vec<&str> = vec![
        "TALB",
        "TCMP",
        "TCON",
        "TDOR",
        "TDRC",
        "TIT2",
        "TPE1",
        "TPE2",
        "TPOS",
        "TRCK",
        "TSO2",
        "TSOP",
        "TXXX:MusicBrainz Album Artist Id",
        "TXXX:MusicBrainz Album Id",
        "TXXX:MusicBrainz Artist Id",
        "TXXX:MusicBrainz Release Group Id",
        "TXXX:MusicBrainz Release Track Id",
        "UFID:http://musicbrainz.org",
    ];
    assert_eq!(new.keys().map(String::as_str).collect::<Vec<_>>(), expected);
    assert_eq!(new["TRCK"], vec!["3/12"]);
    assert_eq!(new["TXXX:MusicBrainz Artist Id"], vec![ARTIST_A, ARTIST_B]);

    let w = write(&p, 1, &new).unwrap();
    assert_eq!(read_native(&p, Container::Mp3, &keys(&new)).unwrap(), new);
    let after = id3(&p).unwrap();
    assert_eq!(cover(&after), cover(&before));
    assert!(cover(&after).is_some());
    assert_eq!(
        user_text(&after, "REPLAYGAIN_TRACK_GAIN").as_deref(),
        Some("-6.50 dB")
    );
    assert_eq!(user_text(&after, "MY TAG").as_deref(), Some("keep me"));
    assert_eq!(audio_md5(&p).unwrap(), md5);
    assert_eq!(w.audio_md5, md5);
    assert_eq!(w.after_sha.to_vec(), sha(&p));
    assert_eq!(id3_version(&p).unwrap(), Some(Id3Version::V4));
    assert!(leftovers(dir.path()).is_empty());

    // The scanner reads what was written.
    let t = kima_library::tags::read(&p).unwrap();
    assert_eq!(t.title.as_deref(), Some("New Title"));
    assert_eq!(t.recording_mbid, Some(Uuid::parse_str(REC).unwrap()));
    assert_eq!(t.artist_mbids.len(), 2);
    assert_eq!(t.genres, vec!["Rock".to_string(), "Pop".to_string()]);
    assert_eq!(
        (t.track, t.track_total, t.disc, t.disc_total),
        (Some(3), Some(12), Some(1), Some(2))
    );
    assert!(t.compilation);
}

#[test]
fn mp3_v23_stays_v23() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    make_v23(&p);
    let md5 = audio_md5(&p).unwrap();

    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V3), &every_field()).unwrap();
    assert!(new.contains_key("XSOP") && !new.contains_key("TSOP"));
    assert_eq!(new["TDOR"], vec!["1999"]);
    write(&p, 7, &new).unwrap();

    assert_eq!(id3_version(&p).unwrap(), Some(Id3Version::V3));
    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(&bytes[..4], b"ID3\x03");
    let has = |id: &[u8]| bytes.windows(4).any(|w| w == id);
    assert!(has(b"TYER") && has(b"TDAT") && has(b"TORY") && has(b"XSOP"));
    assert!(!has(b"TDRC") && !has(b"TDOR") && !has(b"TSOP"));

    let got = read_native(&p, Container::Mp3, &keys(&new)).unwrap();
    assert_eq!(got, new);
    assert_eq!(got["TDRC"], vec!["2001-03-04"]);
    assert_eq!(got["XSOP"], vec!["Artist, New"]);
    let tag = id3(&p).unwrap();
    assert_eq!(
        user_text(&tag, "MusicBrainz Artist Id"),
        Some(format!("{ARTIST_A}/{ARTIST_B}"))
    );
    assert_eq!(user_text(&tag, "MY TAG").as_deref(), Some("keep me"));
    assert!(cover(&tag).is_some());
    assert_eq!(audio_md5(&p).unwrap(), md5);
    // The scanner splits on `/`.
    assert_eq!(kima_library::tags::read(&p).unwrap().artist_mbids.len(), 2);
}

#[test]
fn mp3_without_id3_gets_v24() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = make_audio(dir.path(), "bare.mp3", Tags::default());
    // Saving an empty tag strips it.
    Id3v2Tag::new()
        .save_to_path(&p, WriteOptions::default())
        .unwrap();
    assert!(id3(&p).is_none());
    assert_eq!(id3_version(&p).unwrap(), None);

    let new = native_for_file(&p, Container::Mp3, None, &every_field()).unwrap();
    assert!(new.contains_key("TSOP"));
    let old = read_native(&p, Container::Mp3, &keys(&new)).unwrap();
    assert!(old.values().all(Vec::is_empty));
    write(&p, 1, &new).unwrap();
    assert_eq!(id3_version(&p).unwrap(), Some(Id3Version::V4));
    assert_eq!(std::fs::read(&p).unwrap()[..4], *b"ID3\x04");
    assert_eq!(read_native(&p, Container::Mp3, &keys(&new)).unwrap(), new);
}

#[test]
fn genre_multi_value_v24_v23() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let genres = TagValues {
        genres: Some(vec!["Rock".into(), "Pop".into(), "Trip Hop".into()]),
        ..TagValues::default()
    };
    let p4 = fixture(dir.path(), "v4.mp3");
    write(
        &p4,
        1,
        &native_for_file(&p4, Container::Mp3, Some(Id3Version::V4), &genres).unwrap(),
    )
    .unwrap();
    assert_eq!(
        text_frame(&id3(&p4).unwrap(), "TCON").as_deref(),
        Some("Rock\0Pop\0Trip Hop")
    );

    let p3 = fixture(dir.path(), "v3.mp3");
    make_v23(&p3);
    write(
        &p3,
        2,
        &native_for_file(&p3, Container::Mp3, Some(Id3Version::V3), &genres).unwrap(),
    )
    .unwrap();
    assert_eq!(
        text_frame(&id3(&p3).unwrap(), "TCON").as_deref(),
        Some("Rock/Pop/Trip Hop")
    );
}

#[test]
fn undo_values_restore_absent_keys() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = make_audio(dir.path(), "a.mp3", Tags::full());
    embed_cover(&p, png_bytes(8, 8, [1, 2, 3]));
    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let old = read_native(&p, Container::Mp3, &keys(&new)).unwrap();
    assert_eq!(old["TIT2"], vec!["The Title"]);
    assert!(old["UFID:http://musicbrainz.org"].is_empty());
    assert!(old["TXXX:MusicBrainz Artist Id"].is_empty());
    assert!(old["TSOP"].is_empty());

    write(&p, 1, &new).unwrap();
    assert_eq!(read_native(&p, Container::Mp3, &keys(&new)).unwrap(), new);
    write(&p, 2, &old).unwrap();
    assert_eq!(read_native(&p, Container::Mp3, &keys(&new)).unwrap(), old);
    let tag = id3(&p).unwrap();
    assert!(
        !tag.iter()
            .any(|f| matches!(f, Frame::UniqueFileIdentifier(_)))
    );
    assert!(user_text(&tag, "MusicBrainz Album Id").is_none());
    assert!(
        text_frame(&tag, "TCMP").is_some(),
        "ffmpeg's compilation flag is back"
    );
    assert!(cover(&tag).is_some());
}

#[test]
fn forced_lofty_failure_leaves_original_byte_identical() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    let before = sha(&p);
    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let hooks = WriteHooks {
        fail_lofty: true,
        ..WriteHooks::default()
    };
    let err = safe_write(
        &p,
        3,
        Container::Mp3,
        Some(Id3Version::V4),
        &new,
        &IgnoreSet::new(),
        &hooks,
    )
    .unwrap_err();
    assert!(matches!(err, TagWriteError::Lofty(_)), "{err:?}");
    assert_eq!(sha(&p), before);
    assert!(leftovers(dir.path()).is_empty());
}

#[test]
fn audio_changed_refused() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    let before = sha(&p);
    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let hooks = WriteHooks {
        corrupt_audio: true,
        ..WriteHooks::default()
    };
    let err = safe_write(
        &p,
        4,
        Container::Mp3,
        Some(Id3Version::V4),
        &new,
        &IgnoreSet::new(),
        &hooks,
    )
    .unwrap_err();
    assert_eq!(err, TagWriteError::AudioChanged);
    assert_eq!(
        err.to_string(),
        "the tag writer changed the audio; nothing written"
    );
    assert_eq!(sha(&p), before);
    assert!(leftovers(dir.path()).is_empty());
}

#[test]
fn stop_after_rename_leaves_the_new_tags() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let hooks = WriteHooks {
        stop_after_rename: true,
        ..WriteHooks::default()
    };
    assert!(
        safe_write(
            &p,
            5,
            Container::Mp3,
            Some(Id3Version::V4),
            &new,
            &IgnoreSet::new(),
            &hooks
        )
        .is_err()
    );
    assert_eq!(read_native(&p, Container::Mp3, &keys(&new)).unwrap(), new);
    assert!(leftovers(dir.path()).is_empty());
}

#[cfg(target_os = "linux")]
fn non_utf8_name() -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(b"caf\xe9 \xff.mp3".to_vec())
}

#[test]
fn audio_md5_and_write_handle_odd_names() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = fixture(dir.path(), "src/src.mp3");
    let md5 = audio_md5(&src).unwrap();
    let folder = dir.path().join("It's Here");
    std::fs::create_dir_all(&folder).unwrap();
    let mut names: Vec<std::ffi::OsString> = vec![
        "-start.mp3".into(),
        "a:b.mp3".into(),
        "plain name.mp3".into(),
    ];
    #[cfg(target_os = "linux")]
    names.push(non_utf8_name());

    let new = native_for_file(&src, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    for (i, name) in names.iter().enumerate() {
        let p = folder.join(name);
        std::fs::copy(&src, &p).unwrap();
        assert_eq!(audio_md5(&p).unwrap(), md5, "{name:?}");
        assert_eq!(container_of(&p).unwrap(), Container::Mp3, "{name:?}");
        let w = write(&p, 10 + i as i64, &new).unwrap_or_else(|e| panic!("{name:?}: {e}"));
        assert_eq!(w.audio_md5, md5, "{name:?}");
        assert_eq!(
            read_native(&p, Container::Mp3, &keys(&new)).unwrap(),
            new,
            "{name:?}"
        );
        assert_eq!(audio_md5(&p).unwrap(), md5, "{name:?}");
        // The written file is the real one at that exact name, a regular file.
        assert!(std::fs::symlink_metadata(&p).unwrap().is_file(), "{name:?}");
        if let Some(utf8) = name.to_str() {
            let rel = format!("It's Here/{utf8}");
            assert_eq!(refuse_symlink(dir.path(), &rel), Ok(()), "{name:?}");
        }
    }
    let listed: Vec<std::ffi::OsString> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    for name in &names {
        assert!(
            listed.contains(name),
            "{name:?} is still there under its own name"
        );
    }
    assert!(leftovers(&folder).is_empty());
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), names.len());
}

#[test]
fn wma_and_wav_unsupported_message() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = make_audio(dir.path(), "a.wav", Tags::full());
    let err = container_of(&wav).unwrap_err();
    assert_eq!(err, TagWriteError::Unsupported("wav".into()));
    assert_eq!(
        err.to_string(),
        "Tag writing is not supported for .wav files"
    );
    assert_eq!(id3_version(&wav).unwrap(), None);

    // An ASF header lofty does not know.
    let wma = dir.path().join("b.WMA");
    let mut bytes = vec![
        0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE,
        0x6C,
    ];
    bytes.resize(4096, 0);
    std::fs::write(&wma, bytes).unwrap();
    let err = container_of(&wma).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Tag writing is not supported for .wma files"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_file_refused() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("lib");
    let real = fixture(&root, "Artist/Album/a.mp3");
    std::os::unix::fs::symlink(&real, root.join("Artist/Album/link.mp3")).unwrap();
    std::os::unix::fs::symlink(root.join("Artist/Album"), root.join("Artist/Linked")).unwrap();
    let note = "a folder on the target path is a symbolic link";

    assert_eq!(refuse_symlink(&root, "Artist/Album/a.mp3"), Ok(()));
    let err = refuse_symlink(&root, "Artist/Album/link.mp3").unwrap_err();
    assert_eq!(err, TagWriteError::Symlink);
    assert_eq!(err.to_string(), note);
    assert_eq!(
        refuse_symlink(&root, "Artist/Linked/a.mp3"),
        Err(TagWriteError::Symlink)
    );
    assert!(refuse_symlink(&root, "../lib/Artist/Album/a.mp3").is_err());
    assert!(refuse_symlink(&root, "/etc/passwd").is_err());

    // The writer itself never writes through a linked file.
    let before = sha(&real);
    let link = root.join("Artist/Album/link.mp3");
    let new = native_for_file(&real, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    assert_eq!(write(&link, 1, &new).unwrap_err(), TagWriteError::Symlink);
    assert_eq!(sha(&real), before);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(leftovers(&root.join("Artist/Album")).is_empty());
}

#[test]
fn path_held_in_ignore_set_until_grace() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    let failing = fixture(dir.path(), "b.mp3");
    let ignore = IgnoreSet::new();
    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    assert!(!ignore.contains(&p));
    safe_write(
        &p,
        1,
        Container::Mp3,
        Some(Id3Version::V4),
        &new,
        &ignore,
        &WriteHooks::default(),
    )
    .unwrap();
    let hooks = WriteHooks {
        fail_lofty: true,
        ..WriteHooks::default()
    };
    safe_write(
        &failing,
        2,
        Container::Mp3,
        Some(Id3Version::V4),
        &new,
        &ignore,
        &hooks,
    )
    .unwrap_err();
    assert!(ignore.contains(&p), "held for the grace after the write");
    assert!(ignore.contains(&failing), "a failed write is held too");
    std::thread::sleep(IGNORE_GRACE + std::time::Duration::from_millis(500));
    assert!(!ignore.contains(&p));
    assert!(!ignore.contains(&failing));
}

#[test]
fn v23_frame_lofty_cannot_keep_refuses_the_write() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    let mut tag = id3(&p).unwrap();
    tag.insert(Frame::Text(lofty::id3::v2::TextInformationFrame::new(
        FrameId::Valid(Cow::Borrowed("TPUB")),
        lofty::TextEncoding::UTF16,
        "Album Sort".to_string(),
    )));
    tag.save_to_path(&p, WriteOptions::default().use_id3v23(true))
        .unwrap();
    // A v2.3 file carrying TSOA (album sort, which other tools write into v2.3): lofty cannot
    // save it as v2.3, so the write must be refused rather than drop it.
    let mut bytes = std::fs::read(&p).unwrap();
    let at = bytes.windows(4).position(|w| w == b"TPUB").unwrap();
    bytes[at..at + 4].copy_from_slice(b"TSOA");
    std::fs::write(&p, bytes).unwrap();
    assert_eq!(id3_version(&p).unwrap(), Some(Id3Version::V3));
    let before = sha(&p);

    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V3), &every_field()).unwrap();
    let err = write(&p, 1, &new).unwrap_err();
    assert_eq!(
        err.to_string(),
        "ID3v2.3 cannot keep TSOA with this writer; nothing written"
    );
    assert_eq!(sha(&p), before);
    assert!(leftovers(dir.path()).is_empty());
}

/// Renames `from` to `to` (same length) in the file's bytes, at its first occurrence.
fn patch(path: &Path, from: &[u8], to: &[u8]) {
    let mut bytes = std::fs::read(path).unwrap();
    let at = bytes.windows(from.len()).position(|w| w == from).unwrap();
    bytes[at..at + to.len()].copy_from_slice(to);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn duplicate_frames_lofty_merges_refuse_the_write() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    let mut tag = id3(&p).unwrap();
    tag.insert_user_text("MY TAH".into(), "second".into());
    tag.save_to_path(&p, WriteOptions::default()).unwrap();
    // Two TXXX frames with one description: lofty reads one of them.
    patch(&p, b"MY TAH", b"MY TAG");
    let before = sha(&p);

    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let err = write(&p, 1, &new).unwrap_err();
    assert!(matches!(err, TagWriteError::Verify(_)), "{err:?}");
    assert!(err.to_string().contains("TXXX:MY TAG (2 → 1)"), "{err}");
    assert_eq!(sha(&p), before);
    assert!(leftovers(dir.path()).is_empty());
}

#[test]
fn unparseable_frame_refuses_the_write() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    let mut tag = id3(&p).unwrap();
    tag.insert(Frame::Text(lofty::id3::v2::TextInformationFrame::new(
        FrameId::Valid(Cow::Borrowed("TPUB")),
        lofty::TextEncoding::UTF8,
        "Some Label".to_string(),
    )));
    tag.save_to_path(&p, WriteOptions::default()).unwrap();
    // An invalid frame id: lofty skips the frame while reading.
    patch(&p, b"TPUB", b"TP@B");
    let before = sha(&p);

    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let err = write(&p, 1, &new).unwrap_err();
    assert!(err.to_string().contains("TP@B (1 → 0)"), "{err}");
    assert_eq!(sha(&p), before);
    assert!(leftovers(dir.path()).is_empty());
}

#[test]
fn wrong_version_or_container_refused() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    make_v23(&p);
    let before = sha(&p);
    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let ignore = IgnoreSet::new();
    let hooks = WriteHooks::default();
    let err = safe_write(
        &p,
        1,
        Container::Mp3,
        Some(Id3Version::V4),
        &new,
        &ignore,
        &hooks,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the file has ID3v2 v3 but v4 was asked; nothing written"
    );
    let err = safe_write(&p, 1, Container::Flac, None, &new, &ignore, &hooks).unwrap_err();
    assert!(matches!(err, TagWriteError::Verify(_)), "{err:?}");
    // The writer's own copy name is never a file to write, nor deleted.
    let own = dir.path().join(".kima-tag-9.mp3");
    std::fs::copy(&p, &own).unwrap();
    let own_before = sha(&own);
    assert!(
        safe_write(
            &own,
            9,
            Container::Mp3,
            Some(Id3Version::V3),
            &new,
            &ignore,
            &hooks
        )
        .is_err()
    );
    assert!(own.exists(), "the file named like a copy is still there");
    assert_eq!(sha(&own), own_before);
    assert_eq!(sha(&p), before);
}

#[test]
fn panic_in_write_cleans_up() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = fixture(dir.path(), "a.mp3");
    let before = sha(&p);
    let new = native_for_file(&p, Container::Mp3, Some(Id3Version::V4), &every_field()).unwrap();
    let ignore = IgnoreSet::new();
    let hooks = WriteHooks {
        panic_in_write: true,
        ..WriteHooks::default()
    };
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        safe_write(
            &p,
            6,
            Container::Mp3,
            Some(Id3Version::V4),
            &new,
            &ignore,
            &hooks,
        )
    }));
    assert!(res.is_err(), "the hook panics");
    assert_eq!(sha(&p), before);
    assert!(leftovers(dir.path()).is_empty(), "the copy is deleted");
    assert!(
        ignore.contains(&p),
        "released with the grace, not dropped at once"
    );
    std::thread::sleep(IGNORE_GRACE + std::time::Duration::from_millis(500));
    assert!(!ignore.contains(&p), "the hold is released");
}

// ---------------------------------------------------------------------------------------------
// Vorbis comments (FLAC, Ogg Vorbis, Opus) and MP4

use lofty::flac::FlacFile;
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst, Mp4File};
use lofty::ogg::tag::VorbisComments;
use lofty::ogg::{OggPictureStorage, OpusFile, VorbisFile};
use lofty::picture::{MimeType, Picture, PictureType};

fn container_for(ext: &str) -> Container {
    match ext {
        "flac" => Container::Flac,
        "ogg" => Container::Ogg,
        "opus" => Container::Opus,
        "m4a" => Container::Mp4,
        _ => Container::Mp3,
    }
}

fn opts() -> ParseOptions {
    ParseOptions::new()
        .read_properties(false)
        .implicit_conversions(false)
}

/// The file's Vorbis comments, a FLAC file's `PICTURE` blocks included.
fn vorbis(path: &Path) -> VorbisComments {
    let mut f = std::fs::File::open(path).unwrap();
    match path.extension().and_then(|e| e.to_str()).unwrap() {
        "flac" => {
            let file = FlacFile::read_from(&mut f, opts()).unwrap();
            let mut vc = file.vorbis_comments().cloned().unwrap_or_default();
            for (pic, info) in file.pictures() {
                vc.insert_picture(pic.clone(), Some(*info)).unwrap();
            }
            vc
        }
        "ogg" => VorbisFile::read_from(&mut f, opts())
            .unwrap()
            .vorbis_comments()
            .clone(),
        _ => OpusFile::read_from(&mut f, opts())
            .unwrap()
            .vorbis_comments()
            .clone(),
    }
}

fn ilst(path: &Path) -> Ilst {
    let mut f = std::fs::File::open(path).unwrap();
    Mp4File::read_from(&mut f, opts())
        .unwrap()
        .ilst()
        .cloned()
        .unwrap()
}

fn freeform(name: &str) -> AtomIdent<'static> {
    AtomIdent::Freeform {
        mean: "com.apple.iTunes".into(),
        name: name.to_string().into(),
    }
}

fn cover_png() -> Picture {
    Picture::unchecked(png_bytes(16, 16, [200, 10, 10]))
        .pic_type(PictureType::CoverFront)
        .mime_type(MimeType::Png)
        .build()
}

fn picard() -> Picard<'static> {
    Picard {
        recording: Some("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
        release: Some("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
        artists: vec!["cccccccc-cccc-4ccc-8ccc-cccccccccccc"],
        genres: vec!["Jazz"],
        track: Some((5, 9)),
        ..Default::default()
    }
}

/// A tone with ffmpeg's tags, Picard's MBIDs, a cover, ReplayGain and a two-valued custom field.
fn vorbis_fixture(dir: &Path, rel: &str) -> PathBuf {
    let p = make_audio(dir, rel, Tags::full());
    tag_picard(&p, &picard());
    let mut vc = vorbis(&p);
    vc.push("REPLAYGAIN_TRACK_GAIN".into(), "-6.50 dB".into());
    vc.push("MY TAG".into(), "keep me".into());
    vc.push("MY TAG".into(), "and me".into());
    vc.insert_picture(cover_png(), None).unwrap();
    vc.save_to_path(&p, WriteOptions::default()).unwrap();
    p
}

/// The same for MP4: freeform ReplayGain and custom atoms, a cover.
fn mp4_fixture(dir: &Path, rel: &str) -> PathBuf {
    let p = make_audio(dir, rel, Tags::full());
    tag_picard(&p, &picard());
    let mut tag = ilst(&p);
    tag.insert(Atom::new(
        freeform("REPLAYGAIN_TRACK_GAIN"),
        AtomData::UTF8("-6.50 dB".into()),
    ));
    tag.insert(
        Atom::from_collection(
            freeform("MY TAG"),
            vec![
                AtomData::UTF8("keep me".into()),
                AtomData::UTF8("and me".into()),
            ],
        )
        .unwrap(),
    );
    tag.insert_picture(cover_png());
    tag.save_to_path(&p, WriteOptions::default()).unwrap();
    p
}

fn fixture_for(dir: &Path, rel: &str) -> PathBuf {
    if rel.ends_with(".m4a") {
        mp4_fixture(dir, rel)
    } else {
        vorbis_fixture(dir, rel)
    }
}

fn write_as(
    path: &Path,
    id: i64,
    container: Container,
    new: &NativeTags,
) -> Result<kima_library::tagwrite::Written, TagWriteError> {
    safe_write(
        path,
        id,
        container,
        None,
        new,
        &IgnoreSet::new(),
        &WriteHooks::default(),
    )
}

/// What every container must keep: the cover, ReplayGain and the custom field.
fn kept_extras(path: &Path) -> (Vec<u8>, Vec<String>, Vec<String>) {
    if path.extension().is_some_and(|e| e == "m4a") {
        let tag = ilst(path);
        let texts = |name: &str| {
            tag.get(&freeform(name))
                .map(|a| {
                    a.data()
                        .filter_map(|d| match d {
                            AtomData::UTF8(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let cover = tag.pictures().unwrap().next().unwrap().data().to_vec();
        (cover, texts("REPLAYGAIN_TRACK_GAIN"), texts("MY TAG"))
    } else {
        let vc = vorbis(path);
        let all = |k: &str| vc.get_all(k).map(str::to_string).collect::<Vec<_>>();
        let cover = vc.pictures()[0].0.data().to_vec();
        (cover, all("REPLAYGAIN_TRACK_GAIN"), all("MY TAG"))
    }
}

fn round_trip(ext: &str) {
    let dir = tempfile::tempdir().unwrap();
    let p = fixture_for(dir.path(), &format!("a.{ext}"));
    let c = container_for(ext);
    assert_eq!(container_of(&p).unwrap(), c);
    assert_eq!(id3_version(&p).unwrap(), None);
    let md5 = audio_md5(&p).unwrap();
    let extras = kept_extras(&p);
    assert_eq!(extras.0, png_bytes(16, 16, [200, 10, 10]));
    assert_eq!(extras.1, vec!["-6.50 dB"]);
    assert_eq!(extras.2, vec!["keep me", "and me"]);

    let new = native_for_file(&p, c, None, &every_field()).unwrap();
    assert!(!new.contains_key("TOTALTRACKS") && !new.contains_key("TOTALDISCS"));
    if c == Container::Mp4 {
        assert_eq!(new.len(), 18);
        assert_eq!(new["trkn"], vec!["3/12"]);
        assert_eq!(new["disk"], vec!["1/2"]);
        assert_eq!(new["©gen"], vec!["Rock; Pop"]);
        assert_eq!(new["cpil"], vec!["1"]);
        assert_eq!(
            new["----:com.apple.iTunes:MusicBrainz Artist Id"],
            vec![ARTIST_A, ARTIST_B]
        );
    } else {
        let expected = [
            "ALBUM",
            "ALBUMARTIST",
            "ALBUMARTISTSORT",
            "ARTIST",
            "ARTISTSORT",
            "COMPILATION",
            "DATE",
            "DISCNUMBER",
            "DISCTOTAL",
            "GENRE",
            "MUSICBRAINZ_ALBUMARTISTID",
            "MUSICBRAINZ_ALBUMID",
            "MUSICBRAINZ_ARTISTID",
            "MUSICBRAINZ_RELEASEGROUPID",
            "MUSICBRAINZ_RELEASETRACKID",
            "MUSICBRAINZ_TRACKID",
            "ORIGINALDATE",
            "TITLE",
            "TRACKNUMBER",
            "TRACKTOTAL",
        ];
        assert_eq!(new.keys().map(String::as_str).collect::<Vec<_>>(), expected);
        assert_eq!(new["GENRE"], vec!["Rock", "Pop"]);
        assert_eq!(new["TRACKNUMBER"], vec!["3"]);
        assert_eq!(new["TRACKTOTAL"], vec!["12"]);
        assert_eq!(new["MUSICBRAINZ_ARTISTID"], vec![ARTIST_A, ARTIST_B]);
    }

    let w = write_as(&p, 1, c, &new).unwrap_or_else(|e| panic!("{ext}: {e}"));
    assert_eq!(read_native(&p, c, &keys(&new)).unwrap(), new);
    assert_eq!(kept_extras(&p), extras, "{ext}: cover, ReplayGain, custom");
    assert_eq!(audio_md5(&p).unwrap(), md5);
    assert_eq!(w.audio_md5, md5);
    assert_eq!(w.after_sha.to_vec(), sha(&p));
    assert!(leftovers(dir.path()).is_empty());

    // The scanner reads what was written.
    let t = kima_library::tags::read(&p).unwrap();
    assert_eq!(t.title.as_deref(), Some("New Title"));
    assert_eq!(t.album_artist.as_deref(), Some("New Album Artist"));
    assert_eq!(t.recording_mbid, Some(Uuid::parse_str(REC).unwrap()));
    assert_eq!(t.artist_mbids.len(), 2);
    assert_eq!(t.genres, vec!["Rock".to_string(), "Pop".to_string()]);
    assert_eq!(
        (t.track, t.track_total, t.disc, t.disc_total),
        (Some(3), Some(12), Some(1), Some(2))
    );
    assert!(t.compilation);

    // Undo restores every touched key, absent ones included.
    let old_dir = tempfile::tempdir().unwrap();
    let orig = fixture_for(old_dir.path(), &format!("o.{ext}"));
    let old = read_native(&orig, c, &keys(&new)).unwrap();
    write_as(&p, 2, c, &old).unwrap();
    assert_eq!(read_native(&p, c, &keys(&new)).unwrap(), old);
    assert_eq!(kept_extras(&p), extras);
    assert_eq!(audio_md5(&p).unwrap(), md5);
}

#[test]
fn flac_round_trip() {
    if !require_ffmpeg() {
        return;
    }
    round_trip("flac");
}

#[test]
fn ogg_round_trip() {
    if !require_ffmpeg() {
        return;
    }
    round_trip("ogg");
}

#[test]
fn opus_round_trip() {
    if !require_ffmpeg() {
        return;
    }
    round_trip("opus");
}

#[test]
fn m4a_round_trip() {
    if !require_ffmpeg() {
        return;
    }
    round_trip("m4a");
}

#[test]
fn vorbis_totaltracks_only_when_present() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = vorbis_fixture(dir.path(), "a.flac");
    let v = every_field();
    let n = native_for_file(&p, Container::Flac, None, &v).unwrap();
    assert!(!n.contains_key("TOTALTRACKS") && !n.contains_key("TOTALDISCS"));

    let mut vc = vorbis(&p);
    vc.push("totaltracks".into(), "9".into());
    vc.push("TOTALDISCS".into(), "3".into());
    vc.save_to_path(&p, WriteOptions::default()).unwrap();
    let n = native_for_file(&p, Container::Flac, None, &v).unwrap();
    assert_eq!(n["TOTALTRACKS"], vec!["12"]);
    assert_eq!(n["TOTALDISCS"], vec!["2"]);
    let old = read_native(&p, Container::Flac, &keys(&n)).unwrap();
    assert_eq!(old["TOTALTRACKS"], vec!["9"]);
    assert_eq!(old["TRACKTOTAL"], vec!["9"], "Picard's TRACKTOTAL");

    write_as(&p, 1, Container::Flac, &n).unwrap();
    let vc = vorbis(&p);
    assert_eq!(vc.get_all("TOTALTRACKS").collect::<Vec<_>>(), ["12"]);
    assert_eq!(vc.get_all("TRACKTOTAL").collect::<Vec<_>>(), ["12"]);
    assert_eq!(vc.get_all("TOTALDISCS").collect::<Vec<_>>(), ["2"]);
    write_as(&p, 2, Container::Flac, &old).unwrap();
    assert_eq!(read_native(&p, Container::Flac, &keys(&n)).unwrap(), old);

    // Without a total, the totals are removed with the number written.
    let no_total = TagValues {
        track: Some(NumberPair {
            number: 4,
            total: None,
        }),
        ..TagValues::default()
    };
    let n = native_for_file(&p, Container::Flac, None, &no_total).unwrap();
    assert!(n["TRACKTOTAL"].is_empty() && n["TOTALTRACKS"].is_empty());
    write_as(&p, 3, Container::Flac, &n).unwrap();
    let vc = vorbis(&p);
    assert_eq!(vc.get_all("TRACKNUMBER").collect::<Vec<_>>(), ["4"]);
    assert_eq!(vc.get_all("TRACKTOTAL").count(), 0);
    assert_eq!(vc.get_all("TOTALTRACKS").count(), 0);
    assert_eq!(vc.get_all("TOTALDISCS").collect::<Vec<_>>(), ["3"]);
}

#[test]
fn mp4_freeform_multi_value() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = mp4_fixture(dir.path(), "a.m4a");
    let new = native_for_file(&p, Container::Mp4, None, &every_field()).unwrap();
    write_as(&p, 1, Container::Mp4, &new).unwrap();

    let tag = ilst(&p);
    let artist_ids: Vec<&AtomData> = tag
        .get(&freeform("MusicBrainz Artist Id"))
        .unwrap()
        .data()
        .collect();
    assert_eq!(
        artist_ids,
        [
            &AtomData::UTF8(ARTIST_A.into()),
            &AtomData::UTF8(ARTIST_B.into())
        ],
        "one data atom per value"
    );
    let genre: Vec<&AtomData> = tag
        .get(&AtomIdent::Fourcc(*b"\xa9gen"))
        .unwrap()
        .data()
        .collect();
    assert_eq!(genre, [&AtomData::UTF8("Rock; Pop".into())]);
    assert!(tag.is_compilation());
    let trkn: Vec<&AtomData> = tag
        .get(&AtomIdent::Fourcc(*b"trkn"))
        .unwrap()
        .data()
        .collect();
    assert!(
        matches!(trkn[..], [AtomData::Unknown { data, .. }] if data == &[0, 0, 0, 3, 0, 12, 0, 0])
    );
    let disk: Vec<&AtomData> = tag
        .get(&AtomIdent::Fourcc(*b"disk"))
        .unwrap()
        .data()
        .collect();
    assert!(matches!(disk[..], [AtomData::Unknown { data, .. }] if data == &[0, 0, 0, 1, 0, 2]));

    // A file whose atom already holds several values keeps them apart on undo.
    let old = read_native(&p, Container::Mp4, &keys(&new)).unwrap();
    let mut two = old.clone();
    two.insert(
        "©gen".to_string(),
        vec!["Jazz".to_string(), "Blues".to_string()],
    );
    write_as(&p, 2, Container::Mp4, &two).unwrap();
    assert_eq!(read_native(&p, Container::Mp4, &keys(&new)).unwrap(), two);
    assert_eq!(
        kima_library::tags::read(&p).unwrap().genres,
        vec!["Jazz".to_string(), "Blues".to_string()]
    );
    // `cpil` false is removed; a stored `0` reads back as `0`.
    let mut flags = NativeTags::new();
    flags.insert("cpil".into(), vec!["0".into()]);
    write_as(&p, 3, Container::Mp4, &flags).unwrap();
    assert_eq!(
        read_native(&p, Container::Mp4, &keys(&flags)).unwrap(),
        flags
    );
    assert!(!ilst(&p).is_compilation());
}

#[test]
fn m4a_moov_before_mdat_grows_with_valid_offsets() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("fast.m4a");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=duration=2"])
        .args(["-c:a", "aac", "-movflags", "+faststart"])
        .arg(&p)
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&p).unwrap();
    let at = |f: &[u8]| bytes.windows(4).position(|w| w == f).unwrap();
    assert!(at(b"moov") < at(b"mdat"), "moov comes first");
    let md5 = audio_md5(&p).unwrap();

    let big = TagValues {
        title: Some("Long ".repeat(2000)),
        ..every_field()
    };
    let new = native_for_file(&p, Container::Mp4, None, &big).unwrap();
    let before_len = bytes.len();
    write_as(&p, 1, Container::Mp4, &new).unwrap();
    assert!(std::fs::metadata(&p).unwrap().len() > before_len as u64 + 10_000);
    let bytes = std::fs::read(&p).unwrap();
    let at = |f: &[u8]| bytes.windows(4).position(|w| w == f).unwrap();
    assert!(at(b"moov") < at(b"mdat"), "the audio moved");
    assert_eq!(audio_md5(&p).unwrap(), md5, "the chunk offsets follow it");
    assert_eq!(read_native(&p, Container::Mp4, &keys(&new)).unwrap(), new);
}

#[test]
fn flac_keeps_other_metadata_blocks() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = vorbis_fixture(dir.path(), "a.flac");
    // An APPLICATION block right after STREAMINFO.
    let mut bytes = std::fs::read(&p).unwrap();
    let after_streaminfo = 4 + 4 + 34;
    let app = [&[0x02, 0, 0, 8][..], b"kima", b"data"].concat();
    bytes.splice(after_streaminfo..after_streaminfo, app.iter().copied());
    std::fs::write(&p, &bytes).unwrap();
    let md5 = audio_md5(&p).unwrap();

    write_as(
        &p,
        1,
        Container::Flac,
        &native_for_file(&p, Container::Flac, None, &every_field()).unwrap(),
    )
    .unwrap();
    let bytes = std::fs::read(&p).unwrap();
    assert!(bytes.windows(8).any(|w| w == b"kimadata"));
    assert_eq!(audio_md5(&p).unwrap(), md5);
    assert_eq!(kept_extras(&p).0, png_bytes(16, 16, [200, 10, 10]));
}

/// Writes every field into `p` and asserts the refusal: the message contains `needle`, the file
/// is byte-identical and no copy is left.
fn assert_refused(p: &Path, c: Container, needle: &str) {
    let before = sha(p);
    // Building the keys reads a Vorbis file, which may already refuse it; then a title is enough.
    let new = match native_for_file(p, c, None, &every_field()) {
        Ok(n) => n,
        Err(e) => {
            assert!(e.to_string().contains(needle), "{e}");
            let title = if c == Container::Mp4 {
                "©nam"
            } else {
                "TITLE"
            };
            NativeTags::from([(title.to_string(), vec!["x".to_string()])])
        }
    };
    let err = write_as(p, 1, c, &new).unwrap_err();
    assert!(matches!(err, TagWriteError::Verify(_)), "{err:?}");
    assert!(err.to_string().contains(needle), "{err}");
    assert!(err.to_string().ends_with("; nothing written"), "{err}");
    assert_eq!(sha(p), before);
    assert!(leftovers(p.parent().unwrap()).is_empty());
}

#[test]
fn vorbis_field_lofty_cannot_read_refuses_the_write() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for ext in ["flac", "ogg"] {
        let p = vorbis_fixture(dir.path(), &format!("a.{ext}"));
        // A value that is not UTF-8: lofty drops the field while reading.
        patch(&p, b"keep me", b"keep\xffme");
        if ext == "ogg" {
            fix_ogg_crcs(&p);
        }
        assert_refused(&p, container_for(ext), "MY TAG (2 → 1)");
    }
}

#[test]
fn flac_picture_field_refuses_the_write() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = vorbis_fixture(dir.path(), "a.flac");
    // A METADATA_BLOCK_PICTURE comment in a FLAC file: lofty would turn it into a PICTURE block.
    let pic = Picture::unchecked(png_bytes(4, 4, [1, 2, 3]))
        .mime_type(MimeType::Png)
        .build();
    let info = lofty::picture::PictureInformation::from_picture(&pic).unwrap();
    let encoded = String::from_utf8(pic.as_flac_bytes(info, true)).unwrap();
    let mut file = {
        let mut f = std::fs::File::open(&p).unwrap();
        FlacFile::read_from(&mut f, opts()).unwrap()
    };
    let mut vc = file.vorbis_comments().cloned().unwrap();
    vc.push("METADATA_BLOCK_PICTURE".into(), encoded);
    file.set_vorbis_comments(vc);
    file.save_to_path(&p, WriteOptions::default()).unwrap();
    assert!(
        std::fs::read(&p)
            .unwrap()
            .windows(23)
            .any(|w| w == b"METADATA_BLOCK_PICTURE=")
    );
    assert_refused(&p, Container::Flac, "METADATA_BLOCK_PICTURE (1 → 0)");
}

#[test]
fn flac_two_comment_blocks_refused() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = vorbis_fixture(dir.path(), "a.flac");
    let mut bytes = std::fs::read(&p).unwrap();
    let mut block = vec![0x04, 0, 0, 0];
    let mut content = 0u32.to_le_bytes().to_vec();
    content.extend(1u32.to_le_bytes());
    content.extend(5u32.to_le_bytes());
    content.extend(b"X=one");
    block[3] = content.len() as u8;
    block.extend(content);
    bytes.splice(42..42, block);
    std::fs::write(&p, &bytes).unwrap();
    assert_refused(
        &p,
        Container::Flac,
        "the file has 2 VORBIS_COMMENT blocks; nothing written",
    );
}

#[test]
fn chained_ogg_refused() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let a = make_audio(dir.path(), "a.ogg", Tags::full());
    let b = make_audio(dir.path(), "b.ogg", Tags::default());
    let p = dir.path().join("chained.ogg");
    let mut bytes = std::fs::read(&a).unwrap();
    bytes.extend(std::fs::read(&b).unwrap());
    std::fs::write(&p, bytes).unwrap();
    std::fs::remove_file(&a).unwrap();
    std::fs::remove_file(&b).unwrap();
    assert_refused(
        &p,
        Container::Ogg,
        "multi-stream and chained Ogg files are not written; nothing written",
    );
}

/// The Ogg page checksum (CRC-32, polynomial 0x04C11DB7, not reflected).
fn ogg_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for b in page {
        crc ^= u32::from(*b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Recomputes every page checksum after an edit that kept the page sizes.
fn fix_ogg_crcs(path: &Path) {
    let mut b = std::fs::read(path).unwrap();
    let mut pos = 0;
    while pos + 27 <= b.len() && &b[pos..pos + 4] == b"OggS" {
        let nseg = usize::from(b[pos + 26]);
        let size: usize = b[pos + 27..pos + 27 + nseg]
            .iter()
            .map(|x| usize::from(*x))
            .sum();
        let end = pos + 27 + nseg + size;
        b[pos + 22..pos + 26].fill(0);
        let crc = ogg_crc(&b[pos..end]);
        b[pos + 22..pos + 26].copy_from_slice(&crc.to_le_bytes());
        pos = end;
    }
    std::fs::write(path, b).unwrap();
}

#[test]
fn opus_tags_binary_data_refused() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = make_audio(dir.path(), "a.opus", Tags::default());
    // Append binary data (first bit set: RFC 7845 says keep it) to the OpusTags packet, which
    // ends the second page.
    let mut b = std::fs::read(&p).unwrap();
    let page2 = 27
        + usize::from(b[26])
        + b[27..27 + usize::from(b[26])]
            .iter()
            .map(|x| usize::from(*x))
            .sum::<usize>();
    assert_eq!(&b[page2..page2 + 4], b"OggS");
    let nseg = usize::from(b[page2 + 26]);
    let last = page2 + 27 + nseg - 1;
    assert!(b[last] < 250, "the packet ends with a short segment");
    b[last] += 3;
    let size: usize = b[page2 + 27..page2 + 27 + nseg]
        .iter()
        .map(|x| usize::from(*x))
        .sum();
    let end = page2 + 27 + nseg + size - 3;
    b.splice(end..end, [0x01, 0xAB, 0xCD]);
    std::fs::write(&p, b).unwrap();
    fix_ogg_crcs(&p);
    assert!(audio_md5(&p).is_ok(), "still a valid file");
    assert_refused(&p, Container::Opus, "OpusTags binary data (1 → 0)");
}

#[test]
fn mp4_item_lofty_cannot_read_refuses_the_write() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = mp4_fixture(dir.path(), "a.m4a");
    // An unknown type set in the custom atom's first `data`: lofty skips that value (and, its
    // skip miscounting the bytes, loses the items after it).
    let mut b = std::fs::read(&p).unwrap();
    let name = b.windows(6).position(|w| w == b"MY TAG").unwrap();
    let data = name + b[name..].windows(4).position(|w| w == b"data").unwrap();
    b[data + 4] = 7;
    std::fs::write(&p, b).unwrap();
    assert_refused(
        &p,
        Container::Mp4,
        "these atoms would be lost or changed: ilst ",
    );
}

#[test]
fn fragmented_mp4_refused() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("frag.m4a");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=duration=2"])
        .args(["-c:a", "aac", "-metadata", "title=T"])
        .args(["-movflags", "frag_keyframe+empty_moov"])
        .arg(&p)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(container_of(&p).unwrap(), Container::Mp4);
    assert_refused(
        &p,
        Container::Mp4,
        "fragmented MP4 files are not written; nothing written",
    );
}

// ---------------------------------------------------------------------------------------------
// MP4 structure (fix round 1)

/// The atom at `path` (fourccs from the top level down): `(start, body start, end)`. A full
/// `meta`'s version and flags are part of its header here.
fn find_atom(b: &[u8], path: &[&[u8; 4]]) -> (usize, usize, usize) {
    let (mut from, mut to) = (0, b.len());
    let mut found = (0, 0, 0);
    for want in path {
        let mut pos = from;
        loop {
            assert!(pos + 8 <= to, "no {:?}", String::from_utf8_lossy(*want));
            let len = u32::from_be_bytes(b[pos..pos + 4].try_into().unwrap()) as usize;
            if &b[pos + 4..pos + 8] == *want {
                let mut body = pos + 8;
                if *want == b"meta" && b[body..body + 4] == [0, 0, 0, 0] {
                    body += 4;
                }
                found = (pos, body, pos + len);
                break;
            }
            pos += len;
        }
        (from, to) = (found.1, found.2);
    }
    found
}

/// Inserts `payload` at the start of the body of the atom at `path`, growing every atom on the
/// way (the file's `mdat` is before `moov`, so no chunk offset moves).
fn insert_into(p: &Path, path: &[&[u8; 4]], payload: &[u8]) {
    let mut b = std::fs::read(p).unwrap();
    let body = find_atom(&b, path).1;
    for depth in 1..=path.len() {
        let start = find_atom(&b, &path[..depth]).0;
        let len = u32::from_be_bytes(b[start..start + 4].try_into().unwrap());
        b[start..start + 4].copy_from_slice(&(len + payload.len() as u32).to_be_bytes());
    }
    b.splice(body..body, payload.iter().copied());
    std::fs::write(p, b).unwrap();
}

fn atom(fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut a = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    a.extend(fourcc);
    a.extend(body);
    a
}

/// `levels` `edts` atoms, each inside the one before.
fn nested(levels: usize) -> Vec<u8> {
    let mut a = Vec::new();
    for _ in 0..levels {
        a = atom(b"edts", &a);
    }
    a
}

fn plain_m4a(dir: &Path) -> PathBuf {
    let p = make_audio(dir, "a.m4a", Tags::full());
    let b = std::fs::read(&p).unwrap();
    assert!(find_atom(&b, &[b"mdat"]).0 < find_atom(&b, &[b"moov"]).0);
    p
}

#[test]
fn mp4_nesting_depth_is_capped() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let keys = vec!["©nam".to_string()];
    let damaged = "the MP4 atom structure is damaged; nothing written";
    // `moov` is level 1, so 15 nested atoms reach level 16, the cap.
    for (levels, ok) in [(15, true), (16, false)] {
        let p = plain_m4a(dir.path());
        insert_into(&p, &[b"moov"], &nested(levels));
        let got = read_native(&p, Container::Mp4, &keys);
        assert_eq!(got.is_ok(), ok, "{levels}: {got:?}");
        if !ok {
            assert_eq!(got.unwrap_err().to_string(), damaged);
        }
        std::fs::remove_file(&p).unwrap();
    }
    // 10 000 levels (80 kB): refused, not a stack overflow, in the read and in the write.
    let p = plain_m4a(dir.path());
    insert_into(&p, &[b"moov"], &nested(10_000));
    assert_eq!(
        read_native(&p, Container::Mp4, &keys)
            .unwrap_err()
            .to_string(),
        damaged
    );
    assert_refused(&p, Container::Mp4, damaged);
}

#[test]
fn mp4_with_absolute_offset_atoms_refused() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = plain_m4a(dir.path());
    insert_into(&p, &[b"moov"], &atom(b"saio", &[0; 8]));
    assert_refused(
        &p,
        Container::Mp4,
        "MP4 files with a saio atom are not written; nothing written",
    );

    // `iloc` in a top-level `meta`, after `moov`.
    std::fs::remove_file(&p).unwrap();
    let p = plain_m4a(dir.path());
    let meta = atom(b"meta", &[&[0u8; 4][..], &atom(b"iloc", &[0; 8])].concat());
    let mut b = std::fs::read(&p).unwrap();
    b.extend(meta);
    std::fs::write(&p, b).unwrap();
    assert_refused(
        &p,
        Container::Mp4,
        "MP4 files with a iloc atom are not written; nothing written",
    );
}

#[test]
fn mp4_track_ilst_is_not_the_tag() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = plain_m4a(dir.path());
    // A track's own udta.meta.ilst with a title: lofty neither reads nor writes it.
    let data = atom(
        b"data",
        &[&[0, 0, 0, 1, 0, 0, 0, 0][..], b"Track Level"].concat(),
    );
    let ilst = atom(b"ilst", &atom(b"\xa9nam", &data));
    let meta = atom(b"meta", &[&[0u8; 4][..], &ilst].concat());
    insert_into(&p, &[b"moov", b"trak"], &atom(b"udta", &meta));
    let md5 = audio_md5(&p).unwrap();

    let new = native_for_file(&p, Container::Mp4, None, &every_field()).unwrap();
    write_as(&p, 1, Container::Mp4, &new).unwrap();
    assert_eq!(read_native(&p, Container::Mp4, &keys(&new)).unwrap(), new);
    let b = std::fs::read(&p).unwrap();
    assert!(b.windows(11).any(|w| w == b"Track Level"), "kept as it was");
    assert_eq!(audio_md5(&p).unwrap(), md5);
}

#[test]
fn mp4_without_udta_gets_one() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = plain_m4a(dir.path());
    // ffmpeg's moov.udta becomes padding: the file has no tag at all.
    let mut b = std::fs::read(&p).unwrap();
    let udta = find_atom(&b, &[b"moov", b"udta"]).0;
    b[udta + 4..udta + 8].copy_from_slice(b"free");
    std::fs::write(&p, &b).unwrap();
    let new = native_for_file(&p, Container::Mp4, None, &every_field()).unwrap();
    assert!(
        read_native(&p, Container::Mp4, &keys(&new))
            .unwrap()
            .values()
            .all(Vec::is_empty)
    );
    let md5 = audio_md5(&p).unwrap();

    write_as(&p, 1, Container::Mp4, &new).unwrap();
    let b = std::fs::read(&p).unwrap();
    find_atom(&b, &[b"moov", b"udta", b"meta", b"hdlr"]);
    assert_eq!(read_native(&p, Container::Mp4, &keys(&new)).unwrap(), new);
    assert_eq!(audio_md5(&p).unwrap(), md5);
}
