//! M6 podcast work the server does at startup and on its own schedule (plan Task 26 Step 3,
//! J4-J6; upstream `index.ts:553-577`): the 30-day cache cleanup and then the one-off length
//! measuring, the category fill-in for shows Apple knows but that have no categories yet, and
//! the daily 04:00 UTC cleanup entry. Driven through the same `kima_server` functions `main`
//! calls, against a loopback Apple stand-in and a temporary data directory.

use std::collections::BTreeMap;
use std::sync::Arc;

use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_db::users::{self, Registration};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use kima_podcast::cache;
use kima_podcast::ctx::PodcastCtx;
use serde_json::{Value, json};
use uuid::Uuid;

const APPLE_IDS: [&str; 3] = ["9100001", "9100002", "9100003"];

async fn user(pool: &PgPool) -> Uuid {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(pool)
        .await
        .unwrap();
    let Registration::Created(user) = users::register(pool, Uuid::now_v7(), "listener", "hash")
        .await
        .unwrap()
    else {
        panic!("registration failed")
    };
    user.id
}

async fn show(pool: &PgPool, feed: &str, itunes: Option<&str>) -> Uuid {
    podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: feed,
            title: "Startup Show",
            author: None,
            description: None,
            image_url: None,
            itunes_id: itunes,
            category_ids: &[],
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

/// One episode on `show` claiming to last 4414 s; returns its id.
async fn episode(pool: &PgPool, show: Uuid, guid: &str) -> Uuid {
    podcasts::insert_episodes(
        pool,
        show,
        &[NewEpisode {
            guid: guid.into(),
            title: guid.into(),
            description: None,
            audio_url: format!("https://a.invalid/{guid}.mp3"),
            duration: 4414,
            published_at: time::OffsetDateTime::now_utc(),
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: Some("audio/mpeg".into()),
        }],
    )
    .await
    .unwrap();
    sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = $1")
        .bind(guid)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn duration(pool: &PgPool, episode: Uuid) -> i32 {
    sqlx::query_scalar("SELECT duration FROM podcast_episodes WHERE id = $1")
        .bind(episode)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Apple's batched lookup for the three shows (`podcastStartupJobs.test.ts:46`).
async fn apple() -> Replay {
    let results: Vec<Value> = APPLE_IDS
        .iter()
        .map(|id| {
            json!({"kind": "podcast", "collectionId": id.parse::<i64>().unwrap(),
                   "feedUrl": format!("https://f.example.invalid/{id}"), "genreIds": ["1495", "26"]})
        })
        .collect();
    Replay::start(Recording(vec![Entry {
        method: "GET".into(),
        url: "/lookup?id=9100001,9100002,9100003&entity=podcast".into(),
        status: 200,
        headers: BTreeMap::new(),
        body: json!({"resultCount": 3, "results": results}),
        body_base64: None,
        request_body_sha256: None,
    }]))
    .await
}

fn ctx(pool: &PgPool, base: &str, data_dir: &std::path::Path) -> PodcastCtx {
    PodcastCtx {
        pool: pool.clone(),
        net: Arc::new(Net::new(
            NetConfig::for_tests(Bases::all(base)),
            pool.clone(),
            IdentifySwitch::new(false, false),
        )),
        data_dir: data_dir.to_path_buf(),
    }
}

/// Caches `src` as `episode`'s download for `user`, last played `days_ago` days ago.
async fn cached(
    pool: &PgPool,
    data_dir: &std::path::Path,
    src: &std::path::Path,
    user: Uuid,
    episode: Uuid,
    days_ago: i32,
) -> std::path::PathBuf {
    let dir = cache::cache_dir(data_dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{episode}.audio"));
    std::fs::copy(src, &path).unwrap();
    let mb = std::fs::metadata(&path).unwrap().len() as f64 / 1024.0 / 1024.0;
    podcasts::upsert_download(
        pool,
        user,
        episode,
        &format!("podcast-audio/{episode}.audio"),
        mb,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE podcast_downloads SET last_accessed_at = now() - make_interval(days => $2)
         WHERE episode_id = $1",
    )
    .bind(episode)
    .bind(days_ago)
    .execute(pool)
    .await
    .unwrap();
    path
}

/// What `main` starts: the cleanup first and then the measuring of what stays cached (so an
/// expired file is deleted, not measured), and the category fill-in with one Apple request.
/// A second start does neither again.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn startup_cleans_up_then_measures_and_fills_missing_categories(pool: PgPool) {
    if !kima_test_support::require_ffmpeg() {
        return;
    }
    let replay = apple().await;
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let mut apple_shows = Vec::new();
    for id in APPLE_IDS {
        apple_shows.push(show(&pool, &format!("https://f.example.invalid/{id}"), Some(id)).await);
    }
    let no_apple_id = show(&pool, "https://f.example.invalid/none", None).await;
    let kept = episode(&pool, no_apple_id, "kept").await;
    let expired = episode(&pool, no_apple_id, "expired").await;
    let listener = user(&pool).await;
    let src = kima_test_support::make_audio(tmp.path(), "src.mp3", Default::default());
    cached(&pool, &data_dir, &src, listener, kept, 1).await;
    let expired_path = cached(&pool, &data_dir, &src, listener, expired, 31).await;

    let report = kima_server::jobs::spawn_podcast_startup(ctx(&pool, &replay.url(), &data_dir))
        .await
        .unwrap();
    assert_eq!(
        report,
        kima_server::jobs::PodcastStartup {
            cleaned: 1,
            measured: 1,
            categories_filled: 3,
        }
    );
    assert!(!expired_path.exists());
    assert_eq!(duration(&pool, kept).await, 2);
    assert_eq!(duration(&pool, expired).await, 4414);
    let lookups = replay
        .asked()
        .iter()
        .filter(|a| a.contains("/lookup"))
        .count();
    assert_eq!(lookups, 1);
    for id in &apple_shows {
        let stored = podcasts::show_by_id(&pool, *id).await.unwrap().unwrap();
        assert_eq!(stored.category_ids, vec![1495]);
    }

    let again = kima_server::jobs::spawn_podcast_startup(ctx(&pool, &replay.url(), &data_dir))
        .await
        .unwrap();
    assert_eq!(again, kima_server::jobs::PodcastStartup::default());
    let lookups = replay
        .asked()
        .iter()
        .filter(|a| a.contains("/lookup"))
        .count();
    assert_eq!(lookups, 1);
}

/// The daily cleanup `main` schedules: one `podcast-cache-cleanup` entry firing at 04:00 UTC
/// (upstream's 24-hour interval) that deletes downloads untouched for 30 days.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_daily_cache_cleanup_is_scheduled_at_four_utc(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let entries =
        kima_server::podcast_schedule_entries(ctx(&pool, "http://127.0.0.1:9", &data_dir));
    assert_eq!(
        entries.iter().map(|(name, _, _)| *name).collect::<Vec<_>>(),
        vec!["podcast-cache-cleanup"]
    );
    let t = time::macros::datetime!(2026-01-01 05:00:00 UTC);
    assert_eq!(
        entries[0].1.next_after(t),
        Some(time::macros::datetime!(2026-01-02 04:00:00 UTC))
    );

    let owner = show(&pool, "https://f.example.invalid/cron", None).await;
    let old = episode(&pool, owner, "old").await;
    let fresh = episode(&pool, owner, "fresh").await;
    let listener = user(&pool).await;
    let src = tmp.path().join("src.audio");
    std::fs::write(&src, b"audio").unwrap();
    let old_path = cached(&pool, &data_dir, &src, listener, old, 31).await;
    let fresh_path = cached(&pool, &data_dir, &src, listener, fresh, 29).await;
    (entries[0].2)(pool.clone()).await.unwrap();
    assert!(!old_path.exists());
    assert!(fresh_path.exists());
    let left: Vec<Uuid> = sqlx::query_scalar("SELECT episode_id FROM podcast_downloads")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, vec![fresh]);
}
