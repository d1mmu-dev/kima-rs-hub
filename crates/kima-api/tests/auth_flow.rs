mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use kima_db::PgPool;
use serde_json::{Value, json};

async fn register(server: &TestServer, username: &str, password: &str) -> axum_test::TestResponse {
    server
        .post("/api/onboarding/register")
        .json(&json!({ "username": username, "password": password }))
        .await
}

async fn login(server: &TestServer, username: &str, password: &str) -> axum_test::TestResponse {
    server
        .post("/api/auth/login")
        .json(&json!({ "username": username, "password": password }))
        .await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fresh_install_status(pool: PgPool) {
    let res = common::server(pool).get("/api/onboarding/status").await;
    res.assert_status_ok();
    res.assert_json(
        &json!({ "needsOnboarding": true, "hasAccount": false, "registrationOpen": true }),
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn first_registration_makes_an_admin_who_can_use_the_api(pool: PgPool) {
    let server = common::server(pool);
    let res = register(&server, "ana", "secret1").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["user"]["username"], "ana");
    assert_eq!(body["user"]["role"], "admin");
    assert_eq!(body["user"]["onboardingComplete"], false);

    let token = body["token"].as_str().unwrap();
    let me: Value = server
        .get("/api/auth/me")
        .authorization_bearer(token)
        .await
        .json();
    assert_eq!(me["username"], "ana");
    assert_eq!(me["onboardingComplete"], false);
    assert_eq!(me["enrichmentSettings"], Value::Null);
    assert!(
        me["createdAt"].as_str().unwrap().contains('T'),
        "RFC 3339 timestamp"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn second_registration_is_refused_while_closed(pool: PgPool) {
    let server = common::server(pool);
    register(&server, "ana", "secret1").await.assert_status_ok();
    let res = register(&server, "ben", "secret1").await;
    res.assert_status(StatusCode::FORBIDDEN);
    res.assert_json(&json!({ "error": "Registration is closed" }));

    let status: Value = server.get("/api/onboarding/status").await.json();
    assert_eq!(
        status,
        json!({ "needsOnboarding": false, "hasAccount": true, "registrationOpen": false })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn registration_validates_input(pool: PgPool) {
    let server = common::server(pool);
    for (username, password) in [
        ("ab", "secret1"),
        (&"x".repeat(51)[..], "secret1"),
        ("ana", "12345"),
        ("   ab   ", "secret1"),
    ] {
        let res = register(&server, username, password).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(
            res.json::<Value>()["error"],
            "Invalid request",
            "{username:?}/{password:?}"
        );
    }
    let res = server
        .post("/api/onboarding/register")
        .text("{not json")
        .content_type("application/json")
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(res.json::<Value>()["error"], "Invalid request");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn registration_trims_the_username(pool: PgPool) {
    let server = common::server(pool);
    let body: Value = register(&server, "  ana  ", "secret1").await.json();
    assert_eq!(body["user"]["username"], "ana");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn login_checks_credentials(pool: PgPool) {
    let server = common::server(pool);
    register(&server, "Ana", "secret1").await.assert_status_ok();

    for (u, p) in [("Ana", "wrong"), ("nobody", "secret1")] {
        let res = login(&server, u, p).await;
        res.assert_status(StatusCode::UNAUTHORIZED);
        res.assert_json(&json!({ "error": "Invalid credentials" }));
    }

    let res = login(&server, " ana ", "secret1").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert!(body["token"].is_string());
    assert!(body["refreshToken"].is_string());
    assert_eq!(body["user"]["username"], "Ana");
    assert_eq!(body["user"]["role"], "admin");
    // The frontend routes to /onboarding off this flag right after login.
    assert_eq!(body["user"]["onboardingComplete"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn login_requires_both_fields(pool: PgPool) {
    let res = login(&common::server(pool), "", "").await;
    res.assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_issues_new_tokens(pool: PgPool) {
    let server = common::server(pool);
    register(&server, "ana", "secret1").await;
    let body: Value = login(&server, "ana", "secret1").await.json();

    let res = server
        .post("/api/auth/refresh")
        .json(&json!({ "refreshToken": body["refreshToken"] }))
        .await;
    res.assert_status_ok();
    let fresh: Value = res.json();
    let token = fresh["token"].as_str().unwrap();
    server
        .get("/api/auth/me")
        .authorization_bearer(token)
        .await
        .assert_status_ok();

    let res = server
        .post("/api/auth/refresh")
        .json(&json!({ "refreshToken": body["token"] }))
        .await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    res.assert_json(&json!({ "error": "Invalid refresh token" }));

    let res = server.post("/api/auth/refresh").json(&json!({})).await;
    res.assert_status(StatusCode::BAD_REQUEST);
    res.assert_json(&json!({ "error": "Refresh token required" }));
}

/// Review focus 2: revoking (bumping token_version) kills every outstanding token.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn revoked_tokens_stop_working(pool: PgPool) {
    let server = common::server(pool.clone());
    register(&server, "ana", "secret1").await;
    let body: Value = login(&server, "ana", "secret1").await.json();
    sqlx::query("UPDATE users SET token_version = token_version + 1")
        .execute(&pool)
        .await
        .unwrap();

    server
        .get("/api/auth/me")
        .authorization_bearer(body["token"].as_str().unwrap())
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let res = server
        .post("/api/auth/refresh")
        .json(&json!({ "refreshToken": body["refreshToken"] }))
        .await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    res.assert_json(&json!({ "error": "Token invalidated" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn completing_onboarding_shows_in_status(pool: PgPool) {
    let server = common::server(pool);
    let body: Value = register(&server, "ana", "secret1").await.json();
    let token = body["token"].as_str().unwrap();

    let status: Value = server
        .get("/api/onboarding/status")
        .authorization_bearer(token)
        .await
        .json();
    assert_eq!(status["needsOnboarding"], true);

    let res = server
        .post("/api/onboarding/complete")
        .authorization_bearer(token)
        .await;
    res.assert_status_ok();
    res.assert_json(&json!({ "success": true }));

    let status: Value = server
        .get("/api/onboarding/status")
        .authorization_bearer(token)
        .await
        .json();
    assert_eq!(
        status,
        json!({ "needsOnboarding": false, "hasAccount": true, "registrationOpen": false })
    );

    server
        .post("/api/onboarding/complete")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn status_with_a_bad_token_falls_back_to_the_basic_answer(pool: PgPool) {
    let server = common::server(pool);
    register(&server, "ana", "secret1").await;
    let status: Value = server
        .get("/api/onboarding/status")
        .authorization_bearer("garbage")
        .await
        .json();
    assert_eq!(
        status,
        json!({ "needsOnboarding": false, "hasAccount": true, "registrationOpen": false })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn logout_and_features(pool: PgPool) {
    let server = common::server(pool);
    server
        .post("/api/auth/logout")
        .await
        .assert_json(&json!({ "message": "Logged out" }));

    let body: Value = register(&server, "ana", "secret1").await.json();
    let token = body["token"].as_str().unwrap();
    server
        .get("/api/system/features")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server.get("/api/system/features").authorization_bearer(token).await.assert_json(&json!({
        "musicCNN": false, "vibeEmbeddings": false, "audiobookshelfEnabled": false, "fpcalc": false
    }));
}
