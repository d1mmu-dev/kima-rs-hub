mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_db::PgPool;
use serde_json::Value;

struct Env {
    server: TestServer,
    token: String,
}

async fn env(pool: PgPool) -> Env {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        token,
    }
}

async fn search(e: &Env, path: &str) -> Value {
    let res = e.server.get(path).authorization_bearer(&e.token).await;
    res.assert_status_ok();
    res.json()
}

fn len(v: &Value, key: &str) -> usize {
    v[key].as_array().unwrap().len()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn finds_artist_album_track(pool: PgPool) {
    seed(
        &pool,
        Seed::new(
            "Radiohead",
            "Kid A",
            Some(2000),
            "Everything In Its Right Place",
        ),
    )
    .await;
    seed(
        &pool,
        Seed::new("Other", "Unrelated", Some(2001), "Nothing"),
    )
    .await;
    let e = env(pool).await;

    let v = search(&e, "/api/search?q=radiohead").await;
    assert_eq!(v["artists"][0]["name"], "Radiohead");
    assert_eq!(v["artists"][0]["trackCount"], 1);
    assert_eq!(len(&v, "albums"), 1);
    assert_eq!(v["albums"][0]["title"], "Kid A");
    assert_eq!(v["albums"][0]["artist"]["name"], "Radiohead");
    assert_eq!(len(&v, "tracks"), 1);
    for k in ["audiobooks", "podcasts", "episodes"] {
        assert_eq!(len(&v, k), 0);
    }

    let v = search(&e, "/api/search?q=kid").await;
    assert_eq!(v["albums"][0]["title"], "Kid A");
    let v = search(&e, "/api/search?q=right+place").await;
    assert_eq!(v["tracks"][0]["title"], "Everything In Its Right Place");
    assert!(v["tracks"][0]["album"]["coverUrl"].is_string());

    let v = search(&e, "/api/search?q=radiohead&type=albums").await;
    assert_eq!(len(&v, "artists"), 0);
    assert_eq!(len(&v, "albums"), 1);
    assert_eq!(len(&v, "tracks"), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prefix_matches_partial_word(pool: PgPool) {
    seed(
        &pool,
        Seed::new("Radiohead", "Kid A", Some(2000), "Idioteque"),
    )
    .await;
    let e = env(pool).await;
    let v = search(&e, "/api/search?q=radioh").await;
    assert_eq!(v["artists"][0]["name"], "Radiohead");
    let v = search(&e, "/api/search?q=kid%20a").await;
    assert_eq!(len(&v, "albums"), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn case_and_accents(pool: PgPool) {
    seed(&pool, Seed::new("Beyoncé", "Lemonade", Some(2016), "Sorry")).await;
    let e = env(pool).await;
    for q in ["beyonce", "BEYONC%C3%89", "BeYoN"] {
        let v = search(&e, &format!("/api/search?q={q}")).await;
        assert_eq!(v["artists"][0]["name"], "Beyoncé", "query {q}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_tracks_excluded(pool: PgPool) {
    let gone = seed(&pool, Seed::new("Ghost", "Vanished", Some(2000), "Poof")).await;
    seed(&pool, Seed::new("Ghost", "Present", Some(2001), "Here")).await;
    mark_missing(&pool, gone.track).await;
    let e = env(pool).await;

    let v = search(&e, "/api/search?q=poof").await;
    assert_eq!(len(&v, "tracks"), 0);
    let v = search(&e, "/api/search?q=vanished").await;
    assert_eq!(len(&v, "albums"), 0);
    let v = search(&e, "/api/search?q=ghost").await;
    assert_eq!(len(&v, "artists"), 1);
    assert_eq!(v["artists"][0]["trackCount"], 1);
    assert_eq!(len(&v, "albums"), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn empty_query_400(pool: PgPool) {
    let e = env(pool).await;
    for path in ["/api/search", "/api/search?q=", "/api/search?q=%20%20"] {
        let res = e.server.get(path).authorization_bearer(&e.token).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert!(res.json::<Value>()["error"].is_string());
    }
    // Text with no searchable term is an empty result, not an error.
    let v = search(&e, "/api/search?q=%21%21").await;
    assert_eq!(len(&v, "tracks"), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requires_auth(pool: PgPool) {
    let e = env(pool).await;
    e.server
        .get("/api/search?q=a")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
