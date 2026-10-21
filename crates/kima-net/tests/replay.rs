//! The replay server itself: unknown requests, matching, and upstream's recorded fixture.

use std::collections::BTreeMap;

use kima_core::identify::IdentifySwitch;
use kima_net::testing::{Entry, Recording, Replay, body_sha256};
use kima_net::{Bases, Host, Net, NetConfig, Outcome, Req};
use serde_json::{Value, json};
use sqlx::PgPool;

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lidarr-pipeline.json")
}

#[tokio::test]
async fn replay_unknown_request_599() {
    let replay = Replay::start(Recording(vec![])).await;
    let res = reqwest::Client::new()
        .get(format!("{}/nope?b=2&a=1", replay.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 599);
    let text = res.text().await.unwrap();
    assert!(text.contains("GET /nope"), "{text}");
    assert_eq!(replay.asked(), ["GET /nope?b=2&a=1"]);
}

#[tokio::test]
async fn replay_matches_sorted_decoded_query_and_post_body() {
    let mut post = Entry {
        method: "POST".into(),
        url: "/v2/lookup".into(),
        status: 200,
        headers: BTreeMap::new(),
        body: json!({"status": "ok"}),
        body_base64: None,
        request_body_sha256: Some(body_sha256("client=k&duration=10")),
    };
    post.headers.insert("x-test".into(), "1".into());
    let replay = Replay::start(Recording(vec![
        Entry {
            method: "GET".into(),
            url: "http://example.invalid/s?b=x%20y&a=1".into(),
            status: 200,
            headers: BTreeMap::new(),
            body: json!([1]),
            body_base64: None,
            request_body_sha256: None,
        },
        post,
    ]))
    .await;
    let client = reqwest::Client::new();
    let res = client
        .get(format!("{}/s?a=%31&b=x%20y", replay.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
    assert_eq!(res.json::<Value>().await.unwrap(), json!([1]));

    let res = client
        .post(format!("{}/v2/lookup", replay.url()))
        .body("client=k&duration=10")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
    assert_eq!(res.headers()["x-test"], "1");
    let res = client
        .post(format!("{}/v2/lookup", replay.url()))
        .body("client=k&duration=11")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 599);
}

#[tokio::test]
async fn replay_repeats_in_order_then_holds_the_last() {
    let at = |status| Entry {
        method: "GET".into(),
        url: "/x".into(),
        status,
        headers: BTreeMap::new(),
        body: Value::Null,
        body_base64: None,
        request_body_sha256: None,
    };
    let replay = Replay::start(Recording(vec![at(429), at(200)])).await;
    let client = reqwest::Client::new();
    let mut statuses = Vec::new();
    for _ in 0..3 {
        let res = client
            .get(format!("{}/x", replay.url()))
            .send()
            .await
            .unwrap();
        statuses.push(res.status().as_u16());
    }
    assert_eq!(statuses, [429, 200, 200]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replay_serves_upstream_recording(pool: PgPool) {
    let recording = Recording::load(fixture());
    assert_eq!(recording.0.len(), 5);
    assert!(recording.0.iter().all(|e| e.method == "GET"));
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    let key = "/release-group/d0484284-1ee7-4157-951a-50f003cbcfb4?inc=artist-credits%2Breleases&fmt=json";
    let recorded = raw["replies"][key]["body"].clone();
    assert!(recorded.is_object());

    let replay = Replay::start(recording).await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool,
        IdentifySwitch::new(false, true),
    );
    let out = net
        .get_json::<Value>(
            Req::get(
                Host::MusicBrainz,
                "/release-group/d0484284-1ee7-4157-951a-50f003cbcfb4",
            )
            .query("inc", "artist-credits+releases")
            .query("fmt", "json"),
        )
        .await;
    assert_eq!(out, Outcome::Answered(recorded));
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn record_collects_raw_answers_with_secrets_redacted(pool: PgPool) {
    let mut redirect = Entry {
        method: "GET".into(),
        url: "/r?api_key=RECKEY1234".into(),
        status: 302,
        headers: BTreeMap::new(),
        body: Value::Null,
        body_base64: None,
        request_body_sha256: None,
    };
    redirect
        .headers
        .insert("location".into(), "/music/m".into());
    let replay = Replay::start(Recording(vec![
        redirect,
        Entry {
            method: "GET".into(),
            url: "/music/m".into(),
            status: 200,
            headers: BTreeMap::new(),
            body: json!({"echo": "key RECKEY1234"}),
            body_base64: None,
            request_body_sha256: None,
        },
        Entry {
            method: "POST".into(),
            url: "/lookup".into(),
            status: 500,
            headers: BTreeMap::new(),
            body: Value::Null,
            body_base64: Some("PGh0bWw+".into()), // "<html>"
            request_body_sha256: None,
        },
    ]))
    .await;
    let record = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
    cfg.record = Some(record.clone());
    let net = Net::new(cfg, pool, IdentifySwitch::new(false, true));
    let out = net
        .get_json::<Value>(
            Req::get(Host::Fanart, "/r")
                .query("api_key", "RECKEY1234")
                .no_cache(),
        )
        .await;
    assert!(matches!(out, Outcome::Answered(_)), "{out:?}");
    let _ = net
        .post_form_json::<Value>(Req::post_form(
            Host::AcoustId,
            "/lookup",
            "client=FORMKEY99&duration=7".into(),
        ))
        .await;

    let entries = record.lock().unwrap().clone();
    assert_eq!(entries.len(), 3, "{entries:?}");
    let text = serde_json::to_string(&Recording(entries.clone())).unwrap();
    assert!(!text.contains("RECKEY1234"), "{text}");
    assert!(!text.contains("FORMKEY99"), "{text}");
    assert_eq!(entries[0].status, 302);
    assert!(
        entries[0].url.ends_with("/r?api_key=REDACTED"),
        "{}",
        entries[0].url
    );
    assert_eq!(entries[0].headers["location"], "/music/m");
    assert_eq!(entries[1].body, json!({"echo": "key REDACTED"}));
    assert_eq!(entries[2].method, "POST");
    assert_eq!(entries[2].body_base64.as_deref(), Some("PGh0bWw+"));
    assert_eq!(
        entries[2].request_body_sha256.as_deref(),
        Some(body_sha256("client=REDACTED&duration=7").as_str())
    );
    // What was recorded replays.
    let again = Replay::start(Recording(entries)).await;
    let res = reqwest::Client::new()
        .get(format!("{}/music/m", again.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
}
