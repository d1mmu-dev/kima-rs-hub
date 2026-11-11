//! Playlists on the SQL fixture (spec §4.6 #42-#46). The `updatePlaylist` cases are the port of
//! upstream's `updatePlaylist.route.test.ts`.

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_db::PgPool;
use serde_json::Value;
use uuid::Uuid;

const ROAD_TRIP: Uuid = fixture_id(5, 1);
const PRIVATE_MIX: Uuid = fixture_id(5, 2);
const ADMIN_PICKS: Uuid = fixture_id(5, 3);

fn app_with_state(pool: PgPool) -> (TestServer, kima_api::AppState) {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let state = fixture_state(pool);
    let router = kima_subsonic::router(state.clone()).layer(MockConnectInfo(peer));
    (TestServer::new(router).unwrap(), state)
}

fn app(pool: PgPool) -> TestServer {
    app_with_state(pool).0
}

/// A call as alice (her app token). `params` may repeat keys.
async fn call(server: &TestServer, endpoint: &str, params: &[(&str, String)]) -> Value {
    let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN)])
        .add_query_params(params)
        .await;
    res.assert_status_ok();
    res.json::<Value>()["subsonic-response"].clone()
}

/// A call as the admin, with the password.
async fn call_admin(server: &TestServer, endpoint: &str, params: &[(&str, String)]) -> Value {
    let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_params([("f", "json"), ("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)])
        .add_query_params(params)
        .await;
    res.assert_status_ok();
    res.json::<Value>()["subsonic-response"].clone()
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

fn tid(n: u128) -> String {
    track_id(n).to_string()
}

fn failed(res: &Value, code: u64) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
}

fn ok(res: &Value) {
    assert_eq!(res["status"], "ok", "{res}");
}

fn entry_ids(res: &Value) -> Vec<String> {
    res["playlist"]["entry"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| e["id"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The playlist's `(track, sort)` rows, straight from the table.
async fn rows(pool: &PgPool, playlist: &str) -> Vec<(Uuid, i32)> {
    sqlx::query_as(
        "SELECT track_id, sort FROM playlist_items WHERE playlist_id = $1::uuid ORDER BY sort, id",
    )
    .bind(playlist)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn assert_dense(rows: &[(Uuid, i32)]) {
    let sorts: Vec<i32> = rows.iter().map(|r| r.1).collect();
    assert_eq!(sorts, (0..rows.len() as i32).collect::<Vec<_>>());
}

/// A new playlist of alice's with these tracks; returns its id.
async fn make(server: &TestServer, name: &str, tracks: &[u128]) -> String {
    let mut params = vec![("name", name.to_owned())];
    params.extend(tracks.iter().map(|n| ("songId", tid(*n))));
    let res = call(server, "createPlaylist", &params).await;
    ok(&res);
    res["playlist"]["id"].as_str().unwrap().to_owned()
}

fn update(playlist: &str, extra: Vec<(&'static str, String)>) -> Vec<(&'static str, String)> {
    let mut params = vec![("playlistId", playlist.to_owned())];
    params.extend(extra);
    params
}

fixture_test!(remove_index_1_add_two, |server, pool| {
    let id = make(&server, "Mix", &[1, 2, 3]).await;
    let res = call(
        &server,
        "updatePlaylist",
        &update(
            &id,
            vec![
                ("songIndexToRemove", "1".into()),
                ("songIdToAdd", tid(4)),
                ("songIdToAdd", tid(5)),
            ],
        ),
    )
    .await;
    ok(&res);
    let got = rows(&pool, &id).await;
    let tracks: Vec<Uuid> = got.iter().map(|r| r.0).collect();
    assert_eq!(tracks, [track_id(1), track_id(3), track_id(4), track_id(5)]);
    assert_dense(&got);
});

fixture_test!(four_concurrent_adds_keep_13, |server, pool| {
    let id = make(&server, "Big", &[1]).await;
    let batch = |from: u128| {
        update(
            &id,
            (from..from + 3).map(|n| ("songIdToAdd", tid(n))).collect(),
        )
    };
    let (b1, b2, b3, b4) = (batch(2), batch(5), batch(8), batch(11));
    // Four separate requests in flight at once, each with its own pool connection.
    let (r1, r2, r3, r4) = tokio::join!(
        call(&server, "updatePlaylist", &b1),
        call(&server, "updatePlaylist", &b2),
        call(&server, "updatePlaylist", &b3),
        call(&server, "updatePlaylist", &b4),
    );
    for r in [&r1, &r2, &r3, &r4] {
        ok(r);
    }
    let got = rows(&pool, &id).await;
    assert_eq!(got.len(), 13);
    let mut tracks: Vec<Uuid> = got.iter().map(|r| r.0).collect();
    tracks.sort();
    let mut want: Vec<Uuid> = (1..=13).map(track_id).collect();
    want.sort();
    assert_eq!(tracks, want);
    assert_dense(&got);
});

fixture_test!(index_skips_missing_track, |server, pool| {
    let id = make(&server, "Gappy", &[1, 2, 3]).await;
    // Slip a missing track (20) in between the first two; the client never sees it.
    sqlx::query(
        "UPDATE playlist_items SET sort = sort + 1 WHERE playlist_id = $1::uuid AND sort >= 1",
    )
    .bind(&id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO playlist_items (id, playlist_id, track_id, sort) VALUES ($1, $2::uuid, $3, 1)",
    )
    .bind(Uuid::now_v7())
    .bind(&id)
    .bind(track_id(20))
    .execute(&pool)
    .await
    .unwrap();
    let shown = call(&server, "getPlaylist", &[("id", id.clone())]).await;
    assert_eq!(entry_ids(&shown), [tid(1), tid(2), tid(3)]);

    // Index 1 is track 2, not the hidden track 20.
    ok(&call(
        &server,
        "updatePlaylist",
        &update(&id, vec![("songIndexToRemove", "1".into())]),
    )
    .await);
    let got = rows(&pool, &id).await;
    let tracks: Vec<Uuid> = got.iter().map(|r| r.0).collect();
    assert_eq!(tracks, [track_id(1), track_id(20), track_id(3)]);
    assert_dense(&got);
    // Out-of-range, repeated and non-numeric indexes are ignored.
    ok(&call(
        &server,
        "updatePlaylist",
        &update(
            &id,
            vec![
                ("songIndexToRemove", "7".into()),
                ("songIndexToRemove", "-1".into()),
                ("songIndexToRemove", "x".into()),
                ("songIndexToRemove", "0".into()),
                ("songIndexToRemove", "0".into()),
            ],
        ),
    )
    .await);
    let tracks: Vec<Uuid> = rows(&pool, &id).await.iter().map(|r| r.0).collect();
    assert_eq!(tracks, [track_id(20), track_id(3)]);
});

fixture_test!(
    get_playlists_own_and_public_minus_hidden_newest_first,
    |server, pool| {
        let res = call(&server, "getPlaylists", &[]).await;
        ok(&res);
        let list = res["playlists"]["playlist"].as_array().unwrap();
        let names: Vec<&str> = list.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Private Mix", "Road Trip"]);
        let road = &list[1];
        assert_eq!(road["id"], ROAD_TRIP.to_string());
        assert_eq!(road["songCount"], 3);
        assert_eq!(road["public"], true);
        assert_eq!(road["owner"], "alice");
        assert_eq!(road["created"], "2026-01-01T01:01:00.000Z");
        assert_eq!(road["changed"], "2026-01-01T01:01:00.000Z");
        let secs: i64 =
            sqlx::query_scalar("SELECT sum(duration_secs)::bigint FROM tracks WHERE id = ANY($1)")
                .bind([track_id(1), track_id(10), track_id(13)].as_slice())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(road["duration"], secs);
        assert_eq!(list[0]["public"], false);

        // The admin's playlist turns public: it shows up, newest first, under its owner.
        sqlx::query("UPDATE playlists SET is_public = true WHERE id = $1")
            .bind(ADMIN_PICKS)
            .execute(&pool)
            .await
            .unwrap();
        let res = call(&server, "getPlaylists", &[]).await;
        let list = res["playlists"]["playlist"].as_array().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0]["name"], "Admin Picks");
        assert_eq!(list[0]["owner"], "admin");
        // Alice hides it in the web app: gone from her list.
        kima_db::playlists::hide(&pool, ALICE_ID, ADMIN_PICKS)
            .await
            .unwrap();
        let res = call(&server, "getPlaylists", &[]).await;
        assert_eq!(res["playlists"]["playlist"].as_array().unwrap().len(), 2);
    }
);

fixture_test!(get_private_of_other_70, |server, pool| {
    failed(
        &call(&server, "getPlaylist", &[("id", ADMIN_PICKS.to_string())]).await,
        70,
    );
    failed(
        &call(&server, "getPlaylist", &[("id", "nonsense".into())]).await,
        70,
    );
    failed(
        &call(
            &server,
            "getPlaylist",
            &[("id", Uuid::now_v7().to_string())],
        )
        .await,
        70,
    );
    failed(&call(&server, "getPlaylist", &[]).await, 10);
    // Public, someone else's: readable. Own: entries in order.
    sqlx::query("UPDATE playlists SET is_public = true WHERE id = $1")
        .bind(ADMIN_PICKS)
        .execute(&pool)
        .await
        .unwrap();
    let res = call(&server, "getPlaylist", &[("id", ADMIN_PICKS.to_string())]).await;
    ok(&res);
    assert_eq!(entry_ids(&res), [tid(2)]);
    let res = call(&server, "getPlaylist", &[("id", ROAD_TRIP.to_string())]).await;
    assert_eq!(entry_ids(&res), [tid(1), tid(10), tid(13)]);
});

fixture_test!(create_reply_matches_get_playlist, |server, _pool| {
    for params in [
        vec![
            ("name", "Full".to_owned()),
            ("songId", tid(5)),
            ("songId", tid(1)),
        ],
        vec![("name", "Empty".to_owned())],
        vec![("playlistId", ROAD_TRIP.to_string()), ("songId", tid(4))],
    ] {
        let created = call(&server, "createPlaylist", &params).await;
        ok(&created);
        let id = created["playlist"]["id"].as_str().unwrap();
        let fetched = call(&server, "getPlaylist", &[("id", id.to_owned())]).await;
        ok(&fetched);
        assert_eq!(created["playlist"], fetched["playlist"]);
    }
});

fixture_test!(create_with_name_private_full_answer, |server, pool| {
    let res = call(
        &server,
        "createPlaylist",
        &[
            ("name", "New One".into()),
            ("songId", tid(5)),
            ("songId", tid(4)),
        ],
    )
    .await;
    ok(&res);
    let p = &res["playlist"];
    assert_eq!(p["name"], "New One");
    assert_eq!(p["public"], false);
    assert_eq!(p["owner"], "alice");
    assert_eq!(p["songCount"], 2);
    assert_eq!(entry_ids(&res), [tid(5), tid(4)]);
    assert!(p["entry"][0]["title"].is_string());
    let id: Uuid = p["id"].as_str().unwrap().parse().unwrap();
    let stored = kima_db::playlists::find(&pool, id).await.unwrap().unwrap();
    assert_eq!(stored.user_id, ALICE_ID);
    assert!(!stored.is_public);
    // An empty playlist has no entries; neither name nor playlistId is 10.
    let empty = call(&server, "createPlaylist", &[("name", "Empty".into())]).await;
    ok(&empty);
    assert_eq!(empty["playlist"]["songCount"], 0);
    assert!(empty["playlist"].get("entry").is_none());
    failed(&call(&server, "createPlaylist", &[]).await, 10);
});

fixture_test!(
    create_with_playlist_id_replaces_owner_only_50,
    |server, pool| {
        let res = call(
            &server,
            "createPlaylist",
            &[
                ("playlistId", ROAD_TRIP.to_string()),
                ("songId", tid(5)),
                ("songId", tid(4)),
            ],
        )
        .await;
        ok(&res);
        assert_eq!(res["playlist"]["id"], ROAD_TRIP.to_string());
        assert_eq!(res["playlist"]["name"], "Road Trip");
        assert_eq!(entry_ids(&res), [tid(5), tid(4)]);
        let got = rows(&pool, &ROAD_TRIP.to_string()).await;
        assert_dense(&got);
        assert_eq!(got.len(), 2);
        // No songs clears it.
        let res = call(
            &server,
            "createPlaylist",
            &[("playlistId", ROAD_TRIP.to_string())],
        )
        .await;
        assert_eq!(res["playlist"]["songCount"], 0);
        // Another user's playlist, private or not, is 50; an unknown one is 70.
        failed(
            &call(
                &server,
                "createPlaylist",
                &[("playlistId", ADMIN_PICKS.to_string()), ("songId", tid(1))],
            )
            .await,
            50,
        );
        failed(
            &call(
                &server,
                "createPlaylist",
                &[
                    ("playlistId", ADMIN_PICKS.to_string()),
                    ("songId", "nonsense".into()),
                ],
            )
            .await,
            50,
        );
        assert_eq!(rows(&pool, &ADMIN_PICKS.to_string()).await.len(), 1);
        let res = call_admin(
            &server,
            "createPlaylist",
            &[("playlistId", ROAD_TRIP.to_string()), ("songId", tid(1))],
        )
        .await;
        failed(&res, 50);
        failed(
            &call(
                &server,
                "createPlaylist",
                &[("playlistId", Uuid::now_v7().to_string())],
            )
            .await,
            70,
        );
    }
);

fixture_test!(
    create_rejects_unknown_invalid_and_missing_ids,
    |server, pool| {
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM playlists")
            .fetch_one(&pool)
            .await
            .unwrap();
        let res = call(
            &server,
            "createPlaylist",
            &[
                ("name", "Skippy".into()),
                ("songId", tid(3)),
                ("songId", Uuid::now_v7().to_string()),
                ("songId", tid(3)),
                ("songId", "nonsense".into()),
                ("songId", tid(20)),
                ("songId", tid(1)),
            ],
        )
        .await;
        failed(&res, 70);
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM playlists")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before, after);
    }
);

fixture_test!(
    update_unknown_song_rolls_back_metadata_and_items,
    |server, pool| {
        let before = rows(&pool, &PRIVATE_MIX.to_string()).await;
        let result = call(
            &server,
            "updatePlaylist",
            &update(
                &PRIVATE_MIX.to_string(),
                vec![
                    ("name", "Should Roll Back".into()),
                    ("public", "true".into()),
                    ("songIndexToRemove", "0".into()),
                    ("songIdToAdd", Uuid::now_v7().to_string()),
                ],
            ),
        )
        .await;
        failed(&result, 70);
        let playlist = kima_db::playlists::find(&pool, PRIVATE_MIX)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(playlist.name, "Private Mix");
        assert!(!playlist.is_public);
        assert_eq!(rows(&pool, &PRIVATE_MIX.to_string()).await, before);
    }
);

fixture_test!(concurrent_missing_song_rolls_back_update, |server, pool| {
    let before = rows(&pool, &PRIVATE_MIX.to_string()).await;
    // Pause the request after its presence check and before its second add check.
    sqlx::query("CREATE FUNCTION pause_playlist_delete() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(71317); RETURN OLD; END $$")
        .execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER pause_delete BEFORE DELETE ON playlist_items FOR EACH ROW EXECUTE FUNCTION pause_playlist_delete()")
        .execute(&pool).await.unwrap();
    let mut gate = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(71317)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let request = update(
        &PRIVATE_MIX.to_string(),
        vec![
            ("name", "Must Roll Back".into()),
            ("public", "true".into()),
            ("songIndexToRemove", "0".into()),
            ("songIdToAdd", tid(3)),
        ],
    );
    let (result, ()) = tokio::join!(call(&server, "updatePlaylist", &request), async {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE wait_event = 'advisory' AND query ILIKE '%DELETE FROM playlist_items%' AND pid <> pg_backend_pid())")
                    .fetch_one(&pool).await.unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await.expect("request reached delete after validation");
        sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
            .bind(track_id(3))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("SELECT pg_advisory_unlock(71317)")
            .execute(&mut *gate)
            .await
            .unwrap();
    });
    failed(&result, 70);
    let playlist = kima_db::playlists::find(&pool, PRIVATE_MIX)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(playlist.name, "Private Mix");
    assert!(!playlist.is_public);
    assert_eq!(rows(&pool, &PRIVATE_MIX.to_string()).await, before);
});

fixture_test!(
    missing_update_waits_for_playlist_insert_commit,
    |server, pool| {
        let id = make(&server, "Race gate", &[]).await;
        // BEFORE INSERT runs after add_locked's final presence check.
        sqlx::query("CREATE FUNCTION pause_playlist_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(71318); RETURN NEW; END $$")
        .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER pause_insert BEFORE INSERT ON playlist_items FOR EACH ROW EXECUTE FUNCTION pause_playlist_insert()")
        .execute(&pool).await.unwrap();
        let mut gate = pool.acquire().await.unwrap();
        sqlx::query("SELECT pg_advisory_lock(71318)")
            .execute(&mut *gate)
            .await
            .unwrap();
        let request = update(&id, vec![("songIdToAdd", tid(3))]);
        let (result, (blocked, update_task)) = tokio::join!(
            call(&server, "updatePlaylist", &request),
            async {
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    loop {
                        let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE wait_event = 'advisory' AND query ILIKE '%INSERT INTO playlist_items%' AND pid <> pg_backend_pid())")
                            .fetch_one(&pool).await.unwrap();
                        if waiting { break; }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                }).await.expect("request reached INSERT after final check");
                let update_pool = pool.clone();
                let update_task = tokio::spawn(async move {
                    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
                        .bind(track_id(3))
                        .execute(&update_pool)
                        .await
                        .unwrap()
                });
                // Observe either a real PostgreSQL row-lock wait or a completed update.
                let blocked = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    loop {
                        if update_task.is_finished() { break false; }
                        let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query ILIKE 'UPDATE tracks SET missing_since%' AND pid <> pg_backend_pid())")
                            .fetch_one(&pool).await.unwrap();
                        if waiting { break true; }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                }).await.expect("updater either waited or finished");
                sqlx::query("SELECT pg_advisory_unlock(71318)")
                    .execute(&mut *gate)
                    .await
                    .unwrap();
                (blocked, update_task)
            }
        );
        ok(&result);
        let updated = tokio::time::timeout(std::time::Duration::from_secs(10), update_task)
            .await
            .expect("missing update finished")
            .unwrap();
        assert_eq!(updated.rows_affected(), 1);
        assert!(
            blocked,
            "missing update passed final check before INSERT committed"
        );
        assert_eq!(rows(&pool, &id).await, vec![(track_id(3), 0)]);
    }
);

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn playlist_events_only_on_committed_changes(pool: PgPool) {
    let (server, state) = app_with_state(pool.clone());
    let mut rx = state.events.subscribe();
    let id = make(&server, "Events", &[1, 2]).await;
    let check =
        |rx: &mut tokio::sync::broadcast::Receiver<kima_core::events::Event>, id: &str, deleted| {
            let event = rx.try_recv().expect("one event");
            assert_eq!(event.user_id, Some(ALICE_ID));
            assert_eq!(
                event.data,
                serde_json::json!({"type":"playlist:updated", "playlistId":id, "deleted":deleted})
            );
            assert!(rx.try_recv().is_err(), "exactly one event");
        };
    check(&mut rx, &id, false);
    let before: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "SELECT id, track_id, sort FROM playlist_items WHERE playlist_id = $1::uuid ORDER BY sort, id",
    ).bind(&id).fetch_all(&pool).await.unwrap();
    let changed_before = kima_db::playlists::find(&pool, id.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .updated_at;
    ok(&call(
        &server,
        "createPlaylist",
        &[
            ("playlistId", id.clone()),
            ("songId", tid(1)),
            ("songId", tid(2)),
        ],
    )
    .await);
    ok(&call(
        &server,
        "updatePlaylist",
        &update(
            &id,
            vec![
                ("name", "Events".into()),
                ("public", "false".into()),
                ("songIdToAdd", tid(1)),
            ],
        ),
    )
    .await);
    assert!(rx.try_recv().is_err(), "no-op events");
    let after: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "SELECT id, track_id, sort FROM playlist_items WHERE playlist_id = $1::uuid ORDER BY sort, id",
    ).bind(&id).fetch_all(&pool).await.unwrap();
    assert_eq!(after, before, "identical replace preserves item identities");
    assert_eq!(
        kima_db::playlists::find(&pool, id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        changed_before
    );
    failed(
        &call(
            &server,
            "updatePlaylist",
            &update(
                &id,
                vec![("name", "No".into()), ("songIdToAdd", "invalid".into())],
            ),
        )
        .await,
        70,
    );
    assert!(rx.try_recv().is_err(), "rollback event");
    failed(
        &call(
            &server,
            "createPlaylist",
            &[
                ("playlistId", id.clone()),
                ("songId", tid(1)),
                ("songId", Uuid::now_v7().to_string()),
            ],
        )
        .await,
        70,
    );
    assert!(rx.try_recv().is_err(), "failed replace event");
    let after_failure: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "SELECT id, track_id, sort FROM playlist_items WHERE playlist_id = $1::uuid ORDER BY sort, id",
    ).bind(&id).fetch_all(&pool).await.unwrap();
    assert_eq!(after_failure, before, "failed replace leaves items intact");
    ok(&call(
        &server,
        "updatePlaylist",
        &update(&id, vec![("name", "New".into()), ("songIdToAdd", tid(3))]),
    )
    .await);
    check(&mut rx, &id, false);
    ok(&call(&server, "deletePlaylist", &[("id", id.clone())]).await);
    check(&mut rx, &id, true);
}

fixture_test!(update_name_public_owner_only_50, |server, pool| {
    let res = call(
        &server,
        "updatePlaylist",
        &update(
            &PRIVATE_MIX.to_string(),
            vec![("name", "Renamed".into()), ("public", "true".into())],
        ),
    )
    .await;
    ok(&res);
    let got = call(&server, "getPlaylist", &[("id", PRIVATE_MIX.to_string())]).await;
    assert_eq!(got["playlist"]["name"], "Renamed");
    assert_eq!(got["playlist"]["public"], true);
    // `comment` is ignored; the songs stay.
    ok(&call(
        &server,
        "updatePlaylist",
        &update(
            &PRIVATE_MIX.to_string(),
            vec![("comment", "hi".into()), ("public", "false".into())],
        ),
    )
    .await);
    let got = call(&server, "getPlaylist", &[("id", PRIVATE_MIX.to_string())]).await;
    assert_eq!(got["playlist"]["public"], false);
    assert_eq!(got["playlist"]["name"], "Renamed");
    assert_eq!(rows(&pool, &PRIVATE_MIX.to_string()).await.len(), 2);
    // Not the owner: 50, for private and public ones, and nothing changes.
    failed(
        &call(
            &server,
            "updatePlaylist",
            &update(&ADMIN_PICKS.to_string(), vec![("name", "Hijack".into())]),
        )
        .await,
        50,
    );
    failed(
        &call_admin(
            &server,
            "updatePlaylist",
            &update(&ROAD_TRIP.to_string(), vec![("name", "Hijack".into())]),
        )
        .await,
        50,
    );
    let name: String = sqlx::query_scalar("SELECT name FROM playlists WHERE id = $1")
        .bind(ROAD_TRIP)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Road Trip");
    failed(
        &call(
            &server,
            "updatePlaylist",
            &update(&Uuid::now_v7().to_string(), vec![]),
        )
        .await,
        70,
    );
    failed(&call(&server, "updatePlaylist", &[]).await, 10);
});

fixture_test!(delete_owner_only_50_unknown_70, |server, pool| {
    failed(
        &call(
            &server,
            "deletePlaylist",
            &[("id", ADMIN_PICKS.to_string())],
        )
        .await,
        50,
    );
    failed(
        &call(
            &server,
            "deletePlaylist",
            &[("id", Uuid::now_v7().to_string())],
        )
        .await,
        70,
    );
    failed(
        &call(&server, "deletePlaylist", &[("id", "x".into())]).await,
        70,
    );
    failed(&call(&server, "deletePlaylist", &[]).await, 10);
    ok(&call(&server, "deletePlaylist", &[("id", ROAD_TRIP.to_string())]).await);
    assert!(
        kima_db::playlists::find(&pool, ROAD_TRIP)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        kima_db::playlists::find(&pool, ADMIN_PICKS)
            .await
            .unwrap()
            .is_some()
    );
});

fixture_test!(update_removal_and_add_land_together, |server, pool| {
    let id = make(&server, "Atomic", &[1, 2, 3]).await;
    // Every insert into playlist_items now fails, so the add step breaks after the removal ran.
    sqlx::query(
        "CREATE FUNCTION refuse_insert() RETURNS trigger LANGUAGE plpgsql AS
         $$ BEGIN RAISE EXCEPTION 'no inserts'; END $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse BEFORE INSERT ON playlist_items
         FOR EACH ROW EXECUTE FUNCTION refuse_insert()",
    )
    .execute(&pool)
    .await
    .unwrap();
    let res = call(
        &server,
        "updatePlaylist",
        &update(
            &id,
            vec![
                ("name", "Trigger Must Roll Back".into()),
                ("songIndexToRemove", "0".into()),
                ("songIdToAdd", tid(4)),
            ],
        ),
    )
    .await;
    failed(&res, 0);
    // Nothing was removed: a retry would delete the same song again.
    let got = rows(&pool, &id).await;
    let tracks: Vec<Uuid> = got.iter().map(|r| r.0).collect();
    assert_eq!(tracks, [track_id(1), track_id(2), track_id(3)]);
    assert_dense(&got);
    assert_eq!(
        kima_db::playlists::find(&pool, id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .name,
        "Atomic"
    );
    sqlx::query("DROP TRIGGER refuse ON playlist_items")
        .execute(&pool)
        .await
        .unwrap();
    ok(&call(
        &server,
        "updatePlaylist",
        &update(
            &id,
            vec![
                ("name", "Trigger Must Roll Back".into()),
                ("songIndexToRemove", "0".into()),
                ("songIdToAdd", tid(4)),
            ],
        ),
    )
    .await);
    let tracks: Vec<Uuid> = rows(&pool, &id).await.iter().map(|r| r.0).collect();
    assert_eq!(tracks, [track_id(2), track_id(3), track_id(4)]);
});
