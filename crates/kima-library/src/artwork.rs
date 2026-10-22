//! Album artwork: find the cover (embedded first, then a folder image), resize it and cache it.

use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, Limits};
use lofty::file::TaggedFileExt;
use lofty::picture::PictureType;
use lofty::probe::Probe;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ArtworkError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("source image is too large")]
    TooLarge,
}

/// Where a cover came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverSource {
    /// Raw bytes of a picture embedded in the audio file's tags.
    Embedded(Vec<u8>),
    /// An image file in the track's folder.
    File(PathBuf),
}

const FOLDER_NAMES: &[&str] = &["cover", "folder", "front"];
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// Largest source (file or embedded picture) we will read.
pub const MAX_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_DIMENSION: u32 = 16384;
const MAX_ALLOC: u64 = 256 * 1024 * 1024;

/// The decode limits applied to every cover.
pub fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC);
    limits
}

/// Requested sizes snap up to one of these, so callers cannot fill the cache with arbitrary sizes.
pub const SIZE_BUCKETS: [u32; 5] = [64, 128, 256, 512, 1024];

/// The bucket for a requested size: the next bucket up, with anything above 1024 mapped to 1024.
pub fn bucket(size: u32) -> u32 {
    SIZE_BUCKETS
        .iter()
        .copied()
        .find(|b| size <= *b)
        .unwrap_or(1024)
}

/// The best candidate: embedded art, else a folder image.
pub fn find_cover(track_path: &Path) -> Option<CoverSource> {
    candidates(track_path).into_iter().next()
}

/// Every cover candidate for a track, best first: embedded art (front cover preferred), then
/// `cover.*`, `folder.*`, `front.*` (jpg, jpeg, png, webp; case-insensitive) next to the track.
/// Oversized sources are left out.
pub fn candidates(track_path: &Path) -> Vec<CoverSource> {
    let mut out: Vec<CoverSource> = embedded(track_path)
        .into_iter()
        .map(CoverSource::Embedded)
        .collect();
    out.extend(folder_images(track_path).into_iter().map(CoverSource::File));
    out
}

/// Like [`candidates`], but pairs each candidate with the file it came from (the track itself
/// for embedded art), so a cache entry can record what it was built from.
pub fn candidates_with_origin(track_path: &Path) -> Vec<(CoverSource, PathBuf)> {
    let mut out: Vec<(CoverSource, PathBuf)> = embedded(track_path)
        .into_iter()
        .map(|bytes| (CoverSource::Embedded(bytes), track_path.to_path_buf()))
        .collect();
    out.extend(
        folder_images(track_path)
            .into_iter()
            .map(|p| (CoverSource::File(p.clone()), p)),
    );
    out
}

fn embedded(track_path: &Path) -> Option<Vec<u8>> {
    let tagged = Probe::open(track_path).ok()?.read().ok()?;
    let mut fallback = None;
    for pic in tagged.tags().iter().flat_map(|t| t.pictures()) {
        if pic.data().is_empty() || pic.data().len() as u64 > MAX_SOURCE_BYTES {
            continue;
        }
        if pic.pic_type() == PictureType::CoverFront {
            return Some(pic.data().to_vec());
        }
        fallback.get_or_insert(pic.data());
    }
    fallback.map(<[u8]>::to_vec)
}

fn folder_images(track_path: &Path) -> Vec<PathBuf> {
    let Some(dir) = track_path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(usize, usize, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || entry.metadata().is_ok_and(|m| m.len() > MAX_SOURCE_BYTES) {
            continue;
        }
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|e| e.to_str()),
        ) else {
            continue;
        };
        let (stem, ext) = (stem.to_ascii_lowercase(), ext.to_ascii_lowercase());
        let (Some(name_rank), Some(ext_rank)) = (
            FOLDER_NAMES.iter().position(|n| *n == stem),
            IMAGE_EXTS.iter().position(|e| *e == ext),
        ) else {
            continue;
        };
        found.push((name_rank, ext_rank, path));
    }
    // Deterministic: name priority, then extension priority, then path.
    found.sort();
    found.into_iter().map(|(_, _, p)| p).collect()
}

/// Fits the image inside `size` x `size`, keeping its aspect ratio. Never upscales.
pub fn resize(img: DynamicImage, size: u32) -> DynamicImage {
    let size = size.max(1);
    if img.width() <= size && img.height() <= size {
        img
    } else {
        img.resize(size, size, FilterType::Lanczos3)
    }
}

/// Decodes a source under the size and decode limits.
pub fn load(source: &CoverSource) -> Result<DynamicImage, ArtworkError> {
    let bytes = match source {
        CoverSource::Embedded(bytes) => bytes.clone(),
        CoverSource::File(path) => {
            let mut buf = Vec::new();
            std::fs::File::open(path)?
                .take(MAX_SOURCE_BYTES + 1)
                .read_to_end(&mut buf)?;
            buf
        }
    };
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(ArtworkError::TooLarge);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits());
    Ok(reader.decode()?)
}

/// Decodes `source`, resizes it and encodes JPEG q85.
fn render(source: &CoverSource, size: Option<u32>) -> Result<Vec<u8>, ArtworkError> {
    let mut img = load(source)?;
    if let Some(size) = size {
        img = resize(img, size);
    }
    // JPEG has no alpha channel.
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 85).encode_image(&rgb)?;
    Ok(jpeg)
}

/// Where the album's cover at `size` lives: `<data_dir>/artwork/<album_id>/<size|orig>.jpg`.
pub fn cache_path(data_dir: &Path, album_id: &str, size: Option<u32>) -> PathBuf {
    let name = size.map_or_else(|| "orig".to_string(), |s| s.to_string());
    data_dir
        .join("artwork")
        .join(album_id)
        .join(format!("{name}.jpg"))
}

/// Returns the cached JPEG (quality 85) for the album at `size`, creating it from `source`
/// if it does not exist yet. An existing cache file is returned untouched. Blocking.
pub fn cached(
    data_dir: &Path,
    album_id: &str,
    size: Option<u32>,
    source: &CoverSource,
) -> Result<PathBuf, ArtworkError> {
    let out = cache_path(data_dir, album_id, size);
    if out.is_file() {
        return Ok(out);
    }
    store(&out, &render(source, size)?)?;
    Ok(out)
}

/// Like [`cached`], but tries each candidate in turn and skips ones that cannot be decoded.
/// Returns the cache file (if any candidate worked) and how many candidates failed.
pub fn cached_from(
    data_dir: &Path,
    album_id: &str,
    size: Option<u32>,
    candidates: impl IntoIterator<Item = CoverSource>,
) -> Result<(Option<PathBuf>, usize), ArtworkError> {
    let out = cache_path(data_dir, album_id, size);
    if out.is_file() {
        return Ok((Some(out), 0));
    }
    let mut failed = 0;
    for source in candidates {
        match render(&source, size) {
            Ok(jpeg) => {
                store(&out, &jpeg)?;
                return Ok((Some(out), failed));
            }
            Err(_) => failed += 1,
        }
    }
    Ok((None, failed))
}

/// Writes through a uniquely named temp file in the same directory, then renames it into
/// place, so concurrent callers never see or clobber a partial file. The temp file is removed
/// on error.
fn store(out: &Path, jpeg: &[u8]) -> Result<(), ArtworkError> {
    let dir = out.parent().expect("cache path has a parent");
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(jpeg)?;
    tmp.persist(out).map_err(|e| e.error)?;
    Ok(())
}

const STAMP_FILE: &str = "source.json";

/// An entry folder with no `source.json` younger than this is a build in progress; eviction
/// leaves it alone.
const IN_FLIGHT_GRACE: std::time::Duration = std::time::Duration::from_secs(120);

/// What a cache entry was built from. Stored as `<entry>/source.json`; the entry is stale
/// once the source or the source's folder no longer matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStamp {
    /// The source file, relative to the library root (the track file for embedded art).
    pub source_rel: String,
    pub size: u64,
    pub mtime_ns: u128,
    /// The mtime of the source's folder; it changes when a `cover.*` is added or renamed.
    pub folder_mtime_ns: u128,
}

fn mtime_ns(meta: &std::fs::Metadata) -> std::io::Result<u128> {
    Ok(meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos())
}

fn safe_rel(rel: &Path) -> bool {
    !rel.as_os_str().is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)))
}

impl SourceStamp {
    /// Stamps `source` (an absolute path under `root`) as it is now.
    pub fn capture(root: &Path, source: &Path) -> std::io::Result<Self> {
        let rel = source
            .strip_prefix(root)
            .ok()
            .filter(|r| safe_rel(r))
            .ok_or_else(|| std::io::Error::other("cover source is outside the library root"))?;
        let source_rel = rel
            .to_str()
            .ok_or_else(|| std::io::Error::other("cover source path is not valid UTF-8"))?
            .to_owned();
        let meta = std::fs::metadata(source)?;
        let folder = std::fs::metadata(source.parent().unwrap_or(root))?;
        Ok(Self {
            source_rel,
            size: meta.len(),
            mtime_ns: mtime_ns(&meta)?,
            folder_mtime_ns: mtime_ns(&folder)?,
        })
    }
}

/// Writes the entry's `source.json`.
pub fn write_stamp(entry_dir: &Path, stamp: &SourceStamp) -> std::io::Result<()> {
    let json = serde_json::to_vec(stamp).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(entry_dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(entry_dir)?;
    tmp.write_all(&json)?;
    tmp.persist(entry_dir.join(STAMP_FILE))
        .map_err(|e| e.error)?;
    Ok(())
}

/// True when the entry's source and its folder still have the stamped size and mtimes.
/// A missing or unreadable stamp counts as stale.
pub fn stamp_is_fresh(root: &Path, entry_dir: &Path) -> bool {
    fn check(root: &Path, entry_dir: &Path) -> Option<bool> {
        let raw = std::fs::read(entry_dir.join(STAMP_FILE)).ok()?;
        let stamp: SourceStamp = serde_json::from_slice(&raw).ok()?;
        let rel = Path::new(&stamp.source_rel);
        if !safe_rel(rel) {
            return None;
        }
        let now = SourceStamp::capture(root, &root.join(rel)).ok()?;
        Some(now == stamp)
    }
    check(root, entry_dir).unwrap_or(false)
}

/// A cache hit refreshes the stamp's mtime, which is what eviction orders by.
pub fn touch_stamp(entry_dir: &Path) {
    let touched = std::fs::File::options()
        .write(true)
        .open(entry_dir.join(STAMP_FILE))
        .and_then(|f| f.set_modified(SystemTime::now()));
    if let Err(e) = touched {
        tracing::debug!(error = %e, "could not refresh the cover cache stamp");
    }
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Deletes the album's cache folder. A symlink in its place is unlinked, never followed.
pub fn remove_album(data_dir: &Path, album_id: Uuid) -> std::io::Result<()> {
    let entry = data_dir.join("artwork").join(album_id.to_string());
    let result = match std::fs::symlink_metadata(&entry) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(&entry),
        Ok(_) => std::fs::remove_file(&entry),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Bytes of the regular files directly inside `dir` (symlinks are not followed or counted).
fn entry_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Removes whole album folders, oldest `source.json` mtime first, until the cache holds at
/// most `max_bytes`. Returns the bytes removed. Only real directories named by an album UUID
/// directly under `<data_dir>/artwork` are considered; anything else is left alone.
pub fn evict_to(data_dir: &Path, max_bytes: u64) -> std::io::Result<u64> {
    let root = data_dir.join("artwork");
    let reader = match std::fs::read_dir(&root) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    for entry in reader.flatten() {
        let path = entry.path();
        let named_by_album = entry
            .file_name()
            .to_str()
            .is_some_and(|n| Uuid::try_parse(n).is_ok());
        if !named_by_album || !is_real_dir(&path) {
            continue;
        }
        let size = entry_size(&path);
        total += size;
        let stamped = match std::fs::metadata(path.join(STAMP_FILE)).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => {
                // No stamp yet: a build in flight, unless the folder has sat there a while.
                let young = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_none_or(|age| age < IN_FLIGHT_GRACE);
                if young {
                    continue;
                }
                UNIX_EPOCH
            }
        };
        entries.push((stamped, size, path));
    }
    entries.sort();
    let mut removed = 0;
    for (_, size, path) in entries {
        if total <= max_bytes {
            break;
        }
        // Re-checked at delete time: the entry could have been swapped for a symlink.
        if !is_real_dir(&path) {
            continue;
        }
        // Gone already (a concurrent removal) counts as removed.
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        total = total.saturating_sub(size);
        removed += size;
    }
    Ok(removed)
}

// ---- artist images (spec §12.3, §12.6) ----

/// The folder under the data directory that holds downloaded and copied artist images. Not
/// part of the evictable cover cache: these cannot be rebuilt without the network.
pub const ARTIST_IMAGES_DIR: &str = "artist-images";

/// The long side of a stored artist image, at most.
pub const MAX_STORED_SIDE: u32 = 1500;
/// Quality of a stored artist image.
pub const STORED_QUALITY: u8 = 90;
/// A local artist image must be larger than this (upstream `imageStorage.ts`).
pub const MIN_LOCAL_IMAGE_BYTES: u64 = 1000;
/// The largest local artist image read (the download limit, spec §12.6).
pub const MAX_LOCAL_IMAGE_BYTES: u64 = 15 * 1024 * 1024;
/// Untrusted images: the largest width or height decoded, and the most pixels.
const MAX_UNTRUSTED_SIDE: u32 = 12_000;
const MAX_UNTRUSTED_PIXELS: u64 = 50_000_000;

/// The names a local artist image may have, in order (upstream `imageStorage.ts:136-139`).
const LOCAL_ARTIST_NAMES: [&str; 6] = [
    "artist.jpg",
    "artist.png",
    "artist.webp",
    "folder.jpg",
    "folder.png",
    "folder.webp",
];

/// Where the artist's stored image lives: `<data_dir>/artist-images/<id>.jpg`. The name comes
/// from the id alone, so nothing an answer or a file says can steer it.
pub fn artist_image_path(data_dir: &Path, artist: Uuid) -> PathBuf {
    data_dir
        .join(ARTIST_IMAGES_DIR)
        .join(format!("{}.jpg", artist.hyphenated()))
}

fn untrusted_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_UNTRUSTED_SIDE);
    limits.max_image_height = Some(MAX_UNTRUSTED_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    limits
}

/// Decodes untrusted image bytes and re-encodes them as JPEG q90, at most 1500 px on the long
/// side (spec §12.6), so nothing but a valid image is ever stored. The header's dimensions are
/// checked before any pixel is decoded (decompression bombs), and the decode itself runs under
/// width, height and allocation limits. Blocking.
pub fn normalize_image(bytes: &[u8]) -> Result<Vec<u8>, ArtworkError> {
    if bytes.len() as u64 > MAX_LOCAL_IMAGE_BYTES {
        return Err(ArtworkError::TooLarge);
    }
    let reader = || -> Result<ImageReader<Cursor<&[u8]>>, ArtworkError> {
        let mut r = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
        r.limits(untrusted_limits());
        Ok(r)
    };
    let (w, h) = reader()?.into_dimensions()?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_UNTRUSTED_PIXELS {
        return Err(ArtworkError::TooLarge);
    }
    let mut img = reader()?.decode()?;
    if img.width().max(img.height()) > MAX_STORED_SIDE {
        img = img.resize(MAX_STORED_SIDE, MAX_STORED_SIDE, FilterType::Lanczos3);
    }
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, STORED_QUALITY).encode_image(&rgb)?;
    Ok(jpeg)
}

/// Writes the artist's stored image atomically (a temp file in the same folder, then a
/// rename). The folder is created when missing; a symlink in its place is refused. Blocking.
pub fn store_artist_image(data_dir: &Path, artist: Uuid, jpeg: &[u8]) -> std::io::Result<()> {
    let dir = data_dir.join(ARTIST_IMAGES_DIR);
    std::fs::create_dir_all(&dir)?;
    if !is_real_dir(&dir) {
        return Err(std::io::Error::other(
            "the artist image folder is not a directory",
        ));
    }
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(jpeg)?;
    tmp.as_file().sync_all()?;
    tmp.persist(artist_image_path(data_dir, artist))
        .map_err(|e| e.error)?;
    Ok(())
}

/// Deletes the artist's stored image, and nothing else: only
/// [`artist_image_path`]`(data_dir, artist)`, unlinked (a symlink there is removed, never
/// followed). Gone already is fine. Blocking.
pub fn remove_artist_image(data_dir: &Path, artist: Uuid) -> std::io::Result<()> {
    let path = artist_image_path(data_dir, artist);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A relative path of plain components only: no root, no `..`, no `.`.
fn plain_rel(p: &Path) -> bool {
    p.components().all(|c| matches!(c, Component::Normal(_)))
}

/// A regular file (not a symlink) larger than [`MIN_LOCAL_IMAGE_BYTES`].
fn usable_local_image(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| {
        m.file_type().is_file()
            && m.len() > MIN_LOCAL_IMAGE_BYTES
            && m.len() <= MAX_LOCAL_IMAGE_BYTES
    })
}

/// The artist's image in the library (spec §12.3 step 2, upstream `imageStorage.ts:119-173`):
/// for each album folder (relative to `root`), the folders above it, nearest first, up to but
/// not including the root, each holding `artist.(jpg|png|webp)` or `folder.(jpg|png|webp)`
/// (exact names, in that order) that is a regular file larger than 1000 bytes and not a
/// symlink. A folder that is absolute, holds `..`, or resolves (through a symlink) outside the
/// root is skipped. Blocking.
pub fn find_local_artist_image(root: &Path, album_dirs: &[PathBuf]) -> Option<PathBuf> {
    let root = std::fs::canonicalize(root).ok()?;
    let mut seen = std::collections::HashSet::new();
    for album in album_dirs {
        if album.as_os_str().is_empty() || !plain_rel(album) {
            continue;
        }
        let mut dir = album.parent();
        while let Some(rel) = dir {
            if rel.as_os_str().is_empty() {
                break;
            }
            dir = rel.parent();
            if !seen.insert(rel.to_path_buf()) {
                continue;
            }
            let Ok(real) = std::fs::canonicalize(root.join(rel)) else {
                continue;
            };
            if real == root || !real.starts_with(&root) || !is_real_dir(&real) {
                continue;
            }
            if let Some(found) = LOCAL_ARTIST_NAMES
                .iter()
                .map(|n| real.join(n))
                .find(|p| usable_local_image(p))
            {
                return Some(found);
            }
        }
    }
    None
}

/// Reads a local artist image found by [`find_local_artist_image`], refusing it if it was
/// swapped for a symlink or another file since (the opened file must be the one checked).
/// Blocking.
pub fn read_local_image(path: &Path) -> std::io::Result<Vec<u8>> {
    let before = std::fs::symlink_metadata(path)?;
    if !before.file_type().is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let file = std::fs::File::open(path)?;
    let opened = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.dev(), before.ino()) != (opened.dev(), opened.ino()) {
            return Err(std::io::Error::other(
                "the file changed while it was opened",
            ));
        }
    }
    if !opened.is_file() || opened.len() > MAX_LOCAL_IMAGE_BYTES {
        return Err(std::io::Error::other("not a usable image file"));
    }
    let mut buf = Vec::new();
    file.take(MAX_LOCAL_IMAGE_BYTES + 1).read_to_end(&mut buf)?;
    Ok(buf)
}
