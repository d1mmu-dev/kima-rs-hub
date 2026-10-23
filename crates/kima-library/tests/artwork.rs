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

// ---- artist images (spec §12.3 step 2, §12.6; upstream `imageStorage.test.ts`) ----

mod artist_images {
    use std::path::{Path, PathBuf};

    use super::common::{noisy_png, png_bytes};
    use kima_library::artwork;
    use uuid::Uuid;

    fn put(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn dirs(d: &[&str]) -> Vec<PathBuf> {
        d.iter().map(PathBuf::from).collect()
    }

    /// Port of "returns true for existing native file" / "missing native file": the stored
    /// image's place is derived from the artist id alone.
    #[test]
    fn artist_image_path_is_the_id_under_artist_images() {
        let data = Path::new("/data");
        let id = Uuid::parse_str("0192f0c4-1111-7000-8000-000000000001").unwrap();
        assert_eq!(
            artwork::artist_image_path(data, id),
            PathBuf::from("/data/artist-images/0192f0c4-1111-7000-8000-000000000001.jpg")
        );
    }

    /// Port of "returns true for existing native file": the nearest folder above the album
    /// folders holding `artist.*` or `folder.*` larger than 1000 bytes.
    #[test]
    fn finds_the_nearest_artist_image_above_the_albums() {
        let root = tempfile::tempdir().unwrap();
        let near = put(root.path(), "Pop/Artist/artist.jpg", &noisy_png(64, 64));
        put(root.path(), "Pop/folder.png", &noisy_png(64, 64));
        // The album folder's own folder.jpg is the album's cover, not the artist's.
        put(
            root.path(),
            "Pop/Artist/Album/folder.jpg",
            &noisy_png(64, 64),
        );
        let found = artwork::find_local_artist_image(root.path(), &dirs(&["Pop/Artist/Album"]));
        assert_eq!(
            found
                .as_deref()
                .map(std::fs::canonicalize)
                .map(Result::unwrap),
            Some(std::fs::canonicalize(&near).unwrap())
        );
        // Above that: the genre folder's.
        std::fs::remove_file(&near).unwrap();
        let found = artwork::find_local_artist_image(root.path(), &dirs(&["Pop/Artist/Album"]));
        assert!(found.unwrap().ends_with("Pop/folder.png"));
    }

    /// Port of "returns false for missing native file" and "returns false for null/empty".
    #[test]
    fn no_image_or_a_small_one_is_none() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(artwork::find_local_artist_image(root.path(), &[]), None);
        assert_eq!(
            artwork::find_local_artist_image(root.path(), &dirs(&["A/B"])),
            None
        );
        // 1000 bytes or fewer is a placeholder.
        put(root.path(), "A/artist.jpg", &[7u8; 1000]);
        assert_eq!(
            artwork::find_local_artist_image(root.path(), &dirs(&["A/B"])),
            None
        );
        // A directory with the right name is not a file.
        std::fs::create_dir_all(root.path().join("A/folder.jpg")).unwrap();
        assert_eq!(
            artwork::find_local_artist_image(root.path(), &dirs(&["A/B"])),
            None
        );
        // Another name, or another case, is not looked at.
        put(root.path(), "A/Artist.JPG", &noisy_png(64, 64));
        put(root.path(), "A/cover.jpg", &noisy_png(64, 64));
        assert_eq!(
            artwork::find_local_artist_image(root.path(), &dirs(&["A/B"])),
            None
        );
        // The library root itself is not an artist folder.
        put(root.path(), "artist.jpg", &noisy_png(64, 64));
        assert_eq!(
            artwork::find_local_artist_image(root.path(), &dirs(&["A/B"])),
            None
        );
    }

    /// Port of "returns false for path traversal attempts".
    #[test]
    fn path_traversal_is_refused() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("music");
        std::fs::create_dir_all(root.join("A/B")).unwrap();
        put(outer.path(), "artist.jpg", &noisy_png(64, 64));
        put(outer.path(), "x/artist.jpg", &noisy_png(64, 64));
        for bad in ["../x/y", "A/../../x/y", "/etc/x", "../../etc/passwd"] {
            assert_eq!(
                artwork::find_local_artist_image(&root, &dirs(&[bad])),
                None,
                "{bad}"
            );
        }
    }

    /// Port of "returns false for non-native paths": a symlink, as the image or as a folder on
    /// the way, never leads out.
    #[cfg(unix)]
    #[test]
    fn symlinks_are_refused() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("music");
        std::fs::create_dir_all(root.join("A/B")).unwrap();
        let elsewhere = put(outer.path(), "secret/artist.jpg", &noisy_png(64, 64));
        std::os::unix::fs::symlink(&elsewhere, root.join("A/artist.jpg")).unwrap();
        assert_eq!(
            artwork::find_local_artist_image(&root, &dirs(&["A/B"])),
            None
        );
        // A symlinked artist folder pointing out of the root.
        std::fs::create_dir_all(outer.path().join("secret/B")).unwrap();
        std::os::unix::fs::symlink(outer.path().join("secret"), root.join("L")).unwrap();
        assert_eq!(
            artwork::find_local_artist_image(&root, &dirs(&["L/B"])),
            None
        );
    }

    #[test]
    fn stored_image_is_jpeg_at_most_1500px() {
        let big = noisy_png(3000, 1200);
        let jpeg = artwork::normalize_image(&big).unwrap();
        assert_eq!(&jpeg[..3], &[0xFF, 0xD8, 0xFF], "a JPEG");
        let img = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (1500, 600));
        // A small one keeps its size.
        let small = artwork::normalize_image(&png_bytes(300, 200, [1, 2, 3])).unwrap();
        let img = image::load_from_memory(&small).unwrap();
        assert_eq!((img.width(), img.height()), (300, 200));
        // Not an image, or a header claiming enormous dimensions, is refused.
        assert!(artwork::normalize_image(b"not an image at all").is_err());
        let mut bomb = png_bytes(1, 1, [0, 0, 0]);
        // IHDR width/height at bytes 16..24: claim 60000 x 60000.
        bomb[16..20].copy_from_slice(&60000u32.to_be_bytes());
        bomb[20..24].copy_from_slice(&60000u32.to_be_bytes());
        assert!(artwork::normalize_image(&bomb).is_err());
    }

    #[test]
    fn artist_images_are_stored_and_removed_by_id_only() {
        let data = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let jpeg = artwork::normalize_image(&noisy_png(40, 40)).unwrap();
        artwork::store_artist_image(data.path(), id, &jpeg).unwrap();
        let stored = artwork::artist_image_path(data.path(), id);
        assert_eq!(std::fs::read(&stored).unwrap(), jpeg);
        // Only that file is in the folder (no temp file left behind).
        assert_eq!(
            std::fs::read_dir(data.path().join("artist-images"))
                .unwrap()
                .count(),
            1
        );
        let other = Uuid::now_v7();
        artwork::remove_artist_image(data.path(), other).unwrap();
        assert!(stored.exists());
        artwork::remove_artist_image(data.path(), id).unwrap();
        assert!(!stored.exists());
        // Removing again is fine.
        artwork::remove_artist_image(data.path(), id).unwrap();
    }
}

mod stored_images {
    use std::path::{Path, PathBuf};

    use kima_library::artwork;
    use uuid::Uuid;

    fn noisy_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let v = x
                .wrapping_mul(2_654_435_761)
                .wrapping_add(y.wrapping_mul(40_503));
            image::Rgb([(v >> 3) as u8, (v >> 11) as u8, (v >> 19) as u8])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    #[test]
    fn album_image_path_is_the_id_under_album_images() {
        let id = Uuid::parse_str("0192f0c4-1111-7000-8000-000000000001").unwrap();
        assert_eq!(
            artwork::album_image_path(Path::new("/data"), id),
            PathBuf::from("/data/album-images/0192f0c4-1111-7000-8000-000000000001.jpg")
        );
    }

    /// A staged image reaches its place only when persisted; dropped, it leaves nothing.
    #[test]
    fn staged_images_land_only_when_persisted() {
        let data = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let staged = artwork::stage_album_image(data.path(), b"first").unwrap();
        drop(staged);
        let dir = data.path().join("album-images");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let staged = artwork::stage_album_image(data.path(), b"second").unwrap();
        staged
            .persist(&artwork::album_image_path(data.path(), id))
            .unwrap();
        assert_eq!(
            std::fs::read(artwork::album_image_path(data.path(), id)).unwrap(),
            b"second"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        artwork::remove_album_image(data.path(), id).unwrap();
        assert!(!artwork::album_image_path(data.path(), id).exists());
        artwork::remove_album_image(data.path(), id).unwrap();
    }

    /// The local artist image's stamp: current while the source is unchanged and the stored
    /// image is there; stale once either changes.
    #[test]
    fn local_stamp_tracks_the_source() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let src = root_path.join("Artist/artist.jpg");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, noisy_jpeg(64, 64)).unwrap();
        let id = Uuid::now_v7();
        assert!(!artwork::local_artist_image_is_current(
            data.path(),
            &root_path,
            id,
            &src
        ));
        let stamp = artwork::local_stamp(&root_path, &src).unwrap();
        artwork::store_artist_image(data.path(), id, b"jpeg").unwrap();
        artwork::write_local_stamp(data.path(), id, &stamp).unwrap();
        assert!(artwork::local_artist_image_is_current(
            data.path(),
            &root_path,
            id,
            &src
        ));

        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
        std::fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!artwork::local_artist_image_is_current(
            data.path(),
            &root_path,
            id,
            &src
        ));

        artwork::write_local_stamp(
            data.path(),
            id,
            &artwork::local_stamp(&root_path, &src).unwrap(),
        )
        .unwrap();
        assert!(artwork::local_artist_image_is_current(
            data.path(),
            &root_path,
            id,
            &src
        ));
        artwork::remove_artist_image(data.path(), id).unwrap();
        assert!(!artwork::local_artist_image_is_current(
            data.path(),
            &root_path,
            id,
            &src
        ));
    }

    #[test]
    fn stored_variant_snaps_caches_and_rebuilds() {
        let data = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let src = artwork::artist_image_path(data.path(), id);
        artwork::store_artist_image(data.path(), id, &noisy_jpeg(400, 200)).unwrap();
        let get = |size| {
            let bytes = artwork::stored_variant(data.path(), id, &src, size)
                .unwrap()
                .unwrap();
            let img = image::load_from_memory(&bytes).unwrap();
            (img.width(), img.height())
        };
        assert_eq!(get(Some(128)), (128, 64));
        assert_eq!(get(None), (400, 200));
        let variant = data.path().join(format!("artwork/{id}/stored-128.jpg"));
        assert!(variant.is_file());

        artwork::store_artist_image(data.path(), id, &noisy_jpeg(100, 300)).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
        std::fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(get(Some(128)), (43, 128));

        // A missing source is no image.
        std::fs::remove_file(&src).unwrap();
        assert!(
            artwork::stored_variant(data.path(), id, &src, Some(128))
                .unwrap()
                .is_none()
        );
    }
}
