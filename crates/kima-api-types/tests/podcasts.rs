//! The podcast wire fixtures (M6 Task 19, plan `2026-11-12-m6-podcasts.md`), captured from a
//! running kima-server by `tools/capture-api-fixtures.sh` (`KIMA_CAPTURE_SET=m6`) against
//! loopback feed/Apple stand-ins (`tools/podcast-capture-mock.py`), checked against the DTOs in
//! `kima_api_types::podcasts` and against upstream v1.12.3's backend shapes
//! (`fixtures/podcasts_upstream_keys.json`, itself re-checked against the pinned upstream
//! sources by `tools/check-podcast-upstream-keys.py`).
//!
//! Tests marked `#[ignore = "…capture…"]` read the captured fixtures; they stay ignored until the
//! fixtures are captured from the current checkout (never hand-written). The key-file and
//! comparison-machinery tests run without them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kima_api_types::podcasts::{
    Category, DiscoveryItem, EpisodeRow, EpisodeSearch, FollowedShow, ForYouRow, Listening,
    Preview, Progress, RefreshAll, ShowDetail, ShowSearch, Subscribed,
};
use kima_api_types::stats::StatsResponse;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Every captured podcast fixture (`podcasts_<name>_response.json`), by key-file name.
const FIXTURES: [&str; 30] = [
    "categories",
    "subscribe",
    "list",
    "list_ai_show",
    "detail",
    "episodes",
    "progress",
    "next",
    "next_end",
    "listening",
    "listening_ai_show",
    "charts",
    "discover_top",
    "discover_genre",
    "discover_genres",
    "for_you",
    "similar",
    "preview",
    "preview_followed",
    "preview_similar",
    "search",
    "search_discover",
    "recently_listened",
    "stats_spoken",
    "refresh",
    "refresh_all",
    "progress_clear",
    "ai_label_show",
    "ai_label_episode",
    "unsubscribe",
];

/// The 24 route registrations of upstream `routes/podcasts.ts` (v1.12.3), as the capture
/// manifest names them; each must have answered 2xx/3xx in the capture run.
const UPSTREAM_ROUTES: [&str; 24] = [
    "GET /podcasts",
    "GET /podcasts/categories",
    "GET /podcasts/charts",
    "GET /podcasts/for-you",
    "GET /podcasts/listening",
    "GET /podcasts/discover/top",
    "GET /podcasts/discover/genres",
    "GET /podcasts/discover/genre/:genreId",
    "GET /podcasts/preview/:itunesId",
    "GET /podcasts/preview/:itunesId/snippet",
    "GET /podcasts/preview/:itunesId/similar",
    "POST /podcasts/refresh-all",
    "GET /podcasts/:id",
    "GET /podcasts/:id/episodes",
    "GET /podcasts/:podcastId/episodes/:episodeId/next",
    "POST /podcasts/subscribe",
    "DELETE /podcasts/:id/unsubscribe",
    "GET /podcasts/:id/refresh",
    "GET /podcasts/:podcastId/episodes/:episodeId/stream",
    "POST /podcasts/:podcastId/episodes/:episodeId/progress",
    "DELETE /podcasts/:podcastId/episodes/:episodeId/progress",
    "GET /podcasts/:id/similar",
    "GET /podcasts/:id/cover",
    "GET /podcasts/episodes/:episodeId/cover",
];

/// The only keys D-207 lets kima add to upstream's podcast objects.
const AI_KEYS: [&str; 3] = ["aiLabel", "aiLabelSource", "aiHidden"];

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn keys() -> Value {
    serde_json::from_str(&read("tests/fixtures/podcasts_upstream_keys.json"))
        .expect("key file is JSON")
}

fn fixture_text(name: &str) -> String {
    read(&format!("tests/fixtures/podcasts_{name}_response.json"))
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(&fixture_text(name)).unwrap_or_else(|e| panic!("{name}: not JSON: {e}"))
}

fn manifest() -> Value {
    serde_json::from_str(&read("tests/fixtures/podcasts_capture_manifest.json"))
        .expect("manifest is JSON")
}

/// `{"type": "…", "divergence": "D-…"}` entries of a key-file entry's `kimaOnly`.
fn kima_only(entry: &Value) -> &serde_json::Map<String, Value> {
    entry["kimaOnly"]
        .as_object()
        .expect("kimaOnly is an object")
}

/// The key-file entry for `name`, following `sameShapeAs`.
fn entry<'a>(keys: &'a Value, name: &str) -> &'a Value {
    let e = keys
        .get(name)
        .unwrap_or_else(|| panic!("{name}: no entry in podcasts_upstream_keys.json"));
    match e.get("sameShapeAs").and_then(Value::as_str) {
        Some(other) => entry(keys, other),
        None => e,
    }
}

/// One response's allowed shape: key path -> allowed JSON types, and whether it may be absent.
struct Shape {
    types: BTreeMap<String, (BTreeSet<String>, bool)>,
    /// Paths that are kima-only (allowed, but never required by upstream).
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
    /// The upstream shape plus the listed kima-only keys (each optional: kima-only keys are
    /// allowed, upstream never requires them).
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

    /// A stats-file shape (plain `path: type` map, no kima-only part).
    fn plain(spec: &Value) -> Self {
        Shape::of(&json!({"upstream": spec}))
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

/// Mismatches between one fixture and its shape; empty when they agree. A key that is neither
/// upstream nor listed kima-only is reported, as is an upstream key the fixture lacks (unless
/// it sits below an empty array, a null or an absent optional key).
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

fn shape_for(keys: &Value, name: &str) -> Shape {
    if name == "stats_spoken" {
        // The same `GET /api/stats` route and DTO as the M-STATS fixture, with podcast data.
        let stats: Value = serde_json::from_str(&read("tests/fixtures/stats_upstream_keys.json"))
            .expect("stats key file is JSON");
        return Shape::plain(&stats["stats_response_s1"]);
    }
    Shape::of(entry(keys, name))
}

// ---------------------------------------------------------------------------------------------
// Without captured fixtures.

/// The key file names every fixture, its kima-only keys are only the D-207 AI keys (or cite
/// another existing DIVERGENCES row), and `sameShapeAs` points at real entries.
#[test]
fn key_file_lists_every_fixture_and_justifies_kima_extras() {
    let keys = keys();
    let rows: BTreeSet<String> = read("../../docs/DIVERGENCES.md")
        .lines()
        .filter_map(|l| l.strip_prefix("| D-"))
        .filter_map(|l| l.split(' ').next().map(|n| format!("D-{n}")))
        .collect();
    assert!(rows.contains("D-207"), "DIVERGENCES has no D-207 row");
    for name in FIXTURES.iter().filter(|n| **n != "stats_spoken") {
        let e = entry(&keys, name);
        assert!(e["upstream"].is_object(), "{name}: no upstream shape");
        for (path, spec) in kima_only(e) {
            let row = spec["divergence"].as_str().unwrap_or_default();
            assert!(
                rows.contains(row),
                "{name}: `{path}` cites missing row {row:?}"
            );
            let leaf = path.rsplit(['.', ']', '}']).next().unwrap_or_default();
            if row == "D-207" {
                assert!(AI_KEYS.contains(&leaf), "{name}: `{path}` is not an AI key");
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
    let expected: BTreeSet<&str> = FIXTURES
        .iter()
        .copied()
        .filter(|n| *n != "stats_spoken")
        .collect();
    assert_eq!(listed, expected, "key file and FIXTURES disagree");
}

/// The comparison catches a renamed key, a wrong type, a missing key, an unlisted kima-only
/// key, and a null hiding a nested shape is reported only as unobservable, never as a pass of
/// the nested keys' types.
#[test]
fn key_comparison_catches_drift_and_unlisted_extras() {
    let shape = Shape::of(&json!({
        "upstream": {"": "object", ".a": "number|null", ".l": "array", ".l[]": "object",
            ".l[].x": "string", ".s": "object|null", ".s.v": "number"},
        "kimaOnly": {".l[].aiLabel": {"type": "string", "divergence": "D-207"}}
    }));
    let ok = json!({"a": null, "l": [{"x": "1", "aiLabel": "ai"}], "s": null});
    assert!(compare("ok", &shape, &ok).is_empty());
    let without_extra = json!({"a": 1, "l": [{"x": "1"}], "s": {"v": 2}});
    assert!(compare("plain", &shape, &without_extra).is_empty());
    let bad = json!({"a": "1", "l": [{"y": "z", "aiHidden": 1}], "s": {}});
    let problems = compare("bad", &shape, &bad);
    for needle in [
        "`.a` is string",
        "`.l[].y` is neither",
        "`.l[].aiHidden` is neither",
        "upstream key `.l[].x` is missing",
        "upstream key `.s.v` is missing",
    ] {
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "{needle} not reported in {problems:?}"
        );
    }
}

/// The DTOs put whole numbers on the wire as integers (JavaScript has one number type).
#[test]
fn dto_numbers_are_js_numbers() {
    let p = Progress {
        current_time: 120.0,
        progress: 10.0,
        is_finished: false,
        last_played_at: "2026-11-13T00:00:00.000Z".into(),
    };
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["currentTime"], json!(120));
    assert_eq!(v["progress"], json!(10));
    let mut found = Vec::new();
    integral_floats(&v, "progress".into(), &mut found);
    assert!(found.is_empty(), "{found:?}");
}

// ---------------------------------------------------------------------------------------------
// Captured fixtures.

fn round_trip<T: DeserializeOwned + Serialize>(name: &str, value: &Value) {
    let typed: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|e| panic!("{name}: fixture does not parse as the DTO: {e}"));
    let again = serde_json::to_value(&typed).unwrap();
    assert_eq!(&again, value, "{name}: DTO changed the wire shape");
}

fn decode<T: DeserializeOwned>(name: &str) -> T {
    serde_json::from_value(fixture(name))
        .unwrap_or_else(|e| panic!("{name}: fixture does not parse as the DTO: {e}"))
}

#[test]
fn podcast_types_round_trip() {
    round_trip::<Vec<FollowedShow>>("list", &fixture("list"));
    round_trip::<Vec<FollowedShow>>("list_ai_show", &fixture("list_ai_show"));
    round_trip::<Vec<EpisodeRow>>("episodes", &fixture("episodes")["episodes"]);
    round_trip::<Vec<EpisodeRow>>("detail", &fixture("detail")["episodes"]);
    round_trip::<StatsResponse>("stats_spoken", &fixture("stats_spoken"));
    let _: ShowDetail = decode("detail");
    let _: Listening = decode("listening");
    let _: Listening = decode("listening_ai_show");
    let _: Vec<Category> = decode("categories");
    for name in [
        "charts",
        "discover_top",
        "discover_genre",
        "similar",
        "preview_similar",
    ] {
        let _: Vec<DiscoveryItem> = decode(name);
    }
    let _: Vec<ForYouRow> = decode("for_you");
    let _: Preview = decode("preview");
    let _: Preview = decode("preview_followed");
    let _: Subscribed = decode("subscribe");
    let _: ShowSearch = decode("search_discover");
    let _: EpisodeSearch = decode("search");
    let _: RefreshAll = decode("refresh_all");
}

#[test]
fn podcast_fixtures_cover_upstream_keys() {
    let keys = keys();
    let problems: Vec<String> = FIXTURES
        .iter()
        .flat_map(|name| compare(name, &shape_for(&keys, name), &fixture(name)))
        .collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Nulls and empty arrays hide nested shapes, so the capture's populated cases must really
/// populate them: every required upstream path of these fixtures is observed with a non-null
/// value at least once (a key that may be null still needs one non-null occurrence).
#[test]
fn populated_cases_observe_every_nested_key() {
    let keys = keys();
    // (fixture, paths that may legitimately stay null/absent in the populated case)
    let cases: [(&str, &[&str]); 6] = [
        ("preview", &[".subscribedPodcastId"]),
        ("detail", &[]),
        ("listening", &[".newEpisodes[].episode.progress"]),
        ("next", &[]),
        ("search", &[".artists", ".albums", ".tracks", ".audiobooks"]),
        ("for_you", &[]),
    ];
    for (name, allowed_empty) in cases {
        let shape = shape_for(&keys, name);
        let mut seen = BTreeMap::new();
        walk(
            &shape,
            &fixture(name),
            String::new(),
            &mut seen,
            &mut Vec::new(),
        );
        // Required upstream paths, each seen with a non-null value somewhere in the response.
        let unobserved: Vec<&String> = shape
            .types
            .iter()
            .filter(|(p, (_, optional))| !*optional && !shape.kima.contains(*p))
            .map(|(p, _)| p)
            .filter(|p| !allowed_empty.iter().any(|a| p == a || below(p, a)))
            .filter(|p| {
                // A key upstream always sends as null (a discovery item's `feedUrl`) is fine.
                let only_null = shape.types[*p].0 == BTreeSet::from(["null".to_owned()]);
                seen.get(*p)
                    .is_none_or(|types| !only_null && types == &BTreeSet::from(["null"]))
            })
            .collect();
        assert!(
            unobserved.is_empty(),
            "{name}: never observed {unobserved:?}"
        );
    }
}

fn ids(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("not an array: {v}"))
        .iter()
        .map(|e| {
            e["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| e["id"].to_string())
        })
        .collect()
}

fn itunes_ids(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e["itunesId"].as_str().unwrap().to_owned())
        .collect()
}

fn by_title<'a>(rows: &'a Value, title: &str) -> &'a Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|e| e["title"] == title)
        .unwrap_or_else(|| panic!("no {title:?} in {rows}"))
}

/// Cross-response identities and the critical values of the capture scenario
/// (`tools/capture-api-fixtures.sh`, M6 part): Race Show (Apple 555, unknown, one AI and one
/// human-tagged episode) and Robot Radio (Apple 777, channel tagged AI) followed through
/// `POST /subscribe`; Preview Show (888) only previewed.
#[test]
fn podcast_fixtures_share_captured_identities() {
    let race = fixture("subscribe")["podcast"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Followed list: Robot Radio hidden by default, shown with ?ai=show.
    assert_eq!(ids(&fixture("list")), BTreeSet::from([race.clone()]));
    let all = fixture("list_ai_show");
    let robot = all
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["title"] == "Robot Radio")
        .expect("Robot Radio listed with ai=show");
    assert_eq!(robot["aiLabel"], "ai");
    assert_eq!(robot["aiLabelSource"], "feed-show");
    let robot_id = robot["id"].as_str().unwrap().to_owned();

    // Detail and the episode page: the AI-tagged "Race Recap" hidden and counted.
    let detail = fixture("detail");
    assert_eq!(detail["id"], race.as_str());
    assert_eq!(detail["aiHidden"], 1);
    assert_eq!(detail["aiLabel"], "unknown");
    assert_eq!(detail["aiLabelSource"], Value::Null);
    let episodes = fixture("episodes");
    assert_eq!(ids(&episodes["episodes"]), ids(&detail["episodes"]));
    assert_eq!(episodes["aiHidden"], 1);
    assert_eq!(episodes["total"], 3);
    let titles: BTreeSet<&str> = detail["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        BTreeSet::from(["Grand Prix", "Sprint", "Qualifying"])
    );
    let sprint = by_title(&detail["episodes"], "Sprint");
    assert_eq!(sprint["aiLabel"], "human");
    assert_eq!(sprint["aiLabelSource"], "feed-episode");
    let sprint_id = sprint["id"].as_str().unwrap().to_owned();
    let gp_id = by_title(&detail["episodes"], "Grand Prix")["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Progress posted on Sprint (120 of 1200 s) is what `next` (after Qualifying) and the
    // listening shelf report.
    let progress = fixture("progress");
    assert_eq!(progress["success"], true);
    assert_eq!(progress["progress"]["currentTime"], 120);
    assert_eq!(progress["progress"]["progress"], 10);
    let next = fixture("next");
    assert_eq!(next["podcast"]["id"], race.as_str());
    assert_eq!(next["episode"]["id"], sprint_id.as_str());
    assert_eq!(next["episode"]["progress"]["currentTime"], 120);
    assert_eq!(fixture("next_end"), json!({"episode": null}));

    let listening = fixture("listening");
    let in_progress = listening["inProgress"].as_array().unwrap();
    assert_eq!(
        in_progress.len(),
        1,
        "Robot Radio's in-progress row is hidden"
    );
    assert_eq!(in_progress[0]["episode"]["id"], sprint_id.as_str());
    assert_eq!(in_progress[0]["podcast"]["id"], race.as_str());
    assert_eq!(listening["newEpisodes"][0]["episode"]["id"], gp_id.as_str());
    assert!(listening["aiHidden"].as_i64().unwrap() >= 1);
    let shown = fixture("listening_ai_show");
    assert!(shown.get("aiHidden").is_none(), "no count when showing");
    assert!(
        shown["inProgress"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["podcast"]["id"] == robot_id.as_str())
    );

    // Apple catalogue: Robot Radio (777) is labelled from its stored show and hidden.
    for name in ["charts", "discover_top"] {
        let shows = itunes_ids(&fixture(name));
        assert!(
            shows.contains("555") && shows.contains("888"),
            "{name}: {shows:?}"
        );
        assert!(!shows.contains("777"), "{name}: AI show not hidden");
    }
    assert_eq!(
        itunes_ids(&fixture("discover_genre")),
        BTreeSet::from(["555".into(), "888".into()])
    );
    let genres = fixture("discover_genres");
    assert_eq!(
        itunes_ids(&genres["1495"]),
        BTreeSet::from(["555".into(), "888".into()])
    );
    let similar = itunes_ids(&fixture("similar"));
    assert!(similar.contains("888") && !similar.contains("555"));
    let preview_similar = itunes_ids(&fixture("preview_similar"));
    assert!(preview_similar.contains("555") && !preview_similar.contains("888"));
    let for_you = fixture("for_you");
    for row in for_you.as_array().unwrap() {
        let shows = itunes_ids(&row["podcasts"]);
        assert!(
            !shows.contains("555") && !shows.contains("777"),
            "followed shows offered"
        );
    }
    assert!(for_you.as_array().unwrap().iter().any(|r| {
        r["because"]
            .as_array()
            .unwrap()
            .contains(&json!("Race Show"))
    }));
    let categories = fixture("categories");
    let improv = categories
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == 1495)
        .expect("Improv listed");
    assert_eq!(improv["parentId"], 1303);
    let known: BTreeSet<String> = ids(&categories);
    for c in detail["categories"].as_array().unwrap() {
        assert!(known.contains(&c["id"].to_string()), "{c} not a category");
    }

    // Previews: the unfollowed show populated from its feed, the followed one linked.
    let preview = fixture("preview");
    assert_eq!(preview["itunesId"], "888");
    assert_eq!(preview["isSubscribed"], false);
    assert_eq!(preview["subscribedPodcastId"], Value::Null);
    assert_eq!(preview["description"], "A show to preview.");
    let preview_titles: Vec<&str> = preview["previewEpisodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(preview_titles, ["Preview Two", "Preview One"]);
    assert_eq!(preview["snippet"]["title"], "Preview One");
    assert_eq!(preview["snippet"]["duration"], 420);
    let followed = fixture("preview_followed");
    assert_eq!(followed["isSubscribed"], true);
    assert_eq!(followed["subscribedPodcastId"], race.as_str());

    // Search: the followed show and its visible episodes; Apple search finds 555.
    let search = fixture("search");
    assert_eq!(ids(&search["podcasts"]), BTreeSet::from([race.clone()]));
    for e in search["episodes"].as_array().unwrap() {
        assert_eq!(e["podcastId"], race.as_str());
        assert!(ids(&detail["episodes"]).contains(e["id"].as_str().unwrap()));
    }
    assert!(!search["episodes"].as_array().unwrap().is_empty());
    for group in ["artists", "albums", "tracks", "audiobooks"] {
        assert_eq!(search[group], json!([]), "{group}");
    }
    let discover = fixture("search_discover");
    assert_eq!(discover["results"][0]["id"], 555);
    assert_eq!(discover["results"][0]["name"], "Race Show");

    // Recently listened: Race Show's in-progress Sprint, Robot Radio hidden; artist rows kept.
    let recent = fixture("recently_listened");
    let podcasts: Vec<&Value> = recent["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["type"] == "podcast")
        .collect();
    assert_eq!(podcasts.len(), 1);
    assert_eq!(podcasts[0]["id"], race.as_str());
    assert_eq!(podcasts[0]["episodeId"], sprint_id.as_str());
    assert_eq!(podcasts[0]["progress"], 10);
    assert_eq!(podcasts[0]["aiLabel"], "human");
    assert!(
        recent["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["type"] == "artist")
    );

    // Spoken stats (never filtered): two follows, two episodes in progress, four untouched.
    let spoken = &fixture("stats_spoken")["spoken"]["podcasts"];
    assert_eq!(spoken["subscriptions"], 2);
    assert_eq!(spoken["inProgress"], 2);
    assert_eq!(spoken["finished"], 0);
    assert_eq!(spoken["backlog"], 4);

    // Refresh finds the mock's added "Pit Stop"; refresh-all queues both follows.
    let refresh = fixture("refresh");
    assert_eq!(refresh["newEpisodesCount"], 1);
    assert_eq!(refresh["totalEpisodes"], 5);
    assert_eq!(fixture("refresh_all")["queued"], 2);
    assert_eq!(fixture("refresh_all")["total"], 2);

    assert_eq!(fixture("progress_clear")["success"], true);
    assert_eq!(
        fixture("ai_label_show"),
        json!({"aiLabel": "human", "aiLabelSource": "admin"})
    );
    assert_eq!(
        fixture("ai_label_episode"),
        json!({"aiLabel": "ai", "aiLabelSource": "admin"})
    );
    assert_eq!(fixture("unsubscribe")["success"], true);
}

/// Every upstream route answered 2xx/3xx in the capture run, and every list that hides AI
/// items carried `X-Kima-Ai-Hidden`, equal to the body's `aiHidden` where the body has one.
#[test]
fn capture_manifest_counts_every_route_and_hidden_header() {
    let manifest = manifest();
    let captures = manifest["captures"].as_array().expect("captures");
    let answered: BTreeSet<&str> = captures
        .iter()
        .filter(|c| (200..400).contains(&c["status"].as_i64().unwrap_or(0)))
        .map(|c| c["route"].as_str().unwrap())
        .collect();
    let missing: Vec<&&str> = UPSTREAM_ROUTES
        .iter()
        .filter(|r| !answered.contains(**r))
        .collect();
    assert!(
        missing.is_empty(),
        "routes without a 2xx/3xx capture: {missing:?}"
    );
    let named: BTreeSet<&str> = captures.iter().filter_map(|c| c["name"].as_str()).collect();
    for name in FIXTURES {
        assert!(named.contains(name), "{name} not in the manifest");
    }
    for c in captures {
        let Some(name) = c["name"].as_str() else {
            continue;
        };
        let body = fixture(name);
        if let (Some(header), Some(count)) = (c["aiHidden"].as_str(), body.get("aiHidden")) {
            assert_eq!(
                header,
                count.to_string(),
                "{name}: header and body disagree"
            );
        }
        if name.ends_with("_ai_show") {
            assert!(
                c["aiHidden"].is_null(),
                "{name}: showing must send no count"
            );
        }
    }
    for name in [
        "list",
        "charts",
        "listening",
        "episodes",
        "search",
        "recently_listened",
    ] {
        let c = captures.iter().find(|c| c["name"] == name).unwrap();
        assert!(
            c["aiHidden"].is_string(),
            "{name}: no X-Kima-Ai-Hidden header"
        );
    }
}

fn timestamps(v: &Value, path: String, out: &mut Vec<(String, String)>) {
    match v {
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                timestamps(item, format!("{path}[{i}]"), out);
            }
        }
        Value::Object(members) => {
            for (key, item) in members {
                match item {
                    Value::String(s) if key.ends_with("At") => {
                        out.push((format!("{path}.{key}"), s.clone()));
                    }
                    _ => timestamps(item, format!("{path}.{key}"), out),
                }
            }
        }
        _ => {}
    }
}

/// Upstream's dates are JS `Date`s serialized by `toISOString`: `YYYY-MM-DDTHH:MM:SS.mmmZ`.
#[test]
fn timestamps_are_js_iso() {
    // `0000-00-00T00:00:00.000Z`: digits where the template has `0`, the rest literal.
    let js_iso = |s: &str| {
        let template = b"0000-00-00T00:00:00.000Z";
        s.len() == template.len()
            && s.bytes().zip(template).all(|(c, &t)| {
                if t == b'0' {
                    c.is_ascii_digit()
                } else {
                    c == t
                }
            })
    };
    let mut bad = Vec::new();
    for name in FIXTURES.iter().filter(|n| **n != "stats_spoken") {
        let mut found = Vec::new();
        timestamps(&fixture(name), (*name).to_owned(), &mut found);
        bad.extend(found.into_iter().filter(|(_, s)| !js_iso(s)));
    }
    assert!(bad.is_empty(), "not toISOString: {bad:?}");
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
fn no_integral_floats_on_the_wire() {
    let mut found = Vec::new();
    for name in FIXTURES {
        integral_floats(&fixture(name), name.to_owned(), &mut found);
    }
    assert!(found.is_empty(), "integral floats on the wire: {found:?}");
}
