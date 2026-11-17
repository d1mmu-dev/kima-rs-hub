mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use kima_api::AppState;
use kima_core::profile::{ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile};
use kima_db::users::User;
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
    res.assert_json(&json!({ "publicUrl": "", "canAcquire": false }));

    let configured = state
        .clone()
        .with_public_url(Some("https://kima.example".into()));
    let res = TestServer::new(kima_api::router(configured))
        .unwrap()
        .get("/api/system-settings/public-config")
        .authorization_bearer(&jwt)
        .await;
    res.assert_json(&json!({ "publicUrl": "https://kima.example", "canAcquire": false }));
}

// ---- per profile (M-PROF Task 8, D-244) ----

/// An access token for another profile of `user`, created in the database.
async fn profile_token(
    state: &AppState,
    user: &User,
    name: &str,
    kind: ProfileKind,
) -> (ProfileId, String) {
    let profile = profiles::create(
        &state.pool,
        user.account(),
        NewProfile {
            name: name.into(),
            kind,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let token = state.jwt.issue_access(
        user.account(),
        profile.id,
        &user.username,
        &user.role,
        user.token_version,
    );
    (profile.id, token)
}

async fn names(server: &TestServer, token: &str) -> Vec<String> {
    let res = server
        .get("/api/api-keys")
        .authorization_bearer(token)
        .await;
    res.assert_status_ok();
    let mut names: Vec<String> = res.json::<Value>()["apiKeys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

/// The profile a stored token belongs to, by its name.
async fn owner_of(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT profile_id FROM api_keys WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn id_of(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM api_keys WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn keys_list_shows_only_current_profile(pool: PgPool) {
    let state = common::state(pool);
    let (ana, main) = common::user_with_token(&state, "ana").await;
    let (kids, kids_token) = profile_token(&state, &ana, "Kids", ProfileKind::Kids).await;
    let (teen, teen_token) = profile_token(&state, &ana, "Teen", ProfileKind::Standard).await;
    let (_, bob) = common::user_with_token(&state, "bob").await;
    let server = serve(&state);

    create(&server, &main, "main phone").await;
    create(&server, &kids_token, "kids tablet").await;
    create(&server, &kids_token, "kids speaker").await;
    create(&server, &bob, "bob phone").await;

    // Each token lands in the profile that created it.
    assert_eq!(owner_of(&state.pool, "main phone").await, ana.id);
    assert_eq!(owner_of(&state.pool, "kids tablet").await, kids.into_uuid());
    assert_eq!(
        owner_of(&state.pool, "kids speaker").await,
        kids.into_uuid()
    );

    // Each profile lists its own tokens only; the main profile does not see its children's.
    assert_eq!(names(&server, &main).await, ["main phone"]);
    assert_eq!(
        names(&server, &kids_token).await,
        ["kids speaker", "kids tablet"]
    );
    assert!(names(&server, &teen_token).await.is_empty());
    assert_eq!(names(&server, &bob).await, ["bob phone"]);
    assert_ne!(teen, kids);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn revoke_sibling_key_is_404(pool: PgPool) {
    let state = common::state(pool);
    let (ana, main) = common::user_with_token(&state, "ana").await;
    let (_, kids) = profile_token(&state, &ana, "Kids", ProfileKind::Kids).await;
    let (_, teen) = profile_token(&state, &ana, "Teen", ProfileKind::Standard).await;
    let server = serve(&state);
    create(&server, &main, "main phone").await;
    create(&server, &kids, "kids tablet").await;
    let main_key = id_of(&state.pool, "main phone").await;
    let kids_key = id_of(&state.pool, "kids tablet").await;

    // Neither a sibling nor the main profile can revoke another profile's token, and the
    // answer is the unknown-id one.
    for (token, key) in [
        (&kids, main_key),
        (&teen, main_key),
        (&teen, kids_key),
        (&main, kids_key),
    ] {
        let res = server
            .delete(&format!("/api/api-keys/{key}"))
            .authorization_bearer(token)
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        res.assert_json(&json!({ "error": "API key not found or already deleted" }));
    }
    assert_eq!(names(&server, &main).await, ["main phone"]);
    assert_eq!(names(&server, &kids).await, ["kids tablet"]);

    // Each profile revokes its own.
    server
        .delete(&format!("/api/api-keys/{kids_key}"))
        .authorization_bearer(&kids)
        .await
        .assert_status_ok();
    server
        .delete(&format!("/api/api-keys/{main_key}"))
        .authorization_bearer(&main)
        .await
        .assert_status_ok();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_counts_against_account_cap(pool: PgPool) {
    let state = common::state(pool);
    let (ana, main) = common::user_with_token(&state, "ana").await;
    let (_, kids) = profile_token(&state, &ana, "Kids", ProfileKind::Kids).await;
    let (_, bob) = common::user_with_token(&state, "bob").await;
    let server = serve(&state);
    for i in 0..30 {
        create(&server, &main, &format!("m{i}")).await;
    }
    for i in 0..20 {
        create(&server, &kids, &format!("k{i}")).await;
    }
    assert_eq!(names(&server, &kids).await.len(), 20);
    // 50 over the account's profiles: neither profile gets another one.
    for token in [&kids, &main] {
        let res = server
            .post("/api/api-keys")
            .authorization_bearer(token)
            .json(&json!({ "deviceName": "one too many" }))
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        res.assert_json(&json!({ "error": "Too many app tokens; revoke one first" }));
    }
    // Another account is not affected.
    create(&server, &bob, "bob phone").await;
    // A kids token revoked frees a slot for any profile of the account.
    let k0 = id_of(&state.pool, "k0").await;
    server
        .delete(&format!("/api/api-keys/{k0}"))
        .authorization_bearer(&kids)
        .await
        .assert_status_ok();
    create(&server, &main, "m30").await;
    assert_eq!(names(&server, &main).await.len(), 31);
}
