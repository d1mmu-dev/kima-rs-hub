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
    res.assert_json(
        &json!({"mode": "poll", "active": true, "lastError": "boom", "nextSyncAt": null}),
    );

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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn watch_status_next_sync_at(pool: PgPool) {
    let cron = kima_jobs::schedule::Cron::parse("0 4 * * *").unwrap();
    let state = common::state(pool)
        .with_library_services(LibraryServices::new(WatchMode::Poll).with_sync_cron(Some(cron)));
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let before = time::OffsetDateTime::now_utc();
    let body: serde_json::Value = server
        .get("/api/library/watch-status")
        .authorization_bearer(&admin)
        .await
        .json();
    let next = body["nextSyncAt"].as_str().expect("an ISO 8601 string");
    let next =
        time::OffsetDateTime::parse(next, &time::format_description::well_known::Rfc3339).unwrap();
    assert!(next > before && next <= before + time::Duration::days(1));
    assert_eq!((next.hour(), next.minute(), next.second()), (4, 0, 0));
    assert!(next.offset().is_utc());
}
