mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use kima_api::AppState;
use kima_db::PgPool;
use serde_json::{Value, json};
use uuid::Uuid;

fn serve(state: &AppState) -> TestServer {
    TestServer::new(kima_api::router(state.clone())).unwrap()
}

async fn create(server: &TestServer, token: &str, name: &str) -> Value {
    let res = server
        .post("/api/api-keys")
        .authorization_bearer(token)
        .json(&json!({ "deviceName": name }))
        .await;
    res.assert_status(StatusCode::CREATED);
    res.json()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_returns_token_once(pool: PgPool) {
    let state = common::state(pool);
    let (_, jwt) = common::user_with_token(&state, "ana").await;
    let server = serve(&state);

    let body = create(&server, &jwt, "  Symfonium  ").await;
    let token = body["apiKey"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(body["name"], "Symfonium");
    assert!(body["createdAt"].as_str().is_some());
    assert_eq!(
        body["message"],
        "API key created successfully. Save this key - you won't see it again!"
    );

    let list = server.get("/api/api-keys").authorization_bearer(&jwt).await;
    list.assert_status_ok();
    let list: Value = list.json();
    let keys = list["apiKeys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["name"], "Symfonium");
    assert!(keys[0].get("apiKey").is_none());
    assert!(!list.to_string().contains(token));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stored_secret_opens_to_token(pool: PgPool) {
    let state = common::state(pool);
    let (_, jwt) = common::user_with_token(&state, "ana").await;
    let server = serve(&state);

    let list = create(&server, &jwt, "phone").await;
    let token = list["apiKey"].as_str().unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT id FROM api_keys")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (key_hash, secret_enc): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT key_hash, secret_enc FROM api_keys")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        key_hash,
        kima_core::app_token::hash(token).to_vec(),
        "lookup hash is sha256 of the token"
    );
    assert_eq!(
        kima_core::app_token::open(&state.cipher, id, &secret_enc).unwrap(),
        token
    );
    assert!(
        !secret_enc.windows(64).any(|w| w == token.as_bytes()),
        "the token is not stored in the clear"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn empty_name_400(pool: PgPool) {
    let state = common::state(pool);
    let (_, jwt) = common::user_with_token(&state, "ana").await;
    let server = serve(&state);
    for body in [json!({ "deviceName": "   " }), json!({})] {
        let res = server
            .post("/api/api-keys")
            .authorization_bearer(&jwt)
            .json(&body)
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        res.assert_json(&json!({ "error": "Device name is required" }));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn long_name_400(pool: PgPool) {
    let state = common::state(pool);
    let (_, jwt) = common::user_with_token(&state, "ana").await;
    let server = serve(&state);
    let res = server
        .post("/api/api-keys")
        .authorization_bearer(&jwt)
        .json(&json!({ "deviceName": "x".repeat(101) }))
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    res.assert_json(&json!({ "error": "Device name must be at most 100 characters" }));
    // 100 is fine.
    create(&server, &jwt, &"x".repeat(100)).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fifty_first_400(pool: PgPool) {
    let state = common::state(pool);
    let (_, jwt) = common::user_with_token(&state, "ana").await;
    let server = serve(&state);
    for i in 0..50 {
        create(&server, &jwt, &format!("k{i}")).await;
    }
    let res = server
        .post("/api/api-keys")
        .authorization_bearer(&jwt)
        .json(&json!({ "deviceName": "one too many" }))
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    res.assert_json(&json!({ "error": "Too many app tokens; revoke one first" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_revokes_and_hides_from_others(pool: PgPool) {
    let state = common::state(pool);
    let (_, ana) = common::user_with_token(&state, "ana").await;
    let (_, bob) = common::user_with_token(&state, "bob").await;
    let server = serve(&state);
    create(&server, &ana, "phone").await;
    let list: Value = server
        .get("/api/api-keys")
        .authorization_bearer(&ana)
        .await
        .json();
    let id = list["apiKeys"][0]["id"].as_str().unwrap().to_string();

    // Someone else's id is indistinguishable from an unknown one.
    let res = server
        .delete(&format!("/api/api-keys/{id}"))
        .authorization_bearer(&bob)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    res.assert_json(&json!({ "error": "API key not found or already deleted" }));

    let res = server
        .delete(&format!("/api/api-keys/{id}"))
        .authorization_bearer(&ana)
        .await;
    res.assert_status_ok();
    res.assert_json(&json!({ "message": "API key revoked successfully" }));
    server
        .delete(&format!("/api/api-keys/{id}"))
        .authorization_bearer(&ana)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_someone_elses_404(pool: PgPool) {
    let state = common::state(pool);
    let (_, ana) = common::user_with_token(&state, "ana").await;
    let (_, bob) = common::user_with_token(&state, "bob").await;
    let server = serve(&state);
    create(&server, &ana, "phone").await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM api_keys")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    server
        .delete(&format!("/api/api-keys/{id}"))
        .authorization_bearer(&bob)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(left, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_non_uuid_404(pool: PgPool) {
    let state = common::state(pool);
    let (_, jwt) = common::user_with_token(&state, "ana").await;
    let res = serve(&state)
        .delete("/api/api-keys/not-a-uuid")
        .authorization_bearer(&jwt)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    res.assert_json(&json!({ "error": "API key not found or already deleted" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn last_used_null_until_used(pool: PgPool) {
    let state = common::state(pool);
    let (_, jwt) = common::user_with_token(&state, "ana").await;
    let server = serve(&state);
    create(&server, &jwt, "phone").await;

    let list: Value = server
        .get("/api/api-keys")
        .authorization_bearer(&jwt)
        .await
        .json();
    assert!(list["apiKeys"][0]["lastUsed"].is_null());

    let id: Uuid = sqlx::query_scalar("SELECT id FROM api_keys")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    kima_db::api_keys::touch(&state.pool, id).await.unwrap();
    let list: Value = server
        .get("/api/api-keys")
        .authorization_bearer(&jwt)
        .await
        .json();
    assert!(list["apiKeys"][0]["lastUsed"].as_str().is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn needs_a_session(pool: PgPool) {
    let server = common::server(pool);
    server
        .get("/api/api-keys")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server
        .post("/api/api-keys")
        .json(&json!({ "deviceName": "x" }))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn public_config_for_any_user(pool: PgPool) {
    let state = common::state(pool);
    let (_, jwt) = common::user_with_token(&state, "ana").await;
    let server = serve(&state);
    server
        .get("/api/system-settings/public-config")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let res = server
        .get("/api/system-settings/public-config")
        .authorization_bearer(&jwt)
        .await;
    res.assert_status_ok();
    res.assert_json(&json!({ "publicUrl": "" }));

    let configured = state
        .clone()
        .with_public_url(Some("https://kima.example".into()));
    let res = TestServer::new(kima_api::router(configured))
        .unwrap()
        .get("/api/system-settings/public-config")
        .authorization_bearer(&jwt)
        .await;
    res.assert_json(&json!({ "publicUrl": "https://kima.example" }));
}
