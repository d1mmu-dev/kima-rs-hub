//! Podcast feeds (`rss-parser.ts:88-118`): any public host, every hop checked and pinned
//! (`fetch.rs`), conditional when the caller has a copy, at most 32 MiB (D-203).

use std::time::Duration;

use reqwest::header::{ETAG, LAST_MODIFIED};

use crate::client::{Live, Net, disabled, permanent, reqwest_failure, transient};
use crate::fetch::{parse_http, refused_is_absent, status_failure};
use crate::outcome::Outcome;
use crate::source::Source;

/// Largest feed read (D-203; upstream reads any size).
pub const MAX_FEED: usize = 32 * 1024 * 1024;
/// Redirects followed per feed (`maxRedirects: 5`).
const MAX_REDIRECTS: usize = 5;
/// `rss-parser.ts:93`.
const ACCEPT_FEED: &str = "application/rss+xml, application/xml, text/xml, */*";

/// What the caller already has of a feed (a refresh): sent as `If-None-Match` /
/// `If-Modified-Since`, each only when present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Conditional<'a> {
    pub etag: Option<&'a str>,
    pub last_modified: Option<&'a str>,
}

impl Conditional<'_> {
    fn any(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

/// A fetched feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedFetch {
    /// 304 to a conditional request: the caller's copy is current.
    NotModified,
    /// The feed's text (read as UTF-8, as upstream's `responseType: "text"`) and its validators.
    Body {
        text: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

impl Net {
    /// Validate the initial feed URL before subscription, without fetching it. Fetches still
    /// validate and pin each hop independently, so this check is not a DNS-rebinding boundary.
    pub async fn validate_feed_url(&self, input: &str) -> Result<(), String> {
        let url = reqwest::Url::parse(input).map_err(|_| "Invalid URL".to_string())?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("Blocked scheme: {}:", url.scheme()));
        }
        let Some(live) = &self.live else {
            return Err("Network disabled".into());
        };
        if url.host_str() == Some("localhost") && !live.cfg.allow_private_images {
            return Err("Blocked hostname: localhost".into());
        }
        match live.checked_addrs::<()>(&url).await {
            Ok(_) => Ok(()),
            Err(Outcome::Permanent(f)) if f.code == "NOT_PUBLIC" => {
                Err("Blocked: internal address".into())
            }
            Err(Outcome::Transient(f)) if f.code == "NETWORK" => Err(format!(
                "DNS resolution failed for {}",
                url.host_str().unwrap_or("")
            )),
            Err(_) => Err("Invalid URL".into()),
        }
    }

    /// Fetches a feed (source `feeds`, never cached, not stopped by the identification switch:
    /// a person follows the show). `Absent` for a 4xx other than 429 or a refused address;
    /// `Transient` for a 429, 5xx, timeout (`budget`, at most the source's 30 s) or a 304 to a
    /// request that named no copy; `Permanent` for a bad URL, too many redirects or a feed over
    /// 32 MiB (`TOO_LARGE`).
    pub async fn get_feed(
        &self,
        url: &str,
        cond: Conditional<'_>,
        budget: Duration,
    ) -> Outcome<FeedFetch> {
        let Some(live) = &self.live else {
            return disabled();
        };
        let url = match parse_http(url) {
            Ok(u) => u,
            Err(out) => return out,
        };
        let source = Source::Feeds;
        let limit = budget.min(source.limits().timeout);
        let Ok(permit) = live.gates[&source].acquire(None).await else {
            return transient("BUSY", "feeds is busy");
        };
        let out = tokio::time::timeout(limit, live.feed(url, cond)).await;
        drop(permit);
        match out {
            Err(_) => transient("TIMEOUT", format!("no feed within {limit:?}")),
            Ok(out) => refused_is_absent(out),
        }
    }
}

impl Live {
    async fn feed(&self, url: reqwest::Url, cond: Conditional<'_>) -> Outcome<FeedFetch> {
        let agent = self.cfg.feed_user_agent.as_str();
        let mut headers = vec![("user-agent", agent), ("accept", ACCEPT_FEED)];
        if let Some(etag) = cond.etag {
            headers.push(("if-none-match", etag));
        }
        if let Some(since) = cond.last_modified {
            headers.push(("if-modified-since", since));
        }
        let mut res = match self.get_pinned(url, &headers, true, MAX_REDIRECTS).await {
            Ok(res) => res,
            Err(out) => return out,
        };
        let host = res.url().host_str().unwrap_or("").to_string();
        let status = res.status();
        if status.as_u16() == 304 {
            return if cond.any() {
                Outcome::Answered(FeedFetch::NotModified)
            } else {
                // `rss-parser.ts:114-118`: a host breaking the protocol.
                transient(
                    "BAD_RESPONSE",
                    "the host answered 304 Not Modified to a request that named no earlier copy",
                )
            };
        }
        if status.as_u16() >= 400 {
            return status_failure(status, &host);
        }
        let too_large = || {
            permanent(
                "TOO_LARGE",
                format!("feed from {host} larger than {MAX_FEED} bytes"),
            )
        };
        if res.content_length().is_some_and(|n| n > MAX_FEED as u64) {
            return too_large();
        }
        let header = |name| {
            res.headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let etag = header(ETAG);
        let last_modified = header(LAST_MODIFIED);
        let mut bytes = Vec::new();
        loop {
            match res.chunk().await {
                Ok(Some(chunk)) => {
                    if bytes.len() + chunk.len() > MAX_FEED {
                        return too_large();
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => return reqwest_failure(e),
            }
        }
        Outcome::Answered(FeedFetch::Body {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            etag,
            last_modified,
        })
    }
}

/// Compare feed identities the way upstream does: ignore scheme and trailing slashes, but
/// retain host, port, path and query. Invalid URLs compare as trimmed text.
pub fn same_feed(a: &str, b: &str) -> bool {
    fn key(s: &str) -> String {
        match reqwest::Url::parse(s.trim()) {
            Ok(u) => format!(
                "{}{}{}{}",
                u.host_str().unwrap_or(""),
                u.port().map(|p| format!(":{p}")).unwrap_or_default(),
                u.path().trim_end_matches('/'),
                u.query().map(|q| format!("?{q}")).unwrap_or_default()
            ),
            Err(_) => s.trim().to_string(),
        }
    }
    key(a) == key(b)
}
