mod common;

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use common::{Picard, Tags, embed_cover, make_audio, png_bytes, require_ffmpeg, tag_picard};
use kima_library::organize::apply::IGNORE_GRACE;
use kima_library::tagwrite::{
    Container, Id3Version, NativeTags, NumberPair, TagValues, TagWriteError, WriteHooks, audio_md5,
    container_of, id3_version, native, read_native, refuse_symlink, safe_write,
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

    let new = native(Container::Mp3, Some(Id3Version::V4), &every_field());
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

    let new = native(Container::Mp3, Some(Id3Version::V3), &every_field());
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

    let new = native(Container::Mp3, None, &every_field());
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
        &native(Container::Mp3, Some(Id3Version::V4), &genres),
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
        &native(Container::Mp3, Some(Id3Version::V3), &genres),
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
    let new = native(Container::Mp3, Some(Id3Version::V4), &every_field());
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
    let new = native(Container::Mp3, Some(Id3Version::V4), &every_field());
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
    let new = native(Container::Mp3, Some(Id3Version::V4), &every_field());
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
    let new = native(Container::Mp3, Some(Id3Version::V4), &every_field());
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

    let new = native(Container::Mp3, Some(Id3Version::V4), &every_field());
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
        assert_eq!(
            refuse_symlink(dir.path(), &format!("It's Here/{}", name.to_string_lossy())),
            Ok(())
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
    let new = native(Container::Mp3, Some(Id3Version::V4), &every_field());
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
    let new = native(Container::Mp3, Some(Id3Version::V4), &every_field());
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

    let new = native(Container::Mp3, Some(Id3Version::V3), &every_field());
    let err = write(&p, 1, &new).unwrap_err();
    assert!(matches!(err, TagWriteError::Verify(_)), "{err:?}");
    assert_eq!(sha(&p), before);
    assert!(leftovers(dir.path()).is_empty());
}
