//! The route table. Hand-written enum with a `From<String>` / `Display` pair (vertigo's router pattern).
//!
//! Login, setup (onboarding), the library browse root with its albums / tracks drill-downs, and search.
//! Playlists and settings arrive with their pages; until then they fall under `NotFound`.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Login,
    Setup,
    /// `/collection`: the browse root (artists).
    Artists,
    /// `/collection/albums`, optionally `?artist=<uuid>`.
    Albums {
        artist: Option<String>,
    },
    /// `/collection/tracks`, optionally `?album=<uuid>`.
    Tracks {
        album: Option<String>,
    },
    /// `/search`; the query lives in the page, not in the URL.
    Search,
    NotFound,
}

impl Route {
    pub fn requires_auth(&self) -> bool {
        !matches!(self, Route::Login | Route::Setup | Route::NotFound)
    }

    pub fn new(url: &str) -> Route {
        // The router hands over path + query + hash.
        let url = url.split('#').next().unwrap_or("");
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        let path = if path.len() > 1 {
            path.trim_end_matches('/')
        } else {
            path
        };
        let param = |name: &str| -> Result<Option<String>, ()> {
            match query
                .split('&')
                .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
            {
                None | Some("") => Ok(None),
                Some(id) if is_id(id) => Ok(Some(id.to_owned())),
                Some(_) => Err(()),
            }
        };
        match path {
            "" | "/" | "/collection" => Route::Artists,
            "/login" => Route::Login,
            "/setup" => Route::Setup,
            "/collection/albums" => {
                param("artist").map_or(Route::NotFound, |artist| Route::Albums { artist })
            }
            "/collection/tracks" => {
                param("album").map_or(Route::NotFound, |album| Route::Tracks { album })
            }
            "/search" => Route::Search,
            _ => Route::NotFound,
        }
    }
}

/// Ids are UUIDs; accept only their alphabet so a query value can never smuggle anything into a URL.
fn is_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Login => f.write_str("/login"),
            Route::Setup => f.write_str("/setup"),
            Route::Artists => f.write_str("/collection"),
            Route::Albums { artist: None } => f.write_str("/collection/albums"),
            Route::Albums { artist: Some(id) } => write!(f, "/collection/albums?artist={id}"),
            Route::Tracks { album: None } => f.write_str("/collection/tracks"),
            Route::Tracks { album: Some(id) } => write!(f, "/collection/tracks?album={id}"),
            Route::Search => f.write_str("/search"),
            Route::NotFound => f.write_str("/404"),
        }
    }
}

impl From<String> for Route {
    fn from(url: String) -> Self {
        Route::new(&url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a1aad3-f0c4-710b-b701-36f9f6b6972e";

    fn table() -> Vec<(&'static str, Route)> {
        vec![
            ("/login", Route::Login),
            ("/setup", Route::Setup),
            ("/collection", Route::Artists),
            ("/collection/albums", Route::Albums { artist: None }),
            ("/collection/tracks", Route::Tracks { album: None }),
            ("/search", Route::Search),
        ]
    }

    #[test]
    fn every_route_round_trips_through_its_path() {
        for (path, route) in table() {
            assert_eq!(Route::from(path.to_string()), route, "{path}");
            assert_eq!(route.to_string(), path);
        }
        for route in [
            Route::Albums {
                artist: Some(ID.into()),
            },
            Route::Tracks {
                album: Some(ID.into()),
            },
        ] {
            assert_eq!(Route::from(route.to_string()), route);
        }
        assert_eq!(
            Route::Albums {
                artist: Some(ID.into())
            }
            .to_string(),
            format!("/collection/albums?artist={ID}")
        );
    }

    #[test]
    fn root_and_trailing_slashes_land_on_the_browse_root() {
        for url in ["", "/", "/collection/", "/collection#x"] {
            assert_eq!(Route::new(url), Route::Artists, "{url:?}");
        }
        assert_eq!(Route::new("/login/"), Route::Login);
    }

    #[test]
    fn anything_else_is_not_found() {
        for url in [
            "/podcasts",
            "/collection/nope",
            "/collection/albums?artist=<script>",
            "/404",
        ] {
            assert_eq!(Route::new(url), Route::NotFound, "{url}");
        }
    }

    #[test]
    fn everything_but_login_setup_and_404_is_guarded() {
        assert!(Route::Search.requires_auth());
        assert!(Route::Artists.requires_auth());
        assert!(!Route::Login.requires_auth());
        assert!(!Route::Setup.requires_auth());
        assert!(!Route::NotFound.requires_auth());
    }
}
