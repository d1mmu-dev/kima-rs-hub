//! Playlist API DTOs used by kima-api-types.
//!
//! TODO: reconsider serde defaults (camelCase vs snake_case) after resolving
//! crate-level indentation drift in migrated DTO tests.

use crate::library::Track;
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// The playlist summary returned by list/detail views and saved fixtures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Uuid,
    pub is_public: bool,
    pub is_hidden: bool,
    pub item_count: i64,
    pub track_count: i64,
    pub is_playlist_visible: bool,
    pub create_time: i64,
    pub update_time: i64,
    pub reverse_order: bool,
}

/// One track of a playlist. `type` is only present in the detail answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistItem {
    pub id: Uuid,
    pub playlist_id: Uuid,
    pub track_id: Uuid,
    pub sort: i32,
    /// Zero-padded position; compares as a string in item order.
    pub rank: String,
    pub track: Track,
}

impl fmt::Display for PlaylistItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{} ({})", self.playlist_id, self.track_id, self.rank)
    }
}

/// `GET /playlists/:id`. `items` and `mergedItems` are identical in kima-rs (no pending tracks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    #[serde(flatten)]
    pub summary: PlaylistSummary,
    pub pending_count: i64,
    pub pending_tracks: Vec<Value>,
    pub merged_items: Vec<PlaylistItem>,
    pub items: Vec<PlaylistItem>,
}

/// Body of `POST /playlists/:id/items` (exactly one of `trackId` / `trackIds`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddItemBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_ids: Option<Vec<Uuid>>,
}

impl AddItemBody {
    pub fn one(track_id: Uuid) -> Self {
        Self {
            track_id: Some(track_id),
            track_ids: None,
        }
    }

    pub fn many(track_ids: Vec<Uuid>) -> Self {
        Self {
            track_id: None,
            track_ids: Some(track_ids),
        }
    }
}

/// Single replacement of a playlist item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceItemBody {
    pub item_id: Uuid,
    pub track_id: Uuid,
}

/// Answer of `POST /playlists/:id/items`. A track already in the playlist is not added again
/// and the accepted set is still reported as `updated` so callers do not treat it as an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddItemsResponse {
    pub status: String,
    pub added: i64,
    pub duplicates: i64,
    pub rejected: Vec<Uuid>,
    pub updated: Vec<Uuid>,
}

/// Body of `POST /playlists/:id/items/:itemId/move` (per-item patch contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveItemBody {
    pub after_item_id: Option<Uuid>,
}

/// Answer of `POST /playlists/:id/items/:itemId/move`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveItemResponse {
    pub success: bool,
    pub rank: String,
}

/// Answer of `DELETE /playlists/:id/items/:trackId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveItemResponse {
    pub success: bool,
    pub rank: String,
}

/// One entry of `GET /playlists` (a bare JSON array).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistListEntry {
    #[serde(flatten)]
    pub summary: PlaylistSummary,
    #[serde(default)]
    pub items: Vec<PlaylistItem>,
}

/// Wire fixtures for playlist-related API captures.
impl PlaylistSummary {
    pub fn playlist_detached_owner() -> Self {
        Self {
            id: Uuid::nil(),
            name: "Road trip".into(),
            owner_id: Uuid::nil(),
            is_public: false,
            is_hidden: false,
            item_count: 2,
            track_count: 2,
            is_playlist_visible: true,
            create_time: 1_700_000_000,
            update_time: 1_700_000_120,
            reverse_order: false,
        }
    }

    pub fn playlist_hidden_owner() -> Self {
        Self {
            id: Uuid::nil(),
            name: "Road trip".into(),
            owner_id: Uuid::nil(),
            is_public: false,
            is_hidden: true,
            item_count: 2,
            track_count: 2,
            is_playlist_visible: true,
            create_time: 1_700_000_000,
            update_time: 1_700_000_120,
            reverse_order: false,
        }
    }
}

/// Create/parse payload consumed by API playlist fixtures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistCreate {
    pub name: String,
    pub is_public: Option<bool>,
    pub is_hidden: Option<bool>,
}

impl PlaylistCreate {
    pub fn new(name: &str, is_public: bool) -> Self {
        Self {
            name: name.into(),
            is_public: Some(is_public),
            is_hidden: None,
        }
    }
}
