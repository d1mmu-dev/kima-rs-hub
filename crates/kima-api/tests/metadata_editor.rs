//! MetadataEditor overrides and reset (spec §16.2-§16.3, D-118).

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use base64::Engine;
use common::library::{Seed, Seeded, seed};
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use serde_json::{Value, json};
use uuid::Uuid;

const M1: &str = "11111111-1111-4111-8111-111111111111";
const M2: &str = "22222222-2222-4222-8222-222222222222";
const RG: &str = "33333333-3333-4333-8333-333333333333";

fn jpeg(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let v = (x ^ seed)
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

/// The image host: `/hero.jpg` and `/cover.jpg` are images, `/missing.jpg` is a 404 and
/// `/page.html` is not an image.
fn images() -> Recording {
    Recording(vec![
        entry("/hero.jpg", 200, "image/jpeg", &jpeg(400, 300, 1)),
        entry("/cover.jpg", 200, "image/jpeg", &jpeg(300, 300, 2)),
        entry("/missing.jpg", 404, "text/plain", b"nope"),
        entry("/page.html", 200, "text/html", &[b'x'; 4000]),
    ])
}

struct Env {
    srv: TestServer,
    admin: String,
    user: String,
    replay: Replay,
    data: tempfile::TempDir,
}

impl Env {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.replay.url())
    }
}

async fn env_with(pool: PgPool, allow_private_images: bool) -> Env {
    let replay = Replay::start(images()).await;
    let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
    cfg.allow_private_images = allow_private_images;
    let net = Net::new(cfg, pool.clone(), IdentifySwitch::new(false, true));
    let data = tempfile::tempdir().unwrap();
    let state = common::state(pool.clone())
        .with_identify(IdentifySwitch::new(false, true))
        .with_net(Arc::new(net))
        .with_data_dir(data.path().to_path_buf());
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let (_, user) = common::user_with_token(&state, "bob").await;
    Env {
        srv: TestServer::new(kima_api::router(state)).unwrap(),
        admin,
        user,
        replay,
        data,
    }
}

async fn env(pool: PgPool) -> Env {
    env_with(pool, true).await
}

async fn put(e: &Env, path: &str, body: Value) -> (StatusCode, Value) {
    let res = e
        .srv
        .put(path)
        .authorization_bearer(&e.admin)
        .json(&body)
        .await;
    (res.status_code(), res.json())
}

async fn post(e: &Env, token: &str, path: &str) -> (StatusCode, Value) {
    let res = e.srv.post(path).authorization_bearer(token).await;
    (res.status_code(), res.json())
}

async fn get(e: &Env, path: &str) -> Value {
    let res = e.srv.get(path).authorization_bearer(&e.user).await;
    res.assert_status_ok();
    res.json()
}

fn artist_path(id: impl std::fmt::Display) -> String {
    format!("/api/enrichment/artists/{id}/metadata")
}

fn album_path(id: impl std::fmt::Display) -> String {
    format!("/api/enrichment/albums/{id}/metadata")
}

async fn one(pool: &PgPool) -> Seeded {
    seed(pool, Seed::new("Alpha", "First", Some(1999), "Song")).await
}

async fn exec(pool: &PgPool, sql: &str, id: Uuid) {
    sqlx::query(sql).bind(id).execute(pool).await.unwrap();
}

/// `(display_name, user_bio, user_genres)` as stored, NULLs kept apart from empty values.
async fn artist_overrides(
    pool: &PgPool,
    id: Uuid,
) -> (Option<String>, Option<String>, Option<Vec<String>>) {
    sqlx::query_as("SELECT display_name, user_bio, user_genres FROM artists WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn artist_col(pool: &PgPool, id: Uuid, col: &str) -> Option<String> {
    sqlx::query_scalar(&format!("SELECT {col}::text FROM artists WHERE id = $1"))
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn album_col(pool: &PgPool, id: Uuid, col: &str) -> Option<String> {
    sqlx::query_scalar(&format!("SELECT {col}::text FROM albums WHERE id = $1"))
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn queued_cycles(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'enrich' AND state = 'queued'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn name_bio_genres_set_and_cleared(pool: PgPool) {
    let s = one(&pool).await;
    let e = env(pool.clone()).await;
    let (status, _) = put(
        &e,
        &artist_path(s.artist),
        json!({"name": "Shown", "bio": "My bio", "genres": ["Shoegaze", " dream pop ", ""]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (name, bio, genres) = artist_overrides(&pool, s.artist).await;
    assert_eq!(name.as_deref(), Some("Shown"));
    assert_eq!(bio.as_deref(), Some("My bio"));
    assert_eq!(
        genres,
        Some(vec!["Shoegaze".to_owned(), "dream pop".to_owned()])
    );
    let v = get(&e, &format!("/api/library/artists/{}", s.artist)).await;
    assert_eq!(v["displayName"], "Shown");
    assert_eq!(v["userSummary"], "My bio");
    assert_eq!(v["bio"], "My bio");
    assert_eq!(v["userGenres"], json!(["Shoegaze", "dream pop"]));
    assert_eq!(v["hasUserOverrides"], true);

    // Empty strings clear.
    let (status, _) = put(
        &e,
        &artist_path(s.artist),
        json!({"name": "", "bio": "  ", "genres": ""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (name, bio, genres) = artist_overrides(&pool, s.artist).await;
    assert_eq!(name, None, "a cleared name is NULL, never ''");
    assert_eq!(bio, None);
    assert!(genres.unwrap_or_default().is_empty());
    let v = get(&e, &format!("/api/library/artists/{}", s.artist)).await;
    assert_eq!(v["hasUserOverrides"], false);
    assert_eq!(v["displayName"], Value::Null);

    // So does null.
    put(
        &e,
        &artist_path(s.artist),
        json!({"name": "Again", "bio": "b", "genres": ["x"]}),
    )
    .await;
    let (status, _) = put(
        &e,
        &artist_path(s.artist),
        json!({"name": null, "bio": null, "genres": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (name, bio, genres) = artist_overrides(&pool, s.artist).await;
    assert_eq!((name, bio), (None, None));
    assert!(genres.unwrap_or_default().is_empty());

    // A field left out is left alone.
    put(&e, &artist_path(s.artist), json!({"name": "Kept"})).await;
    put(&e, &artist_path(s.artist), json!({"bio": "only bio"})).await;
    let (name, bio, _) = artist_overrides(&pool, s.artist).await;
    assert_eq!(name.as_deref(), Some("Kept"));
    assert_eq!(bio.as_deref(), Some("only bio"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_title_year_set_and_cleared(pool: PgPool) {
    let s = one(&pool).await;
    let e = env(pool.clone()).await;
    let (status, _) = put(
        &e,
        &album_path(s.album),
        json!({"title": "Shown Title", "year": 2001, "genres": ["Rock"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        album_col(&pool, s.album, "display_title").await.as_deref(),
        Some("Shown Title")
    );
    assert_eq!(
        album_col(&pool, s.album, "display_year").await.as_deref(),
        Some("2001")
    );
    let v = get(&e, &format!("/api/library/albums/{}", s.album)).await;
    assert_eq!(v["displayTitle"], "Shown Title");
    assert_eq!(v["displayYear"], 2001);
    assert_eq!(v["userGenres"], json!(["Rock"]));
    assert_eq!(v["hasUserOverrides"], true);
    // The album row keeps its own title and year.
    assert_eq!(v["title"], "First");
    assert_eq!(v["year"], 1999);

    // A numeric string is read as upstream's parseInt reads it.
    put(&e, &album_path(s.album), json!({"year": "1987"})).await;
    assert_eq!(
        album_col(&pool, s.album, "display_year").await.as_deref(),
        Some("1987")
    );

    let (status, _) = put(&e, &album_path(s.album), json!({"title": "", "year": ""})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(album_col(&pool, s.album, "display_title").await, None);
    assert_eq!(album_col(&pool, s.album, "display_year").await, None);

    put(
        &e,
        &album_path(s.album),
        json!({"title": "T", "year": 1990}),
    )
    .await;
    let (status, _) = put(
        &e,
        &album_path(s.album),
        json!({"title": null, "year": null, "genres": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(album_col(&pool, s.album, "display_title").await, None);
    assert_eq!(album_col(&pool, s.album, "display_year").await, None);
    let v = get(&e, &format!("/api/library/albums/{}", s.album)).await;
    assert_eq!(v["hasUserOverrides"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn year_not_a_number_400(pool: PgPool) {
    let s = one(&pool).await;
    let e = env(pool.clone()).await;
    for year in [json!("abc"), json!(true), json!([1999]), json!({"y": 1})] {
        let (status, body) = put(
            &e,
            &album_path(s.album),
            json!({"title": "Not written", "year": year}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{year}");
        assert_eq!(body, json!({"error": "year must be a number"}));
    }
    for year in [json!(0), json!(-5), json!(10000), json!("12345")] {
        let (status, body) = put(&e, &album_path(s.album), json!({"year": year})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{year}");
        assert_eq!(body, json!({"error": "year must be between 1 and 9999"}));
    }
    // Nothing of a refused request is written.
    assert_eq!(album_col(&pool, s.album, "display_title").await, None);
    assert_eq!(album_col(&pool, s.album, "display_year").await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn invalid_mbid_400(pool: PgPool) {
    let s = one(&pool).await;
    let e = env(pool.clone()).await;
    let invalid = json!({"error": "Invalid MusicBrainz ID"});
    for mbid in [
        json!("not-an-id"),
        json!("{11111111-1111-4111-8111-111111111111}"),
        json!("00000000-0000-0000-0000-000000000000"),
        json!(5),
    ] {
        let (status, body) = put(
            &e,
            &artist_path(s.artist),
            json!({"name": "Not written", "mbid": mbid}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{mbid}");
        assert_eq!(body, invalid);
        let (status, body) = put(&e, &album_path(s.album), json!({"rgMbid": mbid})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{mbid}");
        assert_eq!(body, invalid);
    }
    assert_eq!(artist_col(&pool, s.artist, "display_name").await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mbid_in_use_409(pool: PgPool) {
    let a = one(&pool).await;
    let b = seed(&pool, Seed::new("Beta", "Second", Some(2000), "Other")).await;
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = 'tag' WHERE id = $1")
        .bind(b.artist)
        .bind(Uuid::parse_str(M1).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE albums SET release_group_mbid = $2, release_group_mbid_source = 'tag'
         WHERE id = $1",
    )
    .bind(b.album)
    .bind(Uuid::parse_str(RG).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let e = env(pool.clone()).await;

    let (status, body) = put(
        &e,
        &artist_path(a.artist),
        json!({"name": "Not written", "mbid": M1.to_uppercase()}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({"error": format!("MBID {M1} is already in use by another artist")})
    );
    assert_eq!(artist_col(&pool, a.artist, "mbid").await, None);
    assert_eq!(artist_col(&pool, a.artist, "display_name").await, None);

    let (status, body) = put(
        &e,
        &album_path(a.album),
        json!({"title": "Not written", "rgMbid": RG}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({"error": format!("MBID {RG} is already in use by another album")})
    );
    assert_eq!(album_col(&pool, a.album, "release_group_mbid").await, None);
    assert_eq!(album_col(&pool, a.album, "display_title").await, None);

    // The row's own MBID is not "another row".
    let (status, _) = put(&e, &artist_path(b.artist), json!({"mbid": M1})).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn empty_mbid_unchanged(pool: PgPool) {
    let s = one(&pool).await;
    sqlx::query(
        "UPDATE artists SET mbid = $2, mbid_source = 'tag', enrichment_status = 'completed'
         WHERE id = $1",
    )
    .bind(s.artist)
    .bind(Uuid::parse_str(M1).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE albums SET release_group_mbid = $2, release_group_mbid_source = 'tag'
         WHERE id = $1",
    )
    .bind(s.album)
    .bind(Uuid::parse_str(RG).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let e = env(pool.clone()).await;
    // Empty, blank, null, and the value already stored: nothing changes, not even the source.
    for mbid in [json!(""), json!("   "), json!(null), json!(M1)] {
        let (status, _) = put(&e, &artist_path(s.artist), json!({"mbid": mbid})).await;
        assert_eq!(status, StatusCode::OK, "{mbid}");
    }
    for rg in [json!(""), json!(null), json!(RG)] {
        let (status, _) = put(&e, &album_path(s.album), json!({"rgMbid": rg})).await;
        assert_eq!(status, StatusCode::OK, "{rg}");
    }
    assert_eq!(
        artist_col(&pool, s.artist, "mbid").await.as_deref(),
        Some(M1)
    );
    assert_eq!(
        artist_col(&pool, s.artist, "mbid_source").await.as_deref(),
        Some("tag")
    );
    assert_eq!(
        artist_col(&pool, s.artist, "enrichment_status")
            .await
            .as_deref(),
        Some("completed")
    );
    assert_eq!(
        album_col(&pool, s.album, "release_group_mbid")
            .await
            .as_deref(),
        Some(RG)
    );
    assert_eq!(
        album_col(&pool, s.album, "release_group_mbid_source")
            .await
            .as_deref(),
        Some("tag")
    );
    assert_eq!(queued_cycles(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_mbid_change_marks_pending(pool: PgPool) {
    let s = one(&pool).await;
    sqlx::query(
        "UPDATE artists SET mbid = $2, mbid_source = 'tag', enrichment_status = 'completed'
         WHERE id = $1",
    )
    .bind(s.artist)
    .bind(Uuid::parse_str(M1).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let e = env(pool.clone()).await;
    let (status, body) = put(
        &e,
        &artist_path(s.artist),
        json!({"mbid": format!(" {M2} ")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mbid"], M2);
    assert_eq!(
        artist_col(&pool, s.artist, "mbid").await.as_deref(),
        Some(M2)
    );
    assert_eq!(
        artist_col(&pool, s.artist, "mbid_source").await.as_deref(),
        Some("user")
    );
    assert_eq!(
        artist_col(&pool, s.artist, "enrichment_status")
            .await
            .as_deref(),
        Some("pending")
    );
    assert_eq!(queued_cycles(&pool).await, 1, "the coordinator was kicked");
}

async fn stage(pool: &PgPool, track: Uuid, stage: &str, status: &str, source: Option<&str>) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, source, attempts, finished_at)
         VALUES ($1, $2, $3, $4, 1, now())",
    )
    .bind(track)
    .bind(stage)
    .bind(status)
    .bind(source)
    .execute(pool)
    .await
    .unwrap();
}

async fn stage_status(pool: &PgPool, track: Uuid, stage: &str) -> String {
    sqlx::query_scalar("SELECT status FROM track_stages WHERE track_id = $1 AND stage = $2")
        .bind(track)
        .bind(stage)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rg_mbid_change_reopens_non_tag_recordings(pool: PgPool) {
    let t1 = seed(
        &pool,
        Seed::new("Alpha", "First", Some(1999), "One").at(1, 1),
    )
    .await;
    let t2 = seed(
        &pool,
        Seed::new("Alpha", "First", Some(1999), "Two").at(1, 2),
    )
    .await;
    let t3 = seed(
        &pool,
        Seed::new("Alpha", "First", Some(1999), "Three").at(1, 3),
    )
    .await;
    let other = seed(&pool, Seed::new("Alpha", "Other", Some(2005), "Else")).await;
    assert_eq!(t1.album, t2.album);
    stage(&pool, t1.track, "recording", "done", Some("tracklist")).await;
    stage(&pool, t1.track, "fingerprint", "done", Some("ffmpeg")).await;
    stage(&pool, t2.track, "recording", "done", Some("file_tag")).await;
    stage(&pool, t3.track, "recording", "failed", None).await;
    stage(&pool, other.track, "recording", "done", Some("search")).await;
    let e = env(pool.clone()).await;

    let (status, body) = put(&e, &album_path(t1.album), json!({"rgMbid": RG})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["rgMbid"], RG);
    assert_eq!(
        album_col(&pool, t1.album, "release_group_mbid")
            .await
            .as_deref(),
        Some(RG)
    );
    assert_eq!(
        album_col(&pool, t1.album, "release_group_mbid_source")
            .await
            .as_deref(),
        Some("user")
    );
    assert_eq!(stage_status(&pool, t1.track, "recording").await, "pending");
    assert_eq!(stage_status(&pool, t1.track, "fingerprint").await, "done");
    assert_eq!(stage_status(&pool, t2.track, "recording").await, "done");
    assert_eq!(stage_status(&pool, t3.track, "recording").await, "failed");
    assert_eq!(stage_status(&pool, other.track, "recording").await, "done");
    assert_eq!(queued_cycles(&pool).await, 1, "the coordinator was kicked");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hero_url_downloaded_and_served(pool: PgPool) {
    let s = one(&pool).await;
    let e = env(pool.clone()).await;
    let (status, body) = put(
        &e,
        &artist_path(s.artist),
        json!({"heroUrl": e.url("/hero.jpg")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["coverArt"], json!(s.artist));
    assert_eq!(body["hasUserOverrides"], true);
    assert_eq!(
        artist_col(&pool, s.artist, "image_source").await.as_deref(),
        Some("user")
    );
    let stored = e
        .data
        .path()
        .join("artist-images")
        .join(format!("{}.jpg", s.artist));
    let bytes = std::fs::read(&stored).unwrap();
    assert!(image::load_from_memory(&bytes).is_ok());
    let res = e
        .srv
        .get(&format!("/api/library/cover-art/{}", s.artist))
        .authorization_bearer(&e.user)
        .await;
    res.assert_status_ok();
    assert_eq!(res.header("content-type"), "image/jpeg");

    let (status, body) = put(
        &e,
        &album_path(s.album),
        json!({"coverUrl": e.url("/cover.jpg")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hasUserOverrides"], true);
    assert_eq!(
        album_col(&pool, s.album, "image_source").await.as_deref(),
        Some("user")
    );
    assert!(
        e.data
            .path()
            .join("album-images")
            .join(format!("{}.jpg", s.album))
            .is_file()
    );
    let res = e
        .srv
        .get(&format!("/api/library/cover-art/{}", s.album))
        .authorization_bearer(&e.user)
        .await;
    res.assert_status_ok();
    assert_eq!(e.replay.asked().len(), 2);

    // The frontend sends back what the page showed: the entity's own id is no new image.
    let (status, _) = put(&e, &artist_path(s.artist), json!({"heroUrl": s.artist})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = put(&e, &album_path(s.album), json!({"coverUrl": s.album})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(e.replay.asked().len(), 2, "no download for the own id");
    assert_eq!(
        artist_col(&pool, s.artist, "image_source").await.as_deref(),
        Some("user")
    );

    // Clearing removes the user image and its file.
    let (status, _) = put(&e, &artist_path(s.artist), json!({"heroUrl": ""})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(artist_col(&pool, s.artist, "image_source").await, None);
    assert!(!stored.exists());
    // No temp file is left behind.
    let left: Vec<_> = std::fs::read_dir(e.data.path().join("artist-images"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hero_url_failure_400(pool: PgPool) {
    let s = one(&pool).await;
    let e = env(pool.clone()).await;
    let failed = json!({"error": "Could not fetch that image"});
    for url in [
        e.url("/missing.jpg?token=secret"),
        e.url("/page.html"),
        "ftp://example.org/x.jpg".to_owned(),
        "not a url".to_owned(),
    ] {
        let (status, body) = put(
            &e,
            &artist_path(s.artist),
            json!({"name": "Not written", "heroUrl": url}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}");
        assert_eq!(body, failed, "no URL or internal text in the answer");
        let (status, body) = put(&e, &album_path(s.album), json!({"coverUrl": url})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}");
        assert_eq!(body, failed);
    }
    assert_eq!(artist_col(&pool, s.artist, "display_name").await, None);
    assert_eq!(artist_col(&pool, s.artist, "image_source").await, None);
    assert_eq!(album_col(&pool, s.album, "image_source").await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hero_url_private_address_refused(pool: PgPool) {
    let s = one(&pool).await;
    // Production rule: the replay server listens on 127.0.0.1, which is not public.
    let e = env_with(pool.clone(), false).await;
    for url in [
        e.url("/hero.jpg"),
        "http://[::1]:9/x.jpg".to_owned(),
        "http://169.254.169.254/latest/meta-data".to_owned(),
        "http://10.0.0.1/x.jpg".to_owned(),
    ] {
        let (status, body) = put(&e, &artist_path(s.artist), json!({"heroUrl": url})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}");
        assert_eq!(body, json!({"error": "Could not fetch that image"}));
    }
    assert!(e.replay.asked().is_empty(), "asked {:?}", e.replay.asked());
    assert_eq!(artist_col(&pool, s.artist, "image_source").await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sort_name_follows_real_name(pool: PgPool) {
    // Upstream `enrichmentSortName.route.test.ts`, inverted (D-118): sort names stay generated
    // from the real name and title.
    let s = seed(
        &pool,
        Seed::new("The Beatles", "The Wall", Some(1979), "Song"),
    )
    .await;
    let e = env(pool.clone()).await;
    put(
        &e,
        &artist_path(s.artist),
        json!({"name": "The Artist Formerly Known As Prince"}),
    )
    .await;
    put(&e, &album_path(s.album), json!({"title": "Zebra"})).await;
    assert_eq!(
        artist_col(&pool, s.artist, "sort_name").await.as_deref(),
        Some("beatles")
    );
    assert_eq!(
        album_col(&pool, s.album, "sort_name").await.as_deref(),
        Some("wall")
    );
    assert_eq!(
        artist_col(&pool, s.artist, "name").await.as_deref(),
        Some("The Beatles")
    );
    assert_eq!(
        album_col(&pool, s.album, "title").await.as_deref(),
        Some("The Wall")
    );
    for name in [json!(""), json!(null)] {
        put(&e, &artist_path(s.artist), json!({"name": name})).await;
        assert_eq!(
            artist_col(&pool, s.artist, "sort_name").await.as_deref(),
            Some("beatles")
        );
    }
    post(
        &e,
        &e.admin,
        &format!("/api/enrichment/artists/{}/reset", s.artist),
    )
    .await;
    post(
        &e,
        &e.admin,
        &format!("/api/enrichment/albums/{}/reset", s.album),
    )
    .await;
    assert_eq!(
        artist_col(&pool, s.artist, "sort_name").await.as_deref(),
        Some("beatles")
    );
    assert_eq!(
        album_col(&pool, s.album, "sort_name").await.as_deref(),
        Some("wall")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reset_clears_overrides_user_mbids_and_images(pool: PgPool) {
    let s = one(&pool).await;
    let tagged = seed(&pool, Seed::new("Beta", "Second", Some(2000), "Other")).await;
    let e = env(pool.clone()).await;
    put(
        &e,
        &artist_path(s.artist),
        json!({"name": "N", "bio": "B", "genres": ["g"], "heroUrl": e.url("/hero.jpg"),
               "mbid": M1}),
    )
    .await;
    put(
        &e,
        &album_path(s.album),
        json!({"title": "T", "year": 2001, "genres": ["g"], "coverUrl": e.url("/cover.jpg"),
               "rgMbid": RG}),
    )
    .await;
    exec(
        &pool,
        "UPDATE albums SET release_mbid = gen_random_uuid(), release_mbid_source = 'user'
         WHERE id = $1",
        s.album,
    )
    .await;
    exec(
        &pool,
        "UPDATE artists SET enrichment_status = 'completed' WHERE id = $1",
        s.artist,
    )
    .await;
    // A tag MBID is not the user's: reset keeps it.
    sqlx::query(
        "UPDATE artists SET mbid = $2, mbid_source = 'tag', display_name = 'X' WHERE id = $1",
    )
    .bind(tagged.artist)
    .bind(Uuid::parse_str(M2).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let artist_file = e
        .data
        .path()
        .join("artist-images")
        .join(format!("{}.jpg", s.artist));
    let album_file = e
        .data
        .path()
        .join("album-images")
        .join(format!("{}.jpg", s.album));
    assert!(artist_file.is_file() && album_file.is_file());

    let (status, body) = post(
        &e,
        &e.admin,
        &format!("/api/enrichment/artists/{}/reset", s.artist),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Artist metadata reset to original values");
    assert_eq!(body["artist"]["id"], json!(s.artist));
    assert_eq!(body["artist"]["hasUserOverrides"], false);
    let (name, bio, genres) = artist_overrides(&pool, s.artist).await;
    assert_eq!((name, bio), (None, None));
    assert_eq!(genres, Some(Vec::new()), "user_genres = '{{}}'");
    assert_eq!(artist_col(&pool, s.artist, "mbid").await, None);
    assert_eq!(artist_col(&pool, s.artist, "mbid_source").await, None);
    assert_eq!(artist_col(&pool, s.artist, "image_source").await, None);
    assert_eq!(
        artist_col(&pool, s.artist, "enrichment_status")
            .await
            .as_deref(),
        Some("pending")
    );
    assert!(!artist_file.exists());

    let (status, body) = post(
        &e,
        &e.admin,
        &format!("/api/enrichment/albums/{}/reset", s.album),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Album metadata reset to original values");
    assert_eq!(body["album"]["id"], json!(s.album));
    assert_eq!(body["album"]["hasUserOverrides"], false);
    for col in [
        "display_title",
        "display_year",
        "release_mbid",
        "release_mbid_source",
        "release_group_mbid",
        "release_group_mbid_source",
        "image_path",
        "image_source",
    ] {
        assert_eq!(album_col(&pool, s.album, col).await, None, "{col}");
    }
    assert_eq!(
        album_col(&pool, s.album, "user_genres").await.as_deref(),
        Some("{}")
    );
    assert!(!album_file.exists());

    post(
        &e,
        &e.admin,
        &format!("/api/enrichment/artists/{}/reset", tagged.artist),
    )
    .await;
    assert_eq!(
        artist_col(&pool, tagged.artist, "mbid").await.as_deref(),
        Some(M2)
    );
    assert_eq!(
        artist_col(&pool, tagged.artist, "mbid_source")
            .await
            .as_deref(),
        Some("tag")
    );
    assert_eq!(artist_col(&pool, tagged.artist, "display_name").await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reset_needs_admin_403(pool: PgPool) {
    let s = one(&pool).await;
    let e = env(pool.clone()).await;
    let forbidden = json!({"error": "Admin access required"});
    for path in [
        format!("/api/enrichment/artists/{}/reset", s.artist),
        format!("/api/enrichment/albums/{}/reset", s.album),
    ] {
        let (status, body) = post(&e, &e.user, &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body, forbidden);
    }
    for path in [artist_path(s.artist), album_path(s.album)] {
        let res = e
            .srv
            .put(&path)
            .authorization_bearer(&e.user)
            .json(&json!({"name": "x", "title": "x"}))
            .await;
        assert_eq!(res.status_code(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(res.json::<Value>(), forbidden);
    }
    assert_eq!(artist_col(&pool, s.artist, "display_name").await, None);
    let res = e
        .srv
        .post(&format!("/api/enrichment/artists/{}/reset", s.artist))
        .await;
    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

/// Final review F1: a reset commits and answers 200 even when every track of the entity is
/// missing (the page reads present tracks only), from the reset row.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reset_with_every_track_missing_answers_200(pool: PgPool) {
    let s = one(&pool).await;
    exec(
        &pool,
        "UPDATE artists SET display_name = 'Shown' WHERE id = $1",
        s.artist,
    )
    .await;
    exec(
        &pool,
        "UPDATE albums SET display_title = 'Shown' WHERE id = $1",
        s.album,
    )
    .await;
    exec(
        &pool,
        "UPDATE tracks SET missing_since = now() WHERE album_id = $1",
        s.album,
    )
    .await;
    let e = env(pool.clone()).await;

    let (status, body) = post(
        &e,
        &e.admin,
        &format!("/api/enrichment/artists/{}/reset", s.artist),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "Artist metadata reset to original values");
    assert_eq!(body["artist"]["id"], s.artist.to_string());
    assert_eq!(body["artist"]["name"], "Alpha");
    assert_eq!(artist_col(&pool, s.artist, "display_name").await, None);

    let (status, body) = post(
        &e,
        &e.admin,
        &format!("/api/enrichment/albums/{}/reset", s.album),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "Album metadata reset to original values");
    assert_eq!(body["album"]["id"], s.album.to_string());
    assert_eq!(body["album"]["title"], "First");
    assert_eq!(album_col(&pool, s.album, "display_title").await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reset_unknown_404(pool: PgPool) {
    let e = env(pool.clone()).await;
    let missing = Uuid::new_v4().to_string();
    for id in [missing.as_str(), "not-a-uuid"] {
        let (status, body) =
            post(&e, &e.admin, &format!("/api/enrichment/artists/{id}/reset")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body, json!({"error": "Artist not found"}));
        let (status, body) =
            post(&e, &e.admin, &format!("/api/enrichment/albums/{id}/reset")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body, json!({"error": "Album not found"}));
        let (status, body) = put(&e, &artist_path(id), json!({"name": "x"})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body, json!({"error": "Artist not found"}));
        let (status, body) = put(&e, &album_path(id), json!({"title": "x"})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body, json!({"error": "Album not found"}));
    }
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut k: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    k.sort();
    k
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn put_answer_shapes(pool: PgPool) {
    let s = one(&pool).await;
    seed(
        &pool,
        Seed::new("Alpha", "First", Some(1999), "Song 2").at(1, 2),
    )
    .await;
    let e = env(pool.clone()).await;

    let (status, a) = put(&e, &artist_path(s.artist), json!({"name": "Shown"})).await;
    assert_eq!(status, StatusCode::OK);
    let detail = get(&e, &format!("/api/library/artists/{}", s.artist)).await;
    for key in keys(&detail) {
        assert!(a.get(&key).is_some(), "artist answer lacks {key}");
    }
    assert_eq!(a["displayName"], "Shown");
    assert_eq!(a["name"], "Alpha");
    let albums = a["albums"].as_array().unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(
        keys(&albums[0]),
        sorted(&["id", "title", "year", "coverUrl"])
    );
    assert_eq!(albums[0]["id"], json!(s.album));
    assert_eq!(albums[0]["title"], "First");
    assert_eq!(albums[0]["year"], 1999);

    let (status, al) = put(&e, &album_path(s.album), json!({"title": "Shown"})).await;
    assert_eq!(status, StatusCode::OK);
    let detail = get(&e, &format!("/api/library/albums/{}", s.album)).await;
    for key in keys(&detail) {
        assert!(al.get(&key).is_some(), "album answer lacks {key}");
    }
    assert_eq!(al["displayTitle"], "Shown");
    assert_eq!(al["artist"], json!({"id": s.artist, "name": "Alpha"}));
    let tracks = al["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);
    for t in tracks {
        assert_eq!(keys(t), sorted(&["id", "title", "trackNo", "duration"]));
    }
    assert_eq!(tracks[0]["title"], "Song");
    assert_eq!(tracks[0]["trackNo"], 1);
    assert_eq!(tracks[0]["duration"], 180);

    // Reset answers carry the same shapes.
    let (_, r) = post(
        &e,
        &e.admin,
        &format!("/api/enrichment/artists/{}/reset", s.artist),
    )
    .await;
    assert_eq!(keys(&r), sorted(&["message", "artist"]));
    assert_eq!(
        keys(&r["artist"]["albums"][0]),
        sorted(&["id", "title", "year", "coverUrl"])
    );
    let (_, r) = post(
        &e,
        &e.admin,
        &format!("/api/enrichment/albums/{}/reset", s.album),
    )
    .await;
    assert_eq!(keys(&r), sorted(&["message", "album"]));
    assert_eq!(
        r["album"]["artist"],
        json!({"id": s.artist, "name": "Alpha"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn input_bounds_400(pool: PgPool) {
    let s = one(&pool).await;
    let e = env(pool.clone()).await;
    let long = "x".repeat(501);
    let cases = [
        (
            artist_path(s.artist),
            json!({"name": long}),
            "name is too long",
        ),
        (
            artist_path(s.artist),
            json!({"name": 5}),
            "name must be a string",
        ),
        (
            artist_path(s.artist),
            json!({"bio": "b".repeat(20_001)}),
            "bio is too long",
        ),
        (
            artist_path(s.artist),
            json!({"genres": "rock"}),
            "genres must be a list of names",
        ),
        (
            artist_path(s.artist),
            json!({"genres": [1]}),
            "genres must be a list of names",
        ),
        (
            artist_path(s.artist),
            json!({"genres": (0..51).map(|i| format!("g{i}")).collect::<Vec<_>>()}),
            "too many genres",
        ),
        (
            artist_path(s.artist),
            json!({"genres": ["g".repeat(101)]}),
            "genre is too long",
        ),
        (
            artist_path(s.artist),
            json!({"heroUrl": format!("http://a.example/{}", "p".repeat(2048))}),
            "heroUrl is too long",
        ),
        (
            album_path(s.album),
            json!({"title": long}),
            "title is too long",
        ),
        (
            album_path(s.album),
            json!({"coverUrl": 7}),
            "coverUrl must be a string",
        ),
    ];
    for (path, body, message) in cases {
        let (status, answer) = put(&e, &path, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{message}");
        assert_eq!(answer, json!({"error": message}));
    }
    for path in [artist_path(s.artist), album_path(s.album)] {
        let res = e
            .srv
            .put(&path)
            .authorization_bearer(&e.admin)
            .content_type("application/json")
            .bytes("[1, 2]".into())
            .await;
        assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
    }
    assert!(e.replay.asked().is_empty());
}

/// What the frontend's editor sends back unchanged (D-129): the shown name, bio, year and
/// genres, and the page's own image id, are no overrides.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn frontend_round_trip_changes_nothing(pool: PgPool) {
    let s = one(&pool).await;
    exec(
        &pool,
        "UPDATE artists SET bio = 'Wiki bio' WHERE id = $1",
        s.artist,
    )
    .await;
    let e = env(pool.clone()).await;
    let (status, a) = put(
        &e,
        &artist_path(s.artist),
        json!({"name": "Alpha", "bio": "Wiki bio", "genres": [], "_originalName": "Alpha",
               "_hasUserOverrides": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(a["hasUserOverrides"], false);
    assert_eq!(artist_overrides(&pool, s.artist).await.0, None);
    assert_eq!(artist_overrides(&pool, s.artist).await.1, None);
    let (status, al) = put(
        &e,
        &album_path(s.album),
        json!({"title": "First", "year": 1999, "genres": [], "coverUrl": s.album,
               "mbid": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(al["hasUserOverrides"], false);

    // With user genres, the shown (merged) list sent back keeps them as they are.
    exec(
        &pool,
        "WITH g AS (INSERT INTO genres (id, name, key) VALUES (gen_random_uuid(), 'Rock', 'rock')
                    RETURNING id)
         INSERT INTO album_genres (album_id, genre_id, source, rank)
         SELECT $1, g.id, 'file', 0 FROM g",
        s.album,
    )
    .await;
    put(&e, &album_path(s.album), json!({"genres": ["Mine"]})).await;
    let shown = get(&e, &format!("/api/library/albums/{}", s.album)).await["genres"].clone();
    assert_eq!(shown, json!(["Mine", "Rock"]));
    put(&e, &album_path(s.album), json!({"genres": shown})).await;
    assert_eq!(
        album_col(&pool, s.album, "user_genres").await.as_deref(),
        Some("{Mine}")
    );
    assert!(e.replay.asked().is_empty());
}
