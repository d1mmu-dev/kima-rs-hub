//! `GET /api/search?q=&type=&limit=` (library part; the audiobook/podcast groups are always empty).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::library::{Album, Artist, Track};

/// Artists, albums and tracks reuse the library list shapes. `type` filters which groups are filled;
/// the others come back as empty arrays, never missing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
    pub audiobooks: Vec<Value>,
    pub podcasts: Vec<Value>,
    pub episodes: Vec<Value>,
}
