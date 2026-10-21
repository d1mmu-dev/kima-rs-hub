//! The response cache's keys (spec §7.4): a canonical request with its secrets removed, hashed
//! under the source's name. The rows live in `kima_db::http_cache`.

use sha2::{Digest, Sha256};

use crate::req::{Method, Req, query_string};
use crate::secrets::{is_secret_key, strip_form};
use crate::source::Source;

/// Query and form keys that carry an API key or client key. They never reach a cache key, the
/// `request` column, a log line or a failure message.
pub const SECRET_KEYS: [&str; 2] = ["api_key", "client"];

/// `GET <base><path>?<sorted query>`; a POST adds a newline and the hex SHA-256 of its body.
/// `api_key` and `client` are removed from the query and from a form body first, so a changed
/// key neither orphans the cache nor lands in the `request` column.
pub fn canonical(req: &Req, base: &str) -> String {
    let mut pairs: Vec<&(String, String)> = req
        .query
        .iter()
        .filter(|(k, _)| !is_secret_key(k))
        .collect();
    pairs.sort();
    let mut out = format!(
        "{} {base}{}{}",
        req.method.as_str(),
        req.path,
        query_string(pairs)
    );
    if req.method == Method::Post {
        out.push('\n');
        out.push_str(&sha256_hex(
            strip_form(req.body.as_deref().unwrap_or("")).as_bytes(),
        ));
    }
    out
}

/// `<source>:` + the hex SHA-256 of `canonical`.
pub fn key(source: Source, canonical: &str) -> String {
    format!("{}:{}", source.as_str(), sha256_hex(canonical.as_bytes()))
}

/// Lowercase hex SHA-256.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Host;

    #[test]
    fn canonical_sorts_query_and_drops_secrets() {
        let req = Req::get(Host::Fanart, "/music/x")
            .query("b", "2")
            .query("api_key", "SECRET")
            .query("a", "x y");
        assert_eq!(
            canonical(&req, "https://webservice.fanart.tv/v3"),
            "GET https://webservice.fanart.tv/v3/music/x?a=x%20y&b=2"
        );
        let bare = Req::get(Host::Deezer, "/search");
        assert_eq!(
            canonical(&bare, "https://api.deezer.com"),
            "GET https://api.deezer.com/search"
        );
    }

    #[test]
    fn canonical_post_hashes_the_body_without_client() {
        let a = Req::post_form(
            Host::AcoustId,
            "/lookup",
            "client=AAA&duration=10&fingerprint=F".into(),
        );
        let b = Req::post_form(
            Host::AcoustId,
            "/lookup",
            "duration=10&client=BBB&fingerprint=F".into(),
        );
        let c = Req::post_form(
            Host::AcoustId,
            "/lookup",
            "client=AAA&duration=10&fingerprint=G".into(),
        );
        let base = "https://api.acoustid.org/v2";
        let ca = canonical(&a, base);
        assert!(
            ca.starts_with("POST https://api.acoustid.org/v2/lookup\n"),
            "{ca}"
        );
        assert!(!ca.contains("AAA"));
        assert_eq!(ca.split('\n').nth(1).map(str::len), Some(64));
        assert_eq!(ca, canonical(&b, base));
        assert_ne!(ca, canonical(&c, base));
    }

    #[test]
    fn key_is_source_and_hex_sha() {
        let k = key(Source::MusicBrainz, "GET https://x/y");
        let (source, hash) = k.split_once(':').unwrap();
        assert_eq!(source, "musicbrainz");
        assert_eq!(hash.len(), 64);
        assert!(
            hash.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_ne!(k, key(Source::Deezer, "GET https://x/y"));
    }
}
