//! `/api/playback-state`: the signed-in user's resume point and queue.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::library::ArtistRef;

/// Album part of a stored queue entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueAlbum {
    pub id: String,
    pub title: String,
    pub cover_art: Option<String>,
}

/// Artist part of a stored queue entry (ids are strings here, as the server sanitises them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueArtist {
    pub id: String,
    pub name: String,
}

impl From<ArtistRef> for QueueArtist {
    fn from(a: ArtistRef) -> Self {
        Self {
            id: a.id.to_string(),
            name: a.name,
        }
    }
}

/// One queue entry, both in a save request and in the stored state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub title: String,
    pub duration: f64,
    pub artist: Option<QueueArtist>,
    pub album: Option<QueueAlbum>,
}

/// `GET /playback-state` (the body is `null` when nothing was saved yet) and the answer of
/// `POST /playback-state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackState {
    pub user_id: Uuid,
    pub playback_type: String,
    pub track_id: Option<String>,
    pub audiobook_id: Option<String>,
    pub podcast_id: Option<String>,
    /// `null` when the last save carried no queue.
    pub queue: Option<Vec<QueueItem>>,
    pub current_index: i32,
    pub is_shuffle: bool,
    pub position: i32,
    pub changed_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Body of `POST /playback-state`. Absent fields leave the stored value alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavePlaybackState {
    pub playback_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<Vec<QueueItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_index: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_shuffle: Option<bool>,
}
