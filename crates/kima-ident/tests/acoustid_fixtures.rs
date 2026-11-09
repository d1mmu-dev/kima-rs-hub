//! M-SYNC's synthetic batch and album-vote examples; never a real AcoustID answer.
use serde_json::Value;
use std::path::Path;

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/acoustid")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn synthetic_batch_has_corresponding_request_and_replay_entries() {
    let batch = fixture("batch-3.json");
    assert_eq!(batch["request"]["meta"], "recordings releasegroups");
    assert_eq!(batch["request"]["batch"], 1);
    let inputs = batch["request"]["inputs"].as_array().unwrap();
    let outputs = batch["reply"]["fingerprints"].as_array().unwrap();
    assert_eq!(inputs.len(), 3);
    assert_eq!(outputs.len(), 3);
    for (i, (question, answer)) in inputs.iter().zip(outputs).enumerate() {
        assert_eq!(question["index"], i);
        assert_eq!(answer["index"], i);
        assert!(question["duration"].as_i64().unwrap() > 0);
        assert!(
            question["fingerprint"]
                .as_str()
                .unwrap()
                .starts_with("SYNTHETIC_")
        );
        assert!(answer["results"].is_array());
        let results: Vec<kima_ident::acoustid::AcoustidResult> =
            serde_json::from_value(answer["results"].clone()).unwrap();
        if i < 2 {
            let picked = kima_ident::acoustid::pick_fingerprint_match(
                &results,
                question["duration"].as_i64().unwrap() as i32,
                None,
            )
            .unwrap();
            assert!(!picked.title_agrees);
            assert_eq!(picked.release_groups[0].title, "Fixture Album");
        } else {
            assert!(results.is_empty());
        }
    }
    let requests = fixture("requests.json");
    assert_eq!(requests[0]["host"], "acoustid");
    assert_eq!(requests[0]["path"], "/lookup");
    assert!(requests[0]["body"].as_str().unwrap().contains("batch=1"));
}

#[test]
fn synthetic_album_votes_have_three_distinct_tracks_and_a_majority() {
    let vote = fixture("album-vote.json");
    let tracks = vote["songs"].as_array().unwrap();
    assert_eq!(tracks.len(), 3);
    let groups: Vec<&str> = tracks
        .iter()
        .map(|s| s["releasegroups"][0]["id"].as_str().unwrap())
        .collect();
    assert_eq!(groups[0], groups[1]);
    assert_ne!(groups[1], groups[2]);
    assert_eq!(fixture("requests.json")[1]["host"], "acoustid");
}
