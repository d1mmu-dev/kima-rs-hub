//! Tag reading via lofty. Strings are trimmed; empty values become `None`.
//!
//! Besides the plain tags, `read` takes the MusicBrainz ids, ISRC, disc/track totals, genres
//! and embedded lyrics (spec §8.1, D-107) through lofty's generic `ItemKey`s; ID3 `SYLT` is
//! read from the concrete `Id3v2Tag`, since the generic `Tag` does not carry it.

use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum TagError {
    #[error("cannot read tags: {0}")]
    Lofty(#[from] lofty::error::FileParseError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawTags {
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub year: Option<i32>,
    pub date: Option<String>,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    pub compilation: bool,
    pub duration_secs: f64,
    pub codec: Option<String>,
    pub bitrate: Option<u32>,
    pub recording_mbid: Option<Uuid>,
    pub release_track_mbid: Option<Uuid>,
    pub release_mbid: Option<Uuid>,
    pub release_group_mbid: Option<Uuid>,
    pub artist_mbids: Vec<Uuid>,
    pub album_artist_mbids: Vec<Uuid>,
    /// Normalised (`normalize_isrc`).
    pub isrc: Option<String>,
    /// Split on `;`, trimmed, `_`-prefixed names dropped, deduplicated ignoring case.
    pub genres: Vec<String>,
    pub disc_total: Option<u32>,
    pub track_total: Option<u32>,
    pub lyrics: Option<EmbeddedLyrics>,
}

/// Lyrics embedded in the file; at least one side is present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbeddedLyrics {
    pub plain: Option<String>,
    /// LRC text.
    pub synced: Option<String>,
}

const AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "m4a", "aac", "wav", "wma", "aiff", "ape", "wv",
];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn clean(s: Option<impl AsRef<str>>) -> Option<String> {
    let t = s?.as_ref().trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn item(tag: &Tag, key: ItemKey) -> Option<String> {
    clean(tag.get_string(key))
}

/// Leading four-digit year of a date such as `1997`, `1997-05-21` or `1997-05-21T00:00:00`.
fn year_of(date: &str) -> Option<i32> {
    let digits: String = date.chars().take(4).collect();
    if digits.len() == 4 && digits.chars().all(|c| c.is_ascii_digit()) {
        digits.parse().ok().filter(|y| *y > 0)
    } else {
        None
    }
}

/// A MusicBrainz id: the trimmed value must be the 36-character hyphenated form (any case) and
/// not the nil UUID. Braced, `urn:uuid:` and 32-hex forms are refused, as upstream's `isMbid`.
pub fn parse_mbid(s: &str) -> Option<Uuid> {
    let t = s.trim();
    let b = t.as_bytes();
    if b.len() != 36 {
        return None;
    }
    let shape_ok = b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    });
    if !shape_ok {
        return None;
    }
    Uuid::parse_str(t).ok().filter(|u| !u.is_nil())
}

/// A single-valued id field: every value present must be the same valid id, else nothing.
fn single_mbid(tag: &Tag, key: ItemKey) -> Option<Uuid> {
    let mut found = None;
    for v in tag.get_strings(key) {
        if v.trim().is_empty() {
            continue;
        }
        let id = parse_mbid(v)?;
        if found.is_some_and(|f| f != id) {
            return None;
        }
        found = Some(id);
    }
    found
}

/// A multi-valued id field: each value, also split on `;` and `/`; invalid parts are dropped.
fn multi_mbids(tag: &Tag, key: ItemKey) -> Vec<Uuid> {
    tag.get_strings(key)
        .flat_map(|v| v.split([';', '/']))
        .filter_map(parse_mbid)
        .collect()
}

/// The first value (split on `;` or `,`), spaces and hyphens removed, upper-cased, and it must
/// match `^[A-Z]{2}[A-Z0-9]{3}\d{7}$` (upstream `normalizeIsrc`).
pub fn normalize_isrc(s: &str) -> Option<String> {
    let first = s.split([';', ',']).next()?;
    let n: String = first
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let b = n.as_bytes();
    let ok = b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..5]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && b[5..].iter().all(u8::is_ascii_digit);
    ok.then_some(n)
}

fn genres_of(tag: &Tag) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for g in tag.get_strings(ItemKey::Genre).flat_map(|v| v.split(';')) {
        let g = g.trim();
        if g.is_empty() || g.starts_with('_') {
            continue;
        }
        if !out.iter().any(|o| o.to_lowercase() == g.to_lowercase()) {
            out.push(g.to_string());
        }
    }
    out
}

/// ID3 `SYLT` lines `(ms, text)` as LRC: `[mm:ss.cc] text`, one per line.
pub fn sylt_to_lrc(frames: &[(u32, String)]) -> String {
    frames
        .iter()
        .map(|(ms, text)| {
            let (mins, secs, cs) = (ms / 60_000, (ms % 60_000) / 1000, (ms % 1000) / 10);
            format!("[{mins:02}:{secs:02}.{cs:02}] {}", text.trim())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// LRC text: its first non-empty line starts with `[dd:dd`.
fn looks_synced(text: &str) -> bool {
    let Some(line) = text.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return false;
    };
    let b = line.as_bytes();
    b.len() >= 6
        && b[0] == b'['
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3] == b':'
        && b[4].is_ascii_digit()
        && b[5].is_ascii_digit()
}

/// The first `SYLT` frame with millisecond timestamps and text content, as LRC.
fn mp3_sylt(path: &Path) -> Option<String> {
    use lofty::config::ParseOptions;
    use lofty::id3::v2::{Frame, SyncTextContentType, SynchronizedTextFrame, TimestampFormat};
    use lofty::mpeg::MpegFile;
    let mut f = std::fs::File::open(path).ok()?;
    let file = MpegFile::read_from(&mut f, ParseOptions::new().read_properties(false)).ok()?;
    file.id3v2()?.into_iter().find_map(|frame| {
        let Frame::Binary(bin) = frame else {
            return None;
        };
        if bin.id().as_str() != "SYLT" {
            return None;
        }
        let sylt = SynchronizedTextFrame::parse(&bin.data, bin.flags()).ok()?;
        let texty = matches!(
            sylt.content_type,
            SyncTextContentType::Lyrics
                | SyncTextContentType::TextTranscription
                | SyncTextContentType::Other
        );
        (texty && sylt.timestamp_format == TimestampFormat::MS && !sylt.content.is_empty())
            .then(|| sylt_to_lrc(&sylt.content))
    })
}

/// `UnsyncLyrics` then `Lyrics` values: the first LRC-looking text is synced, the first other
/// text is plain. A `SYLT` frame (MP3) wins for the synced side.
fn lyrics_of(tag: &Tag, sylt: Option<String>) -> Option<EmbeddedLyrics> {
    let mut out = EmbeddedLyrics {
        plain: None,
        synced: sylt,
    };
    let texts = tag
        .get_strings(ItemKey::UnsyncLyrics)
        .chain(tag.get_strings(ItemKey::Lyrics));
    for t in texts {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if looks_synced(t) {
            out.synced.get_or_insert_with(|| t.to_string());
        } else {
            out.plain.get_or_insert_with(|| t.to_string());
        }
    }
    (out.plain.is_some() || out.synced.is_some()).then_some(out)
}

fn codec_name(ft: FileType) -> String {
    match ft {
        FileType::Mpeg => "mp3".to_string(),
        FileType::Flac => "flac".to_string(),
        FileType::Vorbis => "vorbis".to_string(),
        FileType::Opus => "opus".to_string(),
        FileType::Mp4 => "aac".to_string(),
        FileType::Wav => "wav".to_string(),
        FileType::Aiff => "aiff".to_string(),
        FileType::Ape => "ape".to_string(),
        FileType::WavPack => "wavpack".to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

pub fn read(path: &Path) -> Result<RawTags, TagError> {
    let tagged = Probe::open(path)?.guess_file_type()?.read()?;
    let props = tagged.properties();
    let mut out = RawTags {
        duration_secs: props.duration().as_secs_f64(),
        codec: Some(codec_name(tagged.file_type())),
        bitrate: props.audio_bitrate().filter(|b| *b > 0),
        ..Default::default()
    };
    let sylt = (tagged.file_type() == FileType::Mpeg)
        .then(|| mp3_sylt(path))
        .flatten();
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        out.lyrics = sylt.map(|s| EmbeddedLyrics {
            plain: None,
            synced: Some(s),
        });
        return Ok(out);
    };
    out.album_artist = item(tag, ItemKey::AlbumArtist);
    out.artist = clean(tag.artist());
    out.album = clean(tag.album());
    out.title = clean(tag.title());
    out.disc = tag.disk();
    out.track = tag.track();
    out.date = item(tag, ItemKey::RecordingDate).or_else(|| item(tag, ItemKey::Year));
    out.year = out.date.as_deref().and_then(year_of);
    out.compilation = tag
        .get_string(ItemKey::FlagCompilation)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false);
    out.recording_mbid = single_mbid(tag, ItemKey::MusicBrainzRecordingId);
    out.release_track_mbid = single_mbid(tag, ItemKey::MusicBrainzTrackId);
    out.release_mbid = single_mbid(tag, ItemKey::MusicBrainzReleaseId);
    out.release_group_mbid = single_mbid(tag, ItemKey::MusicBrainzReleaseGroupId);
    out.artist_mbids = multi_mbids(tag, ItemKey::MusicBrainzArtistId);
    out.album_artist_mbids = multi_mbids(tag, ItemKey::MusicBrainzReleaseArtistId);
    out.isrc = tag
        .get_strings(ItemKey::Isrc)
        .find(|v| !v.trim().is_empty())
        .and_then(normalize_isrc);
    out.genres = genres_of(tag);
    out.disc_total = tag.disk_total();
    out.track_total = tag.track_total();
    out.lyrics = lyrics_of(tag, sylt);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{looks_synced, normalize_isrc, parse_mbid, year_of};

    #[test]
    fn parse_mbid_cases() {
        let id = "a8b4c3d2-1e0f-4a9b-8c7d-6e5f4a3b2c1d";
        assert_eq!(parse_mbid(id).unwrap().to_string(), id);
        assert_eq!(
            parse_mbid(&format!(" {} ", id.to_uppercase()))
                .unwrap()
                .to_string(),
            id
        );
        assert_eq!(parse_mbid(&format!("{{{id}}}")), None);
        assert_eq!(parse_mbid(&format!("urn:uuid:{id}")), None);
        assert_eq!(parse_mbid(&id.replace('-', "")), None);
        assert_eq!(parse_mbid("00000000-0000-0000-0000-000000000000"), None);
        assert_eq!(parse_mbid("a8b4c3d2-1e0f-4a9b-8c7d-6e5f4a3b2c1g"), None);
        assert_eq!(parse_mbid("a8b4c3d21-e0f-4a9b-8c7d-6e5f4a3b2c1d"), None);
        assert_eq!(parse_mbid(""), None);
    }

    #[test]
    fn normalize_isrc_cases() {
        assert_eq!(
            normalize_isrc(" us-rc1 76-07839 , x").as_deref(),
            Some("USRC17607839")
        );
        assert_eq!(
            normalize_isrc("GB-AYE-06-01498;US").as_deref(),
            Some("GBAYE0601498")
        );
        assert_eq!(normalize_isrc("1BAYE0601498"), None);
        assert_eq!(normalize_isrc("GBAYE060149X"), None);
        assert_eq!(normalize_isrc("GBAYE06014988"), None);
    }

    #[test]
    fn looks_synced_cases() {
        assert!(looks_synced("\n  [00:01.00] a"));
        assert!(looks_synced("[12:34]"));
        assert!(!looks_synced("[ar:Artist]\n[00:01.00] a"));
        assert!(!looks_synced("plain [00:01]"));
        assert!(!looks_synced(""));
    }

    #[test]
    fn year_of_cases() {
        assert_eq!(year_of("1997"), Some(1997));
        assert_eq!(year_of("1997-05-21"), Some(1997));
        // Accepted limitation: non-ISO dates (month first) are not parsed.
        assert_eq!(year_of("05/21/1997"), None);
        assert_eq!(year_of(""), None);
        assert_eq!(year_of("abcd"), None);
        assert_eq!(year_of("0000"), None);
    }
}
