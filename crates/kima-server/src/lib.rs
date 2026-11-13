//! Assembles the API, the static frontend and the cross-cutting HTTP layers.

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::http::{HeaderName, HeaderValue};
use kima_api::AppState;
use kima_core::config::Config;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tracing::Span;

pub mod frontend;
pub mod jobs;
pub mod web_proxy;
pub mod web_ssr;

use kima_jobs::schedule::{Cron, EnqueueFn};

/// Upstream's Next.js `headers()` block, now applied by the server to every response. `font-src` also allows
/// `data:` because kima-web embeds Montserrat in its inline stylesheet as a data URI.
const SECURITY_HEADERS: &[(&str, &str)] = &[
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=()",
    ),
    ("x-frame-options", "DENY"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    ("x-dns-prefetch-control", "on"),
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob: https://cdn-images.dzcdn.net https://e-cdns-images.dzcdn.net \
         https://assets.pippa.io https://is1-ssl.mzstatic.com; media-src 'self' blob:; \
         connect-src 'self' ws: wss:; font-src 'self' data:; frame-ancestors 'none'; worker-src 'self' blob:",
    ),
];

pub fn app(state: AppState, config: &Config) -> Router {
    app_with_web(state, config, None)
}

/// [`app`], with the `kima-web` routes forwarded to the embedded SSR server when `web_proxy` is
/// given. The proxy sits in front of everything under the app-wide CORS and header layers, so a
/// page wins over the legacy export's fallback and redirects (`/setup`), while `/api` and `/rest`
/// are not on its allow-list.
pub fn app_with_web(
    state: AppState,
    config: &Config,
    web_proxy: Option<std::sync::Arc<web_proxy::WebProxy>>,
) -> Router {
    let mut web = kima_api::router(state.clone());
    if let Some(dir) = &config.frontend_dir {
        web = web.merge(frontend::routes(dir));
    }
    if let Some(proxy) = web_proxy {
        web = web_proxy::layer(web, proxy);
    }
    // `/rest` has its own CORS policy (D-090); the app-wide one must not apply to it.
    let rest = Router::new()
        .nest("/rest", kima_subsonic::router(state))
        .layer(rest_cors());
    let mut app = web.layer(cors(&config.allowed_origins)).merge(rest);
    for &(name, value) in SECURITY_HEADERS {
        app = app.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        ));
    }
    app.layer(TraceLayer::new_for_http().make_span_with(span_for))
}

/// Query strings can contain credentials or private remote URLs on any route.
fn span_for(request: &Request<Body>) -> Span {
    let path = request.uri().path();
    let path = if path == "/api/library/cover-art" || path.starts_with("/api/library/cover-art/") {
        "/api/library/cover-art/{id}"
    } else {
        path
    };
    tracing::debug_span!(
        "request",
        method = %request.method(),
        path = %path,
        version = ?request.version(),
    )
}

/// Any origin, method and header; no credentials. Browser players on other origins put their
/// credentials in the URL, never in cookies (D-090).
fn rest_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
}

/// Unset `ALLOWED_ORIGINS` allows any origin, as upstream does.
fn cors(origins: &[String]) -> CorsLayer {
    let layer = CorsLayer::new().allow_methods(Any).allow_headers(Any);
    if origins.is_empty() {
        return layer.allow_origin(Any);
    }
    let list: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    layer.allow_origin(AllowOrigin::list(list))
}

/// The scheduler's entries (spec §5): `library-sync` when `KIMA_LIBRARY_SYNC_CRON` is not `off`
/// (`sync`, parsed at startup), and always `http-cache-prune` (03:17 UTC daily: expired cache
/// rows, then unused genres) and `enrich-kick` (every ten minutes: a backstop for a lost
/// self-reschedule; a paused or halted pipeline stays so), plus `podcast-refresh-sweep`
/// every 15 minutes independently of identification.
pub fn schedule_entries(
    _config: &Config,
    sync: Option<Cron>,
) -> Vec<(&'static str, Cron, EnqueueFn)> {
    let mut entries: Vec<(&'static str, Cron, EnqueueFn)> = Vec::new();
    if let Some(cron) = sync {
        entries.push(("library-sync", cron, library_sync_job()));
    }
    entries.push((
        "http-cache-prune",
        Cron::parse("17 3 * * *").expect("a valid cron expression"),
        prune_job(),
    ));
    entries.push((
        "enrich-kick",
        Cron::parse("*/10 * * * *").expect("a valid cron expression"),
        enrich_kick_job(),
    ));
    entries.push((
        "podcast-refresh-sweep",
        Cron::parse("*/15 * * * *").expect("a valid cron expression"),
        std::sync::Arc::new(|pool| {
            Box::pin(async move {
                kima_podcast::refresh::sweep(&pool)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
        }),
    ));
    entries
}

/// The podcast entries of the scheduler: the daily episode-cache cleanup at 04:00 UTC (upstream's
/// 24-hour interval after the startup run, `index.ts:571-576`). Kept apart from
/// [`schedule_entries`] because it needs the podcast context (data directory, client).
pub fn podcast_schedule_entries(
    ctx: kima_podcast::ctx::PodcastCtx,
) -> Vec<(&'static str, Cron, EnqueueFn)> {
    vec![(
        "podcast-cache-cleanup",
        Cron::parse("0 4 * * *").expect("a valid cron expression"),
        podcast_cache_cleanup_job(ctx),
    )]
}

/// The scheduler passes only a `PgPool` to its entries; `ctx` already carries its own pool
/// clone (and `net`/`data_dir`), so the one the scheduler hands over is unused here.
fn podcast_cache_cleanup_job(ctx: kima_podcast::ctx::PodcastCtx) -> EnqueueFn {
    std::sync::Arc::new(move |_pool| {
        let ctx = ctx.clone();
        Box::pin(async move {
            let (deleted, freed_mb) = kima_podcast::cache::cleanup(&ctx)
                .await
                .map_err(|e| e.to_string())?;
            if deleted > 0 {
                tracing::info!(
                    deleted,
                    freed_mb,
                    "cleaned up expired podcast episode cache"
                );
            }
            Ok(())
        })
    })
}

/// Queues the incremental reconcile; it dedupes against one already queued or running.
fn library_sync_job() -> EnqueueFn {
    std::sync::Arc::new(|pool| {
        Box::pin(async move {
            kima_library::jobs::enqueue_reconcile(&pool)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    })
}

/// Expired response-cache rows, then genres nothing refers to.
fn prune_job() -> EnqueueFn {
    std::sync::Arc::new(|pool| {
        Box::pin(async move {
            let cache = kima_db::http_cache::prune_expired(&pool)
                .await
                .map_err(|e| e.to_string())?;
            let genres = kima_db::genres::prune_unused(&pool)
                .await
                .map_err(|e| e.to_string())?;
            tracing::info!(cache, genres, "pruned the response cache and unused genres");
            Ok(())
        })
    })
}

fn enrich_kick_job() -> EnqueueFn {
    std::sync::Arc::new(|pool| {
        Box::pin(async move {
            kima_ident::coordinator::kick(&pool)
                .await
                .map_err(|e| e.to_string())
        })
    })
}

#[cfg(test)]
mod trace_tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

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

    #[tokio::test(flavor = "current_thread")]
    async fn request_trace_omits_query_even_for_redirects() {
        use axum::routing::get;
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let router = Router::new()
            .route("/api/podcasts/123/cover", get(|| async {
                axum::response::Redirect::temporary("/api/library/cover-art?url=https%3A%2F%2Fexample.test%2Fimage-secret&token=sample-token")
            }))
            .route("/api/library/cover-art", get(|| async { "ok" }))
            .route("/api/library/cover-art/{id}", get(|| async { "ok" }))
            .layer(TraceLayer::new_for_http().make_span_with(span_for));
        let server = axum_test::TestServer::new(router).unwrap();
        server
            .get("/api/podcasts/123/cover?token=sample-token")
            .await
            .assert_status(axum::http::StatusCode::TEMPORARY_REDIRECT);
        server.get("/api/library/cover-art?url=https%3A%2F%2Fexample.test%2Fimage-secret%3Fkey%3Dimage-key&token=sample-token").await.assert_status_ok();
        server.get("/api/library/cover-art/https%3A%2F%2Fexample.test%2Fimage-secret%3Fkey%3Dimage-key").await.assert_status_ok();
        let listener = tokio::net::TcpSocket::new_v4().unwrap();
        listener.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let proxy = std::sync::Arc::new(web_proxy::WebProxy::new(port, vec![]));
        let proxy_server =
            axum_test::TestServer::new(web_proxy::layer(Router::new(), proxy)).unwrap();
        proxy_server
            .get("/login?token=sample-token")
            .await
            .assert_status(axum::http::StatusCode::BAD_GATEWAY);
        let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("/api/podcasts/123/cover"));
        assert!(log.contains("/api/library/cover-art"));
        assert!(log.contains("SSR server did not answer"));
        assert!(log.contains("/login"));
        for secret in [
            "sample-token",
            "image-secret",
            "image-key",
            "example.test",
            "?",
        ] {
            assert!(
                !log.contains(secret),
                "request trace contains sensitive query data"
            );
        }
    }
}
