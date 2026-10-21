//! Shared test helper: generates real audio files with ffmpeg.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Default)]
pub struct Tags {
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub date: Option<String>,
    pub disc: Option<String>,
    pub track: Option<String>,
    pub compilation: bool,
}

impl Tags {
    pub fn full() -> Self {
        Tags {
            album_artist: Some("The Album Artist".into()),
            artist: Some("The Track Artist".into()),
            album: Some("The Album".into()),
            title: Some("The Title".into()),
            date: Some("1997-05-21".into()),
            disc: Some("2".into()),
            track: Some("5".into()),
            compilation: true,
        }
    }
}

#[allow(unused_imports)]
pub use kima_test_support::require_ffmpeg;

/// Generates a 2 s sine tone at `dir/rel_path`; the format comes from the extension.
pub fn make_audio(dir: &Path, rel_path: &str, tags: Tags) -> PathBuf {
    let out = dir.join(rel_path);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("");
    let codec: &[&str] = match ext {
        "mp3" => &["-c:a", "libmp3lame"],
        "flac" => &["-c:a", "flac"],
        "ogg" => &["-c:a", "libvorbis"],
        "opus" => &["-c:a", "libopus"],
        "m4a" => &["-c:a", "aac"],
        "wav" => &["-c:a", "pcm_s16le"],
        other => panic!("unsupported fixture extension {other}"),
    };
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=duration=2"])
        .args(codec);
    for (key, val) in [
        ("album_artist", &tags.album_artist),
        ("artist", &tags.artist),
        ("album", &tags.album),
        ("title", &tags.title),
        ("date", &tags.date),
        ("disc", &tags.disc),
        ("track", &tags.track),
    ] {
        if let Some(v) = val {
            cmd.arg("-metadata").arg(format!("{key}={v}"));
        }
    }
    if tags.compilation {
        cmd.args(["-metadata", "compilation=1"]);
    }
    let status = cmd
        .arg(&out)
        .stdout(Stdio::null())
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed for {rel_path}");
    out
}

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
