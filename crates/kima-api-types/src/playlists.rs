//! `/api/playlists` shapes (the Spotify and mix fields are always `null` / empty in kima-rs).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::library::Track;

/// A playlist row: the answer of create and update, and the base of list and detail entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: Uuid,
    pub user_id: Uuid,
    pub mix_id: Option<Value>,
    pub name: String,
    pub is_public: bool,
    /// Shown newest first. Absent in answers of a server older than the 1.12 base.
    #[serde(default)]
    pub reverse_order: bool,
    pub created_at: String,
    pub updated_at: String,
    pub spotify_playlist_id: Option<String>,
    pub spotify_playlist_url: Option<String>,
}

/// `{ "username": ... }`: the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistUser {
    pub username: String,
}

/// `_count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemCount {
    pub items: i64,
}

/// What list and detail add to a [`Playlist`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    #[serde(flatten)]
    pub playlist: Playlist,
    pub user: PlaylistUser,
    pub track_count: i64,
    #[serde(rename = "_count")]
    pub count: ItemCount,
    pub is_owner: bool,
    pub is_hidden: bool,
}

/// One of the first items shown in a list entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewItem {
    /// The playlist item id (not the track id).
    pub id: Uuid,
    pub track: Track,
}

/// One entry of `GET /playlists` (a bare JSON array).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistListEntry {
    #[serde(flatten)]
    pub summary: PlaylistSummary,
    /// The first few items, for a cover mosaic.
    pub items: Vec<PreviewItem>,
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
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
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

/// Body of `POST /playlists`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePlaylist {
    pub name: String,
    #[serde(default)]
    pub is_public: bool,
}

/// Body of `PUT` / `PATCH /playlists/:id`; absent fields are left alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePlaylist {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_public: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse_order: Option<bool>,
}

/// Body of `POST /playlists/:id/items` (exactly one of `trackId` / `trackIds`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddItems {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_ids: Option<Vec<String>>,
}

impl AddItems {
    pub fn one(track_id: impl Into<String>) -> Self {
        Self {
            track_id: Some(track_id.into()),
            track_ids: None,
        }
    }
}

/// Answer of `POST /playlists/:id/items`. A track already in the playlist is not added again
/// (`duplicates`), which still answers 200.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddItemsResponse {
    pub added: i64,
    pub duplicated: bool,
    pub duplicates: i64,
    pub rejected: Vec<String>,
}

/// Answer of `DELETE /playlists/:id/items/:trackId`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveItemResponse {
    pub success: bool,
    pub removed: i64,
}

/// Answer of `DELETE /playlists/:id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageResponse {
    pub message: String,
}

/// Body of `POST /playlists/:id/items/:itemId/move`; malformed anchors reach handler validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveItemBody {
    pub after_item_id: Option<String>,
}

/// Answer of `POST /playlists/:id/items/:itemId/move`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveItemResponse {
    pub success: bool,
    pub rank: String,
}

/// Compatibility name for the add-items request.
pub type AddItemBody = AddItems;
