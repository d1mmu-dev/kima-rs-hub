//! `GET /api/library/radio` (M5b Task 13; upstream `routes/library/tracks.ts` `GET /radio`): the radio's track rows,
//! upstream's keys in its order.

use serde::{Deserialize, Serialize};

/// One track of a radio refill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioTrack {
    pub id: String,
    pub title: String,
    pub display_title: Option<String>,
    pub duration: i32,
    pub track_no: i32,
    pub disc_number: i32,
    /// kima keeps no disc subtitle: always `null`.
    pub disc_subtitle: Option<String>,
    pub file_path: String,
    pub artist: RadioArtist,
    pub album: RadioAlbum,
}

/// The track's album artist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioArtist {
    pub id: String,
    pub name: String,
}

/// The track's album; `coverArt` is the album's cover token (its id) when it has a cover image (D-265).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioAlbum {
    pub id: String,
    pub title: String,
    pub cover_art: Option<String>,
}

/// A refill: the tracks, `next` only for a station asked with a seed, `message` only for a retired station.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioReply {
    pub tracks: Vec<RadioTrack>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub next: Option<u64>,
}
