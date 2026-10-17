//! Album artwork: find the cover (embedded first, then a folder image), resize it and cache it.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, Limits};
use lofty::file::TaggedFileExt;
use lofty::picture::PictureType;
use lofty::probe::Probe;

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
