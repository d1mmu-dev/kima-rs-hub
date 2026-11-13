//! M6 Task 16: `GET /api/search/discover` (`routes/search.ts:111-270`): Apple's podcast search
//! for `type=podcasts`/`all`, the music part empty until M7, 900 s caching, and an Apple
//! failure answering an empty, uncached section (`searchDiscoverPodcasts.route.test.ts`).
//! `KIMA_NET_DENY_REAL=1`.

mod common;

use std::sync::Arc;

use axum_test::TestServer;
use kima_api::AppState;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_net::testing::{Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use serde_json::Value;

fn recording() -> Recording {
    Recording::load(format!(
        "{}/../kima-podcast/tests/fixtures/apple/search.json",
        env!("CARGO_MANIFEST_DIR")
    ))
}

struct Env {
    server: TestServer,
    token: String,
    replay: Replay,
}

async fn env_with(pool: PgPool, rec: Recording) -> Env {
    let replay = Replay::start(rec).await;
    let net = Arc::new(Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    ));
    let state: AppState = common::state(pool.clone()).with_net(net);
    let token = common::user_with_token(&state, "poddiscover").await.1;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    Env {
        server,
        token,
        replay,
    }
}

async fn env(pool: PgPool) -> Env {
    env_with(pool, recording()).await
}

impl Env {
    async fn get(&self, path: &str) -> axum_test::TestResponse {
        self.server
            .get(path)
            .authorization_bearer(&self.token)
            .await
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discover_empty_q(pool: PgPool) {
    let env = env(pool).await;
    let res = env.get("/api/search/discover").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body, serde_json::json!({"results": [], "aliasInfo": null}));
    assert!(env.replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discover_music_is_empty_until_m7(pool: PgPool) {
    let env = env(pool).await;
    let res = env.get("/api/search/discover?q=dungeons").await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
    assert!(body["aliasInfo"].is_null());
    assert!(
        env.replay.asked().is_empty(),
        "music search must not ask Apple"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discover_podcasts_from_apple(pool: PgPool) {
    let env = env(pool).await;
    let res = env
        .get("/api/search/discover?q=dungeons&type=podcasts&limit=5")
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 5);
    let dnd = results
        .iter()
        .find(|r| r["name"] == "Dungeons and Daddies")
        .expect("Dungeons and Daddies in results");
    assert_eq!(dnd["type"], "podcast");
    assert_eq!(dnd["id"], 1450241676);
    assert_eq!(dnd["trackCount"], 211);
    assert!(dnd["artist"].is_string());
    assert!(!dnd["categoryIds"].as_array().unwrap().is_empty());
    assert!(
        !dnd["genres"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g == "Podcasts"),
        "the generic 'Podcasts' genre is filtered out"
    );
    assert_eq!(dnd["aiLabel"], "unknown");
    assert!(dnd["aiLabelSource"].is_null());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discover_podcasts_empty_and_uncached_when_apple_fails(pool: PgPool) {
    let env = env_with(pool.clone(), Recording(Vec::new())).await;
    let res = env
        .get("/api/search/discover?q=dungeons&type=podcasts&limit=5")
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
    let cached = kima_db::http_cache::get(&pool, "search:discover:podcasts:dungeons:5")
        .await
        .unwrap();
    assert!(cached.is_none(), "a failed section must not be cached");
}

/// M6 Task 8 (spec S3, D-207): Apple results carry the label kima already knows for that Apple
/// id (a stored show, else a preview's remembered channel tag), are hidden by default when it is
/// AI, and a label learnt after an answer was cached still applies to the cached answer.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discover_labels_known_shows_and_hides_ai(pool: PgPool) {
    let env = env(pool.clone()).await;
    let path = "/api/search/discover?q=dungeons&type=podcasts&limit=5";
    let res = env.get(path).await;
    assert_eq!(res.headers()["x-kima-ai-hidden"], "0");
    assert_eq!(res.json::<Value>()["results"].as_array().unwrap().len(), 5);

    kima_db::podcasts::remember_feed_label(&pool, "1450241676", Some(true))
        .await
        .unwrap();
    let res = env.get(path).await;
    assert_eq!(res.headers()["x-kima-ai-hidden"], "1");
    let body: Value = res.json();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|r| r["id"] != 1450241676));
    assert!(results.iter().all(|r| r["aiLabel"] == "unknown"));

    let res = env.get(&format!("{path}&ai=show")).await;
    assert!(res.headers().get("x-kima-ai-hidden").is_none());
    let body: Value = res.json();
    let dnd = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == 1450241676)
        .unwrap()
        .clone();
    assert_eq!(dnd["aiLabel"], "ai");
    assert_eq!(dnd["aiLabelSource"], "feed-show");
    assert_eq!(
        env.replay.asked().len(),
        1,
        "later answers came from the cache"
    );
}
