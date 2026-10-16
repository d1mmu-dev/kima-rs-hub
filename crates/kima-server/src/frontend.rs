//! The exported Next.js app, plus redirects from upstream's path-style page URLs (D-011).

use std::path::Path;

use axum::Router;
use axum::extract::Path as UrlPath;
use axum::response::Redirect;
use axum::routing::get;
use tower_http::services::{ServeDir, ServeFile};

/// (old route, new page, query parameter name)
pub const LEGACY_ROUTES: &[(&str, &str, &str)] = &[
    ("/album/{value}", "/album/", "id"),
    ("/artist/{value}", "/artist/", "id"),
    ("/playlist/{value}", "/playlist/", "id"),
    ("/mix/{value}", "/mix/", "id"),
    ("/podcasts/{value}", "/podcasts/show/", "id"),
    ("/podcasts/genre/{value}", "/podcasts/genre/", "genreId"),
    ("/audiobooks/{value}", "/audiobooks/book/", "id"),
    ("/audiobooks/series/{value}", "/audiobooks/series/", "name"),
    ("/browse/playlists/{value}", "/browse/playlists/view/", "id"),
    ("/share/{value}", "/share/", "token"),
];

/// Fixed redirects that upstream's Next.js config or pages performed.
const FIXED_REDIRECTS: &[(&str, &str)] = &[
    ("/library", "/collection/"),
    ("/setup", "/login/"),
    ("/import/spotify", "/import/playlist/"),
];

/// New page directories that sit where a legacy `{value}` would match.
const PAGE_DIRS: &[&str] = &[
    "/podcasts/show",
    "/podcasts/genre",
    "/audiobooks/book",
    "/audiobooks/series",
    "/browse/playlists/view",
];

pub fn routes(dir: &Path) -> Router {
    let files = ServeDir::new(dir)
        .append_index_html_on_directories(true)
        .not_found_service(ServeFile::new(dir.join("404.html")));

    let mut router = Router::new();
    for &(route, page, param) in LEGACY_ROUTES {
        router = router.route(
            route,
            get(move |UrlPath(value): UrlPath<String>| async move {
                let encoded: String = form_urlencoded::byte_serialize(value.as_bytes()).collect();
                Redirect::permanent(&format!("{page}?{param}={encoded}"))
            }),
        );
    }
    for &(from, to) in FIXED_REDIRECTS {
        router = router.route(from, get(move || async move { Redirect::permanent(to) }));
    }
    for &page_dir in PAGE_DIRS {
        router = router.route_service(page_dir, files.clone());
    }
    router.fallback_service(files)
}
