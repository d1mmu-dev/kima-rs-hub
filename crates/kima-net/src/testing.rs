//! A replay server for tests (spec §24.1), the port of upstream's `musicbrainzReplay.ts`: an
//! axum server on an ephemeral local port that answers recorded requests, keeps a list of
//! what was asked, and answers 599 naming the request for anything not recorded.
//!
//! A request matches an entry on method, path, the sorted percent-decoded query pairs and, for a
//! POST entry that names one, the SHA-256 of the body. Several entries for one request are
//! served in order, the last one repeating.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::Response;
use base64::Engine;
use percent_encoding::percent_decode_str;

pub use crate::record::{Entry, Recording};

/// Lowercase hex SHA-256 of a request body, for [`Entry::request_body_sha256`].
pub fn body_sha256(body: &str) -> String {
    crate::cache::sha256_hex(body.as_bytes())
}

/// Method, path, sorted decoded query pairs, body hash (POST entries that name one).
type Key = (String, String, Vec<(String, String)>, Option<String>);

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// Path and sorted decoded query of a path-with-query or full URL.
fn split(url: &str) -> (String, Vec<(String, String)>) {
    let rest = match url.find("://") {
        Some(i) => {
            let after = &url[i + 3..];
            after.find('/').map_or("/", |j| &after[j..])
        }
        None => url,
    };
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    pairs.sort();
    (decode(path), pairs)
}

struct State {
    entries: HashMap<Key, Vec<Entry>>,
    served: Mutex<HashMap<Key, usize>>,
    asked: Mutex<Vec<String>>,
    user_agents: Mutex<Vec<String>>,
    content_types: Mutex<Vec<String>>,
    headers: Mutex<Vec<Vec<(String, String)>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    fn answer(&self, method: &Method, uri: &Uri, headers: &HeaderMap, body: &Bytes) -> Response {
        let target = uri
            .path_and_query()
            .map_or_else(|| uri.path().to_string(), |pq| pq.as_str().to_string());
        let asked = format!("{method} {target}");
        lock(&self.asked).push(asked.clone());
        lock(&self.user_agents).push(
            headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string(),
        );
        lock(&self.content_types).push(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string(),
        );
        lock(&self.headers).push(
            headers
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect(),
        );
        let (path, query) = split(&target);
        let sha = body_sha256(&String::from_utf8_lossy(body));
        let keyed = (method.to_string(), path.clone(), query.clone(), Some(sha));
        let any = (method.to_string(), path, query, None);
        let key = [keyed, any]
            .into_iter()
            .find(|k| self.entries.contains_key(k));
        let Some(key) = key else {
            return Response::builder()
                .status(599)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from(format!("no recorded reply for {asked}")))
                .expect("599 response");
        };
        let entries = &self.entries[&key];
        let entry = {
            let mut served = lock(&self.served);
            let n = served.entry(key).or_insert(0);
            let entry = &entries[(*n).min(entries.len() - 1)];
            *n += 1;
            entry
        };
        let mut res = Response::builder()
            .status(StatusCode::from_u16(entry.status).expect("a recorded status is valid"));
        let body = match &entry.body_base64 {
            Some(b64) => base64::engine::general_purpose::STANDARD
                .decode(b64)
                .expect("body_base64 is base64"),
            None => {
                res = res.header(header::CONTENT_TYPE, "application/json");
                serde_json::to_vec(&entry.body).expect("a JSON value serializes")
            }
        };
        for (k, v) in &entry.headers {
            res = res.header(
                HeaderName::try_from(k.as_str()).expect("recorded header name"),
                HeaderValue::try_from(v.as_str()).expect("recorded header value"),
            );
        }
        res.body(Body::from(body)).expect("recorded response")
    }
}

/// A running replay server; stopped when dropped.
pub struct Replay {
    url: String,
    state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}

impl Replay {
    pub async fn start(rec: Recording) -> Replay {
        let mut entries: HashMap<Key, Vec<Entry>> = HashMap::new();
        for entry in rec.0 {
            let method = entry.method.to_ascii_uppercase();
            let (path, query) = split(&entry.url);
            let sha = entry.request_body_sha256.clone();
            entries
                .entry((method, path, query, sha))
                .or_default()
                .push(entry);
        }
        let state = Arc::new(State {
            entries,
            served: Mutex::new(HashMap::new()),
            asked: Mutex::new(Vec::new()),
            user_agents: Mutex::new(Vec::new()),
            content_types: Mutex::new(Vec::new()),
            headers: Mutex::new(Vec::new()),
        });
        let shared = state.clone();
        let app = Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let state = shared.clone();
                async move { state.answer(&method, &uri, &headers, &body) }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding the replay server");
        let addr = listener.local_addr().expect("replay server address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("replay server");
        });
        Replay {
            url: format!("http://{addr}"),
            state,
            task,
        }
    }

    /// `http://127.0.0.1:<port>`, no trailing slash.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Every request received, in order, as `METHOD /path?query` (as sent).
    pub fn asked(&self) -> Vec<String> {
        lock(&self.state.asked).clone()
    }

    /// The User-Agent of every request received, in order.
    pub fn user_agents(&self) -> Vec<String> {
        lock(&self.state.user_agents).clone()
    }

    /// The `Content-Type` of every request received, in order (empty when none was sent).
    pub fn content_types(&self) -> Vec<String> {
        lock(&self.state.content_types).clone()
    }

    /// The headers of every request received, in order (lowercase names).
    pub fn headers(&self) -> Vec<Vec<(String, String)>> {
        lock(&self.state.headers).clone()
    }

    /// The value of header `name` in request `i` (0-based), if it was sent.
    pub fn header(&self, i: usize, name: &str) -> Option<String> {
        lock(&self.state.headers)
            .get(i)?
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    }
}

impl Drop for Replay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// How an [`AudioHost`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioMode {
    /// Serves the bytes, honouring one `Range` (`bytes=a-b`, `bytes=a-`, `bytes=-n`): 206, or 416
    /// with `Content-Range: bytes */<len>` when it cannot be satisfied; several ranges get the
    /// whole file (200).
    Normal,
    /// A request with a `Range` header gets 400 (a CDN that refuses ranges).
    RefuseRanges,
    /// Sends this `Content-Length` whatever the body is.
    LieLength(u64),
    /// 302 to this location.
    RedirectTo(String),
    /// This status with an empty body.
    Status(u16),
    /// Sends the full headers, then this many bytes, then drops the connection.
    CutAfter(usize),
    /// Waits this long before answering at all (a host that does not answer).
    HoldHeaders(std::time::Duration),
}

#[derive(Default)]
struct AudioState {
    hits: Vec<String>,
    user_agents: Vec<String>,
    ranges: Vec<Option<String>>,
    /// Connections whose body could not be written to the end (the client went away).
    closed_early: usize,
}

/// A loopback HTTP/1.1 server for episode audio (tests): range-aware, one request per
/// connection, the body written in small chunks so a client that stops reading is noticed.
pub struct AudioHost {
    url: String,
    state: Arc<Mutex<AudioState>>,
    task: tokio::task::JoinHandle<()>,
}

/// One range of a `bytes=` header over `len` bytes: `Some(Ok((start, end)))` inclusive,
/// `Some(Err(()))` unsatisfiable, `None` when the header is not a single byte range.
fn one_range(header: &str, len: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 || len == 0 {
            return Some(Err(()));
        }
        return Some(Ok((len.saturating_sub(n), len - 1)));
    }
    let start: u64 = a.parse().ok()?;
    let end: u64 = if b.is_empty() {
        len.saturating_sub(1)
    } else {
        b.parse::<u64>().ok()?.min(len.saturating_sub(1))
    };
    if start >= len || start > end {
        return Some(Err(()));
    }
    Some(Ok((start, end)))
}

impl AudioHost {
    pub async fn start(bytes: Vec<u8>, mode: AudioMode) -> AudioHost {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding the audio host");
        let addr = listener.local_addr().expect("audio host address");
        let state = Arc::new(Mutex::new(AudioState::default()));
        let shared = state.clone();
        let bytes = Arc::new(bytes);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let state = shared.clone();
                let bytes = bytes.clone();
                let mode = mode.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let mut lines = head.split("\r\n");
                    let target = lines
                        .next()
                        .and_then(|l| l.split(' ').nth(1))
                        .unwrap_or("/")
                        .to_string();
                    let header = |name: &str| {
                        head.split("\r\n").skip(1).find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case(name)
                                .then(|| v.trim().to_string())
                        })
                    };
                    let range = header("range");
                    {
                        let mut st = lock(&state);
                        st.hits.push(target);
                        st.user_agents
                            .push(header("user-agent").unwrap_or_default());
                        st.ranges.push(range.clone());
                    }
                    let len = bytes.len() as u64;
                    let empty = |status: &str, extra: &str| {
                        format!(
                            "HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                    };
                    let (head, body, cut): (String, &[u8], Option<usize>) = match &mode {
                        AudioMode::HoldHeaders(d) => {
                            tokio::time::sleep(*d).await;
                            (empty("204 No Content", ""), &[], None)
                        }
                        AudioMode::Status(code) => {
                            (empty(&format!("{code} Status"), ""), &[], None)
                        }
                        AudioMode::RedirectTo(to) => (
                            empty("302 Found", &format!("Location: {to}\r\n")),
                            &[],
                            None,
                        ),
                        AudioMode::RefuseRanges if range.is_some() => {
                            (empty("400 Bad Request", ""), &[], None)
                        }
                        _ => {
                            let picked = range.as_deref().and_then(|r| one_range(r, len));
                            match picked {
                                Some(Err(())) => (
                                    empty(
                                        "416 Range Not Satisfiable",
                                        &format!("Content-Range: bytes */{len}\r\n"),
                                    ),
                                    &[],
                                    None,
                                ),
                                Some(Ok((a, b))) => {
                                    let body = &bytes[a as usize..=b as usize];
                                    (
                                        format!(
                                            "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/mpeg\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {a}-{b}/{len}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                            body.len()
                                        ),
                                        body,
                                        None,
                                    )
                                }
                                None => {
                                    let stated = match &mode {
                                        AudioMode::LieLength(n) => *n,
                                        _ => len,
                                    };
                                    let cut = match &mode {
                                        AudioMode::CutAfter(n) => Some(*n),
                                        _ => None,
                                    };
                                    (
                                        format!(
                                            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nAccept-Ranges: bytes\r\nContent-Length: {stated}\r\nConnection: close\r\n\r\n"
                                        ),
                                        &bytes[..],
                                        cut,
                                    )
                                }
                            }
                        }
                    };
                    if sock.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let body = cut.map_or(body, |n| &body[..n.min(body.len())]);
                    for chunk in body.chunks(16 * 1024) {
                        if sock.write_all(chunk).await.is_err() || sock.flush().await.is_err() {
                            lock(&state).closed_early += 1;
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        AudioHost {
            url: format!("http://{addr}"),
            state,
            task,
        }
    }

    /// `http://127.0.0.1:<port>`, no trailing slash; any path serves the bytes.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Every request target received, in order.
    pub fn hits(&self) -> Vec<String> {
        lock(&self.state).hits.clone()
    }

    /// The User-Agent of every request received, in order.
    pub fn user_agents(&self) -> Vec<String> {
        lock(&self.state).user_agents.clone()
    }

    /// The `Range` header of every request received, in order.
    pub fn ranges(&self) -> Vec<Option<String>> {
        lock(&self.state).ranges.clone()
    }

    /// How many answers stopped because the client went away before the body was written.
    pub fn closed_early(&self) -> usize {
        lock(&self.state).closed_early
    }
}

impl Drop for AudioHost {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_sorts_and_decodes() {
        assert_eq!(
            split("https://musicbrainz.org/ws/2/x?b=a%2Bb&a=1"),
            (
                "/ws/2/x".to_string(),
                vec![("a".into(), "1".into()), ("b".into(), "a+b".into())]
            )
        );
        assert_eq!(split("/y"), ("/y".to_string(), vec![]));
    }

    #[test]
    fn one_range_forms() {
        assert_eq!(one_range("bytes=10-19", 100), Some(Ok((10, 19))));
        assert_eq!(one_range("bytes=90-", 100), Some(Ok((90, 99))));
        assert_eq!(one_range("bytes=-5", 100), Some(Ok((95, 99))));
        assert_eq!(one_range("bytes=95-200", 100), Some(Ok((95, 99))));
        assert_eq!(one_range("bytes=100-", 100), Some(Err(())));
        assert_eq!(one_range("bytes=0-1,5-6", 100), None);
        assert_eq!(one_range("items=0-1", 100), None);
    }
}
