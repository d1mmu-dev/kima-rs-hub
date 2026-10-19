//! The shared play queue on the SQL fixture (spec §4.5 #35-#38, §6.4).

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_db::PgPool;
use serde_json::{Value, json};

fn app(pool: PgPool) -> TestServer {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(fixture_state(pool)).layer(MockConnectInfo(peer));
    TestServer::new(router).unwrap()
}

/// A call as alice with repeated `id`s.
async fn call(
    server: &TestServer,
    endpoint: &str,
    ids: &[String],
    params: &[(&str, &str)],
) -> Value {
    let ids: Vec<(&str, &str)> = ids.iter().map(|i| ("id", i.as_str())).collect();
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN)])
        .add_query_params(ids)
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

fn ids(ns: &[u128]) -> Vec<String> {
    ns.iter().map(|&n| track_id(n).to_string()).collect()
}

fn entry_ids(res: &Value, key: &str) -> Vec<String> {
    res[key]["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

fn failed(res: &Value, code: u64) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
}

/// The fixture gives alice a saved queue; tests that need none remove it.
async fn no_state(pool: &PgPool) {
    kima_db::playback::delete(pool, ALICE_ID).await.unwrap();
}

async fn web_state(server_pool: &PgPool) -> kima_db::playback::PlaybackState {
    kima_db::playback::get(server_pool, ALICE_ID)
        .await
        .unwrap()
        .unwrap()
}

fixture_test!(save_and_get_queue_round_trip, |server, pool| {
    let queue = ids(&[3, 1, 2]);
    let res = call(
        &server,
        "savePlayQueue",
        &queue,
        &[
            ("current", &queue[1]),
            ("position", "4321"),
            ("c", "Symfonium"),
        ],
    )
    .await;
    assert_eq!(res["status"], "ok", "{res}");

    // The web player's view of the same row.
    let state = web_state(&pool).await;
    assert_eq!(state.playback_type, "track");
    assert_eq!(state.track_id.as_deref(), Some(queue[1].as_str()));
    assert_eq!((state.current_index, state.position), (1, 4321));
    assert_eq!(state.changed_by.as_deref(), Some("Symfonium"));
    assert!(!state.is_shuffle);
    let stored = state.queue.unwrap();
    assert_eq!(
        stored[0],
        json!({
            "id": queue[0],
            "title": "Maxwell's Silver Hammer",
            "duration": 207,
            "artist": { "id": artist_id(1), "name": "The Beatles" },
            "album": { "id": album_id(1), "title": "Abbey Road", "coverArt": album_id(1).to_string() },
        })
    );
    let stored_ids: Vec<&str> = stored
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        stored_ids,
        queue.iter().map(String::as_str).collect::<Vec<_>>()
    );

    let got = call(&server, "getPlayQueue", &[], &[]).await;
    assert_eq!(entry_ids(&got, "playQueue"), queue);
    let pq = &got["playQueue"];
    assert_eq!(pq["current"], queue[1]);
    assert_eq!(pq["position"], 4321);
    assert_eq!(pq["username"], "alice");
    assert_eq!(pq["changedBy"], "Symfonium");
    assert!(pq["changed"].as_str().unwrap().ends_with('Z'));
    assert_eq!(pq["entry"][0]["title"], "Maxwell's Silver Hammer");

    let by_index = call(&server, "getPlayQueueByIndex", &[], &[]).await;
    assert_eq!(entry_ids(&by_index, "playQueueByIndex"), queue);
    assert_eq!(by_index["playQueueByIndex"]["currentIndex"], 1);
    assert_eq!(by_index["playQueueByIndex"]["position"], 4321);
});

fixture_test!(save_queue_post_body_repeated_ids, |server, pool| {
    no_state(&pool).await;
    let q = ids(&[1, 2, 3]);
    let body = format!("id={}&id={}&id={}&current={}", q[0], q[1], q[2], q[1]);
    let res = server
        .post("/savePlayQueue.view")
        .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN), ("c", "Feishin")])
        .text(body)
        .content_type("application/x-www-form-urlencoded")
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["subsonic-response"]["status"], "ok");
    let state = web_state(&pool).await;
    assert_eq!(state.current_index, 1);
    let stored: Vec<String> = state
        .queue
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(stored, q);
    assert_eq!(state.changed_by.as_deref(), Some("Feishin"));
});

fixture_test!(save_queue_current_not_in_ids_10, |server, pool| {
    no_state(&pool).await;
    let q = ids(&[1, 2]);
    failed(
        &call(
            &server,
            "savePlayQueue",
            &q,
            &[("current", &track_id(3).to_string())],
        )
        .await,
        10,
    );
    failed(&call(&server, "savePlayQueue", &q, &[]).await, 10);
    assert!(
        kima_db::playback::get(&pool, ALICE_ID)
            .await
            .unwrap()
            .is_none()
    );
});

fixture_test!(save_queue_no_ids_clears, |server, pool| {
    let q = ids(&[1, 2]);
    call(&server, "savePlayQueue", &q, &[("current", &q[0])]).await;
    let res = call(&server, "savePlayQueue", &[], &[]).await;
    assert_eq!(res["status"], "ok", "{res}");
    let state = web_state(&pool).await;
    assert!(state.queue.is_none());
    assert!(state.track_id.is_none());
    assert_eq!((state.current_index, state.position), (0, 0));
    let got = call(&server, "getPlayQueue", &[], &[]).await;
    assert_eq!(got["playQueue"]["entry"], json!([]));
    assert!(got["playQueue"].get("current").is_none());
});

fixture_test!(by_index_rules, |server, pool| {
    no_state(&pool).await;
    let q = ids(&[1, 2, 3]);
    // Required with ids, and within 0..len-1.
    failed(&call(&server, "savePlayQueueByIndex", &q, &[]).await, 10);
    failed(
        &call(
            &server,
            "savePlayQueueByIndex",
            &q,
            &[("currentIndex", "3")],
        )
        .await,
        10,
    );
    failed(
        &call(
            &server,
            "savePlayQueueByIndex",
            &q,
            &[("currentIndex", "-1")],
        )
        .await,
        10,
    );
    failed(
        &call(
            &server,
            "savePlayQueueByIndex",
            &q,
            &[("currentIndex", "x")],
        )
        .await,
        10,
    );
    // Forbidden without ids.
    failed(
        &call(
            &server,
            "savePlayQueueByIndex",
            &[],
            &[("currentIndex", "0")],
        )
        .await,
        10,
    );
    assert!(
        kima_db::playback::get(&pool, ALICE_ID)
            .await
            .unwrap()
            .is_none()
    );

    let ok = call(
        &server,
        "savePlayQueueByIndex",
        &q,
        &[("currentIndex", "2"), ("position", "99")],
    )
    .await;
    assert_eq!(ok["status"], "ok", "{ok}");
    let state = web_state(&pool).await;
    assert_eq!((state.current_index, state.position), (2, 99));
    assert_eq!(state.track_id.as_deref(), Some(q[2].as_str()));
    // No ids and no index clears.
    assert_eq!(
        call(&server, "savePlayQueueByIndex", &[], &[]).await["status"],
        "ok"
    );
    assert!(web_state(&pool).await.queue.is_none());
});

fixture_test!(get_queue_rebuilds_from_rows, |server, pool| {
    let q = ids(&[1, 2, 3]);
    call(&server, "savePlayQueue", &q, &[("current", &q[2])]).await;
    sqlx::query("UPDATE tracks SET title = 'Renamed' WHERE id = $1")
        .bind(track_id(2))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(track_id(1))
        .execute(&pool)
        .await
        .unwrap();
    let got = call(&server, "getPlayQueue", &[], &[]).await;
    assert_eq!(entry_ids(&got, "playQueue"), ids(&[2, 3]));
    assert_eq!(got["playQueue"]["entry"][0]["title"], "Renamed");
    // The current track keeps its place among the survivors.
    assert_eq!(got["playQueue"]["current"], q[2]);
    let by_index = call(&server, "getPlayQueueByIndex", &[], &[]).await;
    assert_eq!(by_index["playQueueByIndex"]["currentIndex"], 1);
});

fixture_test!(get_queue_empty_row_defaults, |server, pool| {
    no_state(&pool).await;
    let got = call(&server, "getPlayQueue", &[], &[]).await;
    let pq = &got["playQueue"];
    assert_eq!(pq["entry"], json!([]), "{got}");
    assert_eq!(pq["changedBy"], "Kima");
    assert_eq!(pq["username"], "alice");
    assert_eq!(pq["position"], 0);
    let changed = pq["changed"].as_str().unwrap();
    let at = time::OffsetDateTime::parse(changed, &time::format_description::well_known::Rfc3339)
        .unwrap();
    assert!((time::OffsetDateTime::now_utc() - at).whole_seconds().abs() < 30);
    let by_index = call(&server, "getPlayQueueByIndex", &[], &[]).await;
    assert_eq!(by_index["playQueueByIndex"]["currentIndex"], 0);
    assert_eq!(by_index["playQueueByIndex"]["changedBy"], "Kima");
    // Without `c`, a save is attributed to `Subsonic`; a long `c` is cut to 64.
    let q = ids(&[1]);
    call(&server, "savePlayQueue", &q, &[("current", &q[0])]).await;
    assert_eq!(
        web_state(&pool).await.changed_by.as_deref(),
        Some("Subsonic")
    );
    let long = "x".repeat(100);
    call(
        &server,
        "savePlayQueue",
        &q,
        &[("current", &q[0]), ("c", &long)],
    )
    .await;
    assert_eq!(web_state(&pool).await.changed_by.unwrap().len(), 64);
});
