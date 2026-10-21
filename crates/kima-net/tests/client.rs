//! The client against the replay server: classification, rate-limit retries and persisted
//! pauses, the switch, the network guard, the User-Agent and redirects.

use std::collections::BTreeMap;
use std::time::Duration;

use kima_core::identify::IdentifySwitch;
use kima_db::http_cache;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Host, Net, NetConfig, Outcome, Req, Source, redirect_allowed};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;

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

fn redirect(url: &str, to: &str) -> Entry {
    let mut e = entry("GET", url, 302, Value::Null);
    e.headers.insert("location".into(), to.into());
    e
}

fn net(replay: &Replay, pool: &PgPool) -> Net {
    Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, true),
    )
}

#[derive(Debug, PartialEq)]
enum Kind {
    Answered,
    Absent,
    RateLimited,
    Transient(&'static str),
    Permanent(&'static str),
}

fn kind<T>(o: &Outcome<T>) -> Kind {
    match o {
        Outcome::Answered(_) => Kind::Answered,
        Outcome::Absent(_) => Kind::Absent,
        Outcome::RateLimited { .. } => Kind::RateLimited,
        Outcome::Transient(f) => Kind::Transient(leak(&f.code)),
        Outcome::Permanent(f) => Kind::Permanent(leak(&f.code)),
    }
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// Seconds from now until `until`.
fn secs_until(until: OffsetDateTime) -> f64 {
    (until - OffsetDateTime::now_utc()).as_seconds_f64()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn classify_statuses(pool: PgPool) {
    let big = Value::String("x".repeat(4 * 1024 * 1024 + 16));
    let mut not_json = entry("GET", "/not-json", 200, Value::Null);
    not_json.body_base64 = Some("bm90IGpzb24=".into()); // "not json"
    let replay = Replay::start(Recording(vec![
        entry("GET", "/ok", 200, json!({"id": 1})),
        entry("GET", "/gone", 404, json!({"error": "Not Found"})),
        entry("GET", "/bad", 400, json!({"error": "bad"})),
        entry("GET", "/slow", 408, Value::Null),
        entry("GET", "/limited", 429, Value::Null),
        entry("GET", "/unavailable", 503, Value::Null),
        entry("GET", "/says-limit", 500, json!("rate limit exceeded")),
        entry("GET", "/broken", 502, Value::Null),
        not_json,
        entry("GET", "/big", 200, big),
        entry("GET", "/empty", 200, json!({"results": []})),
    ]))
    .await;

    type Case = (Host, &'static str, fn(Req) -> Req, Kind);
    let cases: Vec<Case> = vec![
        (Host::MusicBrainz, "/ok", |r| r, Kind::Answered),
        (
            Host::MusicBrainz,
            "/gone",
            |r| r.absent_on_404(),
            Kind::Absent,
        ),
        (
            Host::MusicBrainz,
            "/gone",
            |r| r,
            Kind::Permanent("HTTP_404"),
        ),
        (Host::Deezer, "/bad", |r| r, Kind::Permanent("HTTP_400")),
        (Host::Deezer, "/slow", |r| r, Kind::Transient("HTTP_408")),
        (Host::Fanart, "/limited", |r| r, Kind::RateLimited),
        (Host::MusicBrainz, "/unavailable", |r| r, Kind::RateLimited),
        (
            Host::Deezer,
            "/unavailable",
            |r| r,
            Kind::Transient("HTTP_503"),
        ),
        (Host::Lrclib, "/says-limit", |r| r, Kind::RateLimited),
        (Host::Deezer, "/broken", |r| r, Kind::Transient("HTTP_502")),
        (
            Host::Deezer,
            "/not-json",
            |r| r,
            Kind::Transient("BAD_RESPONSE"),
        ),
        (
            Host::Deezer,
            "/big",
            |r| r,
            Kind::Transient("BODY_TOO_LARGE"),
        ),
        (
            Host::AcoustId,
            "/empty",
            |r| r.absent_when(|v| v["results"].as_array().is_some_and(|a| a.is_empty())),
            Kind::Absent,
        ),
        (
            Host::Wikidata,
            "/unknown",
            |r| r,
            Kind::Transient("HTTP_599"),
        ),
    ];
    for (host, path, build, expected) in cases {
        // A fresh client each time, so one case's pause cannot answer for the next; no cache,
        // so one case's cached answer cannot either (the cache has its own tests).
        let out = net(&replay, &pool)
            .get_json::<Value>(build(Req::get(host, path)).no_cache())
            .await;
        assert_eq!(kind(&out), expected, "{host:?} {path}: {out:?}");
    }

    // An answer that does not fit the caller's type is a bad response.
    #[derive(serde::Deserialize, Debug)]
    #[allow(dead_code)]
    struct Wanted {
        name: String,
    }
    let out = net(&replay, &pool)
        .get_json::<Wanted>(Req::get(Host::Deezer, "/ok"))
        .await;
    assert_eq!(kind(&out), Kind::Transient("BAD_RESPONSE"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn connect_failure_is_network(pool: PgPool) {
    // A port nobody listens on fails to connect: NETWORK.
    let cfg = NetConfig::for_tests(Bases::all("http://127.0.0.1:9"));
    let net = Net::new(cfg, pool.clone(), IdentifySwitch::new(false, true));
    let out = net.get_json::<Value>(Req::get(Host::Deezer, "/x")).await;
    assert_eq!(kind(&out), Kind::Transient("NETWORK"), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retries_exhausted_pause_persisted(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("GET", "/artist/x", 429, Value::Null)])).await;
    let net = net(&replay, &pool);
    let out = net
        .get_json::<Value>(Req::get(Host::MusicBrainz, "/artist/x"))
        .await;
    let Outcome::RateLimited { until } = out else {
        panic!("{out:?}")
    };
    assert_eq!(replay.asked().len(), 1 + 3, "{:?}", replay.asked());
    let wait = secs_until(until);
    assert!((28.0..=31.0).contains(&wait), "{wait}");

    let rows = http_cache::pauses(&pool).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "musicbrainz");
    assert_eq!(rows[0].2, 1);
    assert_eq!(net.paused_until(Source::MusicBrainz), Some(until));

    // Paused: refused at once, nothing asked.
    let out = net
        .get_json::<Value>(Req::get(Host::MusicBrainz, "/artist/x"))
        .await;
    assert!(matches!(out, Outcome::RateLimited { .. }), "{out:?}");
    assert_eq!(replay.asked().len(), 4);

    // A restart honours the pause.
    let restarted = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    restarted.load_pauses().await.unwrap();
    assert!(restarted.paused_until(Source::MusicBrainz).is_some());
    let out = restarted
        .get_json::<Value>(Req::get(Host::MusicBrainz, "/artist/x"))
        .await;
    assert!(matches!(out, Outcome::RateLimited { .. }), "{out:?}");
    assert_eq!(replay.asked().len(), 4);
    // Other sources are not paused.
    assert_eq!(restarted.paused_until(Source::Deezer), None);
}

async fn expire_pauses(pool: &PgPool) {
    sqlx::query("UPDATE source_pauses SET paused_until = now() - interval '1 second'")
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn pause_doubles_after_five_consecutive_capped_60s(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("GET", "/a", 429, Value::Null)])).await;
    let past = OffsetDateTime::now_utc() - time::Duration::minutes(1);
    for _ in 0..4 {
        http_cache::record_pause(&pool, "fanart", past, "seed")
            .await
            .unwrap();
    }
    let mut waits = Vec::new();
    for _ in 0..3 {
        let net = net(&replay, &pool);
        net.load_pauses().await.unwrap();
        let out = net.get_json::<Value>(Req::get(Host::Fanart, "/a")).await;
        let Outcome::RateLimited { until } = out else {
            panic!("{out:?}")
        };
        waits.push(secs_until(until));
        expire_pauses(&pool).await;
    }
    // The fifth consecutive: 30 s; the sixth doubles to 60 s; the seventh stays capped at 60 s.
    assert!((28.0..=31.0).contains(&waits[0]), "{waits:?}");
    assert!((58.0..=61.0).contains(&waits[1]), "{waits:?}");
    assert!((58.0..=61.0).contains(&waits[2]), "{waits:?}");
    let rows = http_cache::pauses(&pool).await.unwrap();
    assert_eq!(rows[0].2, 7);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answer_clears_pause_row(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("GET", "/a", 200, json!({}))])).await;
    let past = OffsetDateTime::now_utc() - time::Duration::minutes(1);
    http_cache::record_pause(&pool, "deezer", past, "HTTP 429")
        .await
        .unwrap();
    http_cache::record_pause(&pool, "fanart", past, "HTTP 429")
        .await
        .unwrap();
    let net = net(&replay, &pool);
    net.load_pauses().await.unwrap();
    let out = net.get_json::<Value>(Req::get(Host::Deezer, "/a")).await;
    assert_eq!(kind(&out), Kind::Answered);
    let rows = http_cache::pauses(&pool).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["fanart"]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_off_never_touches_network(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("GET", "/a", 200, json!({}))])).await;
    let switch = IdentifySwitch::new(false, false);
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        switch.clone(),
    );
    let hosts = [
        Host::MusicBrainz,
        Host::AcoustId,
        Host::WikidataSparql,
        Host::Wikidata,
        Host::Wikipedia,
        Host::Fanart,
        Host::Deezer,
        Host::ListenBrainz,
        Host::Lrclib,
        Host::CoverArtArchive,
    ];
    for host in hosts {
        let out = net.get_json::<Value>(Req::get(host, "/a")).await;
        assert_eq!(kind(&out), Kind::Transient("DISABLED"), "{host:?}");
        let out = net
            .post_form_json::<Value>(Req::post_form(host, "/a", "x=1".into()))
            .await;
        assert_eq!(kind(&out), Kind::Transient("DISABLED"), "{host:?}");
    }
    assert!(replay.asked().is_empty());

    // The env lock wins over the setting.
    let locked = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(true, true),
    );
    let out = locked.get_json::<Value>(Req::get(Host::Deezer, "/a")).await;
    assert_eq!(kind(&out), Kind::Transient("DISABLED"));

    let disabled = Net::disabled();
    let out = disabled
        .get_json::<Value>(Req::get(Host::Deezer, "/a"))
        .await;
    assert_eq!(kind(&out), Kind::Transient("DISABLED"));
    assert!(replay.asked().is_empty());

    // Switched back on, the same client asks.
    switch.set_setting(true);
    let out = net.get_json::<Value>(Req::get(Host::Deezer, "/a")).await;
    assert_eq!(kind(&out), Kind::Answered);
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
#[should_panic(expected = "kima-net refused a real host")]
async fn deny_real_panics_on_real_host(pool: PgPool) {
    let net = Net::new(
        NetConfig::for_tests(Bases::production("https://musicbrainz.org/ws/2")),
        pool,
        IdentifySwitch::new(false, true),
    );
    let _ = net
        .get_json::<Value>(Req::get(Host::MusicBrainz, "/artist/x"))
        .await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_agent_sent_on_every_request(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        entry("GET", "/a", 200, json!({})),
        redirect("/r", "/a"),
        entry("POST", "/p", 200, json!({})),
    ]))
    .await;
    let cfg = NetConfig::for_tests(Bases::all(&replay.url()));
    let ua = cfg.user_agent.clone();
    assert_eq!(
        ua,
        format!(
            "kima-rs/{} ( https://github.com/d1mmu-dev/kima-rs-hub )",
            env!("CARGO_PKG_VERSION")
        )
    );
    let net = Net::new(cfg, pool.clone(), IdentifySwitch::new(false, true));
    let get = net.get_json::<Value>(Req::get(Host::Deezer, "/a")).await;
    let hop = net.get_json::<Value>(Req::get(Host::Lrclib, "/r")).await;
    let post = net
        .post_form_json::<Value>(Req::post_form(Host::AcoustId, "/p", "a=1".into()))
        .await;
    assert_eq!(
        (kind(&get), kind(&hop), kind(&post)),
        (Kind::Answered, Kind::Answered, Kind::Answered)
    );
    let agents = replay.user_agents();
    assert_eq!(agents.len(), 4, "{:?}", replay.asked());
    assert!(agents.iter().all(|a| *a == ua), "{agents:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redirects_capped_at_three(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        redirect("/a0", "/a1"),
        redirect("/a1", "/a2"),
        redirect("/a2", "/a3"),
        entry("GET", "/a3", 200, json!({"hops": 3})),
        redirect("/b0", "/b1"),
        redirect("/b1", "/b2"),
        redirect("/b2", "/b3"),
        redirect("/b3", "/b4"),
        entry("GET", "/b4", 200, json!({"hops": 4})),
    ]))
    .await;
    let net = net(&replay, &pool);
    let out = net.get_json::<Value>(Req::get(Host::Deezer, "/a0")).await;
    assert_eq!(out, Outcome::Answered(json!({"hops": 3})));
    let out = net.get_json::<Value>(Req::get(Host::Deezer, "/b0")).await;
    assert_eq!(kind(&out), Kind::Permanent("TOO_MANY_REDIRECTS"), "{out:?}");
    assert!(!replay.asked().iter().any(|a| a.contains("/b4")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn https_to_http_redirect_refused(pool: PgPool) {
    assert!(!redirect_allowed(
        "https://a.example/x",
        "http://a.example/y"
    ));
    assert!(redirect_allowed(
        "http://a.example/x",
        "https://b.example/y"
    ));
    assert!(redirect_allowed(
        "https://a.example/x",
        "https://b.example/y"
    ));
    assert!(redirect_allowed("http://a.example/x", "http://b.example/y"));
    assert!(!redirect_allowed("http://a.example/x", "ftp://a.example/y"));
    assert!(!redirect_allowed(
        "https://a.example/x",
        "file:///etc/passwd"
    ));

    // A redirect to another scheme is refused without following it.
    let replay = Replay::start(Recording(vec![redirect("/r", "ftp://127.0.0.1/x")])).await;
    let out = net(&replay, &pool)
        .get_json::<Value>(Req::get(Host::CoverArtArchive, "/r"))
        .await;
    assert_eq!(kind(&out), Kind::Permanent("REDIRECT_REFUSED"), "{out:?}");
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn query_encoded_and_matched(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry(
        "GET",
        "/search?query=a%20b%2Bc&fmt=json",
        200,
        json!({"hit": true}),
    )]))
    .await;
    let out = net(&replay, &pool)
        .get_json::<Value>(
            Req::get(Host::MusicBrainz, "/search")
                .query("fmt", "json")
                .query("query", "a b+c"),
        )
        .await;
    assert_eq!(out, Outcome::Answered(json!({"hit": true})));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn budget_blown_is_timeout(pool: PgPool) {
    // A spent budget stops the call before an answer can arrive.
    let replay = Replay::start(Recording(vec![entry("GET", "/a", 200, json!({}))])).await;
    let net = net(&replay, &pool);
    let out = net
        .get_json::<Value>(Req::get(Host::Deezer, "/a").budget(Duration::ZERO))
        .await;
    assert_eq!(kind(&out), Kind::Transient("TIMEOUT"), "{out:?}");
}
