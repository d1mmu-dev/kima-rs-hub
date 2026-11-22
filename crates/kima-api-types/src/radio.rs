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

/// `GET /api/library/radio/row`: a page's stations, each with this listener's cover for the local day (upstream
/// `routes/library/tracks.ts` `GET /radio/row`, `services/mixes/radioRow.ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioRowReply {
    pub stations: Vec<RowStation>,
}

/// One station of the row. `type` is `all`/`workout`/`discovery`/`favorites` (no value), `mood` (value, name,
/// description) or `genre`/`decade` (value, count); `cover` is a cover token or `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowStation {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub count: Option<i64>,
    pub cover: Option<String>,
}

/// `GET /api/library/radio/stations`: the mood stations and Wildcard with their generated subtitles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoodStationsReply {
    pub stations: Vec<MoodStation>,
}

/// One mood station of [`MoodStationsReply`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoodStation {
    pub value: String,
    pub name: String,
    pub description: String,
}
