//! The M5b wire fixtures (plan `2026-11-19-m5b-mixes-radio.md`, Task 19): all 22 mixes, radio, decades, what's-new
//! and recommendations routes, captured by `tools/capture-api-fixtures.sh` (`KIMA_CAPTURE_SET=m5b`, see
//! `tools/capture-m5b-fixtures.inc.sh`) from the shipped server (`live`: no mood heads, empty catalog, network denied)
//! and from the same router with synthetic mood heads and a one-release catalog (`heads`). Checked against the DTOs
//! and against upstream's backend shapes at the M5b reference `9450c94b` (`fixtures/m5b_upstream_keys.json`,
//! re-checked against the upstream sources by `tools/check-m5b-upstream-keys.py`); the mix track rows against the full
//! Prisma `Track`/`Album` inventory (owner Q6 = B, Appendix D, D-264).
//!
//! The fixtures are committed as captured (never hand-written; recapture, do not edit).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kima_api_types::mixes::MixTrack;
use kima_api_types::radio::RadioReply;
use kima_api_types::recommendations::ForYouReply;
use kima_api_types::releases::{
    ChangelogReply, ChoiceReply, CurrentReply, DismissReply, MoodMixDecisionReply,
    MoodMixSaveReply, MoodMixStateReply, OpenReply, PendingReply, UpdateStatus,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Every captured fixture (`m5b_<name>_response.json`), in capture order; equal to the script's `m5b_names`.
const FIXTURES: [&str; 45] = [
    "mixes",
    "mix",
    "mix_save",
    "mixes_refresh",
    "presets",
    "bucket_no_heads",
    "bucket_save",
    "mixes_with_mood",
    "decades",
    "radio_stations",
    "radio_row",
    "radio_row_all",
    "radio_all",
    "radio_genre",
    "radio_decade",
    "radio_artist",
    "radio_discovery",
    "radio_favorites",
    "radio_mood_no_heads",
    "whats_new_pending",
    "whats_new_current_none",
    "whats_new_changelog",
    "whats_new_open",
    "update_status",
    "mood_mixes",
    "mood_mixes_decision",
    "mood_mixes_save",
    "mood_mixes_saved",
    "choice_unknown_release",
    "dismiss_unknown_release",
    "recommendations",
    "heads_mixes",
    "heads_mix",
    "heads_presets",
    "heads_bucket",
    "heads_bucket_save",
    "heads_radio_row_all",
    "heads_radio_mood",
    "heads_radio_workout",
    "heads_whats_new_pending",
    "heads_whats_new_current",
    "heads_whats_new_choice",
    "heads_whats_new_changelog",
    "heads_whats_new_open",
    "heads_whats_new_dismiss",
];

/// The 22 upstream route registrations (plan inventory), as the capture manifest names them; each must have
/// answered 2xx at least once.
const UPSTREAM_ROUTES: [&str; 22] = [
    "GET /mixes",
    "GET /mixes/mood/buckets/presets",
    "GET /mixes/mood/buckets/:mood",
    "POST /mixes/mood/buckets/:mood/save",
    "POST /mixes/refresh",
    "POST /mixes/:id/save",
    "GET /mixes/:id",
    "GET /whats-new/pending",
    "GET /whats-new/update-status",
    "GET /whats-new/current",
    "GET /whats-new/changelog",
    "GET /whats-new/open",
    "GET /whats-new/mood-mixes",
    "POST /whats-new/mood-mixes/decision",
    "POST /whats-new/mood-mixes/save",
    "POST /whats-new/:version/choice",
    "POST /whats-new/:version/dismiss",
    "GET /recommendations/for-you",
    "GET /library/decades",
    "GET /library/radio/stations",
    "GET /library/radio/row",
    "GET /library/radio",
];

/// The real non-2xx answers captured on purpose: the page that needs the heads, and the release routes with
/// kima-rs's empty catalog (D-267).
const ERROR_FIXTURES: [(&str, u64); 4] = [
    ("bucket_no_heads", 400),
    ("whats_new_current_none", 404),
    ("choice_unknown_release", 404),
    ("dismiss_unknown_release", 404),
];

/// Appendix D of the M5b plan: upstream's Prisma `Track` scalar keys (owner Q6 = B, D-264).
const TRACK_KEYS: [&str; 58] = [
    "id",
    "albumId",
    "title",
    "trackNo",
    "duration",
    "mime",
    "filePath",
    "fileModified",
    "fileSize",
    "bpm",
    "bpmConfidence",
    "beatsCount",
    "key",
    "keyScale",
    "keyStrength",
    "energy",
    "loudness",
    "dynamicRange",
    "danceability",
    "valence",
    "arousal",
    "deamValence",
    "deamArousal",
    "clapValenceRaw",
    "clapArousalRaw",
    "instrumentalness",
    "acousticness",
    "speechiness",
    "danceabilityMl",
    "moodTags",
    "essentiaGenres",
    "analysisStatus",
    "analysisVersion",
    "analysisMode",
    "analyzedAt",
    "analysisError",
    "analysisRetryCount",
    "updatedAt",
    "displayTitle",
    "displayTrackNo",
    "hasUserOverrides",
    "analysisStartedAt",
    "vibeAnalysisStatus",
    "vibeAnalysisStartedAt",
    "vibeAnalysisError",
    "vibeAnalysisRetryCount",
    "vibeAnalysisStatusUpdatedAt",
    "isrc",
    "isrcSource",
    "acoustidFingerprint",
    "recordingMbid",
    "corrupt",
    "missingSince",
    "scanStatus",
    "scanError",
    "scanStartedAt",
    "discNumber",
    "discSubtitle",
];

/// Upstream's Prisma `Album` scalar keys.
const ALBUM_KEYS: [&str; 20] = [
    "id",
    "rgMbid",
    "rgMbidSource",
    "artistId",
    "title",
    "identityKey",
    "sortName",
    "year",
    "coverUrl",
    "primaryType",
    "label",
    "genres",
    "lastSynced",
    "location",
    "displayTitle",
    "displayYear",
    "hasUserOverrides",
    "userCoverUrl",
    "userGenres",
    "originalYear",
];

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn keys() -> Value {
    serde_json::from_str(&read("tests/fixtures/m5b_upstream_keys.json")).expect("key file is JSON")
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(&read(&format!("tests/fixtures/m5b_{name}_response.json")))
        .unwrap_or_else(|e| panic!("{name}: not JSON: {e}"))
}

fn manifest() -> Value {
    serde_json::from_str(&read("tests/fixtures/m5b_capture_manifest.json"))
        .expect("manifest is JSON")
}

/// The key-file entry for `name`, following `sameShapeAs`.
fn entry<'a>(keys: &'a Value, name: &str) -> &'a Value {
    let e = keys
        .get(name)
        .unwrap_or_else(|| panic!("{name}: no entry in m5b_upstream_keys.json"));
    match e.get("sameShapeAs").and_then(Value::as_str) {
        Some(other) => entry(keys, other),
        None => e,
    }
}

// ---------------------------------------------------------------------------------------------
// Shape comparison (as tests/podcasts.rs).

struct Shape {
    types: BTreeMap<String, (BTreeSet<String>, bool)>,
    kima: BTreeSet<String>,
}

fn parse_type(ty: &str) -> (BTreeSet<String>, bool) {
    let (ty, optional) = match ty.strip_suffix('?') {
        Some(t) => (t, true),
        None => (ty, false),
    };
    (ty.split('|').map(str::to_owned).collect(), optional)
}

impl Shape {
    fn of(entry: &Value) -> Self {
        let mut types: BTreeMap<_, _> = entry["upstream"]
            .as_object()
            .expect("upstream is an object")
            .iter()
            .map(|(path, ty)| (path.clone(), parse_type(ty.as_str().expect("type is text"))))
            .collect();
        let mut kima = BTreeSet::new();
        if let Some(extra) = entry.get("kimaOnly").and_then(Value::as_object) {
            for (path, spec) in extra {
                let (allowed, _) = parse_type(spec["type"].as_str().expect("kimaOnly type"));
                types.insert(path.clone(), (allowed, true));
                kima.insert(path.clone());
            }
        }
        Shape { types, kima }
    }

    fn is_record(&self, path: &str) -> bool {
        self.types.contains_key(&format!("{path}{{}}"))
    }
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn walk(
    shape: &Shape,
    v: &Value,
    path: String,
    seen: &mut BTreeMap<String, BTreeSet<&'static str>>,
    hidden: &mut Vec<String>,
) {
    seen.entry(path.clone()).or_default().insert(json_type(v));
    match v {
        Value::Null => hidden.push(path),
        Value::Array(items) => {
            if items.is_empty() {
                hidden.push(path.clone());
            }
            for item in items {
                walk(shape, item, format!("{path}[]"), seen, hidden);
            }
        }
        Value::Object(members) if shape.is_record(&path) => {
            if members.is_empty() {
                hidden.push(path.clone());
            }
            for item in members.values() {
                walk(shape, item, format!("{path}{{}}"), seen, hidden);
            }
        }
        Value::Object(members) => {
            for (key, item) in members {
                walk(shape, item, format!("{path}.{key}"), seen, hidden);
            }
        }
        _ => {}
    }
}

fn below(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with(['.', '[', '{']))
}

/// Mismatches between one fixture and its shape: an unlisted key, a wrong type, or an upstream key the fixture lacks
/// (unless it sits below an empty array, a null or an absent optional key).
fn compare(name: &str, shape: &Shape, value: &Value) -> Vec<String> {
    let mut seen = BTreeMap::new();
    let mut hidden = Vec::new();
    walk(shape, value, String::new(), &mut seen, &mut hidden);
    let mut problems = Vec::new();
    for (path, types) in &seen {
        match shape.types.get(path) {
            None => problems.push(format!(
                "{name}: `{path}` is neither an upstream nor a listed kima-only key"
            )),
            Some((allowed, _)) => {
                for ty in types {
                    if !allowed.contains(*ty) {
                        problems.push(format!("{name}: `{path}` is {ty}, allowed {allowed:?}"));
                    }
                }
            }
        }
    }
    let absent_optional: Vec<&String> = shape
        .types
        .iter()
        .filter(|(path, (_, optional))| *optional && !seen.contains_key(*path))
        .map(|(path, _)| path)
        .collect();
    for path in shape.types.keys() {
        if seen.contains_key(path) || shape.kima.contains(path) {
            continue;
        }
        let excused = absent_optional.contains(&path)
            || absent_optional.iter().any(|p| below(path, p))
            || hidden.iter().any(|p| below(path, p));
        if !excused {
            problems.push(format!("{name}: upstream key `{path}` is missing"));
        }
    }
    problems
}

fn integral_floats(v: &Value, path: String, out: &mut Vec<String>) {
    match v {
        Value::Number(n) if n.is_f64() && n.as_f64().is_some_and(|f| f.fract() == 0.0) => {
            out.push(format!("{path} = {n}"));
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                integral_floats(item, format!("{path}[{i}]"), out);
            }
        }
        Value::Object(members) => {
            for (key, item) in members {
                integral_floats(item, format!("{path}.{key}"), out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Without captured fixtures.

/// The key file names every fixture and nothing else, its kima-only keys cite existing DIVERGENCES rows, and
/// `sameShapeAs` points at real entries.
#[test]
fn key_file_lists_every_fixture_and_justifies_kima_extras() {
    let keys = keys();
    let rows: BTreeSet<String> = read("../../docs/DIVERGENCES.md")
        .lines()
        .filter_map(|l| l.strip_prefix("| D-"))
        .filter_map(|l| l.split(' ').next().map(|n| format!("D-{n}")))
        .collect();
    for name in FIXTURES {
        let e = entry(&keys, name);
        assert!(e["upstream"].is_object(), "{name}: no upstream shape");
        if let Some(extra) = e.get("kimaOnly").and_then(Value::as_object) {
            for (path, spec) in extra {
                let row = spec["divergence"].as_str().unwrap_or_default();
                assert!(
                    rows.contains(row),
                    "{name}: `{path}` cites missing row {row:?}"
                );
            }
        }
    }
    let listed: BTreeSet<&str> = keys
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| !k.starts_with('_'))
        .map(String::as_str)
        .collect();
    assert_eq!(
        listed,
        FIXTURES.into_iter().collect(),
        "key file and FIXTURES disagree"
    );
}

/// The capture script copies exactly these fixtures (its `m5b_names`, in this order).
#[test]
fn capture_script_names_every_fixture() {
    let script = read("../../tools/capture-api-fixtures.sh");
    let start = script
        .find("m5b_names=(")
        .expect("m5b_names in the capture script")
        + "m5b_names=(".len();
    let end = start + script[start..].find(')').unwrap();
    let names: Vec<&str> = script[start..end].split_whitespace().collect();
    assert_eq!(names, FIXTURES);
}

/// The mix track rows in the key file are the whole Appendix D inventory: every Prisma `Track` and `Album` scalar
/// column (owner Q6 = B), and the album's `artist` as upstream selects it.
#[test]
fn track_rows_are_the_full_appendix_d_inventory() {
    let keys = keys();
    for (name, rows) in [
        ("mix", ".tracks[]"),
        ("heads_bucket", ".tracks[]"),
        ("bucket_save", ".mix.tracks[]"),
    ] {
        let upstream = entry(&keys, name)["upstream"].as_object().unwrap();
        let direct = |prefix: &str| -> BTreeSet<String> {
            upstream
                .keys()
                .filter_map(|p| p.strip_prefix(prefix)?.strip_prefix('.'))
                .filter(|rest| !rest.contains(['.', '[', '{']))
                .map(str::to_owned)
                .collect()
        };
        let mut track: BTreeSet<String> = TRACK_KEYS.iter().map(|k| k.to_string()).collect();
        track.insert("album".into());
        let mut album: BTreeSet<String> = ALBUM_KEYS.iter().map(|k| k.to_string()).collect();
        album.insert("artist".into());
        assert_eq!(direct(rows), track, "{name}: track keys");
        assert_eq!(
            direct(&format!("{rows}.album")),
            album,
            "{name}: album keys"
        );
        assert_eq!(
            direct(&format!("{rows}.album.artist")),
            ["id", "mbid", "name"].map(str::to_owned).into(),
            "{name}: artist keys"
        );
    }
}

/// The comparison catches a renamed key, a wrong type, a missing key and an unlisted extra; a null hides its nested
/// shape only as unobservable.
#[test]
fn key_comparison_catches_drift_and_unlisted_extras() {
    let shape = Shape::of(&json!({
        "upstream": {"": "object", ".a": "number|null", ".l": "array", ".l[]": "object",
            ".l[].x": "string", ".s": "object|null", ".s.v": "number", ".r": "object", ".r{}": "boolean"},
        "kimaOnly": {}
    }));
    let ok = json!({"a": null, "l": [{"x": "1"}], "s": null, "r": {"k": true}});
    assert!(
        compare("ok", &shape, &ok).is_empty(),
        "{:?}",
        compare("ok", &shape, &ok)
    );
    let bad = json!({"a": "1", "l": [{"y": "z"}], "s": {}, "r": {"k": 1}});
    let problems = compare("bad", &shape, &bad);
    for needle in [
        "`.a` is string",
        "`.l[].y` is neither",
        "upstream key `.l[].x` is missing",
        "upstream key `.s.v` is missing",
        "`.r{}` is number",
    ] {
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "{needle} not reported in {problems:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Captured fixtures.

#[test]
fn manifest_answers_every_upstream_route() {
    let m = manifest();
    assert!(m["source"]["head"].as_str().is_some_and(|h| h.len() == 40));
    let captures = m["captures"].as_array().unwrap();
    let names: Vec<&str> = captures
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, FIXTURES, "manifest and FIXTURES disagree");
    let ok: BTreeSet<&str> = captures
        .iter()
        .filter(|c| c["status"].as_u64().unwrap() < 300)
        .map(|c| c["route"].as_str().unwrap())
        .collect();
    assert_eq!(
        ok,
        UPSTREAM_ROUTES.into_iter().collect(),
        "routes answered 2xx"
    );
    for c in captures {
        let name = c["name"].as_str().unwrap();
        let want = ERROR_FIXTURES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| *s);
        match want {
            Some(s) => assert_eq!(c["status"], s, "{name}"),
            None => assert!(c["status"].as_u64().unwrap() < 300, "{name}: {c}"),
        }
        let variant = c["variant"].as_str().unwrap();
        assert_eq!(
            variant == "heads",
            name.starts_with("heads_"),
            "{name}: variant {variant}"
        );
    }
}

#[test]
fn m5b_fixtures_cover_upstream_keys() {
    let keys = keys();
    let problems: Vec<String> = FIXTURES
        .iter()
        .flat_map(|name| compare(name, &Shape::of(entry(&keys, name)), &fixture(name)))
        .collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_track_row_has_exactly_the_appendix_d_keys() {
    let key_set =
        |v: &Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    let mut track: BTreeSet<String> = TRACK_KEYS.iter().map(|k| k.to_string()).collect();
    track.insert("album".into());
    let mut album: BTreeSet<String> = ALBUM_KEYS.iter().map(|k| k.to_string()).collect();
    album.insert("artist".into());
    let mut rows = 0;
    for (name, at) in [
        ("mix", "/tracks"),
        ("heads_mix", "/tracks"),
        ("heads_bucket", "/tracks"),
        ("bucket_save", "/mix/tracks"),
        ("heads_bucket_save", "/mix/tracks"),
    ] {
        let f = fixture(name);
        let list = f.pointer(at).and_then(Value::as_array).unwrap();
        assert!(!list.is_empty(), "{name}: no track rows");
        for row in list {
            assert_eq!(key_set(row), track, "{name}");
            assert_eq!(key_set(&row["album"]), album, "{name}");
            assert_eq!(
                key_set(&row["album"]["artist"]),
                ["id", "mbid", "name"].map(str::to_owned).into(),
                "{name}"
            );
            let _: MixTrack = serde_json::from_value(row.clone())
                .unwrap_or_else(|e| panic!("{name}: a row does not parse as MixTrack: {e}"));
            rows += 1;
        }
    }
    assert!(rows >= 40, "{rows} rows");
}

fn decode<T: DeserializeOwned>(name: &str) -> T {
    serde_json::from_value(fixture(name))
        .unwrap_or_else(|e| panic!("{name}: fixture does not parse as the DTO: {e}"))
}

#[test]
fn m5b_types_parse_the_fixtures() {
    for name in [
        "radio_all",
        "radio_genre",
        "radio_decade",
        "radio_artist",
        "radio_discovery",
        "radio_favorites",
        "radio_mood_no_heads",
        "heads_radio_mood",
        "heads_radio_workout",
    ] {
        let _: RadioReply = decode(name);
    }
    let _: ForYouReply = decode("recommendations");
    let _: PendingReply = decode("whats_new_pending");
    let _: PendingReply = decode("heads_whats_new_pending");
    let _: CurrentReply = decode("heads_whats_new_current");
    let _: ChangelogReply = decode("whats_new_changelog");
    let _: ChangelogReply = decode("heads_whats_new_changelog");
    let _: OpenReply = decode("whats_new_open");
    let _: OpenReply = decode("heads_whats_new_open");
    let _: ChoiceReply = decode("heads_whats_new_choice");
    let _: DismissReply = decode("heads_whats_new_dismiss");
    let _: MoodMixStateReply = decode("mood_mixes");
    let _: MoodMixStateReply = decode("mood_mixes_saved");
    let _: MoodMixDecisionReply = decode("mood_mixes_decision");
    let _: MoodMixSaveReply = decode("mood_mixes_save");
    let _: UpdateStatus = decode("update_status");
}

/// The ids agree across replies, and the optional arrays and values the keys check could only call unobservable
/// are populated somewhere.
#[test]
fn cross_ids_and_populated_values() {
    let ids = |v: &Value| -> Vec<Value> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].clone())
            .collect()
    };
    // Mixes.
    let mixes = fixture("mixes");
    let mix = fixture("mix");
    assert!(
        mixes
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == mix["id"])
    );
    assert_eq!(mix["trackIds"].as_array().unwrap(), &ids(&mix["tracks"]));
    assert!(
        !mix["coverUrls"].as_array().unwrap().is_empty(),
        "covers (D-265)"
    );
    let saved = fixture("mix_save");
    assert_eq!(saved["trackCount"], mix["trackCount"]);
    assert_eq!(saved["name"], "Saved capture mix");
    assert_eq!(fixture("mixes_refresh")["message"], "Mixes refreshed");
    // The saved mood mix: on Home, in the what's-new question, saved as a playlist.
    let bucket = fixture("bucket_save");
    assert_eq!(bucket["success"], true);
    assert_eq!(
        bucket["mix"]["trackIds"].as_array().unwrap(),
        &ids(&bucket["mix"]["tracks"])
    );
    let home = fixture("mixes_with_mood");
    let on_home = home
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == bucket["mix"]["id"])
        .expect("the saved mood mix is on Home");
    assert_eq!(
        (on_home["type"].as_str(), on_home["mood"].as_str()),
        (Some("mood"), Some("chill"))
    );
    assert_eq!(fixture("mood_mixes")["mix"]["id"], bucket["mix"]["id"]);
    assert_eq!(fixture("mood_mixes_decision")["decision"], "keep");
    let as_playlist = fixture("mood_mixes_save");
    assert_eq!(as_playlist["created"], true);
    let after = fixture("mood_mixes_saved");
    assert_eq!(after["playlist"]["id"], as_playlist["id"]);
    assert_eq!(after["decision"], "keep");
    // Mood pages with the heads; without them, the real degraded answers.
    let page = fixture("heads_bucket");
    assert_eq!(page["trackIds"].as_array().unwrap(), &ids(&page["tracks"]));
    assert!(page["trackCount"].as_u64().unwrap() >= 8);
    assert!(
        fixture("heads_presets")
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["trackCount"].as_u64().unwrap() > 0)
    );
    assert!(
        fixture("presets")
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["trackCount"] == 0 && p["pendingTracks"] == 0)
    );
    assert_eq!(
        fixture("bucket_no_heads")["error"],
        "Not enough tracks for mood: chill"
    );
    assert!(
        fixture("radio_mood_no_heads")["tracks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !fixture("heads_radio_mood")["tracks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture("heads_bucket_save")["mix"]["trackCount"]
            .as_u64()
            .unwrap()
            >= 8
    );
    // Track rows: the seeded values reach the wire.
    let rows: Vec<Value> = ["mix", "heads_bucket", "bucket_save"]
        .iter()
        .flat_map(|n| {
            let f = fixture(n);
            let at = if *n == "bucket_save" {
                "/mix/tracks"
            } else {
                "/tracks"
            };
            f.pointer(at).unwrap().as_array().unwrap().clone()
        })
        .collect();
    for key in [
        "bpm",
        "bpmConfidence",
        "key",
        "keyScale",
        "valence",
        "arousal",
        "clapArousalRaw",
        "instrumentalness",
    ] {
        assert!(
            rows.iter().any(|r| !r[key].is_null()),
            "no row carries {key}"
        );
    }
    assert!(
        rows.iter()
            .any(|r| !r["moodTags"].as_array().unwrap().is_empty()),
        "moodTags"
    );
    assert!(
        rows.iter().any(|r| r["album"]["coverUrl"].is_string()),
        "album covers"
    );
    // Radio.
    let decades = fixture("decades");
    let decades = decades["decades"].as_array().unwrap();
    assert!(decades.len() >= 2 && decades.iter().all(|d| d["count"].as_u64().unwrap() >= 15));
    // Artist radio of "Nineties A" (capture seed): 4 of its own songs, then the 6 the genre
    // fallback draws from the artists sharing Rock (Nineties B-D). Exact for every draw.
    let by_artist = fixture("radio_artist");
    let tracks = by_artist["tracks"].as_array().unwrap();
    let own = tracks
        .iter()
        .filter(|t| t["artist"]["name"] == "Nineties A")
        .count();
    assert_eq!(tracks.len(), 10, "artist radio length");
    assert_eq!(own, 4, "artist radio: songs of the artist itself");
    assert!(
        tracks.iter().all(|t| matches!(
            t["artist"]["name"].as_str(),
            Some("Nineties A" | "Nineties B" | "Nineties C" | "Nineties D")
        )),
        "artist radio: only the artist and the artists sharing its genre"
    );
    for name in [
        "radio_all",
        "radio_genre",
        "radio_decade",
        "radio_artist",
        "radio_discovery",
        "radio_favorites",
    ] {
        assert!(
            !fixture(name)["tracks"].as_array().unwrap().is_empty(),
            "{name}"
        );
    }
    let row = fixture("radio_row_all");
    let stations = row["stations"].as_array().unwrap();
    assert!(
        stations
            .iter()
            .any(|s| s["type"] == "genre" && s["count"].as_u64().is_some())
    );
    assert!(
        stations.iter().any(|s| s["cover"].is_string()),
        "station covers"
    );
    assert_eq!(
        fixture("radio_stations")["stations"]
            .as_array()
            .unwrap()
            .len(),
        14
    );
    // What's new: the live catalog is empty (D-267); the harness catalog asks one choice.
    assert_eq!(fixture("whats_new_pending"), json!({"releases": []}));
    assert_eq!(
        fixture("whats_new_changelog"),
        json!({"releases": [], "openCount": 0})
    );
    assert_eq!(fixture("whats_new_open"), json!({"count": 0, "items": []}));
    assert_eq!(
        fixture("whats_new_current_none"),
        json!({"error": "No release"})
    );
    assert_eq!(
        fixture("choice_unknown_release"),
        json!({"error": "Unknown release"})
    );
    assert_eq!(
        fixture("dismiss_unknown_release"),
        json!({"error": "Unknown release"})
    );
    assert_eq!(
        fixture("heads_whats_new_current")["release"]["version"],
        "0.2.0"
    );
    assert_eq!(
        fixture("heads_whats_new_choice"),
        json!({"key": "capture.choice", "enabled": true, "decidedBy": "EXPLICIT"})
    );
    let changelog = fixture("heads_whats_new_changelog");
    let answered = changelog["releases"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "capture.choice")
        .unwrap();
    assert_eq!(
        answered["answer"],
        json!({"enabled": true, "decidedBy": "EXPLICIT"})
    );
    let dismissed = fixture("heads_whats_new_dismiss");
    assert_eq!(
        (&dismissed["version"], &dismissed["dismissed"]),
        (&json!("0.2.0"), &json!(true))
    );
    // The update check with the network denied: enabled, answered like a failed check (D-270).
    let update = fixture("update_status");
    assert_eq!(
        (&update["enabled"], &update["latest"], &update["hasUpdate"]),
        (&json!(true), &Value::Null, &json!(false))
    );
    assert!(update["lastCheckedAt"].is_string());
    // Recommendations: the similar artists kima holds no songs of.
    let recs = fixture("recommendations");
    let names: Vec<&str> = recs["artists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Faraway Band", "Nowhere Collective"]);
    assert_eq!(recs["artists"][0]["albumCount"], 1);
}

/// Upstream sends a whole-valued Prisma `Float` (a song at 80 bpm) as the JavaScript number `80`; serde's `f64`
/// would write `80.0`. Re-serializing the captured rows through `MixTrack` must not produce an integral float.
#[test]
fn mix_track_floats_serialize_like_javascript_numbers() {
    let mut found = Vec::new();
    for (name, at) in [
        ("mix", "/tracks"),
        ("heads_mix", "/tracks"),
        ("heads_bucket", "/tracks"),
        ("bucket_save", "/mix/tracks"),
        ("heads_bucket_save", "/mix/tracks"),
    ] {
        let rows = fixture(name).pointer(at).unwrap().clone();
        let tracks: Vec<MixTrack> = serde_json::from_value(rows).unwrap();
        assert!(!tracks.is_empty(), "{name}: no track rows");
        let wire: Value = serde_json::from_str(&serde_json::to_string(&tracks).unwrap()).unwrap();
        integral_floats(&wire, format!("{name}{at}"), &mut found);
    }
    assert!(found.is_empty(), "integral floats on the wire: {found:?}");
}

#[test]
fn no_integral_floats_and_no_tokens_on_the_wire() {
    let mut found = Vec::new();
    for name in FIXTURES {
        integral_floats(&fixture(name), name.to_owned(), &mut found);
        let text = read(&format!("tests/fixtures/m5b_{name}_response.json"));
        assert!(!text.contains("eyJ"), "{name}: a token-like string");
    }
    assert!(!read("tests/fixtures/m5b_capture_manifest.json").contains("eyJ"));
    assert!(found.is_empty(), "integral floats on the wire: {found:?}");
}
