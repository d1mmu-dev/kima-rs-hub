//! M6 Task 5: Apple's podcast catalogue (`podcastCatalog.ts`) over upstream's recorded replies
//! (trimmed, `fixtures/apple/catalog.json`), the in-flight sharing and the two lanes, and how
//! an upstream's failure is classed (`utils/upstream.ts`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use kima_core::identify::IdentifySwitch;
use kima_db::podcasts::{self, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use kima_podcast::catalog::{Catalog, Category, Priority, oldest_episode};
use kima_podcast::feed::FeedEpisode;
use kima_podcast::upstream::Upstream;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const DND: &str = "1450241676";

fn recording() -> Recording {
    Recording::load(format!(
        "{}/tests/fixtures/apple/catalog.json",
        env!("CARGO_MANIFEST_DIR")
    ))
}

fn catalog_at(replay: &Replay, pool: &PgPool) -> Catalog {
    // The identification switch is off: Apple requests are user-facing.
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    Catalog::new(Arc::new(net), pool.clone())
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

fn chart_entry(id: &str) -> Value {
    json!({"id": {"attributes": {"im:id": id}}, "im:name": {"label": format!("Show {id}")}})
}

fn asks(replay: &Replay, part: &str) -> usize {
    replay.asked().iter().filter(|a| a.contains(part)).count()
}

async fn store(pool: &PgPool, feed: &str, itunes: Option<&str>) -> Uuid {
    podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: feed,
            title: "Dungeons and Daddies",
            author: None,
            description: None,
            image_url: None,
            itunes_id: itunes,
            category_ids: &[],
            language: None,
            explicit: false,
        },
    )
    .await
    .unwrap()
    .id
}

async fn stored_categories(pool: &PgPool, id: Uuid) -> Vec<i32> {
    podcasts::show_by_id(pool, id)
        .await
        .unwrap()
        .unwrap()
        .category_ids
}

// podcastCatalog.integration.test.ts

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reads_apples_whole_category_tree_sub_categories_under_their_parents(pool: PgPool) {
    let replay = Replay::start(recording()).await;
    let tree = catalog_at(&replay, &pool).categories().await.unwrap();
    assert_eq!(tree.iter().filter(|c| c.parent_id.is_none()).count(), 19);
    let has = |id, name: &str, parent| {
        tree.contains(&Category {
            id,
            name: name.into(),
            parent_id: Some(parent),
        })
    };
    assert!(has(1495, "Improv", 1303));
    assert!(has(1507, "Games", 1502));
    assert!(!tree.iter().any(|c| c.id == 26));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reads_a_categorys_chart_in_rank_order_with_large_artwork_and_apples_description(
    pool: PgPool,
) {
    let replay = Replay::start(recording()).await;
    let improv = catalog_at(&replay, &pool)
        .chart(Some(1495), Priority::Interactive)
        .await
        .unwrap();
    assert_eq!(improv.len(), 200);
    let ranks: Vec<Option<i32>> = improv.iter().take(3).map(|p| p.chart_rank).collect();
    assert_eq!(ranks, vec![Some(1), Some(2), Some(3)]);
    let dnd = improv.iter().find(|p| p.itunes_id == DND).unwrap();
    assert_eq!(dnd.title, "Dungeons and Daddies");
    assert!(dnd.chart_rank.unwrap() <= 20);
    assert!(dnd.description.chars().count() > 20);
    assert!(dnd.cover_url.as_deref().unwrap().contains("600x600bb."));
    assert_eq!(dnd.category_ids, vec![1495]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reads_the_overall_chart_without_a_category(pool: PgPool) {
    let replay = Replay::start(recording()).await;
    let top = catalog_at(&replay, &pool)
        .chart(None, Priority::Interactive)
        .await
        .unwrap();
    assert!(top.len() > 100);
    assert_eq!(top[0].chart_rank, Some(1));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn names_a_shows_categories_most_specific_first_without_apples_root(pool: PgPool) {
    let replay = Replay::start(recording()).await;
    let ids: Vec<String> = [DND, "1276705357", "367330921", "not-an-id"]
        .map(String::from)
        .to_vec();
    let shows = catalog_at(&replay, &pool)
        .lookup(&ids, Priority::Interactive)
        .await
        .unwrap();
    assert_eq!(shows[DND].category_ids, vec![1495, 1303, 1502, 1507]);
    assert_eq!(shows["1276705357"].category_ids, vec![1485, 1483, 1486]);
    assert_eq!(shows["367330921"].category_ids, vec![1303]);
    assert!(!shows.contains_key("not-an-id"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn maps_a_feeds_own_category_names_onto_apples_ids_and_drops_names_apple_does_not_use(
    pool: PgPool,
) {
    let replay = Replay::start(recording()).await;
    let names = ["Comedy", "TV & Film", "Not A Category"].map(String::from);
    assert_eq!(
        catalog_at(&replay, &pool)
            .category_ids_for_names(&names)
            .await
            .unwrap(),
        vec![1303, 1309]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stores_a_shows_categories_from_apple_else_from_its_feed(pool: PgPool) {
    let replay = Replay::start(recording()).await;
    let catalog = catalog_at(&replay, &pool);
    let id = store(&pool, "https://feeds.example.invalid/dnd", Some(DND)).await;
    assert_eq!(
        catalog
            .assign_categories(id, Some(DND), &[], Priority::Interactive)
            .await,
        vec![1495, 1303, 1502, 1507]
    );
    assert_eq!(
        stored_categories(&pool, id).await,
        vec![1495, 1303, 1502, 1507]
    );
    let names = ["Fiction", "Comedy Fiction"].map(String::from);
    assert_eq!(
        catalog
            .assign_categories(id, None, &names, Priority::Interactive)
            .await,
        vec![1483, 1486]
    );
}

#[test]
fn samples_a_show_by_its_oldest_dated_episode_with_audio() {
    let ep = |title: &str, unix: i64, audio: &str| FeedEpisode {
        guid: title.into(),
        title: title.into(),
        description: None,
        audio_url: audio.into(),
        duration: 0,
        published_at: OffsetDateTime::from_unix_timestamp(unix).unwrap(),
        episode_number: None,
        season: None,
        image_url: None,
        file_size: None,
        mime_type: "audio/mpeg".into(),
    };
    let a = "https://a.invalid/x.mp3";
    let feed = [
        ep("S4 Ep. 15", 1_790_812_800, a),
        ep("Ep. 1 - A Man and His Handshake", 1_550_102_400, a),
        ep("Season 4 Trailer", 1_735_689_600, a),
    ];
    assert_eq!(
        oldest_episode(&feed).map(|e| e.title.as_str()),
        Some("Ep. 1 - A Man and His Handshake")
    );
    // An episode without audio is never the sample. (kima-rs dates an unreadable date "now"
    // when parsing, so the upstream "not a date" case cannot reach here; D-212.)
    assert!(oldest_episode(&[ep("no audio", 1_262_304_000, "")]).is_none());
    assert!(oldest_episode(&[]).is_none());
}

// podcastCatalog.inflight.test.ts

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn asks_apple_once_when_three_ask_for_the_same_chart_while_it_is_on_its_way(pool: PgPool) {
    let replay = Replay::start(recording()).await;
    let catalog = catalog_at(&replay, &pool);
    let held = replay.hold("genre=1495");
    let ask = || {
        let c = catalog.clone();
        tokio::spawn(async move { c.chart(Some(1495), Priority::Interactive).await })
    };
    let (a, b) = (ask(), ask());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let c = ask();
    tokio::time::sleep(Duration::from_millis(200)).await;
    held.release();
    let (a, b, c) = (
        a.await.unwrap().unwrap(),
        b.await.unwrap().unwrap(),
        c.await.unwrap().unwrap(),
    );
    assert_eq!(asks(&replay, "genre=1495"), 1);
    assert_eq!(a.len(), 200);
    assert_eq!(b, a);
    assert_eq!(c, a);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn asks_again_once_a_request_fails_rather_than_handing_that_failure_to_later_askers(
    pool: PgPool,
) {
    let mut rec = recording();
    let good = rec
        .0
        .iter()
        .find(|e| e.url.contains("genre=1495"))
        .unwrap()
        .clone();
    rec.0.retain(|e| !e.url.contains("genre=1495"));
    rec.0.push(json_entry(&good.url, 503, json!({})));
    rec.0.push(good);
    let replay = Replay::start(rec).await;
    let catalog = catalog_at(&replay, &pool);
    let first = catalog.chart(Some(1495), Priority::Interactive).await;
    assert!(matches!(first, Err(Upstream::Down(_))), "{first:?}");
    assert_eq!(
        catalog
            .chart(Some(1495), Priority::Interactive)
            .await
            .unwrap()
            .len(),
        200
    );
    assert_eq!(asks(&replay, "genre=1495"), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn queues_a_category_rows_chart_behind_a_show_pages_lookup_and_every_other_chart(
    pool: PgPool,
) {
    let replay = Replay::start(recording()).await;
    let catalog = catalog_at(&replay, &pool);
    // A show page's lookup is on its way.
    let held = replay.hold("/lookup");
    let page = {
        let c = catalog.clone();
        tokio::spawn(async move { c.lookup(&[DND.to_string()], Priority::Interactive).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    // A category row (background) asks first, then a person opens a chart.
    let row = {
        let c = catalog.clone();
        tokio::spawn(async move { c.chart(Some(1502), Priority::Background).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let person = {
        let c = catalog.clone();
        tokio::spawn(async move { c.chart(Some(1507), Priority::Interactive).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    held.release();
    page.await.unwrap().unwrap();
    person.await.unwrap().unwrap();
    row.await.unwrap().unwrap();
    let order: Vec<String> = replay
        .asked()
        .into_iter()
        .filter(|a| !a.contains("/genres"))
        .collect();
    assert_eq!(order.len(), 3, "{order:?}");
    assert!(order[0].contains("/lookup"), "{order:?}");
    assert!(order[1].contains("genre=1507"), "{order:?}");
    assert!(order[2].contains("genre=1502"), "{order:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn moves_a_rows_waiting_request_to_the_front_when_a_person_asks_for_the_same_chart_and_asks_apple_once(
    pool: PgPool,
) {
    let replay = Replay::start(recording()).await;
    let catalog = catalog_at(&replay, &pool);
    let held = replay.hold("/lookup");
    let page = {
        let c = catalog.clone();
        tokio::spawn(async move { c.lookup(&[DND.to_string()], Priority::Interactive).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let spawn_chart = |genre: i32, p: Priority| {
        let c = catalog.clone();
        tokio::spawn(async move { c.chart(Some(genre), p).await })
    };
    // Two rows wait behind the page; then a person opens the second row's category.
    let other_row = spawn_chart(1502, Priority::Background);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let row = spawn_chart(1507, Priority::Background);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let opened = spawn_chart(1507, Priority::Interactive);
    let again = spawn_chart(1507, Priority::Background);
    tokio::time::sleep(Duration::from_millis(100)).await;
    held.release();
    page.await.unwrap().unwrap();
    let (a, b, c) = (
        row.await.unwrap().unwrap(),
        opened.await.unwrap().unwrap(),
        again.await.unwrap().unwrap(),
    );
    other_row.await.unwrap().unwrap();
    assert_eq!(asks(&replay, "genre=1507"), 1);
    assert_eq!(b, a);
    assert_eq!(c, a);
    let order: Vec<String> = replay
        .asked()
        .into_iter()
        .filter(|a| a.contains("/rss/"))
        .collect();
    assert!(order[0].contains("genre=1507"), "{order:?}");
    assert!(order[1].contains("genre=1502"), "{order:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lets_a_category_row_share_a_request_a_person_already_started_without_lowering_it(
    pool: PgPool,
) {
    let replay = Replay::start(recording()).await;
    let catalog = catalog_at(&replay, &pool);
    let held = replay.hold("genre=1495");
    let opened = {
        let c = catalog.clone();
        tokio::spawn(async move { c.chart(Some(1495), Priority::Interactive).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let row = {
        let c = catalog.clone();
        tokio::spawn(async move { c.chart(Some(1495), Priority::Background).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    // While the shared load is on its way, another row's chart waits behind it: the load
    // stayed interactive.
    let other = {
        let c = catalog.clone();
        tokio::spawn(async move { c.chart(Some(1507), Priority::Background).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(asks(&replay, "genre=1507"), 0);
    held.release();
    let (a, b) = (opened.await.unwrap().unwrap(), row.await.unwrap().unwrap());
    other.await.unwrap().unwrap();
    assert_eq!(a, b);
    assert_eq!(asks(&replay, "genre=1495"), 1);
}

// podcastUpstreamFailures.route.test.ts (catalogue parts)

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_chart_that_comes_back_as_an_error_page_fails_and_the_next_good_reply_is_used(
    pool: PgPool,
) {
    let url = "/us/rss/toppodcasts/limit=200/genre=1495/json";
    let replay = Replay::start(Recording(vec![
        html_entry(url, "<html>busy</html>"),
        json_entry(
            url,
            200,
            json!({"feed": {"entry": [chart_entry("1"), chart_entry("2")]}}),
        ),
    ]))
    .await;
    let catalog = catalog_at(&replay, &pool);
    let first = catalog.chart(Some(1495), Priority::Interactive).await;
    assert!(matches!(first, Err(Upstream::Down(_))), "{first:?}");
    let ids: Vec<String> = catalog
        .chart(Some(1495), Priority::Interactive)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.itunes_id)
        .collect();
    assert_eq!(ids, vec!["1", "2"]);
    // A reply without a feed is not cached either.
    let replay = Replay::start(Recording(vec![
        json_entry(url, 200, json!({"errorMessage": "busy"})),
        json_entry(url, 200, json!({"feed": {"entry": [chart_entry("3")]}})),
    ]))
    .await;
    let catalog = catalog_at(&replay, &pool);
    assert!(matches!(
        catalog.chart(Some(1495), Priority::Interactive).await,
        Err(Upstream::Down(_))
    ));
    assert_eq!(
        catalog
            .chart(Some(1495), Priority::Interactive)
            .await
            .unwrap()[0]
            .itunes_id,
        "3"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reads_a_chart_of_one_show_which_apple_sends_as_an_object(pool: PgPool) {
    let replay = Replay::start(Recording(vec![json_entry(
        "/us/rss/toppodcasts/limit=200/genre=1495/json",
        200,
        json!({"feed": {"entry": chart_entry("7")}}),
    )]))
    .await;
    let ids: Vec<String> = catalog_at(&replay, &pool)
        .chart(Some(1495), Priority::Interactive)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.itunes_id)
        .collect();
    assert_eq!(ids, vec!["7"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_lookup_without_a_results_list_fails_instead_of_marking_every_show_unknown_for_a_week(
    pool: PgPool,
) {
    let url = "/lookup?id=5&entity=podcast";
    let replay = Replay::start(Recording(vec![
        json_entry(url, 200, json!({"errorMessage": "throttled"})),
        json_entry(
            url,
            200,
            json!({"resultCount": 1, "results": [{"kind": "podcast", "collectionId": 5,
                   "feedUrl": "https://f.example.invalid/5", "genreIds": ["1495"]}]}),
        ),
    ]))
    .await;
    let catalog = catalog_at(&replay, &pool);
    let ids = vec!["5".to_string()];
    let first = catalog.lookup(&ids, Priority::Interactive).await;
    assert!(matches!(first, Err(Upstream::Down(_))), "{first:?}");
    let shows = catalog.lookup(&ids, Priority::Interactive).await.unwrap();
    assert_eq!(
        shows["5"].feed_url.as_deref(),
        Some("https://f.example.invalid/5")
    );
    // Known now, and remembered: no third request.
    catalog.lookup(&ids, Priority::Interactive).await.unwrap();
    assert_eq!(replay.asked().len(), 2);
}

// podcastStartupJobs.test.ts:46

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fills_missing_categories_for_many_shows_with_one_request_to_apple(pool: PgPool) {
    let ids = ["9100001", "9100002", "9100003"];
    let results: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({"kind": "podcast", "collectionId": id.parse::<i64>().unwrap(),
                   "feedUrl": format!("https://f.example.invalid/{id}"), "genreIds": ["1495", "26"]})
        })
        .collect();
    let replay = Replay::start(Recording(vec![json_entry(
        "/lookup?id=9100001,9100002,9100003&entity=podcast",
        200,
        json!({"resultCount": 3, "results": results}),
    )]))
    .await;
    let mut shows = Vec::new();
    for id in ids {
        shows.push(store(&pool, &format!("https://f.example.invalid/{id}"), Some(id)).await);
    }
    // A show without an Apple id is not asked about.
    store(&pool, "https://f.example.invalid/none", None).await;
    let catalog = catalog_at(&replay, &pool);
    assert_eq!(catalog.assign_missing_categories().await, 3);
    assert_eq!(asks(&replay, "/lookup"), 1);
    assert_eq!(stored_categories(&pool, shows[0]).await, vec![1495]);
    // Nothing is missing any more.
    assert_eq!(catalog.assign_missing_categories().await, 0);
    assert_eq!(asks(&replay, "/lookup"), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn chart_country_falls_back_to_us(pool: PgPool) {
    let replay = Replay::start(Recording(vec![])).await;
    let catalog = catalog_at(&replay, &pool);
    assert_eq!(catalog.chart_country().await, "us");
    for (stored, used) in [
        (json!("GB"), "gb"),
        (json!("pl"), "pl"),
        (json!("usa"), "us"),
        (json!("1x"), "us"),
        (json!(12), "us"),
    ] {
        sqlx::query(
            "INSERT INTO settings_kv (key, value) VALUES ('podcastChartCountry', $1)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(&stored)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(catalog.chart_country().await, used, "{stored}");
    }
    // The chart is asked for in that country.
    sqlx::query("UPDATE settings_kv SET value = '\"gb\"' WHERE key = 'podcastChartCountry'")
        .execute(&pool)
        .await
        .unwrap();
    let _ = catalog.chart(None, Priority::Interactive).await;
    assert_eq!(
        replay.asked(),
        vec!["GET /gb/rss/toppodcasts/limit=200/json".to_string()]
    );
}
