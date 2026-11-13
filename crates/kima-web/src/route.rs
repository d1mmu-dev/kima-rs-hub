//! The route table. Hand-written enum with a `From<String>` / `Display` pair (vertigo's router pattern).
//!
//! Login, setup (onboarding), the library browse root with its albums / tracks drill-downs, search,
//! playlists (list and detail), settings and podcasts (home, a followed show or an Apple preview,
//! a category's chart).

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
    /// `/playlists`.
    Playlists,
    /// `/playlists/<uuid>`.
    Playlist {
        id: String,
    },
    /// `/settings` (account slice).
    Settings,
    /// `/podcasts`: followed shows, listening shelves, discovery.
    Podcasts,
    /// `/podcasts/show?id=<uuid>`: a show stored on this server.
    PodcastShow {
        id: String,
    },
    /// `/podcasts/show?itunesId=<digits>`: a show from Apple's catalogue, not followed yet.
    PodcastPreview {
        itunes_id: String,
    },
    /// `/podcasts/category?id=<n>`: one Apple category's chart.
    PodcastCategory {
        id: u32,
    },
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
            "/playlists" => Route::Playlists,
            "/settings" => Route::Settings,
            "/podcasts" => Route::Podcasts,
            "/podcasts/show" => match (param("id"), raw(query, "itunesId")) {
                (Ok(Some(id)), _) => Route::PodcastShow { id },
                (Ok(None), Some(itunes)) if is_number(itunes) => Route::PodcastPreview {
                    itunes_id: itunes.to_owned(),
                },
                _ => Route::NotFound,
            },
            "/podcasts/category" => match raw(query, "id").filter(|v| is_number(v)) {
                Some(id) => id
                    .parse()
                    .map_or(Route::NotFound, |id| Route::PodcastCategory { id }),
                None => Route::NotFound,
            },
            _ => match path.strip_prefix("/playlists/") {
                Some(id) if is_id(id) => Route::Playlist { id: id.to_owned() },
                _ => Route::NotFound,
            },
        }
    }
}

/// The raw value of `name` in a query string, if present and non-empty.
fn raw<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
        .filter(|v| !v.is_empty())
}

/// Apple ids and category ids: decimal digits only, short enough for any id Apple uses.
fn is_number(s: &str) -> bool {
    !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit())
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
            Route::Playlists => f.write_str("/playlists"),
            Route::Playlist { id } => write!(f, "/playlists/{id}"),
            Route::Settings => f.write_str("/settings"),
            Route::Podcasts => f.write_str("/podcasts"),
            Route::PodcastShow { id } => write!(f, "/podcasts/show?id={id}"),
            Route::PodcastPreview { itunes_id } => write!(f, "/podcasts/show?itunesId={itunes_id}"),
            Route::PodcastCategory { id } => write!(f, "/podcasts/category?id={id}"),
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
            ("/playlists", Route::Playlists),
            ("/settings", Route::Settings),
            ("/podcasts", Route::Podcasts),
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
            Route::Playlist { id: ID.into() },
            Route::PodcastShow { id: ID.into() },
            Route::PodcastPreview {
                itunes_id: "1450241676".into(),
            },
            Route::PodcastCategory { id: 1303 },
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
            "/podcasts/nope",
            "/podcasts/show",
            "/podcasts/show?id=<script>",
            "/podcasts/show?itunesId=12a",
            "/podcasts/category",
            "/podcasts/category?id=-1",
            "/podcasts/category?id=99999999999",
            "/collection/nope",
            "/playlists/not-an-id!",
            "/playlists/x/y",
            "/collection/albums?artist=<script>",
            "/404",
        ] {
            assert_eq!(Route::new(url), Route::NotFound, "{url}");
        }
    }

    #[test]
    fn podcast_pages_use_query_params_like_the_rest_of_kima_web() {
        assert_eq!(
            Route::PodcastShow { id: ID.into() }.to_string(),
            format!("/podcasts/show?id={ID}")
        );
        assert_eq!(
            Route::PodcastPreview {
                itunes_id: "555".into()
            }
            .to_string(),
            "/podcasts/show?itunesId=555"
        );
        assert_eq!(
            Route::PodcastCategory { id: 1303 }.to_string(),
            "/podcasts/category?id=1303"
        );
        // A followed show's id wins over a preview's Apple id when both are given.
        assert_eq!(
            Route::new(&format!("/podcasts/show?itunesId=555&id={ID}")),
            Route::PodcastShow { id: ID.into() }
        );
        assert!(Route::Podcasts.requires_auth());
        assert!(Route::PodcastCategory { id: 1 }.requires_auth());
    }

    #[test]
    fn everything_but_login_setup_and_404_is_guarded() {
        for r in [
            Route::Artists,
            Route::Search,
            Route::Playlists,
            Route::Playlist { id: ID.into() },
            Route::Settings,
        ] {
            assert!(r.requires_auth(), "{r}");
        }
        assert!(Route::Artists.requires_auth());
        assert!(!Route::Login.requires_auth());
        assert!(!Route::Setup.requires_auth());
        assert!(!Route::NotFound.requires_auth());
    }
}
