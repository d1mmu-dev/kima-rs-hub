mod common;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use axum_test::TestServer;
use kima_api::auth::{AdminUser, AuthUser};
use kima_db::PgPool;
use serde_json::{Value, json};

/// The real router plus two probe routes that require a user.
fn probe(state: kima_api::AppState) -> TestServer {
    let probes = Router::new()
        .route(
            "/whoami",
            get(|user: AuthUser| async move { user.username }),
        )
        .route(
            "/media/stream",
            get(|user: AuthUser| async move { user.username }),
        )
        .route(
            "/admin-probe",
            get(|AdminUser(user): AdminUser| async move { user.username }),
        )
        .with_state(state.clone());
    TestServer::new(kima_api::router(state).merge(probes)).unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn health_reports_db(pool: PgPool) {
    let server = common::server(pool);
    for path in ["/health", "/api/health"] {
        let res = server.get(path).await;
        res.assert_status_ok();
        res.assert_json(&json!({ "status": "ok", "checks": { "db": "ok" } }));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_api_path_is_json_404(pool: PgPool) {
    let res = common::server(pool).get("/api/nope").await;
    res.assert_status(StatusCode::NOT_FOUND);
    res.assert_json(&json!({ "error": "Not found" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_or_bad_token_is_401(pool: PgPool) {
    let server = probe(common::state(pool));
    let res = server.get("/whoami").await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(res.json::<Value>()["error"], "Not authenticated");
    server
        .get("/whoami")
        .authorization_bearer("garbage")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn valid_access_token_is_accepted(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let res = probe(state)
        .get("/whoami")
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    res.assert_text("ana");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_token_is_not_an_access_token(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let refresh = state.jwt.issue_refresh(user.id, user.token_version);
    probe(state)
        .get("/whoami")
        .authorization_bearer(&refresh)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_token_only_works_on_stream_paths(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let stream = state.jwt.issue_stream(user.id, user.token_version);
    let server = probe(state);
    server
        .get("/whoami")
        .authorization_bearer(&stream)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server
        .get("/media/stream")
        .authorization_bearer(&stream)
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bumped_token_version_revokes(pool: PgPool) {
    let state = common::state(pool);
    let (user, token) = common::user_with_token(&state, "ana").await;
    sqlx::query("UPDATE users SET token_version = token_version + 1 WHERE id = $1")
        .bind(user.id)
        .execute(&state.pool)
        .await
        .unwrap();
    probe(state)
        .get("/whoami")
        .authorization_bearer(&token)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn token_of_deleted_user_is_rejected(pool: PgPool) {
    let state = common::state(pool);
    let (user, token) = common::user_with_token(&state, "ana").await;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.id)
        .execute(&state.pool)
        .await
        .unwrap();
    probe(state)
        .get("/whoami")
        .authorization_bearer(&token)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_token_in_query_works_on_stream_paths(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let stream = state.jwt.issue_stream(user.id, user.token_version);
    probe(state)
        .get(&format!("/media/stream?token={stream}"))
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn query_token_rejected_off_stream_paths(pool: PgPool) {
    let state = common::state(pool);
    let (user, access) = common::user_with_token(&state, "ana").await;
    let stream = state.jwt.issue_stream(user.id, user.token_version);
    let server = probe(state);
    for token in [access, stream] {
        server
            .get(&format!("/whoami?token={token}"))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn access_token_in_query_works_on_stream_paths(pool: PgPool) {
    let state = common::state(pool);
    let (_, access) = common::user_with_token(&state, "ana").await;
    probe(state)
        .get(&format!("/media/stream?token={access}"))
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_ticket_endpoint(pool: PgPool) {
    let state = common::state(pool);
    let (user, access) = common::user_with_token(&state, "ana").await;
    let server = probe(state.clone());
    server
        .post("/api/auth/stream-ticket")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let body: Value = server
        .post("/api/auth/stream-ticket")
        .authorization_bearer(&access)
        .await
        .json();
    let claims = state.jwt.decode(body["token"].as_str().unwrap()).unwrap();
    assert_eq!(claims.kind(), kima_core::auth::TokenKind::Stream);
    assert_eq!(claims.user_id, user.id);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_and_empty_query_tokens_are_rejected(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let refresh = state.jwt.issue_refresh(user.id, user.token_version);
    let server = probe(state);
    server
        .get(&format!("/media/stream?token={refresh}"))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server
        .get("/media/stream?token=")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bad_header_falls_through_to_query_token(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let stream = state.jwt.issue_stream(user.id, user.token_version);
    probe(state)
        .get(&format!("/media/stream?token={stream}"))
        .authorization_bearer("garbage")
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_user_rejects_anonymous_401(pool: PgPool) {
    let res = probe(common::state(pool)).get("/admin-probe").await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    res.assert_json(&json!({ "error": "Not authenticated" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_user_rejects_user_403(pool: PgPool) {
    let state = common::state(pool);
    let (_admin, _) = common::user_with_token(&state, "root").await;
    let (_, token) = common::user_with_token(&state, "ana").await;
    let res = probe(state)
        .get("/admin-probe")
        .authorization_bearer(&token)
        .await;
    res.assert_status(StatusCode::FORBIDDEN);
    res.assert_text(r#"{"error":"Admin access required"}"#);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_user_admits_admin(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "root").await;
    let res = probe(state)
        .get("/admin-probe")
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    res.assert_text("root");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn features_fpcalc_reflects_state(pool: PgPool) {
    for fpcalc in [false, true] {
        let state = common::state(pool.clone()).with_fpcalc(fpcalc);
        let (_, token) = common::user_with_token(&state, &format!("ana{fpcalc}")).await;
        let server = TestServer::new(kima_api::router(state)).unwrap();
        let body: Value = server
            .get("/api/system/features")
            .authorization_bearer(&token)
            .await
            .json();
        assert_eq!(body["fpcalc"], json!(fpcalc));
    }
}
