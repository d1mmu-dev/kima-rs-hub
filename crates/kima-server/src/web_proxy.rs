//! Reverse proxy from `kima-server` to the embedded vertigo SSR server (M9 spec §6.1).
//!
//! Only the pages `kima-web` implements ([`WEB_ROUTES`]) and the two build assets the rendered
//! HTML names (the hashed `.wasm` and `wasm_run.*.js`) are forwarded, and only for `GET`/`HEAD`.
//! Every other request reaches `kima-server`'s own routing unchanged (so `/api`, `/rest`,
//! `/podcasts`, the legacy export, 404s ... never touch the SSR server).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};

/// The pages `kima-web` serves. A path is proxied when it is one of these or lies below one
/// (`/collection/albums`, `/playlists/<id>`). Each page task of M9 adds its entry here and
/// nowhere else.
pub const WEB_ROUTES: &[&str] = &[
    "/login",
    "/setup",
    "/collection",
    "/search",
    "/playlists",
    "/settings",
    "/podcasts",
];

/// Pages matched by the exact path only. `/` is `kima-web`'s browse root (`Route::new` maps it to
/// the artists page); a prefix match on it would swallow every path.
pub const WEB_EXACT_ROUTES: &[&str] = &["/"];

/// Headers that describe one connection, not the message (RFC 9110 §7.6.1), plus `host`
/// (the upstream is addressed by its own authority).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "keep-alive",
    "proxy-connection",
];

/// What may be proxied and where to.
#[derive(Debug)]
pub struct WebProxy {
    upstream: String,
    assets: Vec<String>,
    client: reqwest::Client,
}

impl WebProxy {
    /// `assets` are the exact public paths of the build files (see [`crate::web_ssr`]).
    pub fn new(loopback_port: u16, assets: Vec<String>) -> Self {
        let client = reqwest::Client::builder()
            // The browser must see the SSR server's redirects, not have them followed here.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("an HTTP client for the loopback SSR server");
        Self {
            upstream: format!("http://127.0.0.1:{loopback_port}"),
            assets,
            client,
        }
    }

    /// Whether `path` (the request path, query excluded) belongs to `kima-web`.
    pub fn allows(&self, path: &str) -> bool {
        is_web_route(path) || self.assets.iter().any(|a| a == path)
    }
}

/// `path` is a [`WEB_EXACT_ROUTES`] entry, a [`WEB_ROUTES`] entry or lies below one, matched by whole segments
/// (`/loginx` and `/searching` are not pages).
pub fn is_web_route(path: &str) -> bool {
    WEB_EXACT_ROUTES.contains(&path)
        || WEB_ROUTES.iter().any(|route| {
            path.strip_prefix(route)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
}

/// Forwards the allow-listed requests of `router` to the SSR server; the rest is `router`'s.
pub fn layer(router: Router, proxy: Arc<WebProxy>) -> Router {
    // `Router::layer` does not wrap axum's built-in 404 (a router without a fallback of its own),
    // so `router` becomes the explicit fallback of a new one and the layer wraps that.
    Router::new()
        .fallback_service(router)
        .layer(middleware::from_fn_with_state(proxy, handle))
}

async fn handle(State(proxy): State<Arc<WebProxy>>, req: Request, next: Next) -> Response {
    let readonly = matches!(*req.method(), Method::GET | Method::HEAD);
    if !readonly || !proxy.allows(req.uri().path()) {
        return next.run(req).await;
    }
    forward(&proxy, req).await
}

async fn forward(proxy: &WebProxy, req: Request) -> Response {
    let (parts, _body) = req.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or("/", |pq| pq.as_str())
        .to_owned();
    let url = format!("{}{}", proxy.upstream, path_and_query);

    let mut out = proxy.client.request(parts.method.clone(), &url);
    out = out.headers(forwardable(&parts.headers));
    if let Some(host) = parts.headers.get(header::HOST) {
        out = out.header("x-forwarded-host", host);
    }
    let upstream = match out.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e.without_url(), path = %parts.uri.path(), "SSR server did not answer");
            return (StatusCode::BAD_GATEWAY, "kima-web is unavailable").into_response();
        }
    };

    let mut response = Response::builder().status(upstream.status());
    if let Some(headers) = response.headers_mut() {
        *headers = forwardable(upstream.headers());
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

fn forwardable(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if !HOP_BY_HOP.contains(&name.as_str()) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_match_by_whole_segments() {
        for ok in [
            "/",
            "/login",
            "/login/",
            "/setup",
            "/collection",
            "/collection/albums",
            "/collection/tracks",
            "/search",
            "/playlists",
            "/playlists/0b1c",
            "/settings",
            "/podcasts",
            "/podcasts/show",
            "/podcasts/category",
        ] {
            assert!(is_web_route(ok), "{ok}");
        }
        for no in [
            "//",
            "/loginx",
            "/searching",
            "/collections",
            "/playlist",
            "/podcastsx",
            "/api/podcasts",
            "/api/library/tracks",
            "/rest/ping",
            "/health",
            "/x/login",
            "",
        ] {
            assert!(!is_web_route(no), "{no}");
        }
    }
}
