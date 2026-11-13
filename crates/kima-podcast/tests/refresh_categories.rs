//! M6 Task 9 (review finding): a refresh gives a show without categories the ones its feed or
//! Apple names (upstream `refreshPodcastFeed`: `assignCategories(..., "background")` when
//! `categoryIds` is empty); a show that has categories keeps them.

use std::sync::Arc;

use base64::Engine;
use kima_core::identify::IdentifySwitch;
use kima_db::podcasts::{self, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use kima_podcast::catalog::Catalog;
use sqlx::PgPool;
use uuid::Uuid;

const FEED: &str = r#"<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"><channel><title>Tales</title><itunes:category text="Fiction"/><item><guid>t-1</guid><title>One</title><enclosure url="https://example.org/1.mp3" type="audio/mpeg"/></item></channel></rss>"#;

/// Apple's recorded replies (category tree) plus the show's feed at `/feed`.
async fn host() -> Replay {
    let mut rec = Recording::load(format!(
        "{}/tests/fixtures/apple/catalog.json",
        env!("CARGO_MANIFEST_DIR")
    ));
    rec.0.push(Entry {
        method: "GET".into(),
        url: "/feed".into(),
        status: 200,
        headers: Default::default(),
        body: serde_json::Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(FEED)),
        request_body_sha256: None,
    });
    Replay::start(rec).await
}

fn catalog(pool: &PgPool, replay: &Replay) -> Catalog {
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    Catalog::new(Arc::new(net), pool.clone())
}

async fn show(pool: &PgPool, replay: &Replay, categories: &[i32]) -> Uuid {
    let feed_url = format!("{}/feed", replay.url());
    podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &feed_url,
            title: "Tales",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: categories,
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap()
    .id
}

async fn categories(pool: &PgPool, id: Uuid) -> Vec<i32> {
    podcasts::show_by_id(pool, id)
        .await
        .unwrap()
        .unwrap()
        .category_ids
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_fills_missing_categories_from_the_feed(pool: PgPool) {
    let replay = host().await;
    let id = show(&pool, &replay, &[]).await;
    kima_podcast::refresh::refresh(&catalog(&pool, &replay), id)
        .await
        .unwrap();
    assert_eq!(categories(&pool, id).await, vec![1483]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_keeps_existing_categories(pool: PgPool) {
    let replay = host().await;
    let id = show(&pool, &replay, &[1495]).await;
    kima_podcast::refresh::refresh(&catalog(&pool, &replay), id)
        .await
        .unwrap();
    assert_eq!(categories(&pool, id).await, vec![1495]);
    assert!(
        !replay.asked().iter().any(|a| a.contains("genres")),
        "no category lookup for a show that has categories: {:?}",
        replay.asked()
    );
}
