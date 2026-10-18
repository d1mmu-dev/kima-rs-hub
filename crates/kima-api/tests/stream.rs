mod common;

use std::path::Path;
use std::time::Duration;

use axum::http::StatusCode;
use axum::http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use common::library::{Seed, seed};
use kima_db::PgPool;
use kima_library::transcode::Transcoder;

fn bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// Seeds one track and writes `data` at the path the seed row names.
async fn track_with_file(pool: &PgPool, root: &Path, data: &[u8]) -> uuid::Uuid {
    let s = seed(pool, Seed::new("Artist", "Album", Some(2001), "Song")).await;
    let file = root.join("Artist/Album/1-1 Song.mp3");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, data).unwrap();
    s.track
}

fn setup(pool: PgPool, root: &Path) -> kima_api::AppState {
    common::state(pool).with_library_root(root.into())
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn range_206_headers(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let data = bytes(10_000);
    let id = track_with_file(&pool, root.path(), &data).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let full = server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .authorization_bearer(&token)
        .await;
    full.assert_status_ok();
    assert_eq!(full.header(ACCEPT_RANGES), "bytes");
    assert_eq!(full.header(CONTENT_TYPE), "audio/mpeg");
    assert_eq!(full.header(CONTENT_LENGTH), "10000");
    assert_eq!(full.as_bytes().as_ref(), &data[..]);

    let res = server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .authorization_bearer(&token)
        .add_header(RANGE, "bytes=100-")
        .await;
    res.assert_status(StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.header(CONTENT_RANGE), "bytes 100-9999/10000");
    assert_eq!(res.header(ACCEPT_RANGES), "bytes");
    assert_eq!(res.header(CONTENT_LENGTH), "9900");
    assert_eq!(res.as_bytes().as_ref(), &data[100..]);

    let res = server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .authorization_bearer(&token)
        .add_header(RANGE, "bytes=10-19")
        .await;
    res.assert_status(StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.header(CONTENT_RANGE), "bytes 10-19/10000");
    assert_eq!(res.as_bytes().as_ref(), &data[10..20]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn suffix_range(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let data = bytes(10_000);
    let id = track_with_file(&pool, root.path(), &data).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .authorization_bearer(&token)
        .add_header(RANGE, "bytes=-500")
        .await;
    res.assert_status(StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.header(CONTENT_RANGE), "bytes 9500-9999/10000");
    assert_eq!(res.as_bytes().as_ref(), &data[9500..]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn out_of_range_416(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), &bytes(10_000)).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .authorization_bearer(&token)
        .add_header(RANGE, "bytes=10000-")
        .await;
    res.assert_status(StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(res.header(CONTENT_RANGE), "bytes */10000");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn quality_invalid_400(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), &bytes(100)).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    for q in ["ultra", ""] {
        let res = server
            .get(&format!("/api/library/tracks/{id}/stream?quality={q}"))
            .authorization_bearer(&token)
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        res.assert_json(&serde_json::json!({
            "error": "quality must be original, high, medium or low"
        }));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn query_token_auth_works(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), &bytes(100)).await;
    let state = setup(pool, root.path());
    let (user, token) = common::user_with_token(&state, "ana").await;
    let stream = state.jwt.issue_stream(user.id, user.token_version);
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    for t in [&token, &stream] {
        server
            .get(&format!("/api/library/tracks/{id}/stream?token={t}"))
            .await
            .assert_status_ok();
    }
    server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_track_404(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), &bytes(100)).await;
    let state = setup(pool.clone(), root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let get = |id: String| {
        server
            .get(&format!("/api/library/tracks/{id}/stream"))
            .authorization_bearer(&token)
    };
    get(uuid::Uuid::now_v7().to_string())
        .await
        .assert_status(StatusCode::NOT_FOUND);
    get("not-a-uuid".into())
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // Row present, file gone.
    std::fs::remove_file(root.path().join("Artist/Album/1-1 Song.mp3")).unwrap();
    get(id.to_string())
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // Row flagged missing.
    common::library::mark_missing(&pool, id).await;
    get(id.to_string())
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn path_escaping_root_404(pool: PgPool) {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(work.path().join("secret.mp3"), b"secret").unwrap();
    let id = track_with_file(&pool, &root, &bytes(100)).await;
    sqlx::query("UPDATE tracks SET path = '../secret.mp3' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let state = setup(pool, &root);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .authorization_bearer(&token)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_transcoder_falls_back_to_original(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let data = bytes(500);
    let id = track_with_file(&pool, root.path(), &data).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!("/api/library/tracks/{id}/stream?quality=low"))
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn transcode_first_play_200_no_ranges_then_cached_206(pool: PgPool) {
    if !Transcoder::ffmpeg_available() {
        eprintln!("ffmpeg missing, skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), b"").await;
    let src = root.path().join("Artist/Album/1-1 Song.mp3");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:duration=3")
        .args(["-c:a", "libmp3lame", "-b:a", "320k"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());
    let state =
        setup(pool, root.path()).with_transcoder(Transcoder::new(cache.path(), 100 * 1024 * 1024));
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let url = format!("/api/library/tracks/{id}/stream?quality=low");

    // The first play follows the growing file: 200, no ranges, no length.
    let first = server
        .get(&url)
        .authorization_bearer(&token)
        .add_header(RANGE, "bytes=10-19")
        .await;
    first.assert_status_ok();
    assert!(first.maybe_header(ACCEPT_RANGES).is_none());
    assert!(first.maybe_header(CONTENT_LENGTH).is_none());
    assert_eq!(first.header(CONTENT_TYPE), "audio/mpeg");
    let first_len = first.as_bytes().len();
    assert!(first_len > 10_000, "got {first_len} bytes");

    // Once it is cached, ranges work.
    let mut cached = None;
    for _ in 0..50 {
        let r = server
            .get(&url)
            .authorization_bearer(&token)
            .add_header(RANGE, "bytes=100-199")
            .await;
        if r.status_code() == StatusCode::PARTIAL_CONTENT {
            cached = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let r = cached.expect("transcode never became cached");
    assert_eq!(r.header(ACCEPT_RANGES), "bytes");
    assert_eq!(r.as_bytes().len(), 100);
    assert!(
        r.header(CONTENT_RANGE)
            .to_str()
            .unwrap()
            .starts_with("bytes 100-199/")
    );
}

use axum::http::header::{ETAG, IF_NONE_MATCH, IF_RANGE, LAST_MODIFIED};

/// A tiny mp3 for the transcode tests.
fn make_mp3(dest: &Path) {
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:duration=3")
        .args(["-c:a", "libmp3lame", "-b:a", "320k"])
        .arg(dest)
        .status()
        .unwrap();
    assert!(status.success());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn original_has_etag_and_last_modified(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), &bytes(10_000)).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    let etag = res.header(ETAG).to_str().unwrap().to_string();
    assert!(etag.starts_with("\"2710-") && etag.ends_with('"'), "{etag}");
    assert!(
        res.header(LAST_MODIFIED)
            .to_str()
            .unwrap()
            .ends_with(" GMT")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn if_none_match_304(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), &bytes(10_000)).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let url = format!("/api/library/tracks/{id}/stream");
    let first = server.get(&url).authorization_bearer(&token).await;
    let etag = first.header(ETAG).to_str().unwrap().to_string();
    let res = server
        .get(&url)
        .authorization_bearer(&token)
        .add_header(IF_NONE_MATCH, etag.as_str())
        .await;
    res.assert_status(StatusCode::NOT_MODIFIED);
    assert!(res.as_bytes().is_empty());
    assert_eq!(res.header(ETAG).to_str().unwrap(), etag);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn if_range_matching_206(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let data = bytes(10_000);
    let id = track_with_file(&pool, root.path(), &data).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let url = format!("/api/library/tracks/{id}/stream");
    let first = server.get(&url).authorization_bearer(&token).await;
    let etag = first.header(ETAG).to_str().unwrap().to_string();
    let lm = first.header(LAST_MODIFIED).to_str().unwrap().to_string();
    for validator in [etag, lm] {
        let res = server
            .get(&url)
            .authorization_bearer(&token)
            .add_header(RANGE, "bytes=10-19")
            .add_header(IF_RANGE, validator.as_str())
            .await;
        res.assert_status(StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.as_bytes().as_ref(), &data[10..20]);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn if_range_stale_200_full(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let data = bytes(10_000);
    let id = track_with_file(&pool, root.path(), &data).await;
    let state = setup(pool, root.path());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!("/api/library/tracks/{id}/stream"))
        .authorization_bearer(&token)
        .add_header(RANGE, "bytes=10-19")
        .add_header(IF_RANGE, "\"stale\"")
        .await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    assert_eq!(res.header(CONTENT_LENGTH), "10000");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cached_transcode_etag_has_quality(pool: PgPool) {
    if !Transcoder::ffmpeg_available() {
        eprintln!("ffmpeg missing, skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), b"").await;
    make_mp3(&root.path().join("Artist/Album/1-1 Song.mp3"));
    let state =
        setup(pool, root.path()).with_transcoder(Transcoder::new(cache.path(), 100 * 1024 * 1024));
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let url = format!("/api/library/tracks/{id}/stream?quality=low");
    // Drain the first (growing) play, then wait for the cached file.
    server.get(&url).authorization_bearer(&token).await;
    for _ in 0..50 {
        let r = server.get(&url).authorization_bearer(&token).await;
        if let Some(e) = r.maybe_header(ETAG) {
            assert!(e.to_str().unwrap().ends_with("-low\""), "{e:?}");
            assert!(r.maybe_header(LAST_MODIFIED).is_some());
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("transcode never became cached");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn growing_transcode_has_no_etag(pool: PgPool) {
    if !Transcoder::ffmpeg_available() {
        eprintln!("ffmpeg missing, skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let id = track_with_file(&pool, root.path(), b"").await;
    make_mp3(&root.path().join("Artist/Album/1-1 Song.mp3"));
    let state =
        setup(pool, root.path()).with_transcoder(Transcoder::new(cache.path(), 100 * 1024 * 1024));
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!("/api/library/tracks/{id}/stream?quality=low"))
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    assert!(res.maybe_header(ETAG).is_none());
    assert!(res.maybe_header(LAST_MODIFIED).is_none());
}
