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
    login_as(server, "alice", password).await
}

async fn login_as(server: &TestServer, username: &str, password: &str) -> axum_test::TestResponse {
    server
        .post("/api/auth/login")
        .json(&json!({ "username": username, "password": password }))
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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_wrong_logins_are_capped_at_five(pool: PgPool) {
    let state = common::state(pool);
    common::user_with_token(&state, "alice").await;
    let server = server_from(&state, [10, 0, 0, 1]);
    let before = state.password_permits_issued();
    let responses =
        futures::future::join_all((0..10).map(|_| login(&server, "wrong-password"))).await;
    let limited = responses
        .iter()
        .filter(|r| r.status_code() == axum::http::StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(limited, 5, "the other five were refused");
    assert_eq!(state.password_permits_issued() - before, 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn overlong_username_is_401_without_hashing_or_limiting(pool: PgPool) {
    let state = common::state(pool);
    let server = server_from(&state, [10, 0, 0, 1]);
    let long = "x".repeat(51);
    for _ in 0..8 {
        login_as(&server, &long, "whatever")
            .await
            .assert_status_unauthorized();
    }
    assert_eq!(state.password_permits_issued(), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_username_is_limited_too(pool: PgPool) {
    let state = common::state(pool);
    let server = server_from(&state, [10, 0, 0, 1]);
    for _ in 0..5 {
        login_as(&server, "ghost", "x")
            .await
            .assert_status_unauthorized();
    }
    login_as(&server, "ghost", "x")
        .await
        .assert_status(axum::http::StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn differently_cased_usernames_share_a_key(pool: PgPool) {
    let state = common::state(pool);
    common::user_with_token(&state, "alice").await;
    let server = server_from(&state, [10, 0, 0, 1]);
    for name in ["alice", "ALICE", "Alice", "alice", "aLiCe"] {
        login_as(&server, name, "wrong-password")
            .await
            .assert_status_unauthorized();
    }
    for name in ["alice", "Alice"] {
        login_as(&server, name, "password1")
            .await
            .assert_status(axum::http::StatusCode::TOO_MANY_REQUESTS);
    }
}

/// What a client can observe of one sign-in answer.
async fn observe(res: axum_test::TestResponse) -> (u16, Option<String>, String) {
    let retry_after = res
        .maybe_header("retry-after")
        .map(|v| v.to_str().unwrap().to_owned());
    (res.status_code().as_u16(), retry_after, res.text())
}

/// Five failures as `noisy`, then one sign-in as `probe` with the account's password, against a
/// fresh limiter.
async fn fold_probe(pool: &PgPool, noisy: &str, probe: &str) -> (u16, Option<String>, String) {
    let state = common::state(pool.clone());
    let server = server_from(&state, [10, 0, 0, 1]);
    for _ in 0..5 {
        login_as(&server, noisy, "wrong-password")
            .await
            .assert_status_unauthorized();
    }
    observe(login_as(&server, probe, "password1").await).await
}

async fn db_fold(pool: &PgPool, name: &str) -> String {
    sqlx::query_scalar("SELECT lower($1)")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Spec §5.7: the limiter reveals nothing about whether an account exists. Rust lowercases `İ`
/// (U+0130) to `i` + U+0307, Postgres under `en_US` to plain `i`, so `İvy` and `ivy` are
/// different Rust keys but the same account. Failures as `İvy` must then block `ivy` the same
/// way whether or not `ivy` exists (M2 final review I2).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fold_variant_failures_answer_the_same_for_known_and_unknown_names(pool: PgPool) {
    let unknown = fold_probe(&pool, "\u{130}vy", "ivy").await;
    common::user_with_token(&common::state(pool.clone()), "ivy").await;
    let known = fold_probe(&pool, "\u{130}vy", "ivy").await;
    assert_eq!(known, unknown, "the answer must not depend on existence");
    if db_fold(&pool, "\u{130}vy").await == "ivy" {
        // The database folds like en_US: every spelling of the account shares one budget.
        assert_eq!(known.0, 429, "{known:?}");
    }
}

/// Postgres folds `ΑΣ` to `ασ`, while Rust (final sigma) folds it to `ας`. Failures typed as
/// `ΑΣ` are charged on the database fold `ασ` too, so a sign-in as `ασ` is refused before any
/// hashing, exactly as for a name nobody holds.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn database_fold_key_is_charged_whether_or_not_the_account_exists(pool: PgPool) {
    let unknown = fold_probe(&pool, "ΑΣ", "ασ").await;
    common::user_with_token(&common::state(pool.clone()), "ΑΣ").await;
    let known = fold_probe(&pool, "ΑΣ", "ασ").await;
    assert_eq!(known, unknown, "the answer must not depend on existence");
    if db_fold(&pool, "ΑΣ").await == "ασ" {
        assert_eq!(known.0, 429, "{known:?}");
        assert_eq!(known.1.as_deref(), Some("2"));
    }
}
