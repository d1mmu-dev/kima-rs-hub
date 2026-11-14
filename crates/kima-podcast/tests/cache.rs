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
use kima_podcast::cache::{self, CacheError, DownloadLimits};
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
    assert_eq!(temp_files(&data_dir), Vec::<std::path::PathBuf>::new());
}

/// How [`endless_host`] frames its never-ending body.
#[derive(Clone, Copy)]
enum Framing {
    /// `Transfer-Encoding: chunked`, no `Content-Length`.
    Chunked,
    /// `Content-Length: n` and a raw body that never stops at `n`.
    Declared(u64),
    /// Both headers: `Content-Length: n` and a chunked body that runs past it.
    ChunkedDeclaring(u64),
}

/// A loopback host that answers 200 framed as `framing` and then sends `chunk` bytes every
/// `every`, forever, until the client goes away.
async fn endless_host(
    chunk: usize,
    every: std::time::Duration,
    framing: Framing,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let length = match framing {
                    Framing::Chunked => String::from("Transfer-Encoding: chunked\r\n"),
                    Framing::Declared(n) => format!("Content-Length: {n}\r\n"),
                    Framing::ChunkedDeclaring(n) => {
                        format!("Content-Length: {n}\r\nTransfer-Encoding: chunked\r\n")
                    }
                };
                let start = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\n{length}Connection: close\r\n\r\n"
                );
                if sock.write_all(start.as_bytes()).await.is_err() {
                    return;
                }
                let frame = match framing {
                    Framing::Declared(_) => vec![0xAAu8; chunk],
                    Framing::Chunked | Framing::ChunkedDeclaring(_) => {
                        let mut frame = format!("{chunk:x}\r\n").into_bytes();
                        frame.extend(std::iter::repeat_n(0xAAu8, chunk));
                        frame.extend_from_slice(b"\r\n");
                        frame
                    }
                };
                loop {
                    if sock.write_all(&frame).await.is_err() || sock.flush().await.is_err() {
                        return;
                    }
                    tokio::time::sleep(every).await;
                }
            });
        }
    });
    (url, task)
}

/// Every temp file in the cache dir, whatever attempt wrote it.
fn temp_files(data_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(cache::cache_dir(data_dir)) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "tmp"))
        .collect()
}

/// Polls until a temp file of at least `min_len` bytes exists that is not the file `not`
/// (by inode); returns its inode.
async fn wait_for_temp_file(data_dir: &std::path::Path, min_len: u64, not: Option<u64>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        for path in temp_files(data_dir) {
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() >= min_len && Some(meta.ino()) != not {
                    return meta.ino();
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no temp file appeared"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Nothing of an aborted download is left: no temp file, no cached file, no download record.
async fn assert_nothing_kept(
    c: &kima_podcast::ctx::PodcastCtx,
    data_dir: &std::path::Path,
    episode: Uuid,
) {
    let dir = cache::cache_dir(data_dir);
    assert_eq!(
        temp_files(data_dir),
        Vec::<std::path::PathBuf>::new(),
        "temp file left"
    );
    assert!(
        !dir.join(format!("{episode}.audio")).exists(),
        "cached file kept"
    );
    assert!(cache::cached_path(c, episode).await.is_none());
    let records: i64 =
        sqlx::query_scalar("SELECT count(*) FROM podcast_downloads WHERE episode_id = $1")
            .bind(episode)
            .fetch_one(&c.pool)
            .await
            .unwrap();
    assert_eq!(records, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn aborts_a_download_of_unknown_length_at_the_size_cap(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let (url, _host) =
        endless_host(4096, std::time::Duration::from_millis(1), Framing::Chunked).await;
    let episode = show_and_episode(&pool, &format!("{url}/e.mp3")).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &url);
    let limits = DownloadLimits {
        max_bytes: 64 * 1024,
        ..DownloadLimits::default()
    };

    let started = std::time::Instant::now();
    let err = cache::download_with_limits(&c, episode, Uuid::now_v7(), limits)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CacheError::TooLarge { limit, .. } if limit == 64 * 1024),
        "{err:?}"
    );
    // A host that never stops sending will not stop sending on a retry either.
    assert!(err.is_permanent(), "{err:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert_nothing_kept(&c, &data_dir, episode).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn aborts_a_download_that_trickles_past_its_deadline(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    // A byte trickle well inside kima-net's 30 s idle timeout, so only the whole-download
    // deadline can end it.
    let (url, _host) =
        endless_host(16, std::time::Duration::from_millis(20), Framing::Chunked).await;
    let episode = show_and_episode(&pool, &format!("{url}/e.mp3")).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &url);
    let limits = DownloadLimits {
        whole: std::time::Duration::from_millis(400),
        ..DownloadLimits::default()
    };

    let started = std::time::Instant::now();
    let err = cache::download_with_limits(&c, episode, Uuid::now_v7(), limits)
        .await
        .unwrap_err();
    assert!(matches!(err, CacheError::TimedOut(_)), "{err:?}");
    // A slow host may be faster next time: retried under the usual policy.
    assert!(!err.is_permanent(), "{err:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert_nothing_kept(&c, &data_dir, episode).await;
}

/// A host that declares more than the cap does not lift it: the cap applies to every download.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn caps_a_download_whose_declared_length_is_above_the_ceiling(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let (url, _host) = endless_host(
        4096,
        std::time::Duration::from_millis(1),
        Framing::Declared(10 * 1024 * 1024 * 1024),
    )
    .await;
    let episode = show_and_episode(&pool, &format!("{url}/e.mp3")).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &url);
    let limits = DownloadLimits {
        max_bytes: 64 * 1024,
        whole: std::time::Duration::from_secs(5),
        ..DownloadLimits::default()
    };

    let err = cache::download_with_limits(&c, episode, Uuid::now_v7(), limits)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CacheError::TooLarge { limit, .. } if limit == 64 * 1024),
        "{err:?}"
    );
    assert_nothing_kept(&c, &data_dir, episode).await;
}

/// A host that declares a `Content-Length` and then sends a longer (chunked) body is cut off
/// at the declared length plus the tolerance.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn aborts_a_body_longer_than_its_declared_content_length(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let (url, _host) = endless_host(
        4096,
        std::time::Duration::from_millis(1),
        Framing::ChunkedDeclaring(10_000),
    )
    .await;
    let episode = show_and_episode(&pool, &format!("{url}/e.mp3")).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &url);
    let limits = DownloadLimits {
        whole: std::time::Duration::from_secs(5),
        ..DownloadLimits::default()
    };

    let err = cache::download_with_limits(&c, episode, Uuid::now_v7(), limits)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CacheError::TooLarge { limit, .. } if limit == 10_100),
        "{err:?}"
    );
    assert_nothing_kept(&c, &data_dir, episode).await;
}

/// A download whose future is dropped midway (a cancelled job, an aborted task, a panic
/// unwinding through it) leaves no temp file behind.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn dropping_a_download_midway_leaves_no_temp_file(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let (url, _host) =
        endless_host(16, std::time::Duration::from_millis(20), Framing::Chunked).await;
    let episode = show_and_episode(&pool, &format!("{url}/e.mp3")).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &url);

    let task = tokio::spawn({
        let c = c.clone();
        async move { cache::download(&c, episode, Uuid::now_v7()).await }
    });
    wait_for_temp_file(&data_dir, 1, None).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    assert_nothing_kept(&c, &data_dir, episode).await;
}

/// A loopback host that answers every request with `body` (and its `Content-Length`), sends
/// the first half, then waits for its own gate (the n-th connection waits on `gates[n]`)
/// before sending the rest.
async fn gated_host(
    body: Vec<u8>,
    connections: usize,
) -> (String, Arc<Vec<tokio::sync::Semaphore>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let gates: Arc<Vec<_>> = Arc::new(
        (0..connections)
            .map(|_| tokio::sync::Semaphore::new(0))
            .collect(),
    );
    let body = Arc::new(body);
    tokio::spawn({
        let gates = gates.clone();
        async move {
            for n in 0..connections {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (gates, body) = (gates.clone(), body.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let start = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n",
                        body.len()
                    );
                    let half = body.len() / 2;
                    if sock.write_all(start.as_bytes()).await.is_err()
                        || sock.write_all(&body[..half]).await.is_err()
                        || sock.flush().await.is_err()
                    {
                        return;
                    }
                    let Ok(_permit) = gates[n].acquire().await else {
                        return;
                    };
                    let _ = sock.write_all(&body[half..]).await;
                    let _ = sock.flush().await;
                });
            }
        }
    });
    (url, gates)
}

/// Two attempts at the same episode (a lost lease redelivered the job while the first worker
/// still runs) never publish each other's partial file: each writes its own temp file and
/// renames only that one in, so whichever finishes, the cached file is complete.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn two_concurrent_attempts_publish_one_complete_file(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let body = (0..64 * 1024u32)
        .map(|i| (i % 251) as u8)
        .collect::<Vec<_>>();
    let half = (body.len() / 2) as u64;
    let (url, gates) = gated_host(body.clone(), 2).await;
    let episode = show_and_episode(&pool, &format!("{url}/e.mp3")).await;
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, &url);
    let user = user(&pool, "ana").await;

    let first = tokio::spawn({
        let c = c.clone();
        async move { cache::download(&c, episode, user).await }
    });
    let first_file = wait_for_temp_file(&data_dir, half, None).await;
    let second = tokio::spawn({
        let c = c.clone();
        async move { cache::download(&c, episode, user).await }
    });
    wait_for_temp_file(&data_dir, half, Some(first_file)).await;

    // The first attempt finishes while the second is still midway, then the second finishes.
    gates[0].add_permits(1);
    let first = first.await.unwrap();
    let after_first = std::fs::read(cache::cache_dir(&data_dir).join(format!("{episode}.audio")));
    gates[1].add_permits(1);
    let second = second.await.unwrap();

    assert!(first.is_ok(), "{first:?}");
    let after_first = after_first.unwrap();
    assert!(
        after_first == body,
        "a partial file was published ({} of {} bytes)",
        after_first.len(),
        body.len()
    );
    assert!(second.is_ok(), "{second:?}");
    let cached = cache::cached_path(&c, episode).await.expect("cached");
    assert!(
        std::fs::read(cached).unwrap() == body,
        "the cached file is not the body"
    );
    assert_eq!(temp_files(&data_dir), Vec::<std::path::PathBuf>::new());
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

/// A download that is running counts as one in progress (upstream's `isDownloading` is true
/// from the start of the download to its end): a second job for the same episode would run
/// beside it and remove its temp file.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn download_is_not_queued_again_while_one_runs(pool: PgPool) {
    let episode = Uuid::now_v7();
    let user = Uuid::now_v7();
    let first = cache::enqueue_download(&pool, episode, user)
        .await
        .unwrap()
        .expect("a fresh job");
    let claimed =
        kima_jobs::queue::claim(&pool, cache::QUEUE, 10, std::time::Duration::from_secs(60))
            .await
            .unwrap();
    assert_eq!(
        claimed.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![first]
    );
    assert_eq!(
        cache::enqueue_download(&pool, episode, user).await.unwrap(),
        None
    );
    // Once it is done, a later play may queue the episode again.
    assert!(
        kima_jobs::queue::complete(&pool, &claimed[0])
            .await
            .unwrap()
    );
    assert!(
        cache::enqueue_download(&pool, episode, user)
            .await
            .unwrap()
            .is_some()
    );
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

/// Temp files a hard kill left behind are reclaimed by the cleanup once they are a day old,
/// unless that episode's download is queued or running; fresh ones and other files stay.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cleanup_removes_orphaned_temp_files_older_than_a_day(pool: PgPool) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let c = ctx(&pool, &data_dir, "http://127.0.0.1:9");
    let dir = cache::cache_dir(&data_dir);
    std::fs::create_dir_all(&dir).unwrap();
    let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    let write = |name: String, old: bool| {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        if old {
            file.set_modified(two_days_ago).unwrap();
        }
        path
    };
    let orphan = write(format!("{}.{}.tmp", Uuid::now_v7(), Uuid::now_v7()), true);
    let legacy = write(format!("{}.tmp", Uuid::now_v7()), true);
    let fresh = write(format!("{}.{}.tmp", Uuid::now_v7(), Uuid::now_v7()), false);
    let busy_episode = Uuid::now_v7();
    cache::enqueue_download(&pool, busy_episode, Uuid::now_v7())
        .await
        .unwrap()
        .expect("a fresh job");
    let busy = write(format!("{busy_episode}.{}.tmp", Uuid::now_v7()), true);
    let other = write("notes.txt".into(), true);

    cache::cleanup(&c).await.unwrap();

    assert!(!orphan.exists(), "an old orphan stays");
    assert!(!legacy.exists(), "an old single-name temp file stays");
    assert!(fresh.exists(), "a fresh temp file was removed");
    assert!(busy.exists(), "a running download's temp file was removed");
    assert!(other.exists(), "a file that is not a temp file was removed");
}
