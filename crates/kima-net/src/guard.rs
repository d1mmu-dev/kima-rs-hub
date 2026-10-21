//! The network guard (spec §24.1): with `KIMA_NET_DENY_REAL=1` (CI) or a test config, any host
//! but this machine is a bug in a test, so it panics loudly instead of reaching the internet.

/// Hosts a test may talk to (the replay server).
fn is_local(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    )
}

/// Panics when `deny_real` is set and `url` names a real host.
pub(crate) fn check(url: &reqwest::Url, deny_real: bool) {
    if deny_real && !is_local(url) {
        // Scheme, host and path only: a query may carry an API key.
        panic!(
            "kima-net refused a real host: {}://{}{}",
            url.scheme(),
            url.host_str().unwrap_or(""),
            url.path()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn local_hosts_pass() {
        check(&url("http://127.0.0.1:4000/x"), true);
        check(&url("http://localhost/x"), true);
        check(&url("http://[::1]:80/x"), true);
        check(&url("https://musicbrainz.org/ws/2"), false);
    }

    #[test]
    #[should_panic(expected = "kima-net refused a real host: https://musicbrainz.org/ws/2")]
    fn real_host_panics_without_query() {
        check(&url("https://musicbrainz.org/ws/2?api_key=secret"), true);
    }
}
