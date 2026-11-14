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

async fn stored_plays(pool: &PgPool, user: Uuid) -> Vec<(Uuid, bool, String)> {
    sqlx::query_as(
        "SELECT track_id, heard, source FROM plays WHERE user_id = $1 ORDER BY played_at DESC",
    )
    .bind(user)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn play_recorded_and_listed(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", Some(2000), "Song")).await;
    let e = env(pool.clone()).await;

    // The seeded track is 180 s: half of it (90 s) is required.
    let out = play(&e, &e.ana, s.track, 90.0).await;
    assert_eq!(out["recorded"], true);
    assert!(out["playId"].is_string());

    assert_eq!(
        stored_plays(&pool, e.ana_id).await,
        vec![(s.track, true, "LIBRARY".into())]
    );
    let ben_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'ben'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored_plays(&pool, ben_id).await.is_empty());
    e.server
        .get("/api/plays")
        .authorization_bearer(&e.ana)
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
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
    let e = env(pool.clone()).await;

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
    assert!(stored_plays(&pool, e.ana_id).await.is_empty());

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
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
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
    assert_eq!(stored_plays(&pool, e.ana_id).await.len(), 1);

    // A play older than one track length (180 s) is a different listen.
    sqlx::query("UPDATE plays SET played_at = now() - interval '181 seconds' WHERE user_id = $1")
        .bind(e.ana_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(play(&e, &e.ana, s.track, 100.0).await["recorded"], true);
    assert_eq!(stored_plays(&pool, e.ana_id).await.len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_reports_store_one_row(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", None, "Song")).await;
    let e = env(pool.clone()).await;
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
    assert_eq!(stored_plays(&pool, e.ana_id).await.len(), 1);
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
    // `lastPlayedAt` is a JS `Date` on the wire (`toISOString`: milliseconds, `Z`).
    let at = items[0]["lastPlayedAt"].as_str().unwrap();
    assert!(
        at.len() == 24 && at.ends_with('Z') && at.as_bytes()[19] == b'.',
        "{at} is not toISOString"
    );
    assert_eq!(items[0]["albumCount"], 1);

    let limited = get(&e, &e.ana, "/api/library/recently-listened?limit=1").await;
    assert_eq!(limited["items"].as_array().unwrap().len(), 1);
    // Another user has heard nothing.
    assert_eq!(
        get(&e, &e.ben, "/api/library/recently-listened").await,
        json!({ "items": [] })
    );
    // Historical rows remain persisted even when a track becomes unplayable.
    assert_eq!(stored_plays(&pool, e.ana_id).await.len(), 4);
}

/// Routes upstream v1.12.3 removed (plan Appendix C). `GET /api/plays` and the bulk
/// `DELETE /api/playlists/{id}/items` were served by kima-rs; another method is still routed on
/// each path (`POST`), so axum answers them 405 (owner decision D-172, applied to D-173). Every
/// other removed path must stay 404 so a future re-add is caught.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn removed_routes_are_404(pool: PgPool) {
    let e = env(pool).await;
    e.server
        .get("/api/plays")
        .authorization_bearer(&e.ana)
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);
    e.server
        .delete("/api/playlists/x/items")
        .authorization_bearer(&e.ana)
        .json(&json!({ "trackIds": [] }))
        .await
        .assert_status(StatusCode::METHOD_NOT_ALLOWED);

    let removed: &[(&str, &str)] = &[
        ("POST", "/api/offline/albums/x/download"),
        ("POST", "/api/offline/tracks/x/complete"),
        ("GET", "/api/offline/albums"),
        ("DELETE", "/api/offline/albums/x"),
        ("GET", "/api/offline/stats"),
        ("GET", "/api/downloads"),
        ("GET", "/api/downloads/x"),
        ("PATCH", "/api/downloads/x"),
        ("GET", "/api/recommendations"),
        ("GET", "/api/recommendations/albums"),
        ("GET", "/api/recommendations/tracks"),
        ("GET", "/api/releases/upcoming"),
        ("GET", "/api/releases/recent"),
        ("POST", "/api/spotify/parse"),
        ("GET", "/api/spotify/import/session-log"),
        ("GET", "/api/discover/exclusions"),
        ("DELETE", "/api/discover/exclusions"),
        ("DELETE", "/api/discover/exclusions/x"),
        ("GET", "/api/discover/generate/status/x"),
        ("GET", "/api/library/cover-art-colors"),
        ("GET", "/api/analysis/track/x"),
        ("POST", "/api/enrichment/artist/x"),
        ("POST", "/api/enrichment/album/x"),
        ("POST", "/api/onboarding/enrichment"),
        ("GET", "/api/browse/playlists/featured"),
        ("POST", "/api/playlists/x/pending/reconcile"),
        ("POST", "/api/mixes/mood/buckets/backfill"),
        ("POST", "/api/system-settings/test-spotify"),
        ("GET", "/api/artists/preview/a/t"),
        ("GET", "/api/podcasts/p/episodes/e/cache-status"),
        ("GET", "/api/vibe/status"),
    ];
    let mut wrong = Vec::new();
    for &(method, path) in removed {
        let req = match method {
            "GET" => e.server.get(path),
            "POST" => e.server.post(path).json(&json!({})),
            "PATCH" => e.server.patch(path).json(&json!({})),
            "DELETE" => e.server.delete(path),
            _ => unreachable!(),
        };
        let status = req.authorization_bearer(&e.ana).await.status_code();
        if status != StatusCode::NOT_FOUND {
            wrong.push(format!("{method} {path} -> {status}"));
        }
    }
    assert!(wrong.is_empty(), "removed routes not 404: {wrong:#?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_plays_returns_stored_source(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", Some(2000), "Song")).await;
    let e = env(pool.clone()).await;
    sqlx::query("UPDATE tracks SET duration_secs = 200 WHERE id = $1")
        .bind(s.track)
        .execute(&pool)
        .await
        .unwrap();
    // Played through the web API: LIBRARY.
    assert_eq!(play(&e, &e.ana, s.track, 150.0).await["recorded"], true);
    // A Subsonic play, a day earlier (outside the duplicate window).
    sqlx::query(
        "INSERT INTO plays (id, user_id, track_id, played_at, heard, source)
         VALUES ($1, $2, $3, now() - interval '1 day', true, 'SUBSONIC')",
    )
    .bind(Uuid::now_v7())
    .bind(e.ana_id)
    .bind(s.track)
    .execute(&pool)
    .await
    .unwrap();
    let sources: Vec<String> = stored_plays(&pool, e.ana_id)
        .await
        .into_iter()
        .map(|p| p.2)
        .collect();
    assert_eq!(sources, ["LIBRARY", "SUBSONIC"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recently_listened_leaves_out_various_artists_and_unknown_artist(pool: PgPool) {
    let real = seed(&pool, Seed::new("Real", "R1", None, "r1")).await;
    let va = seed(&pool, Seed::new("Various Artists", "V1", None, "v1")).await;
    let unknown = seed(&pool, Seed::new("Unknown Artist", "U1", None, "u1")).await;
    let e = env(pool.clone()).await;
    // The compilation and the untagged album were played after the real artist's album.
    for (track, mins) in [(real.track, 30), (va.track, 10), (unknown.track, 20)] {
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
    let v = get(&e, &e.ana, "/api/library/recently-listened?limit=10").await;
    let names: Vec<_> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Real"]);
}

// ---- M6 Task 17: podcasts mixed into /library/recently-listened ----

/// Stores a show with one episode and the caller's progress on it, `mins` minutes ago.
async fn seed_podcast_progress(
    pool: &PgPool,
    user: Uuid,
    title: &str,
    mins: i32,
    finished: bool,
) -> (Uuid, Uuid) {
    let show = kima_db::podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &kima_db::podcasts::NewShow {
            feed_url: &format!("https://feeds.example.invalid/{title}"),
            title,
            author: Some("Some Author"),
            description: None,
            image_url: Some("https://img.example.invalid/cover.png"),
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 1,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    kima_db::podcasts::insert_episodes(
        pool,
        show.id,
        &[kima_db::podcasts::NewEpisode {
            guid: "ep1".into(),
            title: "Episode 1".into(),
            description: None,
            audio_url: "https://audio.example.invalid/ep1.mp3".into(),
            duration: 1000,
            published_at: time::OffsetDateTime::now_utc(),
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: None,
        }],
    )
    .await
    .unwrap();
    let episode: Uuid = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE podcast_id = $1")
        .bind(show.id)
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO podcast_progress (user_id, episode_id, \"current_time\", duration, is_finished, last_played_at)
         VALUES ($1, $2, 100, 1000, $3, now() - make_interval(mins => $4))",
    )
    .bind(user)
    .bind(episode)
    .bind(finished)
    .bind(mins)
    .execute(pool)
    .await
    .unwrap();
    (show.id, episode)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recently_listened_mixes_podcasts_by_last_played(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "A1", None, "a1")).await;
    let e = env(pool.clone()).await;
    // Artist play 40 minutes ago; three shows in progress at 35/25/5 minutes ago.
    sqlx::query(
        "INSERT INTO plays (id, user_id, track_id, played_at, heard)
         VALUES ($1, $2, $3, now() - make_interval(mins => 40), true)",
    )
    .bind(Uuid::now_v7())
    .bind(e.ana_id)
    .bind(a.track)
    .execute(&pool)
    .await
    .unwrap();
    let (_show1, _) = seed_podcast_progress(&pool, e.ana_id, "Show One", 35, false).await;
    let (show2, _) = seed_podcast_progress(&pool, e.ana_id, "Show Two", 25, false).await;
    let (show3, ep3) = seed_podcast_progress(&pool, e.ana_id, "Show Three", 5, false).await;

    let v = get(&e, &e.ana, "/api/library/recently-listened?limit=6").await;
    let items = v["items"].as_array().unwrap();
    let podcasts: Vec<&Value> = items.iter().filter(|i| i["type"] == "podcast").collect();
    // ceil(6/3) = 2 podcast rows: only the two most recently progressed shows, one row each.
    assert_eq!(podcasts.len(), 2, "{items:#?}");
    assert_eq!(podcasts[0]["id"], json!(show3));
    assert_eq!(podcasts[0]["episodeId"], json!(ep3));
    assert_eq!(podcasts[0]["name"], json!("Show Three"));
    assert_eq!(podcasts[0]["author"], json!("Some Author"));
    assert_eq!(podcasts[0]["type"], json!("podcast"));
    assert_eq!(
        podcasts[0]["coverArt"],
        json!("https://img.example.invalid/cover.png")
    );
    assert_eq!(podcasts[0]["progress"], json!(10));
    assert_eq!(podcasts[1]["id"], json!(show2));
    // Overall order is newest `lastPlayedAt` first across artists and podcasts; the oldest
    // show (Show One) is squeezed out by the ceil(limit/3) cap on podcast rows.
    let names: Vec<_> = items
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["Show Three", "Show Two", "Alpha"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recently_listened_podcasts_skip_finished_and_other_users(pool: PgPool) {
    let e = env(pool.clone()).await;
    seed_podcast_progress(&pool, e.ana_id, "Finished Show", 5, true).await;
    let ben_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'ben'")
        .fetch_one(&pool)
        .await
        .unwrap();
    seed_podcast_progress(&pool, ben_id, "Bens Show", 1, false).await;

    let v = get(&e, &e.ana, "/api/library/recently-listened?limit=6").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 0);
}
