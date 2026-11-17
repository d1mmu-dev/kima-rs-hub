//! Synthetic wire examples, not captured runtime/model fixtures.
use kima_api_types::vibe::{Computing, MapResponse, MoodSummary, TrackMood};
use serde_json::{Value, json};

#[test]
fn matching_and_path_dtos_match_reference_typescript_fields() {
    use kima_api_types::vibe::{
        AlchemyResponse, MatchTrack, PathResponse, PathTrack, SimilarResponse,
    };
    use std::collections::BTreeSet;
    let ts = include_str!("../../../frontend/lib/api.ts");
    let keys = |name: &str| -> BTreeSet<String> {
        ts.split(&format!("interface {name} {{"))
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("    "))
            .filter_map(|line| {
                line.split_once(':')
                    .map(|(key, _)| key.trim_end_matches('?').to_owned())
            })
            .filter(|key| key != "trackNo")
            .collect()
    };
    let track = json!({"id":"track","title":"title","displayTitle":null,"duration":123,"distance":0.25,"similarity":0.75,"album":{"id":"album","title":"album","coverUrl":null},"artist":{"id":"artist","name":"artist"}});
    let flat = json!({"id":"track","title":"title","displayTitle":"edit","duration":123,"albumId":"album","albumTitle":"album","albumCoverUrl":null,"artistId":"artist","artistName":"artist"});
    assert_eq!(
        track
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        keys("VibeTrack")
    );
    assert_eq!(
        flat.as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        keys("VibePathTrack")
    );
    roundtrip::<MatchTrack>(track.clone());
    roundtrip::<PathTrack>(flat.clone());
    roundtrip::<SimilarResponse>(json!({"sourceTrackId":"source","tracks":[track.clone()]}));
    roundtrip::<AlchemyResponse>(json!({"tracks":[track]}));
    roundtrip::<PathResponse>(
        json!({"startTrack":flat,"endTrack":flat,"path":[flat],"metadata":{"totalTracks":3,"embeddingDistance":0.5,"averageStepSize":0.1,"mode":"smooth"}}),
    );
    for field in [
        "sourceTrackId: string; tracks: VibeTrack[]",
        "tracks: VibeTrack[]",
        "startTrack: VibePathTrack",
        "endTrack: VibePathTrack",
        "path: Array<VibePathTrack>",
        "totalTracks: number; embeddingDistance: number; averageStepSize: number; mode: string",
    ] {
        assert!(ts.contains(field), "{field}");
    }
}

fn roundtrip<T: serde::de::DeserializeOwned + serde::Serialize>(value: Value) {
    let dto: T = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(dto).unwrap(), value);
}

#[test]
fn map_preserves_upstream_keys_and_nullable_readings() {
    roundtrip::<MapResponse>(json!({"tracks":[{
        "id":"synthetic-track","x":0.5,"y":0.25,"z":1.0,"colour":0.4,
        "title":"Synthetic","artist":"Artist","artistId":"artist","albumId":"album",
        "coverUrl":null,"dominantMood":"moodHappy","moodScore":0.9,
        "moods":{"moodHappy":0.9,"moodSad":0.1,"moodRelaxed":0.2,"moodAggressive":0.3,"moodParty":0.4},
        "energy":null,"valence":0.75}],"trackCount":1,"computedAt":"1970-01-01T00:00:00.000Z","moodSignature":"synthetic"}));
}

#[test]
fn mood_summary_detail_and_computing_have_no_personal_fields() {
    roundtrip::<MoodSummary>(
        json!({"shares":[{"key":"moodHappy","fraction":0.75},{"key":"neutral","fraction":0.25}],"albums":{"album":"moodHappy"},"artists":{"artist":"moodHappy"}}),
    );
    roundtrip::<TrackMood>(json!({"dominantMood":"unanalyzed"}));
    roundtrip::<Computing>(json!({"computing":true}));
}
