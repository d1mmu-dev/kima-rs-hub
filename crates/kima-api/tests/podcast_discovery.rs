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

// ---- M6 Task 17: podcastChartCountry changes the chart path (`systemSettings.ts:113`) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn chart_country_changes_chart_path(pool: PgPool) {
    let mut rec = recording();
    rec.0.push(json_entry(
        "/de/rss/toppodcasts/limit=200/json",
        200,
        json!({"feed": {"entry": []}}),
    ));
    let env = env_with(pool, rec).await;
    // `env`'s first registered user is admin (`users::register`).
    env.server
        .post("/api/system-settings")
        .authorization_bearer(&env.token)
        .json(&json!({ "podcastChartCountry": "de" }))
        .await
        .assert_status_ok();
    env.get("/api/podcasts/charts").await.assert_status_ok();
    assert!(
        env.replay
            .asked()
            .contains(&"GET /de/rss/toppodcasts/limit=200/json".to_string())
    );
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

// --- Task 14: preview a show and sample its first episode ---
// (`podcastSnippetTicket.route.test.ts`, the preview/discovery-adjacent cases of
// `podcastUpstreamFailures.route.test.ts`.) `preview_records_channel_ai_label` is not covered
// here: it needs the `ai_feed`/`podcast_feed_labels` schema that Task 0/Task 8 own, which this
// slice does not touch per the owner's explicit boundary (same note as Task 13's
// `charts_hide_known_ai_shows`).

use axum::http::header;
use axum::response::IntoResponse;
use kima_net::testing::{AudioHost, AudioMode};

/// A show's lookup reply pointing at a feed URL this test controls.
fn lookup_entry(itunes_id: &str, feed_url: &str, category_ids: &[i32]) -> Entry {
    json_entry(
        &format!("/lookup?id={itunes_id}&entity=podcast"),
        200,
        json!({"resultCount": 1, "results": [{
            "kind": "podcast",
            "collectionId": itunes_id.parse::<i64>().unwrap(),
            "feedUrl": feed_url,
            "genreIds": category_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "collectionName": "Preview Show",
            "artistName": "Preview Author",
            "artworkUrl600": "https://img.example.invalid/600.jpg",
            "trackCount": 5,
        }]}),
    )
}

/// A fresh network pointed at `replay` for Apple, used with a custom recording (Task 14 tests
/// each add their own lookup entry, so they do not share [`env`]'s plain recording).
fn net_to(replay: &Replay, pool: PgPool) -> Arc<Net> {
    Arc::new(Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool,
        IdentifySwitch::new(false, false),
    ))
}

/// Where a preview feed's one episode points its audio.
enum PreviewAudio {
    /// No episode: an empty feed.
    None,
    /// `/ep1.mp3` on this same host.
    SelfHosted,
    /// A URL this test controls directly (another local host).
    External(String),
}

/// A loopback feed host for Task 14: `/feed.xml` (counted), and `/ep1.mp3` when self-hosted.
struct PreviewFeedHost {
    base: String,
    task: tokio::task::JoinHandle<()>,
    feed_hits: Arc<std::sync::atomic::AtomicUsize>,
}

impl PreviewFeedHost {
    async fn start(audio: PreviewAudio) -> PreviewFeedHost {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let feed_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits = feed_hits.clone();
        let audio_url = match audio {
            PreviewAudio::None => None,
            PreviewAudio::SelfHosted => Some(format!("{base}/ep1.mp3")),
            PreviewAudio::External(url) => Some(url),
        };
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let hits = hits.clone();
            let audio_url = audio_url.clone();
            async move {
                match uri.path() {
                    "/feed.xml" => {
                        hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let item = audio_url.map(|url| format!(
                            r#"<item><title>Ep 1</title><guid>ep-1</guid><pubDate>Mon, 13 Oct 2026 10:00:00 GMT</pubDate><description>first</description><enclosure url="{url}" length="9" type="audio/mpeg"/></item>"#
                        )).unwrap_or_default();
                        let xml = format!(
                            r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Preview Show</title><description>d</description>{item}</channel></rss>"#
                        );
                        ([(header::CONTENT_TYPE, "application/rss+xml")], xml).into_response()
                    }
                    "/ep1.mp3" => {
                        ([(header::CONTENT_TYPE, "audio/mpeg")], b"sample-mp3".to_vec()).into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        PreviewFeedHost {
            base,
            task,
            feed_hits,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn feed_hits(&self) -> usize {
        self.feed_hits.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for PreviewFeedHost {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn preview_404s_when_the_apple_id_is_not_a_number(pool: PgPool) {
    let env = env(pool).await;
    let res = env.get("/api/podcasts/preview/not-a-number").await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>()["error"], json!("Podcast not found"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn preview_answers_from_apple_alone_when_the_feed_is_slow(pool: PgPool) {
    let slow = AudioHost::start(
        Vec::new(),
        AudioMode::HoldHeaders(std::time::Duration::from_secs(5)),
    )
    .await;
    let mut rec = recording();
    rec.0.push(lookup_entry("9001001", &slow.url(), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone())
        .with_net(net_to(&replay, pool.clone()))
        .with_preview_feed_timeout(std::time::Duration::from_millis(200));
    let token = common::user_with_token(&state, "preview1").await.1;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get("/api/podcasts/preview/9001001")
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["itunesId"], json!("9001001"));
    assert_eq!(body["title"], json!("Preview Show"));
    assert_eq!(body["previewEpisodes"], json!([]));
    assert_eq!(body["snippet"], Value::Null);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn preview_marks_a_followed_show(pool: PgPool) {
    let feed = PreviewFeedHost::start(PreviewAudio::SelfHosted).await;
    let mut rec = recording();
    rec.0
        .push(lookup_entry("9001002", &feed.url("/feed.xml"), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone()).with_net(net_to(&replay, pool.clone()));
    let token = common::user_with_token(&state, "preview2").await.1;
    let user: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'preview2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let show = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &feed.url("/feed.xml"),
            title: "Preview Show",
            author: None,
            description: None,
            image_url: None,
            itunes_id: Some("9001002"),
            category_ids: &[1495],
            language: None,
            explicit: false,
            episode_count: 1,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    podcasts::subscribe(&pool, user, show.id).await.unwrap();
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get("/api/podcasts/preview/9001002")
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["isSubscribed"], json!(true));
    assert_eq!(body["subscribedPodcastId"], json!(show.id));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn snippet_is_remembered_after_preview(pool: PgPool) {
    let feed = PreviewFeedHost::start(PreviewAudio::SelfHosted).await;
    let mut rec = recording();
    rec.0
        .push(lookup_entry("9001003", &feed.url("/feed.xml"), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone()).with_net(net_to(&replay, pool.clone()));
    let token = common::user_with_token(&state, "preview3").await.1;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let preview = server
        .get("/api/podcasts/preview/9001003")
        .authorization_bearer(&token)
        .await;
    preview.assert_status_ok();
    assert_eq!(feed.feed_hits(), 1);
    let sample = server
        .get("/api/podcasts/preview/9001003/snippet")
        .authorization_bearer(&token)
        .await;
    sample.assert_status_ok();
    assert_eq!(sample.as_bytes().as_ref(), b"sample-mp3");
    assert_eq!(
        feed.feed_hits(),
        1,
        "snippet refetched the feed although the pick was remembered"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lets_a_stream_ticket_reach_the_first_episode_sample(pool: PgPool) {
    let feed = PreviewFeedHost::start(PreviewAudio::SelfHosted).await;
    let mut rec = recording();
    rec.0
        .push(lookup_entry("9001004", &feed.url("/feed.xml"), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone()).with_net(net_to(&replay, pool.clone()));
    let (user, _) = common::user_with_token(&state, "preview4").await;
    let ticket = state.jwt.issue_stream(user.id, user.token_version);
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!(
            "/api/podcasts/preview/9001004/snippet?token={ticket}"
        ))
        .await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), b"sample-mp3");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn takes_the_ticket_as_a_bearer_credential_too(pool: PgPool) {
    let feed = PreviewFeedHost::start(PreviewAudio::SelfHosted).await;
    let mut rec = recording();
    rec.0
        .push(lookup_entry("9001005", &feed.url("/feed.xml"), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone()).with_net(net_to(&replay, pool.clone()));
    let (user, _) = common::user_with_token(&state, "preview5").await;
    let ticket = state.jwt.issue_stream(user.id, user.token_version);
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get("/api/podcasts/preview/9001005/snippet")
        .authorization_bearer(&ticket)
        .await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), b"sample-mp3");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_404_when_the_show_id_hides_an_encoded_slash(pool: PgPool) {
    let env = env(pool).await;
    let res = env.get("/api/podcasts/preview/1%2F2/snippet").await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>()["error"], json!("Podcast not found"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refuses_a_ticket_on_a_trailing_slash_spelling_of_the_sample(pool: PgPool) {
    let feed = PreviewFeedHost::start(PreviewAudio::SelfHosted).await;
    let mut rec = recording();
    rec.0
        .push(lookup_entry("9001006", &feed.url("/feed.xml"), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone()).with_net(net_to(&replay, pool.clone()));
    let (user, _) = common::user_with_token(&state, "preview6").await;
    let ticket = state.jwt.issue_stream(user.id, user.token_version);
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get(&format!(
            "/api/podcasts/preview/9001006/snippet/?token={ticket}"
        ))
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apple_down_while_previewing_a_show_or_its_sample_is_a_502(pool: PgPool) {
    let mut rec = recording();
    rec.0.push(json_entry(
        "/lookup?id=9001007&entity=podcast",
        503,
        json!("maintenance"),
    ));
    let env = env_with(pool, rec).await;
    env.get("/api/podcasts/preview/9001007")
        .await
        .assert_status(StatusCode::BAD_GATEWAY);
    env.get("/api/podcasts/preview/9001007/snippet")
        .await
        .assert_status(StatusCode::BAD_GATEWAY);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn snippet_404_when_the_show_has_no_episode_to_sample(pool: PgPool) {
    let feed = PreviewFeedHost::start(PreviewAudio::None).await;
    let mut rec = recording();
    rec.0
        .push(lookup_entry("9001008", &feed.url("/feed.xml"), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone()).with_net(net_to(&replay, pool.clone()));
    let token = common::user_with_token(&state, "preview8").await.1;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get("/api/podcasts/preview/9001008/snippet")
        .authorization_bearer(&token)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        res.json::<Value>()["error"],
        json!("This podcast has no episode to sample")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn snippet_404_when_the_episode_host_refuses_it(pool: PgPool) {
    let bad_audio = AudioHost::start(Vec::new(), AudioMode::Status(404)).await;
    let feed =
        PreviewFeedHost::start(PreviewAudio::External(format!("{}/x.mp3", bad_audio.url()))).await;
    let mut rec = recording();
    rec.0
        .push(lookup_entry("9001009", &feed.url("/feed.xml"), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone()).with_net(net_to(&replay, pool.clone()));
    let token = common::user_with_token(&state, "preview9a").await.1;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get("/api/podcasts/preview/9001009/snippet")
        .authorization_bearer(&token)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        res.json::<Value>()["error"],
        json!("Episode audio not found")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn snippet_502_when_the_episode_host_is_down(pool: PgPool) {
    let bad_audio = AudioHost::start(Vec::new(), AudioMode::Status(502)).await;
    let feed =
        PreviewFeedHost::start(PreviewAudio::External(format!("{}/x.mp3", bad_audio.url()))).await;
    let mut rec = recording();
    rec.0
        .push(lookup_entry("9001010", &feed.url("/feed.xml"), &[1495]));
    let replay = Replay::start(rec).await;
    let state: AppState = common::state(pool.clone()).with_net(net_to(&replay, pool.clone()));
    let token = common::user_with_token(&state, "preview9b").await.1;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get("/api/podcasts/preview/9001010/snippet")
        .authorization_bearer(&token)
        .await;
    res.assert_status(StatusCode::BAD_GATEWAY);
    assert_eq!(
        res.json::<Value>()["error"],
        json!("The preview's host did not answer; try again shortly")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn snippet_400_for_a_blocked_audio_url(pool: PgPool) {
    let net = Arc::new(Net::new(
        {
            let mut c = NetConfig::for_tests(Bases::all("http://127.0.0.1:9"));
            c.allow_private_images = false;
            c
        },
        pool.clone(),
        IdentifySwitch::new(false, false),
    ));
    let state: AppState = common::state(pool.clone()).with_net(net);
    let token = common::user_with_token(&state, "preview9c").await.1;
    kima_db::http_cache::put(
        &pool,
        "podcasts:snippet:v1:9001099",
        "podcasts",
        "snippet 9001099",
        kima_db::http_cache::CacheOutcome::Answered,
        &json!({"audio_url": "http://127.0.0.1:9/ep.mp3", "mime_type": "audio/mpeg"}),
        std::time::Duration::from_secs(86_400),
    )
    .await
    .unwrap();
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .get("/api/podcasts/preview/9001099/snippet")
        .authorization_bearer(&token)
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    let body: Value = res.json();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("Invalid episode audio URL:"),
        "{body}"
    );
}
