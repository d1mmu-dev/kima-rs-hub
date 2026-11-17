mod common;

use axum::http::StatusCode;
use kima_db::PgPool;
use kima_db::library::{self, Guessed, ResolvedTrack};
use time::OffsetDateTime;
use uuid::Uuid;

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

async fn album_with_track(pool: &PgPool, path: &str) -> Uuid {
    let mut tx = pool.begin().await.unwrap();
    let track = library::upsert_track(
        &mut tx,
        &ResolvedTrack {
            album_artist: "Artist".into(),
            album_artist_key: "artist".into(),
            track_artist: "Artist".into(),
            album: path.into(),
            album_key: path.into(),
            album_year: Some(2001),
            track_year: Some(2001),
            is_compilation: false,
            disc: 1,
            track_no: 1,
            title: "Song".into(),
            duration_secs: 2,
            codec: "mp3".into(),
            bitrate: None,
            path: path.into(),
            size: 1,
            mtime: OffsetDateTime::now_utc(),
            fingerprint: {
                let mut fp = [1u8; 32];
                for (i, b) in path.bytes().enumerate() {
                    fp[i % 32] ^= b.wrapping_add(i as u8);
                }
                fp
            },
            guessed: Guessed::default(),
            ids: Default::default(),
            isrc: None,
            disc_total: None,
            track_total: None,
            artist_mbid_candidate: None,
        },
    )
    .await
    .unwrap()
    .track;
    tx.commit().await.unwrap();
    sqlx::query_scalar!("SELECT album_id FROM tracks WHERE id = $1", track)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cover_art_404_for_unknown_album(pool: PgPool) {
    let data = tempfile::tempdir().unwrap();
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(data.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!("/api/library/cover-art/{}", Uuid::now_v7()))
        .authorization_bearer(&token)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    let bad = server
        .get("/api/library/cover-art/not-a-uuid")
        .authorization_bearer(&token)
        .await;
    bad.assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cover_art_serves_resized_folder_image(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(music.path().join("A/B")).unwrap();
    std::fs::write(music.path().join("A/B/cover.png"), png(400, 200)).unwrap();
    let album = album_with_track(&pool, "A/B/01.mp3").await;
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let res = server
        .get(&format!(
            "/api/library/cover-art/{album}?size=100&token={token}"
        ))
        .await;
    res.assert_status_ok();
    assert_eq!(res.header("content-type"), "image/jpeg");
    assert_eq!(res.header("cache-control"), "private, max-age=86400");
    let img = image::load_from_memory(res.as_bytes()).unwrap();
    // 100 snaps up to the 128 bucket.
    assert_eq!((img.width(), img.height()), (128, 64));

    let orig = server
        .get(&format!("/api/library/cover-art/{album}"))
        .authorization_bearer(&token)
        .await;
    orig.assert_status_ok();
    let img = image::load_from_memory(orig.as_bytes()).unwrap();
    assert_eq!((img.width(), img.height()), (400, 200));

    let bad = server
        .get(&format!("/api/library/cover-art/{album}?size=abc"))
        .authorization_bearer(&token)
        .await;
    bad.assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cover_art_404_without_data_dir_or_art(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let album = album_with_track(&pool, "A/B/01.mp3").await;
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state.clone())).unwrap();
    let url = format!("/api/library/cover-art/{album}");
    server
        .get(&url)
        .authorization_bearer(&token)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    let data = tempfile::tempdir().unwrap();
    let state = state
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .get(&url)
        .authorization_bearer(&token)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sizes_share_a_bucket_and_cache_file(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(music.path().join("A/B")).unwrap();
    std::fs::write(music.path().join("A/B/cover.png"), png(1200, 600)).unwrap();
    let album = album_with_track(&pool, "A/B/01.mp3").await;
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    for size in [300, 400] {
        let res = server
            .get(&format!("/api/library/cover-art/{album}?size={size}"))
            .authorization_bearer(&token)
            .await;
        res.assert_status_ok();
        let img = image::load_from_memory(res.as_bytes()).unwrap();
        assert_eq!((img.width(), img.height()), (512, 256));
    }
    let mut files: Vec<_> = std::fs::read_dir(data.path().join(format!("artwork/{album}")))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, ["512.jpg", "source.json"]);
    assert!(
        data.path()
            .join(format!("artwork/{album}/512.jpg"))
            .is_file()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn all_corrupt_is_404(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(music.path().join("A/B")).unwrap();
    std::fs::write(music.path().join("A/B/cover.jpg"), b"not a jpeg").unwrap();
    let album = album_with_track(&pool, "A/B/01.mp3").await;
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .get(&format!("/api/library/cover-art/{album}"))
        .authorization_bearer(&token)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn query_token_is_rejected_on_non_media_routes(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .get(&format!("/api/system/features?token={token}"))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
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

struct Fixture {
    server: axum_test::TestServer,
    token: String,
    album: Uuid,
    music: tempfile::TempDir,
    data: tempfile::TempDir,
}

/// A music dir with `A/B/cover.png` (400x200), an album for `A/B/01.mp3`, and a server on them.
async fn cover_fixture(pool: PgPool, cache_mb: Option<u64>) -> Fixture {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(music.path().join("A/B")).unwrap();
    std::fs::write(music.path().join("A/B/cover.png"), png(400, 200)).unwrap();
    let album = album_with_track(&pool, "A/B/01.mp3").await;
    let mut state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    if let Some(mb) = cache_mb {
        state = state.with_artwork_cache_mb(mb);
    }
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    Fixture {
        server,
        token,
        album,
        music,
        data,
    }
}

impl Fixture {
    async fn orig_dims(&self) -> (u32, u32) {
        let res = self
            .server
            .get(&format!("/api/library/cover-art/{}", self.album))
            .authorization_bearer(&self.token)
            .await;
        res.assert_status_ok();
        let img = image::load_from_memory(res.as_bytes()).unwrap();
        (img.width(), img.height())
    }

    fn entry(&self) -> std::path::PathBuf {
        self.data.path().join(format!("artwork/{}", self.album))
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stamp_written_with_entry(pool: PgPool) {
    let f = cover_fixture(pool, None).await;
    assert_eq!(f.orig_dims().await, (400, 200));
    let raw = std::fs::read(f.entry().join("source.json")).unwrap();
    let stamp: kima_library::artwork::SourceStamp = serde_json::from_slice(&raw).unwrap();
    assert_eq!(stamp.source_rel, "A/B/cover.png");
    let meta = std::fs::metadata(f.music.path().join("A/B/cover.png")).unwrap();
    assert_eq!(stamp.size, meta.len());
    assert!(stamp.mtime_ns > 0 && stamp.folder_mtime_ns > 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rewritten_cover_file_rebuilds(pool: PgPool) {
    let f = cover_fixture(pool, None).await;
    assert_eq!(f.orig_dims().await, (400, 200));
    let cover = f.music.path().join("A/B/cover.png");
    std::fs::write(&cover, png(300, 300)).unwrap();
    set_mtime(&cover, 30);
    assert_eq!(f.orig_dims().await, (300, 300));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn new_cover_file_rebuilds(pool: PgPool) {
    let f = cover_fixture(pool, None).await;
    assert_eq!(f.orig_dims().await, (400, 200));
    // cover.jpg outranks cover.png; adding it changes the folder's mtime.
    let jpg = image::RgbImage::from_pixel(100, 50, image::Rgb([0, 0, 200]));
    jpg.save(f.music.path().join("A/B/cover.jpg")).unwrap();
    set_mtime(&f.music.path().join("A/B"), 30);
    assert_eq!(f.orig_dims().await, (100, 50));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unchanged_hit_reuses_and_refreshes_stamp(pool: PgPool) {
    let f = cover_fixture(pool, None).await;
    f.orig_dims().await;
    let (jpg, stamp) = (f.entry().join("orig.jpg"), f.entry().join("source.json"));
    set_mtime(&stamp, -3600);
    let old_stamp = std::fs::metadata(&stamp).unwrap().modified().unwrap();
    let built = std::fs::metadata(&jpg).unwrap().modified().unwrap();
    f.orig_dims().await;
    assert_eq!(std::fs::metadata(&jpg).unwrap().modified().unwrap(), built);
    assert!(std::fs::metadata(&stamp).unwrap().modified().unwrap() > old_stamp);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn entry_without_stamp_is_rebuilt(pool: PgPool) {
    let f = cover_fixture(pool, None).await;
    // An M1-era entry: a cache file but no source.json.
    std::fs::create_dir_all(f.entry()).unwrap();
    std::fs::write(f.entry().join("orig.jpg"), b"stale").unwrap();
    assert_eq!(f.orig_dims().await, (400, 200));
    assert!(f.entry().join("source.json").is_file());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn parallel_builds_survive_eviction(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut albums = Vec::new();
    for i in 0..16 {
        std::fs::create_dir_all(music.path().join(format!("d{i}"))).unwrap();
        std::fs::write(music.path().join(format!("d{i}/cover.png")), png(300, 300)).unwrap();
        albums.push(album_with_track(&pool, &format!("d{i}/01.mp3")).await);
    }
    let mut state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    // Over the cap after the first build or two, so every build evicts others.
    state.artwork_cache_bytes = 3000;
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let (server, token) = (&server, &token);
    let results = futures::future::join_all(albums.iter().map(|album| async move {
        server
            .get(&format!("/api/library/cover-art/{album}"))
            .authorization_bearer(token)
            .await
    }))
    .await;
    for res in results {
        res.assert_status_ok();
        assert!(image::load_from_memory(res.as_bytes()).is_ok());
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cache_cap_evicts_after_write(pool: PgPool) {
    // A zero cap evicts every entry, yet the response that built it still carries the image.
    let f = cover_fixture(pool, Some(0)).await;
    assert_eq!(f.orig_dims().await, (400, 200));
    assert!(!f.entry().exists());
}

// ---- M3: stored images (spec §12.6, §12.7; D-113) ----

fn jpeg_file(path: &std::path::Path, w: u32, h: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(w, h, image::Rgb([10, 120, 10]))
        .save_with_format(path, image::ImageFormat::Jpeg)
        .unwrap();
}

async fn artist_with_image(pool: &PgPool, data: &std::path::Path, w: u32, h: u32) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO artists (id, name, name_key, image_path, image_source) \
         VALUES ($1, 'Painter', 'painter', $1::text || '.jpg', 'wikidata')",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
    jpeg_file(&kima_library::artwork::artist_image_path(data, id), w, h);
    id
}

async fn set_album_image(pool: &PgPool, album: Uuid, source: &str) {
    sqlx::query(
        "UPDATE albums SET image_path = id::text || '.jpg', image_source = $2 WHERE id = $1",
    )
    .bind(album)
    .bind(source)
    .execute(pool)
    .await
    .unwrap();
}

async fn dims(
    server: &axum_test::TestServer,
    token: &str,
    id: Uuid,
    size: Option<u32>,
) -> (u32, u32) {
    let url = match size {
        Some(s) => format!("/api/library/cover-art/{id}?size={s}"),
        None => format!("/api/library/cover-art/{id}"),
    };
    let res = server.get(&url).authorization_bearer(token).await;
    res.assert_status_ok();
    assert_eq!(res.header("content-type"), "image/jpeg");
    assert_eq!(res.header("cache-control"), "private, max-age=86400");
    let img = image::load_from_memory(res.as_bytes()).unwrap();
    (img.width(), img.height())
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cover_route_serves_artist_id_with_size_snapping(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let artist = artist_with_image(&pool, data.path(), 800, 400).await;
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    // 100 snaps up to 128.
    assert_eq!(dims(&server, &token, artist, Some(100)).await, (128, 64));
    assert_eq!(dims(&server, &token, artist, Some(5000)).await, (800, 400));
    assert_eq!(dims(&server, &token, artist, None).await, (800, 400));
    // The variant is cached under the artist's id.
    assert!(data.path().join(format!("artwork/{artist}")).is_dir());
    // Without a session the image is not served, as album covers are not.
    server
        .get(&format!("/api/library/cover-art/{artist}"))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cover_route_unknown_id_404(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    // A file named by an id with no row is never served.
    let stray = Uuid::now_v7();
    jpeg_file(
        &kima_library::artwork::artist_image_path(data.path(), stray),
        50,
        50,
    );
    jpeg_file(
        &kima_library::artwork::album_image_path(data.path(), stray),
        50,
        50,
    );
    // An artist whose row records no image is not served either, even with a file present.
    let artist = artist_with_image(&pool, data.path(), 50, 50).await;
    sqlx::query("UPDATE artists SET image_path = NULL, image_source = NULL WHERE id = $1")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .get("/api/library/cover-art/..%2F..%2Fetc%2Fpasswd")
        .authorization_bearer(&token)
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    for id in [stray.to_string(), artist.to_string()] {
        let res = server
            .get(&format!("/api/library/cover-art/{id}"))
            .authorization_bearer(&token)
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(
            res.json::<serde_json::Value>()["error"],
            "Cover art not found"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fallback_served_after_embedded_and_folder(pool: PgPool) {
    let f = cover_fixture(pool.clone(), None).await;
    jpeg_file(
        &kima_library::artwork::album_image_path(f.data.path(), f.album),
        90,
        90,
    );
    set_album_image(&pool, f.album, "deezer").await;
    // The folder's cover.png (400x200) wins over the fallback.
    assert_eq!(f.orig_dims().await, (400, 200));
    // Without it, the fallback is served.
    std::fs::remove_file(f.music.path().join("A/B/cover.png")).unwrap();
    set_mtime(&f.music.path().join("A/B"), 30);
    assert_eq!(f.orig_dims().await, (90, 90));
    assert_eq!(dims(&f.server, &f.token, f.album, Some(64)).await, (64, 64));
    // Served again from the cache.
    assert_eq!(f.orig_dims().await, (90, 90));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_album_image_first(pool: PgPool) {
    let f = cover_fixture(pool.clone(), None).await;
    jpeg_file(
        &kima_library::artwork::album_image_path(f.data.path(), f.album),
        70,
        30,
    );
    set_album_image(&pool, f.album, "user").await;
    assert_eq!(f.orig_dims().await, (70, 30));
    // A user image whose file is gone falls back to the folder's cover.
    std::fs::remove_file(kima_library::artwork::album_image_path(
        f.data.path(),
        f.album,
    ))
    .unwrap();
    assert_eq!(f.orig_dims().await, (400, 200));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn variant_rebuilt_when_image_replaced(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let artist = artist_with_image(&pool, data.path(), 800, 400).await;
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    assert_eq!(dims(&server, &token, artist, Some(256)).await, (256, 128));
    assert_eq!(dims(&server, &token, artist, Some(256)).await, (256, 128));

    let source = kima_library::artwork::artist_image_path(data.path(), artist);
    jpeg_file(&source, 300, 600);
    set_mtime(&source, 30);
    assert_eq!(dims(&server, &token, artist, Some(256)).await, (128, 256));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_symlinked_stored_image_is_not_served(pool: PgPool) {
    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let artist = artist_with_image(&pool, data.path(), 80, 80).await;
    let source = kima_library::artwork::artist_image_path(data.path(), artist);
    let elsewhere = music.path().join("secret.jpg");
    jpeg_file(&elsewhere, 40, 40);
    std::fs::remove_file(&source).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &source).unwrap();
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .get(&format!("/api/library/cover-art/{artist}"))
        .authorization_bearer(&token)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// Review M3: embedded art (a real tagged file) is served before a stored fallback.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedded_cover_beats_the_fallback(pool: PgPool) {
    if !kima_test_support::require_ffmpeg() {
        return;
    }
    use lofty::config::WriteOptions;
    use lofty::file::TaggedFileExt;
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::prelude::*;
    use lofty::probe::Probe;

    let music = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let track =
        kima_test_support::make_audio(music.path(), "A/B/01.mp3", kima_test_support::Tags::full());
    let mut tagged = Probe::open(&track).unwrap().read().unwrap();
    let tag = tagged.primary_tag_mut().expect("the fixture has a tag");
    tag.push_picture(
        Picture::unchecked(png(300, 100))
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .build(),
    );
    tag.save_to_path(&track, WriteOptions::default()).unwrap();

    let album = album_with_track(&pool, "A/B/01.mp3").await;
    jpeg_file(
        &kima_library::artwork::album_image_path(data.path(), album),
        90,
        90,
    );
    set_album_image(&pool, album, "deezer").await;
    let state = common::state(pool)
        .with_data_dir(data.path().into())
        .with_library_root(music.path().into());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    assert_eq!(dims(&server, &token, album, None).await, (300, 100));
}

// ---- Task 15: the external cover proxy `?url=` (D-210) ----

mod remote_proxy {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use axum::http::StatusCode;
    use base64::Engine;
    use kima_core::identify::IdentifySwitch;
    use kima_db::PgPool;
    use kima_net::testing::{Entry, Recording, Replay};
    use kima_net::{Bases, Net, NetConfig};
    use serde_json::Value;

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

    fn entry(path: &str, status: u16, content_type: &str, body: &[u8]) -> Entry {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), content_type.to_string());
        Entry {
            method: "GET".into(),
            url: path.into(),
            status,
            headers,
            body: Value::Null,
            body_base64: Some(base64::engine::general_purpose::STANDARD.encode(body)),
            request_body_sha256: None,
        }
    }

    static PROXY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct Env {
        _guard: tokio::sync::MutexGuard<'static, ()>,
        server: axum_test::TestServer,
        token: String,
        replay: Replay,
        #[allow(dead_code)]
        data: tempfile::TempDir,
    }

    impl Env {
        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.replay.url())
        }
    }

    async fn env(pool: PgPool, rec: Recording) -> Env {
        env_with(pool, rec, true).await
    }

    async fn env_with(pool: PgPool, rec: Recording, allow_private_images: bool) -> Env {
        let guard = PROXY_TEST_LOCK.lock().await;
        let replay = Replay::start(rec).await;
        let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
        cfg.allow_private_images = allow_private_images;
        let net = Net::new(cfg, pool.clone(), IdentifySwitch::new(false, true));
        let data = tempfile::tempdir().unwrap();
        let state = super::common::state(pool.clone())
            .with_net(Arc::new(net))
            .with_data_dir(data.path().to_path_buf());
        let (_, token) = super::common::user_with_token(&state, "ana").await;
        Env {
            _guard: guard,
            server: axum_test::TestServer::new(kima_api::router(state)).unwrap(),
            token,
            replay,
            data,
        }
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn task15_small_png_is_returned_unchanged_green(pool: PgPool) {
        let bytes = super::png(1, 1);
        assert!(bytes.len() < 1000);
        let e = env(
            pool,
            Recording(vec![entry(
                "/tiny",
                200,
                "application/octet-stream",
                &bytes,
            )]),
        )
        .await;
        let response = e
            .server
            .get(&format!("/api/library/cover-art?url={}", e.url("/tiny")))
            .authorization_bearer(&e.token)
            .await;
        response.assert_status_ok();
        assert_eq!(response.as_bytes().as_ref(), bytes.as_slice());
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn task15_gif_original_bytes_green(pool: PgPool) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7")
            .unwrap();
        let e = env(
            pool,
            Recording(vec![entry("/tiny.gif", 200, "image/gif", &bytes)]),
        )
        .await;
        let response = e
            .server
            .get(&format!(
                "/api/library/cover-art?url={}",
                e.url("/tiny.gif")
            ))
            .authorization_bearer(&e.token)
            .await;
        response.assert_status_ok();
        assert_eq!(response.as_bytes().as_ref(), bytes.as_slice());
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn task15_bad_address_is_400_green(pool: PgPool) {
        let e = env_with(pool, Recording(vec![]), false).await;
        for url in [
            "/api/library/cover-art/not-a-uuid",
            "/api/library/cover-art?url=http://127.0.0.1:1/a",
        ] {
            e.server
                .get(url)
                .authorization_bearer(&e.token)
                .await
                .assert_status(StatusCode::BAD_REQUEST);
        }
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn interactive_cover_works_with_identification_off(pool: PgPool) {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let replay = Replay::start(Recording(vec![entry(
            "/off.jpg",
            200,
            "image/jpeg",
            &noisy_jpeg(200, 200),
        )]))
        .await;
        let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
        cfg.allow_private_images = true;
        let net = Net::new(cfg, pool.clone(), IdentifySwitch::new(true, false));
        let data = tempfile::tempdir().unwrap();
        let state = super::common::state(pool)
            .with_net(Arc::new(net))
            .with_data_dir(data.path().to_path_buf());
        let (_, token) = super::common::user_with_token(&state, "ana").await;
        let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
        let res = server
            .get(&format!(
                "/api/library/cover-art?url={}/off.jpg",
                replay.url()
            ))
            .authorization_bearer(&token)
            .await;
        res.assert_status_ok();
        assert_eq!(replay.asked().len(), 1);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn path_encoded_url_is_proxied_task15(pool: PgPool) {
        let e = env(
            pool,
            Recording(vec![entry(
                "/path.jpg",
                200,
                "image/jpeg",
                &noisy_jpeg(200, 200),
            )]),
        )
        .await;
        let encoded: String =
            form_urlencoded::byte_serialize(e.url("/path.jpg").as_bytes()).collect();
        let res = e
            .server
            .get(&format!("/api/library/cover-art/{encoded}"))
            .authorization_bearer(&e.token)
            .await;
        res.assert_status_ok();
        assert_eq!(e.replay.asked().len(), 1);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn no_url_or_id_is_400(pool: PgPool) {
        let e = env(pool, Recording(vec![])).await;
        let res = e
            .server
            .get("/api/library/cover-art")
            .authorization_bearer(&e.token)
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>()["error"], "No cover ID or URL provided");
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn url_proxy_fetches_resizes_and_caches_after_first_response_drop(pool: PgPool) {
        let e = env(
            pool,
            Recording(vec![entry(
                "/cover.jpg",
                200,
                "image/jpeg",
                &noisy_jpeg(1200, 800),
            )]),
        )
        .await;
        let url = e.url("/cover.jpg");

        let first = e
            .server
            .get(&format!("/api/library/cover-art?url={url}&size=100"))
            .authorization_bearer(&e.token)
            .await;
        first.assert_status_ok();
        assert_eq!(first.header("content-type"), "image/jpeg");
        let img = image::load_from_memory(first.as_bytes()).unwrap();
        assert_eq!((img.width(), img.height()), (100, 66));
        let etag = first.header("etag").to_str().unwrap().to_string();
        use md5::{Digest, Md5};
        assert_eq!(etag, hex::encode(Md5::digest(first.as_bytes())));
        // TestResponse retains the response bytes and therefore the proxy slot.
        drop(first);

        // Served again from the memory cache: no second fetch.
        let second = e
            .server
            .get(&format!("/api/library/cover-art?url={url}&size=100"))
            .authorization_bearer(&e.token)
            .await;
        second.assert_status_ok();
        assert_eq!(e.replay.asked().len(), 1);

        // A matching `If-None-Match` is a 304.
        let not_modified = e
            .server
            .get(&format!("/api/library/cover-art?url={url}&size=100"))
            .add_header(axum::http::header::IF_NONE_MATCH, etag)
            .authorization_bearer(&e.token)
            .await;
        not_modified.assert_status(StatusCode::NOT_MODIFIED);
        assert_eq!(e.replay.asked().len(), 1);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn retained_proxy_responses_get_busy_then_release_slots(pool: PgPool) {
        let e = env(
            pool,
            Recording(vec![
                entry("/first.jpg", 200, "image/jpeg", &noisy_jpeg(200, 200)),
                entry("/second.jpg", 200, "image/jpeg", &noisy_jpeg(200, 200)),
                entry("/third.jpg", 200, "image/jpeg", &noisy_jpeg(200, 200)),
            ]),
        )
        .await;
        let request = |path: &str| format!("/api/library/cover-art?url={}", e.url(path));
        let first = e
            .server
            .get(&request("/first.jpg"))
            .authorization_bearer(&e.token)
            .await;
        first.assert_status_ok();
        let second = e
            .server
            .get(&request("/second.jpg"))
            .authorization_bearer(&e.token)
            .await;
        second.assert_status_ok();
        let busy = tokio::time::timeout(
            std::time::Duration::from_secs(17),
            e.server
                .get(&request("/third.jpg"))
                .authorization_bearer(&e.token),
        )
        .await
        .expect("saturated proxy hung");
        busy.assert_status(StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(e.replay.asked().len(), 2);
        drop(first);
        let third = e
            .server
            .get(&request("/third.jpg"))
            .authorization_bearer(&e.token)
            .await;
        third.assert_status_ok();
        assert_eq!(e.replay.asked().len(), 3);
        drop(second);
        drop(third);
        let cached = e
            .server
            .get(&request("/first.jpg"))
            .authorization_bearer(&e.token)
            .await;
        cached.assert_status_ok();
        assert_eq!(e.replay.asked().len(), 3);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn url_proxy_remembers_404_for_an_hour(pool: PgPool) {
        let e = env(
            pool,
            Recording(vec![entry("/gone.jpg", 404, "text/plain", b"nope")]),
        )
        .await;
        let url = e.url("/gone.jpg");
        for _ in 0..2 {
            let res = e
                .server
                .get(&format!("/api/library/cover-art?url={url}"))
                .authorization_bearer(&e.token)
                .await;
            res.assert_status(StatusCode::NOT_FOUND);
            assert_eq!(res.json::<Value>()["error"], "Cover art not found");
        }
        // The second request was answered from the negative cache, not a second fetch.
        assert_eq!(e.replay.asked().len(), 1);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn url_proxy_private_address_is_400(pool: PgPool) {
        let e = env_with(pool, Recording(vec![]), false).await;
        let res = e
            .server
            .get("/api/library/cover-art?url=http://127.0.0.1:1/x.jpg")
            .authorization_bearer(&e.token)
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>()["error"], "Invalid cover art URL");
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn url_proxy_host_down_is_502(pool: PgPool) {
        let e = env(pool, Recording(vec![])).await;
        // Keep the port bound but not listening so no other server can reuse it.
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let dead = format!("http://{}/x.jpg", socket.local_addr().unwrap());
        let res = e
            .server
            .get(&format!("/api/library/cover-art?url={dead}"))
            .authorization_bearer(&e.token)
            .await;
        res.assert_status(StatusCode::BAD_GATEWAY);
        assert_eq!(
            res.json::<Value>()["error"],
            "The picture's host did not answer; try again shortly"
        );
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn url_proxy_accepts_token_query(pool: PgPool) {
        let e = env(
            pool,
            Recording(vec![entry(
                "/cover.jpg",
                200,
                "image/jpeg",
                &noisy_jpeg(200, 200),
            )]),
        )
        .await;
        let url = e.url("/cover.jpg");
        // No Authorization header: the session comes from `?token=`.
        let res = e
            .server
            .get(&format!(
                "/api/library/cover-art?url={url}&token={}",
                e.token
            ))
            .await;
        res.assert_status_ok();
    }

    /// Task 15 (D-210): two cold fetches stalled well past `FETCH_BUDGET` must not block a
    /// concurrent cache hit, and a third cold request waiting on the same two admission slots
    /// must time out to a retryable 503 rather than hang. Exercised through the full axum
    /// router (`kima_api::router`), not by calling `remote_cover` directly, so the HTTP layer
    /// (auth, query parsing, status mapping) is covered end to end.
    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn router_parallel_stalled_fetches_leave_hit_fast_and_time_out_cold(pool: PgPool) {
        let _guard = PROXY_TEST_LOCK.lock().await;
        use axum::body::Body;
        use axum::http::Request;
        use kima_core::identify::IdentifySwitch;
        use tower::ServiceExt;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let png = Arc::new(noisy_jpeg(16, 16));
        let origin = axum::Router::new().route(
            "/{id}",
            axum::routing::get({
                let release = release.clone();
                let png = png.clone();
                move |axum::extract::Path(id): axum::extract::Path<String>| {
                    let (tx, release, png) = (tx.clone(), release.clone(), png.clone());
                    async move {
                        tx.send(id.clone()).unwrap();
                        if id != "warm" {
                            release.acquire().await.unwrap().forget();
                        }
                        ([("content-type", "image/jpeg")], png.as_ref().clone())
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let origin_task = tokio::spawn(async move { axum::serve(listener, origin).await.unwrap() });

        let mut cfg = NetConfig::for_tests(Bases::all(&base));
        cfg.allow_private_images = true;
        let net = Net::new(cfg, pool.clone(), IdentifySwitch::new(false, true));
        let data = tempfile::tempdir().unwrap();
        let state = super::common::state(pool.clone())
            .with_net(Arc::new(net))
            .with_data_dir(data.path().to_path_buf());
        let (_, token) = super::common::user_with_token(&state, "ana").await;
        let app = kima_api::router(state);

        let get = move |path: String, token: String, app: axum::Router| async move {
            app.oneshot(
                Request::get(path)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        };

        // Warm the cache through the full router (not remote_cover directly).
        let warm_url = format!("{base}/warm");
        let warm_path = format!("/api/library/cover-art?url={warm_url}");
        let warm_res = get(warm_path.clone(), token.clone(), app.clone()).await;
        assert_eq!(warm_res.status(), StatusCode::OK);
        assert_eq!(rx.recv().await.unwrap(), "warm");
        // `warm_res` would itself retain a miss-gate permit (D-210) until dropped; release it
        // before saturating the gate with genuinely stalled fetches below.
        drop(warm_res);

        // Two cold fetches that stall well past FETCH_BUDGET (15 s), occupying both miss-gate
        // admission slots.
        let mut stalled = Vec::new();
        for id in ["one", "two"] {
            let path = format!("/api/library/cover-art?url={base}/{id}");
            stalled.push(tokio::spawn(get(path, token.clone(), app.clone())));
        }
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();

        // A cache hit must stay fast while both miss slots are stalled beyond budget.
        let hit = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            get(warm_path.clone(), token.clone(), app.clone()),
        )
        .await
        .expect("cache hit was blocked by stalled cold fetches");
        assert_eq!(hit.status(), StatusCode::OK);
        drop(hit);

        // A third, concurrent cold request started after the other two is admitted once one of
        // them gives up its slot at its own FETCH_BUDGET deadline (self-timeout, D-210), then
        // times out on its own still-stalled fetch in turn: 502, not a hang, after roughly two
        // fetch budgets (~30 s). This is the real, deterministic outcome of two fetches stalled
        // past budget (an admission-saturation 503 needs *retained, completed* responses instead
        // — see `retained_proxy_responses_get_busy_then_release_slots` — because a gate permit
        // held only by a stalled fetch always self-frees at the same 15 s deadline a later
        // waiter's own admission wait would expire at).
        let cold_path = format!("/api/library/cover-art?url={base}/cold");
        let cold = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            get(cold_path, token.clone(), app.clone()),
        )
        .await
        .expect("cold request did not resolve within two fetch budgets");
        assert_eq!(cold.status(), StatusCode::BAD_GATEWAY);

        // Both stalled requests already gave up at their own FETCH_BUDGET deadline (same 502),
        // well before `cold` resolved; release the mock origin's blocked handlers so it can stop.
        for task in stalled {
            let res = task.await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        }
        release.add_permits(2);
        origin_task.abort();
    }
}
