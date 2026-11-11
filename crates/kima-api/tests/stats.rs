mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, seed};
use kima_db::PgPool;
use serde_json::{Value, json};
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hour_buckets_in_the_callers_zone_green(pool: PgPool) {
    let track = seed(&pool, Seed::new("Artist", "Album", None, "Song"))
        .await
        .track;
    let state = common::state(pool.clone());
    let (ana, token) = common::user_with_token(&state, "ana").await;
    let (ben, _) = common::user_with_token(&state, "ben").await;
    for user in [ana.id, ben.id] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,'2026-03-01T23:30:00Z')")
            .bind(Uuid::now_v7()).bind(user).bind(track).execute(&pool).await.unwrap();
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let response = server
        .get("/api/stats/hour?dow=1&hour=0&tz=Europe%2FWarsaw")
        .authorization_bearer(&token)
        .await;
    response.assert_status_ok();
    assert_eq!(
        response.json::<Value>(),
        json!({"weekday":1,"hour":0,"listens":1,
        "tracks":[{"title":"Song","artist":"Artist","listens":1}],
        "artists":[{"name":"Artist","listens":1}],"lastAt":"2026-03-01T23:30:00.000Z"})
    );
    for suffix in [
        "",
        "&tz=UTC",
        "&tz=Not%2FAZone",
        "&tz=UTC&tz=Europe%2FWarsaw",
    ] {
        let response = server
            .get(&format!("/api/stats/hour?dow=0&hour=23{suffix}"))
            .authorization_bearer(&token)
            .await;
        response.assert_status_ok();
        assert_eq!(response.json::<Value>()["listens"], 1);
    }
    let response = server
        .get("/api/stats/hour?dow=0&hour=23&tz=Europe%2FWarsaw")
        .authorization_bearer(&token)
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>()["listens"], 0);
}

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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hour_dst_validation_period_and_boundaries_fixed(pool: PgPool) {
    let track = seed(&pool, Seed::new("Artist", "Album", None, "Song"))
        .await
        .track;
    let state = common::state(pool.clone());
    let (ana, token) = common::user_with_token(&state, "ana").await;
    for stamp in [
        "2026-03-29T01:30:00Z",
        "2026-10-25T00:30:00Z",
        "2026-10-25T01:30:00Z",
    ] {
        sqlx::query(
            "INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,$4::timestamptz)",
        )
        .bind(Uuid::now_v7())
        .bind(ana.id)
        .bind(track)
        .bind(stamp)
        .execute(&pool)
        .await
        .unwrap();
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for (dow, hour, tz, count) in [
        (0, 3, "Europe%2FWarsaw", 1),
        (0, 2, "Europe%2FWarsaw", 2),
        (0, 1, "Europe%2FWarsaw", 0),
        (0, 1, "UTC", 2),
        (0, 1, "Not%2FAZone", 2),
    ] {
        let r = server
            .get(&format!("/api/stats/hour?dow={dow}&hour={hour}&tz={tz}"))
            .authorization_bearer(&token)
            .await;
        r.assert_status_ok();
        assert_eq!(r.json::<Value>()["listens"], count, "{dow} {hour} {tz}");
    }
    for query in [
        "dow=7&hour=0",
        "dow=0&hour=24",
        "dow=1.5&hour=0",
        "dow=abc&hour=0",
        "hour=0",
        "dow=1&dow=2&hour=0",
        "dow=-1&hour=0",
    ] {
        let r = server
            .get(&format!("/api/stats/hour?{query}"))
            .authorization_bearer(&token)
            .await;
        r.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(r.json::<Value>(), json!({"error":"Bad request"}));
    }
    for query in ["dow=&hour=", "dow=4.0&hour=19", "dow=6&hour=23"] {
        let r = server
            .get(&format!("/api/stats/hour?{query}"))
            .authorization_bearer(&token)
            .await;
        r.assert_status_ok();
        if query == "dow=6&hour=23" {
            assert_eq!(r.json::<Value>()["lastAt"], Value::Null);
            assert_eq!(r.json::<Value>()["tracks"], json!([]));
            assert_eq!(r.json::<Value>()["artists"], json!([]));
        }
    }
    server
        .get("/api/stats/hour?dow=0&hour=0")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3, date_trunc('hour', now() - interval '40 days'))")
       .bind(Uuid::now_v7()).bind(ana.id).bind(track).execute(&pool).await.unwrap();
    let (dow,hour): (i32,i32)=sqlx::query_as("SELECT extract(dow from now() - interval '40 days')::int, extract(hour from now() - interval '40 days')::int").fetch_one(&pool).await.unwrap();
    let all = server
        .get(&format!(
            "/api/stats/hour?dow={dow}&hour={hour}&tz=UTC&period=all"
        ))
        .authorization_bearer(&token)
        .await;
    let recent = server
        .get(&format!(
            "/api/stats/hour?dow={dow}&hour={hour}&tz=UTC&period=30d"
        ))
        .authorization_bearer(&token)
        .await;
    all.assert_status_ok();
    recent.assert_status_ok();
    assert!(
        all.json::<Value>()["listens"].as_i64().unwrap()
            > recent.json::<Value>()["listens"].as_i64().unwrap()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hour_order_limits_and_display_names(pool: PgPool) {
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "ana").await;
    for i in 0..7 {
        let artist = format!("Artist {}", i % 5);
        let title = format!("Song {i}");
        let row = seed(
            &pool,
            Seed::new(&artist, "Album", None, &title).at(1, i + 1),
        )
        .await;
        if i == 0 {
            sqlx::query("UPDATE tracks SET display_title='A title' WHERE id=$1")
                .bind(row.track)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,'2026-03-01T23:30:00Z')").bind(Uuid::now_v7()).bind(user.id).bind(row.track).execute(&pool).await.unwrap();
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let r = server
        .get("/api/stats/hour?dow=0&hour=23")
        .authorization_bearer(&token)
        .await;
    r.assert_status_ok();
    let v = r.json::<Value>();
    assert_eq!(v["listens"], 7);
    assert_eq!(v["tracks"].as_array().unwrap().len(), 6);
    assert_eq!(v["artists"].as_array().unwrap().len(), 4);
    assert_eq!(v["tracks"][0]["title"], "A title");
    assert_eq!(v["artists"][0], json!({"name":"Artist 0","listens":2}));
    assert_eq!(v["artists"][1], json!({"name":"Artist 1","listens":2}));
}
