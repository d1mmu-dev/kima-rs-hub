mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kima_db::PgPool;
use serde_json::{Value, json};
use tower::ServiceExt;

fn item(n: usize) -> Value {
    json!({
        "id": format!("t{n}"),
        "title": format!("Song {n}"),
        "duration": 180,
        "artist": { "id": "a1", "name": "Alpha", "extra": "dropped" },
        "album": { "id": "al1", "title": "A1", "coverArt": "al1" },
        "junk": "dropped",
    })
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn playback_state_roundtrip(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let (_, ben) = common::user_with_token(&state, "ben").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let none = server
        .get("/api/playback-state")
        .authorization_bearer(&token)
        .await;
    none.assert_status_ok();
    assert_eq!(none.json::<Value>(), Value::Null);

    let saved: Value = server
        .post("/api/playback-state")
        .authorization_bearer(&token)
        .json(&json!({
            "playbackType": "track",
            "trackId": "t1",
            "queue": [item(0), item(1), item(2), { "title": "no id" }],
            "currentIndex": 9,
            "isShuffle": true,
        }))
        .await
        .json();
    for k in [
        "userId",
        "playbackType",
        "trackId",
        "audiobookId",
        "podcastId",
        "queue",
        "currentIndex",
        "isShuffle",
        "position",
        "changedBy",
        "createdAt",
        "updatedAt",
    ] {
        assert!(saved.get(k).is_some(), "missing {k}");
    }
    assert_eq!(saved["trackId"], "t1");
    assert_eq!(saved["isShuffle"], true);
    // Entries without an id are dropped; the index is clamped into the queue.
    assert_eq!(saved["queue"].as_array().unwrap().len(), 3);
    assert_eq!(saved["currentIndex"], 2);
    // Only the fields the player reads are kept.
    assert_eq!(saved["queue"][0], item_clean(0));

    // A partial update (no queue) leaves the queue and shuffle alone.
    let partial: Value = server
        .post("/api/playback-state")
        .authorization_bearer(&token)
        .json(&json!({ "playbackType": "track", "trackId": "t2", "currentIndex": 1 }))
        .await
        .json();
    assert_eq!(partial["trackId"], "t2");
    assert_eq!(partial["currentIndex"], 1);
    assert_eq!(partial["isShuffle"], true);
    assert_eq!(partial["queue"].as_array().unwrap().len(), 3);

    let read: Value = server
        .get("/api/playback-state")
        .authorization_bearer(&token)
        .await
        .json();
    assert_eq!(read["trackId"], "t2");
    assert_eq!(read["updatedAt"], partial["updatedAt"]);

    // An explicit empty queue clears it.
    let cleared_queue: Value = server
        .post("/api/playback-state")
        .authorization_bearer(&token)
        .json(&json!({ "playbackType": "track", "queue": [] }))
        .await
        .json();
    assert_eq!(cleared_queue["queue"], Value::Null);
    assert_eq!(cleared_queue["currentIndex"], 0);
    assert_eq!(cleared_queue["trackId"], "t2");

    // Another user does not see it.
    let theirs = server
        .get("/api/playback-state")
        .authorization_bearer(&ben)
        .await;
    assert_eq!(theirs.json::<Value>(), Value::Null);

    let del = server
        .delete("/api/playback-state")
        .authorization_bearer(&token)
        .await;
    del.assert_status_ok();
    assert_eq!(
        del.json::<Value>(),
        json!({ "success": true, "cleared": 1 })
    );
    let again = server
        .delete("/api/playback-state")
        .authorization_bearer(&token)
        .await;
    assert_eq!(
        again.json::<Value>(),
        json!({ "success": true, "cleared": 0 })
    );
    assert_eq!(
        server
            .get("/api/playback-state")
            .authorization_bearer(&token)
            .await
            .json::<Value>(),
        Value::Null
    );
}

fn item_clean(n: usize) -> Value {
    json!({
        "id": format!("t{n}"),
        "title": format!("Song {n}"),
        "duration": 180.0,
        "artist": { "id": "a1", "name": "Alpha" },
        "album": { "id": "al1", "title": "A1", "coverArt": "al1" },
    })
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn playback_state_validation(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    for (body, message) in [
        (json!({}), "playbackType is required"),
        (json!({ "playbackType": "video" }), "Invalid playbackType"),
    ] {
        let res = server
            .post("/api/playback-state")
            .authorization_bearer(&token)
            .json(&body)
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>()["error"], message);
    }
    server
        .get("/api/playback-state")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server
        .get("/api/playback-state/limit")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn queue_limit_enforced(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    let limit: Value = server
        .get("/api/playback-state/limit")
        .authorization_bearer(&token)
        .await
        .json();
    let max = limit["maxQueueLength"].as_u64().unwrap() as usize;
    assert_eq!(max, 10_000, "upstream's default");

    let queue: Vec<Value> = (0..max + 5)
        .map(|n| json!({ "id": format!("t{n}"), "title": "x", "duration": 1 }))
        .collect();
    let saved: Value = server
        .post("/api/playback-state")
        .authorization_bearer(&token)
        .json(&json!({ "playbackType": "track", "queue": queue, "currentIndex": max + 2 }))
        .await
        .json();
    assert_eq!(saved["queue"].as_array().unwrap().len(), max);
    assert_eq!(saved["currentIndex"], max - 1);
}

async fn next_text(body: &mut Body) -> String {
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("an event within 2 s")
        .expect("stream still open")
        .unwrap();
    String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap()
}

async fn open_stream(app: &axum::Router, ticket: &str) -> Body {
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/api/events?ticket={ticket}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body();
    assert_eq!(
        next_text(&mut body).await,
        "data: {\"type\":\"connected\"}\n\n"
    );
    body
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn live_update_reaches_other_tab(pool: PgPool) {
    // Live reporting belongs to the public vibe (M5): every user gets upstream's 404, and
    // nothing is published to anyone's event stream.
    let state = common::state(pool);
    let (ana, token) = common::user_with_token(&state, "ana").await;
    let app = kima_api::router(state.clone());
    let mut other_tab = open_stream(&app, &state.sse.issue_ticket(ana.id)).await;

    let server = axum_test::TestServer::new(app).unwrap();
    let res = server
        .post("/api/playback-state/live")
        .authorization_bearer(&token)
        .json(&json!({
            "status": "playing", "trackId": "t1", "position": 1500, "duration": 180000, "queue": [],
        }))
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>(), json!({ "error": "Not available" }));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), other_tab.frame())
            .await
            .is_err(),
        "no event published"
    );
    server
        .post("/api/playback-state/live")
        .json(&json!({}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn big_queue_bodies_up_to_5mb(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    // About 3 MB: over axum's 2 MB default, under 5 MB.
    let pad = "x".repeat(300);
    let queue: Vec<Value> = (0..9000)
        .map(|n| json!({ "id": format!("t{n}"), "title": "x", "pad": pad }))
        .collect();
    let body = json!({ "playbackType": "track", "queue": queue });
    let len = serde_json::to_vec(&body).unwrap().len();
    assert!((2 * 1024 * 1024..5 * 1024 * 1024).contains(&len), "{len}");
    server
        .post("/api/playback-state")
        .authorization_bearer(&token)
        .json(&body)
        .await
        .assert_status_ok();

    let huge = json!({ "playbackType": "track", "pad": "x".repeat(5 * 1024 * 1024 + 1) });
    let res = server
        .post("/api/playback-state")
        .authorization_bearer(&token)
        .json(&huge)
        .await;
    res.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(res.json::<Value>()["error"], "Request body too large");
}
