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
async fn empty_query_returns_empty_lists(pool: PgPool) {
    let e = env(pool).await;
    for path in ["/api/search", "/api/search?q=", "/api/search?q=%20%20"] {
        let v = search(&e, path).await;
        for k in [
            "artists",
            "albums",
            "tracks",
            "audiobooks",
            "podcasts",
            "episodes",
        ] {
            assert_eq!(len(&v, k), 0, "{path} {k}");
        }
    }
    let v = search(&e, "/api/search?q=%21%21").await;
    assert_eq!(len(&v, "tracks"), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn odd_queries_never_500(pool: PgPool) {
    seed(&pool, Seed::new("x", "A B", Some(2000), "a b")).await;
    let e = env(pool).await;
    let long = "a".repeat(2000);
    let cases = [
        "x -\"a b\"".to_string(),
        "-\"a b\"".to_string(),
        "\"unterminated".to_string(),
        "a & | b".to_string(),
        ":*".to_string(),
        long,
    ];
    for q in cases {
        let res = e
            .server
            .get("/api/search")
            .add_query_param("q", &q)
            .authorization_bearer(&e.token)
            .await;
        res.assert_status_ok();
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requires_auth(pool: PgPool) {
    let e = env(pool).await;
    e.server
        .get("/api/search?q=a")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// M-SYNC Task 10 (upstream `4338048e`, `nonAsciiNames.integration.test.ts`, intent only):
/// names in any script are found in any case, accents are folded, a decomposed accent reads as
/// the composed letter and curly apostrophes match straight ones. kima-rs's database is UTF-8
/// (`unaccent` + the `simple` configuration), so none of upstream's `SQL_ASCII` workarounds
/// apply; this pins the outcome.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_non_ascii_names_any_case(pool: PgPool) {
    seed(
        &pool,
        Seed::new("Beyoncé", "Lemonade", Some(2016), "Formation"),
    )
    .await;
    seed(
        &pool,
        Seed::new("ДДТ", "Актриса Весна", Some(1992), "Родина"),
    )
    .await;
    seed(&pool, Seed::new("東京事変", "大人", Some(2006), "群青日和")).await;
    seed(
        &pool,
        Seed::new("Guns N’ Roses", "Appetite", Some(1987), "Welcome"),
    )
    .await;
    seed(
        &pool,
        Seed::new("Zoë Keating", "Into the Trees", Some(2010), "Escape"),
    )
    .await;
    let e = env(pool).await;
    let artist = |v: &Value| -> Vec<String> {
        v["artists"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_string())
            .collect()
    };
    for (query, expected) in [
        ("beyonce", "Beyoncé"),
        ("BEYONCÉ", "Beyoncé"),
        ("beyoncé", "Beyoncé"),
        ("ддт", "ДДТ"),
        ("ДДТ", "ДДТ"),
        ("東京事変", "東京事変"),
        // Curly in the library, straight typed, and the reverse.
        ("guns n' roses", "Guns N’ Roses"),
        ("Guns N’ Roses", "Guns N’ Roses"),
        // `e` + U+0308 (decomposed) against the composed `ë`.
        ("zoe\u{308} keating", "Zoë Keating"),
        ("ZOË", "Zoë Keating"),
    ] {
        let v = search(&e, &format!("/api/search?q={}", urlencode(query))).await;
        assert_eq!(artist(&v), [expected], "{query:?}");
    }
}

fn urlencode(s: &str) -> String {
    s.bytes().fold(String::new(), |mut out, b| {
        if b.is_ascii_alphanumeric() {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
        out
    })
}
