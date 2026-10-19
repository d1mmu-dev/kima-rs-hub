use std::fs;
use std::path::Path;

use axum::http::{HeaderValue, StatusCode};
use axum_test::TestServer;
use kima_api::AppState;
use kima_core::config::Config;
use kima_db::PgPool;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// A fake static export with the pages the routing cares about.
fn fake_frontend() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "index.html", "home");
    write(dir.path(), "404.html", "not found page");
    write(dir.path(), "album/index.html", "album page");
    write(dir.path(), "podcasts/index.html", "podcasts list");
    write(dir.path(), "podcasts/show/index.html", "podcast page");
    write(dir.path(), "_next/static/app.js", "js");
    dir
}

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

/// Real HTTP, so requests carry origin-form URIs like a browser's (the in-memory transport sends absolute ones).
fn http_server(pool: PgPool, config: &Config) -> TestServer {
    TestServer::builder()
        .http_transport()
        .build(kima_server::app(AppState::new(pool, "s"), config))
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn serves_pages_and_assets(pool: PgPool) {
    let web = fake_frontend();
    let server = server(pool, &config(Some(web.path()), &[]));
    server.get("/").await.assert_text("home");
    server.get("/album/").await.assert_text("album page");
    server.get("/podcasts/").await.assert_text("podcasts list");
    server.get("/_next/static/app.js").await.assert_text("js");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_page_gets_the_404_page(pool: PgPool) {
    let web = fake_frontend();
    let res = server(pool, &config(Some(web.path()), &[]))
        .get("/no/such/page")
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    res.assert_text("not found page");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn api_still_wins_over_the_frontend(pool: PgPool) {
    let web = fake_frontend();
    let server = server(pool, &config(Some(web.path()), &[]));
    server.get("/api/health").await.assert_status_ok();
    let res = server.get("/api/nope").await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<serde_json::Value>()["error"], "Not found");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn old_path_links_redirect_to_query_pages(pool: PgPool) {
    let web = fake_frontend();
    let server = server(pool, &config(Some(web.path()), &[]));
    let cases = [
        ("/album/abc", "/album/?id=abc"),
        ("/artist/abc", "/artist/?id=abc"),
        ("/playlist/abc", "/playlist/?id=abc"),
        ("/mix/abc", "/mix/?id=abc"),
        ("/podcasts/abc", "/podcasts/show/?id=abc"),
        ("/podcasts/genre/1310", "/podcasts/genre/?genreId=1310"),
        ("/audiobooks/abc", "/audiobooks/book/?id=abc"),
        (
            "/audiobooks/series/Dune%20Saga",
            "/audiobooks/series/?name=Dune+Saga",
        ),
        ("/browse/playlists/abc", "/browse/playlists/view/?id=abc"),
        ("/share/tok", "/share/?token=tok"),
        ("/library", "/collection/"),
        ("/setup", "/login/"),
        ("/import/spotify", "/import/playlist/"),
    ];
    for (from, to) in cases {
        let res = server.get(from).await;
        assert!(
            matches!(
                res.status_code(),
                StatusCode::MOVED_PERMANENTLY | StatusCode::PERMANENT_REDIRECT
            ),
            "{from}"
        );
        assert_eq!(res.header("location"), to, "{from}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn export_files_under_legacy_prefixes_are_served(pool: PgPool) {
    let web = fake_frontend();
    write(web.path(), "album/index.txt", "album rsc");
    write(
        web.path(),
        "album/__next.album.__PAGE__.txt",
        "album segment",
    );
    write(web.path(), "podcasts/index.txt", "podcasts rsc");
    let server = http_server(pool, &config(Some(web.path()), &[]));
    for (path, body) in [
        ("/album/index.txt", "album rsc"),
        ("/album/__next.album.__PAGE__.txt", "album segment"),
        ("/podcasts/index.txt", "podcasts rsc"),
        ("/album/index.html", "album page"),
    ] {
        let res = server.get(path).await;
        res.assert_status_ok();
        res.assert_text(body);
    }
    let res = server.get("/album/abc").await;
    res.assert_status(StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.header("location"), "/album/?id=abc");
    // `../404.html` exists relative to `album/`, but a segment that leaves the prefix is never served.
    let res = server.get("/album/..%2F404.html").await;
    res.assert_status(StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.header("location"), "/album/?id=..%2F404.html");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn new_page_paths_are_not_mistaken_for_ids(pool: PgPool) {
    let web = fake_frontend();
    let server = http_server(pool, &config(Some(web.path()), &[]));
    server
        .get("/podcasts/show/")
        .await
        .assert_text("podcast page");
    let res = server.get("/podcasts/show").await;
    assert_eq!(
        res.header("location"),
        "/podcasts/show/",
        "directory slash, not ?id=show"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn security_headers_on_every_response(pool: PgPool) {
    let web = fake_frontend();
    let server = server(pool, &config(Some(web.path()), &[]));
    for path in ["/", "/api/health"] {
        let res = server.get(path).await;
        assert_eq!(res.header("x-frame-options"), "DENY", "{path}");
        assert_eq!(res.header("x-content-type-options"), "nosniff");
        assert_eq!(
            res.header("referrer-policy"),
            "strict-origin-when-cross-origin"
        );
        assert!(
            res.header("content-security-policy")
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert_eq!(
            res.header("permissions-policy"),
            "camera=(), microphone=(), geolocation=()"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn without_a_frontend_only_the_api_answers(pool: PgPool) {
    let server = server(pool, &config(None, &[]));
    server.get("/api/health").await.assert_status_ok();
    server.get("/").await.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cors_allows_listed_origins_only(pool: PgPool) {
    let server = server(pool, &config(None, &["https://ok.example"]));
    let res = server
        .get("/api/health")
        .add_header("origin", "https://ok.example")
        .await;
    assert_eq!(
        res.header("access-control-allow-origin"),
        "https://ok.example"
    );
    let res = server
        .get("/api/health")
        .add_header("origin", "https://evil.example")
        .await;
    assert!(res.maybe_header("access-control-allow-origin").is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cors_allows_any_origin_when_unset(pool: PgPool) {
    let server = server(pool, &config(None, &[]));
    let res = server
        .get("/api/health")
        .add_header("origin", "https://any.example")
        .await;
    assert_eq!(
        res.header("access-control-allow-origin"),
        HeaderValue::from_static("*")
    );
}
