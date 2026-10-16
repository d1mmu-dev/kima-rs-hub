#![allow(dead_code)]

use axum_test::TestServer;
use kima_api::AppState;
use kima_db::PgPool;
use kima_db::users::{self, Registration, User};
use uuid::Uuid;

pub const SECRET: &str = "test-secret";

pub fn state(pool: PgPool) -> AppState {
    AppState::new(pool, SECRET)
}

pub fn server(pool: PgPool) -> TestServer {
    TestServer::new(kima_api::router(state(pool))).unwrap()
}

/// A user created straight in the DB (password "password1"), plus an access token for them.
pub async fn user_with_token(state: &AppState, name: &str) -> (User, String) {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(&state.pool)
        .await
        .unwrap();
    let hash = kima_core::auth::hash_password("password1");
    let Registration::Created(user) = users::register(&state.pool, Uuid::now_v7(), name, &hash)
        .await
        .unwrap()
    else {
        panic!("registration failed")
    };
    let token = state
        .jwt
        .issue_access(user.id, &user.username, &user.role, user.token_version);
    (user, token)
}
