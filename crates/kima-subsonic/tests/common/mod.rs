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

// The SQL fixture `fixtures/subsonic.sql` (spec §10.1). Ids are `00000000-0000-7000-8000-K0000000000N`
// with K = 1 artists, 2 albums, 3 tracks, 4 users, 5 playlists, 6 app tokens.
pub const FIXTURE_CIPHER_SECRET: &str = "kima-subsonic-fixture";
/// Alice's app token "Phone": `0123456789abcdef` four times.
pub const FIXTURE_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const ADMIN_NAME: &str = "admin";
pub const ADMIN_PASSWORD: &str = "adminpass1";
pub const ALICE_NAME: &str = "alice";
pub const ALICE_PASSWORD: &str = "alicepass1";

/// `n` written out in decimal digits, one per nibble: 20 becomes 0x20, as in the fixture's ids.
const fn dec(mut n: u128) -> u128 {
    let (mut out, mut shift) = (0, 0);
    while n > 0 {
        out |= (n % 10) << shift;
        shift += 4;
        n /= 10;
    }
    out
}

/// `00000000-0000-7000-8000-{kind}{n:011}`: `kind` 1..=6, `n` the running number in that table.
pub const fn fixture_id(kind: u128, n: u128) -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_0000 | (kind << 44) | dec(n))
}
pub const ADMIN_ID: Uuid = fixture_id(4, 1);
pub const ALICE_ID: Uuid = fixture_id(4, 2);
pub const ALICE_TOKEN_ID: Uuid = fixture_id(6, 1);
pub const fn artist_id(n: u128) -> Uuid {
    fixture_id(1, n)
}
pub const fn album_id(n: u128) -> Uuid {
    fixture_id(2, n)
}
pub const fn track_id(n: u128) -> Uuid {
    fixture_id(3, n)
}

/// State for tests on the fixture: the fixture's cipher (so `FIXTURE_TOKEN` opens) and an empty
/// temporary library root.
pub fn fixture_state(pool: PgPool) -> AppState {
    let root = tempfile::tempdir().unwrap().keep();
    AppState::new(pool, SECRET)
        .with_cipher(kima_core::cipher::Cipher::from_secret(
            FIXTURE_CIPHER_SECRET,
        ))
        .with_library_root(root)
}

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

/// The `t` and `s` parameters a token+salt client sends for `secret`, with a random salt.
pub fn ts_params(secret: &str) -> [(String, String); 2] {
    use md5::{Digest, Md5};
    use rand::Rng;
    let salt: String = rand::thread_rng()
        .sample_iter(rand::distributions::Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();
    let t = hex::encode(Md5::digest(format!("{secret}{salt}")));
    [("t".to_owned(), t), ("s".to_owned(), salt)]
}
