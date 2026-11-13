//! Subsonic podcast views (M6 Task 18): `getPodcasts`, `getNewestPodcasts`,
//! `getPodcastEpisode`, `refreshPodcasts` and the podcast branch of `getTranscodeDecision`
//! (`routes/podcasts.rs`, `routes/media.rs`). Rows are seeded directly with `kima_db::podcasts`
//! helpers, as the other podcast test binaries do; the SQL fixture carries no podcast data.
//!
//! `subsonic_hides_ai_by_setting` is **not** implemented here: it depends on the `ai_feed`/
//! `aiLabel` decision and schema that Task 0/Task 8 own (D-200), which is explicitly out of
//! scope for this task, the same boundary Tasks 13/14/16 already recorded.

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

/// A show's feed, as a `kima_net::testing::Replay` serves it from its `fixtures/feed.xml`-style
/// recording. Each entry's `url` is matched against `Replay::url() + path`.
fn feed_entry(path: &str, xml: &str) -> Entry {
    use base64::Engine;
    Entry {
        method: "GET".into(),
        url: format!("http://replay{path}"),
        status: 200,
        headers: std::collections::BTreeMap::from([(
            "content-type".to_string(),
            "application/rss+xml".to_string(),
        )]),
        body: Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(xml)),
        request_body_sha256: None,
    }
}

fn feed_xml(title: &str, episode_title: &str, guid: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel>\
         <title>{title}</title>\
         <item><title>{episode_title}</title><guid>{guid}</guid>\
         <enclosure url=\"https://feed.example/{guid}.mp3\" type=\"audio/mpeg\"/></item>\
         </channel></rss>"
    )
}

struct Env {
    server: TestServer,
}

fn env(pool: PgPool) -> Env {
    env_with_net(pool, Arc::new(Net::disabled()))
}

use std::sync::Arc;

fn env_with_net(pool: PgPool, net: Arc<Net>) -> Env {
    let state = fixture_state(pool).with_net(net);
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let app = kima_subsonic::router(state).layer(MockConnectInfo(peer));
    Env {
        server: TestServer::new(app).unwrap(),
    }
}

impl Env {
    async fn get(&self, endpoint: &str, params: &[(&str, &str)]) -> axum_test::TestResponse {
        self.server
            .get(&format!("/{endpoint}.view"))
            .add_query_param("apiKey", FIXTURE_TOKEN)
            .add_query_params(params)
            .await
    }

    async fn json(&self, endpoint: &str, params: &[(&str, &str)]) -> Value {
        let mut all = vec![("f", "json")];
        all.extend_from_slice(params);
        let res = self.get(endpoint, &all).await;
        res.assert_status_ok();
        res.json::<Value>()["subsonic-response"].clone()
    }

    async fn assert_error(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
        code: u32,
        message: &str,
    ) {
        let body = self.json(endpoint, params).await;
        assert_eq!(body["status"], "failed", "{endpoint} {params:?}: {body}");
        assert_eq!(body["error"]["code"], code, "{endpoint} {params:?}: {body}");
        assert_eq!(body["error"]["message"], message, "{endpoint} {params:?}");
    }
}

/// Stores a show and `n` episodes (newest first by index), subscribes alice unless `follow` is
/// false, and returns the show id.
async fn seeded_show(pool: &PgPool, feed_url: &str, title: &str, n: usize, follow: bool) -> Uuid {
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url,
            title,
            author: Some("Author"),
            description: Some("A show"),
            image_url: Some("https://img.example/show.png"),
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: n as i32,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    let base = OffsetDateTime::from_unix_timestamp(1_767_225_660).unwrap();
    let episodes: Vec<NewEpisode> = (0..n)
        .map(|i| NewEpisode {
            guid: format!("ep-{i}"),
            title: format!("Episode {i}"),
            description: Some("desc".into()),
            audio_url: format!("https://feed.example/{title}/ep{i}.mp3"),
            duration: 100 + i as i32,
            published_at: base + time::Duration::seconds(i as i64),
            episode_number: Some(i as i32 + 1),
            season: None,
            image_url: None,
            file_size: Some(1000 + i as i64),
            mime_type: None,
        })
        .collect();
    podcasts::insert_episodes(pool, show.id, &episodes)
        .await
        .unwrap();
    if follow {
        podcasts::subscribe(pool, ALICE_ID, show.id).await.unwrap();
    }
    show.id
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn get_podcasts_lists_followed_channels_with_200_newest(pool: PgPool) {
    let env = env(pool.clone());
    let show = seeded_show(&pool, "https://feed.example/a.xml", "Show A", 3, true).await;
    // A show alice does not follow never appears.
    seeded_show(&pool, "https://feed.example/b.xml", "Show B", 1, false).await;

    let body = env.json("getPodcasts", &[]).await;
    let channels = body["podcasts"]["channel"].as_array().unwrap();
    assert_eq!(channels.len(), 1, "{channels:?}");
    let channel = &channels[0];
    assert_eq!(channel["id"], show.to_string());
    assert_eq!(channel["url"], "https://feed.example/a.xml");
    assert_eq!(channel["title"], "Show A");
    assert_eq!(channel["description"], "A show");
    assert_eq!(channel["coverArt"], show.to_string());
    assert_eq!(channel["originalImageUrl"], "https://img.example/show.png");
    let episodes = channel["episode"].as_array().unwrap();
    assert_eq!(episodes.len(), 3);
    // Newest first.
    assert_eq!(episodes[0]["title"], "Episode 2");
    let e = &episodes[0];
    assert_eq!(e["parent"], show.to_string());
    assert_eq!(e["channelId"], show.to_string());
    assert_eq!(e["album"], "Show A");
    assert_eq!(e["artist"], "Podcast");
    assert_eq!(e["coverArt"], show.to_string());
    assert_eq!(e["isDir"], false);
    assert_eq!(e["isVideo"], false);
    assert_eq!(e["type"], "podcast");
    assert_eq!(e["status"], "completed");
    assert_eq!(e["contentType"], "audio/mpeg");
    assert_eq!(e["suffix"], "mpeg");
    assert_eq!(e["streamUrl"], "https://feed.example/Show A/ep2.mp3");
    assert_eq!(e["duration"], 102);
    assert_eq!(e["size"], 1002);
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn get_podcasts_include_episodes_false(pool: PgPool) {
    let env = env(pool.clone());
    seeded_show(&pool, "https://feed.example/a.xml", "Show A", 2, true).await;
    let body = env
        .json("getPodcasts", &[("includeEpisodes", "false")])
        .await;
    let channels = body["podcasts"]["channel"].as_array().unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["episode"], serde_json::json!([]));
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn get_podcasts_by_id(pool: PgPool) {
    let env = env(pool.clone());
    let a = seeded_show(&pool, "https://feed.example/a.xml", "Show A", 1, true).await;
    let b = seeded_show(&pool, "https://feed.example/b.xml", "Show B", 1, true).await;

    let body = env.json("getPodcasts", &[("id", &a.to_string())]).await;
    let channels = body["podcasts"]["channel"].as_array().unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0]["id"], a.to_string());

    // An id not among the caller's followed shows: empty, not an error.
    let body = env
        .json(
            "getPodcasts",
            &[("id", "00000000-0000-7000-8000-ffffffffffff")],
        )
        .await;
    assert_eq!(body["podcasts"]["channel"], serde_json::json!([]));

    let _ = b; // used only to prove the filter actually narrows the list
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn get_newest_podcasts_count_clamped(pool: PgPool) {
    let env = env(pool.clone());
    seeded_show(&pool, "https://feed.example/a.xml", "Show A", 5, true).await;

    let body = env.json("getNewestPodcasts", &[("count", "2")]).await;
    assert_eq!(
        body["newestPodcasts"]["episode"].as_array().unwrap().len(),
        2
    );

    // Absent: default 20 (fewer than 5 episodes exist, so all come back).
    let body = env.json("getNewestPodcasts", &[]).await;
    assert_eq!(
        body["newestPodcasts"]["episode"].as_array().unwrap().len(),
        5
    );

    // Over the ceiling and non-numeric both clamp/fall back rather than erroring.
    let body = env.json("getNewestPodcasts", &[("count", "99999")]).await;
    assert_eq!(
        body["newestPodcasts"]["episode"].as_array().unwrap().len(),
        5
    );
    let body = env.json("getNewestPodcasts", &[("count", "abc")]).await;
    assert_eq!(
        body["newestPodcasts"]["episode"].as_array().unwrap().len(),
        5
    );
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn get_podcast_episode_only_followed(pool: PgPool) {
    let env = env(pool.clone());
    let followed = seeded_show(&pool, "https://feed.example/a.xml", "Show A", 1, true).await;
    let not_followed = seeded_show(&pool, "https://feed.example/b.xml", "Show B", 1, false).await;

    let followed_ep = podcasts::episodes_for_channel(&pool, followed, 1)
        .await
        .unwrap()
        .remove(0);
    let other_ep = podcasts::episodes_for_channel(&pool, not_followed, 1)
        .await
        .unwrap()
        .remove(0);

    let body = env
        .json("getPodcastEpisode", &[("id", &followed_ep.id.to_string())])
        .await;
    assert_eq!(body["episode"]["id"], followed_ep.id.to_string());

    env.assert_error(
        "getPodcastEpisode",
        &[("id", &other_ep.id.to_string())],
        70,
        "Podcast episode not found",
    )
    .await;
    env.assert_error(
        "getPodcastEpisode",
        &[("id", "00000000-0000-7000-8000-ffffffffffff")],
        70,
        "Podcast episode not found",
    )
    .await;
    env.assert_error(
        "getPodcastEpisode",
        &[],
        10,
        "Required parameter is missing: id",
    )
    .await;
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn refresh_podcasts_refreshes_each_followed_show(pool: PgPool) {
    let rec = Recording(vec![
        feed_entry("/a.xml", &feed_xml("Show A", "New A1", "a1")),
        feed_entry("/b.xml", &feed_xml("Show B", "New B1", "b1")),
    ]);
    let replay = Replay::start(rec).await;
    let net = Arc::new(Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    ));
    let env = env_with_net(pool.clone(), net);

    let show_a = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &format!("{}/a.xml", replay.url()),
            title: "Show A",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    let show_b = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &format!("{}/b.xml", replay.url()),
            title: "Show B",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    // A show alice does not follow is never refreshed.
    let show_c = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://unreachable.example/c.xml",
            title: "Show C",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    podcasts::subscribe(&pool, ALICE_ID, show_a.id)
        .await
        .unwrap();
    podcasts::subscribe(&pool, ALICE_ID, show_b.id)
        .await
        .unwrap();
    let _ = show_c;

    let body = env.json("refreshPodcasts", &[]).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(replay.asked().len(), 2, "{:?}", replay.asked());

    let refreshed_a = podcasts::show_by_id(&pool, show_a.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed_a.episode_count, 1);
    let eps = podcasts::episodes_for_channel(&pool, show_a.id, 10)
        .await
        .unwrap();
    assert!(eps.iter().any(|e| e.title == "New A1"), "{eps:?}");
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn decision_podcast_direct_only(pool: PgPool) {
    let env = env(pool.clone());
    let show = seeded_show(&pool, "https://feed.example/a.xml", "Show A", 1, true).await;
    let episode = podcasts::episodes_for_channel(&pool, show, 1)
        .await
        .unwrap()
        .remove(0);
    let id = episode.id.to_string();

    for format in [None, Some("raw"), Some("mpeg"), Some("MPEG")] {
        let mut params = vec![("mediaId", id.as_str()), ("mediaType", "podcast")];
        if let Some(f) = format {
            params.push(("format", f));
        }
        let body = env.json("getTranscodeDecision", &params).await;
        assert_eq!(
            body["transcodeDecision"]["canDirectPlay"], true,
            "{format:?}: {body}"
        );
        assert_eq!(body["transcodeDecision"]["canTranscode"], false);
        assert_eq!(body["transcodeDecision"]["transcodeParams"], "q-original");
    }

    let body = env
        .json(
            "getTranscodeDecision",
            &[
                ("mediaId", &id),
                ("mediaType", "podcast"),
                ("format", "opus"),
            ],
        )
        .await;
    assert_eq!(body["transcodeDecision"]["canDirectPlay"], false);
    assert_eq!(body["transcodeDecision"]["canTranscode"], false);
    assert!(body["transcodeDecision"].get("transcodeParams").is_none());
    assert_eq!(
        body["transcodeDecision"]["errorReason"],
        "Kima transcodes only to mp3; it cannot make opus"
    );
    assert_eq!(
        body["transcodeDecision"]["transcodeReason"],
        serde_json::json!(["Kima transcodes only to mp3; it cannot make opus"])
    );
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn extensions_advertise_get_podcast_episode(pool: PgPool) {
    let env = env(pool);
    let body = env.json("getOpenSubsonicExtensions", &[]).await;
    let names: Vec<&str> = body["openSubsonicExtensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"getPodcastEpisode"), "{names:?}");
}
