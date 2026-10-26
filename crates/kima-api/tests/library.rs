mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::genres::{self, GenreSource};
use kima_net::testing::{Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use serde_json::{Value, json};
use uuid::Uuid;

struct Env {
    server: TestServer,
    admin: String,
    user: String,
}

async fn env(pool: PgPool) -> Env {
    let state = common::state(pool);
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let (_, user) = common::user_with_token(&state, "bob").await;
    Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        admin,
        user,
    }
}

async fn get(e: &Env, path: &str) -> Value {
    let res = e.server.get(path).authorization_bearer(&e.user).await;
    res.assert_status_ok();
    res.json()
}

fn has_keys(v: &Value, keys: &[&str]) {
    for k in keys {
        assert!(v.get(*k).is_some(), "missing key {k} in {v}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artists_list_has_frontend_keys(pool: PgPool) {
    let a = seed(&pool, Seed::new("Beta", "B1", Some(2000), "s1")).await;
    seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s2").at(1, 2)).await;
    seed(&pool, Seed::new("Alpha", "A2", Some(2001), "s3")).await;
    let e = env(pool).await;

    let v = get(&e, "/api/library/artists").await;
    has_keys(&v, &["artists", "total", "offset", "limit", "nextCursor"]);
    assert_eq!(v["total"], 2);
    assert_eq!(v["limit"], 50);
    let first = &v["artists"][0];
    has_keys(
        first,
        &[
            "id",
            "mbid",
            "name",
            "heroUrl",
            "coverArt",
            "albumCount",
            "trackCount",
        ],
    );
    assert_eq!(first["name"], "Alpha");
    assert_eq!(first["albumCount"], 2);
    assert_eq!(first["trackCount"], 3 - 1);
    assert_eq!(v["artists"][1]["id"], a.artist.to_string());

    let v = get(&e, "/api/library/artists?sortBy=name-desc").await;
    assert_eq!(v["artists"][0]["name"], "Beta");
    let v = get(&e, "/api/library/artists?query=alp").await;
    assert_eq!(v["total"], 1);
    let v = get(&e, "/api/library/artists?query=%25").await;
    assert_eq!(v["total"], 0, "wildcards in the query match themselves");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_detail_has_frontend_keys(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s1")).await;
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/artists/{}", s.artist)).await;
    has_keys(
        &v,
        &[
            "id",
            "name",
            "mbid",
            "coverArt",
            "bio",
            "genres",
            "albums",
            "topTracks",
            "similarArtists",
        ],
    );
    let album = &v["albums"][0];
    has_keys(
        album,
        &[
            "id",
            "title",
            "year",
            "coverArt",
            "trackCount",
            "tracks",
            "owned",
        ],
    );
    has_keys(
        &album["tracks"][0],
        &["id", "title", "duration", "trackNo", "album"],
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_page_albums_sorted_by_year(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "Late", Some(2010), "s1")).await;
    seed(&pool, Seed::new("Alpha", "Zeta", Some(1990), "s2")).await;
    seed(&pool, Seed::new("Alpha", "Early", Some(1990), "s3")).await;
    seed(&pool, Seed::new("Alpha", "Undated", None, "s4")).await;
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/artists/{}", a.artist)).await;
    let titles: Vec<&str> = v["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Early", "Zeta", "Late", "Undated"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn albums_list_and_detail_have_frontend_keys(pool: PgPool) {
    let a = seed(
        &pool,
        Seed::new("Alpha", "A1", Some(1999), "second").at(1, 2),
    )
    .await;
    seed(
        &pool,
        Seed::new("Alpha", "A1", Some(1999), "disc2").at(2, 1),
    )
    .await;
    seed(
        &pool,
        Seed::new("Alpha", "A1", Some(1999), "first").at(1, 1),
    )
    .await;
    seed(&pool, Seed::new("Beta", "B1", None, "x")).await;
    let e = env(pool).await;

    let v = get(&e, "/api/library/albums").await;
    has_keys(&v, &["albums", "total", "offset", "limit"]);
    assert_eq!(v["total"], 2);
    assert_eq!(v["limit"], 500);
    has_keys(
        &v["albums"][0],
        &[
            "id",
            "title",
            "year",
            "coverArt",
            "coverUrl",
            "artist",
            "trackCount",
        ],
    );
    assert_eq!(v["albums"][0]["artist"]["name"], "Alpha");
    assert_eq!(
        v["albums"][0]["coverArt"],
        a.album.to_string(),
        "the bare id api.getCoverArtUrl turns into /api/library/cover-art/<id>"
    );

    let v = get(&e, &format!("/api/library/albums?artistId={}", a.artist)).await;
    assert_eq!(v["total"], 1);
    let v = get(&e, "/api/library/albums?artistId=nope").await;
    assert_eq!(v["total"], 0);

    let v = get(&e, &format!("/api/library/albums/{}", a.album)).await;
    has_keys(
        &v,
        &[
            "id",
            "title",
            "artist",
            "year",
            "coverArt",
            "owned",
            "tracks",
            "missingTracks",
        ],
    );
    let titles: Vec<&str> = v["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["first", "second", "disc2"],
        "disc, then track number"
    );
    has_keys(
        &v["tracks"][0],
        &[
            "id",
            "title",
            "duration",
            "trackNo",
            "discNumber",
            "artist",
            "album",
        ],
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tracks_list_shuffle_and_detail(pool: PgPool) {
    let a = seed(
        &pool,
        Seed::new("Alpha", "A1", Some(1999), "bravo").at(1, 2),
    )
    .await;
    seed(
        &pool,
        Seed::new("Alpha", "A1", Some(1999), "alpha").at(1, 1),
    )
    .await;
    let e = env(pool).await;

    let v = get(&e, "/api/library/tracks").await;
    has_keys(&v, &["tracks", "total", "offset", "limit"]);
    assert_eq!(v["limit"], 100);
    assert_eq!(v["tracks"][0]["title"], "alpha");
    has_keys(
        &v["tracks"][0],
        &[
            "id",
            "title",
            "duration",
            "trackNumber",
            "discNumber",
            "album",
        ],
    );
    has_keys(
        &v["tracks"][0]["album"],
        &["id", "title", "coverArt", "artist"],
    );
    let v = get(&e, "/api/library/tracks?sortBy=name-desc").await;
    assert_eq!(v["tracks"][0]["title"], "bravo");
    let v = get(&e, &format!("/api/library/tracks?albumId={}", a.album)).await;
    assert_eq!(
        v["tracks"][0]["title"], "alpha",
        "an album's tracks go by track number"
    );

    let v = get(&e, "/api/library/tracks/shuffle?limit=1").await;
    assert_eq!(v["total"], 2);
    assert_eq!(v["tracks"].as_array().unwrap().len(), 1);
    has_keys(&v["tracks"][0], &["id", "title", "duration", "album"]);

    let v = get(&e, &format!("/api/library/tracks/{}", a.track)).await;
    has_keys(&v, &["id", "title", "artist", "album", "duration"]);
    assert_eq!(v["artist"]["name"], "Alpha");
    assert_eq!(v["album"]["id"], a.album.to_string());
    assert_eq!(v["duration"], 180);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recently_added_lists_artists_newest_first(pool: PgPool) {
    seed(&pool, Seed::new("Old", "O1", Some(1990), "s1")).await;
    seed(&pool, Seed::new("New", "N1", Some(2020), "s2")).await;
    let e = env(pool).await;
    let v = get(&e, "/api/library/recently-added?limit=10").await;
    let artists = v["artists"].as_array().unwrap();
    assert_eq!(artists.len(), 2);
    assert_eq!(artists[0]["name"], "New");
    has_keys(
        &artists[0],
        &["id", "name", "mbid", "coverArt", "heroUrl", "albumCount"],
    );
    let v = get(&e, "/api/library/recently-added?limit=1").await;
    assert_eq!(v["artists"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_tracks_hidden(pool: PgPool) {
    let gone = seed(&pool, Seed::new("Ghost", "G1", Some(2000), "only")).await;
    let kept = seed(&pool, Seed::new("Alpha", "A1", Some(2000), "kept").at(1, 1)).await;
    let hidden = seed(
        &pool,
        Seed::new("Alpha", "A1", Some(2000), "hidden").at(1, 2),
    )
    .await;
    mark_missing(&pool, gone.track).await;
    mark_missing(&pool, hidden.track).await;
    let e = env(pool).await;

    let v = get(&e, "/api/library/artists").await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["artists"][0]["trackCount"], 1);
    assert_eq!(get(&e, "/api/library/albums").await["total"], 1);
    assert_eq!(get(&e, "/api/library/tracks").await["total"], 1);
    let v = get(&e, "/api/library/tracks/shuffle").await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["tracks"][0]["id"], kept.track.to_string());
    let v = get(&e, "/api/library/recently-added").await;
    assert_eq!(v["artists"].as_array().unwrap().len(), 1);
    let v = get(&e, &format!("/api/library/albums/{}", kept.album)).await;
    assert_eq!(v["tracks"].as_array().unwrap().len(), 1);
    for path in [
        format!("/api/library/artists/{}", gone.artist),
        format!("/api/library/albums/{}", gone.album),
        format!("/api/library/tracks/{}", hidden.track),
    ] {
        e.server
            .get(&path)
            .authorization_bearer(&e.user)
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn paging_limits_and_offsets(pool: PgPool) {
    for i in 1..=5 {
        seed(
            &pool,
            Seed::new("Alpha", "A1", Some(2000), &format!("t{i}")).at(1, i),
        )
        .await;
    }
    let e = env(pool).await;
    let v = get(&e, "/api/library/tracks?limit=2&offset=1").await;
    assert_eq!(v["total"], 5);
    assert_eq!(v["limit"], 2);
    assert_eq!(v["offset"], 1);
    let titles: Vec<&str> = v["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["t2", "t3"]);
    // As upstream: junk takes the default, a huge limit is capped at 10000.
    let v = get(&e, "/api/library/tracks?limit=abc&offset=-3").await;
    assert_eq!(v["limit"], 100);
    assert_eq!(v["offset"], 0);
    let v = get(&e, "/api/library/tracks?limit=999999").await;
    assert_eq!(v["limit"], 10000);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn scan_requires_admin(pool: PgPool) {
    let e = env(pool.clone()).await;
    e.server
        .post("/api/library/scan")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let res = e
        .server
        .post("/api/library/scan")
        .authorization_bearer(&e.user)
        .await;
    res.assert_status(StatusCode::FORBIDDEN);
    res.assert_json(&serde_json::json!({"error": "Admin access required"}));

    let res = e
        .server
        .post("/api/library/scan")
        .authorization_bearer(&e.admin)
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    let id = body["jobId"].as_str().unwrap().to_string();
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE id = $1 AND queue = 'library'")
            .bind(id.parse::<Uuid>().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload, serde_json::json!({"kind": "reconcile"}));

    // A second click while the first is pending returns the same job.
    let again: Value = e
        .server
        .post("/api/library/scan")
        .authorization_bearer(&e.admin)
        .await
        .json();
    assert_eq!(again["jobId"], body["jobId"]);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1);

    // Status follows the job table.
    let v = get(&e, &format!("/api/library/scan/status/{id}")).await;
    assert_eq!(v["status"], "waiting");
    for (state, status, progress) in [("running", "active", 0), ("done", "completed", 100)] {
        sqlx::query("UPDATE jobs SET state = $1")
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        let v = get(&e, &format!("/api/library/scan/status/{id}")).await;
        assert_eq!(v["status"], status);
        assert_eq!(v["progress"], progress);
    }
    sqlx::query("UPDATE jobs SET state = 'failed'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&e, &format!("/api/library/scan/status/{id}")).await["status"],
        "failed"
    );
    // Once it is done a new click starts a new job.
    sqlx::query("UPDATE jobs SET state = 'done'")
        .execute(&pool)
        .await
        .unwrap();
    let next: Value = e
        .server
        .post("/api/library/scan")
        .authorization_bearer(&e.admin)
        .await
        .json();
    assert_ne!(next["jobId"], body["jobId"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_id_404(pool: PgPool) {
    let e = env(pool).await;
    let missing = Uuid::now_v7();
    for path in [
        format!("/api/library/artists/{missing}"),
        format!("/api/library/albums/{missing}"),
        format!("/api/library/tracks/{missing}"),
        format!("/api/library/scan/status/{missing}"),
        "/api/library/artists/not-a-uuid".to_string(),
        "/api/library/albums/not-a-uuid".to_string(),
        "/api/library/tracks/not-a-uuid".to_string(),
        "/api/library/scan/status/not-a-uuid".to_string(),
    ] {
        let res = e.server.get(&path).authorization_bearer(&e.user).await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert!(res.json::<Value>()["error"].is_string(), "{path}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn browse_requires_login(pool: PgPool) {
    let e = env(pool).await;
    for path in [
        "/api/library/artists",
        "/api/library/albums",
        "/api/library/tracks",
    ] {
        e.server
            .get(path)
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}

// ---- M3a: identity, genres, images and similar artists on the detail pages (spec §16.1) ----

const MBID_A: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const MBID_B: &str = "0383dadf-2a4e-4d10-a46a-e9e041da8eb3";

async fn exec(pool: &PgPool, sql: &str, id: Uuid) {
    sqlx::query(sql).bind(id).execute(pool).await.unwrap();
}

async fn set_similar(pool: &PgPool, artist: Uuid, similar: Value) {
    sqlx::query("UPDATE artists SET \"similar\" = $2 WHERE id = $1")
        .bind(artist)
        .bind(similar)
        .execute(pool)
        .await
        .unwrap();
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

async fn artist_genres(pool: &PgPool, artist: Uuid, source: GenreSource, g: &[&str]) {
    let mut conn = pool.acquire().await.unwrap();
    genres::replace_artist_genres(&mut conn, artist, source, &names(g))
        .await
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_detail_filled(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s1")).await;
    let b = seed(&pool, Seed::new("Beta", "B1", Some(2001), "s2")).await;
    sqlx::query(
        "UPDATE artists SET mbid = $2, mbid_source = 'tag', bio = 'From Wikipedia.',
                image_path = 'x.jpg', image_source = 'wikidata', display_name = 'ALPHA',
                user_genres = '{Shoegaze}'
         WHERE id = $1",
    )
    .bind(a.artist)
    .bind(Uuid::parse_str(MBID_A).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    artist_genres(
        &pool,
        a.artist,
        GenreSource::MusicBrainz,
        &["rock", "indie"],
    )
    .await;
    artist_genres(&pool, a.artist, GenreSource::File, &["pop"]).await;
    set_similar(
        &pool,
        a.artist,
        json!([{"name": "Beta", "mbid": null, "match": 0.9}]),
    )
    .await;
    let e = env(pool).await;

    let v = get(&e, &format!("/api/library/artists/{}", a.artist)).await;
    let id = a.artist.to_string();
    assert_eq!(v["mbid"], MBID_A);
    assert_eq!(v["bio"], "From Wikipedia.");
    assert_eq!(v["summary"], "From Wikipedia.");
    assert_eq!(v["userSummary"], Value::Null);
    assert_eq!(v["coverArt"], id);
    assert_eq!(v["image"], id);
    assert_eq!(v["heroUrl"], id);
    assert_eq!(v["userHeroUrl"], Value::Null);
    assert_eq!(v["genres"], json!(["Shoegaze", "rock", "indie"]));
    assert_eq!(v["userGenres"], json!(["Shoegaze"]));
    assert_eq!(v["tags"], json!(["rock", "indie"]));
    assert_eq!(v["displayName"], "ALPHA");
    assert_eq!(
        v["name"], "Alpha",
        "the display name does not rename the artist"
    );
    assert_eq!(v["hasUserOverrides"], true);
    assert_eq!(v["topTracks"], json!([]), "topTracks stay as M1 (D-048)");
    assert_eq!(v["listeners"], 0);
    assert_eq!(
        v["similarArtists"],
        json!([{
            "id": b.artist.to_string(),
            "name": "Beta",
            "mbid": null,
            "coverArt": null,
            "albumCount": 0,
            "ownedAlbumCount": 1,
            "weight": 0.9,
            "inLibrary": true,
        }])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_bio_prefers_user_summary(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s1")).await;
    exec(
        &pool,
        "UPDATE artists SET bio = 'wiki', user_bio = 'mine' WHERE id = $1",
        a.artist,
    )
    .await;
    // No MusicBrainz genres: the effective genres are the file's, and `tags` is empty.
    artist_genres(&pool, a.artist, GenreSource::File, &["jazz"]).await;
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/artists/{}", a.artist)).await;
    assert_eq!(v["bio"], "mine");
    assert_eq!(v["summary"], "wiki");
    assert_eq!(v["userSummary"], "mine");
    assert_eq!(v["hasUserOverrides"], true);
    assert_eq!(v["genres"], json!(["jazz"]));
    assert_eq!(v["tags"], json!([]));
    assert_eq!(v["userGenres"], json!([]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_cover_art_is_id_only_with_image(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s1")).await;
    let e = env(pool.clone()).await;
    let path = format!("/api/library/artists/{}", a.artist);
    let v = get(&e, &path).await;
    for k in ["coverArt", "image", "heroUrl", "userHeroUrl"] {
        assert_eq!(v[k], Value::Null, "{k} without an image");
    }
    exec(
        &pool,
        "UPDATE artists SET image_path = 'x.jpg', image_source = 'local' WHERE id = $1",
        a.artist,
    )
    .await;
    let v = get(&e, &path).await;
    for k in ["coverArt", "image", "heroUrl"] {
        assert_eq!(v[k], a.artist.to_string(), "{k} with an image");
    }
    assert_eq!(v["userHeroUrl"], Value::Null);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artists_list_cover_art(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s1")).await;
    seed(&pool, Seed::new("Beta", "B1", Some(1999), "s2")).await;
    exec(
        &pool,
        "UPDATE artists SET image_path = 'x.jpg', image_source = 'deezer', mbid = gen_random_uuid()
         WHERE id = $1",
        a.artist,
    )
    .await;
    let e = env(pool).await;
    let v = get(&e, "/api/library/artists").await;
    let alpha = &v["artists"][0];
    assert_eq!(alpha["name"], "Alpha");
    assert_eq!(alpha["coverArt"], a.artist.to_string());
    assert_eq!(alpha["heroUrl"], a.artist.to_string());
    assert!(alpha["mbid"].is_string());
    let beta = &v["artists"][1];
    assert_eq!(beta["coverArt"], Value::Null);
    assert_eq!(beta["heroUrl"], Value::Null);
    assert_eq!(beta["mbid"], Value::Null);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn similar_resolved_by_mbid_then_name(pool: PgPool) {
    let me = seed(&pool, Seed::new("Alpha", "A1", None, "s1")).await;
    let beyonce = seed(&pool, Seed::new("Beyonce", "B1", None, "s2")).await;
    seed(&pool, Seed::new("Beyonce", "B2", None, "s3")).await;
    let gamma = seed(&pool, Seed::new("Gamma", "G1", None, "s4")).await;
    exec(
        &pool,
        &format!("UPDATE artists SET mbid = '{MBID_B}' WHERE id = $1"),
        gamma.artist,
    )
    .await;
    set_similar(
        &pool,
        me.artist,
        json!([
            {"name": "Gamma (renamed)", "mbid": MBID_B, "match": 0.95},
            {"name": "Beyoncé", "mbid": null, "match": 0.8},
            // The MBID wins over a name that would match another library artist.
            {"name": "Beyonce", "mbid": MBID_B, "match": 0.7},
            {"name": "Nobody Here", "mbid": MBID_A, "match": 0.6},
        ]),
    )
    .await;
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/artists/{}", me.artist)).await;
    let s = v["similarArtists"].as_array().unwrap();
    assert_eq!(s.len(), 4);
    assert_eq!(s[0]["id"], gamma.artist.to_string());
    assert_eq!(s[0]["name"], "Gamma (renamed)");
    assert_eq!(s[0]["mbid"], MBID_B);
    assert_eq!(s[0]["inLibrary"], true);
    assert_eq!(s[0]["ownedAlbumCount"], 1);
    assert_eq!(s[1]["id"], beyonce.artist.to_string());
    assert_eq!(s[1]["name"], "Beyoncé");
    assert_eq!(s[1]["inLibrary"], true);
    assert_eq!(s[1]["ownedAlbumCount"], 2);
    assert_eq!(s[1]["weight"], 0.8);
    assert_eq!(s[2]["id"], gamma.artist.to_string());
    assert_eq!(s[3]["id"], "Nobody Here");
    assert_eq!(s[3]["mbid"], MBID_A);
    assert_eq!(s[3]["inLibrary"], false);
    assert_eq!(s[3]["ownedAlbumCount"], 0);
    assert_eq!(s[3]["albumCount"], 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn similar_outside_library_has_no_image(pool: PgPool) {
    let me = seed(&pool, Seed::new("Alpha", "A1", None, "s1")).await;
    let pictured = seed(&pool, Seed::new("Pictured", "P1", None, "s2")).await;
    seed(&pool, Seed::new("Plain", "P2", None, "s3")).await;
    exec(
        &pool,
        "UPDATE artists SET image_path = 'x.jpg', image_source = 'wikidata' WHERE id = $1",
        pictured.artist,
    )
    .await;
    set_similar(
        &pool,
        me.artist,
        json!([
            {"name": "Pictured", "mbid": null, "match": 0.9},
            {"name": "Plain", "mbid": null, "match": 0.8},
            {"name": "Elsewhere", "mbid": MBID_A, "match": 0.7},
        ]),
    )
    .await;
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/artists/{}", me.artist)).await;
    let s = &v["similarArtists"];
    assert_eq!(s[0]["coverArt"], pictured.artist.to_string());
    assert_eq!(s[1]["coverArt"], Value::Null, "in the library, no image");
    assert_eq!(
        s[2]["coverArt"],
        Value::Null,
        "outside the library: no image"
    );
    assert_eq!(s[2]["inLibrary"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn similar_first_10(pool: PgPool) {
    let me = seed(&pool, Seed::new("Alpha", "A1", None, "s1")).await;
    let list: Vec<Value> = (0..15)
        .map(|i| json!({"name": format!("Other {i}"), "mbid": null, "match": 0.9 - f64::from(i) * 0.01}))
        .collect();
    set_similar(&pool, me.artist, Value::Array(list)).await;
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/artists/{}", me.artist)).await;
    let s = v["similarArtists"].as_array().unwrap();
    assert_eq!(s.len(), 10);
    for (i, x) in s.iter().enumerate() {
        assert_eq!(x["name"], format!("Other {i}"));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn page_view_makes_no_request(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s1")).await;
    let replay = Replay::start(Recording(Vec::new())).await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    let state = common::state(pool.clone())
        .with_identify(IdentifySwitch::new(false, true))
        .with_net(Arc::new(net));
    let (_, token) = common::user_with_token(&state, "bob").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for path in [
        format!("/api/library/artists/{}", s.artist),
        "/api/library/artists".to_owned(),
        format!("/api/library/albums/{}", s.album),
        format!("/api/library/tracks/{}", s.track),
    ] {
        let res = server.get(&path).authorization_bearer(&token).await;
        res.assert_status_ok();
        if path.contains("/artists/") {
            let v: Value = res.json();
            assert_eq!(v["similarArtists"], json!([]));
            assert_eq!(v["coverArt"], Value::Null);
            assert_eq!(v["hasUserOverrides"], false);
        }
    }
    assert!(replay.asked().is_empty(), "asked {:?}", replay.asked());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_detail_filled(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s1")).await;
    sqlx::query(
        "UPDATE albums SET release_mbid = $2, release_group_mbid = $3, label = 'Factory',
                display_title = 'A One', display_year = 2001, user_genres = '{Dream Pop}'
         WHERE id = $1",
    )
    .bind(s.album)
    .bind(Uuid::parse_str(MBID_A).unwrap())
    .bind(Uuid::parse_str(MBID_B).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    {
        let mut conn = pool.acquire().await.unwrap();
        genres::replace_album_genres(
            &mut conn,
            s.album,
            GenreSource::MusicBrainz,
            &names(&["post-punk"]),
        )
        .await
        .unwrap();
        genres::replace_album_genres(&mut conn, s.album, GenreSource::File, &names(&["rock"]))
            .await
            .unwrap();
    }
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/albums/{}", s.album)).await;
    assert_eq!(v["mbid"], MBID_A);
    assert_eq!(v["rgMbid"], MBID_B);
    assert_eq!(v["label"], "Factory");
    assert_eq!(v["displayTitle"], "A One");
    assert_eq!(v["displayYear"], 2001);
    assert_eq!(v["title"], "A1");
    assert_eq!(v["year"], 1999);
    assert_eq!(v["genres"], json!(["Dream Pop", "post-punk"]));
    assert_eq!(v["userGenres"], json!(["Dream Pop"]));
    assert_eq!(v["userCoverUrl"], Value::Null);
    assert_eq!(v["hasUserOverrides"], true);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_detail_has_recording_isrc_genres(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", Some(1999), "s1")).await;
    sqlx::query(
        "UPDATE tracks SET recording_mbid = $2, recording_source = 'isrc',
                isrc = 'USABC1234567', isrc_source = 'id3'
         WHERE id = $1",
    )
    .bind(s.track)
    .bind(Uuid::parse_str(MBID_A).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    {
        let mut conn = pool.acquire().await.unwrap();
        genres::replace_track_genres(
            &mut conn,
            s.track,
            GenreSource::File,
            &names(&["rock", "pop"]),
        )
        .await
        .unwrap();
    }
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/tracks/{}", s.track)).await;
    assert_eq!(v["recordingMbid"], MBID_A);
    assert_eq!(v["isrc"], "USABC1234567");
    assert_eq!(v["genres"], json!(["rock", "pop"]));
    // Track lists carry the identity too.
    let v = get(&e, &format!("/api/library/albums/{}", s.album)).await;
    assert_eq!(v["tracks"][0]["recordingMbid"], MBID_A);
    assert_eq!(v["tracks"][0]["isrc"], "USABC1234567");
    let v = get(&e, "/api/library/tracks").await;
    assert_eq!(v["tracks"][0]["recordingMbid"], MBID_A);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn similar_names_fold_ampersand_and_punctuation(pool: PgPool) {
    let me = seed(&pool, Seed::new("Alpha", "A1", None, "s1")).await;
    let hall = seed(&pool, Seed::new("Hall & Oates", "H1", None, "s2")).await;
    let bangs = seed(&pool, Seed::new("!!!", "X1", None, "s3")).await;
    let sigur = seed(&pool, Seed::new("Sigur Rós", "S1", None, "s4")).await;
    let gone = seed(&pool, Seed::new("Gone", "G1", None, "s5")).await;
    mark_missing(&pool, gone.track).await;
    set_similar(
        &pool,
        me.artist,
        json!([
            {"name": "Hall and Oates", "mbid": null, "match": 0.9},
            {"name": "!!!", "mbid": null, "match": 0.8},
            {"name": "  SIGUR ROS ", "mbid": null, "match": 0.7},
            // An artist with no present track is not in the library.
            {"name": "Gone", "mbid": null, "match": 0.6},
            // Unreadable items are skipped; a bad MBID reads as none.
            {"name": "", "mbid": null, "match": 0.5},
            {"mbid": null, "match": 0.5},
            {"name": "Odd", "mbid": "not-an-mbid", "match": 0.4},
            "junk",
        ]),
    )
    .await;
    let e = env(pool).await;
    let v = get(&e, &format!("/api/library/artists/{}", me.artist)).await;
    let s = v["similarArtists"].as_array().unwrap();
    let ids: Vec<&str> = s.iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            hall.artist.to_string().as_str(),
            bangs.artist.to_string().as_str(),
            sigur.artist.to_string().as_str(),
            "Gone",
            "Odd",
        ]
    );
    assert_eq!(s[3]["inLibrary"], false);
    assert_eq!(s[4]["mbid"], Value::Null);
}

/// A name that matches many artists through the search vector ("The") reads at most
/// `MAX_CANDIDATES` rows, the exact identity-key match first even though it has the highest id.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn similar_candidates_are_capped_with_exact_matches_first(pool: PgPool) {
    use kima_ident::similar::{Entry, MAX_CANDIDATES, candidates, resolve};

    for n in 0..MAX_CANDIDATES + 50 {
        seed(&pool, Seed::new(&format!("The Band {n}"), "A", None, "t")).await;
    }
    let exact = seed(&pool, Seed::new("The", "T1", None, "t1")).await;
    seed(&pool, Seed::new("The", "T2", None, "t2")).await;
    let entries = [Entry {
        name: "The".into(),
        mbid: None,
        r#match: 0.9,
    }];

    let found = candidates(&pool, &entries).await.unwrap();
    assert_eq!(found.len() as i64, MAX_CANDIDATES);
    assert_eq!(found[0].id, exact.artist);

    let resolved = resolve(&pool, &entries, 10).await.unwrap();
    assert_eq!(resolved[0].id, exact.artist.to_string());
    assert!(resolved[0].in_library);
    assert_eq!(resolved[0].owned_album_count, 2);
}
