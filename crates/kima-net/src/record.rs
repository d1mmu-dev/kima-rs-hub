//! Recorded exchanges (spec §24.1): what the replay server answers and what
//! [`NetConfig::record`](crate::NetConfig::record) collects from real answers for
//! `tools/record-fixtures`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    /// For POST: only a request whose body has this SHA-256 (lowercase hex) matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body_sha256: Option<String>,
}

/// A fixture: a list of entries.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
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
