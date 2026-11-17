//! M6 Task 8 (D-200, D-206): a refresh stores the feed's `ai-content` tags, re-reading them for
//! episodes that are already stored; an episode no longer in the feed keeps its last tag.

use base64::Engine;
use kima_core::identify::IdentifySwitch;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const XML: &str = r#"<rss version="2.0" xmlns:podcast="https://podcastindex.org/namespace/1.0"><channel><title>Show</title>
<podcast:txt purpose="ai-content">true</podcast:txt>
<item><guid>old</guid><title>Old</title><podcast:txt purpose="ai-content">false</podcast:txt><enclosure url="https://example.org/old.mp3" type="audio/mpeg"/></item>
<item><guid>new</guid><title>New</title><enclosure url="https://example.org/new.mp3" type="audio/mpeg"/></item>
</channel></rss>"#;

/// The same show after its publisher dropped every `ai-content` tag.
const UNTAGGED: &str = r#"<rss version="2.0"><channel><title>Show</title>
<item><guid>old</guid><title>Old</title><enclosure url="https://example.org/old.mp3" type="audio/mpeg"/></item>
</channel></rss>"#;

fn episode(guid: &str) -> NewEpisode {
    NewEpisode {
        guid: guid.into(),
        title: guid.into(),
        description: None,
        audio_url: format!("https://example.org/{guid}.mp3"),
        duration: 0,
        published_at: time::OffsetDateTime::now_utc(),
        episode_number: None,
        season: None,
        image_url: None,
        file_size: None,
        mime_type: None,
    }
}

async fn tag(pool: &PgPool, guid: &str) -> Option<bool> {
    sqlx::query_scalar("SELECT ai_feed FROM podcast_episodes WHERE guid = $1")
        .bind(guid)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A feed host that honours conditional requests: `304` whenever the client sends a validator,
/// otherwise the feed with an `ETag`. Returns its base URL and the request heads it received.
async fn conditional_host(xml: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                let conditional =
                    head.contains("\r\nif-none-match:") || head.contains("\r\nif-modified-since:");
                seen.lock().unwrap().push(head);
                let reply = if conditional {
                    "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nETag: \"v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
                        xml.len()
                    )
                };
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (url, heads)
}

/// Upgrade from 0038 (D-206): a show stored before the labels existed has a validator for a
/// feed nobody read for tags, so its first refresh after 0039 must read the full feed, even
/// from a host that would answer the conditional request with `304`.
#[sqlx::test(migrations = false)]
async fn first_refresh_after_upgrade_reads_labels_despite_validators(pool: PgPool) {
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version < 39)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(&pool).await.unwrap();
    let (base, heads) = conditional_host(XML).await;
    let user = Uuid::now_v7();
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, 'u', 'x')")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO podcasts (id, feed_url, title, episode_count, feed_etag, feed_last_modified) VALUES ($1, $2, 'Show', 1, '\"v1\"', 'Mon, 02 Nov 2026 10:00:00 GMT')")
        .bind(id)
        .bind(format!("{base}/feed"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO podcast_subscriptions (user_id, podcast_id) VALUES ($1, $2)")
        .bind(user)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO podcast_episodes (id, podcast_id, guid, title, audio_url, published_at) VALUES ($1, $2, 'old', 'Old', 'https://example.org/old.mp3', now())")
        .bind(Uuid::now_v7())
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();

    kima_db::MIGRATOR.run(&pool).await.unwrap();
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&base)),
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    let outcome = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        kima_podcast::refresh::RefreshOutcome::Applied(_)
    ));
    assert_eq!(heads.lock().unwrap().len(), 1, "one feed request");
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(
        show.ai_feed,
        Some(true),
        "channel tag read after the upgrade"
    );
    assert_eq!(
        tag(&pool, "old").await,
        Some(false),
        "stored episode labelled"
    );
    assert_eq!(show.feed_etag.as_deref(), Some("\"v1\""), "validator kept");

    // The next refresh is conditional again: the host's 304 keeps the labels.
    kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap();
    let heads = heads.lock().unwrap().clone();
    assert_eq!(heads.len(), 2);
    assert!(
        heads[1].contains("if-none-match"),
        "second refresh conditional"
    );
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.ai_feed, Some(true));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_updates_existing_episode_tags(pool: PgPool) {
    let replay = Replay::start(Recording(vec![Entry {
        method: "GET".into(),
        url: "/feed".into(),
        status: 200,
        headers: Default::default(),
        body: serde_json::Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(XML)),
        request_body_sha256: None,
    }]))
    .await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    let id = Uuid::now_v7();
    podcasts::insert_show(
        &pool,
        id,
        &NewShow {
            feed_url: &format!("{}/feed", replay.url()),
            title: "Show",
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
    podcasts::insert_episodes(&pool, id, &[episode("old"), episode("gone")])
        .await
        .unwrap();
    sqlx::query("UPDATE podcast_episodes SET ai_feed = true WHERE guid IN ('old', 'gone')")
        .execute(&pool)
        .await
        .unwrap();

    let outcome = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        kima_podcast::refresh::RefreshOutcome::Applied(_)
    ));
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.ai_feed, Some(true));
    assert_eq!(show.ai_override, None);
    assert_eq!(
        tag(&pool, "old").await,
        Some(false),
        "stored episode re-read"
    );
    assert_eq!(tag(&pool, "new").await, None, "new episode without a tag");
    assert_eq!(tag(&pool, "gone").await, Some(true), "left the feed: kept");
}

/// A refresh rewrites only the feed's tags: an admin's show and episode labels survive it, and
/// a tag the publisher removed becomes "no signal" (`NULL`), not the last value (D-206).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_keeps_admin_overrides_and_clears_removed_tags(pool: PgPool) {
    let replay = Replay::start(Recording(vec![Entry {
        method: "GET".into(),
        url: "/feed".into(),
        status: 200,
        headers: Default::default(),
        body: serde_json::Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(UNTAGGED)),
        request_body_sha256: None,
    }]))
    .await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    let admin = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role) VALUES ($1, 'admin', 'x', 'admin')",
    )
    .bind(admin)
    .execute(&pool)
    .await
    .unwrap();
    let id = Uuid::now_v7();
    podcasts::insert_show(
        &pool,
        id,
        &NewShow {
            feed_url: &format!("{}/feed", replay.url()),
            title: "Show",
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
    podcasts::insert_episodes(&pool, id, &[episode("old")])
        .await
        .unwrap();
    sqlx::query("UPDATE podcasts SET ai_feed = true WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE podcast_episodes SET ai_feed = false WHERE guid = 'old'")
        .execute(&pool)
        .await
        .unwrap();
    let old: Uuid = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = 'old'")
        .fetch_one(&pool)
        .await
        .unwrap();
    podcasts::set_show_override(&pool, id, Some("human"), admin)
        .await
        .unwrap()
        .unwrap();
    podcasts::set_episode_override(&pool, id, old, Some("ai"), admin)
        .await
        .unwrap()
        .unwrap();

    let outcome = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        kima_podcast::refresh::RefreshOutcome::Applied(_)
    ));
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.ai_feed, None, "removed channel tag is no signal");
    assert_eq!(
        show.ai_override.as_deref(),
        Some("human"),
        "show override kept"
    );
    assert_eq!(
        tag(&pool, "old").await,
        None,
        "removed item tag is no signal"
    );
    let (label, by): (Option<String>, Option<Uuid>) =
        sqlx::query_as("SELECT ai_override, ai_override_by FROM podcast_episodes WHERE id = $1")
            .bind(old)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(label.as_deref(), Some("ai"), "episode override kept");
    assert_eq!(by, Some(admin));
}
