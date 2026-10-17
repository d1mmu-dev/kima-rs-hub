//! Tag reading via lofty. Strings are trimmed; empty values become `None`.

use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag};
use std::path::Path;

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
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
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
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::year_of;

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
