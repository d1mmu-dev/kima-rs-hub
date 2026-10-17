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
            album: "Album".into(),
            album_key: "album".into(),
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
            fingerprint: [1; 32],
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
    assert_eq!(res.header("cache-control"), "public, max-age=86400");
    let img = image::load_from_memory(res.as_bytes()).unwrap();
    assert_eq!((img.width(), img.height()), (100, 50));

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
