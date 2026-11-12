//! M6 Task 2: the Apple (iTunes) source: upstream's limits, per-request cache lifetimes, and
//! user-facing requests that the identification switch does not stop.

use std::collections::BTreeMap;
use std::time::Duration;

use kima_core::identify::IdentifySwitch;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Host, Net, NetConfig, Outcome, Req};
use serde_json::{Value, json};
use sqlx::PgPool;

fn entry(url: &str, status: u16, body: Value) -> Entry {
    Entry {
        method: "GET".into(),
        url: url.into(),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

fn apple_net(replay: &Replay, pool: &PgPool, switch: IdentifySwitch) -> Net {
    Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        switch,
    )
}

async fn ttls(pool: &PgPool) -> Vec<(String, Duration)> {
    let rows: Vec<(String, String, f64)> = sqlx::query_as(
        "SELECT source, request, EXTRACT(EPOCH FROM (expires_at - fetched_at))::float8
         FROM http_cache ORDER BY request",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(source, request, secs)| {
            assert_eq!(source, "apple");
            (request, Duration::from_secs_f64(secs.round()))
        })
        .collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn req_ttl_overrides_source_ttl(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        entry(
            "/lookup?id=1&entity=podcast",
            200,
            json!({"resultCount": 1, "results": []}),
        ),
        entry(
            "/us/rss/toppodcasts/limit=200/json",
            200,
            json!({"feed": {}}),
        ),
        entry("/plain", 200, json!({"x": 1})),
    ]))
    .await;
    let net = apple_net(&replay, &pool, IdentifySwitch::new(false, true));
    let week = Duration::from_secs(7 * 86_400);
    let six_hours = Duration::from_secs(6 * 3600);
    let lookup = || {
        Req::get(Host::Itunes, "/lookup")
            .query("id", "1")
            .query("entity", "podcast")
            .ttl(week, week)
    };
    let chart =
        || Req::get(Host::Itunes, "/us/rss/toppodcasts/limit=200/json").ttl(six_hours, six_hours);
    for _ in 0..2 {
        assert!(matches!(
            net.get_json::<Value>(lookup()).await,
            Outcome::Answered(_)
        ));
        assert!(matches!(
            net.get_json::<Value>(chart()).await,
            Outcome::Answered(_)
        ));
    }
    // Without an override the source's own lifetimes apply (6 h answered).
    assert!(matches!(
        net.get_json::<Value>(Req::get(Host::Itunes, "/plain"))
            .await,
        Outcome::Answered(_)
    ));
    // One request each: the second round came from the cache.
    assert_eq!(replay.asked().len(), 3, "{:?}", replay.asked());
    let rows = ttls(&pool).await;
    let base = replay.url();
    assert_eq!(
        rows,
        vec![
            (format!("GET {base}/lookup?entity=podcast&id=1"), week),
            (format!("GET {base}/plain"), six_hours),
            (
                format!("GET {base}/us/rss/toppodcasts/limit=200/json"),
                six_hours
            ),
        ]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_facing_ignores_identify_switch_but_not_guard(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry(
        "/search?term=x",
        200,
        json!({"results": []}),
    )]))
    .await;
    // The identification switch is off (the admin setting).
    let net = apple_net(&replay, &pool, IdentifySwitch::new(false, false));
    let req = || {
        Req::get(Host::Itunes, "/search")
            .query("term", "x")
            .no_cache()
    };
    // An enrichment-style request is stopped by the switch, without a request.
    assert_eq!(net.get_json::<Value>(req()).await.code(), Some("DISABLED"));
    assert!(replay.asked().is_empty());
    // A user-facing one is not.
    assert_eq!(
        net.get_json::<Value>(req().user_facing()).await,
        Outcome::Answered(json!({"results": []}))
    );
    assert_eq!(replay.asked(), vec!["GET /search?term=x".to_string()]);
    // A disabled client stays disabled.
    assert_eq!(
        Net::disabled()
            .get_json::<Value>(req().user_facing())
            .await
            .code(),
        Some("DISABLED")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
#[should_panic(expected = "kima-net refused a real host: https://itunes.apple.com/search")]
async fn user_facing_still_panics_on_a_real_host_under_the_guard(pool: PgPool) {
    let net = Net::new(
        NetConfig::for_tests(Bases::production("https://musicbrainz.org/ws/2")),
        pool,
        IdentifySwitch::new(false, false),
    );
    let _ = net
        .get_json::<Value>(
            Req::get(Host::Itunes, "/search")
                .query("term", "x")
                .user_facing(),
        )
        .await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apple_rate_limit_retries_twice(pool: PgPool) {
    // Two 429s then an answer: answered after two retries.
    let replay = Replay::start(Recording(vec![
        entry("/a", 429, json!({})),
        entry("/a", 429, json!({})),
        entry("/a", 200, json!({"ok": true})),
    ]))
    .await;
    let net = apple_net(&replay, &pool, IdentifySwitch::new(false, true));
    assert_eq!(
        net.get_json::<Value>(Req::get(Host::Itunes, "/a").no_cache().user_facing())
            .await,
        Outcome::Answered(json!({"ok": true}))
    );
    assert_eq!(replay.asked().len(), 3);

    // Three 429s: the two retries are spent and Apple is paused.
    let replay = Replay::start(Recording(vec![entry("/b", 429, json!({}))])).await;
    let net = apple_net(&replay, &pool, IdentifySwitch::new(false, true));
    let out = net
        .get_json::<Value>(Req::get(Host::Itunes, "/b").no_cache().user_facing())
        .await;
    assert!(matches!(out, Outcome::RateLimited { .. }), "{out:?}");
    assert_eq!(replay.asked().len(), 3);
    assert!(net.paused_until(kima_net::Source::Apple).is_some());
}

fn has_feed(v: &Value) -> bool {
    v.get("feed").is_some_and(|f| !f.is_null())
}

/// M6 Task 5: a reply that is not what was asked for (upstream `AppleReplyError`) fails and is
/// never cached; the next good reply is used.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn valid_when_false_is_bad_response_and_not_cached(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        entry("/chart", 200, json!({"errorMessage": "busy"})),
        entry("/chart", 200, json!({"feed": {}})),
    ]))
    .await;
    let net = apple_net(&replay, &pool, IdentifySwitch::new(false, true));
    let req = || {
        Req::get(Host::Itunes, "/chart")
            .valid_when(has_feed)
            .user_facing()
    };
    let first = net.get_json::<Value>(req()).await;
    assert_eq!(first.code(), Some("BAD_RESPONSE"), "{first:?}");
    assert!(matches!(first, Outcome::Transient(_)));
    assert_eq!(
        net.get_json::<Value>(req()).await,
        Outcome::Answered(json!({"feed": {}}))
    );
    // The good reply is cached now.
    net.get_json::<Value>(req()).await;
    assert_eq!(replay.asked().len(), 2);
}

/// M6 Task 5: Apple search's `skipRetry` (`podcastCatalog.ts:194-207`).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_retry_does_not_retry_a_rate_limit(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry("/search", 429, json!({}))])).await;
    let net = apple_net(&replay, &pool, IdentifySwitch::new(false, true));
    let out = net
        .get_json::<Value>(
            Req::get(Host::Itunes, "/search")
                .no_cache()
                .no_retry()
                .user_facing(),
        )
        .await;
    assert!(matches!(out, Outcome::RateLimited { .. }), "{out:?}");
    assert_eq!(replay.asked().len(), 1);
}
