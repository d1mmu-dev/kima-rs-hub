//! Helpers shared by the workspace's tests. Only ever a dev-dependency.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The skip rule for ffmpeg-gated tests: `available` outside CI, and a panic in CI when ffmpeg is
/// missing, so a test cannot pass there by doing nothing.
pub fn ffmpeg_guard(available: bool, ci: bool) -> bool {
    assert!(available || !ci, "ffmpeg is required in CI");
    if !available {
        eprintln!("ffmpeg not found; skipping test");
    }
    available
}

/// Returns false (after printing a note) when ffmpeg is missing; tests should return early.
/// Under `CI` a missing ffmpeg panics instead.
pub fn require_ffmpeg() -> bool {
    ffmpeg_guard(ffmpeg_available(), std::env::var_os("CI").is_some())
}

/// The plain tags `make_audio` writes through ffmpeg's `-metadata`.
#[derive(Debug, Clone, Default)]
pub struct Tags {
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub date: Option<String>,
    pub disc: Option<String>,
    pub track: Option<String>,
    pub compilation: bool,
}

impl Tags {
    pub fn full() -> Self {
        Tags {
            album_artist: Some("The Album Artist".into()),
            artist: Some("The Track Artist".into()),
            album: Some("The Album".into()),
            title: Some("The Title".into()),
            date: Some("1997-05-21".into()),
            disc: Some("2".into()),
            track: Some("5".into()),
            compilation: true,
        }
    }
}

/// Generates a 2 s sine tone at `dir/rel_path`; the format comes from the extension.
pub fn make_audio(dir: &Path, rel_path: &str, tags: Tags) -> PathBuf {
    let out = dir.join(rel_path);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("");
    let codec: &[&str] = match ext {
        "mp3" => &["-c:a", "libmp3lame"],
        "flac" => &["-c:a", "flac"],
        "ogg" => &["-c:a", "libvorbis"],
        "opus" => &["-c:a", "libopus"],
        "m4a" => &["-c:a", "aac"],
        "wav" => &["-c:a", "pcm_s16le"],
        other => panic!("unsupported fixture extension {other}"),
    };
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=duration=2"])
        .args(codec);
    for (key, val) in [
        ("album_artist", &tags.album_artist),
        ("artist", &tags.artist),
        ("album", &tags.album),
        ("title", &tags.title),
        ("date", &tags.date),
        ("disc", &tags.disc),
        ("track", &tags.track),
    ] {
        if let Some(v) = val {
            cmd.arg("-metadata").arg(format!("{key}={v}"));
        }
    }
    if tags.compilation {
        cmd.args(["-metadata", "compilation=1"]);
    }
    let status = cmd
        .arg(&out)
        .stdout(Stdio::null())
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed for {rel_path}");
    out
}

/// What MusicBrainz Picard writes, as raw strings so tests can also write junk.
#[derive(Debug, Clone, Default)]
pub struct Picard<'a> {
    pub recording: Option<&'a str>,
    pub release_track: Option<&'a str>,
    pub release: Option<&'a str>,
    pub release_group: Option<&'a str>,
    pub artists: Vec<&'a str>,
    pub album_artists: Vec<&'a str>,
    pub isrc: Option<&'a str>,
    pub genres: Vec<&'a str>,
    /// `(number, total)`
    pub disc: Option<(u32, u32)>,
    /// `(number, total)`
    pub track: Option<(u32, u32)>,
    pub uslt: Option<&'a str>,
    /// ID3 `SYLT` lines `(ms, text)`; MP3 only.
    pub sylt: Vec<(u32, &'a str)>,
    pub acoustid_id: Option<&'a str>,
}

/// Writes `p` into the file's native tag with lofty's concrete tag types: ID3v2 frames (UFID,
/// TXXX, TSRC, TCON, TRCK, TPOS, USLT, SYLT) for MP3, Vorbis comments for FLAC/Ogg/Opus, and
/// MP4 atoms (freeform `----:com.apple.iTunes:*`, `©gen`, `©lyr`, `trkn`, `disk`) for M4A.
/// Existing tags are kept.
pub fn tag_picard(path: &Path, p: &Picard<'_>) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "mp3" => picard_id3v2(path, p),
        "flac" | "ogg" | "opus" => picard_vorbis(path, ext, p),
        "m4a" => picard_mp4(path, p),
        other => panic!("tag_picard: unsupported extension {other}"),
    }
}

fn picard_id3v2(path: &Path, p: &Picard<'_>) {
    use lofty::TextEncoding;
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile;
    use lofty::id3::v2::{
        BinaryFrame, ExtendedTextFrame, Frame, FrameId, Id3v2Tag, SyncTextContentType,
        SynchronizedTextFrame, TextInformationFrame, TimestampFormat, UniqueFileIdentifierFrame,
        UnsynchronizedTextFrame,
    };
    use lofty::mpeg::MpegFile;
    use lofty::tag::TagExt;
    use std::borrow::Cow;

    let mut f = std::fs::File::open(path).unwrap();
    let file = MpegFile::read_from(&mut f, ParseOptions::new()).unwrap();
    drop(f);
    let mut tag = file.id3v2().cloned().unwrap_or_else(Id3v2Tag::new);
    let enc = TextEncoding::UTF8;
    let text = |id: &'static str, v: String| {
        Frame::Text(TextInformationFrame::new(
            FrameId::Valid(Cow::Borrowed(id)),
            enc,
            v,
        ))
    };
    let txxx =
        |desc: &str, v: String| Frame::UserText(ExtendedTextFrame::new(enc, desc.to_string(), v));
    if let Some(r) = p.recording {
        tag.insert(Frame::UniqueFileIdentifier(UniqueFileIdentifierFrame::new(
            "http://musicbrainz.org",
            r.as_bytes().to_vec(),
        )));
    }
    for (desc, v) in [
        ("MusicBrainz Release Track Id", p.release_track),
        ("MusicBrainz Album Id", p.release),
        ("MusicBrainz Release Group Id", p.release_group),
        ("Acoustid Id", p.acoustid_id),
    ] {
        if let Some(v) = v {
            tag.insert(txxx(desc, v.to_string()));
        }
    }
    for (desc, vs) in [
        ("MusicBrainz Artist Id", &p.artists),
        ("MusicBrainz Album Artist Id", &p.album_artists),
    ] {
        if !vs.is_empty() {
            tag.insert(txxx(desc, vs.join("\0")));
        }
    }
    if let Some(i) = p.isrc {
        tag.insert(text("TSRC", i.to_string()));
    }
    if !p.genres.is_empty() {
        tag.insert(text("TCON", p.genres.join("\0")));
    }
    if let Some((n, t)) = p.track {
        tag.insert(text("TRCK", format!("{n}/{t}")));
    }
    if let Some((n, t)) = p.disc {
        tag.insert(text("TPOS", format!("{n}/{t}")));
    }
    if let Some(l) = p.uslt {
        tag.insert(Frame::UnsynchronizedText(UnsynchronizedTextFrame::new(
            enc,
            *b"eng",
            "",
            l.to_string(),
        )));
    }
    if !p.sylt.is_empty() {
        let sylt = SynchronizedTextFrame::new(
            enc,
            *b"eng",
            TimestampFormat::MS,
            SyncTextContentType::Lyrics,
            None,
            p.sylt.iter().map(|(ms, t)| (*ms, t.to_string())).collect(),
        );
        let bytes = sylt.as_bytes(WriteOptions::default()).unwrap();
        tag.insert(Frame::Binary(BinaryFrame::new(
            FrameId::Valid(Cow::Borrowed("SYLT")),
            bytes,
        )));
    }
    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

fn picard_vorbis(path: &Path, ext: &str, p: &Picard<'_>) {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile;
    use lofty::flac::FlacFile;
    use lofty::ogg::tag::VorbisComments;
    use lofty::ogg::{OpusFile, VorbisFile};
    use lofty::tag::TagExt;

    let mut f = std::fs::File::open(path).unwrap();
    let opts = ParseOptions::new();
    let mut vc: VorbisComments = match ext {
        "flac" => FlacFile::read_from(&mut f, opts)
            .unwrap()
            .vorbis_comments()
            .cloned()
            .unwrap_or_default(),
        "ogg" => VorbisFile::read_from(&mut f, opts)
            .unwrap()
            .vorbis_comments()
            .clone(),
        _ => OpusFile::read_from(&mut f, opts)
            .unwrap()
            .vorbis_comments()
            .clone(),
    };
    drop(f);
    let one = |vc: &mut VorbisComments, k: &str, v: Option<String>| {
        if let Some(v) = v {
            vc.insert(k.to_string(), v);
        }
    };
    one(
        &mut vc,
        "MUSICBRAINZ_TRACKID",
        p.recording.map(str::to_string),
    );
    one(
        &mut vc,
        "MUSICBRAINZ_RELEASETRACKID",
        p.release_track.map(str::to_string),
    );
    one(
        &mut vc,
        "MUSICBRAINZ_ALBUMID",
        p.release.map(str::to_string),
    );
    one(
        &mut vc,
        "MUSICBRAINZ_RELEASEGROUPID",
        p.release_group.map(str::to_string),
    );
    one(&mut vc, "ACOUSTID_ID", p.acoustid_id.map(str::to_string));
    one(&mut vc, "ISRC", p.isrc.map(str::to_string));
    one(&mut vc, "LYRICS", p.uslt.map(str::to_string));
    if let Some((n, t)) = p.track {
        one(&mut vc, "TRACKNUMBER", Some(n.to_string()));
        one(&mut vc, "TRACKTOTAL", Some(t.to_string()));
    }
    if let Some((n, t)) = p.disc {
        one(&mut vc, "DISCNUMBER", Some(n.to_string()));
        one(&mut vc, "DISCTOTAL", Some(t.to_string()));
    }
    for (k, vs) in [
        ("MUSICBRAINZ_ARTISTID", &p.artists),
        ("MUSICBRAINZ_ALBUMARTISTID", &p.album_artists),
        ("GENRE", &p.genres),
    ] {
        if !vs.is_empty() {
            let _ = vc.remove(k).count();
            for v in vs {
                vc.push(k.to_string(), v.to_string());
            }
        }
    }
    vc.save_to_path(path, WriteOptions::default()).unwrap();
}

fn picard_mp4(path: &Path, p: &Picard<'_>) {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile;
    use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst, Mp4File};
    use lofty::tag::{Accessor, TagExt};
    use std::borrow::Cow;

    let mut f = std::fs::File::open(path).unwrap();
    let file = Mp4File::read_from(&mut f, ParseOptions::new()).unwrap();
    drop(f);
    let mut ilst = file.ilst().cloned().unwrap_or_else(Ilst::new);
    let set = |ilst: &mut Ilst, ident: AtomIdent<'static>, vs: Vec<String>| {
        if vs.is_empty() {
            return;
        }
        let data = vs.into_iter().map(AtomData::UTF8).collect();
        ilst.replace_atom(Atom::from_collection(ident, data).unwrap());
    };
    let freeform = |name: &'static str| AtomIdent::Freeform {
        mean: Cow::Borrowed("com.apple.iTunes"),
        name: Cow::Borrowed(name),
    };
    let opt = |v: Option<&str>| v.map(str::to_string).into_iter().collect::<Vec<_>>();
    let many = |vs: &[&str]| vs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    set(
        &mut ilst,
        freeform("MusicBrainz Track Id"),
        opt(p.recording),
    );
    set(
        &mut ilst,
        freeform("MusicBrainz Release Track Id"),
        opt(p.release_track),
    );
    set(&mut ilst, freeform("MusicBrainz Album Id"), opt(p.release));
    set(
        &mut ilst,
        freeform("MusicBrainz Release Group Id"),
        opt(p.release_group),
    );
    set(&mut ilst, freeform("Acoustid Id"), opt(p.acoustid_id));
    set(&mut ilst, freeform("ISRC"), opt(p.isrc));
    set(
        &mut ilst,
        freeform("MusicBrainz Artist Id"),
        many(&p.artists),
    );
    set(
        &mut ilst,
        freeform("MusicBrainz Album Artist Id"),
        many(&p.album_artists),
    );
    set(&mut ilst, AtomIdent::Fourcc(*b"\xa9gen"), many(&p.genres));
    set(&mut ilst, AtomIdent::Fourcc(*b"\xa9lyr"), opt(p.uslt));
    if let Some((n, t)) = p.track {
        ilst.set_track(n);
        ilst.set_track_total(t);
    }
    if let Some((n, t)) = p.disc {
        ilst.set_disk(n);
        ilst.set_disk_total(t);
    }
    ilst.save_to_path(path, WriteOptions::default()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "ffmpeg is required in CI")]
    fn ffmpeg_guard_panics_in_ci_without_ffmpeg() {
        ffmpeg_guard(false, true);
    }

    #[test]
    fn ffmpeg_guard_skips_outside_ci() {
        assert!(!ffmpeg_guard(false, false));
        assert!(ffmpeg_guard(true, false));
        assert!(ffmpeg_guard(true, true));
    }
}
