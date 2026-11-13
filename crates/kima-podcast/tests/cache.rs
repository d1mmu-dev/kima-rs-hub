//! M6 Task 12: the episode cache (`podcastDownload.ts` whole file). A real small MP3 served by
//! a loopback [`AudioHost`] stands in for the episode's host; downloads land under a temporary
//! data directory via the real [`kima_podcast::cache`] + `kima_db::podcasts` code against the
//! real (isolated, migrated) database.

use std::sync::Arc;

use kima_core::identify::IdentifySwitch;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_db::users::{self, Registration};
use kima_net::testing::{AudioHost, AudioMode};
use kima_net::{Bases, Net, NetConfig};
use kima_podcast::cache::{self, CacheError};
use kima_podcast::ctx::PodcastCtx;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

async fn user(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(pool)
        .await
        .unwrap();
    let Registration::Created(user) = users::register(pool, Uuid::now_v7(), name, "hash")
        .await
        .unwrap()
    else {
        panic!("registration failed")
    };
    user.id
}

fn ctx(pool: &PgPool, data_dir: &std::path::Path, base: &str) -> PodcastCtx {
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

async fn show_and_episode(pool: &PgPool, audio_url: &str) -> Uuid {
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://cache.example/feed",
            title: "Cache Show",
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
        },
    )
    .await
    .unwrap();
    podcasts::insert_episodes(
        pool,
        show.id,
        &[NewEpisode {
            guid: "ep1".into(),
            title: "Episode One".into(),
            description: None,
            audio_url: audio_url.into(),
            duration: 100,
            published_at: OffsetDateTime::now_utc(),
            episode_number: None,
            season: None,
            image_url: None,
            file_size: Some(999_999_999),
            mime_type: Some("audio/mpeg".into()),
        }],
    )
    .await
    .unwrap();
    sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = 'ep1'")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A tiny real MP3, read into memory: lofty can measure its duration once cached.
fn small_mp3(dir: &std::path::Path) -> Vec<u8> {
    let path = kima_test_support::make_audio(dir, "src.mp3", Default::default());
    std::fs::read(path).unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn downloads_and_caches_a_real_episode(pool: PgPool) {
    if !kima_test_support::require_ffmpeg() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mp3 = small_mp3(tmp.path());
    let host = AudioHost::start(mp3.clone(), AudioMode::Normal).await;
    let episode = show_and_episode(&pool, &format!("{}/e.mp3", host.url())).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &host.url());
    let user = user(&pool, "ana").await;

    assert!(cache::cached_path(&c, episode).await.is_none());
    cache::download(&c, episode, user).await.unwrap();

    let cached = cache::cached_path(&c, episode).await.expect("now cached");
    assert_eq!(std::fs::read(&cached).unwrap(), mp3);
    assert!(cached.ends_with(format!("{episode}.audio")));

    let stored_size: i64 =
        sqlx::query_scalar("SELECT file_size FROM podcast_episodes WHERE id = $1")
            .bind(episode)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_size, mp3.len() as i64);
    // The real audio's length (2 s) replaced the feed's stale 100 s.
    let duration: i32 = sqlx::query_scalar("SELECT duration FROM podcast_episodes WHERE id = $1")
        .bind(episode)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(duration, 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn keeps_a_valid_cache_and_drops_one_whose_file_no_longer_matches(pool: PgPool) {
    if !kima_test_support::require_ffmpeg() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mp3 = small_mp3(tmp.path());
    let host = AudioHost::start(mp3.clone(), AudioMode::Normal).await;
    let episode = show_and_episode(&pool, &format!("{}/e.mp3", host.url())).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &host.url());
    let ana = user(&pool, "ana").await;
    cache::download(&c, episode, ana).await.unwrap();
    let cached = cache::cached_path(&c, episode).await.unwrap();

    // The feed's declared length does not judge the cache (only the download's own size does).
    sqlx::query("UPDATE podcast_episodes SET file_size = 999999999 WHERE id = $1")
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();
    assert!(cache::cached_path(&c, episode).await.is_some());

    // A file that no longer matches what was recorded is incomplete: it and its record go.
    let mut extra = std::fs::read(&cached).unwrap();
    extra.extend_from_slice(&[0u8; 2000]);
    std::fs::write(&cached, extra).unwrap();
    assert!(cache::cached_path(&c, episode).await.is_none());
    assert!(!cached.exists());
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM podcast_downloads WHERE episode_id = $1")
            .bind(episode)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn keeps_nothing_from_a_download_cut_short_of_the_hosts_content_length(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let body = (0..6000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let host = AudioHost::start(body, AudioMode::CutAfter(3000)).await;
    let episode = show_and_episode(&pool, &format!("{}/e.mp3", host.url())).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &host.url());

    let err = cache::download(&c, episode, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CacheError::Incomplete { .. } | CacheError::Io(_)),
        "{err:?}"
    );
    assert!(cache::cached_path(&c, episode).await.is_none());
    assert!(
        !cache::cache_dir(&data_dir)
            .join(format!("{episode}.tmp"))
            .exists()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cached_path_hides_a_file_while_its_download_job_is_pending(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let episode = Uuid::now_v7();
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, "http://127.0.0.1:9");
    // The cache dir and file exist and would validate, but a download is running for it.
    std::fs::create_dir_all(cache::cache_dir(&data_dir)).unwrap();
    std::fs::write(
        cache::cache_dir(&data_dir).join(format!("{episode}.audio")),
        b"abc",
    )
    .unwrap();
    podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://pending.example/feed",
            title: "Pending",
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
    let user = Uuid::now_v7();
    cache::enqueue_download(&pool, episode, user).await.unwrap();
    assert!(cache::cached_path(&c, episode).await.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn download_is_queued_once_per_episode(pool: PgPool) {
    let episode = Uuid::now_v7();
    let user = Uuid::now_v7();
    let first = cache::enqueue_download(&pool, episode, user).await.unwrap();
    assert!(first.is_some());
    let second = cache::enqueue_download(&pool, episode, user).await.unwrap();
    assert_eq!(second, None);
    // A different episode gets its own job.
    let other = cache::enqueue_download(&pool, Uuid::now_v7(), user)
        .await
        .unwrap();
    assert!(other.is_some());
}

async fn two_episodes(pool: &PgPool) -> (Uuid, Uuid) {
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://cleanup.example/feed",
            title: "Cleanup Show",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 2,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    let episode_of = |guid: &str| NewEpisode {
        guid: guid.into(),
        title: "Ep".into(),
        description: None,
        audio_url: "https://a.invalid/x.mp3".into(),
        duration: 100,
        published_at: OffsetDateTime::now_utc(),
        episode_number: None,
        season: None,
        image_url: None,
        file_size: None,
        mime_type: Some("audio/mpeg".into()),
    };
    podcasts::insert_episodes(pool, show.id, &[episode_of("old"), episode_of("fresh")])
        .await
        .unwrap();
    let old: Uuid = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = 'old'")
        .fetch_one(pool)
        .await
        .unwrap();
    let fresh: Uuid = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = 'fresh'")
        .fetch_one(pool)
        .await
        .unwrap();
    (old, fresh)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cleanup_deletes_only_downloads_untouched_for_30_days(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, "http://127.0.0.1:9");
    std::fs::create_dir_all(cache::cache_dir(&data_dir)).unwrap();
    let (old_ep, fresh_ep) = two_episodes(&pool).await;
    for ep in [old_ep, fresh_ep] {
        std::fs::write(
            cache::cache_dir(&data_dir).join(format!("{ep}.audio")),
            vec![0u8; 1024 * 1024],
        )
        .unwrap();
    }
    let user = user(&pool, "ana").await;
    podcasts::upsert_download(
        &pool,
        user,
        old_ep,
        &format!("podcast-audio/{old_ep}.audio"),
        1.0,
    )
    .await
    .unwrap();
    podcasts::upsert_download(
        &pool,
        user,
        fresh_ep,
        &format!("podcast-audio/{fresh_ep}.audio"),
        1.0,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE podcast_downloads SET last_accessed_at = now() - interval '31 days' WHERE episode_id = $1")
        .bind(old_ep)
        .execute(&pool)
        .await
        .unwrap();

    let (deleted, freed_mb) = cache::cleanup(&c).await.unwrap();
    assert_eq!(deleted, 1);
    assert!((freed_mb - 1.0).abs() < 0.001);
    assert!(
        !cache::cache_dir(&data_dir)
            .join(format!("{old_ep}.audio"))
            .exists()
    );
    assert!(
        cache::cache_dir(&data_dir)
            .join(format!("{fresh_ep}.audio"))
            .exists()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn measures_cached_episodes_once_not_on_every_start(pool: PgPool) {
    if !kima_test_support::require_ffmpeg() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(cache::cache_dir(&data_dir)).unwrap();
    let episode = show_and_episode(&pool, "https://a.invalid/1.mp3").await;
    let src = kima_test_support::make_audio(tmp.path(), "src.mp3", Default::default());
    std::fs::copy(
        &src,
        cache::cache_dir(&data_dir).join(format!("{episode}.audio")),
    )
    .unwrap();
    let c = ctx(&pool, &data_dir, "http://127.0.0.1:9");

    assert_eq!(cache::measure_cached_once(&c).await, 1);
    let duration: i32 = sqlx::query_scalar("SELECT duration FROM podcast_episodes WHERE id = $1")
        .bind(episode)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(duration, 2);
    // Nothing left unmeasured: a second start measures nothing more.
    assert_eq!(cache::measure_cached_once(&c).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tries_again_on_later_starts_only_the_files_it_could_not_read(pool: PgPool) {
    if !kima_test_support::require_ffmpeg() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(cache::cache_dir(&data_dir)).unwrap();
    let readable = show_and_episode(&pool, "https://a.invalid/1.mp3").await;
    let unreadable = Uuid::now_v7(); // no episode row: `record_measured_length` reports "no-episode", not stored.
    let src = kima_test_support::make_audio(tmp.path(), "src.mp3", Default::default());
    std::fs::copy(
        &src,
        cache::cache_dir(&data_dir).join(format!("{readable}.audio")),
    )
    .unwrap();
    // Garbage bytes: lofty cannot read this one.
    std::fs::write(
        cache::cache_dir(&data_dir).join(format!("{unreadable}.audio")),
        b"not an mp3 at all, just junk bytes that lofty cannot parse",
    )
    .unwrap();
    let c = ctx(&pool, &data_dir, "http://127.0.0.1:9");

    assert_eq!(cache::measure_cached_once(&c).await, 1);
    // Next start: only the unreadable file is retried (the marker remembers it), and it is
    // still unreadable, so nothing changes.
    assert_eq!(cache::measure_cached_once(&c).await, 0);
}
