//! M6 `/api/podcasts` (upstream `routes/podcasts.ts`), against a local feed host that also plays
//! Apple (`podcastSubscribe.route.test.ts` does the same with one `http.createServer`). Nothing
//! here reaches the network (`KIMA_NET_DENY_REAL=1`).

mod common;

use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum_test::TestServer;
use kima_api::AppState;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_net::{Bases, Net, NetConfig};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use uuid::Uuid;

const STORED_ITUNES: &str = "1450241676";

const FEED: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Race Show</title><description>d</description>
<item><title>Ep 1</title><guid>race-1</guid><pubDate>Mon, 13 Oct 2026 10:00:00 GMT</pubDate>
<enclosure url="https://cdn.example.invalid/1.mp3" length="1000" type="audio/mpeg"/></item></channel></rss>"#;

/// A loopback host serving feeds and Apple's lookup: show 555 is the feed at `/real.xml`.
/// `/race.xml` answers only once two requests for it are waiting, so two follows of a new show
/// really overlap.
struct FeedHost {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl FeedHost {
    async fn start() -> FeedHost {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let race = Arc::new(tokio::sync::Barrier::new(2));
        let me = base.clone();
        let app = Router::new().fallback(move |uri: Uri| {
            let (me, race) = (me.clone(), race.clone());
            async move { answer(&me, &race, &uri).await }
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        FeedHost { base, task }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

impl Drop for FeedHost {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer(base: &str, race: &tokio::sync::Barrier, uri: &Uri) -> Response {
    let path = uri.path();
    if path.starts_with("/lookup") {
        if uri.query().is_some_and(|q| q.contains("id=888")) {
            return (StatusCode::SERVICE_UNAVAILABLE, "maintenance").into_response();
        }
        if uri.query().is_some_and(|q| q.contains("id=889")) {
            return axum::Json(json!({"error": "bad response"})).into_response();
        }
        let body = json!({"resultCount": 1, "results": [{"kind": "podcast", "collectionId": 555,
            "feedUrl": format!("{base}/real.xml"), "genreIds": ["1495"]}]});
        return axum::Json(body).into_response();
    }
    match path {
        "/down.xml" => (StatusCode::SERVICE_UNAVAILABLE, "maintenance").into_response(),
        "/gone.xml" => (StatusCode::NOT_FOUND, "not here").into_response(),
        "/real.xml/" => {
            let body = FEED.replace(
                "<title>Race Show</title>",
                "<title>Race Show</title><link>https://podcasts.apple.com/id555</link>",
            );
            ([(header::CONTENT_TYPE, "application/rss+xml")], body).into_response()
        }
        "/page.xml" => (
            [(header::CONTENT_TYPE, "text/html")],
            "<html><body>a web page, not a feed</body></html>",
        )
            .into_response(),
        _ => {
            if path == "/race.xml" {
                race.wait().await;
            }
            ([(header::CONTENT_TYPE, "application/rss+xml")], FEED).into_response()
        }
    }
}

fn net_for(host: &FeedHost, pool: &PgPool, f: impl FnOnce(&mut NetConfig)) -> Arc<Net> {
    let mut cfg = NetConfig::for_tests(Bases::all(&host.base));
    f(&mut cfg);
    // The identification switch is off: following a show is user-facing.
    Arc::new(Net::new(
        cfg,
        pool.clone(),
        IdentifySwitch::new(false, false),
    ))
}

struct Env {
    server: TestServer,
    state: AppState,
    host: FeedHost,
    user: Uuid,
    token: String,
}

async fn env_with(pool: PgPool, f: impl FnOnce(&mut NetConfig)) -> Env {
    let host = FeedHost::start().await;
    let net = net_for(&host, &pool, f);
    let state = common::state(pool).with_net(net);
    let (user, token) = common::user_with_token(&state, "podder").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    Env {
        server,
        state,
        host,
        user: user.id,
        token,
    }
}

async fn env(pool: PgPool) -> Env {
    env_with(pool, |_| {}).await
}

fn show<'a>(feed_url: &'a str, title: &'a str, itunes_id: Option<&'a str>) -> NewShow<'a> {
    NewShow {
        feed_url,
        title,
        author: Some("Host"),
        description: Some("About it"),
        image_url: Some("https://feed.example/cover.jpg"),
        itunes_id,
        category_ids: &[],
        language: Some("en"),
        explicit: false,
        episode_count: 0,
        feed_etag: None,
        feed_last_modified: None,
    }
}

async fn store(pool: &PgPool, feed_url: &str, title: &str, itunes: Option<&str>) -> Uuid {
    podcasts::insert_show(pool, Uuid::now_v7(), &show(feed_url, title, itunes))
        .await
        .unwrap()
        .id
}

fn episode(guid: &str, published_at: OffsetDateTime) -> NewEpisode {
    NewEpisode {
        guid: guid.into(),
        title: guid.into(),
        description: Some(format!("about {guid}")),
        audio_url: format!("https://a.invalid/{guid}.mp3"),
        duration: 60,
        published_at,
        episode_number: None,
        season: None,
        image_url: Some(format!("https://a.invalid/{guid}.jpg")),
        file_size: None,
        mime_type: None,
    }
}

/// Day `n` of October 2026, 06:00 UTC (upstream's `episodeOnDay`).
fn day(n: u8) -> OffsetDateTime {
    datetime!(2026-10-01 06:00 UTC).replace_day(n).unwrap()
}

async fn add_episodes(pool: &PgPool, show: Uuid, eps: &[(&str, OffsetDateTime)]) {
    let eps: Vec<NewEpisode> = eps.iter().map(|(g, t)| episode(g, *t)).collect();
    podcasts::insert_episodes(pool, show, &eps).await.unwrap();
}

async fn episode_id(pool: &PgPool, guid: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = $1")
        .bind(guid)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn progress(pool: &PgPool, user: Uuid, guid: &str, at: f64, of: f64, finished: bool) {
    let episode = episode_id(pool, guid).await;
    sqlx::query(
        "INSERT INTO podcast_progress (user_id, episode_id, \"current_time\", duration, is_finished)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(user)
    .bind(episode)
    .bind(at)
    .bind(of)
    .bind(finished)
    .execute(pool)
    .await
    .unwrap();
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

impl Env {
    fn pool(&self) -> &PgPool {
        &self.state.pool
    }

    async fn subscribe(&self, body: Value) -> axum_test::TestResponse {
        self.server
            .post("/api/podcasts/subscribe")
            .authorization_bearer(&self.token)
            .json(&body)
            .await
    }

    async fn get(&self, path: &str) -> axum_test::TestResponse {
        self.server
            .get(path)
            .authorization_bearer(&self.token)
            .await
    }

    async fn delete(&self, path: &str) -> axum_test::TestResponse {
        self.server
            .delete(path)
            .authorization_bearer(&self.token)
            .await
    }

    async fn stored_show(&self) -> Uuid {
        store(
            self.pool(),
            "https://old-host.example.invalid/feed",
            "Stored Show",
            Some(STORED_ITUNES),
        )
        .await
    }
}

// ---- Task 6: follow, unfollow, the followed list (podcastSubscribe.route.test.ts) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn follows_a_stored_show_found_by_its_apple_id_though_its_feed_address_has_changed(
    pool: PgPool,
) {
    let env = env(pool).await;
    let stored = env.stored_show().await;
    let res = env
        .subscribe(
            json!({"itunesId": 1450241676, "feedUrl": "https://new-host.example.invalid/feed"}),
        )
        .await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(
        body,
        json!({"success": true, "podcast": {"id": stored.to_string(), "title": "Stored Show"},
               "message": "Subscribed successfully"})
    );
    let pool = env.pool();
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM podcasts WHERE itunes_id = '1450241676'"
        )
        .await,
        1
    );
    assert!(
        podcasts::is_subscribed(pool, env.user, stored)
            .await
            .unwrap()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn follows_once_however_often_it_is_asked(pool: PgPool) {
    let env = env(pool).await;
    let stored = env.stored_show().await;
    let first = env.subscribe(json!({"itunesId": STORED_ITUNES})).await;
    let again = env.subscribe(json!({"itunesId": STORED_ITUNES})).await;
    assert_eq!(
        [
            first.json::<Value>()["message"].clone(),
            again.json::<Value>()["message"].clone()
        ],
        [
            json!("Subscribed successfully"),
            json!("Already subscribed")
        ]
    );
    assert_eq!(
        count(
            env.pool(),
            &format!("SELECT count(*) FROM podcast_subscriptions WHERE podcast_id = '{stored}'")
        )
        .await,
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn survives_two_follows_of_a_new_show_at_once_one_show_stored_both_answered(pool: PgPool) {
    let env = env(pool).await;
    let feed_url = env.host.url("/race.xml");
    let (a, b) = tokio::join!(
        env.subscribe(json!({"feedUrl": feed_url})),
        env.subscribe(json!({"feedUrl": feed_url}))
    );
    assert_eq!([a.status_code(), b.status_code()], [StatusCode::OK; 2]);
    let (a, b) = (a.json::<Value>(), b.json::<Value>());
    assert_eq!(a["podcast"]["id"], b["podcast"]["id"]);
    assert_eq!(a["podcast"]["title"], "Race Show");
    let pool = env.pool();
    assert_eq!(
        count(
            pool,
            &format!("SELECT count(*) FROM podcasts WHERE feed_url = '{feed_url}'")
        )
        .await,
        1
    );
    assert_eq!(
        count(pool, "SELECT count(*) FROM podcast_subscriptions").await,
        1
    );
    // The show was stored with its feed's episode.
    let row: (i32, i64) = sqlx::query_as(
        "SELECT episode_count, (SELECT count(*) FROM podcast_episodes) FROM podcasts",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(row, (1, 1));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stores_an_apple_id_only_when_apple_says_it_names_the_feed_so_no_one_can_claim_a_real_shows_id(
    pool: PgPool,
) {
    let env = env(pool).await;
    // A member pairs their own feed with show 555's Apple id.
    let impostor = env
        .subscribe(json!({"feedUrl": env.host.url("/impostor.xml"), "itunesId": "555"}))
        .await;
    impostor.assert_status_ok();
    let impostor: Uuid = impostor.json::<Value>()["podcast"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let stored = podcasts::show_by_id(env.pool(), impostor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.itunes_id, None);

    // Following show 555 by its id then reaches the real feed, not the impostor.
    let real = env.subscribe(json!({"itunesId": "555"})).await;
    real.assert_status_ok();
    let real: Uuid = real.json::<Value>()["podcast"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(real, impostor);
    let stored = podcasts::show_by_id(env.pool(), real)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (stored.feed_url, stored.itunes_id.as_deref()),
        (env.host.url("/real.xml"), Some("555"))
    );
    // Apple's record named its categories.
    assert_eq!(stored.category_ids, vec![1495]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_a_feed_that_is_not_there_404_a_host_that_is_down_502_and_a_page_that_is_not_a_feed_422(
    pool: PgPool,
) {
    let env = env(pool).await;
    let mut got = Vec::new();
    for path in ["/gone.xml", "/down.xml", "/page.xml"] {
        let res = env.subscribe(json!({"feedUrl": env.host.url(path)})).await;
        got.push((res.status_code().as_u16(), res.json::<Value>()));
    }
    assert_eq!(
        got[0],
        (
            404,
            json!({"error": "Nothing was found at that feed address"})
        )
    );
    assert_eq!(
        got[1],
        (
            502,
            json!({"error": "The show's feed or Apple Podcasts did not answer; try again shortly"})
        )
    );
    assert_eq!(got[2].0, 422);
    assert_eq!(got[2].1["error"], "Podcast feed could not be read");
    assert!(got[2].1["detail"].is_string());
    assert_eq!(count(env.pool(), "SELECT count(*) FROM podcasts").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn subscribe_requires_feed_or_itunes_id(pool: PgPool) {
    let env = env(pool).await;
    for body in [
        json!({}),
        json!({"feedUrl": "", "itunesId": ""}),
        json!({"itunesId": null}),
    ] {
        let res = env.subscribe(body).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(
            res.json::<Value>(),
            json!({"error": "feedUrl or itunesId is required"})
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn subscribe_by_unknown_apple_id_is_404(pool: PgPool) {
    let env = env(pool).await;
    // The local Apple knows only 555: a lookup for 777 names no feed.
    let res = env.subscribe(json!({"itunesId": "777"})).await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "Podcast not found in iTunes"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn subscribe_refuses_private_feed_address(pool: PgPool) {
    // Private addresses refused as in production; only the local host itself is allowed.
    let env = env_with(pool, |cfg| cfg.allow_private_images = false).await;
    for (url, why) in [
        ("http://10.0.0.1/feed.xml", "Blocked: internal address"),
        ("http://[::1]:9/feed.xml", "Blocked: internal address"),
        ("http://localhost/feed.xml", "Blocked hostname: localhost"),
        ("ftp://example.invalid/feed.xml", "Blocked scheme: ftp:"),
        ("not a url", "Invalid URL"),
    ] {
        let res = env.subscribe(json!({"feedUrl": url})).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(
            res.json::<Value>(),
            json!({"error": format!("Invalid feed URL: {why}")}),
            "{url}"
        );
    }
    assert_eq!(count(env.pool(), "SELECT count(*) FROM podcasts").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn gives_each_followed_show_the_date_of_its_newest_episode_none_for_a_show_without_episodes(
    pool: PgPool,
) {
    let env = env(pool).await;
    let pool = env.pool();
    let old = store(pool, "https://f.invalid/old", "Old", None).await;
    let new = store(pool, "https://f.invalid/new", "New", None).await;
    let empty = store(pool, "https://f.invalid/empty", "Empty", None).await;
    add_episodes(
        pool,
        old,
        &[
            ("pl_o1", datetime!(2026-01-01 00:00 UTC)),
            ("pl_o2", datetime!(2026-03-01 00:00 UTC)),
        ],
    )
    .await;
    add_episodes(pool, new, &[("pl_n1", datetime!(2026-10-01 00:00 UTC))]).await;
    for show in [old, new, empty] {
        podcasts::subscribe(pool, env.user, show).await.unwrap();
    }
    let res = env.get("/api/podcasts").await;
    res.assert_status_ok();
    let latest: std::collections::BTreeMap<String, Value> = res
        .json::<Vec<Value>>()
        .into_iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_string(),
                p["latestEpisodeAt"].clone(),
            )
        })
        .collect();
    assert_eq!(
        latest,
        std::collections::BTreeMap::from([
            (old.to_string(), json!("2026-03-01T00:00:00.000Z")),
            (new.to_string(), json!("2026-10-01T00:00:00.000Z")),
            (empty.to_string(), Value::Null),
        ])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn followed_list_is_newest_follow_first_with_five_newest_episodes_and_progress(pool: PgPool) {
    let env = env(pool).await;
    let pool = env.pool();
    let first = store(pool, "https://f.invalid/first", "First", None).await;
    let second = store(pool, "https://f.invalid/second", "Second", None).await;
    let eps: Vec<(String, OffsetDateTime)> = (1..=7).map(|n| (format!("e{n}"), day(n))).collect();
    let eps: Vec<(&str, OffsetDateTime)> = eps.iter().map(|(g, t)| (g.as_str(), *t)).collect();
    add_episodes(pool, first, &eps).await;
    podcasts::subscribe(pool, env.user, first).await.unwrap();
    podcasts::subscribe(pool, env.user, second).await.unwrap();
    sqlx::query("UPDATE podcast_subscriptions SET subscribed_at = now() - interval '1 day' WHERE podcast_id = $1")
        .bind(first)
        .execute(pool)
        .await
        .unwrap();
    progress(pool, env.user, "e7", 300.0, 600.0, false).await;
    let other = common::user_with_token(&env.state, "other").await.0;
    progress(pool, other.id, "e6", 600.0, 600.0, true).await;

    let list = env.get("/api/podcasts").await.json::<Vec<Value>>();
    assert_eq!(
        list.iter().map(|p| p["title"].clone()).collect::<Vec<_>>(),
        [json!("Second"), json!("First")]
    );
    let first_json = &list[1];
    let e7 = episode_id(pool, "e7").await;
    let last_played = first_json["episodes"][0]["progress"]["lastPlayedAt"].clone();
    assert!(last_played.as_str().unwrap().ends_with('Z'));
    assert_eq!(
        first_json,
        &json!({
            "id": first.to_string(), "title": "First", "author": "Host", "description": "About it",
            "coverUrl": "https://feed.example/cover.jpg", "episodeCount": 0,
            "autoDownloadEpisodes": false, "latestEpisodeAt": "2026-10-07T06:00:00.000Z",
            "episodes": [
                {"id": e7.to_string(), "title": "e7", "description": "about e7", "duration": 60,
                 "publishedAt": "2026-10-07T06:00:00.000Z", "coverUrl": "https://a.invalid/e7.jpg",
                 "progress": {"currentTime": 300, "progress": 50, "isFinished": false,
                              "lastPlayedAt": last_played}},
                {"id": episode_id(pool, "e6").await.to_string(), "title": "e6", "description": "about e6",
                 "duration": 60, "publishedAt": "2026-10-06T06:00:00.000Z",
                 "coverUrl": "https://a.invalid/e6.jpg", "progress": null},
                {"id": episode_id(pool, "e5").await.to_string(), "title": "e5", "description": "about e5",
                 "duration": 60, "publishedAt": "2026-10-05T06:00:00.000Z",
                 "coverUrl": "https://a.invalid/e5.jpg", "progress": null},
                {"id": episode_id(pool, "e4").await.to_string(), "title": "e4", "description": "about e4",
                 "duration": 60, "publishedAt": "2026-10-04T06:00:00.000Z",
                 "coverUrl": "https://a.invalid/e4.jpg", "progress": null},
                {"id": episode_id(pool, "e3").await.to_string(), "title": "e3", "description": "about e3",
                 "duration": 60, "publishedAt": "2026-10-03T06:00:00.000Z",
                 "coverUrl": "https://a.invalid/e3.jpg", "progress": null},
            ]
        })
    );
    // Another user follows nothing.
    let other_token =
        env.state
            .jwt
            .issue_access(other.id, &other.username, &other.role, other.token_version);
    let theirs = env
        .server
        .get("/api/podcasts")
        .authorization_bearer(&other_token)
        .await;
    assert_eq!(theirs.json::<Value>(), json!([]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn progress_percent_capped_at_100(pool: PgPool) {
    let env = env(pool).await;
    let pool = env.pool();
    let show = store(pool, "https://f.invalid/p", "P", None).await;
    add_episodes(
        pool,
        show,
        &[("over", day(3)), ("zero", day(2)), ("half", day(1))],
    )
    .await;
    podcasts::subscribe(pool, env.user, show).await.unwrap();
    progress(pool, env.user, "over", 700.0, 600.0, false).await;
    progress(pool, env.user, "zero", 30.5, 0.0, false).await;
    progress(pool, env.user, "half", 150.0, 600.0, false).await;
    let list = env.get("/api/podcasts").await.json::<Vec<Value>>();
    let got: Vec<(Value, Value)> = list[0]["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["progress"]["currentTime"].clone(),
                e["progress"]["progress"].clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (json!(700), json!(100)),
            (json!(30.5), json!(0)),
            (json!(150), json!(25))
        ]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unfollowing_a_show_forgets_that_it_was_playing_so_no_device_restores_it_other_playback_stays(
    pool: PgPool,
) {
    let env = env(pool).await;
    let pool = env.pool();
    let show = store(pool, "https://f.invalid/next", "Next Show", None).await;
    let other = store(pool, "https://f.invalid/other", "Other", None).await;
    add_episodes(pool, show, &[("nx_15", day(8))]).await;
    add_episodes(pool, other, &[("nx_other", day(9))]).await;
    podcasts::subscribe(pool, env.user, show).await.unwrap();
    podcasts::subscribe(pool, env.user, other).await.unwrap();
    progress(pool, env.user, "nx_15", 10.0, 600.0, false).await;
    progress(pool, env.user, "nx_other", 10.0, 600.0, false).await;
    let nx_15 = episode_id(pool, "nx_15").await;
    sqlx::query(
        "INSERT INTO podcast_downloads (user_id, episode_id, local_path, file_size_mb)
         VALUES ($1, $2, 'podcast-audio/x.audio', 1.5)",
    )
    .bind(env.user)
    .bind(nx_15)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO playback_state (user_id, playback_type, podcast_id) VALUES ($1, 'podcast', $2)",
    )
    .bind(env.user)
    .bind(format!("{show}:{nx_15}"))
    .execute(pool)
    .await
    .unwrap();

    let res = env
        .delete(&format!("/api/podcasts/{other}/unsubscribe"))
        .await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({"success": true, "message": "Unsubscribed successfully"})
    );
    assert_eq!(count(pool, "SELECT count(*) FROM playback_state").await, 1);

    env.delete(&format!("/api/podcasts/{show}/unsubscribe"))
        .await
        .assert_status_ok();
    assert_eq!(count(pool, "SELECT count(*) FROM playback_state").await, 0);
    assert_eq!(
        count(pool, "SELECT count(*) FROM podcast_progress").await,
        0
    );
    assert_eq!(
        count(pool, "SELECT count(*) FROM podcast_downloads").await,
        0
    );
    assert_eq!(
        count(pool, "SELECT count(*) FROM podcast_subscriptions").await,
        0
    );
    // The shows themselves stay.
    assert_eq!(count(pool, "SELECT count(*) FROM podcasts").await, 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unsubscribe_unknown_is_404(pool: PgPool) {
    let env = env(pool).await;
    let show = store(env.pool(), "https://f.invalid/x", "X", None).await;
    for id in [
        show.to_string(),
        Uuid::now_v7().to_string(),
        "not-a-uuid".into(),
    ] {
        let res = env.delete(&format!("/api/podcasts/{id}/unsubscribe")).await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(
            res.json::<Value>(),
            json!({"error": "Not subscribed to this podcast"})
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requires_auth(pool: PgPool) {
    let env = env(pool).await;
    env.server
        .get("/api/podcasts")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    env.server
        .post("/api/podcasts/subscribe")
        .json(&json!({"feedUrl": env.host.url("/race.xml")}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    env.server
        .delete(&format!("/api/podcasts/{}/unsubscribe", Uuid::now_v7()))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apple_unavailable_when_subscribing_is_a_502_not_a_404(pool: PgPool) {
    let env = env(pool).await;
    let res = env.subscribe(json!({"itunesId": "888"})).await;
    res.assert_status(StatusCode::BAD_GATEWAY);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "The show's feed or Apple Podcasts did not answer; try again shortly"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn malformed_apple_reply_uses_the_apple_failure_message(pool: PgPool) {
    let env = env(pool).await;
    let res = env.subscribe(json!({"itunesId": "889"})).await;
    res.assert_status(StatusCode::BAD_GATEWAY);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "Apple Podcasts did not answer; try again shortly"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unique_apple_id_race_falls_back_to_the_stored_show(pool: PgPool) {
    let env = env(pool).await;
    let stored = store(
        env.pool(),
        &env.host.url("/real.xml"),
        "Stored",
        Some("555"),
    )
    .await;
    let res = env
        .subscribe(json!({"feedUrl": env.host.url("/real.xml/")}))
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["podcast"]["id"], stored.to_string());
    assert_eq!(count(env.pool(), "SELECT count(*) FROM podcasts").await, 1);
}
