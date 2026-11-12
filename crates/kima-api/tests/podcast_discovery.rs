//! M6 Task 13: `/api/podcasts/{categories,charts,for-you,discover/*,{id}/similar,
//! preview/{itunesId}/similar}` (`podcasts.ts` `pageParams`/`answerChart` + the discovery part
//! of `podcastCatalog.ts`), over the recorded Apple catalogue also used by
//! `kima-podcast`'s own tests (`podcastDiscovery.route.test.ts`,
//! `podcastUpstreamFailures.route.test.ts` chart/discover cases). `KIMA_NET_DENY_REAL=1`.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use kima_api::AppState;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use serde_json::{Value, json};
use uuid::Uuid;

const DND: &str = "1450241676";

fn recording() -> Recording {
    Recording::load(format!(
        "{}/../kima-podcast/tests/fixtures/apple/catalog.json",
        env!("CARGO_MANIFEST_DIR")
    ))
}

fn json_entry(url: &str, status: u16, body: Value) -> Entry {
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

fn html_entry(url: &str, body: &str) -> Entry {
    use base64::Engine;
    Entry {
        headers: BTreeMap::from([("content-type".to_string(), "text/html".to_string())]),
        body: Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(body)),
        ..json_entry(url, 200, Value::Null)
    }
}

struct Env {
    server: axum_test::TestServer,
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
    let token = common::user_with_token(&state, "poddisc").await.1;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
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

async fn store_dnd(pool: &PgPool) -> Uuid {
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://feeds.example.invalid/disc-dnd",
            title: "Dungeons and Daddies",
            author: None,
            description: None,
            image_url: None,
            itunes_id: Some(DND),
            category_ids: &[1495],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    show.id
}

// podcastDiscovery.route.test.ts

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lists_apples_categories_with_their_parents(pool: PgPool) {
    let env = env(pool).await;
    let res = env.get("/api/podcasts/categories").await;
    res.assert_status_ok();
    let body: Vec<Value> = res.json();
    assert!(body.contains(&json!({"id": 1486, "name": "Comedy Fiction", "parentId": 1483})));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_a_categorys_chart_paged_with_categories_named(pool: PgPool) {
    let env = env(pool).await;
    let res = env
        .get("/api/podcasts/charts?category=1495&limit=5&offset=0")
        .await;
    res.assert_status_ok();
    let body: Vec<Value> = res.json();
    assert_eq!(body.len(), 5);
    assert_eq!(body[0]["chartRank"], json!(1));
    assert_eq!(body[0]["isExternal"], json!(true));
    assert_eq!(body[0]["categoryIds"], json!([1495]));
    let next = env
        .get("/api/podcasts/charts?category=1495&limit=5&offset=5")
        .await;
    let next_body: Vec<Value> = next.json();
    assert_eq!(next_body[0]["chartRank"], json!(6));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn background_request_matches_and_bad_flag_is_400(pool: PgPool) {
    let env = env(pool).await;
    let row = env
        .get("/api/podcasts/charts?category=1495&limit=5&background=true")
        .await;
    row.assert_status_ok();
    let plain = env.get("/api/podcasts/charts?category=1495&limit=5").await;
    assert_eq!(row.json::<Value>(), plain.json::<Value>());
    env.get("/api/podcasts/charts?category=1495&background=yes")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refuses_a_category_that_is_not_an_id(pool: PgPool) {
    let env = env(pool).await;
    env.get("/api/podcasts/charts?category=comedy")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discover_top_and_genres_come_from_the_charts(pool: PgPool) {
    let env = env(pool).await;
    let top = env.get("/api/podcasts/discover/top?limit=3").await;
    top.assert_status_ok();
    let ranks: Vec<Value> = top
        .json::<Vec<Value>>()
        .iter()
        .map(|p| p["chartRank"].clone())
        .collect();
    assert_eq!(ranks, vec![json!(1), json!(2), json!(3)]);
    let genres = env.get("/api/podcasts/discover/genres?genres=1495").await;
    genres.assert_status_ok();
    let body: Value = genres.json();
    assert_eq!(body["1495"].as_array().unwrap().len(), 10);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discover_genre_paginates_like_charts(pool: PgPool) {
    let env = env(pool).await;
    let res = env.get("/api/podcasts/discover/genre/1495?limit=2").await;
    res.assert_status_ok();
    assert_eq!(res.json::<Vec<Value>>().len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn builds_for_you_from_the_followed_shows_category_without_the_show_itself(pool: PgPool) {
    let show = store_dnd(&pool).await;
    let env = env(pool.clone()).await;
    let user: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'poddisc'")
        .fetch_one(&pool)
        .await
        .unwrap();
    podcasts::subscribe(&pool, user, show).await.unwrap();
    let res = env.get("/api/podcasts/for-you").await;
    res.assert_status_ok();
    let body: Vec<Value> = res.json();
    assert_eq!(body.len(), 1);
    assert_eq!(
        body[0]["category"],
        json!({"id": 1495, "name": "Improv", "parentId": 1303})
    );
    assert_eq!(body[0]["because"], json!(["Dungeons and Daddies"]));
    assert!(
        body[0]["podcasts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["itunesId"] != json!(DND))
    );
    assert_eq!(body[0]["podcasts"].as_array().unwrap().len(), 12);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn finds_similar_shows_on_the_shows_own_category_chart_never_the_show(pool: PgPool) {
    // Both scenarios share one lookup cache, as the upstream test file does running in order:
    // the first call warms the batched Apple lookup for the 1495 chart, which the second
    // (across 1495/1303/1502/1507) then reuses instead of asking Apple again for those ids.
    let show = store_dnd(&pool).await;
    let env = env(pool).await;
    let res = env.get(&format!("/api/podcasts/{show}/similar")).await;
    res.assert_status_ok();
    let body: Vec<Value> = res.json();
    assert_eq!(body.len(), 12);
    assert!(body.iter().all(|p| p["itunesId"] != json!(DND)));

    let preview = env
        .get(&format!("/api/podcasts/preview/{DND}/similar"))
        .await;
    preview.assert_status_ok();
    let body: Vec<Value> = preview.json();
    assert_eq!(body.len(), 12);
    assert!(body.iter().all(|p| p["itunesId"] != json!(DND)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_404_for_a_similar_request_on_a_show_it_does_not_have(pool: PgPool) {
    let env = env(pool).await;
    let res = env
        .get(&format!("/api/podcasts/{}/similar", Uuid::now_v7()))
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn preview_similar_404s_when_the_apple_id_is_not_a_number(pool: PgPool) {
    let env = env(pool).await;
    let res = env.get("/api/podcasts/preview/pod_disc_dnd/similar").await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>()["error"], json!("Podcast not found"));
}

// podcastUpstreamFailures.route.test.ts (chart/discover parts)

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_unknown_category_is_404_without_asking_for_its_chart(pool: PgPool) {
    let env = env(pool).await;
    env.get("/api/podcasts/charts?category=999999")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    env.get("/api/podcasts/discover/genre/999999")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // Apple's chart endpoint was never asked: only the category tree request landed.
    assert!(
        !env.replay
            .asked()
            .iter()
            .any(|a| a.contains("genre=999999"))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apple_4xx_is_404_and_a_garbled_reply_is_502(pool: PgPool) {
    let mut rec = recording();
    rec.0.retain(|e| !e.url.contains("genre=1495"));
    rec.0.push(json_entry(
        "/us/rss/toppodcasts/limit=200/genre=1495/json",
        403,
        json!("no"),
    ));
    rec.0
        .retain(|e| !e.url.contains("/us/rss/toppodcasts/limit=200/json"));
    rec.0.push(html_entry(
        "/us/rss/toppodcasts/limit=200/json",
        "<html>maintenance</html>",
    ));
    let env = env_with(pool, rec).await;
    env.get("/api/podcasts/charts?category=1495")
        .await
        .assert_status(StatusCode::NOT_FOUND);
    env.get("/api/podcasts/charts")
        .await
        .assert_status(StatusCode::BAD_GATEWAY);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apple_throttling_is_502_not_no_such_chart(pool: PgPool) {
    let mut rec = recording();
    rec.0.retain(|e| !e.url.contains("genre=1495"));
    rec.0.push(json_entry(
        "/us/rss/toppodcasts/limit=200/genre=1495/json",
        429,
        json!("slow down"),
    ));
    let env = env_with(pool, rec).await;
    env.get("/api/podcasts/charts?category=1495")
        .await
        .assert_status(StatusCode::BAD_GATEWAY);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn asks_apple_once_per_real_category_and_leaves_out_an_id_that_is_not_one(pool: PgPool) {
    let env = env(pool).await;
    let res = env
        .get("/api/podcasts/discover/genres?genres=1495,1495,999999")
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert!(body.get("1495").is_some());
    assert!(body.get("999999").is_none());
    assert_eq!(
        env.replay
            .asked()
            .iter()
            .filter(|a| a.contains("genre=1495"))
            .count(),
        1
    );
    assert!(
        !env.replay
            .asked()
            .iter()
            .any(|a| a.contains("genre=999999"))
    );
}
