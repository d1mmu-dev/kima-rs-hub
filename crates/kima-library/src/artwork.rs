//! Album artwork: find the cover (embedded first, then a folder image), resize it and cache it.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageReader};
use lofty::file::TaggedFileExt;
use lofty::picture::PictureType;
use lofty::probe::Probe;

#[derive(Debug, thiserror::Error)]
pub enum ArtworkError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
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

/// Embedded art first (preferring the front cover), then `cover.*`, `folder.*`, `front.*`
/// (jpg, jpeg, png, webp; case-insensitive) next to the track.
pub fn find_cover(track_path: &Path) -> Option<CoverSource> {
    embedded(track_path)
        .map(CoverSource::Embedded)
        .or_else(|| folder_image(track_path).map(CoverSource::File))
}

fn embedded(track_path: &Path) -> Option<Vec<u8>> {
    let tagged = Probe::open(track_path).ok()?.read().ok()?;
    let mut fallback = None;
    for pic in tagged.tags().iter().flat_map(|t| t.pictures()) {
        if pic.data().is_empty() {
            continue;
        }
        if pic.pic_type() == PictureType::CoverFront {
            return Some(pic.data().to_vec());
        }
        fallback.get_or_insert(pic.data());
    }
    fallback.map(<[u8]>::to_vec)
}

fn folder_image(track_path: &Path) -> Option<PathBuf> {
    let dir = track_path.parent()?;
    let mut best: Option<(usize, usize, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
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
        // Deterministic pick: name priority, then extension priority.
        if best
            .as_ref()
            .is_none_or(|(n, e, _)| (name_rank, ext_rank) < (*n, *e))
        {
            best = Some((name_rank, ext_rank, path));
        }
    }
    best.map(|(_, _, p)| p)
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

fn load(source: &CoverSource) -> Result<DynamicImage, ArtworkError> {
    let bytes = match source {
        CoverSource::Embedded(bytes) => bytes.clone(),
        CoverSource::File(path) => std::fs::read(path)?,
    };
    Ok(ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .decode()?)
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
    let mut img = load(source)?;
    if let Some(size) = size {
        img = resize(img, size);
    }
    // JPEG has no alpha channel.
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 85).encode_image(&rgb)?;
    let dir = out.parent().expect("cache path has a parent");
    std::fs::create_dir_all(dir)?;
    // Write then rename so concurrent requests never serve a half-written file.
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        out.file_name().and_then(|n| n.to_str()).unwrap_or("img"),
        std::process::id()
    ));
    std::fs::write(&tmp, &jpeg)?;
    std::fs::rename(&tmp, &out)?;
    Ok(out)
}
