//! Round-trips the library / playback-state fixtures captured from a running kima-server.

use kima_api_types::library::{
    AlbumsResponse, ArtistsResponse, ScanStarted, ScanStatus, TracksResponse,
};
use kima_api_types::playback_state::{PlaybackState, SavePlaybackState};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn round_trip<T: DeserializeOwned + Serialize>(name: &str, fixture: &str) {
    let original: Value = serde_json::from_str(fixture).expect("fixture is JSON");
    let typed: T = serde_json::from_str(fixture)
        .unwrap_or_else(|e| panic!("{name}: fixture does not parse as the DTO: {e}"));
    let again: Value = serde_json::from_str(&serde_json::to_string(&typed).unwrap()).unwrap();
    assert_eq!(again, original, "{name}: DTO changed the wire shape");
}

#[test]
fn library_types_round_trip() {
    round_trip::<ArtistsResponse>(
        "artists",
        include_str!("fixtures/library_artists_response.json"),
    );
    round_trip::<AlbumsResponse>(
        "albums",
        include_str!("fixtures/library_albums_response.json"),
    );
    round_trip::<TracksResponse>(
        "tracks",
        include_str!("fixtures/library_tracks_response.json"),
    );
    round_trip::<ScanStarted>("scan", include_str!("fixtures/library_scan_response.json"));
    round_trip::<ScanStatus>(
        "scan_status",
        include_str!("fixtures/library_scan_status_response.json"),
    );
}

#[test]
fn playback_state_round_trips() {
    round_trip::<PlaybackState>(
        "playback_state",
        include_str!("fixtures/playback_state_response.json"),
    );
    round_trip::<PlaybackState>(
        "playback_state_save",
        include_str!("fixtures/playback_state_save_response.json"),
    );
    round_trip::<Option<PlaybackState>>(
        "playback_state_empty",
        include_str!("fixtures/playback_state_empty_response.json"),
    );
}

#[test]
fn save_request_omits_unset_fields() {
    let body = SavePlaybackState {
        playback_type: "track".into(),
        track_id: Some("t".into()),
        queue: None,
        current_index: Some(2),
        is_shuffle: None,
    };
    let v: Value = serde_json::to_value(&body).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"playbackType": "track", "trackId": "t", "currentIndex": 2})
    );
}
