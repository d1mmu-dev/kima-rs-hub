mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use kima_api::AppState;
use kima_db::PgPool;
use serde_json::{Value, json};

fn server_from(state: &AppState, ip: [u8; 4]) -> TestServer {
    let app = kima_api::router(state.clone()).layer(MockConnectInfo(SocketAddr::from((ip, 1))));
    TestServer::new(app).unwrap()
}

async fn login(server: &TestServer, password: &str) -> axum_test::TestResponse {
    server
        .post("/api/auth/login")
        .json(&json!({ "username": "alice", "password": password }))
        .await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn login_blocked_after_five_failures_429(pool: PgPool) {
    let state = common::state(pool);
    common::user_with_token(&state, "alice").await;
    let server = server_from(&state, [10, 0, 0, 1]);
    for _ in 0..5 {
        login(&server, "wrong-password")
            .await
            .assert_status_unauthorized();
    }
    let before = state.password_permits_issued();
    let res = login(&server, "password1").await;
    res.assert_status(axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res.header("retry-after"), "2");
    res.assert_json(&json!({
        "error": "Too many failed login attempts. Try again in 2 seconds."
    }));
    assert_eq!(state.password_permits_issued(), before, "no argon2 ran");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn login_block_is_per_ip(pool: PgPool) {
    let state = common::state(pool);
    common::user_with_token(&state, "alice").await;
    let blocked = server_from(&state, [10, 0, 0, 1]);
    for _ in 0..5 {
        login(&blocked, "wrong-password")
            .await
            .assert_status_unauthorized();
    }
    let other = server_from(&state, [10, 0, 0, 2]);
    let res = login(&other, "password1").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["user"]["username"], "alice");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn login_success_clears(pool: PgPool) {
    let state = common::state(pool);
    common::user_with_token(&state, "alice").await;
    let server = server_from(&state, [10, 0, 0, 1]);
    for _ in 0..4 {
        login(&server, "wrong-password")
            .await
            .assert_status_unauthorized();
    }
    login(&server, "password1").await.assert_status_ok();
    for _ in 0..5 {
        login(&server, "wrong-password")
            .await
            .assert_status_unauthorized();
    }
}
