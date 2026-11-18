//! The vibe calls of the `/vibe` page and the player's "match this vibe" (upstream `lib/api.ts`
//! `getVibeMap`, `getVibeSimilarTracks`, `vibeSearch`, `getTrack`), plus `/system/features`.
//!
//! Paths, bodies and the conversions into player queue items are pure functions with native tests;
//! the async wrappers only send them through [`ApiClient`].

use kima_api_types::playback_state::{QueueAlbum, QueueArtist, QueueItem};
use kima_api_types::vibe::{Computing, MapResponse, MatchTrack, SearchResponse, SimilarResponse};
use serde::{Deserialize, Serialize};

use super::client::{ApiClient, ApiError};

/// Matches asked for by "Similar" and "Match Vibe" (upstream asks for 50 in both places).
pub const SIMILAR_LIMIT: u32 = 50;
/// Tracks asked for by a sound search (upstream `useTrackFinder` default).
pub const SEARCH_LIMIT: u32 = 10;

/// The part of `GET /system/features` kima-web reads. Unknown keys are ignored; a missing
/// `vibeEmbeddings` reads as off, like upstream's `useFeatures` default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Features {
    #[serde(rename = "vibeEmbeddings", default)]
    pub vibe_embeddings: bool,
}

/// `GET /vibe/map`: 200 with the map, or 202 `{computing: true}` while the first layout is built.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum MapState {
    Ready(MapResponse),
    Computing(Computing),
}

/// Body of `POST /vibe/search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: u32,
}

/// `GET /library/tracks/:id`: the fields a queue item needs (the detail answer has more).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackDetail {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub duration: f64,
    pub artist: QueueArtist,
    pub album: QueueAlbum,
}

pub fn map_path() -> &'static str {
    "/api/vibe/map"
}

pub fn similar_path(track_id: &str, limit: u32) -> String {
    format!("/api/vibe/similar/{track_id}?limit={limit}")
}

pub fn track_path(track_id: &str) -> String {
    format!("/api/library/tracks/{track_id}")
}

/// The search body, or `None` when the query is shorter than upstream's two characters.
pub fn search_request(typed: &str, limit: u32) -> Option<SearchRequest> {
    let query = typed.trim();
    (query.chars().count() >= 2).then(|| SearchRequest {
        query: query.to_owned(),
        limit,
    })
}

fn item(
    id: &str,
    title: &str,
    duration: i32,
    artist: (&str, &str),
    album: (&str, &str, &Option<String>),
) -> QueueItem {
    QueueItem {
        id: id.to_owned(),
        title: title.to_owned(),
        duration: f64::from(duration),
        artist: Some(QueueArtist {
            id: artist.0.to_owned(),
            name: artist.1.to_owned(),
        }),
        album: Some(QueueAlbum {
            id: album.0.to_owned(),
            title: album.1.to_owned(),
            cover_art: album.2.clone(),
        }),
    }
}

/// A similar-track answer as a player queue item (upstream `toQueueTrack`).
pub fn match_item(t: &MatchTrack) -> QueueItem {
    item(
        &t.id,
        &t.title,
        t.duration,
        (&t.artist.id, &t.artist.name),
        (&t.album.id, &t.album.title, &t.album.cover_url),
    )
}

pub fn detail_item(t: TrackDetail) -> QueueItem {
    QueueItem {
        id: t.id,
        title: t.title,
        duration: t.duration,
        artist: Some(t.artist),
        album: Some(t.album),
    }
}

/// "Match Vibe": the source first, then its matches (upstream `startVibeMode` / `runTrackOperation`).
pub fn vibe_queue(source: QueueItem, matches: &[MatchTrack]) -> Vec<QueueItem> {
    let mut queue = Vec::with_capacity(matches.len() + 1);
    let source_id = source.id.clone();
    queue.push(source);
    queue.extend(matches.iter().filter(|m| m.id != source_id).map(match_item));
    queue
}

pub async fn features(client: &ApiClient) -> Result<Features, ApiError> {
    client.get("/api/system/features").await
}

pub async fn map(client: &ApiClient) -> Result<MapState, ApiError> {
    client.get(map_path()).await
}

pub async fn similar(client: &ApiClient, track_id: &str) -> Result<SimilarResponse, ApiError> {
    client.get(&similar_path(track_id, SIMILAR_LIMIT)).await
}

pub async fn search(client: &ApiClient, body: &SearchRequest) -> Result<SearchResponse, ApiError> {
    client.post("/api/vibe/search", body).await
}

pub async fn track(client: &ApiClient, track_id: &str) -> Result<QueueItem, ApiError> {
    client
        .get::<TrackDetail>(&track_path(track_id))
        .await
        .map(detail_item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::decode_response;
    use crate::api::codec;
    use serde_json::json;

    const ID: &str = "01a1aad3-f0c4-710b-b701-36f9f6b6972e";

    fn js(v: serde_json::Value) -> vertigo::JsJson {
        codec::value_to_js(v)
    }

    fn match_json(id: &str) -> serde_json::Value {
        json!({
            "id": id, "title": "Song", "displayTitle": null, "duration": 241,
            "distance": 0.2, "similarity": 0.8,
            "album": {"id": "al", "title": "Album", "coverUrl": "al"},
            "artist": {"id": "ar", "name": "Artist"}
        })
    }

    #[test]
    fn paths_carry_the_id_and_limit() {
        assert_eq!(map_path(), "/api/vibe/map");
        assert_eq!(
            similar_path(ID, SIMILAR_LIMIT),
            format!("/api/vibe/similar/{ID}?limit=50")
        );
        assert_eq!(track_path(ID), format!("/api/library/tracks/{ID}"));
    }

    #[test]
    fn search_needs_two_characters_and_is_trimmed() {
        assert_eq!(search_request(" a ", 10), None);
        assert_eq!(search_request("", 10), None);
        assert_eq!(
            search_request("  rainy day ", 10),
            Some(SearchRequest {
                query: "rainy day".into(),
                limit: 10
            })
        );
        // Two characters, not two bytes.
        assert!(search_request("ż", 10).is_none());
        let body = serde_json::to_value(search_request("ab", 7).unwrap()).unwrap();
        assert_eq!(body, json!({"query": "ab", "limit": 7}));
    }

    #[test]
    fn map_202_decodes_as_computing_and_200_as_the_map() {
        let computing: MapState = decode_response(202, js(json!({"computing": true}))).unwrap();
        assert_eq!(
            computing,
            MapState::Computing(Computing { computing: true })
        );
        let ready: MapState = decode_response(
            200,
            js(json!({
                "tracks": [{
                    "id": ID, "x": 0.1, "y": 0.2, "z": 0.3, "colour": 0.5, "title": "T",
                    "artist": "A", "artistId": "ar", "albumId": "al", "coverUrl": null,
                    "dominantMood": "moodHappy", "moodScore": 0.7,
                    "moods": {"moodHappy": 0.7}, "energy": null, "valence": 0.4
                }],
                "trackCount": 1, "computedAt": "2026-11-18T00:00:00.000Z", "moodSignature": "s"
            })),
        )
        .unwrap();
        let MapState::Ready(map) = ready else {
            panic!("expected the map")
        };
        assert_eq!(map.track_count, 1);
        assert_eq!(map.tracks[0].dominant_mood, "moodHappy");
    }

    #[test]
    fn map_errors_surface_the_backend_message() {
        let err =
            decode_response::<MapState>(406, js(json!({"error": "No acceptable map encoding"})))
                .unwrap_err();
        assert_eq!(err.message, "No acceptable map encoding");
    }

    #[test]
    fn features_read_vibe_embeddings_and_default_to_off() {
        let on: Features = decode_response(
            200,
            js(json!({"musicCNN": false, "vibeEmbeddings": true, "library": true})),
        )
        .unwrap();
        assert!(on.vibe_embeddings);
        let missing: Features = decode_response(200, js(json!({"library": true}))).unwrap();
        assert!(!missing.vibe_embeddings);
    }

    #[test]
    fn similar_answer_becomes_queue_items() {
        let answer: SimilarResponse = decode_response(
            200,
            js(json!({"sourceTrackId": ID, "tracks": [match_json("m1")]})),
        )
        .unwrap();
        let item = match_item(&answer.tracks[0]);
        assert_eq!(item.id, "m1");
        assert_eq!(item.title, "Song");
        assert_eq!(item.duration, 241.0);
        assert_eq!(item.artist.as_ref().unwrap().name, "Artist");
        assert_eq!(item.artist.as_ref().unwrap().id, "ar");
        let album = item.album.unwrap();
        assert_eq!((album.id.as_str(), album.title.as_str()), ("al", "Album"));
        assert_eq!(album.cover_art.as_deref(), Some("al"));
    }

    #[test]
    fn search_answer_decodes_with_its_track_numbers() {
        let mut t = match_json("s1");
        t["trackNo"] = json!(3);
        let answer: SearchResponse =
            decode_response(200, js(json!({"query": "q", "tracks": [t]}))).unwrap();
        assert_eq!(answer.tracks[0].id, "s1");
        assert_eq!(answer.tracks[0].artist.name, "Artist");
    }

    #[test]
    fn track_detail_becomes_a_queue_item() {
        let detail: TrackDetail = decode_response(
            200,
            js(json!({
                "id": ID, "title": "Shown title", "duration": 200,
                "artist": {"id": "ar", "name": "Artist"},
                "album": {"id": "al", "title": "Album", "coverArt": null},
                "audioFeatures": null, "genres": []
            })),
        )
        .unwrap();
        let item = detail_item(detail);
        assert_eq!(item.id, ID);
        assert_eq!(item.title, "Shown title");
        assert_eq!(item.duration, 200.0);
        assert_eq!(item.album.unwrap().cover_art, None);
    }

    #[test]
    fn vibe_queue_starts_with_the_source_and_skips_it_among_matches() {
        let source = match_item(&serde_json::from_value::<MatchTrack>(match_json("src")).unwrap());
        let matches: Vec<MatchTrack> = ["a", "src", "b"]
            .iter()
            .map(|id| serde_json::from_value(match_json(id)).unwrap())
            .collect();
        let ids: Vec<String> = vibe_queue(source, &matches)
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, ["src", "a", "b"]);
    }
}
