//! Conditional request handling for streams: `ETag`, `If-None-Match`, `If-Range`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use axum::http::header::{IF_NONE_MATCH, IF_RANGE};
use kima_library::transcode::Quality;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conditional {
    /// Answer 304.
    NotModified,
    /// Serve the whole file: the validator in `If-Range` is stale.
    IgnoreRange,
    /// Proceed as usual.
    Normal,
}

/// `"<len hex>-<mtime ns hex>"`, plus `-<quality>` for a cached transcode.
pub fn etag(len: u64, mtime: SystemTime, quality: Option<Quality>) -> String {
    let ns = mtime
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos();
    match quality {
        Some(q) => format!("\"{len:x}-{ns:x}-{}\"", q.name()),
        None => format!("\"{len:x}-{ns:x}\""),
    }
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// `If-None-Match` is compared strongly (a `W/` prefix never matches). `If-Range` keeps the range
/// only when it names this ETag or this `Last-Modified` second.
pub fn evaluate(headers: &HeaderMap, etag: &str, last_modified: SystemTime) -> Conditional {
    if let Some(v) = headers.get(IF_NONE_MATCH).and_then(|v| v.to_str().ok()) {
        if v.split(',').map(str::trim).any(|c| c == "*" || c == etag) {
            return Conditional::NotModified;
        }
    }
    if let Some(v) = headers.get(IF_RANGE).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        let fresh =
            v == etag || httpdate::parse_http_date(v).is_ok_and(|d| secs(d) == secs(last_modified));
        if !fresh {
            return Conditional::IgnoreRange;
        }
    }
    Conditional::Normal
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn t(secs: u64, ns: u32) -> SystemTime {
        UNIX_EPOCH + Duration::new(secs, ns)
    }

    fn h(name: &'static str, v: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(name, HeaderValue::from_str(v).unwrap());
        m
    }

    #[test]
    fn etag_format() {
        let m = t(1, 500);
        assert_eq!(
            etag(255, m, None),
            format!("\"ff-{:x}\"", 1_000_000_500u128)
        );
        assert_eq!(
            etag(16, m, Some(Quality::High)),
            format!("\"10-{:x}-high\"", 1_000_000_500u128)
        );
    }

    #[test]
    fn if_none_match_list_and_star() {
        let e = etag(1, t(1, 0), None);
        let lm = t(1, 0);
        assert_eq!(
            evaluate(&h("if-none-match", &format!("\"x\", {e}")), &e, lm),
            Conditional::NotModified
        );
        assert_eq!(
            evaluate(&h("if-none-match", "*"), &e, lm),
            Conditional::NotModified
        );
        assert_eq!(
            evaluate(&h("if-none-match", "\"x\""), &e, lm),
            Conditional::Normal
        );
        assert_eq!(evaluate(&HeaderMap::new(), &e, lm), Conditional::Normal);
    }

    #[test]
    fn weak_etag_never_matches() {
        let e = etag(1, t(1, 0), None);
        assert_eq!(
            evaluate(&h("if-none-match", &format!("W/{e}")), &e, t(1, 0)),
            Conditional::Normal
        );
    }

    #[test]
    fn if_range_etag() {
        let e = etag(1, t(1, 0), None);
        assert_eq!(
            evaluate(&h("if-range", &e), &e, t(1, 0)),
            Conditional::Normal
        );
    }

    #[test]
    fn if_range_date() {
        let lm = t(1_700_000_000, 123_000_000);
        let date = httpdate::fmt_http_date(lm);
        assert_eq!(
            evaluate(&h("if-range", &date), "\"e\"", lm),
            Conditional::Normal
        );
    }

    #[test]
    fn if_range_mismatch_ignores_range() {
        let lm = t(1_700_000_000, 0);
        assert_eq!(
            evaluate(&h("if-range", "\"other\""), "\"e\"", lm),
            Conditional::IgnoreRange
        );
        let old = httpdate::fmt_http_date(t(1_600_000_000, 0));
        assert_eq!(
            evaluate(&h("if-range", &old), "\"e\"", lm),
            Conditional::IgnoreRange
        );
        assert_eq!(
            evaluate(&h("if-range", "garbage"), "\"e\"", lm),
            Conditional::IgnoreRange
        );
    }
}
