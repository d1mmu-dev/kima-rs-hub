//! Vibe map and mood read contracts (upstream v1.12.3).
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapResponse {
    pub tracks: Vec<MapTrack>,
    pub track_count: usize,
    pub computed_at: String,
    pub mood_signature: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapTrack {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub colour: f64,
    pub title: String,
    pub artist: String,
    pub artist_id: String,
    pub album_id: String,
    pub cover_url: Option<String>,
    pub dominant_mood: String,
    pub mood_score: f64,
    pub moods: BTreeMap<String, f64>,
    pub energy: Option<f64>,
    pub valence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoodSummary {
    pub shares: Vec<MoodShare>,
    pub albums: BTreeMap<String, String>,
    pub artists: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoodShare {
    pub key: String,
    pub fraction: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackMood {
    pub dominant_mood: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Computing {
    pub computing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub tracks: Vec<SearchTrack>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTrack {
    pub id: String,
    pub title: String,
    pub display_title: Option<String>,
    pub duration: i32,
    pub track_no: i32,
    pub distance: f64,
    pub similarity: f64,
    pub album: SearchAlbum,
    pub artist: SearchArtist,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchAlbum {
    pub id: String,
    pub title: String,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchArtist {
    pub id: String,
    pub name: String,
}
