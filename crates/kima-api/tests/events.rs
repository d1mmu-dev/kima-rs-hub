mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kima_api::events::{MAX_STREAMS_PER_USER, SseRegistry};
use kima_core::events::Event;
use kima_db::PgPool;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn open_stream(app: &axum::Router, ticket: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::get(format!("/api/events?ticket={ticket}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn next_text(body: &mut Body) -> String {
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("an event within 2 s")
        .expect("stream still open")
        .unwrap();
    String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap()
}

#[test]
fn tickets_are_single_use() {
    let registry = SseRegistry::new();
    let user = Uuid::now_v7();
    let ticket = registry.issue_ticket(user);
    assert_eq!(registry.redeem_ticket(&ticket), Some(user));
    assert_eq!(registry.redeem_ticket(&ticket), None);
    assert_eq!(registry.redeem_ticket("made-up"), None);
}

#[test]
fn tickets_expire() {
    let registry = SseRegistry::with_ttl(Duration::from_millis(10));
    let ticket = registry.issue_ticket(Uuid::now_v7());
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(registry.redeem_ticket(&ticket), None);
}

#[test]
fn slots_are_capped_and_freed_on_drop() {
    let registry = Arc::new(SseRegistry::new());
    let user = Uuid::now_v7();
    let slots: Vec<_> = (0..MAX_STREAMS_PER_USER)
        .map(|_| registry.try_open(user).unwrap())
        .collect();
    assert!(registry.try_open(user).is_none());
    assert!(
        registry.try_open(Uuid::now_v7()).is_some(),
        "other users unaffected"
    );
    drop(slots);
    assert_eq!(registry.open_streams(user), 0);
    assert!(registry.try_open(user).is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ticket_endpoint_requires_auth(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .post("/api/events/ticket")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let body: Value = server
        .post("/api/events/ticket")
        .authorization_bearer(&token)
        .await
        .json();
    assert!(body["ticket"].as_str().unwrap().len() >= 32);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_needs_a_valid_ticket(pool: PgPool) {
    let app = kima_api::router(common::state(pool));
    let res = app
        .clone()
        .oneshot(Request::get("/api/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        open_stream(&app, "made-up").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_delivers_own_and_broadcast_events_only(pool: PgPool) {
    let state = common::state(pool);
    let (ana, _) = common::user_with_token(&state, "ana").await;
    let app = kima_api::router(state.clone());

    let res = open_stream(&app, &state.sse.issue_ticket(ana.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let mut body = res.into_body();
    assert_eq!(
        next_text(&mut body).await,
        "data: {\"type\":\"connected\"}\n\n"
    );

    state.events.publish(Event {
        user_id: Some(Uuid::now_v7()),
        data: json!({ "type": "not-yours" }),
    });
    state.events.publish(Event {
        user_id: Some(ana.id),
        data: json!({ "type": "yours" }),
    });
    state.events.publish(Event {
        user_id: None,
        data: json!({ "type": "everyone" }),
    });
    assert_eq!(next_text(&mut body).await, "data: {\"type\":\"yours\"}\n\n");
    assert_eq!(
        next_text(&mut body).await,
        "data: {\"type\":\"everyone\"}\n\n"
    );
}

/// Review focus 3: closed tabs give their slot back.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sixth_stream_is_refused_until_one_closes(pool: PgPool) {
    let state = common::state(pool);
    let (ana, _) = common::user_with_token(&state, "ana").await;
    let app = kima_api::router(state.clone());

    let mut open = Vec::new();
    for _ in 0..MAX_STREAMS_PER_USER {
        let res = open_stream(&app, &state.sse.issue_ticket(ana.id)).await;
        assert_eq!(res.status(), StatusCode::OK);
        open.push(res);
    }
    let res = open_stream(&app, &state.sse.issue_ticket(ana.id)).await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

    drop(open.pop());
    let res = open_stream(&app, &state.sse.issue_ticket(ana.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
}

/// Open streams must not hold graceful shutdown open.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_ends_when_shutdown_is_cancelled(pool: PgPool) {
    let state = common::state(pool);
    let (ana, _) = common::user_with_token(&state, "ana").await;
    let app = kima_api::router(state.clone());

    let res = open_stream(&app, &state.sse.issue_ticket(ana.id)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body();
    assert_eq!(
        next_text(&mut body).await,
        "data: {\"type\":\"connected\"}\n\n"
    );

    state.shutdown.cancel();
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("stream ends within 2 s of shutdown");
    assert!(frame.is_none(), "no further frames after shutdown");
    drop(body);
    assert_eq!(state.sse.open_streams(ana.id), 0, "slot released");
}
