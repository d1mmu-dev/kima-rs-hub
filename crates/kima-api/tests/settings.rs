mod common;

use kima_db::PgPool;
use serde_json::{Value, json};

const KEYS: [&str; 8] = [
    "userId",
    "playbackQuality",
    "wifiOnly",
    "offlineEnabled",
    "maxCacheSizeMb",
    "shareLiveListening",
    "hasListenbrainzToken",
    "liveSharingAvailable",
];

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requires_auth(pool: PgPool) {
    let server = axum_test::TestServer::new(kima_api::router(common::state(pool))).unwrap();
    server
        .get("/api/settings")
        .await
        .assert_status_unauthorized();
    server
        .post("/api/settings")
        .json(&json!({}))
        .await
        .assert_status_unauthorized();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_defaults_without_row_and_without_writing(pool: PgPool) {
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let body: Value = server
        .get("/api/settings")
        .authorization_bearer(&token)
        .await
        .json();
    for k in KEYS {
        assert!(body.get(k).is_some(), "missing {k}");
    }
    assert_eq!(body["userId"], user.id.to_string());
    assert_eq!(body["playbackQuality"], "original");
    assert_eq!(body["wifiOnly"], false);
    assert_eq!(body["offlineEnabled"], false);
    assert_eq!(body["maxCacheSizeMb"], 10240);
    assert_eq!(body["shareLiveListening"], true);
    assert_eq!(body["hasListenbrainzToken"], false);
    assert_eq!(body["liveSharingAvailable"], false);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM user_settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "GET must not write");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn partial_post_keeps_other_fields(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let first: Value = server
        .post("/api/settings")
        .authorization_bearer(&token)
        .json(&json!({ "playbackQuality": "medium", "wifiOnly": true, "unknown": 1 }))
        .await
        .json();
    assert_eq!(first["playbackQuality"], "medium");
    assert_eq!(first["wifiOnly"], true);
    assert_eq!(first["maxCacheSizeMb"], 10240);
    let second: Value = server
        .post("/api/settings")
        .authorization_bearer(&token)
        .json(&json!({ "maxCacheSizeMb": 0, "shareLiveListening": false }))
        .await
        .json();
    assert_eq!(second["playbackQuality"], "medium");
    assert_eq!(second["wifiOnly"], true);
    assert_eq!(second["maxCacheSizeMb"], 0);
    assert_eq!(second["shareLiveListening"], false);
    let got: Value = server
        .get("/api/settings")
        .authorization_bearer(&token)
        .await
        .json();
    assert_eq!(got, second);
    assert!(got.get("unknown").is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn invalid_bodies_are_400_with_details(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    for bad in [
        json!({ "playbackQuality": "ultra" }),
        json!({ "maxCacheSizeMb": -1 }),
        json!({ "maxCacheSizeMb": 1.5 }),
        json!({ "wifiOnly": "yes" }),
        json!({ "listenbrainzToken": 5 }),
        json!([1]),
    ] {
        let res = server
            .post("/api/settings")
            .authorization_bearer(&token)
            .json(&bad)
            .await;
        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], "Invalid settings", "{bad}");
        assert!(body["details"].as_array().is_some_and(|d| !d.is_empty()));
    }
    // Nothing was stored by the failed posts.
    let got: Value = server
        .get("/api/settings")
        .authorization_bearer(&token)
        .await
        .json();
    assert_eq!(got["playbackQuality"], "original");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listenbrainz_token_is_ignored_and_not_echoed(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .post("/api/settings")
        .authorization_bearer(&token)
        .json(&json!({ "listenbrainzToken": "secret-token-123", "wifiOnly": true }))
        .await;
    res.assert_status_ok();
    let text = res.text();
    assert!(!text.contains("secret-token-123"));
    let body: Value = res.json();
    assert_eq!(body["hasListenbrainzToken"], false);
    assert!(body.get("listenbrainzToken").is_none());
    server
        .post("/api/settings")
        .authorization_bearer(&token)
        .json(&json!({ "listenbrainzToken": null }))
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn settings_are_per_user(pool: PgPool) {
    let state = common::state(pool);
    let (_, ana) = common::user_with_token(&state, "ana").await;
    let (_, ben) = common::user_with_token(&state, "ben").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .post("/api/settings")
        .authorization_bearer(&ana)
        .json(&json!({ "playbackQuality": "low" }))
        .await
        .assert_status_ok();
    let b: Value = server
        .get("/api/settings")
        .authorization_bearer(&ben)
        .await
        .json();
    assert_eq!(b["playbackQuality"], "original");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cleanup_stale_jobs_is_unrouted(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .post("/api/settings/cleanup-stale-jobs")
        .authorization_bearer(&token)
        .await
        .assert_status_not_found();
}
