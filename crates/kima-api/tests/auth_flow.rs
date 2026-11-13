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

/// The `podcasts` flag (M6 Task 20) is on only because every route of upstream's
/// `routes/podcasts.ts` (v1.12.3, 24 registrations) is served: an authenticated request reaches
/// a handler, never the API's "Not found" fallback or a 405 for a missing method. Unknown ids
/// and the disabled test network answer the handlers' own errors. The single-segment routes
/// that `GET /podcasts/{id}` would also match must give their own handler's exact answer.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn every_upstream_podcast_route_is_served(pool: PgPool) {
    let server = common::server(pool);
    let body: Value = register(&server, "ana", "secret1").await.json();
    let token = body["token"].as_str().unwrap();
    let id = "00000000-0000-7000-8000-000000000001";
    let ep = "00000000-0000-7000-8000-000000000002";
    let routes: Vec<(&str, String)> = vec![
        ("GET", "/api/podcasts".into()),
        ("GET", "/api/podcasts/categories".into()),
        ("GET", "/api/podcasts/charts".into()),
        ("GET", "/api/podcasts/for-you".into()),
        ("GET", "/api/podcasts/listening".into()),
        ("GET", "/api/podcasts/discover/top".into()),
        ("GET", "/api/podcasts/discover/genres?genres=1495".into()),
        ("GET", "/api/podcasts/discover/genre/1495".into()),
        ("GET", "/api/podcasts/preview/555".into()),
        ("GET", "/api/podcasts/preview/555/snippet".into()),
        ("GET", "/api/podcasts/preview/555/similar".into()),
        ("POST", "/api/podcasts/refresh-all".into()),
        ("GET", format!("/api/podcasts/{id}")),
        ("GET", format!("/api/podcasts/{id}/episodes")),
        ("GET", format!("/api/podcasts/{id}/episodes/{ep}/next")),
        ("POST", "/api/podcasts/subscribe".into()),
        ("DELETE", format!("/api/podcasts/{id}/unsubscribe")),
        ("GET", format!("/api/podcasts/{id}/refresh")),
        ("GET", format!("/api/podcasts/{id}/episodes/{ep}/stream")),
        ("POST", format!("/api/podcasts/{id}/episodes/{ep}/progress")),
        (
            "DELETE",
            format!("/api/podcasts/{id}/episodes/{ep}/progress"),
        ),
        ("GET", format!("/api/podcasts/{id}/similar")),
        ("GET", format!("/api/podcasts/{id}/cover")),
        ("GET", format!("/api/podcasts/episodes/{ep}/cover")),
    ];
    assert_eq!(routes.len(), 24, "upstream podcasts.ts registers 24 routes");
    for (method, path) in &routes {
        let req = match *method {
            "GET" => server.get(path),
            "POST" => server.post(path).json(&json!({})),
            "DELETE" => server.delete(path),
            other => unreachable!("{other}"),
        };
        let res = req.authorization_bearer(token).await;
        let status = res.status_code();
        assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        let text = res.text();
        let fallback =
            serde_json::from_str::<Value>(&text).is_ok_and(|v| v == json!({"error": "Not found"}));
        assert!(!fallback, "{method} {path} reached the fallback ({status})");
    }
    // The detector itself: a path no route serves does reach the fallback.
    let res = server
        .get(&format!("/api/podcasts/{id}/no-such-route"))
        .authorization_bearer(token)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>(), json!({"error": "Not found"}));
    // `categories`, `charts`, `for-you` and `listening` also fit `GET /podcasts/{id}`: without
    // their own registration the show handler would answer them (a 404, never the fallback),
    // so each must give its own handler's answer. The test network is off, so the three
    // Apple-backed ones answer their Apple failure.
    let res = server
        .get("/api/podcasts/no-such-segment")
        .authorization_bearer(token)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    let show_handler = res.json::<Value>();
    assert_ne!(show_handler, json!({"error": "Not found"}));
    let apple_down = json!({"error": "Apple Podcasts did not answer; try again shortly"});
    let own = [
        ("categories", StatusCode::BAD_GATEWAY, apple_down.clone()),
        ("charts", StatusCode::BAD_GATEWAY, apple_down.clone()),
        ("for-you", StatusCode::BAD_GATEWAY, apple_down),
        (
            "listening",
            StatusCode::OK,
            json!({"aiHidden": 0, "inProgress": [], "newEpisodes": []}),
        ),
    ];
    for (segment, status, body) in own {
        let res = server
            .get(&format!("/api/podcasts/{segment}"))
            .authorization_bearer(token)
            .await;
        let got = (res.status_code(), res.json::<Value>());
        assert_ne!(
            got.1, show_handler,
            "{segment} reached GET /podcasts/{{id}}"
        );
        assert_eq!(got, (status, body), "{segment}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn features_m1_flags(pool: PgPool) {
    let server = common::server(pool);
    let body: Value = register(&server, "ana", "secret1").await.json();
    let token = body["token"].as_str().unwrap();
    server.get("/api/system/features").authorization_bearer(token).await.assert_json(&json!({
        "musicCNN": false, "vibeEmbeddings": false, "audiobookshelfEnabled": false, "fpcalc": false,
        "library": true, "podcasts": true, "audiobooks": false, "discover": false,
        "mixes": false, "downloads": false, "imports": false,
        // M-STATS S1: every `/stats/*` route the v1.12.3 page calls is served. `changelog`
        // and `accountDeletion` stay unsent (= off in the frontend, D-170).
        "stats": true
    }));
    let stub: Value = server
        .get("/api/notifications/downloads/active")
        .authorization_bearer(token)
        .await
        .json();
    assert_eq!(stub, json!([]));
    server
        .get("/api/discover/batch-status")
        .authorization_bearer(token)
        .await
        .assert_json(&json!({ "active": false, "status": null }));
    server
        .get("/api/discover/batch-status")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
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
    server
        .get("/api/system/features")
        .authorization_bearer(token)
        .await
        .assert_status_ok();
}
