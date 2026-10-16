//! Assembles the API, the static frontend and the cross-cutting HTTP layers.

use axum::Router;
use axum::http::{HeaderName, HeaderValue};
use kima_api::AppState;
use kima_core::config::Config;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

pub mod frontend;
pub mod jobs;

/// Upstream's Next.js `headers()` block, now applied by the server to every response.
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
         connect-src 'self' ws: wss:; font-src 'self'; frame-ancestors 'none'; worker-src 'self' blob:",
    ),
];

pub fn app(state: AppState, config: &Config) -> Router {
    let mut app = kima_api::router(state);
    if let Some(dir) = &config.frontend_dir {
        app = app.merge(frontend::routes(dir));
    }
    for &(name, value) in SECURITY_HEADERS {
        app = app.layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        ));
    }
    app.layer(cors(&config.allowed_origins))
        .layer(TraceLayer::new_for_http())
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
