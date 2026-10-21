//! Keeping API keys out of everything kima writes down (spec §7.4): cache keys and rows, log
//! lines, failure messages and recorded fixtures. A secret is the value of an `api_key` or
//! `client` pair, in a query or a form body.

use crate::cache::SECRET_KEYS;
use crate::req::{Req, encode};

/// What a secret is replaced with.
pub(crate) const REDACTED: &str = "REDACTED";

/// Values shorter than this are not scrubbed from free text: a real key is far longer, and
/// scrubbing a one-letter test value would mangle every message.
const MIN_SCRUBBED: usize = 4;

pub(crate) fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS.contains(&key)
}

/// The decoded key of one `k=v` form or query piece.
fn piece_key(piece: &str) -> String {
    form_urlencoded::parse(piece.as_bytes())
        .next()
        .map(|(k, _)| k.into_owned())
        .unwrap_or_default()
}

/// A form body (or query string) without its secret pairs, the rest in order.
pub(crate) fn strip_form(body: &str) -> String {
    body.split('&')
        .filter(|p| !p.is_empty() && !is_secret_key(&piece_key(p)))
        .collect::<Vec<_>>()
        .join("&")
}

/// A form body (or query string) with each secret value replaced by `REDACTED`.
pub(crate) fn redact_form(body: &str) -> String {
    body.split('&')
        .map(|p| {
            let key = piece_key(p);
            if is_secret_key(&key) {
                let raw_key = p.split_once('=').map_or(p, |(k, _)| k);
                format!("{raw_key}={REDACTED}")
            } else {
                p.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// A URL with each secret query value replaced by `REDACTED`.
pub(crate) fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((head, query)) => {
            let (query, fragment) = match query.split_once('#') {
                Some((q, f)) => (q, Some(f)),
                None => (query, None),
            };
            let mut out = format!("{head}?{}", redact_form(query));
            if let Some(f) = fragment {
                out.push('#');
                out.push_str(f);
            }
            out
        }
        None => url.to_string(),
    }
}

/// The secret values one request carries, in every spelling they can come back in.
#[derive(Debug, Default)]
pub(crate) struct Secrets(Vec<String>);

impl Secrets {
    pub(crate) fn of(req: &Req) -> Secrets {
        let mut values = Vec::new();
        for (k, v) in &req.query {
            if is_secret_key(k) {
                values.push(v.clone());
                values.push(encode(v));
                values.push(form_urlencoded::byte_serialize(v.as_bytes()).collect());
            }
        }
        if let Some(body) = &req.body {
            for piece in body.split('&') {
                let Some((k, raw)) = piece.split_once('=') else {
                    continue;
                };
                if !is_secret_key(&piece_key(k)) {
                    continue;
                }
                values.push(raw.to_string());
                if let Some((_, v)) = form_urlencoded::parse(piece.as_bytes()).next() {
                    values.push(encode(&v));
                    values.push(v.into_owned());
                }
            }
        }
        values.retain(|v| v.len() >= MIN_SCRUBBED);
        // Longest first, so a value is never half-replaced through a shorter spelling.
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        values.dedup();
        Secrets(values)
    }

    /// `text` with every secret value replaced by `REDACTED`.
    pub(crate) fn scrub(&self, text: &str) -> String {
        let mut out = text.to_string();
        for v in &self.0 {
            if out.contains(v.as_str()) {
                out = out.replace(v.as_str(), REDACTED);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Host;

    #[test]
    fn forms_lose_or_hide_their_secrets() {
        let body = "client=K%2F1&duration=10&api_key=Z&fingerprint=AQ";
        assert_eq!(strip_form(body), "duration=10&fingerprint=AQ");
        assert_eq!(
            redact_form(body),
            "client=REDACTED&duration=10&api_key=REDACTED&fingerprint=AQ"
        );
        assert_eq!(
            redact_url("https://x/m?api_key=abc&q=1#f"),
            "https://x/m?api_key=REDACTED&q=1#f"
        );
        assert_eq!(redact_url("https://x/m"), "https://x/m");
    }

    #[test]
    fn scrub_covers_raw_encoded_and_decoded_values() {
        let req = Req::post_form(Host::AcoustId, "/l", "client=ab%2Fcd&x=1".into())
            .query("api_key", "key with space");
        let s = Secrets::of(&req);
        assert_eq!(
            s.scrub("ab%2Fcd ab/cd key with space key%20with%20space key+with+space"),
            "REDACTED REDACTED REDACTED REDACTED REDACTED"
        );
        assert_eq!(s.scrub("nothing here"), "nothing here");
    }
}
