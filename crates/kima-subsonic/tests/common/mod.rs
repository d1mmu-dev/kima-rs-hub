#![allow(dead_code)]

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use kima_api::AppState;
use kima_db::PgPool;
use kima_db::users::{self, User};
use serde_json::Value;
use uuid::Uuid;

pub const SECRET: &str = "test-secret";

/// The Subsonic router on its own (no `/rest` prefix), with a fixed client address.
pub fn server(pool: PgPool) -> (TestServer, AppState) {
    let state = AppState::new(pool, SECRET);
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let app = kima_subsonic::router(state.clone()).layer(MockConnectInfo(peer));
    (TestServer::new(app).unwrap(), state)
}

/// GETs `/{endpoint}.view?f=json&...` and returns the `subsonic-response` object.
pub async fn rest(server: &TestServer, endpoint: &str, params: &[(&str, &str)]) -> Value {
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_param("f", "json")
        .add_query_params(params)
        .await;
    res.assert_status_ok();
    res.json::<Value>()["subsonic-response"].clone()
}

/// GETs `/{endpoint}.view?...` and returns the XML body.
pub async fn rest_xml(server: &TestServer, endpoint: &str, params: &[(&str, &str)]) -> String {
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_params(params)
        .await;
    res.assert_status_ok();
    res.text()
}

pub async fn user(state: &AppState, name: &str, password: &str, role: &str) -> User {
    let hash = kima_core::auth::hash_password(password);
    users::create_by_admin(&state.pool, Uuid::now_v7(), name, &hash, role)
        .await
        .unwrap()
        .unwrap()
}

/// Mints an app token for the user, as the settings page does. Returns its id and the token.
pub async fn app_token(state: &AppState, user: &User, name: &str) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let token = kima_core::app_token::generate();
    kima_db::api_keys::create(
        &state.pool,
        id,
        user.id,
        user.token_version,
        name,
        &kima_core::app_token::hash(&token),
        &kima_core::app_token::seal(&state.cipher, id, &token),
    )
    .await
    .unwrap()
    .unwrap();
    (id, token)
}
