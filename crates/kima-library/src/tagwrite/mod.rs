//! Tag writing (spec §19.8): Picard-compatible native tags, written into a hidden copy, checked
//! and renamed over the original, with an audio-packet hash proving the audio is unchanged.
//!
//! Layout: [`values`] holds the container-independent field values; each container has a module
//! with four functions, dispatched by the `match`es below:
//! - `native(…, &TagValues) -> NativeTags`: the §19.8 key names and their new values;
//! - `read(path, keys) -> NativeTags`: the current values of those keys (`[]` = absent);
//! - `write(path, …, &NativeTags)`: replaces exactly those keys in place (the safe write calls it
//!   on the hidden copy only);
//! - `untouched(path, keys)`: one digest per frame/field the write must not change, so the safe
//!   write can prove every other frame survived.
//!
//! MP3 (ID3v2) is [`id3`], FLAC/Ogg/Opus (Vorbis comments) [`vorbis`], M4A/MP4 (`ilst`)
//! [`mp4`]. Any other container is [`TagWriteError::Unsupported`].
//!
//! Everything here is blocking (file I/O, ffmpeg); call it from `spawn_blocking`.

mod audio;
mod id3;
mod mp4;
mod safe;
mod values;
mod vorbis;

use std::collections::BTreeMap;
use std::path::{Component, Path};

use lofty::file::FileType;
use lofty::probe::Probe;

pub use audio::audio_md5;
pub use safe::{WriteHooks, Written, copy_path, safe_write, safe_write_expecting};
pub use values::{NumberPair, TagValues};

/// Native key (spelled as in the §19.8 table: `TIT2`, `TXXX:MusicBrainz Album Id`,
/// `UFID:http://musicbrainz.org`, …) → its values; `[]` means absent.
pub type NativeTags = BTreeMap<String, Vec<String>>;

/// The containers kima-rs can write tags into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp3,
    Flac,
    Ogg,
    Opus,
    Mp4,
}

/// The ID3v2 version a file is saved with: a v2.3 file stays v2.3 (R4), everything else is v2.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id3Version {
    V3,
    V4,
}

impl Id3Version {
    /// The `autotag_journal.id3_version` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Id3Version::V3 => "v3",
            Id3Version::V4 => "v4",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "v3" => Some(Id3Version::V3),
            "v4" => Some(Id3Version::V4),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TagWriteError {
    /// The extension, lower-cased (`wma`).
    #[error("Tag writing is not supported for .{0} files")]
    Unsupported(String),
    #[error("{}", crate::organize::apply::SYMLINK_NOTE)]
    Symlink,
    #[error("the tag writer changed the audio; nothing written")]
    AudioChanged,
    /// The file no longer holds the content the apply journalled (spec §19.9); nothing written.
    #[error("changed outside kima-rs")]
    ChangedOutside,
    /// A check refused the write (the message says which, and names the keys or frames);
    /// nothing was renamed.
    #[error("{0}")]
    Verify(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("tag library error: {0}")]
    Lofty(String),
}

impl From<std::io::Error> for TagWriteError {
    fn from(e: std::io::Error) -> Self {
        TagWriteError::Io(e.to_string())
    }
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The container by lofty's probe (extension, then content). Any other format, or a file lofty
/// cannot identify, is `Unsupported(<extension>)`.
pub fn container_of(path: &Path) -> Result<Container, TagWriteError> {
    let probe = Probe::open(path)?
        .guess_file_type()
        .map_err(TagWriteError::from)?;
    match probe.file_type() {
        Some(FileType::Mpeg) => Ok(Container::Mp3),
        Some(FileType::Flac) => Ok(Container::Flac),
        Some(FileType::Vorbis) => Ok(Container::Ogg),
        Some(FileType::Opus) => Ok(Container::Opus),
        Some(FileType::Mp4) => Ok(Container::Mp4),
        _ => Err(TagWriteError::Unsupported(ext_of(path))),
    }
}

/// The ID3v2 version an MP3 is written with: `Some(V3)` for a v2.3 tag, `Some(V4)` for v2.4 (and
/// v2.2, which lofty upgrades); `None` for a file that is not an MP3 or has no ID3v2 tag (it
/// gets a v2.4 one).
pub fn id3_version(path: &Path) -> Result<Option<Id3Version>, TagWriteError> {
    match container_of(path) {
        Ok(Container::Mp3) => id3::version(path),
        Ok(_) | Err(TagWriteError::Unsupported(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The native keys and new values of `v` for this container. Only the `Some` fields of `v`
/// appear; a key with `[]` is removed by the write. The keys written only when a file already
/// has them (Vorbis `TOTALTRACKS`/`TOTALDISCS`) are added by [`native_for_file`].
pub(crate) fn native(container: Container, id3: Option<Id3Version>, v: &TagValues) -> NativeTags {
    match container {
        Container::Mp3 => id3::native(id3.unwrap_or(Id3Version::V4), v),
        Container::Flac | Container::Ogg | Container::Opus => vorbis::native(v),
        Container::Mp4 => mp4::native(v),
    }
}

/// [`native`] for the file at `path`: a Vorbis file that already has `TOTALTRACKS` or
/// `TOTALDISCS` gets them too, with the values of `TRACKTOTAL`/`DISCTOTAL` (§19.8). What the
/// apply job journals and writes. Blocking.
pub fn native_for_file(
    path: &Path,
    container: Container,
    id3: Option<Id3Version>,
    v: &TagValues,
) -> Result<NativeTags, TagWriteError> {
    let mut tags = native(container, id3, v);
    if matches!(
        container,
        Container::Flac | Container::Ogg | Container::Opus
    ) {
        vorbis::add_present_totals(path, container, &mut tags)?;
    }
    Ok(tags)
}

/// The current values of `keys` in the file (`[]` for an absent key).
pub fn read_native(
    path: &Path,
    container: Container,
    keys: &[String],
) -> Result<NativeTags, TagWriteError> {
    match container {
        Container::Mp3 => id3::read(path, keys),
        Container::Flac | Container::Ogg | Container::Opus => vorbis::read(path, container, keys),
        Container::Mp4 => mp4::read(path, keys),
    }
}

/// Replaces exactly the keys of `new` in the file at `path`, in place. Only ever called on the
/// safe write's hidden copy.
fn write_native(
    path: &Path,
    container: Container,
    id3: Option<Id3Version>,
    new: &NativeTags,
) -> Result<(), TagWriteError> {
    match container {
        Container::Mp3 => id3::write(path, id3.unwrap_or(Id3Version::V4), new),
        Container::Flac | Container::Ogg | Container::Opus => vorbis::write(path, container, new),
        Container::Mp4 => mp4::write(path, new),
    }
}

/// The container's own check of the written copy, beyond the preservation checks: an Ogg file's
/// pages (numbering and checksums, which lofty rewrites).
fn check_written(path: &Path, container: Container) -> Result<(), TagWriteError> {
    match container {
        Container::Ogg | Container::Opus => vorbis::check_pages(&std::fs::read(path)?),
        Container::Mp3 | Container::Flac | Container::Mp4 => Ok(()),
    }
}

/// One `(label, digest)` per frame, comment or atom that is not one of `keys` (sorted), so a
/// write that lost or changed any of them is caught before the rename and named.
fn untouched(
    path: &Path,
    container: Container,
    id3: Option<Id3Version>,
    keys: &[String],
) -> Result<Vec<(String, [u8; 32])>, TagWriteError> {
    match container {
        Container::Mp3 => id3::untouched(path, id3.unwrap_or(Id3Version::V4), keys),
        Container::Flac | Container::Ogg | Container::Opus => {
            vorbis::untouched(path, container, keys)
        }
        Container::Mp4 => mp4::untouched(path, keys),
    }
}

/// The frames/fields as stored on disk (not as the tag library parsed them), the ones `keys`
/// own left out, with their counts: catches what the library loses while reading (unparseable
/// or merged duplicate frames), which [`untouched`] cannot see.
fn raw_untouched(
    path: &Path,
    container: Container,
    id3: Option<Id3Version>,
    keys: &[String],
) -> Result<BTreeMap<String, usize>, TagWriteError> {
    match container {
        Container::Mp3 => id3::raw_untouched(path, id3.unwrap_or(Id3Version::V4), keys),
        Container::Flac | Container::Ogg | Container::Opus => {
            vorbis::raw_untouched(path, container, keys)
        }
        Container::Mp4 => mp4::raw_untouched(path, keys),
    }
}

/// The refusal when the written copy's raw frames differ from the original's.
fn loss_message(
    container: Container,
    id3: Option<Id3Version>,
    before: &BTreeMap<String, usize>,
    after: &BTreeMap<String, usize>,
) -> String {
    let mut diffs = Vec::new();
    let mut lost = Vec::new();
    for key in before
        .keys()
        .chain(after.keys().filter(|k| !before.contains_key(*k)))
    {
        let (b, a) = (
            before.get(key).copied().unwrap_or(0),
            after.get(key).copied().unwrap_or(0),
        );
        if a != b {
            diffs.push(format!("{key} ({b} → {a})"));
            if a == 0 {
                lost.push(key.as_str());
            }
        }
    }
    if container == Container::Mp3
        && id3 == Some(Id3Version::V3)
        && lost.len() == diffs.len()
        && lost.iter().all(|k| id3::V23_DROPPED.contains(k))
    {
        return format!(
            "ID3v2.3 cannot keep {} with this writer; nothing written",
            lost.join(", ")
        );
    }
    format!(
        "these {} would be lost or changed: {}; nothing written",
        noun(container),
        diffs.join(", ")
    )
}

/// What the preservation checks name: ID3v2 frames, Vorbis comment fields, MP4 atoms.
fn noun(container: Container) -> &'static str {
    match container {
        Container::Mp3 => "frames",
        Container::Flac | Container::Ogg | Container::Opus => "fields",
        Container::Mp4 => "atoms",
    }
}

/// Refuses a file reached through a symbolic link (D-123): the file at `rel` under `root`, or
/// any folder between them, is a link. `rel` must be plain relative components (the M1 rule).
/// Blocking.
pub fn refuse_symlink(root: &Path, rel: &str) -> Result<(), TagWriteError> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(TagWriteError::Io(format!("unsafe path: {rel}")));
    }
    if crate::scan::symlinked(root, rel) {
        return Err(TagWriteError::Symlink);
    }
    Ok(())
}

impl From<lofty::error::FileParseError> for TagWriteError {
    fn from(e: lofty::error::FileParseError) -> Self {
        TagWriteError::Lofty(e.to_string())
    }
}

impl From<lofty::error::FileEncodingError> for TagWriteError {
    fn from(e: lofty::error::FileEncodingError) -> Self {
        TagWriteError::Lofty(e.to_string())
    }
}
