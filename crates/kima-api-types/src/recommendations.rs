//! `GET /api/recommendations/for-you` (M5b Task 18; upstream `routes/recommendations.ts`).

use serde::{Deserialize, Serialize};

/// One recommended artist, upstream's keys in its order (the artist row's `id, mbid, name, heroUrl`, then `coverArt`,
/// `albumCount`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendedArtist {
    /// The kima artist's id; an artist kima knows only by name has its name here (as the artist page's similar
    /// artists, D-113).
    pub id: String,
    pub mbid: Option<String>,
    pub name: String,
    /// kima keeps no hero URL: always `null` (its covers are tokens, `coverArt`).
    pub hero_url: Option<String>,
    /// The artist's cover token (its id) when it has a stored image, else `null`.
    pub cover_art: Option<String>,
    pub album_count: i64,
}

/// `GET /api/recommendations/for-you`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForYouReply {
    pub artists: Vec<RecommendedArtist>,
}
