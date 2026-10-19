use std::path::Path;

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
        artwork_cache_mb: 512,
        public_url: None,
        library_sync_cron: None,
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
    // Auth runs before the not-implemented answer, so an anonymous probe gets 10.
    assert!(body.contains("subsonic-response"), "{body}");
    assert!(body.contains("Required parameter is missing: u"), "{body}");
    // The SPA still answers elsewhere.
    assert_eq!(server.get("/").await.text(), "SPA HOME");
}
