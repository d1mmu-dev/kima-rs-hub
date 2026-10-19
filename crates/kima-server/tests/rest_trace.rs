//! Kept in its own test binary on purpose: tracing caches callsite interest process-wide,
//! so a callsite first hit by another test while no subscriber is set would be cached as
//! "never" and this test's thread-local subscriber would not see it. One test per binary
//! means nothing else can register the /rest callsites before the subscriber is installed.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum_test::TestServer;
use kima_api::AppState;
use kima_core::config::Config;
use kima_db::PgPool;

fn config() -> Config {
    Config {
        database_url: "unused".into(),
        data_dir: "/tmp".into(),
        music_path: "/tmp".into(),
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        frontend_dir: None,
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
    }
}

fn server(pool: PgPool, config: &Config) -> TestServer {
    TestServer::new(kima_server::app(AppState::new(pool, "s"), config)).unwrap()
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
    let server = server(pool, &config());
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
