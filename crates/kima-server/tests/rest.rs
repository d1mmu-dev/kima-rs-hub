use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum_test::TestServer;
use kima_api::AppState;
use kima_core::config::Config;
use kima_db::PgPool;

fn config(frontend: Option<&Path>, origins: &[&str]) -> Config {
    Config {
        database_url: "unused".into(),
        data_dir: "/tmp".into(),
        music_path: "/tmp".into(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        frontend_dir: frontend.map(Path::to_path_buf),
        allowed_origins: origins.iter().map(|s| s.to_string()).collect(),
        log_level: "info".into(),
        session_secret: None,
        settings_encryption_key: None,
        watch_mode: kima_core::config::WatchMode::Auto,
        watch_poll_secs: 60,
        organize_target: None,
        transcode_cache_mb: 2048,
        public_url: None,
    }
}

fn server(pool: PgPool, config: &Config) -> TestServer {
    TestServer::new(kima_server::app(AppState::new(pool, "s"), config)).unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rest_cors_ignores_allowed_origins(pool: PgPool) {
    let server = server(pool, &config(None, &["https://a.example"]));

    let res = server
        .get("/rest/getOpenSubsonicExtensions.view")
        .add_header("origin", "https://b.example")
        .await;
    res.assert_status_ok();
    assert_eq!(res.header("access-control-allow-origin"), "*");

    let res = server
        .method(axum::http::Method::OPTIONS, "/rest/ping.view")
        .add_header("origin", "https://b.example")
        .add_header("access-control-request-method", "GET")
        .add_header("access-control-request-headers", "x-whatever")
        .await;
    assert_eq!(res.header("access-control-allow-origin"), "*");
    assert!(
        res.maybe_header("access-control-allow-credentials")
            .is_none()
    );

    let res = server
        .get("/api/health")
        .add_header("origin", "https://b.example")
        .await;
    res.assert_status_ok();
    assert!(res.maybe_header("access-control-allow-origin").is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rest_paths_never_fall_to_spa(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "SPA HOME").unwrap();
    std::fs::write(dir.path().join("404.html"), "SPA 404").unwrap();
    let server = server(pool, &config(Some(dir.path()), &[]));

    let res = server.get("/rest/nothing.view").await;
    let body = res.text();
    assert!(
        !body.contains("SPA"),
        "fell through to the frontend: {body}"
    );
    assert!(body.contains("Not implemented: /nothing.view"), "{body}");
    // The SPA still answers elsewhere.
    assert_eq!(server.get("/").await.text(), "SPA HOME");
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rest_trace_has_no_credentials(pool: PgPool) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let server = server(pool, &config(None, &[]));
    // sqlx::test runs a current-thread runtime, so the thread-local default covers the request.
    let guard = tracing::subscriber::set_default(subscriber);
    let res = server
        .get("/rest/tokenInfo.view?u=alice&p=sekrit1&t=tok2&s=salt3&apiKey=key4")
        .await;
    drop(guard);
    res.assert_status_ok();

    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("/rest/tokenInfo.view"),
        "no request span:\n{log}"
    );
    for secret in ["sekrit1", "tok2", "salt3", "key4", "alice", "?"] {
        assert!(!log.contains(secret), "log leaks {secret}:\n{log}");
    }
}
