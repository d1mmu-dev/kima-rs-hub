//! The client (spec §7.1-§7.3): switch, pause, slot, cell, send; rate limits retried here, every
//! other failure left to the stage ledger.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use kima_core::identify::IdentifySwitch;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rand::Rng;
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::time::Instant;

use crate::gate::{Busy, Gate};
use crate::guard;
use crate::outcome::{Evidence, Failure, Outcome};
use crate::req::{Method, Req};
use crate::source::{Bases, Source};

/// Largest JSON body read (spec §7.1).
const MAX_JSON_BODY: usize = 4 * 1024 * 1024;
/// Redirects followed per request (spec §7.1).
const MAX_REDIRECTS: usize = 3;
/// First rate-limit backoff (`rateLimiter.ts` `baseDelay` of MusicBrainz).
const BACKOFF_BASE: Duration = Duration::from_secs(2);
/// Longest backoff and longest honoured `Retry-After`.
const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// Pause after the retries are spent (`DEFAULT_PAUSE_WINDOW_MS`).
const PAUSE_BASE: Duration = Duration::from_secs(30);
/// Longest pause (the circuit breaker's cap).
const PAUSE_CAP: Duration = Duration::from_secs(60);
/// Consecutive rate-limited outcomes before the pause starts doubling.
const PAUSE_DOUBLING_AFTER: i32 = 5;

/// Query values: everything but RFC 3986's unreserved characters is escaped (a space is `%20`).
const QUERY: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// `kima-rs/<version> ( <contact> )`, the shape MusicBrainz asks for.
pub fn user_agent(version: &str, contact: &str) -> String {
    format!("kima-rs/{version} ( {contact} )")
}

/// How long to wait before retrying a rate-limited request (`rateLimiter.ts:250-266`). A
/// `Retry-After` of a positive whole number of seconds is honoured, capped at 60 s; anything
/// else (0, a date, garbage) gets `2 s × 2^attempt + jitter`, capped at 60 s.
pub fn retry_delay(attempt: u32, retry_after: Option<&str>, jitter: Duration) -> Duration {
    if let Some(secs) = retry_after
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
    {
        return Duration::from_secs(secs).min(BACKOFF_CAP);
    }
    let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
    BACKOFF_BASE
        .checked_mul(factor)
        .map_or(BACKOFF_CAP, |d| (d + jitter).min(BACKOFF_CAP))
}

/// The pause after the `consecutive`-th rate-limited outcome in a row: 30 s, doubling for each
/// one after the fifth, capped at 60 s (`rateLimiter.ts:231-246`).
pub fn pause_window(consecutive: i32) -> Duration {
    let over = (consecutive - PAUSE_DOUBLING_AFTER).clamp(0, 8) as u32;
    (PAUSE_BASE * (1 << over)).min(PAUSE_CAP)
}

/// Whether a redirect from `from` to `to` may be followed: same scheme, or http to https.
pub fn redirect_allowed(from: &str, to: &str) -> bool {
    let (Ok(from), Ok(to)) = (Url::parse(from), Url::parse(to)) else {
        return false;
    };
    matches!(
        (from.scheme(), to.scheme()),
        ("http", "http") | ("https", "https") | ("http", "https")
    )
}

/// Everything the client needs from the outside.
#[derive(Debug, Clone)]
pub struct NetConfig {
    /// Sent on every request.
    pub user_agent: String,
    pub bases: Bases,
    /// Panic on any host but this machine (the network guard).
    pub deny_real: bool,
    /// Lets image downloads reach private addresses (tests).
    pub allow_private_images: bool,
    /// Scales the limiter spacing and the rate-limit backoff, never the timeouts or the
    /// persisted pause lengths. 1.0 in production; tests shrink it so a retry ladder takes
    /// milliseconds.
    pub wait_scale: f64,
}

impl NetConfig {
    /// The real services. `deny_real` follows `KIMA_NET_DENY_REAL=1`.
    pub fn production(version: &str, contact: &str, musicbrainz_base: &str) -> NetConfig {
        NetConfig {
            user_agent: user_agent(version, contact),
            bases: Bases::production(musicbrainz_base),
            deny_real: std::env::var("KIMA_NET_DENY_REAL").is_ok_and(|v| v.trim() == "1"),
            allow_private_images: false,
            wait_scale: 1.0,
        }
    }

    /// For tests: the guard on, private images allowed, waits a thousandth as long.
    pub fn for_tests(bases: Bases) -> NetConfig {
        NetConfig {
            user_agent: user_agent(
                env!("CARGO_PKG_VERSION"),
                kima_core::config::DEFAULT_CONTACT,
            ),
            bases,
            deny_real: true,
            allow_private_images: true,
            wait_scale: 0.001,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Pause {
    until: OffsetDateTime,
    consecutive: i32,
}

struct Live {
    cfg: NetConfig,
    pool: PgPool,
    switch: IdentifySwitch,
    http: reqwest::Client,
    gates: HashMap<Source, Gate>,
    /// The `source_pauses` rows, mirrored; expired entries keep their count.
    pauses: Mutex<HashMap<Source, Pause>>,
}

/// The one outbound HTTP client of a process.
pub struct Net {
    live: Option<Live>,
}

/// What came back from the wire, before classification.
struct Raw {
    status: StatusCode,
    retry_after: Option<String>,
    body: Vec<u8>,
}

/// A rate-limited answer, before the retry decision.
struct Limited {
    retry_after: Option<String>,
    reason: String,
}

enum Step<T> {
    Done(Outcome<T>),
    Limited(Limited),
}

fn transient<T>(code: &str, message: impl Into<String>) -> Outcome<T> {
    Outcome::Transient(Failure::new(code, message))
}

fn permanent<T>(code: &str, message: impl Into<String>) -> Outcome<T> {
    Outcome::Permanent(Failure::new(code, message))
}

fn disabled<T>() -> Outcome<T> {
    transient("DISABLED", "outbound lookups are switched off")
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `/rate.?limit|too many requests/i` (`musicbrainz.ts:119-152`).
fn says_rate_limited(body: &[u8]) -> bool {
    let lower = body.to_ascii_lowercase();
    if lower.windows(17).any(|w| w == b"too many requests") {
        return true;
    }
    let mut i = 0;
    while let Some(pos) = lower[i..].windows(4).position(|w| w == b"rate") {
        let after = &lower[i + pos + 4..];
        if after.starts_with(b"limit") {
            return true;
        }
        // One character between: an ASCII byte, or a whole UTF-8 sequence.
        let skip = match after.first() {
            Some(b) if *b < 0x80 => 1,
            Some(b) if *b >= 0xF0 => 4,
            Some(b) if *b >= 0xE0 => 3,
            Some(_) => 2,
            None => return false,
        };
        if after.get(skip..).is_some_and(|a| a.starts_with(b"limit")) {
            return true;
        }
        i += pos + 1;
    }
    false
}

/// Drops the time below a microsecond, as Postgres stores it.
fn to_micros(t: OffsetDateTime) -> OffsetDateTime {
    let n = t.nanosecond();
    t.replace_nanosecond(n - n % 1000).unwrap_or(t)
}

fn reqwest_failure<T>(e: reqwest::Error) -> Outcome<T> {
    let e = e.without_url();
    if e.is_timeout() {
        transient("TIMEOUT", e.to_string())
    } else {
        transient("NETWORK", e.to_string())
    }
}

impl Net {
    pub fn new(cfg: NetConfig, pool: PgPool, switch: IdentifySwitch) -> Net {
        let http = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .redirect(reqwest::redirect::Policy::none())
            .gzip(true)
            .build()
            .expect("building the HTTP client");
        let scale = cfg.wait_scale.max(0.0);
        let gates = Source::ALL
            .into_iter()
            .map(|s| {
                let l = s.limits();
                (
                    s,
                    Gate::new(l.period.mul_f64(scale), l.burst, l.concurrency),
                )
            })
            .collect();
        Net {
            live: Some(Live {
                cfg,
                pool,
                switch,
                http,
                gates,
                pauses: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// A client that never calls out: every call is `Transient("DISABLED")`.
    pub fn disabled() -> Net {
        Net { live: None }
    }

    /// Reads the persisted pauses (startup), so a restart cannot burst a source that just
    /// limited us.
    pub async fn load_pauses(&self) -> sqlx::Result<()> {
        let Some(live) = &self.live else {
            return Ok(());
        };
        let rows = kima_db::http_cache::pauses(&live.pool).await?;
        let mut pauses = lock(&live.pauses);
        for (name, until, consecutive) in rows {
            if let Some(source) = Source::from_name(&name) {
                pauses.insert(source, Pause { until, consecutive });
            }
        }
        Ok(())
    }

    /// Scales every source's concurrency by `enrichmentConcurrency` (1-5), never above its burst.
    pub fn set_concurrency(&self, multiplier: u8) {
        if let Some(live) = &self.live {
            for (source, gate) in &live.gates {
                gate.set_concurrency(source.limits().concurrency_for(multiplier));
            }
        }
    }

    /// A source's concurrency cap now; `None` for a disabled client.
    pub fn concurrency(&self, source: Source) -> Option<u32> {
        Some(self.live.as_ref()?.gates[&source].concurrency())
    }

    /// When the source's pause ends, if it is paused now.
    pub fn paused_until(&self, source: Source) -> Option<OffsetDateTime> {
        let live = self.live.as_ref()?;
        let until = lock(&live.pauses).get(&source)?.until;
        (until > OffsetDateTime::now_utc()).then_some(until)
    }

    pub async fn get_json<T: DeserializeOwned>(&self, req: Req) -> Outcome<T> {
        self.json(req).await
    }

    pub async fn post_form_json<T: DeserializeOwned>(&self, req: Req) -> Outcome<T> {
        self.json(req).await
    }

    async fn json<T: DeserializeOwned>(&self, req: Req) -> Outcome<T> {
        let Some(live) = &self.live else {
            return disabled();
        };
        if !live.switch.enabled() {
            return disabled();
        }
        let source = req.host.source();
        if let Some(until) = self.paused_until(source) {
            return Outcome::RateLimited { until };
        }
        let limits = source.limits();
        let gate = &live.gates[&source];
        let per_request = req.timeout.unwrap_or(limits.timeout);
        let mut budget_ends: Option<Instant> = None;
        let mut attempt = 0;
        loop {
            let permit = match gate.acquire(req.max_wait).await {
                Ok(p) => p,
                Err(Busy) => {
                    return transient("BUSY", format!("{} is busy", source.as_str()));
                }
            };
            let now = Instant::now();
            let budget_ends = *budget_ends.get_or_insert_with(|| {
                req.budget
                    .map_or(now + Duration::from_secs(86_400), |b| now + b)
            });
            let ends = budget_ends.min(now + per_request);
            let sent = tokio::time::timeout_at(ends, live.send(&req)).await;
            drop(permit);
            let raw = match sent {
                Err(_) => return transient("TIMEOUT", format!("no answer within {per_request:?}")),
                Ok(Err(outcome)) => return outcome,
                Ok(Ok(raw)) => raw,
            };
            match classify::<T>(&req, source, raw) {
                Step::Done(outcome) => {
                    if matches!(outcome, Outcome::Answered(_) | Outcome::Absent(_)) {
                        live.clear_pause(source).await;
                    }
                    return outcome;
                }
                Step::Limited(limited) => {
                    if attempt < limits.rate_limit_retries {
                        let jitter = Duration::from_millis(rand::thread_rng().gen_range(0..1000));
                        let delay = retry_delay(attempt, limited.retry_after.as_deref(), jitter)
                            .mul_f64(live.cfg.wait_scale.max(0.0));
                        if Instant::now() + delay < budget_ends {
                            tokio::time::sleep(delay).await;
                            attempt += 1;
                            continue;
                        }
                    }
                    let until = live.pause(source, &limited.reason).await;
                    return Outcome::RateLimited { until };
                }
            }
        }
    }
}

impl Live {
    fn url(&self, req: &Req) -> Result<Url, Failure> {
        let mut url = format!("{}{}", self.cfg.bases.get(req.host), req.path);
        for (i, (k, v)) in req.query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.extend(utf8_percent_encode(k, QUERY));
            url.push('=');
            url.extend(utf8_percent_encode(v, QUERY));
        }
        Url::parse(&url).map_err(|e| Failure::new("BAD_URL", format!("{url}: {e}")))
    }

    /// Sends `req`, following up to three redirects by hand, and reads at most 4 MiB.
    async fn send<T>(&self, req: &Req) -> Result<Raw, Outcome<T>> {
        let mut url = self.url(req).map_err(Outcome::Permanent)?;
        let mut method = req.method;
        let mut body = req.body.clone();
        let mut hops = 0;
        loop {
            guard::check(&url, self.cfg.deny_real);
            let mut rb = match method {
                Method::Get => self.http.get(url.clone()),
                Method::Post => self
                    .http
                    .post(url.clone())
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(body.clone().unwrap_or_default()),
            };
            rb = rb.header(ACCEPT, "application/json");
            let mut res = rb.send().await.map_err(reqwest_failure)?;
            let status = res.status();
            if status.is_redirection() {
                if let Some(location) = res.headers().get(LOCATION) {
                    if hops == MAX_REDIRECTS {
                        return Err(permanent(
                            "TOO_MANY_REDIRECTS",
                            format!("more than {MAX_REDIRECTS} redirects"),
                        ));
                    }
                    let next = location
                        .to_str()
                        .ok()
                        .and_then(|l| url.join(l).ok())
                        .filter(|next| redirect_allowed(url.as_str(), next.as_str()));
                    let Some(next) = next else {
                        return Err(permanent(
                            "REDIRECT_REFUSED",
                            format!("redirect from {} refused", url.scheme()),
                        ));
                    };
                    // 307 and 308 repeat the request; the others continue with a GET.
                    if !matches!(
                        status,
                        StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
                    ) {
                        method = Method::Get;
                        body = None;
                    }
                    url = next;
                    hops += 1;
                    continue;
                }
            }
            let retry_after = res
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let too_large = || {
                transient(
                    "BODY_TOO_LARGE",
                    format!("answer larger than {MAX_JSON_BODY} bytes"),
                )
            };
            if res
                .content_length()
                .is_some_and(|n| n > MAX_JSON_BODY as u64)
            {
                return Err(too_large());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = res.chunk().await.map_err(reqwest_failure)? {
                if bytes.len() + chunk.len() > MAX_JSON_BODY {
                    return Err(too_large());
                }
                bytes.extend_from_slice(&chunk);
            }
            return Ok(Raw {
                status,
                retry_after,
                body: bytes,
            });
        }
    }

    /// Pauses `source` after its retries are spent, here and in `source_pauses`.
    async fn pause(&self, source: Source, reason: &str) -> OffsetDateTime {
        let now = OffsetDateTime::now_utc();
        let predicted = lock(&self.pauses)
            .get(&source)
            .map_or(1, |p| p.consecutive + 1);
        let mut until = to_micros(now + pause_window(predicted));
        let mut consecutive = predicted;
        match kima_db::http_cache::record_pause(&self.pool, source.as_str(), until, reason).await {
            Ok(stored) if stored != predicted => {
                // Another process (or no `load_pauses`) counted differently: the row wins.
                consecutive = stored;
                until = to_micros(now + pause_window(stored));
                if let Err(e) =
                    kima_db::http_cache::set_pause_until(&self.pool, source.as_str(), until).await
                {
                    tracing::warn!(source = source.as_str(), error = %e, "could not store a pause");
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(source = source.as_str(), error = %e, "could not store a pause");
            }
        }
        tracing::warn!(
            source = source.as_str(),
            reason,
            consecutive,
            %until,
            "source rate-limited us; paused"
        );
        lock(&self.pauses).insert(source, Pause { until, consecutive });
        until
    }

    /// The source answered: its consecutive count starts again.
    async fn clear_pause(&self, source: Source) {
        if lock(&self.pauses).remove(&source).is_none() {
            return;
        }
        if let Err(e) = kima_db::http_cache::clear_pause(&self.pool, source.as_str()).await {
            tracing::warn!(source = source.as_str(), error = %e, "could not clear a pause");
        }
    }
}

/// Spec §7.2: what an answer means.
fn classify<T: DeserializeOwned>(req: &Req, source: Source, raw: Raw) -> Step<T> {
    let status = raw.status;
    let code = status.as_u16();
    if status.is_success() {
        let value: Value = match serde_json::from_slice(&raw.body) {
            Ok(v) => v,
            Err(e) => return Step::Done(transient("BAD_RESPONSE", format!("not JSON: {e}"))),
        };
        if req.absent_when.is_some_and(|f| f(&value)) {
            return Step::Done(Outcome::Absent(Evidence(value)));
        }
        return Step::Done(match serde_json::from_value::<T>(value) {
            Ok(t) => Outcome::Answered(t),
            Err(e) => transient("BAD_RESPONSE", format!("unexpected answer: {e}")),
        });
    }
    let limited = code == 429
        || (code == 503 && source == Source::MusicBrainz)
        || (code >= 400 && says_rate_limited(&raw.body));
    if limited {
        let positive = raw
            .retry_after
            .as_deref()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&n| n > 0);
        let reason = match positive {
            Some(n) => format!("Retry-After {n}"),
            None => format!("HTTP {code}"),
        };
        return Step::Limited(Limited {
            retry_after: raw.retry_after,
            reason,
        });
    }
    let message = || {
        let text = String::from_utf8_lossy(&raw.body);
        let mut text: String = text.chars().take(200).collect();
        if text.is_empty() {
            text = status.to_string();
        }
        text
    };
    Step::Done(if code == 404 && req.absent_on_404 {
        let body = serde_json::from_slice::<Value>(&raw.body).unwrap_or(Value::Null);
        Outcome::Absent(Evidence(json!({ "status": 404, "body": body })))
    } else if code == 408 || status.is_server_error() {
        transient(&format!("HTTP_{code}"), message())
    } else if status.is_client_error() || status.is_redirection() {
        permanent(&format!("HTTP_{code}"), message())
    } else {
        transient(&format!("HTTP_{code}"), message())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_format() {
        assert_eq!(
            user_agent("0.1.0", "https://github.com/d1mmu-dev/kima-rs-hub"),
            "kima-rs/0.1.0 ( https://github.com/d1mmu-dev/kima-rs-hub )"
        );
        let cfg = NetConfig::production("1.2.3", "me@example.org", "https://musicbrainz.org/ws/2/");
        assert_eq!(cfg.user_agent, "kima-rs/1.2.3 ( me@example.org )");
        assert_eq!(cfg.bases.musicbrainz, "https://musicbrainz.org/ws/2");
        assert!(!cfg.allow_private_images);
    }

    /// `rateLimiterBackoff.test.ts`, with zero jitter.
    #[test]
    fn rate_limiter_backoff() {
        let s = Duration::from_secs;
        // Retry-After: 0 says nothing: the exponential delay, not an immediate retry.
        assert_eq!(retry_delay(0, Some("0"), Duration::ZERO), s(2));
        assert_eq!(retry_delay(1, Some("0"), Duration::ZERO), s(4));
        // A real Retry-After is honoured.
        assert_eq!(retry_delay(0, Some("20"), Duration::ZERO), s(20));
        assert_eq!(
            retry_delay(3, Some(" 2 "), Duration::from_millis(900)),
            s(2)
        );
        // Capped at 60 s either way.
        assert_eq!(retry_delay(0, Some("3600"), Duration::ZERO), s(60));
        assert_eq!(retry_delay(10, None, Duration::ZERO), s(60));
        // Garbage, negative and HTTP-date values fall back to the exponential delay.
        for v in ["-5", "soon", "Wed, 21 Oct 2026 07:28:00 GMT", "1.5"] {
            assert_eq!(retry_delay(2, Some(v), Duration::ZERO), s(8), "{v}");
        }
        assert_eq!(
            retry_delay(0, None, Duration::from_millis(500)),
            Duration::from_millis(2500)
        );
        assert_eq!(retry_delay(5, None, Duration::from_millis(999)), s(60));
    }

    #[test]
    fn pause_window_doubles_after_five() {
        let s = Duration::from_secs;
        assert_eq!(pause_window(1), s(30));
        assert_eq!(pause_window(5), s(30));
        assert_eq!(pause_window(6), s(60));
        assert_eq!(pause_window(40), s(60));
    }
}
