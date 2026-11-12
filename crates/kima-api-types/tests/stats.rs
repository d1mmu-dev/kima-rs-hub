//! The stats wire fixtures, captured from a running kima-server by
//! `tools/capture-api-fixtures.sh`, against the DTOs in `kima_api_types::stats` and
//! against upstream v1.12.3's own TypeScript shapes (`fixtures/stats_upstream_keys.json`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kima_api_types::stats::{
    FalseStartAudit, HourDetail, Ignored, RegionDetail, Removed, RemovedFrom, Restored,
    StatsResponse, TrackPlays, TracksResponse,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Every captured stats fixture, by the name used in the upstream key file.
const FIXTURES: [&str; 10] = [
    "stats_response_s1",
    "stats_hour",
    "stats_tracks",
    "stats_track_plays",
    "stats_region",
    "stats_false_starts",
    "stats_collapse",
    "stats_remove",
    "stats_ignore",
    "stats_restore",
];

fn read(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn fixture_text(name: &str) -> String {
    read(&format!("{name}_response.json"))
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(&fixture_text(name)).unwrap_or_else(|e| panic!("{name}: not JSON: {e}"))
}

fn round_trip<T: DeserializeOwned + Serialize>(name: &str) {
    let text = fixture_text(name);
    let original: Value = serde_json::from_str(&text).expect("fixture is JSON");
    let typed: T = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{name}: fixture does not parse as the DTO: {e}"));
    let again: Value = serde_json::from_str(&serde_json::to_string(&typed).unwrap()).unwrap();
    assert_eq!(again, original, "{name}: DTO changed the wire shape");
}

#[test]
fn stats_types_round_trip() {
    round_trip::<StatsResponse>("stats_response_s1");
    round_trip::<HourDetail>("stats_hour");
    round_trip::<TracksResponse>("stats_tracks");
    round_trip::<TrackPlays>("stats_track_plays");
    round_trip::<RegionDetail>("stats_region");
    round_trip::<FalseStartAudit>("stats_false_starts");
    round_trip::<Removed>("stats_collapse");
    round_trip::<RemovedFrom>("stats_remove");
    round_trip::<Ignored>("stats_ignore");
    round_trip::<Restored>("stats_restore");
}

/// One response's upstream shape: key path -> allowed JSON types, and whether
/// upstream may omit the key.
struct Shape {
    types: BTreeMap<String, (BTreeSet<String>, bool)>,
}

impl Shape {
    fn of(spec: &Value) -> Self {
        let types = spec
            .as_object()
            .expect("shape is an object")
            .iter()
            .map(|(path, ty)| {
                let ty = ty.as_str().expect("type is a string");
                let (ty, optional) = match ty.strip_suffix('?') {
                    Some(t) => (t, true),
                    None => (ty, false),
                };
                (
                    path.clone(),
                    (ty.split('|').map(str::to_owned).collect(), optional),
                )
            })
            .collect();
        Shape { types }
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

/// Walks a fixture, recording every observed (path, type) and the paths whose
/// children cannot be observed (an empty array or record, or `null`).
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

/// Mismatches between one fixture and its upstream shape; empty when they agree.
fn compare(name: &str, shape: &Shape, value: &Value) -> Vec<String> {
    let mut seen = BTreeMap::new();
    let mut hidden = Vec::new();
    walk(shape, value, String::new(), &mut seen, &mut hidden);
    let mut problems = Vec::new();
    for (path, types) in &seen {
        match shape.types.get(path) {
            None => problems.push(format!("{name}: `{path}` is not an upstream key")),
            Some((allowed, _)) => {
                for ty in types {
                    if !allowed.contains(*ty) {
                        problems.push(format!(
                            "{name}: `{path}` is {ty}, upstream allows {allowed:?}"
                        ));
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
        if seen.contains_key(path) {
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

#[test]
fn stats_fixtures_cover_upstream_keys() {
    let keys = read("stats_upstream_keys.json");
    let keys: Value = serde_json::from_str(&keys).expect("key file is JSON");
    let problems: Vec<String> = FIXTURES
        .iter()
        .flat_map(|name| {
            let spec = keys
                .get(name)
                .unwrap_or_else(|| panic!("{name}: no shape in stats_upstream_keys.json"));
            compare(name, &Shape::of(spec), &fixture(name))
        })
        .collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The comparison itself must catch a renamed key, a wrong type and a missing key.
#[test]
fn key_comparison_catches_drift() {
    let shape = Shape::of(&serde_json::json!({
        "": "object", ".a": "number|null", ".m": "object", ".m{}": "number",
        ".l": "array", ".l[]": "object", ".l[].x": "string", ".s": "object?", ".s.v": "number"
    }));
    let ok = serde_json::json!({"a": null, "m": {"LIBRARY": 1}, "l": []});
    assert!(compare("ok", &shape, &ok).is_empty());
    let bad = serde_json::json!({"a": "1", "m": {"k": "x"}, "l": [{"y": "z"}], "s": {}});
    let problems = compare("bad", &shape, &bad);
    for needle in [
        "`.a` is string",
        "`.m{}` is string",
        "`.l[].y` is not an upstream key",
        "upstream key `.l[].x` is missing",
        "upstream key `.s.v` is missing",
    ] {
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "{needle} not reported in {problems:?}"
        );
    }
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

#[test]
fn stats_numbers_are_js_numbers() {
    let mut found = Vec::new();
    for name in FIXTURES {
        integral_floats(&fixture(name), name.to_owned(), &mut found);
    }
    assert!(found.is_empty(), "integral floats on the wire: {found:?}");
}
