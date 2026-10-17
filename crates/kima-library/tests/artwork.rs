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
