//! M6 Task 3: podcast feeds fetched from any public host (`rss-parser.ts:88-118`), every hop
//! checked and pinned like an image download.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use base64::Engine;
use kima_core::identify::IdentifySwitch;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Conditional, FeedFetch, Net, NetConfig, Outcome};
use serde_json::Value;
use sqlx::PgPool;

const XML: &str =
    "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>T</title></channel></rss>";
const BUDGET: Duration = Duration::from_secs(30);

fn feed(url: &str, status: u16, body: &str, headers: &[(&str, &str)]) -> Entry {
    let mut h: BTreeMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    h.entry("content-type".into())
        .or_insert_with(|| "application/rss+xml".into());
    Entry {
        method: "GET".into(),
        url: url.into(),
        status,
        headers: h,
        body: Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(body)),
        request_body_sha256: None,
    }
}

fn redirect(from: &str, to: &str) -> Entry {
    feed(from, 302, "", &[("location", to)])
}

fn net_with(replay: &Replay, pool: &PgPool, f: impl FnOnce(&mut NetConfig)) -> Net {
    let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
    f(&mut cfg);
    // The identification switch is off: feeds are user-facing and not stopped by it.
    Net::new(cfg, pool.clone(), IdentifySwitch::new(false, false))
}

fn net(replay: &Replay, pool: &PgPool) -> Net {
    net_with(replay, pool, |_| {})
}

fn none() -> Conditional<'static> {
    Conditional {
        etag: None,
        last_modified: None,
    }
}

fn addr(replay: &Replay) -> SocketAddr {
    replay
        .url()
        .trim_start_matches("http://")
        .parse()
        .expect("replay address")
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sends_upstream_headers_and_conditionals(pool: PgPool) {
    let replay = Replay::start(Recording(vec![feed(
        "/feed",
        200,
        XML,
        &[
            ("etag", "\"v2\""),
            ("last-modified", "Tue, 10 Jun 2026 04:00:00 GMT"),
        ],
    )]))
    .await;
    let net = net(&replay, &pool);
    let url = format!("{}/feed", replay.url());
    let out = net.get_feed(&url, none(), BUDGET).await;
    assert_eq!(
        out,
        Outcome::Answered(FeedFetch::Body {
            text: XML.into(),
            etag: Some("\"v2\"".into()),
            last_modified: Some("Tue, 10 Jun 2026 04:00:00 GMT".into()),
        })
    );
    let agent = replay.header(0, "user-agent").unwrap();
    assert!(
        agent.starts_with("Kima/") && agent.ends_with("; podcast aggregator)"),
        "{agent}"
    );
    assert_eq!(
        replay.header(0, "accept").as_deref(),
        Some("application/rss+xml, application/xml, text/xml, */*")
    );
    assert_eq!(replay.header(0, "if-none-match"), None);
    assert_eq!(replay.header(0, "if-modified-since"), None);

    // A refresh names what it has; only what it has.
    net.get_feed(
        &url,
        Conditional {
            etag: Some("\"v1\""),
            last_modified: None,
        },
        BUDGET,
    )
    .await;
    assert_eq!(replay.header(1, "if-none-match").as_deref(), Some("\"v1\""));
    assert_eq!(replay.header(1, "if-modified-since"), None);
    net.get_feed(
        &url,
        Conditional {
            etag: None,
            last_modified: Some("Mon, 09 Jun 2026 04:00:00 GMT"),
        },
        BUDGET,
    )
    .await;
    assert_eq!(replay.header(2, "if-none-match"), None);
    assert_eq!(
        replay.header(2, "if-modified-since").as_deref(),
        Some("Mon, 09 Jun 2026 04:00:00 GMT")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn not_modified_to_conditional_is_not_modified(pool: PgPool) {
    let replay = Replay::start(Recording(vec![feed("/feed", 304, "", &[])])).await;
    let net = net(&replay, &pool);
    let out = net
        .get_feed(
            &format!("{}/feed", replay.url()),
            Conditional {
                etag: Some("\"v1\""),
                last_modified: None,
            },
            BUDGET,
        )
        .await;
    assert_eq!(out, Outcome::Answered(FeedFetch::NotModified));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn not_modified_to_unconditional_is_an_error(pool: PgPool) {
    let replay = Replay::start(Recording(vec![feed("/feed", 304, "", &[])])).await;
    let net = net(&replay, &pool);
    let out = net
        .get_feed(&format!("{}/feed", replay.url()), none(), BUDGET)
        .await;
    // `rss-parser.ts:114-118`: a host breaking the protocol; a reply problem, not an absence.
    assert_eq!(out.code(), Some("BAD_RESPONSE"), "{out:?}");
    assert!(matches!(out, Outcome::Transient(_)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn follows_five_redirects_not_six(pool: PgPool) {
    let mut entries: Vec<Entry> = (0..6)
        .map(|i| redirect(&format!("/r{i}"), &format!("/r{}", i + 1)))
        .collect();
    entries.push(feed("/r6", 200, XML, &[]));
    let replay = Replay::start(Recording(entries)).await;
    let net = net(&replay, &pool);
    // Five hops from /r1 to /r6.
    let out = net
        .get_feed(&format!("{}/r1", replay.url()), none(), BUDGET)
        .await;
    assert!(
        matches!(out, Outcome::Answered(FeedFetch::Body { .. })),
        "{out:?}"
    );
    // Six from /r0.
    let out = net
        .get_feed(&format!("{}/r0", replay.url()), none(), BUDGET)
        .await;
    assert_eq!(out.code(), Some("TOO_MANY_REDIRECTS"), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redirect_to_private_address_is_absent(pool: PgPool) {
    let replay = Replay::start(Recording(vec![redirect("/feed", "http://10.0.0.1/feed")])).await;
    let allowed = addr(&replay);
    let net = net_with(&replay, &pool, |c| {
        c.allow_private_images = false;
        c.private_image_allow = vec![allowed];
    });
    let out = net
        .get_feed(&format!("{}/feed", replay.url()), none(), BUDGET)
        .await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    // Asked once: the private hop was never fetched.
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn host_answers_map_to_outcomes(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        feed("/gone", 404, "", &[]),
        feed("/forbidden", 403, "", &[]),
        feed("/slow-down", 429, "", &[]),
        feed("/broken", 503, "", &[]),
    ]))
    .await;
    let net = net(&replay, &pool);
    assert!(matches!(
        get(&net, &replay, "/gone").await,
        Outcome::Absent(_)
    ));
    assert!(matches!(
        get(&net, &replay, "/forbidden").await,
        Outcome::Absent(_)
    ));
    let limited = get(&net, &replay, "/slow-down").await;
    assert_eq!(limited.code(), Some("HTTP_429"), "{limited:?}");
    assert!(matches!(limited, Outcome::Transient(_)));
    let broken = get(&net, &replay, "/broken").await;
    assert_eq!(broken.code(), Some("HTTP_503"));
    assert!(matches!(broken, Outcome::Transient(_)));
    // A feed host's 429 pauses no source: the next feed is asked.
    assert!(matches!(
        get(&net, &replay, "/gone").await,
        Outcome::Absent(_)
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn body_over_cap_is_permanent(pool: PgPool) {
    let big = "x".repeat(32 * 1024 * 1024 + 1);
    let replay = Replay::start(Recording(vec![feed("/big", 200, &big, &[])])).await;
    let net = net(&replay, &pool);
    let out = net
        .get_feed(&format!("{}/big", replay.url()), none(), BUDGET)
        .await;
    assert_eq!(out.code(), Some("TOO_LARGE"), "{out:?}");
    assert!(matches!(out, Outcome::Permanent(_)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn host_resolving_to_private_is_absent(pool: PgPool) {
    let replay = Replay::start(Recording(vec![feed("/feed", 200, XML, &[])])).await;
    let net = net_with(&replay, &pool, |c| {
        c.allow_private_images = false;
        c.static_hosts = HashMap::from([(
            "feeds.example".to_string(),
            vec!["192.168.1.5".parse::<IpAddr>().unwrap()],
        )]);
    });
    let out = net
        .get_feed("http://feeds.example/feed", none(), BUDGET)
        .await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
#[should_panic(expected = "kima-net refused a real host: https://feeds.example.com/feed")]
async fn real_host_panics_under_guard(pool: PgPool) {
    let net = Net::new(
        NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
        pool,
        IdentifySwitch::new(false, true),
    );
    let _ = net
        .get_feed("https://feeds.example.com/feed?key=secret", none(), BUDGET)
        .await;
}

async fn get(net: &Net, replay: &Replay, path: &str) -> Outcome<FeedFetch> {
    net.get_feed(&format!("{}{path}", replay.url()), none(), BUDGET)
        .await
}
