mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, seed};
use kima_db::PgPool;
use serde_json::{Value, json};
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tracks_returns_full_playable_shape(pool: PgPool) {
    let row = seed(&pool, Seed::new("Artist", "Album", Some(1994), "Song")).await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "ana").await;
    sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now())")
        .bind(Uuid::now_v7())
        .bind(user.id)
        .bind(row.track)
        .execute(&pool)
        .await
        .unwrap();
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let response = server
        .get("/api/stats/tracks?of=top")
        .authorization_bearer(&token)
        .await;
    response.assert_status_ok();
    assert_eq!(
        response.json::<Value>(),
        json!({"tracks":[{"id":row.track.to_string(),"title":"Song","duration":180,"artist":{"id":row.artist.to_string(),"name":"Artist"},"album":{"id":row.album.to_string(),"title":"Album","coverArt":row.album.to_string()}}]})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tracks_parameters_limits_and_auth(pool: PgPool) {
    for n in 0..105 {
        seed(
            &pool,
            Seed::new("Artist", "Album", Some(1991), &format!("Song {n:03}")),
        )
        .await;
    }
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    server
        .get("/api/stats/tracks?of=never")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    for query in ["", "of=bogus", "of=top&of=top"] {
        let r = server
            .get(&format!("/api/stats/tracks?{query}"))
            .authorization_bearer(&token)
            .await;
        r.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(r.json::<Value>(), json!({"error":"Bad request"}));
    }
    for (query, count) in [
        ("", 40),
        ("&limit=", 1),
        ("&limit=0", 1),
        ("&limit=-5", 1),
        ("&limit=2.9", 2),
        ("&limit=500", 100),
        ("&limit=abc", 40),
    ] {
        let r = server
            .get(&format!("/api/stats/tracks?of=never{query}"))
            .authorization_bearer(&token)
            .await;
        r.assert_status_ok();
        assert_eq!(
            r.json::<Value>()["tracks"].as_array().unwrap().len(),
            count,
            "{query}"
        );
    }
    for query in [
        "of=key&key=E%20minor",
        "of=key&key=H%20dorian",
        "of=decade&decade=0",
        "of=decade&decade=abc",
    ] {
        let r = server
            .get(&format!("/api/stats/tracks?{query}"))
            .authorization_bearer(&token)
            .await;
        r.assert_status_ok();
        assert_eq!(r.json::<Value>(), json!({"tracks":[]}));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tracks_period_isolation_decade_and_core(pool: PgPool) {
    let a = seed(&pool, Seed::new("Artist", "Album", Some(1981), "A")).await;
    let b = seed(&pool, Seed::new("Artist", "Album", Some(1981), "B")).await;
    let c = seed(&pool, Seed::new("Other", "Other", Some(2005), "C")).await;
    sqlx::query("UPDATE albums SET display_year=1994 WHERE id=$1")
        .bind(a.album)
        .execute(&pool)
        .await
        .unwrap();
    let state = common::state(pool.clone());
    let (ana, token) = common::user_with_token(&state, "ana").await;
    let (ben, _) = common::user_with_token(&state, "ben").await;
    for (user, track, seconds) in [
        (ana.id, a.track, 0),
        (ana.id, a.track, -300),
        (ana.id, b.track, -3000000),
        (ben.id, c.track, 0),
    ] {
        sqlx::query("INSERT INTO plays(id,user_id,track_id,played_at) VALUES ($1,$2,$3,now()+$4 * interval '1 second')").bind(Uuid::now_v7()).bind(user).bind(track).bind(seconds as f64).execute(&pool).await.unwrap();
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let get = |query: &str| format!("/api/stats/tracks?{query}");
    let top = server
        .get(&get("of=top&period=30d"))
        .authorization_bearer(&token)
        .await
        .json::<Value>();
    assert_eq!(top["tracks"].as_array().unwrap().len(), 1);
    assert_eq!(top["tracks"][0]["id"], a.track.to_string());
    let core = server
        .get(&get("of=core&period=30d"))
        .authorization_bearer(&token)
        .await
        .json::<Value>();
    assert_eq!(top, core);
    let all = server
        .get(&get("of=top"))
        .authorization_bearer(&token)
        .await
        .json::<Value>();
    assert_eq!(all["tracks"][0]["id"], a.track.to_string());
    assert_eq!(all["tracks"][1]["id"], b.track.to_string());
    let never = server
        .get(&get("of=never&period=30d"))
        .authorization_bearer(&token)
        .await
        .json::<Value>();
    assert_eq!(never["tracks"].as_array().unwrap().len(), 1);
    assert_eq!(never["tracks"][0]["id"], c.track.to_string());
    let decade = server
        .get(&get("of=decade&decade=1990&period=30d"))
        .authorization_bearer(&token)
        .await
        .json::<Value>();
    let mut ids: Vec<_> = decade["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    let mut expected = vec![a.track.to_string(), b.track.to_string()];
    expected.sort();
    assert_eq!(ids, expected);
}

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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stats_empty_authenticated_contract(pool: PgPool) {
    let state = common::state(pool);
    let (admin, token) = common::user_with_token(&state, "ana").await;
    assert_eq!(admin.role, "admin");
    let server = TestServer::new(kima_api::router(state)).unwrap();
    server
        .get("/api/stats")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let response = server
        .get("/api/stats?period=week")
        .authorization_bearer(&token)
        .await;
    response.assert_status_ok();
    let v = response.json::<Value>();
    assert_eq!(v["period"], "all");
    assert_eq!(
        v["listening"],
        json!({"plays":0,"estimatedSeconds":0,"distinctTracks":0,"distinctArtists":0,"distinctAlbums":0,"firstPlay":null,"lastPlay":null,"bySource":{},"clock":vec![vec![0;24];7],"peak":null,"shape":[],"drift":[],"topArtists":[],"topTracks":[]})
    );
    assert_eq!(
        v["shelf"],
        json!({"tracks":{"total":0,"heard":0},"albums":{"total":0,"heard":0},"artists":{"total":0,"heard":0},"analysis":{"done":0,"failed":0,"pending":0,"total":0},"neverPlayed":0,"decades":[]})
    );
    let dto: kima_api_types::stats::StatsResponse = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(serde_json::to_value(dto).unwrap(), v);
    assert_eq!(
        v.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["period", "listening", "shelf", "spoken", "signature"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        v["spoken"],
        json!({"audiobooks":{"seconds":0,"finished":0,"inProgress":0,"setAside":0,"topAuthors":[],"current":null},"podcasts":{"seconds":0,"finished":0,"inProgress":0,"subscriptions":0,"backlog":0}})
    );
    assert_eq!(v["shelf"]["tracks"], json!({"total":0,"heard":0}));
    assert_eq!(
        v["shelf"]["analysis"],
        json!({"done":0,"failed":0,"pending":0,"total":0})
    );
    assert_eq!(
        v["signature"],
        json!({"concentration":null,"albumHabit":null,"era":null,"key":null,"ownedVsPlayed":[],"hourly":[]})
    );
    assert!(v.get("system").is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stats_shelf_and_listening_are_isolated_per_user(pool: PgPool) {
    let first = seed(&pool, Seed::new("Artist", "Album", Some(1994), "One")).await;
    let second = seed(
        &pool,
        Seed::new("Artist", "Album", Some(1994), "Two").at(1, 2),
    )
    .await;
    let state = common::state(pool.clone());
    let (ana, ana_token) = common::user_with_token(&state, "ana").await;
    let (ben, ben_token) = common::user_with_token(&state, "ben").await;
    for (user, track, stamp) in [
        (ana.id, first.track, "2026-03-01T12:00:00Z"),
        (ana.id, first.track, "2026-03-01T12:00:20Z"),
        (ben.id, second.track, "2026-03-01T12:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,$4::timestamptz)",
        )
        .bind(Uuid::now_v7())
        .bind(user)
        .bind(track)
        .bind(stamp)
        .execute(&pool)
        .await
        .unwrap();
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for (token, title) in [(&ana_token, "One"), (&ben_token, "Two")] {
        let r = server
            .get("/api/stats?period=all")
            .authorization_bearer(token)
            .await;
        r.assert_status_ok();
        let v = r.json::<Value>();
        assert_eq!(v["listening"]["plays"], 1);
        assert_eq!(v["listening"]["topTracks"][0]["title"], title);
        assert_eq!(v["shelf"]["tracks"], json!({"total":2,"heard":1}));
        assert_eq!(v["shelf"]["neverPlayed"], 1);
        assert_eq!(
            v["shelf"]["decades"],
            json!([{"decade":1990,"albums":1,"heard":1}])
        );
        assert_eq!(
            v["signature"]["concentration"],
            json!({"forHalf":1,"forEighty":1,"distinct":1})
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stats_habit_era_decades_and_period(pool: PgPool) {
    let mut tracks = Vec::new();
    for no in [1, 2, 3, 5] {
        let title = format!("Song {no}");
        tracks.push(
            seed(
                &pool,
                Seed::new("Artist", "Record", Some(1994), &title).at(1, no),
            )
            .await
            .track,
        );
    }
    let ancient = seed(&pool, Seed::new("Other", "Old", Some(1905), "Old song")).await;
    let recent = seed(
        &pool,
        Seed::new("Third", "Modern", Some(1912), "Modern song"),
    )
    .await;
    sqlx::query("UPDATE albums SET display_year=2022 WHERE id=$1")
        .bind(recent.album)
        .execute(&pool)
        .await
        .unwrap();
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "ana").await;
    for (i, track) in tracks.iter().enumerate() {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now() - interval '2 days' + $4 * interval '200 seconds')")
            .bind(Uuid::now_v7()).bind(user.id).bind(track).bind(i as f64).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now() - interval '400 days')")
        .bind(Uuid::now_v7()).bind(user.id).bind(ancient.track).execute(&pool).await.unwrap();
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let r = server
        .get("/api/stats?period=30d")
        .authorization_bearer(&token)
        .await;
    r.assert_status_ok();
    let v = r.json::<Value>();
    assert_eq!(v["period"], "30d");
    assert_eq!(v["listening"]["plays"], 4);
    assert_eq!(v["shelf"]["tracks"], json!({"total":6,"heard":5}));
    assert_eq!(
        v["shelf"]["decades"],
        json!([{"decade":1990,"albums":1,"heard":1},{"decade":2020,"albums":1,"heard":0}])
    );
    assert_eq!(
        v["signature"]["concentration"],
        json!({"forHalf":2,"forEighty":4,"distinct":4})
    );
    assert_eq!(
        v["signature"]["albumHabit"],
        json!({"moves":3,"sameAlbumPct":100,"inOrderPct":66.7})
    );
    assert_eq!(
        v["signature"]["era"],
        json!({"medianYear":1994,"buckets":[{"label":"before 2000","plays":4,"percent":100}]})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stats_habit_excludes_exactly_fifteen_minutes(pool: PgPool) {
    let first = seed(
        &pool,
        Seed::new("Artist", "Record", Some(2005), "One").at(1, 1),
    )
    .await;
    let second = seed(
        &pool,
        Seed::new("Artist", "Record", Some(2005), "Two").at(1, 2),
    )
    .await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "ana").await;
    for (track, offset) in [(first.track, 0), (second.track, 900)] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,'2026-01-01T00:00:00Z'::timestamptz+$4*interval '1 second')").bind(Uuid::now_v7()).bind(user.id).bind(track).bind(offset as f64).execute(&pool).await.unwrap();
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let r = server.get("/api/stats").authorization_bearer(&token).await;
    r.assert_status_ok();
    assert_eq!(r.json::<Value>()["signature"]["albumHabit"], Value::Null);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn region_validates_like_js_and_requires_auth(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    server
        .get("/api/stats/region?x=12&y=18")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    for query in [
        "x=0&y=18",
        "x=25&y=18",
        "x=1.5&y=18",
        "y=18",
        "x=12&x=13&y=18",
        "x=12&y=",
        "x=12&y=Infinity",
    ] {
        let r = server
            .get(&format!("/api/stats/region?{query}"))
            .authorization_bearer(&token)
            .await;
        r.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(r.json::<Value>(), json!({"error":"Bad request"}));
    }
    for query in ["x=12&y=18", "x=0x18&y=1&period=garbage", "x=1.0&y=24"] {
        let r = server
            .get(&format!("/api/stats/region?{query}"))
            .authorization_bearer(&token)
            .await;
        r.assert_status_ok();
        let x = if query.starts_with("x=12") {
            12
        } else if query.starts_with("x=0x") {
            24
        } else {
            1
        };
        let y = if x == 12 {
            18
        } else if x == 24 {
            1
        } else {
            24
        };
        assert_eq!(
            r.json::<Value>(),
            json!({"x":x,"y":y,"listens":0,"tracks":[],"energy":null,"valence":null,"bpm":null})
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn false_starts_empty_history(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "audit").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let response = server
        .get("/api/stats/false-starts")
        .authorization_bearer(&token)
        .await;
    response.assert_status_ok();
    assert_eq!(
        response.json::<Value>(),
        json!({"totalRows":0,"totalListens":0,"duplicateRows":0,"collapsibleRows":0,"heard":0,"skipped":0,"tooShort":0,"unclear":0,"earliest":null,"tracks":[],"ignored":[]})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn false_starts_reports_provable_skips_and_isolates_users(pool: PgPool) {
    let a = seed(&pool, Seed::new("Artist", "Album", None, "Song A")).await;
    let b = seed(&pool, Seed::new("Artist", "Album", None, "Song B")).await;
    sqlx::query("UPDATE tracks SET duration_secs=240 WHERE id=$1")
        .bind(a.track)
        .execute(&pool)
        .await
        .unwrap();
    let state = common::state(pool.clone());
    let (ana, token) = common::user_with_token(&state, "audit-ana").await;
    let (ben, other) = common::user_with_token(&state, "audit-ben").await;
    for (u, track, sec) in [
        (ana.id, a.track, 0),
        (ana.id, b.track, 90),
        (ana.id, b.track, 400),
        (ben.id, b.track, 0),
    ] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at,heard) VALUES ($1,$2,$3,'2026-03-01T12:00:00Z'::timestamptz + $4 * interval '1 second',false)")
            .bind(Uuid::now_v7()).bind(u).bind(track).bind(sec as f64).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO play_audit_ignore(user_id,track_id,ignored_at) VALUES ($1,$2,'2026-11-11T10:00:00Z')").bind(ana.id).bind(b.track).execute(&pool).await.unwrap();
    let server = TestServer::new(kima_api::router(state)).unwrap();
    server
        .get("/api/stats/false-starts")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let result = server
        .get("/api/stats/false-starts")
        .authorization_bearer(&token)
        .await;
    result.assert_status_ok();
    assert_eq!(
        result.json::<Value>(),
        json!({"totalRows":3,"totalListens":3,"duplicateRows":0,"collapsibleRows":0,"heard":0,"skipped":1,"tooShort":0,"unclear":2,"earliest":"2026-03-01T12:00:00.000Z","tracks":[{"trackId":a.track.to_string(),"title":"Song A","artist":"Artist","durationSeconds":240,"requiredSeconds":120,"skipped":1,"tooShort":0,"heard":0,"listens":1,"rows":1,"shortestRunSeconds":90,"lastAt":"2026-03-01T12:00:00.000Z"}],"ignored":[{"trackId":b.track.to_string(),"title":"Song B","artist":"Artist","ignoredAt":"2026-11-11T10:00:00.000Z"}]})
    );
    let other = server
        .get("/api/stats/false-starts")
        .authorization_bearer(&other)
        .await
        .json::<Value>();
    assert_eq!(other["totalRows"], 1);
    assert_eq!(other["ignored"], json!([]));
    assert_eq!(other["unclear"], 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn false_starts_native_history_is_all_heard(pool: PgPool) {
    let a = seed(&pool, Seed::new("Artist", "Album", None, "Native A")).await;
    let b = seed(&pool, Seed::new("Artist", "Album", None, "Native B")).await;
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "audit-native").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for t in [a.track, b.track] {
        let r = server
            .post("/api/plays")
            .authorization_bearer(&token)
            .json(&json!({"trackId":t,"secondsListened":90}))
            .await;
        r.assert_status_ok();
        assert_eq!(r.json::<Value>()["recorded"], true);
    }
    let v = server
        .get("/api/stats/false-starts")
        .authorization_bearer(&token)
        .await
        .json::<Value>();
    assert_eq!(v["totalListens"], 2);
    assert_eq!(v["heard"], 2);
    assert_eq!(v["skipped"], 0);
}

// ---- Task 10: POST /api/stats/false-starts/{collapse,remove,ignore,restore} ----

const ACTIONS: [&str; 4] = ["collapse", "remove", "ignore", "restore"];

/// One `plays` row at 2026-03-01T12:00:00Z + `second`.
async fn play(pool: &PgPool, user: Uuid, track: Uuid, second: i64, heard: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at,heard) VALUES ($1,$2,$3,'2026-03-01T12:00:00Z'::timestamptz + $4 * interval '1 second',$5)")
        .bind(id)
        .bind(user)
        .bind(track)
        .bind(second as f64)
        .bind(heard)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn play_ids(pool: &PgPool) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM plays ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

async fn audit(server: &TestServer, token: &str) -> Value {
    let response = server
        .get("/api/stats/false-starts")
        .authorization_bearer(token)
        .await;
    response.assert_status_ok();
    response.json::<Value>()
}

async fn track(pool: &PgPool, title: &str) -> Uuid {
    seed(pool, Seed::new("Artist", "Album", Some(1994), title))
        .await
        .track
}

fn verdicts(v: &Value) -> Value {
    json!([
        v["totalListens"],
        v["heard"],
        v["skipped"],
        v["tooShort"],
        v["unclear"]
    ])
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn collapse_keeps_first_and_last_row(pool: PgPool) {
    let song = track(&pool, "Song").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "collapse_user").await;
    let (other, other_token) = common::user_with_token(&state, "collapse_other").await;
    let mut ids = Vec::new();
    for second in [0, 10, 20, 30] {
        ids.push(play(&pool, user.id, song, second, false).await);
    }
    let theirs = vec![
        play(&pool, other.id, song, 0, false).await,
        play(&pool, other.id, song, 5, false).await,
        play(&pool, other.id, song, 6, false).await,
    ];
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let before = audit(&server, &token).await;
    assert_eq!(before["collapsibleRows"], 2);
    assert_eq!(verdicts(&before), json!([1, 0, 0, 0, 1]));
    let response = server
        .post("/api/stats/false-starts/collapse")
        .authorization_bearer(&token)
        .json(&json!({"trackIds":[]}))
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"removed":2}));
    let mut expected = vec![ids[0], ids[3]];
    expected.extend(&theirs);
    assert_eq!(play_ids(&pool).await, sorted(expected));
    let after = audit(&server, &token).await;
    assert_eq!(verdicts(&after), verdicts(&before));
    assert_eq!(
        (
            &after["totalRows"],
            &after["duplicateRows"],
            &after["collapsibleRows"]
        ),
        (&json!(2), &json!(1), &json!(0))
    );
    assert_eq!(audit(&server, &other_token).await["collapsibleRows"], 1);
    let response = server
        .post("/api/stats/false-starts/collapse")
        .authorization_bearer(&token)
        .json(&json!({}))
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"removed":0}));
}

/// Owner decision 2026-11-12 (D-179): upstream would delete the only heard row
/// and turn the listen into a skip; kima-rs keeps it so the verdict holds.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn collapse_keeps_a_heard_middle_row(pool: PgPool) {
    let song = track(&pool, "Song").await;
    let next = track(&pool, "Next").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "collapse_heard").await;
    let ids = [
        play(&pool, user.id, song, 0, false).await,
        play(&pool, user.id, song, 1, true).await,
        play(&pool, user.id, song, 2, false).await,
        play(&pool, user.id, song, 3, false).await,
        play(&pool, user.id, next, 4, false).await,
    ];
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let before = audit(&server, &token).await;
    assert_eq!(before["collapsibleRows"], 1);
    assert_eq!(verdicts(&before), json!([2, 1, 0, 0, 1]));
    let response = server
        .post("/api/stats/false-starts/collapse")
        .authorization_bearer(&token)
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"removed":1}));
    assert_eq!(
        play_ids(&pool).await,
        sorted(vec![ids[0], ids[1], ids[3], ids[4]])
    );
    let after = audit(&server, &token).await;
    assert_eq!(verdicts(&after), verdicts(&before));
    assert_eq!(after["tracks"], json!([]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn collapse_ignores_its_body(pool: PgPool) {
    let song = track(&pool, "Song").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "collapse_body").await;
    for second in [0, 10, 20] {
        play(&pool, user.id, song, second, false).await;
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let response = server
        .post("/api/stats/false-starts/collapse")
        .authorization_bearer(&token)
        .json(&json!({"trackIds":"junk","extra":[1]}))
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"removed":1}));
    for (content_type, body) in [
        ("application/json", "[]"),
        ("application/json", "{\"trackIds\":[]}"),
        ("application/json", ""),
        ("application/json", "\u{feff}{}"),
        ("text/plain", "{"),
        ("application/vnd.api+json", "{"),
    ] {
        let response = server
            .post("/api/stats/false-starts/collapse")
            .authorization_bearer(&token)
            .content_type(content_type)
            .bytes(body.as_bytes().to_vec().into())
            .await;
        response.assert_status_ok();
        assert_eq!(response.json::<Value>(), json!({"removed":0}), "{body:?}");
    }
    let response = server
        .post("/api/stats/false-starts/collapse")
        .authorization_bearer(&token)
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"removed":0}));
    assert_eq!(play_ids(&pool).await.len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn remove_deletes_only_provable_skips_of_named_tracks(pool: PgPool) {
    let a = track(&pool, "A").await;
    let b = track(&pool, "B").await;
    let c = track(&pool, "C").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "remove_user").await;
    let (other, other_token) = common::user_with_token(&state, "remove_other").await;
    let doomed = [
        play(&pool, user.id, a, 0, false).await,
        play(&pool, user.id, a, 10, false).await,
    ];
    let kept = vec![
        play(&pool, user.id, b, 20, false).await,
        play(&pool, user.id, c, 40, true).await,
        play(&pool, user.id, a, 300, true).await,
        play(&pool, other.id, a, 0, false).await,
        play(&pool, other.id, c, 10, true).await,
    ];
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let before = audit(&server, &token).await;
    assert_eq!(before["skipped"], 2);
    assert_eq!(audit(&server, &other_token).await["skipped"], 1);
    let response = server
        .post("/api/stats/false-starts/remove")
        .authorization_bearer(&token)
        .json(&json!({"trackIds":[a.to_string(), a.to_string(), "not-a-uuid"]}))
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"removed":2,"tracks":1}));
    let remaining = play_ids(&pool).await;
    assert_eq!(remaining, sorted(kept));
    assert!(doomed.iter().all(|id| !remaining.contains(id)));
    let after = audit(&server, &token).await;
    assert_eq!(after["skipped"], 1);
    assert_eq!(after["tracks"][0]["trackId"], b.to_string());
    assert_eq!(audit(&server, &other_token).await["skipped"], 1);
    let response = server
        .post("/api/stats/false-starts/remove")
        .authorization_bearer(&token)
        .json(&json!({"trackIds":[a.to_string()]}))
        .await;
    assert_eq!(response.json::<Value>(), json!({"removed":0,"tracks":0}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn remove_skips_ignored_tracks(pool: PgPool) {
    let a = track(&pool, "A").await;
    let b = track(&pool, "B").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "remove_ignored").await;
    play(&pool, user.id, a, 0, false).await;
    play(&pool, user.id, b, 10, true).await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let ids = json!({"trackIds":[a.to_string()]});
    let response = server
        .post("/api/stats/false-starts/ignore")
        .authorization_bearer(&token)
        .json(&ids)
        .await;
    assert_eq!(response.json::<Value>(), json!({"ignored":1}));
    let response = server
        .post("/api/stats/false-starts/remove")
        .authorization_bearer(&token)
        .json(&ids)
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"removed":0,"tracks":0}));
    assert_eq!(play_ids(&pool).await.len(), 2);
    let after = audit(&server, &token).await;
    assert_eq!(after["skipped"], 1);
    assert_eq!(after["tracks"], json!([]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn remove_rejudges_current_rows(pool: PgPool) {
    let a = track(&pool, "A").await;
    let b = track(&pool, "B").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "remove_rejudge").await;
    play(&pool, user.id, a, 0, false).await;
    play(&pool, user.id, b, 10, true).await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    assert_eq!(audit(&server, &token).await["skipped"], 1);
    play(&pool, user.id, a, 5, true).await;
    let response = server
        .post("/api/stats/false-starts/remove")
        .authorization_bearer(&token)
        .json(&json!({"trackIds":[a.to_string()]}))
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!({"removed":0,"tracks":0}));
    assert_eq!(play_ids(&pool).await.len(), 3);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn remove_never_touches_another_user(pool: PgPool) {
    let a = track(&pool, "A").await;
    let b = track(&pool, "B").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "remove_mine").await;
    let (other, other_token) = common::user_with_token(&state, "remove_theirs").await;
    let theirs = [
        play(&pool, other.id, a, 0, false).await,
        play(&pool, other.id, b, 10, true).await,
    ];
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let body = json!({"trackIds":[a.to_string()]});
    let response = server
        .post("/api/stats/false-starts/remove")
        .authorization_bearer(&token)
        .json(&body)
        .await;
    assert_eq!(response.json::<Value>(), json!({"removed":0,"tracks":0}));
    let mine = play(&pool, user.id, a, 0, false).await;
    play(&pool, user.id, b, 10, true).await;
    let response = server
        .post("/api/stats/false-starts/remove")
        .authorization_bearer(&token)
        .json(&body)
        .await;
    assert_eq!(response.json::<Value>(), json!({"removed":1,"tracks":1}));
    let remaining = play_ids(&pool).await;
    assert!(!remaining.contains(&mine));
    assert!(theirs.iter().all(|id| remaining.contains(id)));
    assert_eq!(audit(&server, &other_token).await["skipped"], 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ignore_skips_unknown_ids_and_duplicates_and_restore_counts(pool: PgPool) {
    let song = track(&pool, "Song").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "ignore_user").await;
    let (other, other_token) = common::user_with_token(&state, "ignore_other").await;
    let kept = play(&pool, user.id, song, 0, false).await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let body = json!({"trackIds":[song.to_string(), song.to_string(), Uuid::now_v7().to_string(), "invalid"], "extra": 1});
    for (auth, expected) in [(&token, 1), (&token, 0), (&other_token, 1)] {
        let response = server
            .post("/api/stats/false-starts/ignore")
            .authorization_bearer(auth)
            .json(&body)
            .await;
        response.assert_status_ok();
        assert_eq!(response.json::<Value>(), json!({"ignored":expected}));
    }
    assert_eq!(play_ids(&pool).await, vec![kept]);
    for expected in [1, 0] {
        let response = server
            .post("/api/stats/false-starts/restore")
            .authorization_bearer(&token)
            .json(&body)
            .await;
        response.assert_status_ok();
        assert_eq!(response.json::<Value>(), json!({"restored":expected}));
    }
    let users: Vec<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM play_audit_ignore WHERE track_id=$1")
            .bind(song)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(users, vec![other.id]);
    assert_ne!(user.id, other.id);
    assert_eq!(audit(&server, &token).await["ignored"], json!([]));
    assert_eq!(
        audit(&server, &other_token).await["ignored"][0]["trackId"],
        song.to_string()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_ids_schema_like_zod(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "zod_user").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let many = |n: usize| json!({"trackIds": vec!["x"; n]});
    let text = |s: String| json!({"trackIds": [s]});
    let bad = [
        json!({"trackIds":[]}),
        json!({}),
        json!({"trackIds":"a"}),
        json!({"trackIds":null}),
        json!({"trackIds":[1]}),
        json!({"trackIds":["a", null]}),
        json!([{"trackIds":["a"]}]),
        many(2001),
        text(String::new()),
        text("a".repeat(65)),
        text("\u{e9}".repeat(65)),
        text("\u{1f3b5}".repeat(33)),
    ];
    let good = [
        json!({"trackIds":["a"],"extra":1}),
        many(2000),
        text("a".repeat(64)),
        text("\u{e9}".repeat(64)),
        text("\u{1f3b5}".repeat(32)),
    ];
    for action in ["remove", "ignore", "restore"] {
        let url = format!("/api/stats/false-starts/{action}");
        for body in &bad {
            let response = server
                .post(&url)
                .authorization_bearer(&token)
                .json(body)
                .await;
            response.assert_status(StatusCode::BAD_REQUEST);
            assert_eq!(response.json::<Value>(), json!({"error":"Bad request"}));
        }
        for (content_type, body) in [
            ("text/plain", "{\"trackIds\":[\"a\"]}"),
            ("application/json", ""),
        ] {
            let response = server
                .post(&url)
                .authorization_bearer(&token)
                .content_type(content_type)
                .bytes(body.as_bytes().to_vec().into())
                .await;
            response.assert_status(StatusCode::BAD_REQUEST);
            assert_eq!(response.json::<Value>(), json!({"error":"Bad request"}));
        }
        let response = server.post(&url).authorization_bearer(&token).await;
        response.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(response.json::<Value>(), json!({"error":"Bad request"}));
        for body in &good {
            server
                .post(&url)
                .authorization_bearer(&token)
                .json(body)
                .await
                .assert_status_ok();
        }
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn malformed_json_matches_upstream_body_parser(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "malformed_user").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for action in ACTIONS {
        for content_type in [
            "application/json",
            "application/json; charset=utf-8",
            "Application/JSON",
        ] {
            for body in [
                "{",
                "   ",
                "5",
                "\"x\"",
                "null",
                "true",
                "{\"trackIds\":[\"a\"],}",
            ] {
                let response = server
                    .post(&format!("/api/stats/false-starts/{action}"))
                    .authorization_bearer(&token)
                    .content_type(content_type)
                    .bytes(body.as_bytes().to_vec().into())
                    .await;
                response.assert_status(StatusCode::BAD_REQUEST);
                assert_eq!(
                    response.json::<Value>(),
                    json!({"error":"Invalid request body"}),
                    "{action} {content_type} {body:?}"
                );
            }
        }
        let response = server
            .post(&format!("/api/stats/false-starts/{action}"))
            .authorization_bearer(&token)
            .content_type("application/json")
            .bytes(b"\xef\xbb\xbf {\"trackIds\":[\"a\"]}".to_vec().into())
            .await;
        response.assert_status_ok();
    }
}

/// D-178: `serde_json` refuses a number outside `f64`; `JSON.parse` reads it as
/// `Infinity`, so upstream answers `Bad request` (zod) or 200 (`collapse`).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn number_overflow_is_an_invalid_body(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "overflow_user").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for action in ACTIONS {
        let response = server
            .post(&format!("/api/stats/false-starts/{action}"))
            .authorization_bearer(&token)
            .content_type("application/json")
            .bytes(b"{\"trackIds\":[1e400]}".to_vec().into())
            .await;
        response.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<Value>(),
            json!({"error":"Invalid request body"}),
            "{action}"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bodies_over_one_mebibyte_are_too_large(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "limit_user").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let padded = |len: usize| {
        let mut body = b"{\"trackIds\":[\"a\"]}".to_vec();
        body.resize(len, b' ');
        body
    };
    for action in ACTIONS {
        let url = format!("/api/stats/false-starts/{action}");
        let response = server
            .post(&url)
            .authorization_bearer(&token)
            .content_type("application/json")
            .bytes(padded(1024 * 1024 + 1).into())
            .await;
        response.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response.json::<Value>(),
            json!({"error":"Request body too large"})
        );
        server
            .post(&url)
            .authorization_bearer(&token)
            .content_type("application/json")
            .bytes(padded(1024 * 1024).into())
            .await
            .assert_status_ok();
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn actions_require_auth(pool: PgPool) {
    let song = track(&pool, "Song").await;
    let state = common::state(pool.clone());
    let (user, _) = common::user_with_token(&state, "auth_user").await;
    for second in [0, 10, 20] {
        play(&pool, user.id, song, second, false).await;
    }
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for action in ACTIONS {
        server
            .post(&format!("/api/stats/false-starts/{action}"))
            .json(&json!({"trackIds":[song.to_string()]}))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    assert_eq!(play_ids(&pool).await.len(), 3);
    let ignored: i64 = sqlx::query_scalar("SELECT count(*) FROM play_audit_ignore")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ignored, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_deletes_roll_back_and_answer_500(pool: PgPool) {
    let a = track(&pool, "A").await;
    let b = track(&pool, "B").await;
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "rollback_user").await;
    for second in [0, 10, 20, 30] {
        play(&pool, user.id, a, second, false).await;
    }
    play(&pool, user.id, b, 40, true).await;
    sqlx::raw_sql(
        "CREATE TABLE fail_on (id uuid PRIMARY KEY);
         CREATE FUNCTION fail_delete() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF EXISTS (SELECT 1 FROM fail_on WHERE id = OLD.id) THEN
             RAISE EXCEPTION 'injected delete failure';
           END IF;
           RETURN OLD;
         END $$;
         CREATE TRIGGER fail_delete BEFORE DELETE ON plays
           FOR EACH ROW EXECUTE FUNCTION fail_delete();",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Fail on the last row each action would delete, after the others went.
    let last: Uuid = sqlx::query_scalar(
        "SELECT id FROM plays WHERE track_id=$1 ORDER BY played_at OFFSET 2 LIMIT 1",
    )
    .bind(a)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO fail_on VALUES ($1)")
        .bind(last)
        .execute(&pool)
        .await
        .unwrap();
    let before = play_ids(&pool).await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    for (action, ids) in [
        ("collapse", json!({})),
        ("remove", json!({"trackIds":[a.to_string()]})),
    ] {
        let response = server
            .post(&format!("/api/stats/false-starts/{action}"))
            .authorization_bearer(&token)
            .json(&ids)
            .await;
        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.json::<Value>(),
            json!({"error":"That didn't finish"})
        );
        assert_eq!(play_ids(&pool).await, before, "{action}");
    }
}
