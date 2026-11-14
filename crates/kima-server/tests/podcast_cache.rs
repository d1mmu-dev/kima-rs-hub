//! M6 Task 12: the `podcast-cache` queue end to end — a real job claimed and run by the real
//! worker downloads a real small MP3 from a loopback host and leaves it cached.

use std::sync::Arc;
use std::time::Duration;

use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_db::users::{self, Registration};
use kima_net::testing::{AudioHost, AudioMode};
use kima_net::{Bases, Net, NetConfig};
use kima_server::jobs::{Registry, drain, spawn_all};
use tokio_util::sync::CancellationToken;
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

async fn wait_for_job_state(pool: &PgPool, id: Uuid, want: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        if state == want || tokio::time::Instant::now() > deadline {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn episode_at(pool: &PgPool, audio_url: &str) -> Uuid {
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://e2e.example/feed",
            title: "E2E Show",
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
            guid: "e2e".into(),
            title: "E2E Episode".into(),
            description: None,
            audio_url: audio_url.into(),
            duration: 100,
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
    let episode: Uuid = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = 'e2e'")
        .fetch_one(pool)
        .await
        .unwrap();
    episode
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_queued_download_job_caches_the_episode(pool: PgPool) {
    if !kima_test_support::require_ffmpeg() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = kima_test_support::make_audio(tmp.path(), "src.mp3", Default::default());
    let mp3 = std::fs::read(&src).unwrap();
    let host = AudioHost::start(mp3.clone(), AudioMode::Normal).await;

    let episode = episode_at(&pool, &format!("{}/e.mp3", host.url())).await;
    let ana = user(&pool, "ana").await;

    let data_dir = tmp.path().join("data");
    let ctx = kima_podcast::ctx::PodcastCtx {
        pool: pool.clone(),
        net: Arc::new(Net::new(
            NetConfig::for_tests(Bases::all(&host.url())),
            pool.clone(),
            IdentifySwitch::new(false, false),
        )),
        data_dir: data_dir.clone(),
    };
    let mut registry = Registry::new();
    registry.register_podcast_cache(ctx.clone());
    let shutdown = CancellationToken::new();
    let workers = spawn_all(&pool, registry, shutdown.clone());

    let job = kima_podcast::cache::enqueue_download(&pool, episode, ana)
        .await
        .unwrap()
        .expect("a fresh job");
    assert_eq!(wait_for_job_state(&pool, job, "done").await, "done");

    shutdown.cancel();
    drain(workers, Duration::from_secs(5)).await;

    let cached = kima_podcast::cache::cached_path(&ctx, episode)
        .await
        .expect("now cached by the real worker");
    assert_eq!(std::fs::read(cached).unwrap(), mp3);
}

/// A loopback host that answers 200 with a chunked body and then trickles 16 bytes every
/// 20 ms, forever, until the client goes away.
async fn trickling_host() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let start = "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\n\
                             Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
                if sock.write_all(start.as_bytes()).await.is_err() {
                    return;
                }
                let mut frame = b"10\r\n".to_vec();
                frame.extend([0xAAu8; 16]);
                frame.extend_from_slice(b"\r\n");
                while sock.write_all(&frame).await.is_ok() && sock.flush().await.is_ok() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            });
        }
    });
    url
}

/// Shutdown during a download gives the attempt back (the job is queued again, its attempt
/// not counted) and leaves no temp file behind.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn shutdown_during_a_download_gives_the_attempt_back_and_leaves_no_temp_file(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let url = trickling_host().await;
    let episode = episode_at(&pool, &format!("{url}/e.mp3")).await;
    let ana = user(&pool, "ana").await;
    let data_dir = tmp.path().join("data");
    let ctx = kima_podcast::ctx::PodcastCtx {
        pool: pool.clone(),
        net: Arc::new(Net::new(
            NetConfig::for_tests(Bases::all(&url)),
            pool.clone(),
            IdentifySwitch::new(false, false),
        )),
        data_dir: data_dir.clone(),
    };
    let mut registry = Registry::new();
    registry.register_podcast_cache(ctx.clone());
    let shutdown = CancellationToken::new();
    let workers = spawn_all(&pool, registry, shutdown.clone());

    let job = kima_podcast::cache::enqueue_download(&pool, episode, ana)
        .await
        .unwrap()
        .expect("a fresh job");
    assert_eq!(wait_for_job_state(&pool, job, "running").await, "running");
    let dir = kima_podcast::cache::cache_dir(&data_dir);
    let temp_files = || -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "tmp"))
                    .collect()
            })
            .unwrap_or_default()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while temp_files().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the download never started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    shutdown.cancel();
    drain(workers, Duration::from_secs(5)).await;

    let (state, attempts): (String, i32) =
        sqlx::query_as("SELECT state, attempts FROM jobs WHERE id = $1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), attempts), ("queued", 0));
    assert_eq!(temp_files(), Vec::<std::path::PathBuf>::new());
}
