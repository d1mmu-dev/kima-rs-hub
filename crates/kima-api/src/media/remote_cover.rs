//! The external-image proxy behind `GET /api/library/cover-art?url=` (Task 15, D-210). A
//! podcast show or episode's cover lives on its feed's own host, so a client never talks to
//! that host directly: this fetches it once through `kima_net::Net::get_bytes` (source
//! `images`, the SSRF rule and 15 MiB cap; interactive images have no minimum size), validates and optionally
//! resizes it (`kima_library::artwork::remote_cover_image`), and caches the result in memory keyed
//! by (url, width) so a later request or a `304`'s `If-None-Match` round trip never re-fetches.
//! Upstream keeps the same cache in Redis; kima-rs uses bounded per-process memory (D-210).
//!
//! Cache hits have 32 independent, non-waiting response slots and share cached bytes; overload
//! returns a retryable error. Misses have two FIFO slots and at most 32 waiting requests, with
//! a 15-second admission deadline (one fetch budget). Miss slots remain held through response
//! consumption; detached blocking tasks retain their slot after cancellation. Cache traffic cannot
//! starve misses or vice versa. A page exceeding either bound can receive a retryable 503.
//! The cache limits accounted keys, metadata and payloads to 256 MiB/256 entries;
//! evicted payloads can remain alive in at most 34 admitted responses. This is not a total
//! process-memory bound: decoder scratch allocations, HTTP/TLS buffers, allocator overhead,
//! queued requests and memory retained elsewhere are outside the cache byte accounting.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kima_net::Outcome;
use md5::{Digest, Md5};

use crate::state::AppState;

/// How long a fetch may take before the caller gets a 502.
const FETCH_BUDGET: Duration = Duration::from_secs(15);
/// A found image is served from the memory cache for this long before the next request re-fetches.
const FOUND_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// A confirmed-missing image (a true 404 or bytes nothing here can decode) is
/// remembered this long, so a dead link is not retried on every page view (upstream: 1 h).
const NOT_FOUND_TTL: Duration = Duration::from_secs(3600);

pub struct RemoteCover {
    pub bytes: axum::body::Bytes,
    pub permit: Option<std::sync::Arc<tokio::sync::OwnedSemaphorePermit>>,
    pub content_type: String,
    pub etag: String,
}

/// What [`remote_cover`] found.
pub enum RemoteCoverResult {
    Found(RemoteCover),
    InvalidAddress,
    /// No usable image: absent or bytes this build cannot decode.
    NotFound,
    /// The host or network could not be reached right now; not cached, try again shortly.
    Unavailable,
    /// Admission is saturated; the request may be retried.
    Busy,
}

#[derive(Clone)]
struct Sidecar {
    content_type: String,
    etag: String,
    fetched_at: u64,
    not_found: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// This cache is process-local: no process reads or writes a shared directory. Publication
// is one synchronous operation under the mutex, so cancellation cannot interrupt eviction.
const MAX_ENTRIES: usize = 256;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;

/// Entries carry their publication sequence number: eviction drops the lowest one, so the
/// oldest publication goes first even when a whole page shares one `fetched_at` second.
#[derive(Default)]
struct Cache {
    entries: std::collections::HashMap<PathBuf, (Sidecar, axum::body::Bytes, u64)>,
    next_seq: u64,
}

impl Cache {
    fn prune(&mut self, now: u64) {
        self.entries.retain(|_, (meta, _, _)| {
            let ttl = if meta.not_found {
                NOT_FOUND_TTL
            } else {
                FOUND_TTL
            };
            now.saturating_sub(meta.fetched_at) < ttl.as_secs()
        });
    }

    fn size(&self) -> usize {
        self.entries
            .iter()
            .map(|(key, (meta, bytes, _))| {
                key.as_os_str().len() + meta.content_type.len() + meta.etag.len() + bytes.len()
            })
            .sum()
    }

    fn insert(
        &mut self,
        key: &Path,
        bytes: Option<&[u8]>,
        sidecar: &Sidecar,
        now: u64,
        max_entries: usize,
        max_bytes: usize,
    ) {
        self.prune(now);
        let bytes = bytes.unwrap_or_default();
        let size =
            key.as_os_str().len() + sidecar.content_type.len() + sidecar.etag.len() + bytes.len();
        if size > MAX_ENTRY_BYTES || size > max_bytes || max_entries == 0 {
            return;
        }
        self.entries.remove(key);
        while self.entries.len() >= max_entries || self.size() > max_bytes - size {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, _, seq))| *seq)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.next_seq += 1;
        self.entries.insert(
            key.to_owned(),
            (
                sidecar.clone(),
                axum::body::Bytes::copy_from_slice(bytes),
                self.next_seq,
            ),
        );
    }
}

static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
fn cache() -> &'static std::sync::Mutex<Cache> {
    CACHE.get_or_init(|| std::sync::Mutex::new(Cache::default()))
}

fn read_cache(_bin: &Path, json: &Path) -> Option<RemoteCoverResult> {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.prune(now_secs());
    let (sidecar, bytes, _) = cache.entries.get(json)?;
    if sidecar.not_found {
        Some(RemoteCoverResult::NotFound)
    } else {
        Some(RemoteCoverResult::Found(RemoteCover {
            bytes: bytes.clone(),
            permit: None,
            content_type: sidecar.content_type.clone(),
            etag: sidecar.etag.clone(),
        }))
    }
}

fn write_cache(_dir: &Path, _bin: &Path, json: &Path, bytes: Option<&[u8]>, sidecar: &Sidecar) {
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(
        json,
        bytes,
        sidecar,
        now_secs(),
        MAX_ENTRIES,
        MAX_BYTES,
    );
}

async fn blocking_decode<T: Send + 'static>(
    permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
}

/// Fetches, validates/resizes and memory-caches a remote cover image. Width is the requested
/// resize target (`None` or outside kima_library's resize range serves the original bytes).
pub async fn remote_cover(state: &AppState, url: &str, width: Option<u32>) -> RemoteCoverResult {
    remote_cover_with_decoder(state, url, width, kima_library::artwork::remote_cover_image).await
}

async fn remote_cover_with_decoder(
    state: &AppState,
    url: &str,
    width: Option<u32>,
    decode: impl FnOnce(
        &[u8],
        Option<u32>,
    )
        -> Result<Option<(Vec<u8>, &'static str)>, kima_library::artwork::ArtworkError>
    + Send
    + 'static,
) -> RemoteCoverResult {
    let Some(data_dir) = state.data_dir.clone() else {
        return RemoteCoverResult::NotFound;
    };
    let (bin, json) = kima_library::artwork::remote_cover_cache_paths(&data_dir, url, width);
    let dir = bin.parent().expect("cache path has a parent").to_path_buf();

    // Reserve before cloning the cached Bytes so retained evictions stay bounded.
    static HIT_GATE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let hit_gate = HIT_GATE
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(32)))
        .clone();
    if let Ok(hit_permit) = hit_gate.try_acquire_owned() {
        if let Some(mut result) = read_cache(&bin, &json) {
            if let RemoteCoverResult::Found(ref mut cover) = result {
                cover.permit = Some(std::sync::Arc::new(hit_permit));
            }
            return result;
        }
    } else if read_cache(&bin, &json).is_some() {
        return RemoteCoverResult::Busy;
    }

    static GATE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    static WAITERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let Ok(waiter) = WAITERS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(32)))
        .clone()
        .try_acquire_owned()
    else {
        return RemoteCoverResult::Busy;
    };
    let gate = GATE
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone();
    let Ok(Ok(permit)) = tokio::time::timeout(FETCH_BUDGET, gate.acquire_owned()).await else {
        return RemoteCoverResult::Busy;
    };
    drop(waiter);
    let permit = std::sync::Arc::new(permit);
    let cached = {
        let _permit = permit.clone();
        let (bin, json) = (bin.clone(), json.clone());
        tokio::task::spawn_blocking(move || {
            let _permit = _permit;
            read_cache(&bin, &json)
        })
        .await
        .ok()
        .flatten()
    };
    if let Some(mut result) = cached {
        if let RemoteCoverResult::Found(ref mut cover) = result {
            cover.permit = Some(permit);
        }
        return result;
    }

    let fetched = tokio::time::timeout(FETCH_BUDGET, state.net.get_bytes_user_facing(url)).await;
    let raw = match fetched {
        Ok(Outcome::Answered(bytes)) => bytes,
        Ok(Outcome::Permanent(f))
            if matches!(f.code.as_str(), "BAD_URL" | "BAD_SCHEME" | "NOT_PUBLIC") =>
        {
            return RemoteCoverResult::InvalidAddress;
        }
        Ok(Outcome::Absent(_)) | Ok(Outcome::Permanent(_)) => {
            not_found(dir, bin, json, permit.clone()).await;
            return RemoteCoverResult::NotFound;
        }
        // Transient, RateLimited, timeout: the host or the network itself, not cached.
        _ => return RemoteCoverResult::Unavailable,
    };

    let decode_permit = permit.clone();
    let processed = blocking_decode(decode_permit, move || decode(&raw, width)).await;
    let processed = match processed {
        Ok(value) => value,
        Err(_) => return RemoteCoverResult::Unavailable,
    };
    let Ok(Some((bytes, mime))) = processed else {
        not_found(dir, bin, json, permit.clone()).await;
        return RemoteCoverResult::NotFound;
    };
    let bytes = axum::body::Bytes::from(bytes);
    let etag = hex::encode(Md5::digest(&bytes));
    let sidecar = Sidecar {
        content_type: mime.to_string(),
        etag: etag.clone(),
        fetched_at: now_secs(),
        not_found: false,
    };
    {
        let (dir, bin, json, bytes) = (dir.clone(), bin.clone(), json.clone(), bytes.clone());
        let cache_permit = permit.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _permit = cache_permit;
            write_cache(&dir, &bin, &json, Some(&bytes), &sidecar)
        })
        .await;
    }
    RemoteCoverResult::Found(RemoteCover {
        bytes,
        permit: Some(permit),
        content_type: mime.to_string(),
        etag,
    })
}

async fn not_found(
    dir: PathBuf,
    bin: PathBuf,
    json: PathBuf,
    permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
) {
    let sidecar = Sidecar {
        content_type: String::new(),
        etag: String::new(),
        fetched_at: now_secs(),
        not_found: true,
    };
    let _ = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        write_cache(&dir, &bin, &json, None, &sidecar)
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache and admission gates are process-wide, so every test that publishes to the
    /// global cache or calls the proxy holds this lock: an unrelated test filling the cache
    /// would otherwise evict the entries a proxy test expects to hit.
    static PROXY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn slow_page_fetches_do_not_block_hits_or_reject_cold_request() {
        use kima_core::identify::IdentifySwitch;
        use kima_net::{Bases, Net, NetConfig};
        use std::sync::Arc;
        let _guard = PROXY_TEST_LOCK.lock().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4, 4)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = Arc::new(png.into_inner());
        let app = axum::Router::new().route(
            "/{id}",
            axum::routing::get({
                let release = release.clone();
                move |axum::extract::Path(id): axum::extract::Path<String>| {
                    let (tx, release, png) = (tx.clone(), release.clone(), png.clone());
                    async move {
                        tx.send(id.clone()).unwrap();
                        if id != "warm" {
                            release.acquire().await.unwrap().forget();
                        }
                        ([("content-type", "image/png")], png.as_ref().clone())
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut cfg = NetConfig::for_tests(Bases::all(&base));
        cfg.allow_private_images = true;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let data = tempfile::tempdir().unwrap();
        let state = AppState::new(pool.clone(), "test-secret")
            .with_net(Arc::new(Net::new(
                cfg,
                pool,
                IdentifySwitch::new(false, true),
            )))
            .with_data_dir(data.path().into());
        let warm = format!("{base}/warm");
        assert!(matches!(
            remote_cover(&state, &warm, None).await,
            RemoteCoverResult::Found(_)
        ));
        assert_eq!(rx.recv().await.unwrap(), "warm");
        let mut slow = Vec::new();
        for id in ["one", "two"] {
            let state = state.clone();
            let url = format!("{base}/{id}");
            slow.push(tokio::spawn(async move {
                remote_cover(&state, &url, None).await
            }));
        }
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();
        let cold = tokio::spawn({
            let state = state.clone();
            let url = format!("{base}/cold");
            async move { remote_cover(&state, &url, None).await }
        });
        let hit = tokio::time::timeout(Duration::from_secs(2), remote_cover(&state, &warm, None))
            .await
            .unwrap();
        assert!(
            matches!(hit, RemoteCoverResult::Found(_)),
            "cached request was blocked by slow fetches"
        );
        drop(hit);
        let mut hits = Vec::new();
        for _ in 0..32 {
            let hit = remote_cover(&state, &warm, None).await;
            assert!(matches!(hit, RemoteCoverResult::Found(_)));
            hits.push(hit);
        }
        assert!(matches!(
            remote_cover(&state, &warm, None).await,
            RemoteCoverResult::Busy
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(650), async {
                while !cold.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err(),
            "cold request rejected before fetch budget"
        );
        release.add_permits(2);
        for task in slow {
            drop(task.await.unwrap());
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap(),
            "cold"
        );
        release.add_permits(1);
        assert!(matches!(cold.await.unwrap(), RemoteCoverResult::Found(_)));
        server.abort();
    }

    #[tokio::test]
    async fn parallel_covers_wait_for_admission() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        use base64::Engine;
        use kima_core::identify::IdentifySwitch;
        use kima_net::testing::{Entry, Recording, Replay};
        use kima_net::{Bases, Net, NetConfig};
        use std::sync::Arc;
        let entries = (0..8)
            .map(|i| Entry {
                method: "GET".into(),
                url: format!("/{i}"),
                status: 200,
                headers: [("content-type".into(), "image/jpeg".into())].into(),
                body: serde_json::Value::Null,
                body_base64: Some(base64::engine::general_purpose::STANDARD.encode(vec![42; 2048])),
                request_body_sha256: None,
            })
            .collect();
        let replay = Replay::start(Recording(entries)).await;
        let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
        cfg.allow_private_images = true;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let data = tempfile::tempdir().unwrap();
        let state = AppState::new(pool.clone(), "test-secret")
            .with_net(Arc::new(Net::new(
                cfg,
                pool,
                IdentifySwitch::new(false, true),
            )))
            .with_data_dir(data.path().into());
        let mut tasks = Vec::new();
        for i in 0..8 {
            let state = state.clone();
            let url = format!("{}/{i}", replay.url());
            tasks.push(tokio::spawn(async move {
                let result = remote_cover_with_decoder(&state, &url, None, |bytes, _| {
                    std::thread::sleep(Duration::from_millis(50));
                    Ok(Some((bytes.to_vec(), "image/jpeg")))
                })
                .await;
                assert!(matches!(result, RemoteCoverResult::Found(_)));
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(replay.asked().len(), 8);
    }

    #[tokio::test]
    async fn real_parallel_images_and_cache_hit_wait_then_complete() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        use base64::Engine;
        use kima_core::identify::IdentifySwitch;
        use kima_net::testing::{Entry, Recording, Replay};
        use kima_net::{Bases, Net, NetConfig};
        use std::sync::Arc;
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4, 4)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        let entries = (0..7)
            .map(|i| Entry {
                method: "GET".into(),
                url: format!("/real-{i}"),
                status: 200,
                headers: [("content-type".into(), "image/png".into())].into(),
                body: serde_json::Value::Null,
                body_base64: Some(encoded.clone()),
                request_body_sha256: None,
            })
            .collect();
        let replay = Replay::start(Recording(entries)).await;
        let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
        cfg.allow_private_images = true;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let data = tempfile::tempdir().unwrap();
        let state = AppState::new(pool.clone(), "test-secret")
            .with_net(Arc::new(Net::new(
                cfg,
                pool,
                IdentifySwitch::new(false, true),
            )))
            .with_data_dir(data.path().into());
        let a = remote_cover(&state, &format!("{}/real-0", replay.url()), None).await;
        let b = remote_cover(&state, &format!("{}/real-1", replay.url()), None).await;
        assert!(
            matches!(a, RemoteCoverResult::Found(_)) && matches!(b, RemoteCoverResult::Found(_))
        );
        let hit = remote_cover(&state, &format!("{}/real-0", replay.url()), None).await;
        assert!(matches!(hit, RemoteCoverResult::Found(_)));
        drop(hit);
        let mut tasks = Vec::new();
        for i in [2, 3, 4, 5, 6] {
            let state = state.clone();
            let url = format!("{}/real-{i}", replay.url());
            tasks.push(tokio::spawn(async move {
                assert!(matches!(
                    remote_cover(&state, &url, None).await,
                    RemoteCoverResult::Found(_)
                ));
            }));
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(tasks.iter().all(|t| !t.is_finished()));
        drop(a);
        drop(b);
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(replay.asked().len(), 7, "cache hit must not fetch");
    }

    #[tokio::test]
    async fn saturated_proxy_rejects_without_fetching_and_recovers() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        cache().lock().unwrap().entries.clear();
        use base64::Engine;
        use kima_core::identify::IdentifySwitch;
        use kima_net::testing::{Entry, Recording, Replay};
        use kima_net::{Bases, Net, NetConfig};
        use std::sync::Arc;

        let entry = |path: &str| Entry {
            method: "GET".into(),
            url: path.into(),
            status: 200,
            headers: [("content-type".into(), "image/jpeg".into())].into(),
            body: serde_json::Value::Null,
            body_base64: Some(base64::engine::general_purpose::STANDARD.encode(vec![42; 2048])),
            request_body_sha256: None,
        };
        let replay = Replay::start(Recording(vec![
            entry("/a"),
            entry("/b"),
            entry("/c"),
            entry("/d"),
            entry("/e"),
            entry("/f"),
        ]))
        .await;
        let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
        cfg.allow_private_images = true;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let data = tempfile::tempdir().unwrap();
        let state = AppState::new(pool.clone(), "test-secret")
            .with_net(Arc::new(Net::new(
                cfg,
                pool,
                IdentifySwitch::new(false, true),
            )))
            .with_data_dir(data.path().into());
        let decode = |bytes: &[u8], _| Ok(Some((bytes.to_vec(), "image/jpeg")));
        let a =
            remote_cover_with_decoder(&state, &format!("{}/a", replay.url()), None, decode).await;
        let b =
            remote_cover_with_decoder(&state, &format!("{}/b", replay.url()), None, decode).await;
        assert!(matches!(a, RemoteCoverResult::Found(_)));
        assert!(matches!(b, RemoteCoverResult::Found(_)));
        let c_url = format!("{}/c", replay.url());
        let rejected = tokio::time::timeout(
            Duration::from_secs(17),
            remote_cover_with_decoder(&state, &c_url, None, decode),
        )
        .await
        .expect("admission hung with two retained responses");
        assert!(matches!(rejected, RemoteCoverResult::Busy));
        assert_eq!(replay.asked().len(), 2);
        drop(a);
        let c = remote_cover_with_decoder(&state, &c_url, None, decode).await;
        assert!(matches!(c, RemoteCoverResult::Found(_)));
        assert_eq!(replay.asked().len(), 3);
        drop(b);
        drop(c);

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocked_state = state.clone();
        let d_url = format!("{}/d", replay.url());
        let task = tokio::spawn(async move {
            remote_cover_with_decoder(&blocked_state, &d_url, None, move |bytes, _| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Some((bytes.to_vec(), "image/jpeg")))
            })
            .await
        });
        started_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        let held =
            remote_cover_with_decoder(&state, &format!("{}/f", replay.url()), None, decode).await;
        assert!(matches!(held, RemoteCoverResult::Found(_)));
        let e_url = format!("{}/e", replay.url());
        assert!(matches!(
            remote_cover_with_decoder(&state, &e_url, None, decode).await,
            RemoteCoverResult::Busy
        ));
        assert_eq!(replay.asked().len(), 5);
        release_tx.send(()).unwrap();
        let recovered = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let result = remote_cover_with_decoder(&state, &e_url, None, decode).await;
                if matches!(result, RemoteCoverResult::Found(_)) {
                    break result;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled decode left a stale permit");
        assert!(matches!(recovered, RemoteCoverResult::Found(_)));
        assert_eq!(replay.asked().len(), 6);
    }

    #[tokio::test]
    async fn decoder_panic_is_transient_and_not_negative_cached() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        use base64::Engine;
        use kima_core::identify::IdentifySwitch;
        use kima_net::testing::{Entry, Recording, Replay};
        use kima_net::{Bases, Net, NetConfig};
        use std::sync::Arc;

        let entry = Entry {
            method: "GET".into(),
            url: "/panic.jpg".into(),
            status: 200,
            headers: [("content-type".into(), "image/jpeg".into())].into(),
            body: serde_json::Value::Null,
            body_base64: Some(base64::engine::general_purpose::STANDARD.encode(vec![42; 2048])),
            request_body_sha256: None,
        };
        let replay = Replay::start(Recording(vec![entry])).await;
        let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
        cfg.allow_private_images = true;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let net = Net::new(cfg, pool.clone(), IdentifySwitch::new(false, true));
        let data = tempfile::tempdir().unwrap();
        let state = AppState::new(pool, "test-secret")
            .with_net(Arc::new(net))
            .with_data_dir(data.path().into());
        let url = format!("{}/panic.jpg", replay.url());
        let first =
            remote_cover_with_decoder(&state, &url, None, |_, _| panic!("injected decoder panic"))
                .await;
        assert!(matches!(first, RemoteCoverResult::Unavailable));
        let second = remote_cover_with_decoder(&state, &url, None, |bytes, _| {
            Ok(Some((bytes.to_vec(), "image/jpeg")))
        })
        .await;
        assert!(matches!(second, RemoteCoverResult::Found(_)));
        assert_eq!(
            replay.asked().len(),
            2,
            "panic must not create a negative cache entry"
        );
    }

    #[tokio::test]
    async fn cancellation_keeps_decode_slot_until_blocking_work_finishes() {
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = std::sync::Arc::new(gate.clone().acquire_owned().await.unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(blocking_decode(permit, {
            let barrier = barrier.clone();
            move || {
                started_tx.send(()).unwrap();
                barrier.wait();
            }
        }));
        started_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(gate.clone().try_acquire_owned().is_err());
        barrier.wait();
        let _permit = tokio::time::timeout(Duration::from_secs(5), gate.clone().acquire_owned())
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn cache_hits_share_the_same_allocation() {
        let _guard = PROXY_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("shared.json");
        let meta = Sidecar {
            content_type: "image/png".into(),
            etag: "x".into(),
            fetched_at: now_secs(),
            not_found: false,
        };
        write_cache(dir.path(), &key, &key, Some(&[1; 1024]), &meta);
        let Some(RemoteCoverResult::Found(first)) = read_cache(&key, &key) else {
            panic!()
        };
        let Some(RemoteCoverResult::Found(second)) = read_cache(&key, &key) else {
            panic!()
        };
        assert_eq!(
            first.bytes.as_ptr(),
            second.bytes.as_ptr(),
            "cache hits copied the image"
        );
    }

    #[test]
    fn memory_cache_evicts_to_byte_budget() {
        let mut cache = Cache::default();
        let meta = Sidecar {
            content_type: String::new(),
            etag: String::new(),
            fetched_at: 100,
            not_found: false,
        };
        cache.insert(Path::new("a"), Some(&[1; 8]), &meta, 100, 10, 15);
        cache.insert(Path::new("b"), Some(&[2; 8]), &meta, 100, 10, 15);
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.entries.contains_key(Path::new("b")));
        assert!(cache.size() <= 15);
    }

    #[test]
    fn memory_cache_evicts_in_publication_order_within_one_second() {
        // Every entry shares one `fetched_at` second, as a page of covers does. Eviction must
        // still drop the oldest publication, never the entry published just before.
        let mut cache = Cache::default();
        let meta = Sidecar {
            content_type: String::new(),
            etag: String::new(),
            fetched_at: 100,
            not_found: false,
        };
        for i in 0..100 {
            cache.insert(Path::new(&format!("{i}")), Some(b"x"), &meta, 100, 2, 1024);
            if i > 0 {
                assert!(
                    cache.entries.contains_key(Path::new(&format!("{}", i - 1))),
                    "publishing {i} evicted the entry published just before it"
                );
            }
        }
        assert_eq!(cache.entries.len(), 2);
    }

    #[test]
    fn memory_cache_expires_positive_and_negative_at_their_ttls() {
        let mut cache = Cache::default();
        let meta = Sidecar {
            content_type: String::new(),
            etag: String::new(),
            fetched_at: 100,
            not_found: false,
        };
        let absent = Sidecar {
            not_found: true,
            ..meta.clone()
        };
        cache.insert(Path::new("yes"), Some(b"x"), &meta, 100, 10, 100);
        cache.insert(Path::new("no"), None, &absent, 100, 10, 100);
        cache.prune(100 + NOT_FOUND_TTL.as_secs());
        assert!(!cache.entries.contains_key(Path::new("no")));
        assert!(cache.entries.contains_key(Path::new("yes")));
        cache.prune(100 + FOUND_TTL.as_secs());
        assert!(cache.entries.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_publisher_finishes_atomic_bounded_insert() {
        let _guard = PROXY_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("cancel.json");
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = cache().lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.await.unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn({
            let dir = dir.path().to_owned();
            let key = key.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    started_tx.send(()).unwrap();
                    let sidecar = Sidecar {
                        content_type: String::new(),
                        etag: String::new(),
                        fetched_at: now_secs(),
                        not_found: true,
                    };
                    write_cache(&dir, &dir.join("cancel.bin"), &key, None, &sidecar);
                })
                .await
                .unwrap();
            }
        });
        started_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    read_cache(&dir.path().join("cancel.bin"), &key),
                    Some(RemoteCoverResult::NotFound)
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let cache = cache().lock().unwrap();
        assert!(cache.entries.len() <= MAX_ENTRIES && cache.size() <= MAX_BYTES);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn cache_publication_never_writes_shared_directory() {
        let _guard = PROXY_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let sidecar = Sidecar {
            content_type: String::new(),
            etag: String::new(),
            fetched_at: now_secs(),
            not_found: true,
        };
        write_cache(
            dir.path(),
            &dir.path().join("x.bin"),
            &dir.path().join("x.json"),
            None,
            &sidecar,
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn cached_etag_matches_upstream_md5() {
        let _guard = PROXY_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("x.bin");
        let json = dir.path().join("x.json");
        let bytes = b"abc";
        let sidecar = Sidecar {
            content_type: "image/jpeg".into(),
            etag: "900150983cd24fb0d6963f7d28e17f72".into(),
            fetched_at: now_secs(),
            not_found: false,
        };
        write_cache(dir.path(), &bin, &json, Some(bytes), &sidecar);
        assert!(
            matches!(read_cache(&bin, &json), Some(RemoteCoverResult::Found(cover)) if cover.etag == sidecar.etag)
        );
    }

    #[test]
    fn negative_cache_is_bounded() {
        let _guard = PROXY_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let sidecar = Sidecar {
            content_type: String::new(),
            etag: String::new(),
            fetched_at: now_secs(),
            not_found: true,
        };
        for i in 0..260 {
            write_cache(
                dir.path(),
                &dir.path().join(format!("{i}.bin")),
                &dir.path().join(format!("{i}.json")),
                None,
                &sidecar,
            );
        }
        assert!(cache().lock().unwrap().entries.len() <= MAX_ENTRIES);
    }

    #[test]
    fn concurrent_publication_never_mixes_body_and_metadata() {
        let _guard = PROXY_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for n in 0..4 {
                let dir = dir.path().to_owned();
                scope.spawn(move || {
                    let bin = dir.join("a.bin");
                    let json = dir.join("a.json");
                    for i in 0..80 {
                        let bytes = format!("{n}-{i}").into_bytes();
                        let sidecar = Sidecar {
                            content_type: format!("image/{n}"),
                            etag: hex::encode(Md5::digest(&bytes)),
                            fetched_at: now_secs(),
                            not_found: false,
                        };
                        write_cache(&dir, &bin, &json, Some(&bytes), &sidecar);
                        if let Some(RemoteCoverResult::Found(value)) = read_cache(&bin, &json) {
                            let text = std::str::from_utf8(&value.bytes).unwrap();
                            assert_eq!(
                                value.content_type,
                                format!("image/{}", text.split('-').next().unwrap())
                            );
                        } else {
                            panic!("published entry missing");
                        }
                    }
                });
            }
        });
    }
}
