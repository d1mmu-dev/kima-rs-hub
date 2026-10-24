//! The response cache (spec §7.4, §24.3): hits cost no cell and no request, TTLs per source,
//! failures never cached, secrets kept out of keys, rows and messages.

use std::collections::BTreeMap;
use std::time::Duration;

use kima_core::identify::IdentifySwitch;
use kima_db::http_cache::{self, CacheOutcome};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Host, Net, NetConfig, Outcome, Req};
use serde_json::{Value, json};
use sqlx::PgPool;

fn entry(method: &str, url: &str, status: u16, body: Value) -> Entry {
    Entry {
        method: method.into(),
        url: url.into(),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

fn net_with(replay: &Replay, pool: &PgPool, f: impl FnOnce(&mut NetConfig)) -> Net {
    let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
    f(&mut cfg);
    Net::new(cfg, pool.clone(), IdentifySwitch::new(false, true))
}

fn net(replay: &Replay, pool: &PgPool) -> Net {
    net_with(replay, pool, |_| {})
}

struct Row {
    key: String,
    source: String,
    request: String,
    outcome: String,
    body: Value,
    ttl: Duration,
}

async fn rows(pool: &PgPool) -> Vec<Row> {
    let rows: Vec<(String, String, String, String, Value, f64)> = sqlx::query_as(
        "SELECT key, source, request, outcome, body,
                EXTRACT(EPOCH FROM (expires_at - fetched_at))::float8
         FROM http_cache ORDER BY key",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(key, source, request, outcome, body, secs)| Row {
            key,
            source,
            request,
            outcome,
            body,
            ttl: Duration::from_secs_f64(secs),
        })
        .collect()
}

const DAY: Duration = Duration::from_secs(86_400);

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cache_hit_costs_no_cell_and_no_request(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry(
        "GET",
        "/recording/r?fmt=json",
        200,
        json!({"id": "r"}),
    )]))
    .await;
    // Spacing scaled up: a second MusicBrainz cell is 110 s away, so a call that needed one
    // with no time to wait would be BUSY.
    let net = net_with(&replay, &pool, |c| c.wait_scale = 100.0);
    let req = || Req::get(Host::MusicBrainz, "/recording/r").query("fmt", "json");
    let first = net.get_json::<Value>(req()).await;
    assert_eq!(first, Outcome::Answered(json!({"id": "r"})));
    let second = net.get_json::<Value>(req().max_wait(Duration::ZERO)).await;
    assert_eq!(second, Outcome::Answered(json!({"id": "r"})));
    assert_eq!(replay.asked().len(), 1, "{:?}", replay.asked());

    // Without the cache the same call needs a cell, and there is none to be had at once.
    let third = net
        .get_json::<Value>(req().no_cache().max_wait(Duration::ZERO))
        .await;
    assert_eq!(third.code(), Some("BUSY"), "{third:?}");

    let rows = rows(&pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "musicbrainz");
    assert_eq!(rows[0].outcome, "answered");
    assert_eq!(rows[0].ttl, 30 * DAY);
    assert!(rows[0].key.starts_with("musicbrainz:"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn absent_cached_with_absent_ttl(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry(
        "GET",
        "/isrc/X",
        404,
        json!({"error": "Not Found"}),
    )]))
    .await;
    let net = net(&replay, &pool);
    for _ in 0..2 {
        let out = net
            .get_json::<Value>(Req::get(Host::MusicBrainz, "/isrc/X").absent_on_404())
            .await;
        let Outcome::Absent(evidence) = out else {
            panic!("{out:?}")
        };
        assert_eq!(evidence.0["status"], 404);
    }
    assert_eq!(replay.asked().len(), 1);
    let rows = rows(&pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, "absent");
    assert_eq!(rows[0].ttl, 7 * DAY);
    assert_eq!(rows[0].body["status"], 404);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn absent_when_marks_empty_search_absent(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry(
        "GET",
        "/recording?query=nothing",
        200,
        json!({"count": 0, "recordings": []}),
    )]))
    .await;
    let net = net(&replay, &pool);
    let req = || {
        Req::get(Host::MusicBrainz, "/recording")
            .query("query", "nothing")
            .absent_when(|v| v["recordings"].as_array().is_some_and(|a| a.is_empty()))
    };
    for _ in 0..2 {
        let out = net.get_json::<Value>(req()).await;
        assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    }
    assert_eq!(replay.asked().len(), 1);
    let rows = rows(&pool).await;
    assert_eq!(rows[0].outcome, "absent");
    assert_eq!(rows[0].ttl, 7 * DAY);
    assert_eq!(rows[0].body["count"], 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failures_never_cached(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        entry("GET", "/x", 500, Value::Null),
        entry("GET", "/x", 200, json!({"ok": true})),
    ]))
    .await;
    let net = net(&replay, &pool);
    let first = net.get_json::<Value>(Req::get(Host::Deezer, "/x")).await;
    assert_eq!(first.code(), Some("HTTP_500"));
    assert!(rows(&pool).await.is_empty());
    let second = net.get_json::<Value>(Req::get(Host::Deezer, "/x")).await;
    assert_eq!(second, Outcome::Answered(json!({"ok": true})));
    assert_eq!(replay.asked().len(), 2);

    // An answer that does not fit the caller's type is a failure too: not cached.
    let replay = Replay::start(Recording(vec![entry("GET", "/y", 200, json!([1]))])).await;
    let net = net_with(&replay, &pool, |_| {});
    let out = net
        .get_json::<std::collections::HashMap<String, i32>>(Req::get(Host::Deezer, "/y"))
        .await;
    assert_eq!(out.code(), Some("BAD_RESPONSE"));
    assert_eq!(rows(&pool).await.len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn secrets_stripped_from_key_and_request(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        entry(
            "GET",
            "/music/m?api_key=FIRSTKEY111",
            200,
            json!({"name": "m"}),
        ),
        entry(
            "GET",
            "/music/m?api_key=SECONDKEY22",
            200,
            json!({"name": "other"}),
        ),
    ]))
    .await;
    let net = net(&replay, &pool);
    let a = net
        .get_json::<Value>(Req::get(Host::Fanart, "/music/m").query("api_key", "FIRSTKEY111"))
        .await;
    let b = net
        .get_json::<Value>(Req::get(Host::Fanart, "/music/m").query("api_key", "SECONDKEY22"))
        .await;
    assert_eq!(a, Outcome::Answered(json!({"name": "m"})));
    assert_eq!(b, a, "the second key is answered from the first one's row");
    assert_eq!(replay.asked().len(), 1);
    let rows = rows(&pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ttl, 7 * DAY);
    for secret in ["FIRSTKEY111", "SECONDKEY22", "api_key"] {
        assert!(!rows[0].request.contains(secret), "{}", rows[0].request);
        assert!(!rows[0].key.contains(secret));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn post_body_in_key_without_client(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry(
        "POST",
        "/lookup",
        200,
        json!({"status": "ok", "results": [{"id": "a"}]}),
    )]))
    .await;
    let net = net(&replay, &pool);
    let post = |body: &str| Req::post_form(Host::AcoustId, "/lookup", body.to_string());
    let one = net
        .post_form_json::<Value>(post("client=KEYONE&duration=200&fingerprint=AQAA"))
        .await;
    let two = net
        .post_form_json::<Value>(post("client=KEYTWO&duration=200&fingerprint=AQAA"))
        .await;
    assert!(matches!(one, Outcome::Answered(_)));
    assert_eq!(one, two);
    assert_eq!(replay.asked().len(), 1);
    let three = net
        .post_form_json::<Value>(post("client=KEYONE&duration=200&fingerprint=AQAB"))
        .await;
    assert!(matches!(three, Outcome::Answered(_)));
    assert_eq!(replay.asked().len(), 2);
    let rows = rows(&pool).await;
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.source, "acoustid");
        assert_eq!(row.ttl, 30 * DAY);
        assert!(!row.request.contains("KEY"), "{}", row.request);
        assert!(!row.request.contains("client"), "{}", row.request);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_cache_skips_cache(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("GET", "/a", 200, json!({}))])).await;
    let net = net(&replay, &pool);
    for _ in 0..2 {
        let out = net
            .get_json::<Value>(Req::get(Host::MusicBrainz, "/a").no_cache())
            .await;
        assert_eq!(out, Outcome::Answered(json!({})));
    }
    assert_eq!(replay.asked().len(), 2);
    assert!(rows(&pool).await.is_empty());

    // A row written by a cached call is not read by a `no_cache` one either.
    let _ = net
        .get_json::<Value>(Req::get(Host::MusicBrainz, "/a"))
        .await;
    let _ = net
        .get_json::<Value>(Req::get(Host::MusicBrainz, "/a").no_cache())
        .await;
    assert_eq!(replay.asked().len(), 4);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_ttl_24h(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry(
        "GET",
        "/search/artist?q=x",
        200,
        json!({"data": [{"name": "x"}]}),
    )]))
    .await;
    let out = net(&replay, &pool)
        .get_json::<Value>(Req::get(Host::Deezer, "/search/artist").query("q", "x"))
        .await;
    assert!(matches!(out, Outcome::Answered(_)));
    let rows = rows(&pool).await;
    assert_eq!(rows[0].source, "deezer");
    assert_eq!(rows[0].ttl, DAY);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listenbrainz_empty_list_absent_one_day(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("GET", "/similar", 200, json!([]))])).await;
    let out = net(&replay, &pool)
        .get_json::<Value>(
            Req::get(Host::ListenBrainz, "/similar")
                .absent_when(|v| v.as_array().is_some_and(|a| a.is_empty())),
        )
        .await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    let rows = rows(&pool).await;
    assert_eq!(rows[0].source, "listenbrainz");
    assert_eq!(rows[0].outcome, "absent");
    assert_eq!(rows[0].ttl, DAY);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lrclib_never_cached(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("GET", "/get", 200, json!({}))])).await;
    let net = net(&replay, &pool);
    for _ in 0..2 {
        let _ = net.get_json::<Value>(Req::get(Host::Lrclib, "/get")).await;
    }
    assert_eq!(replay.asked().len(), 2);
    assert!(rows(&pool).await.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_off_answers_disabled_even_when_cached(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("GET", "/a", 200, json!({}))])).await;
    let switch = IdentifySwitch::new(false, true);
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        switch.clone(),
    );
    let out = net.get_json::<Value>(Req::get(Host::Deezer, "/a")).await;
    assert!(matches!(out, Outcome::Answered(_)));
    switch.set_setting(false);
    let out = net.get_json::<Value>(Req::get(Host::Deezer, "/a")).await;
    assert_eq!(out.code(), Some("DISABLED"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cache_hit_served_while_paused(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        entry("GET", "/a", 200, json!({"a": 1})),
        entry("GET", "/b", 429, Value::Null),
    ]))
    .await;
    let net = net(&replay, &pool);
    assert!(matches!(
        net.get_json::<Value>(Req::get(Host::Fanart, "/a")).await,
        Outcome::Answered(_)
    ));
    assert!(matches!(
        net.get_json::<Value>(Req::get(Host::Fanart, "/b")).await,
        Outcome::RateLimited { .. }
    ));
    let asked = replay.asked().len();
    assert_eq!(
        net.get_json::<Value>(Req::get(Host::Fanart, "/a")).await,
        Outcome::Answered(json!({"a": 1}))
    );
    assert_eq!(replay.asked().len(), asked);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prune_deletes_expired_only(pool: PgPool) {
    let body = json!({"x": 1});
    for key in ["deezer:old", "deezer:new"] {
        http_cache::put(
            &pool,
            key,
            "deezer",
            "GET x",
            CacheOutcome::Answered,
            &body,
            DAY,
        )
        .await
        .unwrap();
    }
    sqlx::query(
        "UPDATE http_cache SET expires_at = now() - interval '1 second' WHERE key = 'deezer:old'",
    )
    .execute(&pool)
    .await
    .unwrap();
    // An expired row is not served.
    assert_eq!(http_cache::get(&pool, "deezer:old").await.unwrap(), None);
    assert_eq!(
        http_cache::get(&pool, "deezer:new").await.unwrap(),
        Some((CacheOutcome::Answered, body.clone()))
    );
    assert_eq!(http_cache::prune_expired(&pool).await.unwrap(), 1);
    let keys: Vec<String> = rows(&pool).await.into_iter().map(|r| r.key).collect();
    assert_eq!(keys, ["deezer:new"]);
    assert_eq!(http_cache::prune_expired(&pool).await.unwrap(), 0);

    // A put on an existing key replaces the row.
    http_cache::put(
        &pool,
        "deezer:new",
        "deezer",
        "GET x",
        CacheOutcome::Absent,
        &json!({"status": 404}),
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    let rows = rows(&pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, "absent");
    assert_eq!(rows[0].ttl, Duration::from_secs(60));
}

/// Final review M3: the message is scrubbed before it is cut to 200 characters, so a key
/// straddling the cut leaves no prefix behind.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_key_straddling_the_message_cut_leaves_no_prefix(pool: PgPool) {
    const KEY: &str = "STRADDLINGSECRET9";
    // `{"error":"` is 10 characters: the key starts at character 192 and runs past 200.
    let body = format!("{}{KEY} trailing", "x".repeat(182));
    let replay = Replay::start(Recording(vec![entry(
        "GET",
        &format!("/music/m?api_key={KEY}"),
        401,
        json!({ "error": body }),
    )]))
    .await;
    let net = net(&replay, &pool);
    let out = net
        .get_json::<Value>(Req::get(Host::Fanart, "/music/m").query("api_key", KEY))
        .await;
    let Outcome::Permanent(f) = out else {
        panic!("{out:?}")
    };
    for n in 4..=KEY.len() {
        assert!(
            !f.message.contains(&KEY[..n]),
            "{}: {}",
            &KEY[..n],
            f.message
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn secrets_scrubbed_from_failure_messages(pool: PgPool) {
    // A provider that echoes the key back in its error body.
    let replay = Replay::start(Recording(vec![
        entry(
            "GET",
            "/music/m?api_key=ECHOEDKEY99",
            401,
            json!({"error": "Invalid API key: ECHOEDKEY99 (sent api_key=ECHOEDKEY99)"}),
        ),
        entry(
            "POST",
            "/lookup",
            400,
            json!({"error": "invalid client key S3CR%2FT"}),
        ),
    ]))
    .await;
    let net = net(&replay, &pool);
    let out = net
        .get_json::<Value>(Req::get(Host::Fanart, "/music/m").query("api_key", "ECHOEDKEY99"))
        .await;
    let Outcome::Permanent(f) = out else {
        panic!("{out:?}")
    };
    assert_eq!(f.code, "HTTP_401");
    assert!(!f.message.contains("ECHOEDKEY99"), "{}", f.message);
    assert!(f.message.contains("REDACTED"), "{}", f.message);

    // A form value is scrubbed both decoded and as sent.
    let out = net
        .post_form_json::<Value>(Req::post_form(
            Host::AcoustId,
            "/lookup",
            "client=S3CR%2FT&duration=1".into(),
        ))
        .await;
    let Outcome::Permanent(f) = out else {
        panic!("{out:?}")
    };
    assert!(!f.message.contains("S3CR"), "{}", f.message);

    // A base URL that does not parse: the message names no query.
    let bad = Net::new(
        NetConfig::for_tests(Bases::all("http://[::1")),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    let out = bad
        .get_json::<Value>(Req::get(Host::Fanart, "/music/m").query("api_key", "URLKEY777"))
        .await;
    let Outcome::Permanent(f) = out else {
        panic!("{out:?}")
    };
    assert_eq!(f.code, "BAD_URL");
    assert!(!f.message.contains("URLKEY777"), "{}", f.message);
    assert!(!f.message.contains('?'), "{}", f.message);
}

/// Review fix (Task 20): a key a service echoes in an answer or a 404 body, raw or
/// percent-encoded, in a value or a field name, never reaches the cache, `Evidence` or an
/// outcome's `Debug`; the rest of the answer still reads.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn echoed_secret_is_scrubbed_from_answers_absences_and_rows(pool: PgPool) {
    let secret = "s3cret/echo+KEY";
    let encoded = "s3cret%2Fecho%2BKEY";
    let echo = json!({
        "note": format!("key {secret} or {encoded}"),
        "nested": [{"url": format!("https://x/a?api_key={encoded}")}],
        secret: "as a field name",
        "kept": "artistthumb",
    });
    let replay = Replay::start(Recording(vec![
        entry(
            "GET",
            &format!("/music/a?api_key={encoded}"),
            200,
            echo.clone(),
        ),
        entry("GET", &format!("/music/b?api_key={encoded}"), 404, echo),
    ]))
    .await;
    let net = net(&replay, &pool);
    let req = |p: &str| {
        Req::get(Host::Fanart, p)
            .query("api_key", secret)
            .absent_on_404()
    };

    let answered: Outcome<Value> = net.get_json(req("/music/a")).await;
    let absent: Outcome<Value> = net.get_json(req("/music/b")).await;
    match &answered {
        Outcome::Answered(v) => {
            assert_eq!(v["kept"], "artistthumb");
            assert_eq!(v["note"], "key REDACTED or REDACTED");
            assert_eq!(v["REDACTED"], "as a field name");
        }
        other => panic!("expected an answer, got {other:?}"),
    }
    match &absent {
        Outcome::Absent(e) => assert_eq!(e.0["body"]["kept"], "artistthumb"),
        other => panic!("expected an absence, got {other:?}"),
    }
    // A second read comes from the cache, scrubbed too.
    let again: Outcome<Value> = net.get_json(req("/music/a")).await;
    assert_eq!(replay.asked().len(), 2);
    for out in [&answered, &absent, &again] {
        let shown = format!("{out:?} {out:#?}");
        assert!(
            !shown.contains(secret) && !shown.contains(encoded),
            "{shown}"
        );
    }
    let rows = rows(&pool).await;
    assert_eq!(rows.len(), 2);
    for r in &rows {
        for text in [&r.key, &r.request, &r.body.to_string()] {
            assert!(!text.contains(secret) && !text.contains(encoded), "{text}");
        }
    }
}

/// The same scrub covers a form-body secret (AcoustID's `client`): `post_form_json` shares the path.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn echoed_form_secret_is_scrubbed(pool: PgPool) {
    let secret = "client/echo+KEY";
    let body = "client=client%2Fecho%2BKEY&duration=200&fingerprint=AQ".to_string();
    let replay = Replay::start(Recording(vec![entry(
        "POST",
        "/lookup",
        200,
        json!({"status": "ok", "results": [], "echo": format!("{secret} client%2Fecho%2BKEY")}),
    )]))
    .await;
    let net = net(&replay, &pool);
    let out: Outcome<Value> = net
        .post_form_json(Req::post_form(Host::AcoustId, "/lookup", body))
        .await;
    let shown = format!("{out:?}");
    assert!(shown.contains("REDACTED REDACTED"), "{shown}");
    assert!(
        !shown.contains("echo+KEY") && !shown.contains("%2Fecho"),
        "{shown}"
    );
    for r in rows(&pool).await {
        let body = r.body.to_string();
        assert!(
            !body.contains("echo+KEY") && !body.contains("%2Fecho"),
            "{body}"
        );
    }
}
