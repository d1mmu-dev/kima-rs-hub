mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, seed};
use kima_db::PgPool;
use serde_json::{Value, json};
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_plays_counts_listens(pool: PgPool) {
    let track = seed(&pool, Seed::new("Artist", "Album", None, "Song"))
        .await
        .track;
    let state = common::state(pool.clone());
    let (ana, token) = common::user_with_token(&state, "ana").await;
    let (ben, ben_token) = common::user_with_token(&state, "ben").await;
    for (user, offset) in [
        (ana.id, 0),
        (ana.id, 44),
        (ana.id, 126),
        (ana.id, 173),
        (ana.id, 230),
        (ben.id, 0),
    ] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now() + $4 * interval '1 second')")
            .bind(Uuid::now_v7()).bind(user).bind(track).bind(offset as f64).execute(&pool).await.unwrap();
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let response = server
        .get(&format!("/api/stats/track/{track}/plays"))
        .authorization_bearer(&token)
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"plays": 2}));
    let response = server
        .get(&format!(
            "/api/stats/track/{track}/plays?period=garbage&dow=nope&dow=7"
        ))
        .authorization_bearer(&ben_token)
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"plays": 1}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_plays_validation_and_auth(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for id in [Uuid::now_v7().to_string(), "not-a-uuid".to_string()] {
        let response = server
            .get(&format!("/api/stats/track/{id}/plays"))
            .authorization_bearer(&token)
            .await;
        response.assert_status_ok();
        assert_eq!(response.json::<Value>(), json!({"plays": 0}));
    }
    let response = server
        .get(&format!("/api/stats/track/{}/plays", "a".repeat(65)))
        .authorization_bearer(&token)
        .await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(response.json::<Value>(), json!({"error": "Bad request"}));
    server
        .get("/api/stats/track/abc/plays")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
