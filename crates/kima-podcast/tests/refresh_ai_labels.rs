//! M6 Task 8 (D-200, D-206): a refresh stores the feed's `ai-content` tags, re-reading them for
//! episodes that are already stored; an episode no longer in the feed keeps its last tag.

use base64::Engine;
use kima_core::identify::IdentifySwitch;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use sqlx::PgPool;
use uuid::Uuid;

const XML: &str = r#"<rss version="2.0" xmlns:podcast="https://podcastindex.org/namespace/1.0"><channel><title>Show</title>
<podcast:txt purpose="ai-content">true</podcast:txt>
<item><guid>old</guid><title>Old</title><podcast:txt purpose="ai-content">false</podcast:txt><enclosure url="https://example.org/old.mp3" type="audio/mpeg"/></item>
<item><guid>new</guid><title>New</title><enclosure url="https://example.org/new.mp3" type="audio/mpeg"/></item>
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
