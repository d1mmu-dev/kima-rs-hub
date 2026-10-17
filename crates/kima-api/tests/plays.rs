mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_db::PgPool;
use serde_json::{Value, json};
use uuid::Uuid;

struct Env {
    server: TestServer,
    ana: String,
    ben: String,
    ana_id: Uuid,
}

async fn env(pool: PgPool) -> Env {
    let state = common::state(pool);
    let (ana, ana_token) = common::user_with_token(&state, "ana").await;
    let (_, ben) = common::user_with_token(&state, "ben").await;
    Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        ana: ana_token,
        ben,
        ana_id: ana.id,
    }
}

async fn play(e: &Env, token: &str, track: Uuid, seconds: f64) -> Value {
    let res = e
        .server
        .post("/api/plays")
        .authorization_bearer(token)
        .json(&json!({ "trackId": track, "secondsListened": seconds }))
        .await;
    res.assert_status_ok();
    res.json()
}

async fn get(e: &Env, token: &str, path: &str) -> Value {
    let res = e.server.get(path).authorization_bearer(token).await;
    res.assert_status_ok();
    res.json()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn play_recorded_and_listed(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", Some(2000), "Song")).await;
    let e = env(pool).await;

    // The seeded track is 180 s: half of it (90 s) is required.
    let out = play(&e, &e.ana, s.track, 90.0).await;
    assert_eq!(out["recorded"], true);
    assert!(out["playId"].is_string());

    let plays = get(&e, &e.ana, "/api/plays").await;
    assert_eq!(plays.as_array().unwrap().len(), 1);
    let p = &plays[0];
    for k in [
        "id", "userId", "trackId", "playedAt", "source", "heard", "track",
    ] {
        assert!(p.get(k).is_some(), "missing {k}");
    }
    assert_eq!(p["trackId"], json!(s.track));
    assert_eq!(p["heard"], true);
    assert_eq!(p["track"]["title"], "Song");
    assert_eq!(p["track"]["album"]["artist"]["name"], "Alpha");

    // Plays are per user.
    assert_eq!(get(&e, &e.ben, "/api/plays").await, json!([]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listening_rules_and_errors(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", None, "Song")).await;
    let short = seed(&pool, Seed::new("Alpha", "A1", None, "Blip").at(1, 2)).await;
    sqlx::query("UPDATE tracks SET duration_secs = 20 WHERE id = $1")
        .bind(short.track)
        .execute(&pool)
        .await
        .unwrap();
    let e = env(pool).await;

    assert_eq!(
        play(&e, &e.ana, s.track, 89.9).await,
        json!({ "recorded": false, "reason": "not-enough-listened" })
    );
    assert_eq!(
        play(&e, &e.ana, short.track, 20.0).await,
        json!({ "recorded": false, "reason": "too-short" })
    );
    // Clients that omit secondsListened record nothing.
    let res = e
        .server
        .post("/api/plays")
        .authorization_bearer(&e.ana)
        .json(&json!({ "trackId": s.track }))
        .await;
    assert_eq!(res.json::<Value>()["reason"], "not-enough-listened");
    assert_eq!(get(&e, &e.ana, "/api/plays").await, json!([]));

    for body in [
        json!({ "trackId": Uuid::now_v7() }),
        json!({ "trackId": "not-a-uuid", "secondsListened": 500 }),
    ] {
        let res = e
            .server
            .post("/api/plays")
            .authorization_bearer(&e.ana)
            .json(&body)
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(res.json::<Value>()["error"], "Track not found");
    }
    for body in [
        json!({}),
        json!({ "trackId": s.track, "secondsListened": -1 }),
        json!({ "trackId": s.track, "secondsListened": 86401 }),
    ] {
        e.server
            .post("/api/plays")
            .authorization_bearer(&e.ana)
            .json(&body)
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }
    e.server
        .post("/api/plays")
        .json(&json!({ "trackId": s.track, "secondsListened": 100 }))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    e.server
        .get("/api/plays")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn duplicate_within_window_not_double_counted(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", None, "Song")).await;
    let e = env(pool.clone()).await;

    assert_eq!(play(&e, &e.ana, s.track, 100.0).await["recorded"], true);
    assert_eq!(
        play(&e, &e.ana, s.track, 150.0).await,
        json!({ "recorded": false, "reason": "duplicate" })
    );
    // Another user's listen of the same track is their own.
    assert_eq!(play(&e, &e.ben, s.track, 100.0).await["recorded"], true);
    assert_eq!(
        get(&e, &e.ana, "/api/plays")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // A play older than one track length (180 s) is a different listen.
    sqlx::query("UPDATE plays SET played_at = now() - interval '181 seconds' WHERE user_id = $1")
        .bind(e.ana_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(play(&e, &e.ana, s.track, 100.0).await["recorded"], true);
    assert_eq!(
        get(&e, &e.ana, "/api/plays")
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_reports_store_one_row(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", None, "Song")).await;
    let e = env(pool).await;
    let (r1, r2, r3, r4) = tokio::join!(
        play(&e, &e.ana, s.track, 100.0),
        play(&e, &e.ana, s.track, 100.0),
        play(&e, &e.ana, s.track, 100.0),
        play(&e, &e.ana, s.track, 100.0),
    );
    let recorded = [r1, r2, r3, r4]
        .iter()
        .filter(|o| o["recorded"] == true)
        .count();
    assert_eq!(recorded, 1);
    assert_eq!(
        get(&e, &e.ana, "/api/plays")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recently_listened_orders_by_last_play(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "A1", None, "a1")).await;
    let b = seed(&pool, Seed::new("Beta", "B1", None, "b1")).await;
    let c = seed(&pool, Seed::new("Gamma", "C1", None, "c1")).await;
    let a2 = seed(&pool, Seed::new("Alpha", "A1", None, "a2").at(1, 2)).await;
    let e = env(pool.clone()).await;

    // (track, minutes ago): Alpha twice, so its newest play wins.
    for (track, mins) in [(a.track, 50), (b.track, 30), (c.track, 20), (a2.track, 10)] {
        sqlx::query(
            "INSERT INTO plays (id, user_id, track_id, played_at, heard)
             VALUES ($1, $2, $3, now() - make_interval(mins => $4), true)",
        )
        .bind(Uuid::now_v7())
        .bind(e.ana_id)
        .bind(track)
        .bind(mins)
        .execute(&pool)
        .await
        .unwrap();
    }
    mark_missing(&pool, c.track).await;

    let v = get(&e, &e.ana, "/api/library/recently-listened").await;
    let items = v["items"].as_array().unwrap();
    let names: Vec<_> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    // Gamma's only track is missing, so it drops out.
    assert_eq!(names, ["Alpha", "Beta"]);
    for k in [
        "id",
        "name",
        "type",
        "lastPlayedAt",
        "coverArt",
        "albumCount",
        "mbid",
    ] {
        assert!(items[0].get(k).is_some(), "missing {k}");
    }
    assert_eq!(items[0]["type"], "artist");
    assert_eq!(items[0]["id"], json!(a.artist));
    assert_eq!(items[0]["albumCount"], 1);

    let limited = get(&e, &e.ana, "/api/library/recently-listened?limit=1").await;
    assert_eq!(limited["items"].as_array().unwrap().len(), 1);
    // Another user has heard nothing.
    assert_eq!(
        get(&e, &e.ben, "/api/library/recently-listened").await,
        json!({ "items": [] })
    );
    // And the plays list hides the missing track's play.
    assert_eq!(
        get(&e, &e.ana, "/api/plays")
            .await
            .as_array()
            .unwrap()
            .len(),
        3
    );
}
