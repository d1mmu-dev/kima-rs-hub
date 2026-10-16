mod common;

use axum::http::StatusCode;
use kima_db::PgPool;
use serde_json::{Value, json};

/// The page's payloads for a disabled integration, by step.
fn disabled_steps() -> [(&'static str, Value); 3] {
    [
        (
            "lidarr",
            json!({ "url": "", "apiKey": "", "enabled": false }),
        ),
        (
            "audiobookshelf",
            json!({ "url": "", "apiKey": "", "enabled": false }),
        ),
        (
            "soulseek",
            json!({ "username": "", "password": "", "enabled": false }),
        ),
    ]
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn disabled_integration_steps_succeed(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    for (step, body) in disabled_steps() {
        let res = server
            .post(&format!("/api/onboarding/{step}"))
            .authorization_bearer(&token)
            .json(&body)
            .await;
        res.assert_status_ok();
        res.assert_json(&json!({ "success": true, "tested": false }));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn enabled_integration_steps_are_refused(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    for (step, name) in [
        ("lidarr", "Lidarr"),
        ("audiobookshelf", "Audiobookshelf"),
        ("soulseek", "Soulseek"),
    ] {
        let res = server
            .post(&format!("/api/onboarding/{step}"))
            .authorization_bearer(&token)
            .json(&json!({ "url": "http://x:1", "apiKey": "k", "enabled": true }))
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        res.assert_json(&json!({ "error": format!("{name} integration is not available yet") }));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn integration_steps_need_auth(pool: PgPool) {
    let server = common::server(pool);
    for (step, body) in disabled_steps() {
        server
            .post(&format!("/api/onboarding/{step}"))
            .json(&body)
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}

/// Upstream's `requireAdmin`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn integration_steps_need_an_admin(pool: PgPool) {
    let state = common::state(pool);
    common::user_with_token(&state, "ana").await;
    let (_, token) = common::user_with_token(&state, "ben").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .post("/api/onboarding/lidarr")
        .authorization_bearer(&token)
        .json(&json!({ "enabled": false }))
        .await;
    res.assert_status(StatusCode::FORBIDDEN);
    res.assert_json(&json!({ "error": "Admin access required" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn integration_step_without_enabled_is_invalid(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .post("/api/onboarding/lidarr")
        .authorization_bearer(&token)
        .json(&json!({ "url": "" }))
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(res.json::<Value>()["error"], "Invalid request");
}
