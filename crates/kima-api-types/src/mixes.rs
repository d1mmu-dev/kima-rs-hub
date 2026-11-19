//! `/api/mixes` track rows (M5b Tasks 9 and 10; owner Q6 = B, D-264): upstream's Prisma `Track` with its `album` and
//! the album's `artist{id,name,mbid}`, every scalar key in upstream's model order and name. Where kima has no data
//! the value is `null`, never a missing key (also for upstream's non-nullable columns). The column-by-column map is
//! Appendix D of the M5b plan.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One track row of `GET /api/mixes/{id}`, `GET /api/mixes/mood/buckets/{mood}` and the mood-mix save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixTrack {
    pub id: String,
    pub album_id: String,
    pub title: String,
    pub track_no: i32,
    pub duration: i32,
    pub mime: Option<String>,
    pub file_path: String,
    pub file_modified: String,
    pub file_size: i64,
    pub bpm: Option<f64>,
    pub bpm_confidence: Option<f64>,
    pub beats_count: Option<i32>,
    pub key: Option<String>,
    pub key_scale: Option<String>,
    pub key_strength: Option<f64>,
    pub energy: Option<f64>,
    pub loudness: Option<f64>,
    pub dynamic_range: Option<f64>,
    pub danceability: Option<f64>,
    pub valence: Option<f64>,
    pub arousal: Option<f64>,
    pub deam_valence: Option<f64>,
    pub deam_arousal: Option<f64>,
    pub clap_valence_raw: Option<f64>,
    pub clap_arousal_raw: Option<f64>,
    pub instrumentalness: Option<f64>,
    pub acousticness: Option<f64>,
    pub speechiness: Option<f64>,
    pub danceability_ml: Option<f64>,
    pub mood_tags: Vec<String>,
    pub essentia_genres: Option<Vec<String>>,
    pub analysis_status: Option<String>,
    pub analysis_version: Option<String>,
    pub analysis_mode: Option<String>,
    pub analyzed_at: Option<String>,
    pub analysis_error: Option<String>,
    pub analysis_retry_count: Option<i32>,
    pub updated_at: String,
    pub display_title: Option<String>,
    pub display_track_no: Option<i32>,
    pub has_user_overrides: bool,
    pub analysis_started_at: Option<String>,
    pub vibe_analysis_status: Option<String>,
    pub vibe_analysis_started_at: Option<String>,
    pub vibe_analysis_error: Option<String>,
    pub vibe_analysis_retry_count: Option<i32>,
    pub vibe_analysis_status_updated_at: Option<String>,
    pub isrc: Option<String>,
    pub isrc_source: Option<String>,
    pub acoustid_fingerprint: Option<String>,
    pub recording_mbid: Option<String>,
    pub corrupt: bool,
    pub missing_since: Option<String>,
    pub scan_status: Option<String>,
    pub scan_error: Option<String>,
    pub scan_started_at: Option<String>,
    pub disc_number: Option<i32>,
    pub disc_subtitle: Option<String>,
    pub album: MixAlbum,
}

/// The track's `album` (upstream's Prisma `Album` scalars) with its `artist`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixAlbum {
    pub id: String,
    pub rg_mbid: Option<String>,
    pub rg_mbid_source: Option<String>,
    pub artist_id: String,
    pub title: String,
    pub identity_key: Option<String>,
    pub sort_name: String,
    pub year: Option<i32>,
    /// The album's cover token (D-265).
    pub cover_url: Option<String>,
    pub primary_type: Option<String>,
    pub label: Option<String>,
    pub genres: Option<Value>,
    pub last_synced: Option<String>,
    pub location: String,
    pub display_title: Option<String>,
    pub display_year: Option<i32>,
    pub has_user_overrides: bool,
    pub user_cover_url: Option<String>,
    pub user_genres: Option<Vec<String>>,
    pub original_year: Option<i32>,
    pub artist: MixArtist,
}

/// The album's `artist`, as upstream selects it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixArtist {
    pub id: String,
    pub name: String,
    pub mbid: Option<String>,
}
