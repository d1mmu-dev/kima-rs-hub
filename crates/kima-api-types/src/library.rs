//! `/api/library/{artists,albums,tracks}` list responses and `POST /library/scan`.
//!
//! Shapes mirror what `kima-api::routes::library` emits today (camelCase, many always-null
//! compatibility fields). Fields whose type cannot be seen in a real response (always `null`) are
//! `Option<serde_json::Value>`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// `{ id, name }`: the artist reference inside albums and tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    pub id: Uuid,
    pub name: String,
}

/// The artist reference inside an album (`mbid` is always present, usually `null`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumArtistRef {
    pub id: Uuid,
    pub mbid: Option<String>,
    pub name: String,
}

/// One entry of `GET /library/artists`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub id: Uuid,
    pub name: String,
    pub mbid: Option<String>,
    pub album_count: i64,
    pub track_count: i64,
    pub cover_art: Option<String>,
    pub hero_url: Option<String>,
}

/// `GET /library/artists`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistsResponse {
    pub artists: Vec<Artist>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
    pub next_cursor: Option<Uuid>,
}

/// One entry of `GET /library/albums`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: Uuid,
    pub title: String,
    pub year: Option<i32>,
    /// Bare album id; the cover is `/api/library/cover-art/<coverArt>`.
    pub cover_url: String,
    pub cover_art: String,
    pub artist_id: Uuid,
    pub artist: AlbumArtistRef,
    pub is_compilation: bool,
    pub track_count: i64,
    pub song_count: i64,
    pub duration: i64,
    pub play_count: i64,
    pub owned: bool,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub mbid: Option<String>,
    pub rg_mbid: Option<String>,
    pub label: Option<String>,
    pub genres: Vec<String>,
    pub location: String,
    pub last_synced: Option<String>,
    pub display_title: Option<String>,
    pub display_year: Option<i32>,
    pub user_cover_url: Option<String>,
    pub user_genres: Vec<String>,
    pub has_user_overrides: bool,
    pub missing_tracks: Vec<Value>,
}

/// `GET /library/albums`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumsResponse {
    pub albums: Vec<Album>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// The album reference inside a track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAlbumRef {
    pub id: Uuid,
    pub title: String,
    pub year: Option<i32>,
    pub cover_url: String,
    pub cover_art: String,
    pub artist_id: Uuid,
    pub artist: AlbumArtistRef,
}

/// One entry of `GET /library/tracks`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: Uuid,
    pub title: String,
    /// Whole seconds.
    pub duration: i64,
    pub track_no: Option<i32>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub disc_subtitle: Option<String>,
    pub display_title: Option<String>,
    pub display_track_no: Option<i32>,
    pub has_user_overrides: bool,
    pub is_missing: bool,
    pub preview_url: Option<String>,
    pub play_count: i64,
    pub user_play_count: i64,
    pub listeners: i64,
    pub year: Option<i32>,
    pub codec: Option<String>,
    pub bitrate: Option<i32>,
    pub audio_features: Option<Value>,
    pub recording_mbid: Option<String>,
    pub isrc: Option<String>,
    pub album_id: Uuid,
    pub artist: ArtistRef,
    pub album: TrackAlbumRef,
}

/// `GET /library/tracks`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TracksResponse {
    pub tracks: Vec<Track>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// `POST /library/scan` (admin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStarted {
    pub job_id: Uuid,
    pub message: String,
    pub music_path: Option<String>,
}

/// `GET /library/scan/status/:jobId`: `status` is `waiting`, `active`, `completed` or `failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanStatus {
    pub status: String,
    pub progress: i64,
    pub result: Option<Value>,
    pub error: Option<String>,
}
