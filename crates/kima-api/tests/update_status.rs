//! `GET /api/whats-new/update-status` (M5b Task 17; upstream `services/updateCheck.ts` and its
//! `services/__tests__/updateCheck.test.ts` at the M5b reference `9450c94b`). Owner Q4 = B (D-270): kima-rs asks the
//! GitHub releases of `d1mmu-dev/kima-rs-hub`, not upstream's, through `kima-net`, with upstream's
//! `updateChecksEnabled` switch. No test reaches the network: GitHub is a loopback replay (`KIMA_NET_DENY_REAL=1`
//! guard on in `NetConfig::for_tests`).

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};

const LATEST: &str = "/repos/d1mmu-dev/kima-rs-hub/releases/latest";

fn entry(status: u16, body: Value) -> Entry {
    Entry {
        method: "GET".into(),
        url: LATEST.into(),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

fn tag(t: &str) -> Entry {
    entry(200, json!({ "tag_name": t, "name": "release" }))
}

struct Env {
    server: TestServer,
    state: kima_api::AppState,
    pool: PgPool,
    token: String,
}

async fn env(pool: PgPool, replay: &Replay) -> Env {
    env_as(pool, replay, "ana").await
}

/// The same, signed in as a new account `name` (a second server on the same database).
async fn env_as(pool: PgPool, replay: &Replay, name: &str) -> Env {
    // Identification off: the update check is not an enrichment lookup and still runs.
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    let state = common::state(pool.clone()).with_net(Arc::new(net));
    let (_, token) = common::user_with_token(&state, name).await;
    Env {
        server: TestServer::new(kima_api::router(state.clone())).unwrap(),
        state,
        pool,
        token,
    }
}

impl Env {
    async fn status(&self) -> Value {
        let res = self
            .server
            .get("/api/whats-new/update-status")
            .authorization_bearer(&self.token)
            .await;
        res.assert_status_ok();
        res.json::<Value>()
    }

    /// Settings as a previous check left them.
    async fn checked_before(&self, latest: &str, hours_ago: i64) {
        sqlx::query("UPDATE system_settings SET latest_known_version = $1, last_update_check = $2")
            .bind(latest)
            .bind(OffsetDateTime::now_utc() - Duration::hours(hours_ago))
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn stored(&self) -> (Option<String>, Option<OffsetDateTime>) {
        sqlx::query_as("SELECT latest_known_version, last_update_check FROM system_settings")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[test]
fn compare_versions_ignores_v_prefix() {
    use kima_api::update_check::compare_versions;
    for (a, b, expected) in [
        ("1.12.0", "1.11.0", 1),
        ("v1.12.0", "1.11.0", 1),
        ("1.11.0", "1.11.0", 0),
        ("1.12.0", "1.12.0", 0),
        ("v1.12.0", "1.12.0", 0),
        ("1.11.0", "1.12.0", -1),
        ("1.11.5", "1.12.0", -1),
        ("1.9.0", "1.10.0", -1),
        ("1.11.0.1", "1.11.0", 1),
        ("1.11", "1.11.0", 0),
        // `parseInt(n, 10) || 0`: a digit prefix counts, anything else is 0.
        ("1.2rc", "1.2", 0),
        ("1.x", "1.0", 0),
        ("2. 1", "2.1", 0),
    ] {
        assert_eq!(compare_versions(a, b).signum(), expected, "{a} vs {b}");
    }
    // A part too long for a double is Infinity in JavaScript; Infinity - Infinity is NaN, which upstream returns at
    // once and `> 0` reads as "not newer", whatever the later parts say.
    let huge = "9".repeat(400);
    let (a, b) = (format!("1.{huge}.5"), format!("1.{huge}.0"));
    assert_eq!(compare_versions(&a, &b), 0);
    assert_eq!(compare_versions(&b, &a), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn marks_an_update_when_the_latest_tag_is_newer(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v99.0.0")])).await;
    let e = env(pool, &replay).await;
    let status = e.status().await;
    assert_eq!(status["enabled"], true);
    assert_eq!(status["current"], current());
    assert_eq!(status["latest"], "99.0.0");
    assert_eq!(status["hasUpdate"], true);
    let keys: Vec<&String> = status.as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 5, "{status}");
    let (latest, checked) = e.stored().await;
    assert_eq!(latest.as_deref(), Some("99.0.0"));
    let checked = checked.expect("the check is stamped");
    // `toISOString`: UTC with milliseconds.
    let at = status["lastCheckedAt"].as_str().unwrap();
    assert_eq!(at.len(), "2026-11-21T12:00:00.000Z".len(), "{at}");
    assert!(at.ends_with('Z'));
    let parsed = OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).unwrap();
    assert!((parsed - checked).abs() < Duration::milliseconds(1));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_update_when_the_latest_tag_is_older_or_equal(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v0.0.1")])).await;
    let e = env(pool, &replay).await;
    let status = e.status().await;
    assert_eq!(
        (status["latest"].clone(), status["hasUpdate"].clone()),
        (json!("0.0.1"), json!(false))
    );
    // Equal to the running version.
    e.checked_before(current(), 25).await;
    let replay = Replay::start(Recording(vec![tag(&format!("v{}", current()))])).await;
    let e = env_as(e.pool.clone(), &replay, "bob").await;
    assert_eq!(e.status().await["hasUpdate"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cached_in_settings_and_single_flight(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v99.0.0"), tag("v98.0.0")])).await;
    let e = env(pool, &replay).await;
    // Uses the stored answer inside a day instead of calling out again.
    assert_eq!(e.status().await["hasUpdate"], true);
    let cached = e.status().await;
    assert_eq!(replay.asked().len(), 1, "{:?}", replay.asked());
    assert_eq!(cached["latest"], "99.0.0");

    // Callers that arrive while a check runs share it.
    e.checked_before("1.0.0", 25).await;
    let held = replay.hold("releases/latest");
    let both = futures::future::join(e.status(), e.status());
    let release = async {
        while replay.arrived().len() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Give a second, unshared request time to arrive before letting the first answer.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        held.release();
    };
    let ((a, b), ()) = futures::future::join(both, release).await;
    assert_eq!(
        replay.asked().len(),
        2,
        "one more request for both callers: {:?}",
        replay.asked()
    );
    assert_eq!(a["latest"], "98.0.0");
    assert_eq!(b["latest"], "98.0.0");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn asks_again_once_a_day_has_passed(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v99.0.0")])).await;
    let e = env(pool, &replay).await;
    e.checked_before("1.0.0", 25).await;
    assert_eq!(e.status().await["latest"], "99.0.0");
    assert_eq!(replay.asked().len(), 1);
    // Inside a day: the stored answer, no call.
    let replay2 = Replay::start(Recording(vec![tag("v100.0.0")])).await;
    let e2 = env_as(e.pool.clone(), &replay2, "bob").await;
    e2.checked_before("1.0.0", 23).await;
    let status = e2.status().await;
    assert_eq!(status["latest"], "1.0.0");
    assert!(replay2.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn network_failure_keeps_last_known(pool: PgPool) {
    let replay = Replay::start(Recording(vec![entry(
        502,
        json!({ "message": "bad gateway" }),
    )]))
    .await;
    let e = env(pool, &replay).await;
    e.checked_before("99.0.0", 48).await;
    let status = e.status().await;
    assert_eq!(replay.asked().len(), 1);
    assert_eq!(
        (status["latest"].clone(), status["hasUpdate"].clone()),
        (json!("99.0.0"), json!(true))
    );
    let (latest, checked) = e.stored().await;
    assert_eq!(latest.as_deref(), Some("99.0.0"));
    assert!(
        checked.unwrap() > OffsetDateTime::now_utc() - Duration::hours(1),
        "the failed check is stamped"
    );

    // No network at all (the client switched off): quiet as well.
    let off = common::state(e.pool.clone());
    let (_, token) = common::user_with_token(&off, "bob").await;
    e.checked_before("99.0.0", 48).await;
    let server = TestServer::new(kima_api::router(off)).unwrap();
    let res = server
        .get("/api/whats-new/update-status")
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["latest"], "99.0.0");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn latest_404_means_none(pool: PgPool) {
    // A repository with no published release answers 404: no newer version, no error.
    let replay = Replay::start(Recording(vec![entry(
        404,
        json!({ "message": "Not Found" }),
    )]))
    .await;
    let e = env(pool, &replay).await;
    let status = e.status().await;
    assert_eq!(
        (
            status["latest"].clone(),
            status["hasUpdate"].clone(),
            status["enabled"].clone()
        ),
        (Value::Null, json!(false), json!(true))
    );
    assert!(status["lastCheckedAt"].is_string());
    // A 200 without a tag string changes nothing either.
    let replay = Replay::start(Recording(vec![entry(200, json!({ "tag_name": 7 }))])).await;
    let e = env_as(e.pool.clone(), &replay, "bob").await;
    e.checked_before("2.0.0", 48).await;
    assert_eq!(e.status().await["latest"], "2.0.0");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn disabled_reports_no_update(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v99.0.0")])).await;
    let e = env(pool, &replay).await;
    e.checked_before("99.0.0", 48).await;
    sqlx::query("UPDATE system_settings SET update_checks_enabled = false")
        .execute(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        e.status().await,
        json!({ "enabled": false, "current": current(), "latest": null, "hasUpdate": false, "lastCheckedAt": null })
    );
    assert!(replay.asked().is_empty(), "no network call");
}

/// Owner Q4 = B: kima-rs's own releases, never upstream's.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn asks_kima_rs_hub_releases_not_upstream(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v1.0.0")])).await;
    let e = env(pool, &replay).await;
    e.status().await;
    assert_eq!(replay.asked(), [format!("GET {LATEST}")]);
    assert!(
        replay.user_agents()[0].contains("kima"),
        "{:?}",
        replay.user_agents()
    );
    assert_eq!(
        Bases::production("https://musicbrainz.org/ws/2").github,
        "https://api.github.com"
    );
}

/// Upstream's switch: `updateChecksEnabled` in the system settings (admin).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_is_an_admin_system_setting(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v99.0.0")])).await;
    let e = env(pool, &replay).await;
    let get = || {
        e.server
            .get("/api/system-settings")
            .authorization_bearer(&e.token)
    };
    assert_eq!(get().await.json::<Value>()["updateChecksEnabled"], true);
    e.server
        .post("/api/system-settings")
        .authorization_bearer(&e.token)
        .json(&json!({ "updateChecksEnabled": false }))
        .await
        .assert_status_ok();
    assert_eq!(get().await.json::<Value>()["updateChecksEnabled"], false);
    assert_eq!(e.status().await["enabled"], false);
    let bad = e
        .server
        .post("/api/system-settings")
        .authorization_bearer(&e.token)
        .json(&json!({ "updateChecksEnabled": "no" }))
        .await;
    bad.assert_status(StatusCode::BAD_REQUEST);
    e.server
        .post("/api/system-settings")
        .authorization_bearer(&e.token)
        .json(&json!({ "updateChecksEnabled": true }))
        .await
        .assert_status_ok();
    assert_eq!(e.status().await["enabled"], true);
    // A member cannot change it.
    let (_, member) = common::user_with_token(&e.state, "member").await;
    e.server
        .post("/api/system-settings")
        .authorization_bearer(&member)
        .json(&json!({ "updateChecksEnabled": false }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn needs_a_signed_in_user(pool: PgPool) {
    let replay = Replay::start(Recording(vec![])).await;
    let e = env(pool, &replay).await;
    e.server
        .get("/api/whats-new/update-status")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// Waits until the replay has seen `n` requests (held ones included).
async fn arrived(replay: &Replay, n: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while replay.arrived().len() < n {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the request arrives");
}

/// Long enough for an unshared second request to reach the replay (the GitHub limiter spaces requests a minute
/// apart, a thousandth of that in tests).
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

fn check(e: &Env) -> tokio::task::JoinHandle<kima_api_types::releases::UpdateStatus> {
    let state = e.state.clone();
    tokio::spawn(async move {
        kima_api::update_check::update_status(&state)
            .await
            .expect("update status")
    })
}

/// Upstream's `inFlight` promise outlives the request that started it: a caller that goes away while the check
/// runs neither cancels it nor makes the next caller ask GitHub a second time.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_dropped_caller_neither_cancels_the_check_nor_starts_another(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v98.0.0"), tag("v97.0.0")])).await;
    let e = env(pool, &replay).await;
    e.checked_before("1.0.0", 25).await;
    let held = replay.hold("releases/latest");
    let first = check(&e);
    arrived(&replay, 1).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let next = check(&e);
    settle().await;
    assert_eq!(replay.arrived().len(), 1, "{:?}", replay.arrived());
    held.release();
    assert_eq!(next.await.unwrap().latest.as_deref(), Some("98.0.0"));
    assert_eq!(replay.asked().len(), 1, "{:?}", replay.asked());
}

/// The check finishes and is stored even when every caller has gone.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_check_finishes_after_every_caller_left(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v98.0.0")])).await;
    let e = env(pool, &replay).await;
    e.checked_before("1.0.0", 25).await;
    let held = replay.hold("releases/latest");
    let (a, b) = (check(&e), check(&e));
    arrived(&replay, 1).await;
    a.abort();
    b.abort();
    let _ = (a.await, b.await);
    held.release();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while e.stored().await.0.as_deref() != Some("98.0.0") {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the abandoned check still stores its answer");
    // The next caller reads it without asking again.
    assert_eq!(e.status().await["latest"], "98.0.0");
    assert_eq!(replay.asked().len(), 1, "{:?}", replay.asked());
}

/// A caller cancelled while it waits for someone else's check changes nothing for the others.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_waiter_cancelled_while_waiting_changes_nothing(pool: PgPool) {
    let replay = Replay::start(Recording(vec![tag("v98.0.0"), tag("v97.0.0")])).await;
    let e = env(pool, &replay).await;
    e.checked_before("1.0.0", 25).await;
    let held = replay.hold("releases/latest");
    let first = check(&e);
    arrived(&replay, 1).await;
    let waiter = check(&e);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let late = check(&e);
    settle().await;
    held.release();
    assert_eq!(first.await.unwrap().latest.as_deref(), Some("98.0.0"));
    assert_eq!(late.await.unwrap().latest.as_deref(), Some("98.0.0"));
    assert_eq!(replay.arrived().len(), 1, "{:?}", replay.arrived());
}

/// `KIMA_NET_DENY_REAL=1` with the real GitHub address (CI, browser runs): the check is refused without a panic and
/// answers like a failed one, stamped, so it is not tried again on every request.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_refused_real_host_answers_like_a_failed_check(pool: PgPool) {
    let mut cfg = NetConfig::for_tests(Bases::production("https://musicbrainz.org/ws/2"));
    cfg.deny_real = true;
    let net = Net::new(cfg, pool.clone(), IdentifySwitch::new(true, true));
    let state = common::state(pool.clone()).with_net(Arc::new(net));
    let (_, token) = common::user_with_token(&state, "ana").await;
    let e = Env {
        server: TestServer::new(kima_api::router(state.clone())).unwrap(),
        state,
        pool,
        token,
    };
    e.checked_before("99.0.0", 48).await;
    let status = e.status().await;
    assert_eq!(
        (status["latest"].clone(), status["hasUpdate"].clone()),
        (json!("99.0.0"), json!(true))
    );
    let (latest, checked) = e.stored().await;
    assert_eq!(latest.as_deref(), Some("99.0.0"));
    assert!(
        checked.unwrap() > OffsetDateTime::now_utc() - Duration::hours(1),
        "the refused check is stamped"
    );
    assert_eq!(e.status().await["latest"], "99.0.0");
}
