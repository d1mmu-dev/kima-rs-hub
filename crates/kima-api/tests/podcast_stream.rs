//! M6 Task 11: `GET /api/podcasts/{podcast}/episodes/{episode}/stream` (`podcasts.ts:916-1084`):
//! the episode's audio from its host, with ranges, status mapping and SSRF handled by
//! `kima_net::open_audio` (D-203); this binary covers the route around it. A cached copy
//! (`kima_podcast::cache`, Task 12) is served first, without asking the host; a miss streams from
//! the host and queues one background download (D-214). `KIMA_NET_DENY_REAL=1`.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use kima_api::AppState;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_net::testing::{AudioHost, AudioMode};
use kima_net::{Bases, Net, NetConfig};
use uuid::Uuid;

mod common;

fn bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn show(feed_url: &str) -> NewShow<'_> {
    NewShow {
        feed_url,
        title: "Show",
        author: None,
        description: None,
        image_url: None,
        itunes_id: None,
        category_ids: &[],
        language: None,
        explicit: false,
        episode_count: 1,
        feed_etag: None,
        feed_last_modified: None,
    }
}

fn episode(audio_url: String, mime_type: Option<String>) -> NewEpisode {
    NewEpisode {
        guid: "ep-1".into(),
        title: "Ep 1".into(),
        description: None,
        audio_url,
        duration: 600,
        published_at: time::OffsetDateTime::now_utc(),
        episode_number: None,
        season: None,
        image_url: None,
        file_size: Some(999_999),
        mime_type,
    }
}

/// Stores one show with one episode pointing at `audio_url`; returns the episode id.
async fn seed(pool: &PgPool, audio_url: String, mime_type: Option<String>) -> (Uuid, Uuid) {
    let show = podcasts::insert_show(pool, Uuid::now_v7(), &show("https://feed.example/x.xml"))
        .await
        .unwrap();
    podcasts::insert_episodes(pool, show.id, &[episode(audio_url, mime_type)])
        .await
        .unwrap();
    let episode: Uuid = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE podcast_id = $1")
        .bind(show.id)
        .fetch_one(pool)
        .await
        .unwrap();
    (show.id, episode)
}

struct Env {
    server: axum_test::TestServer,
    token: String,
    user: Uuid,
    state: AppState,
}

fn env(pool: PgPool) -> impl std::future::Future<Output = Env> {
    let net = Arc::new(Net::new(
        NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
        pool.clone(),
        IdentifySwitch::new(false, false),
    ));
    let state: AppState = common::state(pool).with_net(net);
    env_from(state)
}

async fn env_from(state: AppState) -> Env {
    let (user, token) = common::user_with_token(&state, "podder").await;
    let server = axum_test::TestServer::new(kima_api::router(state.clone())).unwrap();
    Env {
        server,
        token,
        user: user.id,
        state,
    }
}

impl Env {
    async fn stream(&self, podcast: Uuid, episode: Uuid) -> axum_test::TestResponse {
        self.server
            .get(&format!(
                "/api/podcasts/{podcast}/episodes/{episode}/stream"
            ))
            .authorization_bearer(&self.token)
            .await
    }

    async fn stream_ranged(
        &self,
        podcast: Uuid,
        episode: Uuid,
        range: &str,
    ) -> axum_test::TestResponse {
        self.server
            .get(&format!(
                "/api/podcasts/{podcast}/episodes/{episode}/stream"
            ))
            .authorization_bearer(&self.token)
            .add_header(RANGE, range)
            .await
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_episode_is_404(pool: PgPool) {
    let env = env(pool).await;
    let res = env.stream(Uuid::now_v7(), Uuid::now_v7()).await;
    res.assert_status(StatusCode::NOT_FOUND);
    res.assert_json(&serde_json::json!({"error": "Episode not found"}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stored_mime_type_wins_over_the_hosts_content_type(pool: PgPool) {
    let host = AudioHost::start(bytes(1000), AudioMode::Normal).await;
    let (podcast, episode) = seed(
        &pool,
        format!("{}/ep.mp3", host.url()),
        Some("audio/x-custom".into()),
    )
    .await;
    let env = env(pool).await;
    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.header(CONTENT_TYPE), "audio/x-custom");
    assert_eq!(res.header(ACCEPT_RANGES), "bytes");
    assert_eq!(res.header(CACHE_CONTROL), "public, max-age=3600");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_mime_type_falls_back_to_audio_mpeg(pool: PgPool) {
    let host = AudioHost::start(bytes(10), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.header(CONTENT_TYPE), "audio/mpeg");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn valid_range_206_with_the_correct_slice(pool: PgPool) {
    let data = bytes(1000);
    let host = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream_ranged(podcast, episode, "bytes=10-19").await;
    res.assert_status(StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.header(CONTENT_RANGE), "bytes 10-19/1000");
    assert_eq!(res.as_bytes().as_ref(), &data[10..20]);
    assert_eq!(host.ranges(), vec![Some("bytes=10-19".to_string())]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn out_of_range_416_with_content_range_bytes_star_size(pool: PgPool) {
    let host = AudioHost::start(bytes(1000), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream_ranged(podcast, episode, "bytes=5000-").await;
    res.assert_status(StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(res.header(CONTENT_RANGE), "bytes */1000");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn multi_range_200_with_full_body(pool: PgPool) {
    let data = bytes(1000);
    let host = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream_ranged(podcast, episode, "bytes=0-1,5-9").await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    assert_eq!(host.ranges(), vec![None]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tells_the_browser_the_size_of_the_copy_the_host_sends_not_the_feeds_declared_length(
    pool: PgPool,
) {
    let host = AudioHost::start(bytes(1000), AudioMode::LieLength(1000)).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.header(axum::http::header::CONTENT_LENGTH), "1000");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn streams_the_whole_copy_with_its_real_length_when_no_range_or_several_ranges_are_asked(
    pool: PgPool,
) {
    let data = bytes(100_000);
    let host = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cdn_refusing_ranges_gets_the_whole_file(pool: PgPool) {
    let data = bytes(1000);
    let host = AudioHost::start(data.clone(), AudioMode::RefuseRanges).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream_ranged(podcast, episode, "bytes=0-9").await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_host_refusing_an_episode_is_a_404(pool: PgPool) {
    let host = AudioHost::start(bytes(10), AudioMode::Status(404)).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream(podcast, episode).await;
    res.assert_status(StatusCode::NOT_FOUND);
    res.assert_json(&serde_json::json!({"error": "This episode is not available from its host"}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_host_throttling_is_a_502(pool: PgPool) {
    let host = AudioHost::start(bytes(10), AudioMode::Status(502)).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream(podcast, episode).await;
    res.assert_status(StatusCode::BAD_GATEWAY);
    res.assert_json(
        &serde_json::json!({"error": "The episode's host did not answer; try again shortly"}),
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_host_redirecting_into_the_internal_network_is_refused_as_unavailable_not_a_500(
    pool: PgPool,
) {
    let private = AudioHost::start(
        Vec::new(),
        AudioMode::RedirectTo("http://10.0.0.1/x.mp3".into()),
    )
    .await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", private.url()), None).await;
    let net = Arc::new(Net::new(
        {
            let mut c = NetConfig::for_tests(Bases::all("http://127.0.0.1:9"));
            c.allow_private_images = false;
            c
        },
        pool.clone(),
        IdentifySwitch::new(false, false),
    ));
    let state: AppState = common::state(pool).with_net(net);
    let env = env_from(state).await;
    let res = env.stream(podcast, episode).await;
    res.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_token_in_query_is_accepted(pool: PgPool) {
    let host = AudioHost::start(bytes(10), AudioMode::Normal).await;
    let net = Arc::new(Net::new(
        NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
        pool.clone(),
        IdentifySwitch::new(false, false),
    ));
    let state: AppState = common::state(pool.clone()).with_net(net);
    let (user, _) = common::user_with_token(&state, "podder").await;
    let stream_token = state.jwt.issue_stream(user.id, user.token_version);
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .get(&format!(
            "/api/podcasts/{podcast}/episodes/{episode}/stream?token={stream_token}"
        ))
        .await
        .assert_status_ok();
}

/// The route never holds the `Source::Audio` permit past its own response: once a full stream
/// through the route completes, the next 16 raw admissions (the gate's whole capacity, D-203)
/// all succeed, proving the route's own request did not leak a permit (e.g. by buffering the
/// whole episode in a structure that outlives the response instead of streaming it).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_route_releases_its_audio_permit_once_the_response_completes(pool: PgPool) {
    let host = AudioHost::start(bytes(64 * 1024), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env(pool).await;
    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();

    let url = format!("{}/ep.mp3", host.url());
    for _ in 0..16 {
        let out = env
            .state
            .net
            .open_audio(&url, None, Duration::from_secs(5))
            .await;
        assert!(matches!(out, kima_net::Outcome::Answered(_)), "{out:?}");
    }
}

// Cache first (`podcasts.ts:933-1015`, D-214): a cached copy is served without asking the host,
// a miss streams from the host and queues the episode's download once.

/// The jobs queued for `episode`'s download: (state, payload user).
async fn download_jobs(pool: &PgPool, episode: Uuid) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT state, payload ->> 'user' FROM jobs
         WHERE queue = $1 AND payload ->> 'key' = $2 ORDER BY created_at",
    )
    .bind(kima_podcast::cache::QUEUE)
    .bind(format!("episode:{episode}"))
    .fetch_all(pool)
    .await
    .unwrap()
}

/// An environment whose server keeps its files under `data_dir` (the cache lives there).
async fn env_with_data(pool: PgPool, data_dir: &std::path::Path) -> Env {
    let net = Arc::new(Net::new(
        NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
        pool.clone(),
        IdentifySwitch::new(false, false),
    ));
    env_from(
        common::state(pool)
            .with_net(net)
            .with_data_dir(data_dir.to_path_buf()),
    )
    .await
}

/// Puts `bytes` in the cache as `episode`'s complete download, as the worker leaves it.
async fn cache_episode(env: &Env, data_dir: &std::path::Path, episode: Uuid, data: &[u8]) {
    let dir = kima_podcast::cache::cache_dir(data_dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{episode}.audio")), data).unwrap();
    podcasts::upsert_download(
        &env.state.pool,
        env.user,
        episode,
        &format!("podcast-audio/{episode}.audio"),
        data.len() as f64 / 1024.0 / 1024.0,
    )
    .await
    .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_cached_episode_is_served_from_the_cache_without_asking_its_host(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let host = AudioHost::start(bytes(10), AudioMode::Normal).await;
    let (podcast, episode) = seed(
        &pool,
        format!("{}/ep.mp3", host.url()),
        Some("audio/x-custom".into()),
    )
    .await;
    let env = env_with_data(pool.clone(), tmp.path()).await;
    let cached: Vec<u8> = bytes(5000).into_iter().rev().collect();
    cache_episode(&env, tmp.path(), episode, &cached).await;

    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &cached[..]);
    assert_eq!(res.header(CONTENT_TYPE), "audio/x-custom");
    assert_eq!(res.header(ACCEPT_RANGES), "bytes");
    assert_eq!(res.header(CACHE_CONTROL), "public, max-age=3600");
    assert_eq!(res.header(CONTENT_LENGTH), "5000");
    assert!(host.hits().is_empty(), "{:?}", host.hits());
    // Nothing to download: it is already cached.
    assert!(download_jobs(&pool, episode).await.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_range_on_a_cached_episode_is_answered_from_the_file(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let host = AudioHost::start(bytes(10), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env_with_data(pool.clone(), tmp.path()).await;
    let cached: Vec<u8> = bytes(1000).into_iter().rev().collect();
    cache_episode(&env, tmp.path(), episode, &cached).await;

    let res = env.stream_ranged(podcast, episode, "bytes=10-19").await;
    res.assert_status(StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.header(CONTENT_RANGE), "bytes 10-19/1000");
    assert_eq!(res.header(CONTENT_LENGTH), "10");
    assert_eq!(res.header(ACCEPT_RANGES), "bytes");
    assert_eq!(res.header(CONTENT_TYPE), "audio/mpeg");
    assert_eq!(res.as_bytes().as_ref(), &cached[10..20]);

    let res = env.stream_ranged(podcast, episode, "bytes=-100").await;
    res.assert_status(StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.header(CONTENT_RANGE), "bytes 900-999/1000");
    assert_eq!(res.as_bytes().as_ref(), &cached[900..]);

    let res = env.stream_ranged(podcast, episode, "bytes=5000-").await;
    res.assert_status(StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(res.header(CONTENT_RANGE), "bytes */1000");

    // Several ranges at once: the whole file, as 200 (RFC 9110).
    let res = env.stream_ranged(podcast, episode, "bytes=0-1,5-9").await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &cached[..]);
    assert!(host.hits().is_empty(), "{:?}", host.hits());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_empty_cached_file_is_not_served_and_the_host_answers(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(1000);
    let host = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env_with_data(pool.clone(), tmp.path()).await;
    cache_episode(&env, tmp.path(), episode, &[]).await;

    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    assert_eq!(host.hits().len(), 1);
    // And it is downloaded again.
    assert_eq!(download_jobs(&pool, episode).await.len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_cold_episode_streams_from_its_host_and_queues_one_download(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(1000);
    let host = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env_with_data(pool.clone(), tmp.path()).await;

    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    assert_eq!(host.hits().len(), 1);
    let user = env.user.to_string();
    assert_eq!(
        download_jobs(&pool, episode).await,
        vec![("queued".to_string(), user.clone())]
    );

    // Played again (and seeked) before the download ran: still the host, still one job.
    env.stream(podcast, episode).await.assert_status_ok();
    env.stream_ranged(podcast, episode, "bytes=10-19")
        .await
        .assert_status(StatusCode::PARTIAL_CONTENT);
    assert_eq!(host.hits().len(), 3);
    assert_eq!(download_jobs(&pool, episode).await.len(), 1);

    // While the download runs, plays do not queue a second one beside it.
    let claimed = kima_jobs::queue::claim(
        &pool,
        kima_podcast::cache::QUEUE,
        10,
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(claimed.len(), 1);
    env.stream(podcast, episode).await.assert_status_ok();
    assert_eq!(
        download_jobs(&pool, episode).await,
        vec![("running".to_string(), user)]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_download_in_progress_is_not_served_from_the_cache(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let data = bytes(1000);
    let host = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let (podcast, episode) = seed(&pool, format!("{}/ep.mp3", host.url()), None).await;
    let env = env_with_data(pool.clone(), tmp.path()).await;

    // Only the worker's temp file exists yet: never served.
    let dir = kima_podcast::cache::cache_dir(tmp.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{episode}.0123456789abcdef.tmp")),
        b"partial",
    )
    .unwrap();
    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    assert_eq!(host.hits().len(), 1);

    // A complete-looking file whose download job is still running is not served either.
    let cached: Vec<u8> = data.iter().rev().copied().collect();
    cache_episode(&env, tmp.path(), episode, &cached).await;
    let claimed = kima_jobs::queue::claim(
        &pool,
        kima_podcast::cache::QUEUE,
        10,
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(claimed.len(), 1);
    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    assert_eq!(host.hits().len(), 2);

    // Once it is done, the cached copy is served.
    assert!(
        kima_jobs::queue::complete(&pool, &claimed[0])
            .await
            .unwrap()
    );
    let res = env.stream(podcast, episode).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &cached[..]);
    assert_eq!(host.hits().len(), 2);
}
