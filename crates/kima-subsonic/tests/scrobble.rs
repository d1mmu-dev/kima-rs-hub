//! `scrobble` and `getNowPlaying` on the SQL fixture (spec §4.3 #25, §4.5 #34, §6.3).

mod common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_db::PgPool;
use kima_db::playback::{self, Patch};
use kima_subsonic::now_playing::{NowPlaying, player_id};
use serde_json::Value;
use uuid::Uuid;

fn app(pool: PgPool) -> TestServer {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(fixture_state(pool)).layer(MockConnectInfo(peer));
    TestServer::new(router).unwrap()
}

async fn alice(server: &TestServer, endpoint: &str, params: &[(&str, &str)]) -> Value {
    let mut all = vec![("apiKey", FIXTURE_TOKEN)];
    all.extend_from_slice(params);
    rest(server, endpoint, &all).await
}

async fn admin(server: &TestServer, endpoint: &str, params: &[(&str, &str)]) -> Value {
    let mut all = vec![("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)];
    all.extend_from_slice(params);
    rest(server, endpoint, &all).await
}

macro_rules! fixture_test {
    ($name:ident, |$server:ident, $pool:ident| $body:block) => {
        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "fixtures", scripts("subsonic"))
        )]
        async fn $name($pool: PgPool) {
            let $server = app($pool.clone());
            $body
        }
    };
}

fn ok(res: &Value) {
    assert_eq!(res["status"], "ok", "{res}");
}

/// Alice's SUBSONIC plays of the track, as RFC 3339 UTC times.
async fn subsonic_plays(pool: &PgPool, track: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT to_char(played_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
         FROM plays WHERE user_id = $1 AND track_id = $2 AND source = 'SUBSONIC'
         ORDER BY played_at",
    )
    .bind(ALICE_ID)
    .bind(track)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn play_count(pool: &PgPool, track: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM plays WHERE user_id = $1 AND track_id = $2")
        .bind(ALICE_ID)
        .bind(track)
        .fetch_one(pool)
        .await
        .unwrap()
}

fixture_test!(
    submission_true_records_subsonic_play_at_time,
    |server, pool| {
        let t = track_id(3);
        let res = alice(
            &server,
            "scrobble",
            &[("id", &t.to_string()), ("time", "1700000000000")],
        )
        .await;
        ok(&res);
        assert_eq!(subsonic_plays(&pool, t).await, ["2023-11-14T22:13:20Z"]);
    }
);

fixture_test!(now_playing_hides_missing_track, |server, pool| {
    let t = track_id(3);
    ok(&alice(
        &server,
        "scrobble",
        &[("id", &t.to_string()), ("submission", "false")],
    )
    .await);
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    let np = alice(&server, "getNowPlaying", &[]).await;
    assert_eq!(
        np["nowPlaying"]["entry"].as_array().unwrap().len(),
        0,
        "{np}"
    );
});

fixture_test!(
    submission_false_records_none_shows_in_now_playing,
    |server, pool| {
        let t = track_id(3);
        let res = alice(
            &server,
            "scrobble",
            &[
                ("id", &t.to_string()),
                ("submission", "false"),
                ("c", "Symfonium"),
            ],
        )
        .await;
        ok(&res);
        assert_eq!(play_count(&pool, t).await, 0);
        let np = alice(&server, "getNowPlaying", &[]).await;
        let entries = np["nowPlaying"]["entry"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "{np}");
        assert_eq!(entries[0]["id"], t.to_string());
        assert_eq!(entries[0]["username"], "alice");
        assert_eq!(entries[0]["playerName"], "Symfonium");
        assert_eq!(entries[0]["playerId"], player_id("Symfonium"));
        assert_eq!(entries[0]["minutesAgo"], 0);
        assert_eq!(entries[0]["title"], "Maxwell's Silver Hammer");
        // A submission by the same player ends it.
        ok(&alice(
            &server,
            "scrobble",
            &[("id", &t.to_string()), ("c", "Symfonium")],
        )
        .await);
        let np = alice(&server, "getNowPlaying", &[]).await;
        assert_eq!(
            np["nowPlaying"]["entry"].as_array().unwrap().len(),
            0,
            "{np}"
        );
    }
);

fixture_test!(repeat_in_window_writes_nothing, |server, pool| {
    let t = track_id(3).to_string();
    ok(&alice(
        &server,
        "scrobble",
        &[("id", &t), ("time", "1700000000000")],
    )
    .await);
    ok(&alice(
        &server,
        "scrobble",
        &[("id", &t), ("time", "1700000060000")],
    )
    .await);
    assert_eq!(subsonic_plays(&pool, track_id(3)).await.len(), 1);
});

fixture_test!(two_id_time_pairs_two_plays, |server, pool| {
    let (a, b) = (track_id(3), track_id(4));
    let res = server
        .get("/scrobble.view")
        .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN)])
        .add_query_params([("id", a.to_string()), ("time", "1700000000000".into())])
        .add_query_params([("id", b.to_string()), ("time", "1700000500000".into())])
        .await;
    ok(&res.json::<Value>()["subsonic-response"]);
    assert_eq!(subsonic_plays(&pool, a).await, ["2023-11-14T22:13:20Z"]);
    assert_eq!(subsonic_plays(&pool, b).await, ["2023-11-14T22:21:40Z"]);
});

fixture_test!(future_time_clamped_to_now, |server, pool| {
    let t = track_id(3);
    let far = (time::OffsetDateTime::now_utc().unix_timestamp() + 3600) * 1000;
    ok(&alice(
        &server,
        "scrobble",
        &[("id", &t.to_string()), ("time", &far.to_string())],
    )
    .await);
    let at: time::OffsetDateTime = sqlx::query_scalar(
        "SELECT played_at FROM plays WHERE user_id = $1 AND track_id = $2 AND source = 'SUBSONIC'",
    )
    .bind(ALICE_ID)
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    let skew = (time::OffsetDateTime::now_utc() - at).whole_seconds().abs();
    assert!(skew < 30, "played_at is {skew}s from now");
});

fixture_test!(unknown_and_missing_ids_skipped_ok, |server, pool| {
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM plays")
        .fetch_one(&pool)
        .await
        .unwrap();
    for id in [
        Uuid::now_v7().to_string(),
        track_id(20).to_string(),
        "nonsense".to_owned(),
    ] {
        ok(&alice(&server, "scrobble", &[("id", &id)]).await);
    }
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM plays")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    // No id at all is 10.
    let res = alice(&server, "scrobble", &[]).await;
    assert_eq!(res["error"]["code"], 10, "{res}");
    assert_eq!(res["error"]["message"], "Required parameter is missing: id");
});

fixture_test!(too_short_track_not_recorded, |server, pool| {
    let t = track_id(3);
    sqlx::query("UPDATE tracks SET duration_secs = 25 WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    ok(&alice(&server, "scrobble", &[("id", &t.to_string())]).await);
    assert_eq!(play_count(&pool, t).await, 0);
});

fixture_test!(
    now_playing_merges_web_state_one_per_user_player,
    |server, pool| {
        let (a, b, c) = (track_id(3), track_id(4), track_id(5));
        // Alice: two Subsonic players, the first one reporting twice. Admin: a web player.
        for t in [a, b] {
            alice(
                &server,
                "scrobble",
                &[
                    ("id", &t.to_string()),
                    ("submission", "false"),
                    ("c", "Symfonium"),
                ],
            )
            .await;
        }
        alice(
            &server,
            "scrobble",
            &[
                ("id", &c.to_string()),
                ("submission", "false"),
                ("c", "Feishin"),
            ],
        )
        .await;
        // Alice's web state too, and admin's.
        let web = |t: Uuid| Patch {
            playback_type: "track".into(),
            track_id: Some(Some(t.to_string())),
            ..Patch::default()
        };
        playback::upsert(&pool, ALICE_ID, web(a)).await.unwrap();
        // Admin's web state is stale and does not count.
        playback::upsert(&pool, ADMIN_ID, web(c)).await.unwrap();
        sqlx::query(
            "UPDATE playback_state SET updated_at = now() - interval '3 hours' WHERE user_id = $1",
        )
        .bind(ADMIN_ID)
        .execute(&pool)
        .await
        .unwrap();

        let np = admin(&server, "getNowPlaying", &[]).await;
        let entries = np["nowPlaying"]["entry"].as_array().unwrap();
        let mut seen: Vec<(String, String, String)> = entries
            .iter()
            .map(|e| {
                (
                    e["username"].as_str().unwrap().into(),
                    e["playerName"].as_str().unwrap().into(),
                    e["id"].as_str().unwrap().into(),
                )
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            [
                ("alice".into(), "Feishin".into(), c.to_string()),
                ("alice".into(), "Kima".into(), a.to_string()),
                ("alice".into(), "Symfonium".into(), b.to_string()),
            ],
            "{np}"
        );
        let web_entry = entries.iter().find(|e| e["playerName"] == "Kima").unwrap();
        assert_eq!(web_entry["playerId"], player_id("Kima"));
    }
);

#[test]
fn now_playing_expires() {
    let np = NowPlaying::new();
    let (user, track) = (Uuid::now_v7(), Uuid::now_v7());
    let t0 = Instant::now();
    np.report(user, "alice", "Symfonium", track, 200, t0);
    np.report(
        user,
        "alice",
        "Feishin",
        track,
        200,
        t0 + Duration::from_secs(10),
    );
    assert_eq!(np.entries(t0 + Duration::from_secs(259)).len(), 2);
    // 200 s + 60 s after the start: gone. The later start lives 10 s longer.
    let live = np.entries(t0 + Duration::from_secs(261));
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].player, "Feishin");
    assert!(np.entries(t0 + Duration::from_secs(271)).is_empty());
    // One slot per (user, player): reporting again replaces.
    np.report(
        user,
        "alice",
        "Feishin",
        Uuid::now_v7(),
        200,
        t0 + Duration::from_secs(300),
    );
    np.report(
        user,
        "alice",
        "Feishin",
        track,
        200,
        t0 + Duration::from_secs(301),
    );
    let live = np.entries(t0 + Duration::from_secs(302));
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].track, track);
    np.finish(user, "Feishin");
    assert!(np.entries(t0 + Duration::from_secs(302)).is_empty());
}
