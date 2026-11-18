//! Synthetic wire examples, not captured runtime/model fixtures.
use kima_api_types::vibe::{Computing, MapResponse, MoodSummary, TrackMood};
use serde_json::{Value, json};

/// The reference TypeScript shapes, extracted once from upstream `frontend/lib/api.ts` (see the fixture's
/// `source`): the bundled `frontend/` copy is gone, so CI reads the checked-in extraction.
fn reference() -> Value {
    serde_json::from_str(include_str!("fixtures/vibe_reference_shapes.json")).unwrap()
}

fn reference_keys(reference: &Value, name: &str) -> std::collections::BTreeSet<String> {
    reference["interfaces"][name]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key.as_str().unwrap().to_owned())
        .collect()
}

/// Interface keys as the old in-repo test read them: four-space-indented `key:` lines, `?` dropped,
/// `trackNo` excluded (/vibe/similar and /vibe/alchemy never send it).
fn typescript_keys(ts: &str, name: &str) -> std::collections::BTreeSet<String> {
    ts.split(&format!("interface {name} {{"))
        .nth(1)
        .unwrap_or_else(|| panic!("interface {name} missing"))
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
}

#[test]
fn matching_and_path_dtos_match_reference_typescript_fields() {
    use kima_api_types::vibe::{
        AlchemyResponse, MatchTrack, PathResponse, PathTrack, SimilarResponse,
    };
    use std::collections::BTreeSet;
    let reference = reference();
    let track = json!({"id":"track","title":"title","displayTitle":null,"duration":123,"distance":0.25,"similarity":0.75,"album":{"id":"album","title":"album","coverUrl":null},"artist":{"id":"artist","name":"artist"}});
    let flat = json!({"id":"track","title":"title","displayTitle":"edit","duration":123,"albumId":"album","albumTitle":"album","albumCoverUrl":null,"artistId":"artist","artistName":"artist"});
    assert_eq!(
        track
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        reference_keys(&reference, "VibeTrack")
    );
    assert_eq!(
        flat.as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        reference_keys(&reference, "VibePathTrack")
    );
    roundtrip::<MatchTrack>(track.clone());
    roundtrip::<PathTrack>(flat.clone());
    roundtrip::<SimilarResponse>(json!({"sourceTrackId":"source","tracks":[track.clone()]}));
    roundtrip::<AlchemyResponse>(json!({"tracks":[track]}));
    roundtrip::<PathResponse>(
        json!({"startTrack":flat,"endTrack":flat,"path":[flat],"metadata":{"totalTracks":3,"embeddingDistance":0.5,"averageStepSize":0.1,"mode":"smooth"}}),
    );
}

/// Re-derives the fixture from an external, freshly pulled kima-hub checkout (never bundled in this
/// repository). Run: `KIMA_HUB_REFERENCE=<checkout> cargo test -p kima-api-types --test vibe -- --ignored`;
/// a mismatch means upstream moved: re-extract deliberately and record the drift.
#[test]
#[ignore = "needs KIMA_HUB_REFERENCE pointing at an external kima-hub checkout"]
fn reference_fixture_matches_external_checkout() {
    let root = std::env::var("KIMA_HUB_REFERENCE")
        .expect("set KIMA_HUB_REFERENCE to an external kima-hub checkout");
    let path = std::path::Path::new(&root).join("frontend/lib/api.ts");
    let ts = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let reference = reference();
    for name in ["VibeTrack", "VibePathTrack"] {
        assert_eq!(
            typescript_keys(&ts, name),
            reference_keys(&reference, name),
            "{name}"
        );
    }
    for field in reference["fields"].as_array().unwrap() {
        let field = field.as_str().unwrap();
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
