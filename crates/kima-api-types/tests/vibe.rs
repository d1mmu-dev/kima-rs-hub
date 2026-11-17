//! Synthetic wire examples, not captured runtime/model fixtures.
use kima_api_types::vibe::{Computing, MapResponse, MoodSummary, TrackMood};
use serde_json::{Value, json};

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
