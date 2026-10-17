mod common;

use common::{Tags, embed_cover, make_audio, png_bytes, require_ffmpeg};
use kima_library::artwork::{self, CoverSource};

#[test]
fn embedded_beats_folder_image() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let track = make_audio(dir.path(), "a/01.mp3", Tags::full());
    std::fs::write(
        dir.path().join("a/cover.png"),
        png_bytes(10, 10, [0, 255, 0]),
    )
    .unwrap();
    let art = png_bytes(20, 20, [255, 0, 0]);
    embed_cover(&track, art.clone());
    match artwork::find_cover(&track) {
        Some(CoverSource::Embedded(bytes)) => assert_eq!(bytes, art),
        other => panic!("expected embedded art, got {other:?}"),
    }
}

#[test]
fn folder_image_case_insensitive() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let track = make_audio(dir.path(), "a/01.mp3", Tags::full());
    std::fs::write(
        dir.path().join("a/Folder.JPG"),
        png_bytes(10, 10, [0, 0, 255]),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("a/notes.png"),
        png_bytes(10, 10, [0, 0, 255]),
    )
    .unwrap();
    assert_eq!(
        artwork::find_cover(&track),
        Some(CoverSource::File(dir.path().join("a/Folder.JPG")))
    );
}

#[test]
fn no_art_is_none() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let track = make_audio(dir.path(), "a/01.mp3", Tags::full());
    std::fs::write(
        dir.path().join("a/random.png"),
        png_bytes(10, 10, [1, 2, 3]),
    )
    .unwrap();
    assert_eq!(artwork::find_cover(&track), None);
}

#[test]
fn resize_fits_box() {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(1000, 500));
    let out = artwork::resize(img, 300);
    assert_eq!((out.width(), out.height()), (300, 150));
    let small = image::DynamicImage::ImageRgb8(image::RgbImage::new(100, 50));
    let out = artwork::resize(small, 300);
    assert_eq!((out.width(), out.height()), (100, 50));
}

#[test]
fn cached_second_call_does_not_reencode() {
    let dir = tempfile::tempdir().unwrap();
    let src = CoverSource::Embedded(png_bytes(1000, 500, [9, 9, 9]));
    let first = artwork::cached(dir.path(), "album1", Some(300), &src).unwrap();
    assert!(first.ends_with("artwork/album1/300.jpg"));
    let img = image::open(&first).unwrap();
    assert_eq!((img.width(), img.height()), (300, 150));
    let m1 = std::fs::metadata(&first).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let second = artwork::cached(dir.path(), "album1", Some(300), &src).unwrap();
    assert_eq!(first, second);
    assert_eq!(m1, std::fs::metadata(&second).unwrap().modified().unwrap());
    let orig = artwork::cached(dir.path(), "album1", None, &src).unwrap();
    assert!(orig.ends_with("artwork/album1/orig.jpg"));
}

#[test]
fn corrupt_embedded_falls_back_to_folder_image() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let track = make_audio(dir.path(), "a/01.mp3", Tags::full());
    embed_cover(&track, b"definitely not an image".to_vec());
    std::fs::write(
        dir.path().join("a/cover.png"),
        png_bytes(400, 200, [0, 255, 0]),
    )
    .unwrap();
    let cands = artwork::candidates(&track);
    assert_eq!(cands.len(), 2);
    let data = tempfile::tempdir().unwrap();
    let (file, failed) = artwork::cached_from(data.path(), "alb", Some(128), cands).unwrap();
    assert_eq!(failed, 1);
    let img = image::open(file.unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (128, 64));
}

#[test]
fn all_corrupt_is_404() {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let track = make_audio(dir.path(), "a/01.mp3", Tags::full());
    embed_cover(&track, b"junk".to_vec());
    std::fs::write(dir.path().join("a/folder.jpg"), b"also junk").unwrap();
    let data = tempfile::tempdir().unwrap();
    let (file, failed) =
        artwork::cached_from(data.path(), "alb", None, artwork::candidates(&track)).unwrap();
    assert!(file.is_none());
    assert_eq!(failed, 2);
    assert!(!data.path().join("artwork/alb/orig.jpg").exists());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A header-only PNG that claims 100000 x 100000 pixels.
fn huge_png() -> Vec<u8> {
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend(100_000u32.to_be_bytes());
    ihdr.extend(100_000u32.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend(13u32.to_be_bytes());
    out.extend(&ihdr);
    out.extend(crc32(&ihdr).to_be_bytes());
    out
}

#[test]
fn oversized_dimensions_are_rejected() {
    let src = CoverSource::Embedded(huge_png());
    assert!(artwork::load(&src).is_err());
    let data = tempfile::tempdir().unwrap();
    let (file, failed) = artwork::cached_from(data.path(), "alb", None, [src]).unwrap();
    assert!(file.is_none());
    assert_eq!(failed, 1);
    let limits = artwork::limits();
    assert_eq!(limits.max_image_width, Some(16384));
    assert_eq!(limits.max_image_height, Some(16384));
    assert_eq!(limits.max_alloc, Some(256 * 1024 * 1024));
}

#[test]
fn oversized_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cover.png");
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(artwork::MAX_SOURCE_BYTES + 1).unwrap();
    assert!(artwork::load(&CoverSource::File(path)).is_err());
}

#[test]
fn sizes_snap_to_buckets() {
    let got: Vec<u32> = [1, 64, 65, 300, 400, 512, 513, 1024, 5000]
        .into_iter()
        .map(artwork::bucket)
        .collect();
    assert_eq!(got, [64, 64, 128, 512, 512, 512, 1024, 1024, 1024]);
}

#[test]
fn concurrent_cached_calls_all_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let src = CoverSource::Embedded(png_bytes(1000, 500, [9, 9, 9]));
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    barrier.wait();
                    artwork::cached(dir.path(), "album1", Some(256), &src).unwrap()
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
    let img = image::open(dir.path().join("artwork/album1/256.jpg")).unwrap();
    assert_eq!((img.width(), img.height()), (256, 128));
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("artwork/album1"))
        .unwrap()
        .collect();
    assert_eq!(leftovers.len(), 1, "no stray temp files");
}
