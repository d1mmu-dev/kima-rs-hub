//! MP3: the §19.8 ID3v2 column, through lofty's concrete `Id3v2Tag`.
//!
//! lofty upgrades a v2.3 tag to v2.4 frames when reading (`TYER`+`TDAT` become `TDRC`, `TORY`
//! becomes `TDOR`) and converts back when saving with `use_id3v23(true)`. What that means here:
//! - v2.3 dates: `TDRC` is written as `TYER` (+ `TDAT` when the day is known), so a `YYYY-MM`
//!   date keeps only its year; `TDOR` is written as `TORY`, a year. [`native`] produces exactly
//!   what reads back.
//! - v2.3 multiple values (genres, artist MBIDs) are joined with `/` (Picard's v2.3 separator).
//! - v2.3 has no `TSOP` (lofty drops it); the artist sort is `XSOP`, the v2.3 frame Picard and
//!   iTunes use. lofty reads `XSOP` as an unknown (binary) frame, so it is encoded and decoded
//!   here.

use std::borrow::Cow;
use std::fs::File;
use std::path::Path;

use lofty::TextEncoding;
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::id3::v2::{
    BinaryFrame, ExtendedTextFrame, Frame, FrameFlags, FrameId, Id3v2Tag, Id3v2Version,
    TextInformationFrame, TimestampFrame, UniqueFileIdentifierFrame,
};
use lofty::mpeg::MpegFile;
use lofty::tag::TagExt;
use lofty::tag::items::Timestamp;
use sha2::{Digest, Sha256};

use super::values::{NumberPair, TagValues};
use super::{Id3Version, NativeTags, TagWriteError};

const UFID_KEY: &str = "UFID:http://musicbrainz.org";
const TXXX_RELEASE_TRACK: &str = "TXXX:MusicBrainz Release Track Id";
const TXXX_ALBUM: &str = "TXXX:MusicBrainz Album Id";
const TXXX_RELEASE_GROUP: &str = "TXXX:MusicBrainz Release Group Id";
const TXXX_ARTIST: &str = "TXXX:MusicBrainz Artist Id";
const TXXX_ALBUM_ARTIST: &str = "TXXX:MusicBrainz Album Artist Id";

/// Every key this module may touch: the §19.8 ID3v2 column, plus `XSOP` (artist sort in v2.3).
/// A key outside this list (from a corrupted journal, say) is refused, never written.
const KEYS: &[&str] = &[
    "TIT2",
    "TPE1",
    "TSOP",
    "XSOP",
    "TALB",
    "TPE2",
    "TSO2",
    "TRCK",
    "TPOS",
    "TDRC",
    "TDOR",
    "TCON",
    "TCMP",
    UFID_KEY,
    TXXX_RELEASE_TRACK,
    TXXX_ALBUM,
    TXXX_RELEASE_GROUP,
    TXXX_ARTIST,
    TXXX_ALBUM_ARTIST,
];

/// How a key maps onto frames.
#[derive(Debug, Clone, Copy)]
enum Key<'a> {
    Text(&'a str),
    Timestamp(&'a str),
    UserText(&'a str),
    Ufid(&'a str),
    /// `XSOP`, a frame lofty only knows as binary.
    Xsop,
}

fn parse_key(key: &str) -> Result<Key<'_>, TagWriteError> {
    if !KEYS.contains(&key) {
        return Err(TagWriteError::Verify(format!(
            "not a writable ID3v2 key: {key}"
        )));
    }
    Ok(if let Some(desc) = key.strip_prefix("TXXX:") {
        Key::UserText(desc)
    } else if let Some(owner) = key.strip_prefix("UFID:") {
        Key::Ufid(owner)
    } else if key == "XSOP" {
        Key::Xsop
    } else if key == "TDRC" || key == "TDOR" {
        Key::Timestamp(key)
    } else {
        Key::Text(key)
    })
}

impl Key<'_> {
    /// The frame ids this key replaces besides its own, when saving as `version`: in v2.3 the
    /// date keys own the v2.3 frames lofty writes for them (a stray `TDAT` lofty could not merge
    /// would otherwise be written twice), and `XSOP` owns a `TSOP` that v2.3 cannot keep.
    fn aliases(self, version: Id3Version) -> &'static [&'static str] {
        match (self, version) {
            (Key::Timestamp("TDRC"), Id3Version::V3) => &["TYER", "TDAT", "TIME"],
            (Key::Timestamp("TDOR"), Id3Version::V3) => &["TORY"],
            (Key::Xsop, _) => &["TSOP"],
            _ => &[],
        }
    }

    /// `frame` is this key's frame (not an alias).
    fn is(self, frame: &Frame<'_>) -> bool {
        match (self, frame) {
            (Key::UserText(desc), Frame::UserText(f)) => f.description == desc,
            (Key::Ufid(owner), Frame::UniqueFileIdentifier(f)) => f.owner == owner,
            (Key::Text(id) | Key::Timestamp(id), f) => f.id_str() == id,
            (Key::Xsop, f) => f.id_str() == "XSOP",
            _ => false,
        }
    }

    /// `frame` is replaced (or removed) when this key is written as `version`.
    fn owns(self, frame: &Frame<'_>, version: Id3Version) -> bool {
        self.is(frame) || self.aliases(version).contains(&frame.id_str())
    }
}

// ---------------------------------------------------------------------------------------------
// Mapping

fn pair(p: NumberPair) -> String {
    match p.total {
        Some(t) => format!("{}/{}", p.number, t),
        None => p.number.to_string(),
    }
}

/// A date as it reads back after a write: v2.4 keeps `YYYY[-MM[-DD]]`; v2.3 keeps a full date
/// (`TYER`+`TDAT`) or only the year; an original date in v2.3 (`TORY`) is a year. A value that
/// is not a date is left out (the key is not touched).
fn date_value(s: &str, version: Id3Version, original: bool) -> Option<String> {
    let ts: Timestamp = s.trim().parse().ok()?;
    let ts = Timestamp {
        year: ts.year,
        month: ts.month,
        day: ts.month.and(ts.day),
        ..Timestamp::default()
    };
    let ts = match version {
        Id3Version::V4 => ts,
        Id3Version::V3 if !original && ts.day.is_some() => ts,
        Id3Version::V3 => Timestamp {
            year: ts.year,
            ..Timestamp::default()
        },
    };
    Some(ts.to_string())
}

/// Several values of one frame: v2.4 keeps them apart (NUL-separated in the frame), v2.3 gets
/// one value joined with `/`. Empty strings are dropped; no value left means "not touched" (`None`), never a removal.
fn multi(values: Vec<String>, version: Id3Version) -> Option<Vec<String>> {
    let values: Vec<String> = values.into_iter().filter(|s| !s.is_empty()).collect();
    if values.is_empty() {
        return None;
    }
    Some(match version {
        Id3Version::V4 => values,
        Id3Version::V3 => vec![values.join("/")],
    })
}

pub(super) fn native(version: Id3Version, v: &TagValues) -> NativeTags {
    let mut out = NativeTags::new();
    let mut put = |key: &str, val: Option<Vec<String>>| {
        if let Some(val) = val {
            out.insert(key.to_string(), val);
        }
    };
    let one = |s: &Option<String>| s.clone().filter(|s| !s.is_empty()).map(|s| vec![s]);
    put("TIT2", one(&v.title));
    put("TPE1", one(&v.artist));
    let sort_key = match version {
        Id3Version::V4 => "TSOP",
        Id3Version::V3 => "XSOP",
    };
    put(sort_key, one(&v.artist_sort));
    put("TALB", one(&v.album));
    put("TPE2", one(&v.album_artist));
    put("TSO2", one(&v.album_artist_sort));
    put("TRCK", v.track.map(|p| vec![pair(p)]));
    put("TPOS", v.disc.map(|p| vec![pair(p)]));
    put(
        "TDRC",
        v.date
            .as_deref()
            .and_then(|d| date_value(d, version, false))
            .map(|d| vec![d]),
    );
    put(
        "TDOR",
        v.original_date
            .as_deref()
            .and_then(|d| date_value(d, version, true))
            .map(|d| vec![d]),
    );
    put("TCON", v.genres.clone().and_then(|g| multi(g, version)));
    put(
        "TCMP",
        v.compilation
            .map(|c| if c { vec!["1".to_string()] } else { Vec::new() }),
    );
    let id = |u: &Option<uuid::Uuid>| u.map(|u| vec![u.to_string()]);
    let ids = |u: &Option<Vec<uuid::Uuid>>| {
        u.as_ref()
            .and_then(|u| multi(u.iter().map(ToString::to_string).collect(), version))
    };
    put(UFID_KEY, id(&v.recording_mbid));
    put(TXXX_RELEASE_TRACK, id(&v.release_track_mbid));
    put(TXXX_ALBUM, id(&v.release_mbid));
    put(TXXX_RELEASE_GROUP, id(&v.release_group_mbid));
    put(TXXX_ARTIST, ids(&v.artist_mbids));
    put(TXXX_ALBUM_ARTIST, ids(&v.album_artist_mbids));
    out
}

// ---------------------------------------------------------------------------------------------
// Reading

fn options() -> ParseOptions {
    ParseOptions::new().read_properties(false)
}

fn read_file(path: &Path) -> Result<MpegFile, TagWriteError> {
    let mut f = File::open(path)?;
    Ok(MpegFile::read_from(&mut f, options())?)
}

fn read_tag(path: &Path) -> Result<Option<Id3v2Tag>, TagWriteError> {
    Ok(read_file(path)?.id3v2().cloned())
}

pub(super) fn version(path: &Path) -> Result<Option<Id3Version>, TagWriteError> {
    Ok(read_tag(path)?.map(|t| match t.original_version() {
        Id3v2Version::V3 => Id3Version::V3,
        _ => Id3Version::V4,
    }))
}

fn split(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split('\0')
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

/// `XSOP` as lofty stores it: `[encoding byte, text…]`.
fn decode_binary_text(data: &[u8]) -> Option<String> {
    let id = FrameId::Valid(Cow::Borrowed("XSOP"));
    let f =
        TextInformationFrame::parse(&mut &data[..], id, FrameFlags::default(), Id3v2Version::V3)
            .ok()??;
    Some(f.value.into_owned())
}

/// Latin-1 when every character fits, else UTF-16 with a BOM: the two encodings v2.3 has.
fn encode_binary_text(s: &str) -> Vec<u8> {
    if s.chars().all(|c| (c as u32) < 0x100) {
        let mut out = vec![0u8];
        out.extend(s.chars().map(|c| c as u8));
        out
    } else {
        let mut out = vec![1u8, 0xFF, 0xFE];
        for u in s.encode_utf16() {
            out.extend(u.to_le_bytes());
        }
        out
    }
}

fn values_of(tag: &Id3v2Tag, key: Key<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for frame in tag.iter().filter(|f| key.is(f)) {
        match frame {
            Frame::Text(f) => out.extend(split(&f.value)),
            Frame::Timestamp(f) => out.push(f.timestamp.to_string()),
            Frame::UserText(f) => out.extend(split(&f.content)),
            Frame::UniqueFileIdentifier(f) => {
                let s = String::from_utf8_lossy(&f.identifier);
                out.extend(split(&s));
            }
            Frame::Binary(f) => {
                out.extend(decode_binary_text(&f.data).iter().flat_map(|s| split(s)))
            }
            _ => {}
        }
    }
    out
}

pub(super) fn read(path: &Path, keys: &[String]) -> Result<NativeTags, TagWriteError> {
    let tag = read_tag(path)?.unwrap_or_default();
    let mut out = NativeTags::new();
    for key in keys {
        let k = parse_key(key)?;
        let mut vals = values_of(&tag, k);
        if vals.is_empty() && matches!(k, Key::Xsop) {
            // A v2.3 file with a (non-standard) TSOP: that is the artist sort an XSOP replaces,
            // so it is the value to restore on undo.
            vals = values_of(&tag, Key::Text("TSOP"));
        }
        out.insert(key.clone(), vals);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Writing

fn frame_id(id: &str) -> FrameId<'static> {
    FrameId::Valid(Cow::Owned(id.to_string()))
}

fn joined(values: &[String], version: Id3Version) -> String {
    match version {
        Id3Version::V4 => values.join("\0"),
        Id3Version::V3 => values.join("/"),
    }
}

fn new_frame(
    key: Key<'_>,
    values: &[String],
    version: Id3Version,
) -> Result<Frame<'static>, TagWriteError> {
    let enc = TextEncoding::UTF8;
    Ok(match key {
        Key::Text(id) => Frame::Text(TextInformationFrame::new(
            frame_id(id),
            enc,
            joined(values, version),
        )),
        Key::Timestamp(id) => {
            let ts: Timestamp = values[0]
                .parse()
                .map_err(|_| TagWriteError::Verify(format!("{id}: not a date: {}", values[0])))?;
            Frame::Timestamp(TimestampFrame::new(frame_id(id), enc, ts))
        }
        Key::UserText(desc) => Frame::UserText(ExtendedTextFrame::new(
            enc,
            desc.to_string(),
            joined(values, version),
        )),
        Key::Ufid(owner) => Frame::UniqueFileIdentifier(UniqueFileIdentifierFrame::new(
            owner.to_string(),
            values[0].clone().into_bytes(),
        )),
        Key::Xsop => Frame::Binary(BinaryFrame::new(
            frame_id("XSOP"),
            encode_binary_text(&values.join("/")),
        )),
    })
}

/// Replaces the keys of `new` in the file's ID3v2 tag (creating one if there is none) and saves
/// it as `version`. Every other frame is kept as lofty read it; the ID3v1 and APE tags are not
/// touched (lofty rewrites only the ID3v2 tag at the start of the file).
pub(super) fn write(
    path: &Path,
    version: Id3Version,
    new: &NativeTags,
) -> Result<(), TagWriteError> {
    let mut tag = read_tag(path)?.unwrap_or_default();
    for (key, values) in new {
        let k = parse_key(key)?;
        tag.retain(|f| !k.owns(f, version));
        if !values.is_empty() {
            tag.insert(new_frame(k, values, version)?);
        }
    }
    let opts = match version {
        Id3Version::V4 => WriteOptions::default(),
        Id3Version::V3 => WriteOptions::default().use_id3v23(true),
    };
    tag.save_to_path(path, opts)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Preservation check

fn digest(what: impl std::fmt::Debug) -> [u8; 32] {
    struct H(Sha256);
    impl std::fmt::Write for H {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0.update(s.as_bytes());
            Ok(())
        }
    }
    let mut h = H(Sha256::new());
    let _ = std::fmt::write(&mut h, format_args!("{what:?}"));
    h.0.finalize().into()
}

/// One digest per ID3v2 frame no key of `keys` owns (as lofty reads it: id, flags, encoding,
/// content), plus the tag flags and the whole ID3v1 and APE tags; sorted.
pub(super) fn untouched(
    path: &Path,
    version: Id3Version,
    keys: &[String],
) -> Result<Vec<[u8; 32]>, TagWriteError> {
    let parsed = keys
        .iter()
        .map(|k| parse_key(k))
        .collect::<Result<Vec<_>, _>>()?;
    let file = read_file(path)?;
    // No ID3v2 tag reads as an empty one: a write may create the tag, and removing every frame
    // strips it.
    let tag = file.id3v2().cloned().unwrap_or_default();
    let mut out = vec![
        digest(file.id3v1()),
        digest(file.ape()),
        digest(tag.flags()),
    ];
    out.extend(
        tag.iter()
            .filter(|f| !parsed.iter().any(|k| k.owns(f, version)))
            .map(digest),
    );
    out.sort_unstable();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_read_back_as_written() {
        use Id3Version::{V3, V4};
        assert_eq!(
            date_value("1997-05-21", V4, false).as_deref(),
            Some("1997-05-21")
        );
        assert_eq!(date_value("1997-05", V4, true).as_deref(), Some("1997-05"));
        assert_eq!(date_value("1997", V4, false).as_deref(), Some("1997"));
        assert_eq!(
            date_value("1997-05-21", V3, false).as_deref(),
            Some("1997-05-21")
        );
        assert_eq!(date_value("1997-05", V3, false).as_deref(), Some("1997"));
        assert_eq!(date_value("1997-05-21", V3, true).as_deref(), Some("1997"));
        assert_eq!(date_value("someday", V4, false), None);
    }

    #[test]
    fn empty_values_are_not_touched() {
        let v = TagValues {
            title: Some(String::new()),
            genres: Some(vec![String::new()]),
            artist_mbids: Some(Vec::new()),
            compilation: Some(false),
            ..TagValues::default()
        };
        let n = native(Id3Version::V4, &v);
        assert_eq!(n.keys().map(String::as_str).collect::<Vec<_>>(), ["TCMP"]);
        assert!(n["TCMP"].is_empty(), "false removes the flag");
    }

    #[test]
    fn only_table_keys_are_writable() {
        assert!(parse_key("TIT2").is_ok());
        assert!(parse_key(UFID_KEY).is_ok());
        assert!(parse_key("APIC").is_err());
        assert!(parse_key("TXXX:REPLAYGAIN_TRACK_GAIN").is_err());
        assert!(parse_key("UFID:http://example.org").is_err());
    }

    #[test]
    fn binary_text_round_trips() {
        for s in ["Beatles, The", "Björk", "東京事変"] {
            assert_eq!(
                decode_binary_text(&encode_binary_text(s)).as_deref(),
                Some(s)
            );
        }
    }
}
