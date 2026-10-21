//! A replay server for tests (spec §24.1), the port of upstream's `musicbrainzReplay.ts`: an
//! axum server on an ephemeral local port that answers recorded requests, keeps a list of
//! what was asked, and answers 599 naming the request for anything not recorded.
//!
//! A request matches an entry on method, path, the sorted percent-decoded query pairs and, for a
//! POST entry that names one, the SHA-256 of the body. Several entries for one request are
//! served in order, the last one repeating.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::Response;
use base64::Engine;
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One recorded exchange. `url` is a path with query (`/release-group/x?fmt=json`) or a full
/// URL, whose scheme and host are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub method: String,
    pub url: String,
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The answer, sent as JSON.
    #[serde(default)]
    pub body: serde_json::Value,
    /// The answer as raw bytes instead of `body` (non-JSON answers).
    #[serde(default)]
    pub body_base64: Option<String>,
    /// For POST: only a request whose body has this SHA-256 (lowercase hex) matches.
    #[serde(default)]
    pub request_body_sha256: Option<String>,
}

/// A fixture: a list of entries.
#[derive(Debug, Clone, Default)]
pub struct Recording(pub Vec<Entry>);

/// Upstream's `{source, recordedAt, replies: {"<path?query>": {status, body}}}`.
#[derive(Deserialize)]
struct Upstream {
    replies: BTreeMap<String, UpstreamReply>,
}

#[derive(Deserialize)]
struct UpstreamReply {
    status: u16,
    #[serde(default)]
    body: serde_json::Value,
}

impl Recording {
    /// Reads a fixture: a JSON list of entries, or upstream's recorded-replies object (each reply
    /// becomes a GET entry). Panics on a missing or malformed file: it is a broken test.
    pub fn load(path: impl AsRef<Path>) -> Recording {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()));
        if let Ok(entries) = serde_json::from_str::<Vec<Entry>>(&text) {
            return Recording(entries);
        }
        let upstream: Upstream = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("parsing fixture {}: {e}", path.display()));
        Recording(
            upstream
                .replies
                .into_iter()
                .map(|(url, reply)| Entry {
                    method: "GET".into(),
                    url,
                    status: reply.status,
                    headers: BTreeMap::new(),
                    body: reply.body,
                    body_base64: None,
                    request_body_sha256: None,
                })
                .collect(),
        )
    }
}

/// Lowercase hex SHA-256 of a request body, for [`Entry::request_body_sha256`].
pub fn body_sha256(body: &str) -> String {
    Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
}

impl Drop for Replay {
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
}
