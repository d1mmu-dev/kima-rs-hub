//! M9 Task 3: the embedded vertigo SSR server and the allow-list proxy in front of it.
//!
//! * the proxy forwards exactly the `kima-web` routes (and the two build assets) and nothing else;
//! * a page fetched through it is markup rendered by the wasm in wasmtime, not an empty shell;
//! * a dead SSR task ends the process with a non-zero status (crash and restart).
//!
//! The real-SSR test needs `vertigo build` output (`crates/kima-web/build`, or
//! `KIMA_WEB_BUILD_DIR`). Without it the test says so and returns; set `KIMA_WEB_REQUIRE_BUILD=1`
//! (CI, after `tools/verify-kima-web-build.sh`) to make a missing build a failure instead.

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::routing::get;
use kima_api::AppState;
use kima_core::config::Config;
use kima_db::PgPool;
use kima_server::web_proxy::{self, WebProxy};
use kima_server::web_ssr::{self, WebSsr};
use tokio_util::sync::CancellationToken;

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Serves `router` on a loopback port, for the life of the test.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
}

fn config(frontend: Option<&Path>) -> Config {
    Config {
        database_url: "unused".into(),
        data_dir: "/tmp".into(),
        music_path: "/tmp".into(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        frontend_dir: frontend.map(Path::to_path_buf),
        web_build_dir: None,
        web_ssr_port: kima_core::config::DEFAULT_WEB_SSR_PORT,
        allowed_origins: vec![],
        log_level: "info".into(),
        session_secret: None,
        settings_encryption_key: None,
        watch_mode: kima_core::config::WatchMode::Auto,
        watch_poll_secs: 60,
        organize_target: None,
        transcode_cache_mb: 2048,
        artwork_cache_mb: 512,
        public_url: None,
        library_sync_cron: None,
        identify_env_off: false,
        contact: kima_core::config::DEFAULT_CONTACT.into(),
        musicbrainz_base_url: kima_core::config::DEFAULT_MUSICBRAINZ_BASE_URL.into(),
        fingerprint_workers: 1,
        clap_model_dir: None,
        disable_clap: false,
        musicnn_model_dir: None,
        disable_musicnn: false,
        snapshot_dir: None,
        snapshot_max_bytes: None,
        snapshot_max_concurrent: None,
        acoustid_disabled: false,
        acoustid_client_key: None,
        fanart_api_key: None,
    }
}

/// A fake legacy export, to prove the proxy wins over it where the routes overlap.
fn legacy_export() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "legacy home").unwrap();
    std::fs::write(dir.path().join("404.html"), "legacy 404").unwrap();
    std::fs::create_dir_all(dir.path().join("podcasts")).unwrap();
    std::fs::write(dir.path().join("podcasts/index.html"), "legacy podcasts").unwrap();
    dir
}

/// A stand-in for the SSR server that answers `ssr:<path>` and counts what reaches it.
async fn stub_ssr() -> (u16, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let router = Router::new().fallback(move |req: Request| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            format!("ssr:{}", req.uri().path())
        }
    });
    let base = serve(router).await;
    (base.rsplit(':').next().unwrap().parse().unwrap(), hits)
}

const ASSETS: [&str; 2] = ["/build/kima_web.AAAA.wasm", "/build/wasm_run.BBBB.js"];

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn only_the_kima_web_routes_are_proxied(pool: PgPool) {
    let (ssr_port, hits) = stub_ssr().await;
    let proxy = Arc::new(WebProxy::new(ssr_port, ASSETS.map(String::from).to_vec()));
    let legacy = legacy_export();
    let app = kima_server::app_with_web(
        AppState::new(pool, "s"),
        &config(Some(legacy.path())),
        Some(proxy),
    );
    let base = serve(app).await;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let get = |path: &'static str| {
        let http = http.clone();
        let url = format!("{base}{path}");
        async move {
            let res = http.get(url).send().await.unwrap();
            (res.status(), res.text().await.unwrap())
        }
    };

    // Proxied: the pages, below-page paths with a query, and exactly the two build assets.
    for path in [
        "/",
        "/login",
        "/setup",
        "/collection",
        "/collection/albums?artist=1",
        "/collection/tracks",
        "/search",
        "/playlists",
        "/playlists/0b1c2d3e",
        "/settings",
        ASSETS[0],
        ASSETS[1],
    ] {
        let before = hits.load(Ordering::SeqCst);
        let (status, body) = get(path).await;
        let bare = path.split('?').next().unwrap();
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, &*format!("ssr:{bare}")),
            "{path}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), before + 1, "{path}");
    }

    // Not proxied: everything else gets kima-server's own routing, byte for byte what it is
    // without the proxy. None of these may reach the SSR server.
    let reached = hits.load(Ordering::SeqCst);
    let (status, body) = get("/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.starts_with("ssr:"), "{body}");
    let (status, body) = get("/api/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Not found"), "{body}");
    let (status, body) = get("/rest/ping.view").await;
    assert!(!body.starts_with("ssr:"), "{status} {body}");
    // The legacy export keeps what is not a kima-web route (`/podcasts` is its page here).
    assert_eq!(
        get("/podcasts/").await,
        (StatusCode::OK, "legacy podcasts".into())
    );
    for path in [
        "/no/such/page",
        "/loginx",
        "/searching",
        "/collections",
        "/playlist",
        "/x/login",
        "/build/",
        "/build/other.wasm",
        "/build/kima_web.AAAA.wasm.map",
        "/_next/static/app.js",
    ] {
        let (status, body) = get(path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, "legacy 404", "{path}");
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        reached,
        "an unrelated path reached the SSR server"
    );

    // Only reads are forwarded: a POST to a page route is kima-server's to answer.
    let res = http.post(format!("{base}/login")).send().await.unwrap();
    assert_ne!(res.text().await.unwrap(), "ssr:/login");
    assert_eq!(hits.load(Ordering::SeqCst), reached);
}

/// Without a legacy export the app has no fallback of its own, only axum's built-in 404.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_proxy_works_without_a_legacy_export(pool: PgPool) {
    let (ssr_port, hits) = stub_ssr().await;
    let proxy = Arc::new(WebProxy::new(ssr_port, vec![]));
    let app = kima_server::app_with_web(AppState::new(pool, "s"), &config(None), Some(proxy));
    let base = serve(app).await;
    for path in ["/", "/login", "/collection/albums", "/playlists/x"] {
        let res = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        assert_eq!(res.text().await.unwrap(), format!("ssr:{path}"));
    }
    let reached = hits.load(Ordering::SeqCst);
    for path in ["/podcasts", "/nope", "/loginx", "/build/x"] {
        let res = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        reqwest::get(format!("{base}/api/health"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(hits.load(Ordering::SeqCst), reached);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn without_a_proxy_nothing_changes(pool: PgPool) {
    let legacy = legacy_export();
    let base = serve(kima_server::app(
        AppState::new(pool, "s"),
        &config(Some(legacy.path())),
    ))
    .await;
    let res = reqwest::get(format!("{base}/")).await.unwrap();
    assert_eq!(res.text().await.unwrap(), "legacy home");
}

#[tokio::test]
async fn a_bad_gateway_when_the_ssr_server_is_gone() {
    let port = free_port(); // nothing listens there
    let app = web_proxy::layer(
        Router::new().route("/ok", get(|| async { "ok" })),
        Arc::new(WebProxy::new(port, vec![])),
    );
    let base = serve(app).await;
    let res = reqwest::get(format!("{base}/login")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        reqwest::get(format!("{base}/ok"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );
}

// ---- real SSR -----------------------------------------------------------------------------

fn build_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("KIMA_WEB_BUILD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../kima-web/build"));
    if dir.join("index.json").is_file() {
        return Some(dir.canonicalize().unwrap());
    }
    assert!(
        std::env::var_os("KIMA_WEB_REQUIRE_BUILD").is_none(),
        "no `vertigo build` output at {}; run tools/verify-kima-web-build.sh",
        dir.display()
    );
    eprintln!("SKIPPED: no `vertigo build` output at {}", dir.display());
    None
}

/// What the browser gets for `/login` is HTML the embedded wasmtime rendered from `kima-web`'s
/// own view code, through the proxy and the whole app. One test only: vertigo keeps its server
/// state in a process-wide table.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn login_is_server_rendered_markup(pool: PgPool) {
    let Some(dir) = build_dir() else { return };
    let stop = CancellationToken::new();
    let ssr = WebSsr::start(&dir, free_port(), "http://127.0.0.1:1".into(), stop.clone())
        .await
        .expect("the embedded SSR server starts");
    let legacy = legacy_export();
    let app = kima_server::app_with_web(
        AppState::new(pool, "s"),
        &config(Some(legacy.path())),
        Some(ssr.proxy.clone()),
    );
    let base = serve(app).await;

    let res = reqwest::get(format!("{base}/login")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let content_type = res.headers()["content-type"].to_str().unwrap().to_owned();
    assert!(content_type.starts_with("text/html"), "{content_type}");
    // The app-wide headers still apply to proxied pages.
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    let html = res.text().await.unwrap();
    // Markup that exists only because `kima-web`'s login view ran on the server:
    assert!(html.contains("Welcome back"), "{html}");
    assert!(html.contains("<form"), "{html}");
    assert!(
        html.contains(r#"placeholder="Enter your username""#),
        "{html}"
    );
    assert!(html.contains(">Sign In</button>"), "{html}");
    // ... plus the precompiled stylesheet and the bootstrap vertigo adds around it. Not a bare shell:
    assert!(
        html.contains("<style>") && html.contains("data-vertigo-run-wasm"),
        "{html}"
    );
    assert!(!html.contains("<body></body>"), "{html}");

    // A different route renders different markup (so it is not one canned page) ...
    let setup = reqwest::get(format!("{base}/setup"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!setup.contains("Welcome back"), "{setup}");
    assert!(setup.contains(">Loading…</p>"), "{setup}");
    // ... and `/` and `/playlists/<id>` render too (the legacy export would say "legacy home").
    let home = reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        home.contains("data-vertigo-run-wasm") && !home.contains("legacy home"),
        "{home}"
    );
    let playlist = reqwest::get(format!(
        "{base}/playlists/00000000-0000-0000-0000-000000000000"
    ))
    .await
    .unwrap();
    assert_eq!(playlist.status(), StatusCode::OK);

    // The two files the page names are served through the proxy; others in the directory are not.
    let attr = |name: &str| -> String {
        let at = html.find(name).unwrap() + name.len();
        html[at..].split('"').next().unwrap().to_owned()
    };
    let wasm = attr(r#"data-vertigo-run-wasm=""#);
    let js = attr(r#"src=""#);
    let res = reqwest::get(format!("{base}{wasm}")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{wasm}");
    assert_eq!(&res.bytes().await.unwrap()[..4], b"\0asm");
    let res = reqwest::get(format!("{base}{js}")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{js}");
    let index = format!("{}/index.json", wasm.rsplit_once('/').unwrap().0);
    let res = reqwest::get(format!("{base}{index}")).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "{index} must not be served"
    );

    // Not-kima-web paths keep the app's routing while the SSR server is up.
    assert_eq!(
        reqwest::get(format!("{base}/podcasts/"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "legacy podcasts"
    );
    assert_eq!(
        reqwest::get(format!("{base}/api/health"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    stop.cancel();
}

// ---- liveness: crash and restart ----------------------------------------------------------

/// A server that never ends on its own, like axum's.
fn endless_server() -> impl std::future::Future<Output = std::io::Result<()>> + Unpin {
    Box::pin(std::future::pending())
}

fn fake_ssr<F, E>(task: F, stop: &CancellationToken) -> WebSsr
where
    F: std::future::Future<Output = Result<(), E>> + Send + 'static,
    E: std::fmt::Debug + Send + 'static,
{
    WebSsr::from_task(Arc::new(WebProxy::new(1, vec![])), task, stop.clone())
}

const GRACE: Duration = Duration::from_millis(300);

#[tokio::test]
async fn a_panicking_ssr_task_ends_serving_with_an_error() {
    let stop = CancellationToken::new();
    let mut ssr = fake_ssr(
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if true {
                panic!("wasm trapped");
            }
            Ok::<(), String>(())
        },
        &stop,
    );
    let mut server = endless_server();
    let started = Instant::now();
    let err = web_ssr::serve(&mut server, Some(&mut ssr), &stop, GRACE)
        .await
        .expect_err("a dead SSR task must be an error");
    assert!(err.to_string().contains("panicked"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        stop.is_cancelled(),
        "the rest of the process is told to stop"
    );
}

#[tokio::test]
async fn a_failing_ssr_task_ends_serving_with_an_error() {
    // What vertigo returns when the port cannot be bound: `Err(ErrorCode::ServeCantOpenPort)`.
    let stop = CancellationToken::new();
    let mut ssr = fake_ssr(async { Err::<(), _>("ServeCantOpenPort") }, &stop);
    let err = web_ssr::serve(&mut endless_server(), Some(&mut ssr), &stop, GRACE)
        .await
        .expect_err("a failed SSR task must be an error");
    assert!(err.to_string().contains("ServeCantOpenPort"), "{err}");
}

#[tokio::test]
async fn an_ssr_task_that_just_returns_is_an_error_too() {
    let stop = CancellationToken::new();
    let mut ssr = fake_ssr(async { Ok::<(), String>(()) }, &stop);
    let err = web_ssr::serve(&mut endless_server(), Some(&mut ssr), &stop, GRACE)
        .await
        .expect_err("returning without a shutdown request is a failure");
    assert!(
        err.to_string().contains("no shutdown was requested"),
        "{err}"
    );
}

#[tokio::test]
async fn a_requested_shutdown_is_not_a_failure() {
    // vertigo's server also stops on SIGTERM by itself: it returns Ok, and the shutdown that
    // explains it is requested at about the same moment.
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let mut ssr = fake_ssr(
        async move {
            task_stop.cancelled().await;
            Ok::<(), String>(())
        },
        &stop,
    );
    let trigger = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.cancel();
    });
    let mut server = endless_server();
    web_ssr::serve(&mut server, Some(&mut ssr), &stop, GRACE)
        .await
        .expect("a clean shutdown is Ok");
}

#[tokio::test]
async fn without_ssr_the_server_alone_decides() {
    let stop = CancellationToken::new();
    let mut server = Box::pin(async { Ok::<(), std::io::Error>(()) });
    web_ssr::serve(&mut server, None, &stop, GRACE)
        .await
        .unwrap();
    let mut failing = Box::pin(async { Err(std::io::Error::other("bind")) });
    web_ssr::serve(&mut failing, None, &stop, GRACE)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn start_fails_on_a_busy_port_and_on_a_bad_build() {
    let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = busy.local_addr().unwrap().port();
    let empty = tempfile::tempdir().unwrap();
    let stop = CancellationToken::new();
    let err = WebSsr::start(
        empty.path(),
        port,
        "http://127.0.0.1:1".into(),
        stop.clone(),
    )
    .await
    .err()
    .expect("no index.json");
    assert!(
        err.to_string().contains("not a `vertigo build` output"),
        "{err}"
    );

    // A real build dir but an occupied port: refused at once, not after vertigo's 20 s wait.
    let Some(dir) = build_dir() else { return };
    let started = Instant::now();
    let err = WebSsr::start(&dir, port, "http://127.0.0.1:1".into(), stop)
        .await
        .err()
        .expect("port in use");
    assert!(format!("{err:#}").contains("is not free"), "{err:#}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The real binary: a bad SSR setup is a failed start with a non-zero status, before it touches
/// the database (so none is needed here).
#[test]
fn the_process_exits_non_zero_when_the_ssr_server_cannot_start() {
    let data = tempfile::tempdir().unwrap();
    let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = busy.local_addr().unwrap().port().to_string();
    let empty = tempfile::tempdir().unwrap();
    let mut cases = vec![(empty.path().to_path_buf(), free_port().to_string())];
    if let Some(dir) = build_dir() {
        cases.push((dir, port));
    }
    for (build, ssr_port) in cases {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_kima"));
        cmd.env_clear();
        // The cleared environment is the server's configuration. The dynamic loader path is
        // not: the pinned libsoxr lives in a private prefix reached only through
        // LD_LIBRARY_PATH (CI, Dockerfile), and without it the binary cannot even start.
        if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
            cmd.env("LD_LIBRARY_PATH", path);
        }
        let out = cmd
            .env("DATABASE_URL", "postgres://nobody@127.0.0.1:1/none")
            .env("KIMA_DATA_DIR", data.path())
            .env("KIMA_WEB_BUILD_DIR", &build)
            .env("KIMA_WEB_SSR_PORT", &ssr_port)
            .env("PORT", free_port().to_string())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{build:?} {ssr_port}: {:?} {stderr}",
            out.status
        );
        assert!(stderr.contains("embedded kima-web SSR server"), "{stderr}");
    }
}
