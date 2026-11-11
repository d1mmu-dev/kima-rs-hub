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
