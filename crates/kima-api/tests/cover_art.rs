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
        },
    )
    .await
    .unwrap();
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
    bad.assert_status(StatusCode::NOT_FOUND);
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
