mod common;

use axum::http::StatusCode;
use kima_api::LibraryServices;
use kima_core::config::WatchMode;
use kima_db::PgPool;
use serde_json::json;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn watch_status_is_admin_only_and_camel_case(pool: PgPool) {
    let state = common::state(pool).with_library_services(LibraryServices::new(WatchMode::Poll));
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let (_, user) = common::user_with_token(&state, "bob").await;
    {
        let mut s = state.library.watch_status.write().unwrap();
        s.active = true;
        s.last_error = Some("boom".into());
    }
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let res = server
        .get("/api/library/watch-status")
        .authorization_bearer(&admin)
        .await;
    res.assert_status_ok();
    res.assert_json(&json!({"mode": "poll", "active": true, "lastError": "boom"}));

    server
        .get("/api/library/watch-status")
        .authorization_bearer(&user)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    server
        .get("/api/library/watch-status")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
