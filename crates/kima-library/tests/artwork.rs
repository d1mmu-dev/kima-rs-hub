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

fn set_mtime(path: &std::path::Path, secs_from_now: i64) {
    let now = std::time::SystemTime::now();
    let when = if secs_from_now >= 0 {
        now + std::time::Duration::from_secs(secs_from_now as u64)
    } else {
        now - std::time::Duration::from_secs(secs_from_now.unsigned_abs())
    };
    std::fs::File::open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// An album cache entry of `bytes` bytes whose stamp is `age_secs` old.
fn entry(data: &std::path::Path, album: uuid::Uuid, bytes: usize, age_secs: i64) {
    let dir = data.join("artwork").join(album.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("orig.jpg"), vec![0u8; bytes]).unwrap();
    std::fs::write(dir.join("source.json"), b"{}").unwrap();
    set_mtime(&dir.join("source.json"), -age_secs);
}

#[test]
fn evict_to_removes_oldest_albums() {
    let data = tempfile::tempdir().unwrap();
    let (old, mid, new) = (
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
    );
    entry(data.path(), mid, 1000, 200);
    entry(data.path(), old, 1000, 500);
    entry(data.path(), new, 1000, 10);
    // Each entry is 1002 bytes; a cap of 2100 fits two.
    let removed = artwork::evict_to(data.path(), 2100).unwrap();
    assert_eq!(removed, 1002);
    let art = data.path().join("artwork");
    assert!(!art.join(old.to_string()).exists());
    assert!(art.join(mid.to_string()).exists());
    assert!(art.join(new.to_string()).exists());
    assert_eq!(artwork::evict_to(data.path(), 2100).unwrap(), 0);
}

#[test]
fn evict_and_remove_never_leave_the_cache() {
    let data = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious.txt"), b"keep").unwrap();
    let art = data.path().join("artwork");
    std::fs::create_dir_all(&art).unwrap();
    // A symlinked "album", a non-UUID folder and a stray file are all left alone by eviction.
    let linked = uuid::Uuid::now_v7();
    std::os::unix::fs::symlink(outside.path(), art.join(linked.to_string())).unwrap();
    std::fs::create_dir_all(art.join("..sneaky")).unwrap();
    std::fs::write(art.join("..sneaky/x"), vec![0u8; 5000]).unwrap();
    std::fs::write(art.join("loose.bin"), vec![0u8; 5000]).unwrap();
    artwork::evict_to(data.path(), 0).unwrap();
    assert!(outside.path().join("precious.txt").exists());
    assert!(art.join("..sneaky/x").exists());
    assert!(art.join("loose.bin").exists());
    assert!(art.join(linked.to_string()).exists());
    // remove_album unlinks the symlink itself and nothing behind it.
    artwork::remove_album(data.path(), linked).unwrap();
    assert!(!art.join(linked.to_string()).exists());
    assert!(outside.path().join("precious.txt").exists());
    // Removing an album that has no folder is fine.
    artwork::remove_album(data.path(), uuid::Uuid::now_v7()).unwrap();
}

#[test]
fn stamp_goes_stale_when_source_or_folder_changes() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("a")).unwrap();
    let cover = root.path().join("a/cover.png");
    std::fs::write(&cover, png_bytes(10, 10, [1, 2, 3])).unwrap();
    let entry_dir = data.path().join("artwork/e");
    assert!(
        !artwork::stamp_is_fresh(root.path(), &entry_dir),
        "no stamp"
    );

    let stamp = artwork::SourceStamp::capture(root.path(), &cover).unwrap();
    assert_eq!(stamp.source_rel, "a/cover.png");
    artwork::write_stamp(&entry_dir, &stamp).unwrap();
    assert!(artwork::stamp_is_fresh(root.path(), &entry_dir));

    set_mtime(&root.path().join("a"), 30);
    assert!(
        !artwork::stamp_is_fresh(root.path(), &entry_dir),
        "folder changed"
    );
    artwork::write_stamp(
        &entry_dir,
        &artwork::SourceStamp::capture(root.path(), &cover).unwrap(),
    )
    .unwrap();
    assert!(artwork::stamp_is_fresh(root.path(), &entry_dir));
    set_mtime(&cover, 60);
    assert!(
        !artwork::stamp_is_fresh(root.path(), &entry_dir),
        "source mtime changed"
    );

    // A stamp pointing out of the root, or at a missing file, is stale.
    std::fs::write(
        entry_dir.join("source.json"),
        br#"{"source_rel":"../x","size":1,"mtime_ns":1,"folder_mtime_ns":1}"#,
    )
    .unwrap();
    assert!(!artwork::stamp_is_fresh(root.path(), &entry_dir));
    std::fs::write(entry_dir.join("source.json"), b"garbage").unwrap();
    assert!(!artwork::stamp_is_fresh(root.path(), &entry_dir));
}

#[test]
fn reembedded_art_makes_the_stamp_stale() {
    if !require_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let track = make_audio(root.path(), "a/01.mp3", Tags::full());
    embed_cover(&track, png_bytes(20, 20, [255, 0, 0]));
    let (_, origin) = artwork::candidates_with_origin(&track)
        .into_iter()
        .next()
        .expect("embedded candidate");
    assert_eq!(origin, track, "embedded art names the track as its source");
    let entry_dir = data.path().join("artwork/e");
    artwork::write_stamp(
        &entry_dir,
        &artwork::SourceStamp::capture(root.path(), &origin).unwrap(),
    )
    .unwrap();
    assert!(artwork::stamp_is_fresh(root.path(), &entry_dir));
    embed_cover(&track, png_bytes(40, 40, [0, 255, 0]));
    set_mtime(&track, 90);
    assert!(!artwork::stamp_is_fresh(root.path(), &entry_dir));
}
