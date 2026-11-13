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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_episode_import_rolls_back_show(pool: PgPool) {
    let env = env(pool.clone()).await;
    sqlx::query("CREATE FUNCTION reject_episode() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected episode failure'; END $$")
        .execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_episode BEFORE INSERT ON podcast_episodes FOR EACH ROW EXECUTE FUNCTION reject_episode()")
        .execute(&pool).await.unwrap();
    let url = env.host.url("/failure.xml");
    let res = env.subscribe(json!({"feedUrl": url})).await;
    assert!(res.status_code().is_server_error());
    let show = podcasts::show_by_feed_or_itunes(&pool, &url, None)
        .await
        .unwrap();
    assert!(show.is_none(), "failed import left a visible show");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cancelling_category_lookup_never_commits_an_uncategorized_import(pool: PgPool) {
    let env = env(pool.clone()).await;
    let url = env.host.url("/cancel.xml");
    {
        let request = env.subscribe(json!({"feedUrl": url}));
        tokio::pin!(request);
        tokio::select! {
            _ = env.host.category_requested.notified() => {},
            _ = &mut request => panic!("import completed before category lookup"),
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("category lookup did not start"),
        }
        assert!(
            podcasts::show_by_feed_or_itunes(&pool, &url, None)
                .await
                .unwrap()
                .is_none(),
            "show committed before its feed categories were resolved"
        );
    }
    assert!(
        podcasts::show_by_feed_or_itunes(&pool, &url, None)
            .await
            .unwrap()
            .is_none()
    );
    env.host.category_release.notify_waiters();
    env.host.category_release.notify_one();
    let result = env.subscribe(json!({"feedUrl": url})).await;
    assert_eq!(result.status_code(), StatusCode::OK);
    let show = podcasts::show_by_feed_or_itunes(&pool, &url, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(show.category_ids, vec![1310]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_all_without_subscriptions_returns_empty_message_green(pool: PgPool) {
    let env = env(pool).await;
    let response = env.post("/api/podcasts/refresh-all", json!({})).await;
    assert_eq!(response.status_code(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>(),
        json!({"queued": 0, "message": "No subscribed podcasts"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn manual_refresh_unknown_podcast_is_not_found_green(pool: PgPool) {
    let env = env(pool).await;
    let response = env
        .get(&format!("/api/podcasts/{}/refresh", Uuid::now_v7()))
        .await;
    assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    assert_eq!(
        response.json::<Value>(),
        json!({"error": "Podcast not found"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_routes_require_authentication(pool: PgPool) {
    let env = env(pool).await;
    let response = env.server.post("/api/podcasts/refresh-all").await;
    assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    let response = env
        .server
        .get(&format!("/api/podcasts/{}/refresh", Uuid::now_v7()))
        .await;
    assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
}

// ---- Task 9: manual refresh of a followed show, refresh-all, the refresh fence ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn manual_refresh_succeeds_green(pool: PgPool) {
    let env = env(pool.clone()).await;
    let feed = env.host.url("/feed.xml");
    let show = store(&pool, &feed, "Old Title", None).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let res = env.get(&format!("/api/podcasts/{show}/refresh")).await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["success"], json!(true));
    assert_eq!(body["newEpisodesCount"], json!(1));
    assert_eq!(body["totalEpisodes"], json!(1));
    assert_eq!(body["message"], json!("Found 1 new episodes"));
    // The feed consumed: metadata follows the feed, episode stored, no failure recorded.
    let show = podcasts::show_by_id(&pool, show).await.unwrap().unwrap();
    assert_eq!(show.title, "Race Show");
    assert_eq!(show.episode_count, 1);
    assert_eq!(show.refresh_error, None);
    assert_eq!(show.refresh_failed_at, None);
    assert_eq!(show.refresh_failures, 0);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM podcast_episodes WHERE guid = 'race-1'"
        )
        .await,
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn manual_refresh_feed_gone_is_404_green(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, &env.host.url("/gone.xml"), "Gone", None).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let res = env.get(&format!("/api/podcasts/{show}/refresh")).await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        res.json::<Value>()["error"],
        json!("The show's feed is gone")
    );
    // A typed feed failure must not wipe the show's metadata; a gone feed is recorded as a
    // failure so the admin sees it in the enrichment failures list (review finding).
    let show = podcasts::show_by_id(&pool, show).await.unwrap().unwrap();
    assert_eq!(show.title, "Gone");
    assert_eq!(
        show.refresh_error.as_deref(),
        Some("The show's feed is gone")
    );
    assert_eq!(show.refresh_failures, 1);
}

/// Upstream wraps every feed read failure (a page that is not RSS, a host that is down) in
/// `FeedReadError`, which `answerUpstreamFailure` answers as 502 "down".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn manual_refresh_unreadable_or_down_feed_is_502_green(pool: PgPool) {
    let env = env(pool.clone()).await;
    for path in ["/page.xml", "/down.xml"] {
        let show = store(&pool, &env.host.url(path), "Show", None).await;
        podcasts::subscribe(&pool, env.user, show).await.unwrap();
        let res = env.get(&format!("/api/podcasts/{show}/refresh")).await;
        res.assert_status(StatusCode::BAD_GATEWAY);
        assert_eq!(
            res.json::<Value>()["error"],
            json!("The show's feed did not answer; try again shortly"),
            "{path}"
        );
        let show = podcasts::show_by_id(&pool, show).await.unwrap().unwrap();
        assert_eq!(show.refresh_failures, 1, "{path}");
    }
}

/// A 4xx other than 404/429 from the feed's host (`Absent`): upstream's `upstreamStatusFailure`
/// calls it "missing", so `answerUpstreamFailure` answers 404 with the route's one missing text
/// (D-205). The failure is still recorded on the show, as for `Gone`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn manual_refresh_feed_refused_is_404_green(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, &env.host.url("/forbidden.xml"), "Forbidden", None).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let res = env.get(&format!("/api/podcasts/{show}/refresh")).await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "The show's feed is gone"})
    );
    let show = podcasts::show_by_id(&pool, show).await.unwrap().unwrap();
    assert_eq!(show.refresh_failures, 1);
    assert!(show.refresh_error.is_some());
    assert!(show.refresh_failed_at.is_some());
}

/// An address the SSRF guard refuses (`Blocked`): upstream's `isBlockedFetch` makes it "missing",
/// answered 404 with the same body as a gone feed (D-205).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn manual_refresh_feed_blocked_ssrf_is_404_green(pool: PgPool) {
    // The feed host answers loopback only; the show's url points at a private
    // address outside the allowed bases, so the hop is refused here.
    let env = env_with(pool.clone(), |cfg| cfg.allow_private_images = false).await;
    let feed = "http://10.9.9.9:8080/feed.xml";
    let show = store(&pool, feed, "Blocked", None).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let res = env.get(&format!("/api/podcasts/{show}/refresh")).await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "The show's feed is gone"})
    );
    let show = podcasts::show_by_id(env.pool(), show)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(show.refresh_failures, 1);
    assert!(show.refresh_error.is_some());
    assert!(show.refresh_failed_at.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_all_with_mixed_followed_shows_green(pool: PgPool) {
    let env = env(pool.clone()).await;
    // One followed show with a good feed; one with a down feed. refresh-all must
    // best-effort enqueue both and answer from the followed set only.
    let good = store(&pool, &env.host.url("/race.xml"), "Good", None).await;
    let bad = store(&pool, &env.host.url("/down.xml"), "Bad", None).await;
    podcasts::subscribe(&pool, env.user, good).await.unwrap();
    podcasts::subscribe(&pool, env.user, bad).await.unwrap();
    let res = env.post("/api/podcasts/refresh-all", json!({})).await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["queued"], json!(2));
    assert_eq!(body["total"], json!(2));
    assert_eq!(body["message"], json!("Queued 2 podcasts for refresh"));
    let followed: i64 = count(
        env.pool(),
        &format!(
            "SELECT count(*) FROM podcast_subscriptions WHERE user_id = '{}'",
            env.user
        ),
    )
    .await;
    assert_eq!(followed, 2);
    let queued: i64 = count(
        env.pool(),
        "SELECT count(*) FROM jobs WHERE queue = 'podcasts' AND state = 'queued'",
    )
    .await;
    assert_eq!(queued, 2);
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT (payload ->> 'key')::text FROM jobs WHERE queue = 'podcasts' AND state = 'queued'",
    )
    .fetch_all(env.pool())
    .await
    .unwrap();
    assert!(
        keys.contains(&format!("podcast-{good}")),
        "queued key {keys:?}"
    );
    assert!(
        keys.contains(&format!("podcast-{bad}")),
        "queued key {keys:?}"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_one_superseded_by_concurrent_refresh_returns_409_green(pool: PgPool) {
    // The refresh fence is a one-writer barrier: `refresh_one` claims a fresh
    // `refresh_token` with an unconditional UPDATE before it ever touches the
    // feed, so two concurrent refreshes of the same followed show race at that
    // claim; whichever claims last invalidates the other's token. `/race.xml`
    // answers only once two fetches are waiting on it, so both requests are
    // still in flight (claim already done, feed not yet parsed) when the other
    // claims — the loser's later fence recheck finds it has been superseded.
    let env = env(pool.clone()).await;
    let show = store(&pool, &env.host.url("/race.xml"), "Race Show", None).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let path = format!("/api/podcasts/{show}/refresh");
    let (a, b) = tokio::join!(env.get(&path), env.get(&path));
    let mut codes = [a.status_code(), b.status_code()];
    codes.sort_unstable();
    assert_eq!(codes, [StatusCode::OK, StatusCode::CONFLICT]);
    // The fence did its job: exactly one refresh committed; the other was
    // fenced (409) and stored nothing new.
    let episodes = count(
        &pool,
        &format!("SELECT count(*) FROM podcast_episodes WHERE podcast_id = '{show}'"),
    )
    .await;
    assert_eq!(episodes, 1);
    let show = podcasts::show_by_id(&pool, show).await.unwrap().unwrap();
    assert_eq!(show.title, "Race Show");
    assert_eq!(show.refresh_error, None);
    assert_eq!(show.refresh_failures, 0);
}

const STORED_ITUNES: &str = "1450241676";

const FEED: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Race Show</title><description>d</description>
<item><title>Ep 1</title><guid>race-1</guid><pubDate>Mon, 13 Oct 2026 10:00:00 GMT</pubDate>
<enclosure url="https://cdn.example.invalid/1.mp3" length="1000" type="audio/mpeg"/></item></channel></rss>"#;

/// A feed disclosing AI content for the show, with one episode declared human (D-200).
const AI_FEED: &str = r#"<?xml version="1.0"?><rss version="2.0" xmlns:podcast="https://podcastindex.org/namespace/1.0"><channel><title>Synthetic Show</title>
<podcast:txt purpose="ai-content">true</podcast:txt>
<item><title>Made by people</title><guid>ai-human</guid><pubDate>Mon, 13 Oct 2026 10:00:00 GMT</pubDate><podcast:txt purpose="ai-content">false</podcast:txt>
<enclosure url="https://cdn.example.invalid/h.mp3" length="1000" type="audio/mpeg"/></item>
<item><title>Generated</title><guid>ai-gen</guid><pubDate>Sun, 12 Oct 2026 10:00:00 GMT</pubDate>
<enclosure url="https://cdn.example.invalid/g.mp3" length="1000" type="audio/mpeg"/></item></channel></rss>"#;

/// A loopback host serving feeds and Apple's lookup: show 555 is the feed at `/real.xml`.
/// `/race.xml` answers only once two requests for it are waiting, so two follows of a new show
/// really overlap.
struct FeedHost {
    base: String,
    task: tokio::task::JoinHandle<()>,
    category_requested: Arc<tokio::sync::Notify>,
    category_release: Arc<tokio::sync::Notify>,
}

impl FeedHost {
    async fn start() -> FeedHost {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let race = Arc::new(tokio::sync::Barrier::new(2));
        let category_requested = Arc::new(tokio::sync::Notify::new());
        let signal = category_requested.clone();
        let category_release = Arc::new(tokio::sync::Notify::new());
        let release = category_release.clone();
        let gated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let me = base.clone();
        let app = Router::new().fallback(move |uri: Uri| {
            let (me, race) = (me.clone(), race.clone());
            let signal = signal.clone();
            let release = release.clone();
            let gated = gated.clone();
            async move {
                if uri.path() == "/cancel.xml" {
                    gated.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                if uri.path().ends_with("/ws/genres")
                    && gated.load(std::sync::atomic::Ordering::SeqCst)
                {
                    signal.notify_one();
                    release.notified().await;
                    return axum::Json(json!({"26":{"subgenres":{"1310":{"name":"Music"}}}}))
                        .into_response();
                }
                answer(&me, &race, &uri).await
            }
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        FeedHost {
            base,
            task,
            category_requested,
            category_release,
        }
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
        "/cancel.xml" => (
            [(header::CONTENT_TYPE, "application/rss+xml")],
            FEED.replace(
                "<rss version=\"2.0\">",
                "<rss version=\"2.0\" xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\">",
            )
            .replace(
                "<title>Race Show</title>",
                "<title>Race Show</title><itunes:category text=\"Music\"/>",
            ),
        )
            .into_response(),
        "/ai.xml" => ([(header::CONTENT_TYPE, "application/rss+xml")], AI_FEED).into_response(),
        "/down.xml" => (StatusCode::SERVICE_UNAVAILABLE, "maintenance").into_response(),
        "/gone.xml" => (StatusCode::NOT_FOUND, "not here").into_response(),
        "/forbidden.xml" => (StatusCode::FORBIDDEN, "forbidden").into_response(),
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

async fn save_progress(env: &Env, path: &str, body: Value) -> axum_test::TestResponse {
    env.post(path, body).await
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

    async fn post(&self, path: &str, body: Value) -> axum_test::TestResponse {
        self.server
            .post(path)
            .authorization_bearer(&self.token)
            .json(&body)
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
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
async fn concurrent_import_recovers_after_episode_rollback(pool: PgPool) {
    use std::time::Duration;

    let env = env(pool.clone()).await;
    // nextval survives rollback, so only the first importer fails.
    sqlx::raw_sql("CREATE SEQUENCE race_attempt; CREATE FUNCTION race_episode() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('race_attempt') = 1 THEN PERFORM pg_advisory_xact_lock(83472819); RAISE EXCEPTION 'injected first import failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER race_episode BEFORE INSERT ON podcast_episodes FOR EACH ROW EXECUTE FUNCTION race_episode()")
        .execute(&pool).await.unwrap();
    let mut gate = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(83472819)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let url = env.host.url("/race.xml");
    let control = async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (advisory, transaction): (i64, i64) = sqlx::query_as(
                    "SELECT count(*) FILTER (WHERE wait_event = 'advisory'), count(*) FILTER (WHERE wait_event = 'transactionid') FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
                ).fetch_one(&pool).await.unwrap();
                if advisory >= 1 && transaction >= 1 { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("importers did not reach advisory and unique-index transaction waits");
        sqlx::query("SELECT pg_advisory_unlock(83472819)")
            .execute(&mut *gate)
            .await
            .unwrap();
    };
    let (a, b, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(
            env.subscribe(json!({"feedUrl": url})),
            env.subscribe(json!({"feedUrl": url})),
            control
        )
    })
    .await
    .expect("concurrent imports did not finish");
    let (failed, winner) = if a.status_code().is_server_error() {
        (a, b)
    } else {
        (b, a)
    };
    assert!(failed.status_code().is_server_error());
    winner.assert_status_ok();
    let id: Uuid = winner.json::<Value>()["podcast"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let stored = podcasts::show_by_feed_or_itunes(&pool, &url, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.id, id);
    assert_eq!(stored.episode_count, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM podcasts").await, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM podcast_episodes").await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM podcast_subscriptions").await,
        1
    );
    assert!(podcasts::is_subscribed(&pool, env.user, id).await.unwrap());
    let episode: (Uuid, String) = sqlx::query_as("SELECT podcast_id, guid FROM podcast_episodes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(episode, (id, "race-1".to_string()));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_follow_conflict_releases_single_connection(pool: PgPool) {
    let small = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let env = env(small).await;
    let url = env.host.url("/race.xml");
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            env.subscribe(json!({"feedUrl": url})),
            env.subscribe(json!({"feedUrl": url}))
        )
    })
    .await
    .expect("conflicting import starved the single-connection pool");
    a.assert_status_ok();
    b.assert_status_ok();
    assert_eq!(
        a.json::<Value>()["podcast"]["id"],
        b.json::<Value>()["podcast"]["id"]
    );
    assert_eq!(count(env.pool(), "SELECT count(*) FROM podcasts").await, 1);
    assert_eq!(
        count(env.pool(), "SELECT count(*) FROM podcast_episodes").await,
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn survives_two_follows_of_a_new_show_at_once_one_show_stored_both_answered(pool: PgPool) {
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
    // The AI labels (Task 8) are checked on their own; upstream's keys are compared here.
    assert_eq!(list[1]["aiLabel"], "unknown");
    assert_eq!(list[1]["episodes"][0]["aiLabel"], "unknown");
    let mut first_json = list[1].clone();
    strip_ai(&mut first_json);
    let first_json = &first_json;
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
    let env = env(pool.clone()).await;
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
async fn saves_a_position_past_the_stored_length(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, "https://f.invalid/p", "P", None).await;
    add_episodes(&pool, show, &[("past", day(1))]).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let episode = episode_id(&pool, "past").await;
    let path = format!("/api/podcasts/{show}/episodes/{episode}/progress");
    let res = env
        .post(&path, json!({"currentTime": 650, "duration": 700}))
        .await;
    res.assert_status_ok();
    let body: Value = res.json::<Value>();
    assert_eq!(body["progress"]["currentTime"], json!(650));
    assert_eq!(body["progress"]["isFinished"], json!(false));
    let stored: f64 = sqlx::query_scalar(
        "SELECT \"current_time\" FROM podcast_progress WHERE user_id = $1 AND episode_id = $2",
    )
    .bind(env.user)
    .bind(episode)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 650.0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn accepts_numbers_sent_as_strings_and_a_missing_duration_keeps_the_stored_one(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, "https://f.invalid/p", "P", None).await;
    add_episodes(&pool, show, &[("s", day(1))]).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let episode = episode_id(&pool, "s").await;
    let path = format!("/api/podcasts/{show}/episodes/{episode}/progress");
    env.post(&path, json!({"currentTime": 30, "duration": 700}))
        .await
        .assert_status_ok();
    let res = env.post(&path, json!({"currentTime": "120"})).await;
    res.assert_status_ok();
    let stored: (f64, f64) = podcasts::progress(&pool, env.user, episode)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.0, 120.0);
    assert_eq!(stored.1, 700.0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_404_for_an_episode_that_does_not_exist(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, "https://f.invalid/p", "P", None).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let nope = Uuid::now_v7();
    let path = format!("/api/podcasts/{show}/episodes/{nope}/progress");
    let res = env
        .post(&path, json!({"currentTime": 10, "duration": 20}))
        .await;
    res.assert_status_not_found();
    assert_eq!(res.json::<Value>()["error"], json!("Episode not found"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_400_without_a_position(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, "https://f.invalid/p", "P", None).await;
    add_episodes(&pool, show, &[("s", day(1))]).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let episode = episode_id(&pool, "s").await;
    let path = format!("/api/podcasts/{show}/episodes/{episode}/progress");
    let res = env.post(&path, json!({"duration": 20})).await;
    res.assert_status_bad_request();
    assert_eq!(
        res.json::<Value>()["error"],
        json!("currentTime (seconds) is required")
    );
}

async fn is_finished(pool: &PgPool, user: Uuid, episode: Uuid) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT is_finished FROM podcast_progress WHERE user_id = $1 AND episode_id = $2",
    )
    .bind(user)
    .bind(episode)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn counts_an_episode_finished_from_95_percent_of_its_length_on_whatever_the_client_says_and_unfinished_again_when_replayed_from_early_on(
    pool: PgPool,
) {
    let env = env(pool.clone()).await;
    let show = store(&pool, "https://f.invalid/p", "P", None).await;
    add_episodes(&pool, show, &[("f", day(1))]).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let episode = episode_id(&pool, "f").await;
    let path = format!("/api/podcasts/{show}/episodes/{episode}/progress");
    save_progress(&env, &path, json!({"currentTime": 560, "duration": 600}))
        .await
        .assert_status_ok();
    assert_eq!(is_finished(&pool, env.user, episode).await, json!(false));
    save_progress(&env, &path, json!({"currentTime": 570, "duration": 600}))
        .await
        .assert_status_ok();
    assert_eq!(is_finished(&pool, env.user, episode).await, json!(true));
    save_progress(&env, &path, json!({"currentTime": 30, "duration": 600}))
        .await
        .assert_status_ok();
    assert_eq!(is_finished(&pool, env.user, episode).await, json!(false));
    save_progress(&env, &path, json!({"currentTime": 590}))
        .await
        .assert_status_ok();
    assert_eq!(is_finished(&pool, env.user, episode).await, json!(false));
    save_progress(&env, &path, json!({"currentTime": 600, "isFinished": true}))
        .await
        .assert_status_ok();
    assert_eq!(is_finished(&pool, env.user, episode).await, json!(true));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn coercion_like_zod(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, "https://f.invalid/p", "P", None).await;
    add_episodes(&pool, show, &[("c", day(1))]).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let episode = episode_id(&pool, "c").await;
    let path = format!("/api/podcasts/{show}/episodes/{episode}/progress");
    let res = save_progress(&env, &path, json!({"currentTime": "12"})).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["progress"]["currentTime"], json!(12));
    let res = save_progress(&env, &path, json!({"currentTime": null})).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["progress"]["currentTime"], json!(0));
    let res = save_progress(&env, &path, json!({"currentTime": []})).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["progress"]["currentTime"], json!(0));
    let res = save_progress(&env, &path, json!({})).await;
    res.assert_status_bad_request();
    assert_eq!(
        res.json::<Value>()["error"],
        json!("currentTime (seconds) is required")
    );
    let res = save_progress(&env, &path, json!({"currentTime": -1})).await;
    res.assert_status_bad_request();
    let res = save_progress(&env, &path, json!({"currentTime": "Infinity"})).await;
    res.assert_status_bad_request();
    let res = save_progress(
        &env,
        &path,
        json!({"currentTime": 10, "isFinished": "true"}),
    )
    .await;
    res.assert_status_bad_request();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_is_idempotent(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, "https://f.invalid/p", "P", None).await;
    add_episodes(&pool, show, &[("d", day(1))]).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let episode = episode_id(&pool, "d").await;
    let path = format!("/api/podcasts/{show}/episodes/{episode}/progress");
    env.post(&path, json!({"currentTime": 100, "duration": 200}))
        .await
        .assert_status_ok();
    env.delete(&path).await.assert_status_ok();
    env.delete(&path).await.assert_status_ok();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM podcast_progress WHERE user_id = $1 AND episode_id = $2"
        )
        .bind(env.user)
        .bind(episode)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        env.delete(&path).await.json::<Value>(),
        json!({"success": true, "message": "Progress removed"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn episode_of_any_show_id_in_path(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(&pool, "https://f.invalid/p", "P", None).await;
    let other = store(&pool, "https://f.invalid/other", "Other", None).await;
    add_episodes(&pool, show, &[("e", day(1))]).await;
    podcasts::subscribe(&pool, env.user, show).await.unwrap();
    let episode = episode_id(&pool, "e").await;
    // The podcast id in the path is ignored (upstream follows the same convention).
    let path = format!("/api/podcasts/{other}/episodes/{episode}/progress");
    env.post(&path, json!({"currentTime": 10, "duration": 100}))
        .await
        .assert_status_ok();
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unfollowing_a_show_forgets_that_it_was_playing_so_no_device_restores_it_other_playback_stays(
    pool: PgPool,
) {
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
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
    let env = env(pool.clone()).await;
    let res = env.subscribe(json!({"itunesId": "888"})).await;
    res.assert_status(StatusCode::BAD_GATEWAY);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "The show's feed or Apple Podcasts did not answer; try again shortly"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn malformed_apple_reply_uses_the_apple_failure_message(pool: PgPool) {
    let env = env(pool.clone()).await;
    let res = env.subscribe(json!({"itunesId": "889"})).await;
    res.assert_status(StatusCode::BAD_GATEWAY);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "Apple Podcasts did not answer; try again shortly"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unique_apple_id_race_falls_back_to_the_stored_show(pool: PgPool) {
    let env = env(pool.clone()).await;
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

// Task 7: show detail and bounded episode pages.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn episode_pages_are_bounded_report_the_true_total_and_order_on_the_server(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(env.pool(), "https://f.invalid/pages", "Paged", None).await;
    let episodes: Vec<NewEpisode> = (0..120)
        .map(|n| episode(&format!("page-{n:03}"), day(1) + time::Duration::seconds(n)))
        .collect();
    podcasts::insert_episodes(env.pool(), show, &episodes)
        .await
        .unwrap();
    podcasts::subscribe(env.pool(), env.user, show)
        .await
        .unwrap();
    for (suffix, key) in [("", "episodeTotal"), ("/episodes", "total")] {
        let res = env
            .get(&format!(
                "/api/podcasts/{show}{suffix}?limit=2&offset=3&order=oldest"
            ))
            .await;
        res.assert_status_ok();
        let body = res.json::<Value>();
        assert_eq!(body[key], 120);
        assert_eq!(body["episodes"].as_array().unwrap().len(), 2);
        assert_eq!(body["episodes"][0]["title"], "page-003");
        assert_eq!(body["episodes"][1]["title"], "page-004");
        assert_eq!(body["episodes"][0]["isDownloaded"], false);
        assert_eq!(body["episodes"][0]["progress"], Value::Null);
    }
    let res = env
        .get(&format!("/api/podcasts/{show}"))
        .await
        .json::<Value>();
    assert_eq!(res["episodes"].as_array().unwrap().len(), 50);
    assert_eq!(res["episodes"][0]["title"], "page-119");
    assert_eq!(res["episodeLimit"], 50);
    assert_eq!(res["episodeOffset"], 0);
    let res = env
        .get(&format!("/api/podcasts/{show}/episodes?limit=500"))
        .await
        .json::<Value>();
    assert_eq!(res["limit"], 200);
    assert_eq!(res["episodes"].as_array().unwrap().len(), 120);
    for bad in ["abc", "-3", "0"] {
        let res = env
            .get(&format!(
                "/api/podcasts/{show}/episodes?limit={bad}&offset=-5"
            ))
            .await
            .json::<Value>();
        assert_eq!(
            (res["limit"].clone(), res["offset"].clone()),
            (json!(50), json!(0))
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn next_episode_skips_finished_resumes_progress_and_works_without_subscription(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(env.pool(), "https://f.invalid/next2", "Next", None).await;
    add_episodes(
        env.pool(),
        show,
        &[
            ("nx1", day(1)),
            ("nx2", day(8)),
            ("nx3", day(15)),
            ("nx4", day(22)),
        ],
    )
    .await;
    progress(env.pool(), env.user, "nx2", 600.0, 600.0, true).await;
    progress(env.pool(), env.user, "nx3", 120.0, 600.0, false).await;
    let id1 = episode_id(env.pool(), "nx1").await;
    let id3 = episode_id(env.pool(), "nx3").await;
    let id4 = episode_id(env.pool(), "nx4").await;
    let res = env
        .get(&format!("/api/podcasts/{show}/episodes/{id1}/next"))
        .await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["podcast"]["id"], show.to_string());
    assert_eq!(body["episode"]["id"], id3.to_string());
    assert_eq!(body["episode"]["progress"]["currentTime"], 120);
    assert_eq!(body["episode"]["progress"]["progress"], 20);
    let end = env
        .get(&format!("/api/podcasts/{show}/episodes/{id4}/next"))
        .await;
    assert_eq!(end.json::<Value>(), json!({"episode": null}));
    for (s, e) in [(Uuid::now_v7(), id1), (show, Uuid::now_v7())] {
        let r = env
            .get(&format!("/api/podcasts/{s}/episodes/{e}/next"))
            .await;
        r.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(r.json::<Value>(), json!({"error": "Episode not found"}));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listening_lists_progress_and_newest_unplayed_only_for_followed_shows(pool: PgPool) {
    let env = env(pool.clone()).await;
    let a = store(env.pool(), "https://f.invalid/listen-a", "A", None).await;
    let b = store(env.pool(), "https://f.invalid/listen-b", "B", None).await;
    let c = store(env.pool(), "https://f.invalid/listen-c", "C", None).await;
    let other = store(env.pool(), "https://f.invalid/listen-other", "Other", None).await;
    add_episodes(
        env.pool(),
        a,
        &[("a-new", day(10)), ("a-half", day(5)), ("a-done", day(1))],
    )
    .await;
    add_episodes(env.pool(), b, &[("b-new", day(11)), ("b-half", day(8))]).await;
    add_episodes(env.pool(), c, &[("c-new", day(9)), ("c-old", day(2))]).await;
    add_episodes(env.pool(), other, &[("other", day(12))]).await;
    for id in [a, b, c] {
        podcasts::subscribe(env.pool(), env.user, id).await.unwrap();
    }
    progress(env.pool(), env.user, "a-half", 300.0, 600.0, false).await;
    progress(env.pool(), env.user, "b-half", 60.0, 600.0, false).await;
    progress(env.pool(), env.user, "a-done", 600.0, 600.0, true).await;
    progress(env.pool(), env.user, "c-new", 600.0, 600.0, true).await;
    progress(env.pool(), env.user, "other", 60.0, 600.0, false).await;
    sqlx::query("UPDATE podcast_progress SET last_played_at = $1 WHERE episode_id = $2")
        .bind(day(13))
        .bind(episode_id(env.pool(), "a-half").await)
        .execute(env.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE podcast_progress SET last_played_at = $1 WHERE episode_id = $2")
        .bind(day(14))
        .bind(episode_id(env.pool(), "b-half").await)
        .execute(env.pool())
        .await
        .unwrap();
    let res = env.get("/api/podcasts/listening").await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    let titles = |key: &str| {
        body[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["episode"]["title"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(titles("inProgress"), ["b-half", "a-half"]);
    assert_eq!(body["inProgress"][1]["episode"]["progress"]["progress"], 50);
    assert_eq!(titles("newEpisodes"), ["b-new", "a-new"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn detail_requires_follow_and_survives_apple_down_with_user_progress_and_downloads(
    pool: PgPool,
) {
    let env = env(pool.clone()).await;
    let show = store(
        env.pool(),
        "https://f.invalid/detail",
        "Detail",
        Some("555"),
    )
    .await;
    add_episodes(env.pool(), show, &[("detail-ep", day(1))]).await;
    let ep = episode_id(env.pool(), "detail-ep").await;
    for path in [
        format!("/api/podcasts/{show}"),
        format!("/api/podcasts/{show}/episodes"),
    ] {
        let res = env.get(&path).await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(
            res.json::<Value>(),
            json!({"error":"Podcast not found or not subscribed"})
        );
    }
    podcasts::subscribe(env.pool(), env.user, show)
        .await
        .unwrap();
    sqlx::query("UPDATE podcasts SET category_ids='{1495}' WHERE id=$1")
        .bind(show)
        .execute(env.pool())
        .await
        .unwrap();
    progress(env.pool(), env.user, "detail-ep", 30.0, 60.0, false).await;
    sqlx::query("INSERT INTO podcast_downloads(user_id,episode_id,local_path,file_size_mb) VALUES ($1,$2,'test',1)")
        .bind(env.user).bind(ep).execute(env.pool()).await.unwrap();
    let res = env.get(&format!("/api/podcasts/{show}")).await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["genres"], json!([]));
    assert_eq!(body["categories"], json!([]));
    assert_eq!(body["isSubscribed"], true);
    assert_eq!(body["feedUrl"], "https://f.invalid/detail");
    assert_eq!(body["episodes"][0]["isDownloaded"], true);
    assert_eq!(body["episodes"][0]["progress"]["progress"], 50);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn next_and_episode_pages_break_equal_date_ties_by_id(pool: PgPool) {
    let env = env(pool.clone()).await;
    let show = store(env.pool(), "https://f.invalid/ties", "Ties", None).await;
    add_episodes(
        env.pool(),
        show,
        &[("tie-a", day(1)), ("tie-b", day(1)), ("tie-c", day(1))],
    )
    .await;
    podcasts::subscribe(env.pool(), env.user, show)
        .await
        .unwrap();
    let mut ids = vec![
        episode_id(env.pool(), "tie-a").await,
        episode_id(env.pool(), "tie-b").await,
        episode_id(env.pool(), "tie-c").await,
    ];
    ids.sort();
    let next = env
        .get(&format!("/api/podcasts/{show}/episodes/{}/next", ids[0]))
        .await
        .json::<Value>();
    assert_eq!(next["episode"]["id"], ids[1].to_string());
    let page = env
        .get(&format!("/api/podcasts/{show}/episodes?order=oldest"))
        .await
        .json::<Value>();
    let got: Vec<_> = page["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .collect();
    assert_eq!(got, ids);
    let page = env
        .get(&format!("/api/podcasts/{show}/episodes"))
        .await
        .json::<Value>();
    let got: Vec<_> = page["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .collect();
    ids.reverse();
    assert_eq!(got, ids);
}

// ---- Task 15: legacy cover redirects `/:id/cover`, `/episodes/:id/cover` (D-210) ----

mod cover {
    use super::*;

    fn location(res: &axum_test::TestResponse) -> String {
        res.headers()
            .get(axum::http::header::LOCATION)
            .expect("a redirect has a Location header")
            .to_str()
            .unwrap()
            .to_string()
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn show_cover_redirects_to_the_proxy(pool: PgPool) {
        let env = env(pool.clone()).await;
        let show = env.stored_show().await;
        let res = env.get(&format!("/api/podcasts/{show}/cover")).await;
        res.assert_status(StatusCode::FOUND);
        assert_eq!(
            location(&res),
            "/api/library/cover-art?url=https%3A%2F%2Ffeed.example%2Fcover.jpg"
        );
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn show_cover_without_an_image_is_404(pool: PgPool) {
        let env = env(pool.clone()).await;
        let show = env.stored_show().await;
        sqlx::query("UPDATE podcasts SET image_url = NULL WHERE id = $1")
            .bind(show)
            .execute(env.pool())
            .await
            .unwrap();
        let res = env.get(&format!("/api/podcasts/{show}/cover")).await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(res.json::<Value>()["error"], "Cover not found");
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn unknown_show_cover_is_404(pool: PgPool) {
        let env = env(pool.clone()).await;
        let res = env
            .get(&format!("/api/podcasts/{}/cover", Uuid::now_v7()))
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(res.json::<Value>()["error"], "Cover not found");
        let bad = env.get("/api/podcasts/not-a-uuid/cover").await;
        bad.assert_status(StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn episode_cover_redirects_to_the_proxy(pool: PgPool) {
        let env = env(pool.clone()).await;
        let show = env.stored_show().await;
        add_episodes(&pool, show, &[("ep1", day(1))]).await;
        let episode = episode_id(&pool, "ep1").await;
        let res = env
            .get(&format!("/api/podcasts/episodes/{episode}/cover"))
            .await;
        res.assert_status(StatusCode::FOUND);
        assert_eq!(
            location(&res),
            "/api/library/cover-art?url=https%3A%2F%2Fa.invalid%2Fep1.jpg"
        );
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn episode_cover_without_an_image_is_404(pool: PgPool) {
        let env = env(pool.clone()).await;
        let show = env.stored_show().await;
        add_episodes(&pool, show, &[("ep1", day(1))]).await;
        let episode = episode_id(&pool, "ep1").await;
        sqlx::query("UPDATE podcast_episodes SET image_url = NULL WHERE id = $1")
            .bind(episode)
            .execute(env.pool())
            .await
            .unwrap();
        let res = env
            .get(&format!("/api/podcasts/episodes/{episode}/cover"))
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(res.json::<Value>()["error"], "Cover not found");
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn unknown_episode_cover_is_404(pool: PgPool) {
        let env = env(pool.clone()).await;
        let res = env
            .get(&format!("/api/podcasts/episodes/{}/cover", Uuid::now_v7()))
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(res.json::<Value>()["error"], "Cover not found");
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn cover_redirect_forwards_a_token_query(pool: PgPool) {
        let env = env(pool.clone()).await;
        let show = env.stored_show().await;
        let res = env
            .server
            .get(&format!("/api/podcasts/{show}/cover?token={}", env.token))
            .await;
        res.assert_status(StatusCode::FOUND);
        assert_eq!(
            location(&res),
            format!(
                "/api/library/cover-art?url=https%3A%2F%2Ffeed.example%2Fcover.jpg&token={}",
                env.token
            )
        );
    }
}

// ---- Task 8: AI-content labels, filter, switch (spec S1-S5, D-200/D-207) ----

const HIDDEN: &str = "x-kima-ai-hidden";

/// Removes the kima-only `aiLabel`/`aiLabelSource` keys everywhere in `v`.
fn strip_ai(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("aiLabel");
            map.remove("aiLabelSource");
            map.values_mut().for_each(strip_ai);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_ai),
        _ => {}
    }
}

fn hidden(res: &axum_test::TestResponse) -> Option<String> {
    res.headers()
        .get(HIDDEN)
        .map(|v| v.to_str().unwrap().to_string())
}

async fn set_tag(pool: &PgPool, table: &str, id: Uuid, tag: Option<bool>) {
    sqlx::query(&format!("UPDATE {table} SET ai_feed = $2 WHERE id = $1"))
        .bind(id)
        .bind(tag)
        .execute(pool)
        .await
        .unwrap();
}

/// Two followed shows: `ai` (channel tag true, episodes a1 a2) and `plain` (no tag, episodes
/// p1 untagged and p2 with an item tag true).
async fn two_shows(env: &Env) -> (Uuid, Uuid) {
    let ai = store(
        env.pool(),
        "https://ai.example.invalid/feed",
        "AI Show",
        None,
    )
    .await;
    let plain = store(
        env.pool(),
        "https://plain.example.invalid/feed",
        "Plain Show",
        None,
    )
    .await;
    add_episodes(env.pool(), ai, &[("a1", day(1)), ("a2", day(2))]).await;
    add_episodes(env.pool(), plain, &[("p1", day(3)), ("p2", day(4))]).await;
    set_tag(env.pool(), "podcasts", ai, Some(true)).await;
    let p2 = episode_id(env.pool(), "p2").await;
    set_tag(env.pool(), "podcast_episodes", p2, Some(true)).await;
    for show in [ai, plain] {
        podcasts::subscribe(env.pool(), env.user, show)
            .await
            .unwrap();
    }
    (ai, plain)
}

async fn member(env: &Env) -> String {
    common::user_with_token(&env.state, "member").await.1
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn subscribe_stores_the_feed_ai_tags(pool: PgPool) {
    let env = env(pool).await;
    env.subscribe(json!({"feedUrl": env.host.url("/ai.xml")}))
        .await
        .assert_status_ok();
    let res = env.get("/api/podcasts?ai=show").await;
    res.assert_status_ok();
    assert_eq!(hidden(&res), None, "no header with ai=show");
    let v = res.json::<Value>();
    assert_eq!(v[0]["aiLabel"], "ai");
    assert_eq!(v[0]["aiLabelSource"], "feed-show");
    let eps = &v[0]["episodes"];
    assert_eq!(eps[0]["title"], "Made by people");
    assert_eq!(eps[0]["aiLabel"], "human");
    assert_eq!(eps[0]["aiLabelSource"], "feed-episode");
    assert_eq!(eps[1]["aiLabel"], "ai");
    assert_eq!(eps[1]["aiLabelSource"], "feed-show");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_tag_is_unknown_not_human_and_stays_visible(pool: PgPool) {
    let env = env(pool).await;
    let show = store(env.pool(), "https://u.example.invalid/feed", "U", None).await;
    add_episodes(env.pool(), show, &[("u1", day(1))]).await;
    podcasts::subscribe(env.pool(), env.user, show)
        .await
        .unwrap();
    let res = env.get("/api/podcasts").await;
    assert_eq!(hidden(&res).as_deref(), Some("0"));
    let v = res.json::<Value>();
    assert_eq!(v[0]["aiLabel"], "unknown");
    assert!(v[0]["aiLabelSource"].is_null());
    assert_eq!(v[0]["episodes"][0]["aiLabel"], "unknown");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lists_hide_ai_by_default_with_header(pool: PgPool) {
    let env = env(pool).await;
    two_shows(&env).await;
    let res = env.get("/api/podcasts").await;
    res.assert_status_ok();
    // The AI show and the plain show's AI episode.
    assert_eq!(hidden(&res).as_deref(), Some("2"));
    let v = res.json::<Value>();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["title"], "Plain Show");
    let titles: Vec<_> = v[0]["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles, vec!["p1"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ai_show_param_shows_all_without_header(pool: PgPool) {
    let env = env(pool).await;
    two_shows(&env).await;
    let res = env.get("/api/podcasts?ai=show").await;
    assert_eq!(hidden(&res), None);
    let v = res.json::<Value>();
    assert_eq!(v.as_array().unwrap().len(), 2);
    let res = env.get("/api/podcasts?ai=hide").await;
    assert_eq!(hidden(&res).as_deref(), Some("2"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ai_param_rejects_other_values(pool: PgPool) {
    let env = env(pool).await;
    let (ai, _) = two_shows(&env).await;
    for path in [
        "/api/podcasts?ai=all".to_string(),
        format!("/api/podcasts/{ai}?ai=yes"),
        format!("/api/podcasts/{ai}/episodes?ai="),
        "/api/podcasts/listening?ai=1".to_string(),
        "/api/search?q=show&ai=maybe".to_string(),
    ] {
        let res = env.get(&path).await;
        assert_eq!(res.status_code(), StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(
            res.json::<Value>(),
            json!({"error": "ai must be hide or show"}),
            "{path}"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn x_kima_ai_hidden_counts_before_paging(pool: PgPool) {
    let env = env(pool).await;
    let show = store(
        env.pool(),
        "https://page.example.invalid/feed",
        "Paged",
        None,
    )
    .await;
    let eps: Vec<(String, OffsetDateTime)> =
        (1..=10).map(|n| (format!("e{n}"), day(n as u8))).collect();
    let refs: Vec<(&str, OffsetDateTime)> = eps.iter().map(|(g, d)| (g.as_str(), *d)).collect();
    add_episodes(env.pool(), show, &refs).await;
    for g in ["e10", "e8", "e2"] {
        let id = episode_id(env.pool(), g).await;
        set_tag(env.pool(), "podcast_episodes", id, Some(true)).await;
    }
    podcasts::subscribe(env.pool(), env.user, show)
        .await
        .unwrap();

    let res = env
        .get(&format!("/api/podcasts/{show}/episodes?limit=5"))
        .await;
    res.assert_status_ok();
    assert_eq!(hidden(&res).as_deref(), Some("3"));
    let v = res.json::<Value>();
    assert_eq!(v["total"], 7);
    let titles: Vec<_> = v["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles, vec!["e9", "e7", "e6", "e5", "e4"]);

    let res = env.get(&format!("/api/podcasts/{show}?limit=5")).await;
    assert_eq!(hidden(&res).as_deref(), Some("3"));
    let v = res.json::<Value>();
    assert_eq!(v["episodeTotal"], 7);
    assert_eq!(v["episodes"].as_array().unwrap().len(), 5);

    let res = env
        .get(&format!("/api/podcasts/{show}/episodes?limit=5&ai=show"))
        .await;
    assert_eq!(hidden(&res), None);
    let v = res.json::<Value>();
    assert_eq!(v["total"], 10);
    assert_eq!(v["episodes"][0]["title"], "e10");
    assert_eq!(v["episodes"][0]["aiLabel"], "ai");
    assert_eq!(v["episodes"][0]["aiLabelSource"], "feed-episode");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn direct_read_of_ai_show_still_answers(pool: PgPool) {
    let env = env(pool).await;
    let (ai, _) = two_shows(&env).await;
    let res = env.get(&format!("/api/podcasts/{ai}")).await;
    res.assert_status_ok();
    let v = res.json::<Value>();
    assert_eq!(v["title"], "AI Show");
    assert_eq!(v["aiLabel"], "ai");
    assert_eq!(v["aiLabelSource"], "feed-show");
    // Its episodes inherit the show's label, so the hidden page is empty and says so.
    assert_eq!(v["episodes"], json!([]));
    assert_eq!(v["episodeTotal"], 0);
    assert_eq!(hidden(&res).as_deref(), Some("2"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn settings_round_trip_podcast_ai_content(pool: PgPool) {
    let env = env(pool).await;
    let res = env.get("/api/settings").await;
    assert_eq!(res.json::<Value>()["podcastAiContent"], "hide");
    let res = env
        .post("/api/settings", json!({"podcastAiContent": "show"}))
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["podcastAiContent"], "show");
    assert_eq!(
        env.get("/api/settings").await.json::<Value>()["podcastAiContent"],
        "show"
    );
    let res = env
        .post("/api/settings", json!({"podcastAiContent": "all"}))
        .await;
    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
    let body = res.json::<Value>();
    assert_eq!(body["details"][0]["path"], json!(["podcastAiContent"]));
    assert_eq!(
        body["details"][0]["message"],
        "podcastAiContent must be hide or show"
    );
    assert_eq!(
        env.get("/api/settings").await.json::<Value>()["podcastAiContent"],
        "show",
        "a refused change keeps the stored value"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn setting_show_persists_per_user(pool: PgPool) {
    let env = env(pool).await;
    let (ai, plain) = two_shows(&env).await;
    let other = member(&env).await;
    let other_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'member'")
        .fetch_one(env.pool())
        .await
        .unwrap();
    for show in [ai, plain] {
        podcasts::subscribe(env.pool(), other_id, show)
            .await
            .unwrap();
    }
    env.post("/api/settings", json!({"podcastAiContent": "show"}))
        .await
        .assert_status_ok();
    let res = env.get("/api/podcasts").await;
    assert_eq!(hidden(&res), None);
    assert_eq!(res.json::<Value>().as_array().unwrap().len(), 2);
    // `?ai=hide` still hides for this request only.
    let res = env.get("/api/podcasts?ai=hide").await;
    assert_eq!(res.json::<Value>().as_array().unwrap().len(), 1);
    // Another user keeps the default.
    let res = env
        .server
        .get("/api/podcasts")
        .authorization_bearer(&other)
        .await;
    assert_eq!(hidden(&res).as_deref(), Some("2"));
    assert_eq!(res.json::<Value>().as_array().unwrap().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_override_requires_admin(pool: PgPool) {
    let env = env(pool).await;
    let (ai, plain) = two_shows(&env).await;
    let other = member(&env).await;
    let path = format!("/api/podcasts/{plain}/ai-label");
    let res = env
        .server
        .put(&path)
        .authorization_bearer(&other)
        .json(&json!({"label": "ai"}))
        .await;
    assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    let res = env.server.put(&path).json(&json!({"label": "ai"})).await;
    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);

    let put = |path: String, body: Value| {
        env.server
            .put(&path)
            .authorization_bearer(&env.token)
            .json(&body)
    };
    let res = put(path.clone(), json!({"label": "ai"})).await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({"aiLabel": "ai", "aiLabelSource": "admin"})
    );
    let res = put(path.clone(), json!({"label": "maybe"})).await;
    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "label must be ai, human or null"})
    );
    let res = put(
        format!("/api/podcasts/{}/ai-label", Uuid::now_v7()),
        json!({"label": "human"}),
    )
    .await;
    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
    // Both shows are AI now: everything hidden.
    let res = env.get("/api/podcasts").await;
    assert_eq!(res.json::<Value>(), json!([]));
    // The admin confirms the channel-tagged show human: the override beats the channel tag.
    let res = put(
        format!("/api/podcasts/{ai}/ai-label"),
        json!({"label": "human"}),
    )
    .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"aiLabel": "human", "aiLabelSource": "admin"})
    );
    // Clearing goes back to the feed's label.
    let res = put(path.clone(), json!({"label": null})).await;
    assert_eq!(
        res.json::<Value>(),
        json!({"aiLabel": "unknown", "aiLabelSource": null})
    );
    let stored: (Option<String>, Option<Uuid>) =
        sqlx::query_as("SELECT ai_override, ai_override_by FROM podcasts WHERE id = $1")
            .bind(plain)
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert_eq!(stored, (None, None));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_episode_override_wins_and_is_admin_only(pool: PgPool) {
    let env = env(pool).await;
    let (ai, plain) = two_shows(&env).await;
    let other = member(&env).await;
    let a1 = episode_id(env.pool(), "a1").await;
    let p1 = episode_id(env.pool(), "p1").await;
    let path = format!("/api/podcasts/{ai}/episodes/{a1}/ai-label");
    let res = env
        .server
        .put(&path)
        .authorization_bearer(&other)
        .json(&json!({"label": "human"}))
        .await;
    assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    let res = env
        .server
        .put(&path)
        .authorization_bearer(&env.token)
        .json(&json!({"label": "human"}))
        .await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({"aiLabel": "human", "aiLabelSource": "admin"})
    );
    // An episode of another show is not found under this one.
    let res = env
        .server
        .put(&format!("/api/podcasts/{ai}/episodes/{p1}/ai-label"))
        .authorization_bearer(&env.token)
        .json(&json!({"label": "ai"}))
        .await;
    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
    // The episode label beats its AI show's label: visible on the show's page.
    let res = env.get(&format!("/api/podcasts/{ai}/episodes")).await;
    let v = res.json::<Value>();
    assert_eq!(v["total"], 1);
    assert_eq!(v["episodes"][0]["title"], "a1");
    assert_eq!(hidden(&res).as_deref(), Some("1"));

    // Clearing the override (`null`) falls back to the source label: a1 inherits its show's
    // channel tag, p2 has its own item tag.
    let res = env
        .server
        .put(&path)
        .authorization_bearer(&env.token)
        .json(&json!({"label": null}))
        .await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({"aiLabel": "ai", "aiLabelSource": "feed-show"})
    );
    let p2 = episode_id(env.pool(), "p2").await;
    let p2_path = format!("/api/podcasts/{plain}/episodes/{p2}/ai-label");
    for (label, expected) in [
        (
            json!("human"),
            json!({"aiLabel": "human", "aiLabelSource": "admin"}),
        ),
        (
            Value::Null,
            json!({"aiLabel": "ai", "aiLabelSource": "feed-episode"}),
        ),
    ] {
        let res = env
            .server
            .put(&p2_path)
            .authorization_bearer(&env.token)
            .json(&json!({ "label": label }))
            .await;
        res.assert_status_ok();
        assert_eq!(res.json::<Value>(), expected);
    }
    let stored: (Option<String>, Option<Uuid>) =
        sqlx::query_as("SELECT ai_override, ai_override_by FROM podcast_episodes WHERE id = $1")
            .bind(p2)
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert_eq!(stored, (None, None));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn item_true_beats_admin_human(pool: PgPool) {
    let env = env(pool).await;
    let (_, plain) = two_shows(&env).await;
    env.server
        .put(&format!("/api/podcasts/{plain}/ai-label"))
        .authorization_bearer(&env.token)
        .json(&json!({"label": "human"}))
        .await
        .assert_status_ok();
    let v = env
        .get(&format!("/api/podcasts/{plain}/episodes?ai=show"))
        .await
        .json::<Value>();
    let by_title = |t: &str| {
        v["episodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["title"] == t)
            .unwrap()
            .clone()
    };
    assert_eq!(by_title("p2")["aiLabel"], "ai");
    assert_eq!(by_title("p2")["aiLabelSource"], "feed-episode");
    assert_eq!(by_title("p1")["aiLabel"], "human");
    assert_eq!(by_title("p1")["aiLabelSource"], "admin");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listening_next_and_recently_listened_skip_ai(pool: PgPool) {
    let env = env(pool).await;
    let (ai, plain) = two_shows(&env).await;
    // Between p1 and the AI-tagged p2: the plain show's newest visible episode.
    add_episodes(
        env.pool(),
        plain,
        &[("pn", day(3) + time::Duration::hours(12))],
    )
    .await;
    progress(env.pool(), env.user, "a1", 30.0, 60.0, false).await;
    progress(env.pool(), env.user, "p1", 30.0, 60.0, false).await;
    let res = env.get("/api/podcasts/listening").await;
    // Hidden: a1 in progress (AI show), and both shows' newest episodes without progress, a2
    // (AI show) and p2 (AI item tag).
    assert_eq!(hidden(&res).as_deref(), Some("3"));
    let v = res.json::<Value>();
    // The filter runs before each show's newest pick: the plain show offers pn, not nothing.
    let new_titles: Vec<_> = v["newEpisodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["episode"]["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(new_titles, vec!["pn"]);
    let in_progress: Vec<_> = v["inProgress"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["episode"]["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(in_progress, vec!["p1"]);
    assert!(v["inProgress"][0]["episode"]["aiLabel"] == "unknown");
    assert!(v["inProgress"][0]["podcast"]["aiLabel"] == "unknown");
    let all = env.get("/api/podcasts/listening?ai=show").await;
    assert_eq!(hidden(&all), None);
    assert_eq!(
        all.json::<Value>()["inProgress"].as_array().unwrap().len(),
        2
    );

    // `next` after pn skips the AI-tagged p2: the end of the show.
    let pn = episode_id(env.pool(), "pn").await;
    let v = env
        .get(&format!("/api/podcasts/{plain}/episodes/{pn}/next"))
        .await
        .json::<Value>();
    assert_eq!(v, json!({"episode": null}));
    let v = env
        .get(&format!("/api/podcasts/{plain}/episodes/{pn}/next?ai=show"))
        .await
        .json::<Value>();
    assert_eq!(v["episode"]["title"], "p2");
    assert_eq!(v["episode"]["aiLabel"], "ai");

    // The AI-tagged p2 is the plain show's latest played episode: recently-listened falls back
    // to its latest visible one, p1, instead of dropping the show.
    progress(env.pool(), env.user, "p2", 10.0, 60.0, false).await;
    let res = env.get("/api/library/recently-listened").await;
    // Hidden: the AI rows the unfiltered answer has (a1, p2).
    assert_eq!(hidden(&res).as_deref(), Some("2"));
    let items = res.json::<Value>()["items"].clone();
    let podcasts_in: Vec<_> = items
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["type"] == "podcast")
        .map(|i| {
            (
                i["id"].as_str().unwrap().to_string(),
                i["episodeId"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let p1 = episode_id(env.pool(), "p1").await;
    assert_eq!(podcasts_in, vec![(plain.to_string(), p1.to_string())]);
    let _ = ai;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sql_and_rust_resolution_agree(pool: PgPool) {
    use kima_api_types::podcasts::{AiLabelSource, episode_label, show_label};
    let overrides = [None, Some("ai"), Some("human")];
    let tags = [None, Some(true), Some(false)];
    let mut checked = 0;
    for (si, show_override) in overrides.iter().enumerate() {
        for (ci, channel) in tags.iter().enumerate() {
            let show = store(
                &pool,
                &format!("https://c{si}{ci}.example.invalid/feed"),
                "S",
                None,
            )
            .await;
            sqlx::query("UPDATE podcasts SET ai_override = $2, ai_feed = $3 WHERE id = $1")
                .bind(show)
                .bind(show_override)
                .bind(channel)
                .execute(&pool)
                .await
                .unwrap();
            for (ei, ep_override) in overrides.iter().enumerate() {
                for (ii, item) in tags.iter().enumerate() {
                    let guid = format!("c{si}{ci}-{ei}{ii}");
                    add_episodes(&pool, show, &[(&guid, day(1))]).await;
                    let id = episode_id(&pool, &guid).await;
                    sqlx::query(
                        "UPDATE podcast_episodes SET ai_override = $2, ai_feed = $3 WHERE id = $1",
                    )
                    .bind(id)
                    .bind(ep_override)
                    .bind(item)
                    .execute(&pool)
                    .await
                    .unwrap();
                    let sql = format!(
                        "SELECT {}, {}, {}, {} FROM podcast_episodes e
                         JOIN podcasts p ON p.id = e.podcast_id WHERE e.id = $1",
                        podcasts::show_label_sql("p"),
                        podcasts::show_source_sql("p"),
                        podcasts::episode_label_sql("e", "p"),
                        podcasts::episode_source_sql("e", "p"),
                    );
                    let (sl, ss, el, es): (String, Option<String>, String, Option<String>) =
                        sqlx::query_as(&sql)
                            .bind(id)
                            .fetch_one(&pool)
                            .await
                            .unwrap();
                    let show_rust = show_label(*show_override, *channel);
                    let ep_rust = episode_label(*ep_override, *item, show_rust);
                    let src = |s: Option<AiLabelSource>| s.map(|s| s.as_str().to_string());
                    assert_eq!(
                        (sl.as_str(), ss.clone()),
                        (show_rust.0.as_str(), src(show_rust.1))
                    );
                    assert_eq!(
                        (el.as_str(), es.clone()),
                        (ep_rust.0.as_str(), src(ep_rust.1))
                    );
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 81);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ai_label_and_explicit_are_independent_in_storage(pool: PgPool) {
    for (n, (explicit, ai)) in [(true, true), (true, false), (false, true), (false, false)]
        .into_iter()
        .enumerate()
    {
        let url = format!("https://x{n}.example.invalid/feed");
        let mut s = show(&url, "X", None);
        s.explicit = explicit;
        let id = podcasts::insert_show(&pool, Uuid::now_v7(), &s)
            .await
            .unwrap()
            .id;
        set_tag(&pool, "podcasts", id, Some(ai)).await;
        let got = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!((got.explicit, got.ai_feed), (explicit, Some(ai)));
    }
}

/// kima-web cannot read response headers, so the paged lists also carry the hidden count in
/// the body as `aiHidden` (D-207), present exactly when the header is.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hidden_count_is_in_the_body_too(pool: PgPool) {
    let env = env(pool).await;
    let (_, plain) = two_shows(&env).await;
    for path in [
        format!("/api/podcasts/{plain}/episodes"),
        format!("/api/podcasts/{plain}"),
        "/api/podcasts/listening".to_string(),
    ] {
        let res = env.get(&path).await;
        res.assert_status_ok();
        let header: i64 = hidden(&res).unwrap().parse().unwrap();
        assert_eq!(res.json::<Value>()["aiHidden"], json!(header), "{path}");
        let sep = if path.contains('?') { '&' } else { '?' };
        let res = env.get(&format!("{path}{sep}ai=show")).await;
        assert!(res.json::<Value>().get("aiHidden").is_none(), "{path}");
    }
    let res = env.get(&format!("/api/podcasts/{plain}/episodes")).await;
    assert_eq!(res.json::<Value>()["aiHidden"], 1);
}
