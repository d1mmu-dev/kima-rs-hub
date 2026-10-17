mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_db::PgPool;
use serde_json::Value;
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
