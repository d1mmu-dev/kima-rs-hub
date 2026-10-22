//! Shared test helper: generates real audio files with ffmpeg.
#![allow(dead_code)]

use std::path::Path;

#[allow(unused_imports)]
pub use kima_test_support::{Picard, Tags, make_audio, require_ffmpeg, tag_picard};

/// A solid-colour PNG of the given size.
pub fn png_bytes(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb(rgb));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Embeds `png` as the front cover of the audio file at `path`.
pub fn embed_cover(path: &Path, png: Vec<u8>) {
    use lofty::config::WriteOptions;
    use lofty::file::TaggedFileExt;
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::prelude::*;
    use lofty::probe::Probe;
    let mut tagged = Probe::open(path).unwrap().read().unwrap();
    let tag = tagged.primary_tag_mut().expect("fixture has a tag");
    tag.push_picture(
        Picture::unchecked(png)
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .build(),
    );
    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

/// A PNG of the given size whose pixels vary, so it does not compress below 1000 bytes.
pub fn noisy_png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let v = x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503));
        image::Rgb([(v >> 3) as u8, (v >> 11) as u8, (v >> 19) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}
