mod common;

use axum::http::StatusCode;
use kima_db::PgPool;
use serde_json::{Value, json};

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn change_password_requires_current(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let res = server
        .post("/api/auth/change-password")
        .authorization_bearer(&token)
        .json(&json!({ "currentPassword": "wrong-one", "newPassword": "newsecret" }))
        .await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    res.assert_json(&json!({ "error": "Current password is incorrect" }));

    let short = server
        .post("/api/auth/change-password")
        .authorization_bearer(&token)
        .json(&json!({ "currentPassword": "password1", "newPassword": "abc" }))
        .await;
    short.assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn change_password_revokes_old_tokens(pool: PgPool) {
    let state = common::state(pool);
    let (_, old) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let res = server
        .post("/api/auth/change-password")
        .authorization_bearer(&old)
        .json(&json!({ "currentPassword": "password1", "newPassword": "newsecret" }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    let fresh = body["token"].as_str().unwrap();
    assert!(body["refreshToken"].as_str().is_some());

    server
        .get("/api/auth/me")
        .authorization_bearer(&old)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server
        .get("/api/auth/me")
        .authorization_bearer(fresh)
        .await
        .assert_status_ok();

    // The new password logs in; the old one no longer does.
    server
        .post("/api/auth/login")
        .json(&json!({ "username": "ana", "password": "newsecret" }))
        .await
        .assert_status_ok();
    server
        .post("/api/auth/login")
        .json(&json!({ "username": "ana", "password": "password1" }))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_creates_and_lists_users(pool: PgPool) {
    let state = common::state(pool);
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let created = server
        .post("/api/auth/create-user")
        .authorization_bearer(&admin)
        .json(&json!({ "username": "ben", "password": "secret1", "role": "user" }))
        .await;
    created.assert_status_ok();
    let created: Value = created.json();
    assert_eq!(created["username"], "ben");
    assert_eq!(created["role"], "user");
    assert!(created["id"].as_str().is_some());
    assert!(created["createdAt"].as_str().is_some());

    server
        .post("/api/auth/create-user")
        .authorization_bearer(&admin)
        .json(&json!({ "username": "BEN", "password": "secret1" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    server
        .post("/api/auth/create-user")
        .authorization_bearer(&admin)
        .json(&json!({ "username": "cy", "password": "secret1", "role": "root" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let list: Value = server
        .get("/api/auth/users")
        .authorization_bearer(&admin)
        .await
        .json();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["ana", "ben"]);
    assert_eq!(list[1]["onboardingComplete"], true);
    assert!(list[0].get("passwordHash").is_none());

    // The created user can sign in and skips onboarding.
    let login: Value = server
        .post("/api/auth/login")
        .json(&json!({ "username": "ben", "password": "secret1" }))
        .await
        .json();
    assert_eq!(login["user"]["onboardingComplete"], true);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn non_admin_gets_403(pool: PgPool) {
    let state = common::state(pool);
    let (admin, _) = common::user_with_token(&state, "ana").await;
    let (_, plain) = common::user_with_token(&state, "ben").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let forbidden = json!({ "error": "Admin access required" });

    let res = server
        .get("/api/auth/users")
        .authorization_bearer(&plain)
        .await;
    res.assert_status(StatusCode::FORBIDDEN);
    res.assert_json(&forbidden);
    server
        .post("/api/auth/create-user")
        .authorization_bearer(&plain)
        .json(&json!({ "username": "cy", "password": "secret1" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    server
        .delete(&format!("/api/auth/users/{}", admin.id))
        .authorization_bearer(&plain)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    server
        .get("/api/system-settings")
        .authorization_bearer(&plain)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    server
        .post("/api/system-settings")
        .authorization_bearer(&plain)
        .json(&json!({ "registrationOpen": true }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_deletes_a_user_and_their_token_dies(pool: PgPool) {
    let state = common::state(pool);
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let (ben, ben_token) = common::user_with_token(&state, "ben").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    server
        .delete(&format!("/api/auth/users/{}", ben.id))
        .authorization_bearer(&admin)
        .await
        .assert_status_ok();
    server
        .get("/api/auth/me")
        .authorization_bearer(&ben_token)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server
        .delete(&format!("/api/auth/users/{}", ben.id))
        .authorization_bearer(&admin)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    server
        .delete("/api/auth/users/not-a-uuid")
        .authorization_bearer(&admin)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cannot_delete_self(pool: PgPool) {
    let state = common::state(pool);
    let (admin, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let res = server
        .delete(&format!("/api/auth/users/{}", admin.id))
        .authorization_bearer(&token)
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    res.assert_json(&json!({ "error": "Cannot delete your own account" }));
}

/// An API caller is always an admin, so the last-admin guard only bites when two admins delete
/// each other at once. Exactly one of the deletions may win.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cannot_delete_last_admin(pool: PgPool) {
    use kima_db::users::{self, Deletion};

    let state = common::state(pool);
    let (ana, _) = common::user_with_token(&state, "ana").await;
    let (ben, _) = common::user_with_token(&state, "ben").await;
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = $1")
        .bind(ben.id)
        .execute(&state.pool)
        .await
        .unwrap();

    let (a, b) = tokio::join!(
        users::delete_unless_last_admin(&state.pool, ana.id),
        users::delete_unless_last_admin(&state.pool, ben.id),
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert!(outcomes.contains(&Deletion::Deleted), "{outcomes:?}");
    assert!(outcomes.contains(&Deletion::LastAdmin), "{outcomes:?}");
    assert_eq!(users::count(&state.pool).await.unwrap(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn registration_toggle_opens_signup(pool: PgPool) {
    let server = common::server(pool);
    let first: Value = server
        .post("/api/onboarding/register")
        .json(&json!({ "username": "ana", "password": "secret1" }))
        .await
        .json();
    let admin = first["token"].as_str().unwrap();

    server
        .post("/api/onboarding/register")
        .json(&json!({ "username": "ben", "password": "secret1" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);

    let before: Value = server
        .get("/api/system-settings")
        .authorization_bearer(admin)
        .await
        .json();
    assert_eq!(before["registrationOpen"], false);

    let after = server
        .post("/api/system-settings")
        .authorization_bearer(admin)
        .json(&json!({ "registrationOpen": true }))
        .await;
    after.assert_status_ok();
    assert_eq!(after.json::<Value>()["registrationOpen"], true);

    server
        .post("/api/onboarding/register")
        .json(&json!({ "username": "ben", "password": "secret1" }))
        .await
        .assert_status_ok();
}
