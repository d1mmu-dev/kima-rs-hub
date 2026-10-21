//! Image downloads (spec §12.6): the private-address rule on every hop, pinned connections,
//! content checks and size limits.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};

use base64::Engine;
use kima_core::identify::IdentifySwitch;
use kima_net::ssrf::is_public;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig, Outcome};
use serde_json::Value;
use sqlx::PgPool;

fn image(url: &str, content_type: &str, len: usize) -> Entry {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".into(), content_type.into());
    Entry {
        method: "GET".into(),
        url: url.into(),
        status: 200,
        headers,
        body: Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(vec![7u8; len])),
        request_body_sha256: None,
    }
}

fn status(url: &str, code: u16) -> Entry {
    Entry {
        method: "GET".into(),
        url: url.into(),
        status: code,
        headers: BTreeMap::new(),
        body: Value::Null,
        body_base64: None,
        request_body_sha256: None,
    }
}

fn redirect(url: &str, to: &str) -> Entry {
    let mut e = status(url, 302);
    e.headers.insert("location".into(), to.into());
    e
}

fn addr(replay: &Replay) -> SocketAddr {
    replay
        .url()
        .trim_start_matches("http://")
        .parse()
        .expect("replay address")
}

fn net_with(replay: &Replay, pool: &PgPool, f: impl FnOnce(&mut NetConfig)) -> Net {
    let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
    f(&mut cfg);
    Net::new(cfg, pool.clone(), IdentifySwitch::new(false, true))
}

/// The private-address rule on: only the replay server's own socket is let through, and the
/// test host names resolve without DNS.
fn strict(replay: &Replay, pool: &PgPool, hosts: &[(&str, &[&str])]) -> Net {
    let allowed = addr(replay);
    net_with(replay, pool, |c| {
        c.allow_private_images = false;
        c.private_image_allow = vec![allowed];
        c.static_hosts = hosts
            .iter()
            .map(|(h, ips)| {
                (
                    h.to_string(),
                    ips.iter().map(|ip| ip.parse::<IpAddr>().unwrap()).collect(),
                )
            })
            .collect::<HashMap<_, _>>();
    })
}

fn code<T: std::fmt::Debug>(o: &Outcome<T>) -> String {
    match o {
        Outcome::Answered(_) => "ANSWERED".into(),
        Outcome::Absent(_) => "ABSENT".into(),
        Outcome::RateLimited { .. } => "RATE_LIMITED".into(),
        Outcome::Transient(f) => format!("transient {}", f.code),
        Outcome::Permanent(f) => format!("permanent {}", f.code),
    }
}

#[test]
fn is_public_table() {
    let private = [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.1.1",
        "100.64.0.1",
        "224.0.0.1",
        "::1",
        "fc00::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        // Beyond the brief's table: the other non-public forms.
        "0.0.0.0",
        "255.255.255.255",
        "169.254.169.254",
        "100.127.255.254",
        "::",
        "fd12:3456::1",
        "ff02::1",
        "::ffff:10.0.0.1",
        "::ffff:169.254.169.254",
        "64:ff9b::7f00:1",
        "2002:7f00:1::1",
        "2001:db8::1",
    ];
    for ip in private {
        assert!(!is_public(ip.parse().unwrap()), "{ip} must not be public");
    }
    for ip in [
        "1.1.1.1",
        "2606:4700::1",
        "8.8.8.8",
        "100.128.0.1",
        "::ffff:1.1.1.1",
        "172.32.0.1",
    ] {
        assert!(is_public(ip.parse().unwrap()), "{ip} must be public");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_bytes_answers_an_image(pool: PgPool) {
    let replay = Replay::start(Recording(vec![image("/a.jpg", "image/jpeg", 1000)])).await;
    let net = net_with(&replay, &pool, |_| {});
    let out = net.get_bytes(&format!("{}/a.jpg", replay.url())).await;
    assert_eq!(out, Outcome::Answered(vec![7u8; 1000]));
    // Images are never cached.
    let _ = net.get_bytes(&format!("{}/a.jpg", replay.url())).await;
    assert_eq!(replay.asked().len(), 2);
    let ua = NetConfig::for_tests(Bases::all("http://x")).user_agent;
    assert!(replay.user_agents().iter().all(|a| *a == ua));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_bytes_refuses_loopback_without_flag(pool: PgPool) {
    let replay = Replay::start(Recording(vec![image("/a.jpg", "image/jpeg", 2000)])).await;
    let net = net_with(&replay, &pool, |c| c.allow_private_images = false);
    let out = net.get_bytes(&format!("{}/a.jpg", replay.url())).await;
    assert_eq!(code(&out), "permanent NOT_PUBLIC", "{out:?}");
    let out = net.get_bytes("http://[::1]:9/a.jpg").await;
    assert_eq!(code(&out), "permanent NOT_PUBLIC", "{out:?}");
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn non_image_content_type_refused(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        image("/page", "text/html", 5000),
        status("/none", 200),
    ]))
    .await;
    let net = net_with(&replay, &pool, |_| {});
    let out = net.get_bytes(&format!("{}/page", replay.url())).await;
    assert_eq!(code(&out), "permanent NOT_AN_IMAGE");
    // The replay answers JSON here: still not an image.
    let out = net.get_bytes(&format!("{}/none", replay.url())).await;
    assert_eq!(code(&out), "permanent NOT_AN_IMAGE");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn too_small_refused(pool: PgPool) {
    let replay = Replay::start(Recording(vec![image("/tiny.png", "image/png", 999)])).await;
    let out = net_with(&replay, &pool, |_| {})
        .get_bytes(&format!("{}/tiny.png", replay.url()))
        .await;
    assert_eq!(code(&out), "permanent TOO_SMALL");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn too_large_refused(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        image("/big.png", "image/png", 15 * 1024 * 1024 + 1),
        image("/max.png", "image/png", 15 * 1024 * 1024),
    ]))
    .await;
    let net = net_with(&replay, &pool, |_| {});
    let out = net.get_bytes(&format!("{}/big.png", replay.url())).await;
    assert_eq!(code(&out), "permanent TOO_LARGE");
    let out = net.get_bytes(&format!("{}/max.png", replay.url())).await;
    assert_eq!(code(&out), "ANSWERED");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn not_found_is_absent(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        status("/gone.jpg", 404),
        status("/err", 503),
    ]))
    .await;
    let net = net_with(&replay, &pool, |_| {});
    let out = net.get_bytes(&format!("{}/gone.jpg", replay.url())).await;
    assert_eq!(code(&out), "ABSENT");
    let out = net.get_bytes(&format!("{}/err", replay.url())).await;
    assert_eq!(code(&out), "transient HTTP_503");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redirect_to_private_refused(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        redirect("/to-loopback", "http://127.0.0.1/admin"),
        redirect("/to-metadata", "http://169.254.169.254/latest/meta-data/"),
        redirect("/to-private-name", "http://intranet.test/x.jpg"),
        image("/x.jpg", "image/jpeg", 1500),
    ]))
    .await;
    let port = addr(&replay).port();
    let net = strict(
        &replay,
        &pool,
        &[
            ("img.test", &["127.0.0.1"]),
            ("intranet.test", &["10.0.0.7"]),
        ],
    );
    // The allowed test host answers.
    let ok = net
        .get_bytes(&format!("http://img.test:{port}/x.jpg"))
        .await;
    assert_eq!(code(&ok), "ANSWERED", "{ok:?}");

    for path in ["/to-loopback", "/to-metadata", "/to-private-name"] {
        let out = net
            .get_bytes(&format!("http://img.test:{port}{path}"))
            .await;
        assert_eq!(code(&out), "permanent NOT_PUBLIC", "{path}: {out:?}");
    }
    // Every refused hop was refused before it was asked: only the first hops reached the server.
    assert_eq!(
        replay.asked(),
        [
            "GET /x.jpg",
            "GET /to-loopback",
            "GET /to-metadata",
            "GET /to-private-name"
        ]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cross_host_redirect_followed_and_checked(pool: PgPool) {
    let replay = Replay::start(Recording(vec![image("/x.jpg", "image/jpeg", 1500)])).await;
    let port = addr(&replay).port();
    let hop = Replay::start(Recording(vec![redirect(
        "/release/1/front",
        &format!("http://cdn.test:{port}/x.jpg"),
    )]))
    .await;
    let hop_addr = addr(&hop);
    let mut net_cfg = NetConfig::for_tests(Bases::all(&replay.url()));
    net_cfg.allow_private_images = false;
    net_cfg.private_image_allow = vec![addr(&replay), hop_addr];
    net_cfg.static_hosts = HashMap::from([
        ("caa.test".to_string(), vec!["127.0.0.1".parse().unwrap()]),
        ("cdn.test".to_string(), vec!["127.0.0.1".parse().unwrap()]),
    ]);
    let net = Net::new(net_cfg, pool.clone(), IdentifySwitch::new(false, true));
    let out = net
        .get_bytes(&format!(
            "http://caa.test:{}/release/1/front",
            hop_addr.port()
        ))
        .await;
    assert_eq!(out, Outcome::Answered(vec![7u8; 1500]));
    assert_eq!(hop.asked(), ["GET /release/1/front"]);
    assert_eq!(replay.asked(), ["GET /x.jpg"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn host_resolving_to_private_refused(pool: PgPool) {
    let replay = Replay::start(Recording(vec![image("/x.jpg", "image/jpeg", 1500)])).await;
    let port = addr(&replay).port();
    let net = strict(
        &replay,
        &pool,
        &[
            ("private.test", &["10.0.0.1"]),
            // One bad address among good ones refuses the host.
            ("mixed.test", &["127.0.0.1", "192.168.0.10"]),
            ("mapped.test", &["::ffff:10.0.0.1"]),
        ],
    );
    for host in ["private.test", "mixed.test", "mapped.test"] {
        let out = net.get_bytes(&format!("http://{host}:{port}/x.jpg")).await;
        assert_eq!(code(&out), "permanent NOT_PUBLIC", "{host}: {out:?}");
    }
    // The replay's own address on another port is not on the allow list.
    let out = net.get_bytes("http://127.0.0.1:9/x.jpg").await;
    assert_eq!(code(&out), "permanent NOT_PUBLIC", "{out:?}");
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ftp_scheme_refused(pool: PgPool) {
    let replay = Replay::start(Recording(vec![redirect("/r", "ftp://127.0.0.1/x.jpg")])).await;
    let net = net_with(&replay, &pool, |_| {});
    for url in [
        "ftp://127.0.0.1/x.jpg",
        "file:///etc/passwd",
        "gopher://127.0.0.1/",
    ] {
        let out = net.get_bytes(url).await;
        assert_eq!(code(&out), "permanent BAD_SCHEME", "{url}");
    }
    let out = net.get_bytes(&format!("{}/r", replay.url())).await;
    assert_eq!(code(&out), "permanent BAD_SCHEME", "{out:?}");
    assert_eq!(replay.asked(), ["GET /r"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn image_redirects_capped_at_three(pool: PgPool) {
    let replay = Replay::start(Recording(vec![
        redirect("/a0", "/a1"),
        redirect("/a1", "/a2"),
        redirect("/a2", "/a3"),
        redirect("/a3", "/a4"),
        image("/a4", "image/png", 1200),
    ]))
    .await;
    let out = net_with(&replay, &pool, |_| {})
        .get_bytes(&format!("{}/a0", replay.url()))
        .await;
    assert_eq!(code(&out), "permanent TOO_MANY_REDIRECTS", "{out:?}");
    assert!(!replay.asked().iter().any(|a| a.contains("/a4")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bad_url_message_has_no_query(pool: PgPool) {
    let replay = Replay::start(Recording(vec![])).await;
    let net = net_with(&replay, &pool, |_| {});
    let out = net.get_bytes("http://[::1/x.jpg?token=SECRETTOKEN").await;
    let Outcome::Permanent(f) = out else {
        panic!("{out:?}")
    };
    assert_eq!(f.code, "BAD_URL");
    assert!(!f.message.contains("SECRETTOKEN"), "{}", f.message);
    assert!(!f.message.contains('?'), "{}", f.message);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_off_downloads_nothing(pool: PgPool) {
    let replay = Replay::start(Recording(vec![image("/a.jpg", "image/jpeg", 1000)])).await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    let out = net.get_bytes(&format!("{}/a.jpg", replay.url())).await;
    assert_eq!(code(&out), "transient DISABLED");
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
#[should_panic(expected = "kima-net refused a real host")]
async fn real_image_host_panics_under_the_guard(pool: PgPool) {
    let net = Net::new(
        NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
        pool,
        IdentifySwitch::new(false, true),
    );
    let _ = net
        .get_bytes("https://coverartarchive.org/release/x/front")
        .await;
}
