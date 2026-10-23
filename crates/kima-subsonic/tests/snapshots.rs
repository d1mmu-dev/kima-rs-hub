//! Insta snapshots of every endpoint, in XML and JSON, on the SQL fixture (spec §10.1), and a
//! structural test that parses each XML snapshot back by the JSON rules of §8.3.
//!
//! Snapshots are `snapshots/<endpoint>__<case>.xml.snap` and `.json.snap`. The XML is laid out one
//! element per line so a diff reads. Fixture timestamps stay in the snapshots; only values made at
//! request time are redacted (`serverVersion` always, the rest per case). Cases that mutate state
//! get one database per format (`snap_mut!`), so the second request never sees the first's writes.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_db::PgPool;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde_json::{Map, Value, json};

const REDACTED: &str = "[redacted]";
/// Fixture timestamps all start with this and are never redacted, only request-time values.
const FIXTURE_TIME: &str = "2026-01-01T";

/// `0` for a number, else the redaction marker.
fn placeholder(v: &Value) -> Value {
    if v.is_number() {
        json!(0)
    } else {
        json!(REDACTED)
    }
}

fn is_fixture_time(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.starts_with(FIXTURE_TIME))
}

// ---------------------------------------------------------------------------------------------
// The harness

struct Snap {
    pool: PgPool,
    server: TestServer,
    xml: bool,
    json: bool,
}

/// Per-case options: attribute names whose values are made at request time, and list elements
/// whose order is random (sorted by `id` before they are snapshotted).
#[derive(Clone, Copy, Default)]
struct Opts {
    redact: &'static [&'static str],
    unordered: &'static [&'static str],
}

const NONE: Opts = Opts {
    redact: &[],
    unordered: &[],
};

impl Snap {
    fn new(pool: PgPool, xml: bool, json: bool) -> Self {
        let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
        let router =
            kima_subsonic::router(fixture_state(pool.clone())).layer(MockConnectInfo(peer));
        Self {
            pool,
            server: TestServer::new(router).unwrap(),
            xml,
            json,
        }
    }

    async fn get(&self, endpoint: &str, params: &[(&str, &str)], json: bool) -> String {
        let path = if endpoint.contains('.') {
            format!("/{endpoint}")
        } else {
            format!("/{endpoint}.view")
        };
        let mut request = self.server.get(&path);
        if json {
            request = request.add_query_param("f", "json");
        }
        let res = request.add_query_params(params).await;
        res.assert_status_ok();
        res.text()
    }

    /// A request whose answer is not snapshotted (set-up before the case proper).
    async fn call(&self, endpoint: &str, params: &[(&str, &str)]) {
        self.get(endpoint, params, self.json).await;
    }

    /// Set-up SQL on the fixture database.
    async fn sql(&self, sql: &str) {
        sqlx::query(sql).execute(&self.pool).await.unwrap();
    }

    async fn case(&self, endpoint: &str, name: &str, params: &[(&str, &str)]) {
        self.case_with(endpoint, name, params, NONE).await;
    }

    async fn case_with(&self, endpoint: &str, name: &str, params: &[(&str, &str)], opts: Opts) {
        if self.xml {
            let raw = self.get(endpoint, params, false).await;
            assert!(
                raw.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response"),
                "{endpoint}/{name} is not an envelope: {raw}"
            );
            let xml = redact_xml(&sort_xml(&raw, opts.unordered), opts.redact);
            snapshot(&format!("{endpoint}__{name}.xml"), pretty_xml(&xml));
        }
        if self.json {
            let raw = self.get(endpoint, params, true).await;
            let mut value: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("{endpoint}/{name} is not JSON ({e}): {raw}"));
            sort_json(&mut value, opts.unordered);
            redact_json(&mut value, opts.redact);
            snapshot(
                &format!("{endpoint}__{name}.json"),
                serde_json::to_string_pretty(&value).unwrap(),
            );
        }
    }
}

fn snapshot(name: &str, body: String) {
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_omit_expression(true);
    settings.bind(|| insta::assert_snapshot!(name, body));
}

/// A test on one database that takes both formats (reads only).
macro_rules! snap {
    ($name:ident, |$s:ident| $body:block) => {
        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "fixtures", scripts("subsonic"))
        )]
        async fn $name(pool: PgPool) {
            let $s = Snap::new(pool, true, true);
            $body
        }
    };
}

/// A test for cases that write: the same body twice, each on its own database and format.
macro_rules! snap_mut {
    ($xml_name:ident, $json_name:ident, |$s:ident| $body:block) => {
        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "fixtures", scripts("subsonic"))
        )]
        async fn $xml_name(pool: PgPool) {
            let $s = Snap::new(pool, true, false);
            $body
        }

        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "fixtures", scripts("subsonic"))
        )]
        async fn $json_name(pool: PgPool) {
            let $s = Snap::new(pool, false, true);
            $body
        }
    };
}

// ---------------------------------------------------------------------------------------------
// Redaction, ordering and layout

/// Replaces the value of attribute `key` in `xml[from..to]`.
fn redact_attr(xml: &mut String, key: &str, from: usize, to: usize) -> usize {
    let needle = format!(" {key}=\"");
    let (mut at, mut end) = (from, to);
    while let Some(found) = xml[at..end].find(&needle) {
        let start = at + found + needle.len();
        let close = start + xml[start..].find('"').expect("attribute is closed");
        if xml[start..close].starts_with(FIXTURE_TIME) {
            at = close;
            continue;
        }
        // A number stays a number, so the JSON type check still bites.
        let placeholder = if xml[start..close].parse::<i64>().is_ok() {
            "0"
        } else {
            REDACTED
        };
        end = end + placeholder.len() - (close - start);
        xml.replace_range(start..close, placeholder);
        at = start + placeholder.len();
    }
    end
}

/// Redacts `serverVersion` and the case's keys. A key is an attribute name (everywhere) or
/// `element@attribute` (only on that element, so a song's fixture `created` survives while a new
/// playlist's does not).
fn redact_xml(xml: &str, keys: &[&str]) -> String {
    let mut out = xml.to_owned();
    for key in std::iter::once(&"serverVersion").chain(keys) {
        if let Some((element, attr)) = key.split_once('@') {
            let needle = format!("<{element} ");
            let mut from = 0;
            while let Some(at) = out[from..].find(&needle) {
                let start = from + at;
                let tag_end = start + out[start..].find('>').expect("tag is closed");
                from = redact_attr(&mut out, attr, start, tag_end);
            }
        } else {
            let len = out.len();
            redact_attr(&mut out, key, 0, len);
        }
    }
    out
}

fn redact_json(value: &mut Value, keys: &[&str]) {
    fn leaf(v: &Value) -> bool {
        !v.is_object() && !v.is_array()
    }
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if leaf(v)
                    && !is_fixture_time(v)
                    && (key == "serverVersion" || keys.contains(&key.as_str()))
                {
                    *v = placeholder(v);
                    continue;
                }
                let scoped: Vec<&str> = keys
                    .iter()
                    .filter_map(|k| k.strip_prefix(key.as_str())?.strip_prefix('@'))
                    .collect();
                let objects: Vec<&mut Value> = match v {
                    Value::Object(_) => vec![&mut *v],
                    Value::Array(items) => items.iter_mut().collect(),
                    _ => Vec::new(),
                };
                for object in objects {
                    if let Value::Object(inner) = object {
                        for attr in &scoped {
                            if let Some(x) = inner
                                .get_mut(*attr)
                                .filter(|x| leaf(x) && !is_fixture_time(x))
                            {
                                *x = placeholder(x);
                            }
                        }
                    }
                }
                redact_json(v, keys);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| redact_json(v, keys)),
        _ => {}
    }
}

/// Sorts the leaf elements `<tag id="..."/>` in place. `id` is each element's first attribute, so
/// sorting the text sorts by id.
fn sort_xml(xml: &str, tags: &[&str]) -> String {
    let mut out = xml.to_owned();
    for tag in tags {
        let needle = format!("<{tag} ");
        let mut spans = Vec::new();
        let mut from = 0;
        while let Some(at) = out[from..].find(&needle) {
            let start = from + at;
            let end = start + out[start..].find("/>").expect("leaf element") + 2;
            assert!(
                out[start..end].starts_with(&format!("<{tag} id=\"")),
                "id is not first: {}",
                &out[start..end]
            );
            spans.push((start, end));
            from = end;
        }
        let mut chunks: Vec<&str> = spans.iter().map(|&(a, b)| &out[a..b]).collect();
        chunks.sort();
        let mut sorted = String::with_capacity(out.len());
        let mut at = 0;
        for (&(a, b), chunk) in spans.iter().zip(&chunks) {
            sorted.push_str(&out[at..a]);
            sorted.push_str(chunk);
            at = b;
        }
        sorted.push_str(&out[at..]);
        out = sorted;
    }
    out
}

fn sort_json(value: &mut Value, keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if keys.contains(&key.as_str()) {
                    if let Value::Array(items) = v {
                        items.sort_by_key(|i| i["id"].as_str().unwrap_or_default().to_owned());
                    }
                }
                sort_json(v, keys);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| sort_json(v, keys)),
        _ => {}
    }
}

/// One element per line, indented by depth. Text elements stay on one line (`>` and `<` inside
/// text and attributes are escaped, so `><` only ever sits between two tags).
fn pretty_xml(xml: &str) -> String {
    let (decl, rest) = xml.split_once("?>").expect("declaration");
    let mut out = format!("{decl}?>\n");
    let mut depth = 0usize;
    let inner = &rest[1..rest.len() - 1];
    for segment in inner.split("><") {
        let closing = segment.starts_with('/');
        if closing {
            depth -= 1;
        }
        out.push_str(&"  ".repeat(depth));
        out.push('<');
        out.push_str(segment);
        out.push_str(">\n");
        if !closing && !segment.ends_with('/') && !segment.contains("</") {
            depth += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The structural test's parser (spec §8.3)

/// Attributes that are always strings, so a numeric-looking value stays one. (`id` is one too, except on `musicFolder`,
/// which has the number 1.)
const STRING_ATTRS: &[&str] = &[
    "parent", "name", "title", "album", "artist", "albumId", "artistId", "coverArt", "username",
    "comment", "message", "email",
];

/// List items that are objects even when they carry only text (an unsynced lyrics `line` is
/// `{"value": ...}`), unlike the bare values of `versions`.
const OBJECT_TEXT_LISTS: &[&str] = &["subsonic-response/lyricsList/structuredLyrics/line"];

/// Parses an envelope by the JSON rules: an element is an object, an attribute a typed key
/// (number, bool, else string), text `"value"`, and the elements `list_names()` names are arrays.
fn xml_to_json(xml: &str) -> Value {
    struct Open {
        path: String,
        map: Map<String, Value>,
        text: String,
        children: bool,
    }
    fn typed(s: &str) -> Value {
        if s == "true" || s == "false" {
            return json!(s == "true");
        }
        if let Ok(i) = s.parse::<i64>() {
            if i.to_string() == s {
                return json!(i);
            }
        }
        if let Ok(f) = s.parse::<f64>() {
            if f.is_finite() && f.to_string() == s {
                return json!(f);
            }
        }
        json!(s)
    }
    fn open(start: &BytesStart, parent: Option<&str>) -> Open {
        let name = String::from_utf8(start.name().as_ref().to_vec()).unwrap();
        let path = parent.map_or(name.clone(), |p| format!("{p}/{name}"));
        let mut map = Map::new();
        for attr in start.attributes() {
            let attr = attr.unwrap();
            let key = String::from_utf8(attr.key.as_ref().to_vec()).unwrap();
            let value = attr.unescape_value().unwrap();
            // Ids and names are strings even when they look like numbers (`parent="1"`).
            let value = if STRING_ATTRS.contains(&key.as_str())
                || (key == "id" && !path.ends_with("/musicFolder"))
            {
                json!(value)
            } else {
                typed(&value)
            };
            map.insert(key, value);
        }
        // Lists below this element exist even when XML wrote no element for them.
        for list in kima_subsonic::response::list_names() {
            if let Some(rest) = list.strip_prefix(&format!("{path}/")) {
                if !rest.contains('/') && path != "subsonic-response" {
                    map.insert(rest.to_owned(), json!([]));
                }
            }
        }
        Open {
            path,
            map,
            text: String::new(),
            children: false,
        }
    }
    fn close(node: Open, stack: &mut [Open]) -> (String, Value) {
        let name = node.path.rsplit('/').next().unwrap().to_owned();
        let scalar = node.map.is_empty() && !node.children && !node.text.is_empty();
        let value = if kima_subsonic::response::text_names().contains(&&*node.path) {
            json!(node.text)
        } else if scalar
            && kima_subsonic::response::list_names().contains(&&*node.path)
            && !OBJECT_TEXT_LISTS.contains(&&*node.path)
        {
            typed(&node.text)
        } else {
            let mut map = node.map;
            if !node.children && !node.text.is_empty() {
                map.insert("value".to_owned(), json!(node.text));
            }
            Value::Object(map)
        };
        if let Some(parent) = stack.last_mut() {
            parent.children = true;
            if kima_subsonic::response::list_names().contains(&&*node.path) {
                parent
                    .map
                    .entry(name.clone())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .unwrap()
                    .push(value);
            } else {
                parent.map.insert(name.clone(), value);
            }
            return (name, Value::Null);
        }
        (name, value)
    }

    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Open> = Vec::new();
    loop {
        match reader.read_event().expect("well-formed XML") {
            Event::Start(e) => {
                let parent = stack.last().map(|o| o.path.clone());
                stack.push(open(&e, parent.as_deref()));
            }
            Event::Empty(e) => {
                let parent = stack.last().map(|o| o.path.clone());
                let node = open(&e, parent.as_deref());
                let (name, value) = close(node, &mut stack);
                if stack.is_empty() {
                    return json!({ name: value });
                }
            }
            Event::End(_) => {
                let node = stack.pop().unwrap();
                let (name, value) = close(node, &mut stack);
                if stack.is_empty() {
                    return json!({ name: value });
                }
            }
            Event::Text(t) => {
                let text = t.unescape().unwrap();
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&text);
                }
            }
            Event::Eof => panic!("no root element"),
            _ => {}
        }
    }
}

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// The snapshot body without insta's header.
fn body(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let mut parts = text.splitn(3, "---\n");
    parts.next();
    parts.next();
    parts.next().expect("insta header").to_owned()
}

#[test]
fn xml_to_json_applies_the_rules() {
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response status=\"ok\" openSubsonic=\"true\">\
        <starred2><song id=\"a\" track=\"3\" score=\"1.5\"/></starred2>\
        <artist id=\"x\" name=\"1984\"/></subsonic-response>";
    let got = xml_to_json(xml);
    assert_eq!(
        got["subsonic-response"]["starred2"],
        json!({"artist": [], "album": [], "song": [{"id": "a", "track": 3, "score": 1.5}]})
    );
    // `artist` is a list under starred2 but one object under the root.
    assert_eq!(got["subsonic-response"]["artist"]["name"], json!("1984"));
    assert_eq!(got["subsonic-response"]["openSubsonic"], json!(true));
}

#[test]
fn xml_and_json_agree() {
    let mut pairs = 0;
    let mut files: Vec<_> = std::fs::read_dir(snapshot_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for xml_path in files
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".xml.snap"))
    {
        let json_path = PathBuf::from(
            xml_path
                .to_string_lossy()
                .replace(".xml.snap", ".json.snap"),
        );
        let name = xml_path.file_name().unwrap().to_string_lossy().into_owned();
        let json: Value = serde_json::from_str(&body(&json_path))
            .unwrap_or_else(|e| panic!("{name}: no JSON twin ({e})"));
        assert_eq!(xml_to_json(&body(xml_path)), json, "{name}");
        pairs += 1;
    }
    let json_files = files
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".json.snap"))
        .count();
    assert_eq!(pairs, json_files, "every JSON snapshot has an XML twin");
    assert!(pairs >= 150, "only {pairs} snapshot pairs");
}

// ---------------------------------------------------------------------------------------------
// Cases

fn alice<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut all = vec![("apiKey", FIXTURE_TOKEN)];
    all.extend_from_slice(extra);
    all
}

fn admin<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut all = vec![("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)];
    all.extend_from_slice(extra);
    all
}

/// Alice with her password, for the endpoints that refuse app tokens.
fn alice_pw<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut all = vec![("u", ALICE_NAME), ("p", ALICE_PASSWORD)];
    all.extend_from_slice(extra);
    all
}

const UNKNOWN: &str = "00000000-0000-7000-8000-ffffffffffff";

snap!(system, |s| {
    s.case("ping", "ok", &alice(&[])).await;
    s.case("ping", "password", &alice_pw(&[])).await;
    s.case("ping", "no_credentials", &[]).await;
    s.case("ping", "user_only", &[("u", "alice")]).await;
    s.case("ping", "wrong_password", &[("u", "alice"), ("p", "nope")])
        .await;
    s.case(
        "ping",
        "bad_api_key",
        &[("apiKey", "f".repeat(64).as_str())],
    )
    .await;
    s.case("getLicense", "ok", &alice(&[])).await;
    s.case("getMusicFolders", "ok", &alice(&[])).await;
    s.case("getOpenSubsonicExtensions", "no_auth", &[]).await;
    s.case("tokenInfo", "ok", &[("apiKey", FIXTURE_TOKEN)])
        .await;
    s.case("tokenInfo", "missing_key", &[]).await;
    s.case(
        "tokenInfo",
        "unknown_key",
        &[("apiKey", "0".repeat(64).as_str())],
    )
    .await;
    s.case("getScanStatus", "ok", &alice(&[])).await;
    s.case("startScan", "non_admin", &alice(&[])).await;
});

snap_mut!(start_scan_xml, start_scan_json, |s| {
    s.case("startScan", "admin", &admin(&[])).await;
});

snap!(browse, |s| {
    let artist1 = artist_id(1).to_string();
    let artist4 = artist_id(4).to_string();
    let album1 = album_id(1).to_string();
    let album2 = album_id(2).to_string();
    let album4 = album_id(4).to_string();
    let album6 = album_id(6).to_string();
    let track1 = track_id(1).to_string();
    let track2 = track_id(2).to_string();
    let track13 = track_id(13).to_string();
    let track20 = track_id(20).to_string();
    let prefixed_artist = format!("ar-{artist1}");
    let prefixed_album = format!("al-{album1}");

    s.case("getArtists", "alice", &alice(&[])).await;
    s.case("getArtists", "admin", &admin(&[])).await;
    s.case_with(
        "getIndexes",
        "alice",
        &alice(&[]),
        Opts {
            redact: &["lastModified"],
            ..NONE
        },
    )
    .await;
    s.case("getArtist", "beatles", &alice(&[("id", &artist1)]))
        .await;
    s.case("getArtist", "prefixed", &alice(&[("id", &prefixed_artist)]))
        .await;
    s.case(
        "getArtist",
        "no_present_tracks",
        &alice(&[("id", &artist4)]),
    )
    .await;
    s.case("getArtist", "unknown", &alice(&[("id", UNKNOWN)]))
        .await;
    s.case("getArtist", "not_a_uuid", &alice(&[("id", "banana")]))
        .await;
    s.case("getArtist", "missing_id", &alice(&[])).await;

    s.case("getMusicDirectory", "root", &alice(&[("id", "1")]))
        .await;
    s.case("getMusicDirectory", "artist", &alice(&[("id", &artist1)]))
        .await;
    s.case(
        "getMusicDirectory",
        "album",
        &alice(&[("id", &prefixed_album)]),
    )
    .await;
    s.case("getMusicDirectory", "unknown", &alice(&[("id", UNKNOWN)]))
        .await;

    s.case("getAlbum", "starred_rated", &alice(&[("id", &album1)]))
        .await;
    s.case("getAlbum", "other_user", &admin(&[("id", &album1)]))
        .await;
    s.case("getAlbum", "two_discs", &alice(&[("id", &album2)]))
        .await;
    s.case("getAlbum", "compilation", &alice(&[("id", &album4)]))
        .await;
    s.case("getAlbum", "no_present_tracks", &alice(&[("id", &album6)]))
        .await;
    s.case("getAlbum", "unknown", &alice(&[("id", UNKNOWN)]))
        .await;
    s.case("getAlbum", "missing_id", &alice(&[])).await;

    s.case("getSong", "starred_rated", &alice(&[("id", &track1)]))
        .await;
    s.case("getSong", "own_year", &alice(&[("id", &track2)]))
        .await;
    s.case("getSong", "compilation_artist", &alice(&[("id", &track13)]))
        .await;
    s.case("getSong", "missing_track", &alice(&[("id", &track20)]))
        .await;
    s.case("getSong", "unknown", &alice(&[("id", UNKNOWN)]))
        .await;
});

snap!(album_lists, |s| {
    let list = |ty: &'static str| [("type", ty), ("size", "500")];
    for ty in [
        "newest",
        "highest",
        "frequent",
        "recent",
        "alphabeticalByName",
        "alphabeticalByArtist",
        "starred",
        "byGenre",
    ] {
        s.case("getAlbumList2", ty, &alice(&list(ty))).await;
    }
    s.case_with(
        "getAlbumList2",
        "random",
        &alice(&list("random")),
        Opts {
            unordered: &["album"],
            ..NONE
        },
    )
    .await;
    s.case(
        "getAlbumList2",
        "byGenre_rock",
        &alice(&[("type", "byGenre"), ("genre", "rock")]),
    )
    .await;
    s.case(
        "getAlbumList2",
        "byYear",
        &alice(&[("type", "byYear"), ("fromYear", "1968"), ("toYear", "2009")]),
    )
    .await;
    s.case(
        "getAlbumList2",
        "byYear_descending",
        &alice(&[("type", "byYear"), ("fromYear", "2009"), ("toYear", "1968")]),
    )
    .await;
    s.case(
        "getAlbumList2",
        "byYear_missing_bound",
        &alice(&[("type", "byYear"), ("fromYear", "1968")]),
    )
    .await;
    s.case("getAlbumList2", "default_type", &alice(&[])).await;
    s.case(
        "getAlbumList2",
        "unknown_type",
        &alice(&[("type", "nonsense")]),
    )
    .await;
    s.case(
        "getAlbumList2",
        "paged",
        &alice(&[
            ("type", "alphabeticalByName"),
            ("size", "2"),
            ("offset", "1"),
        ]),
    )
    .await;
    s.case("getAlbumList2", "admin_frequent", &admin(&list("frequent")))
        .await;
    s.case("getAlbumList", "newest", &alice(&list("newest")))
        .await;
    s.case("getAlbumList", "starred", &alice(&list("starred")))
        .await;
});

snap!(search, |s| {
    let all = [("songCount", "500")];
    s.case("search3", "query", &alice(&[("query", "road")]))
        .await;
    s.case("search3", "query_songs", &alice(&[("query", "intro")]))
        .await;
    s.case("search3", "no_match", &alice(&[("query", "zzzzqqqq")]))
        .await;
    s.case("search3", "empty_query", &alice(&[("query", "")]))
        .await;
    s.case(
        "search3",
        "quoted_empty_query",
        &alice(&[("query", "\"\"")]),
    )
    .await;
    s.case("search3", "missing_query", &alice(&[])).await;
    s.case(
        "search3",
        "paging_first",
        &alice(&[
            ("query", ""),
            ("artistCount", "2"),
            ("albumCount", "2"),
            ("songCount", "7"),
        ]),
    )
    .await;
    s.case(
        "search3",
        "paging_second",
        &alice(&[
            ("query", ""),
            ("artistCount", "2"),
            ("artistOffset", "2"),
            ("albumCount", "2"),
            ("albumOffset", "2"),
            ("songCount", "7"),
            ("songOffset", "7"),
        ]),
    )
    .await;
    s.case(
        "search3",
        "paging_past_end",
        &alice(&[("query", ""), ("songCount", "7"), ("songOffset", "100")]),
    )
    .await;
    s.case("search2", "query", &alice(&[("query", "road")]))
        .await;
    s.case("search2", "empty_query", &alice(&[("query", ""), all[0]]))
        .await;
    s.case("search", "query", &alice(&[("query", "come")]))
        .await;
    s.case("search", "any", &alice(&[("any", "sun")])).await;
    s.case("search", "title", &alice(&[("title", "dear")]))
        .await;
    s.case(
        "search",
        "paged",
        &alice(&[("query", ""), ("count", "3"), ("offset", "3")]),
    )
    .await;

    s.case_with(
        "getRandomSongs",
        "all",
        &alice(&[("size", "500")]),
        Opts {
            unordered: &["song"],
            ..NONE
        },
    )
    .await;
    s.case_with(
        "getRandomSongs",
        "years",
        &alice(&[("size", "500"), ("fromYear", "1970"), ("toYear", "2009")]),
        Opts {
            unordered: &["song"],
            ..NONE
        },
    )
    .await;
    s.case_with(
        "getRandomSongs",
        "genre",
        &alice(&[("size", "500"), ("genre", "Rock")]),
        Opts {
            unordered: &["song"],
            ..NONE
        },
    )
    .await;
    s.case("getSongsByGenre", "ok", &alice(&[("genre", "Rock")]))
        .await;
    s.case("getSongsByGenre", "missing_genre", &alice(&[]))
        .await;
    s.case(
        "getTopSongs",
        "beatles",
        &alice(&[("artist", "the beatles")]),
    )
    .await;
    s.case(
        "getTopSongs",
        "substring_counted",
        &alice(&[("artist", "roux"), ("count", "2")]),
    )
    .await;
    s.case(
        "getTopSongs",
        "unknown_artist",
        &alice(&[("artist", "Nobody")]),
    )
    .await;
    s.case("getTopSongs", "missing_artist", &alice(&[])).await;
    let track1 = track_id(1).to_string();
    s.case("getSimilarSongs", "ok", &alice(&[("id", &track1)]))
        .await;
    s.case("getSimilarSongs", "missing_id", &alice(&[])).await;
    s.case("getSimilarSongs2", "ok", &alice(&[("id", &track1)]))
        .await;
    s.case("getGenres", "ok", &alice(&[])).await;
    s.case("getNowPlaying", "nobody", &alice(&[])).await;
});

snap_mut!(now_playing_xml, now_playing_json, |s| {
    let track = track_id(3).to_string();
    s.call(
        "scrobble",
        &alice(&[("id", &track), ("submission", "false"), ("c", "Symfonium")]),
    )
    .await;
    s.case_with(
        "getNowPlaying",
        "one_player",
        &admin(&[]),
        Opts {
            redact: &["minutesAgo"],
            ..NONE
        },
    )
    .await;
});

snap!(media_errors, |s| {
    let track20 = track_id(20).to_string();
    for endpoint in ["stream", "download", "hls", "hls.m3u8", "getCoverArt"] {
        s.case(endpoint, "missing_id", &alice(&[])).await;
        // `hls` answers a playlist for any id, as upstream: it never looks the track up.
        if !endpoint.starts_with("hls") {
            s.case(endpoint, "unknown_id", &alice(&[("id", UNKNOWN)]))
                .await;
        }
        s.case(endpoint, "no_credentials", &[]).await;
    }
    s.case("stream", "missing_track", &alice(&[("id", &track20)]))
        .await;
    s.case("download", "not_a_uuid", &alice(&[("id", "banana")]))
        .await;
    s.case("getCoverArt", "artist_has_none", &alice(&[("id", "ar-1")]))
        .await;
    s.case(
        "getAvatar",
        "unknown_user",
        &alice(&[("username", "nobody")]),
    )
    .await;
    s.case("getAvatar", "missing_username", &alice(&[])).await;
    s.case("getTranscodeStream", "missing_media_id", &alice(&[]))
        .await;
    s.case(
        "getTranscodeStream",
        "unknown_media_type",
        &alice(&[
            ("mediaId", UNKNOWN),
            ("mediaType", "video"),
            ("transcodeParams", "x"),
        ]),
    )
    .await;
    s.case(
        "getTranscodeStream",
        "podcast",
        &alice(&[
            ("mediaId", UNKNOWN),
            ("mediaType", "podcast"),
            ("transcodeParams", "x"),
        ]),
    )
    .await;
    s.case(
        "getTranscodeStream",
        "unknown_song",
        &alice(&[
            ("mediaId", UNKNOWN),
            ("mediaType", "song"),
            ("transcodeParams", "x"),
        ]),
    )
    .await;
    let track1 = track_id(1).to_string();
    s.case(
        "getTranscodeDecision",
        "direct",
        &alice(&[("mediaId", &track1), ("mediaType", "song")]),
    )
    .await;
    s.case(
        "getTranscodeDecision",
        "transcode",
        &alice(&[
            ("mediaId", &track1),
            ("mediaType", "song"),
            ("format", "mp3"),
        ]),
    )
    .await;
    s.case(
        "getTranscodeDecision",
        "podcast",
        &alice(&[("mediaId", &track1), ("mediaType", "podcast")]),
    )
    .await;
    s.case("getTranscodeDecision", "missing_media_id", &alice(&[]))
        .await;
});

snap!(queue_reads, |s| {
    s.case("getPlayQueue", "saved", &alice(&[])).await;
    s.case("getPlayQueueByIndex", "saved", &alice(&[])).await;
    let redact = Opts {
        redact: &["changed"],
        ..NONE
    };
    s.case_with("getPlayQueue", "none_saved", &admin(&[]), redact)
        .await;
    s.case_with("getPlayQueueByIndex", "none_saved", &admin(&[]), redact)
        .await;
});

snap_mut!(queue_writes_xml, queue_writes_json, |s| {
    let (t1, t2, t3) = (
        track_id(1).to_string(),
        track_id(2).to_string(),
        track_id(10).to_string(),
    );
    s.case(
        "savePlayQueue",
        "ok",
        &alice(&[
            ("id", &t1),
            ("id", &t2),
            ("id", &t3),
            ("current", &t2),
            ("position", "4000"),
        ]),
    )
    .await;
    s.case_with(
        "getPlayQueue",
        "after_save",
        &alice(&[]),
        Opts {
            redact: &["changed"],
            ..NONE
        },
    )
    .await;
    s.case(
        "savePlayQueue",
        "current_not_in_ids",
        &alice(&[("id", &t1), ("current", &t3)]),
    )
    .await;
    s.case(
        "savePlayQueueByIndex",
        "ok",
        &alice(&[
            ("id", &t3),
            ("id", &t1),
            ("currentIndex", "1"),
            ("position", "500"),
        ]),
    )
    .await;
    s.case_with(
        "getPlayQueueByIndex",
        "after_save",
        &alice(&[]),
        Opts {
            redact: &["changed"],
            ..NONE
        },
    )
    .await;
    s.case(
        "savePlayQueueByIndex",
        "index_out_of_range",
        &alice(&[("id", &t1), ("currentIndex", "5")]),
    )
    .await;
    s.case("savePlayQueueByIndex", "clear", &alice(&[])).await;
});

snap_mut!(scrobble_xml, scrobble_json, |s| {
    let t = track_id(4).to_string();
    s.case("scrobble", "submission", &alice(&[("id", &t)]))
        .await;
    s.case(
        "scrobble",
        "now_playing",
        &alice(&[("id", &t), ("submission", "false")]),
    )
    .await;
    s.case(
        "scrobble",
        "unknown_track_skipped",
        &alice(&[("id", UNKNOWN)]),
    )
    .await;
    s.case("scrobble", "missing_id", &alice(&[])).await;
});

snap_mut!(bookmarks_xml, bookmarks_json, |s| {
    let t3 = track_id(3).to_string();
    let t5 = track_id(5).to_string();
    s.case("getBookmarks", "alice", &alice(&[])).await;
    s.case("getBookmarks", "admin_none", &admin(&[])).await;
    s.case(
        "createBookmark",
        "ok",
        &alice(&[("id", &t5), ("position", "1234"), ("comment", "later")]),
    )
    .await;
    s.case(
        "createBookmark",
        "upsert",
        &alice(&[("id", &t3), ("position", "5")]),
    )
    .await;
    s.case_with(
        "getBookmarks",
        "after_writes",
        &alice(&[]),
        Opts {
            redact: &["bookmark@created", "bookmark@changed"],
            ..NONE
        },
    )
    .await;
    s.case(
        "createBookmark",
        "unknown_track",
        &alice(&[("id", UNKNOWN), ("position", "1")]),
    )
    .await;
    s.case("createBookmark", "missing_position", &alice(&[("id", &t5)]))
        .await;
    s.case("deleteBookmark", "ok", &alice(&[("id", &t3)])).await;
    s.case("deleteBookmark", "none_there", &alice(&[("id", &t5)]))
        .await;
});

snap!(playlist_reads, |s| {
    let public = fixture_id(5, 1).to_string();
    let private = fixture_id(5, 2).to_string();
    let admins = fixture_id(5, 3).to_string();
    s.case("getPlaylists", "alice", &alice(&[])).await;
    s.case("getPlaylists", "admin", &admin(&[])).await;
    s.case("getPlaylist", "own_public", &alice(&[("id", &public)]))
        .await;
    s.case("getPlaylist", "own_private", &alice(&[("id", &private)]))
        .await;
    s.case("getPlaylist", "others_public", &admin(&[("id", &public)]))
        .await;
    s.case("getPlaylist", "others_private", &admin(&[("id", &private)]))
        .await;
    s.case("getPlaylist", "not_yours", &alice(&[("id", &admins)]))
        .await;
    s.case("getPlaylist", "unknown", &alice(&[("id", UNKNOWN)]))
        .await;
    s.case("getPlaylist", "missing_id", &alice(&[])).await;
});

snap_mut!(playlist_writes_xml, playlist_writes_json, |s| {
    let public = fixture_id(5, 1).to_string();
    let admins = fixture_id(5, 3).to_string();
    let (t1, t2, t13) = (
        track_id(1).to_string(),
        track_id(2).to_string(),
        track_id(13).to_string(),
    );
    let fresh = Opts {
        redact: &["playlist@id", "playlist@created", "playlist@changed"],
        ..NONE
    };
    s.case_with(
        "createPlaylist",
        "new",
        &alice(&[
            ("name", "Fresh"),
            ("songId", &t1),
            ("songId", &t13),
            ("songId", &t1),
            ("songId", UNKNOWN),
        ]),
        fresh,
    )
    .await;
    s.case_with(
        "createPlaylist",
        "replace",
        &alice(&[("playlistId", &public), ("songId", &t2), ("songId", &t13)]),
        Opts {
            redact: &["changed"],
            ..NONE
        },
    )
    .await;
    s.case(
        "createPlaylist",
        "replace_not_yours",
        &alice(&[("playlistId", &admins), ("songId", &t2)]),
    )
    .await;
    s.case("createPlaylist", "no_name_or_id", &alice(&[])).await;
    s.case_with(
        "updatePlaylist",
        "edit",
        &alice(&[
            ("playlistId", &public),
            ("name", "Renamed"),
            ("public", "false"),
            ("songIdToAdd", &t2),
            ("songIdToAdd", &t13),
            ("songIndexToRemove", "0"),
        ]),
        NONE,
    )
    .await;
    s.case_with(
        "getPlaylist",
        "after_edit",
        &alice(&[("id", &public)]),
        Opts {
            redact: &["changed"],
            ..NONE
        },
    )
    .await;
    s.case(
        "updatePlaylist",
        "not_yours",
        &alice(&[("playlistId", &admins), ("name", "x")]),
    )
    .await;
    s.case("deletePlaylist", "ok", &alice(&[("id", &public)]))
        .await;
    s.case("deletePlaylist", "not_yours", &alice(&[("id", &admins)]))
        .await;
    s.case("deletePlaylist", "unknown", &alice(&[("id", UNKNOWN)]))
        .await;
});

snap!(star_reads, |s| {
    s.case("getStarred", "alice", &alice(&[])).await;
    s.case("getStarred2", "alice", &alice(&[])).await;
    s.case("getStarred", "admin", &admin(&[])).await;
    s.case("getStarred2", "admin", &admin(&[])).await;
});

snap_mut!(star_writes_xml, star_writes_json, |s| {
    let (t2, t5) = (track_id(2).to_string(), track_id(5).to_string());
    let (al2, ar2) = (album_id(2).to_string(), artist_id(2).to_string());
    s.case(
        "star",
        "ok",
        &alice(&[("id", &t5), ("albumId", &al2), ("artistId", &ar2)]),
    )
    .await;
    s.case_with(
        "getStarred2",
        "after_star",
        &alice(&[]),
        Opts {
            redact: &["starred"],
            ..NONE
        },
    )
    .await;
    s.case("star", "unknown_id", &alice(&[("id", UNKNOWN)]))
        .await;
    s.case("star", "no_ids", &alice(&[])).await;
    s.case("unstar", "ok", &alice(&[("id", &t5), ("albumId", &al2)]))
        .await;
    s.case("unstar", "unknown_id", &alice(&[("id", UNKNOWN)]))
        .await;
    s.case("setRating", "song", &alice(&[("id", &t2), ("rating", "3")]))
        .await;
    s.case(
        "setRating",
        "album",
        &alice(&[("id", &al2), ("rating", "5")]),
    )
    .await;
    s.case(
        "setRating",
        "clear",
        &alice(&[("id", &t2), ("rating", "0")]),
    )
    .await;
    s.case(
        "setRating",
        "out_of_range",
        &alice(&[("id", &t2), ("rating", "6")]),
    )
    .await;
    s.case(
        "setRating",
        "unknown_id",
        &alice(&[("id", UNKNOWN), ("rating", "3")]),
    )
    .await;
    s.case("setRating", "missing_rating", &alice(&[("id", &t2)]))
        .await;
    s.case_with(
        "getStarred",
        "after_rating",
        &alice(&[]),
        Opts {
            redact: &["starred"],
            ..NONE
        },
    )
    .await;
});

snap!(user_reads, |s| {
    s.case("getUser", "self_user", &alice(&[])).await;
    s.case("getUser", "self_admin", &admin(&[])).await;
    s.case(
        "getUser",
        "admin_asks_other",
        &admin(&[("username", "alice")]),
    )
    .await;
    s.case(
        "getUser",
        "user_asks_other",
        &alice(&[("username", "admin")]),
    )
    .await;
    s.case("getUser", "unknown_user", &admin(&[("username", "nobody")]))
        .await;
    s.case("getUsers", "admin", &admin(&[])).await;
    s.case("getUsers", "non_admin", &alice(&[])).await;
    s.case(
        "createUser",
        "token_auth_refused",
        &alice(&[("username", "bob"), ("password", "bobpass123")]),
    )
    .await;
    s.case(
        "createUser",
        "non_admin",
        &alice_pw(&[("username", "bob"), ("password", "bobpass123")]),
    )
    .await;
    s.case("createUser", "missing_username", &admin(&[])).await;
    s.case(
        "createUser",
        "duplicate",
        &admin(&[("username", "alice"), ("password", "whatever123")]),
    )
    .await;
    s.case(
        "updateUser",
        "non_admin",
        &alice_pw(&[("username", "alice"), ("adminRole", "true")]),
    )
    .await;
    s.case(
        "updateUser",
        "own_admin_role",
        &admin(&[("username", "admin"), ("adminRole", "false")]),
    )
    .await;
    s.case("deleteUser", "yourself", &admin(&[("username", "admin")]))
        .await;
    s.case(
        "deleteUser",
        "non_admin",
        &alice_pw(&[("username", "admin")]),
    )
    .await;
    s.case(
        "changePassword",
        "app_key_refused",
        &alice(&[("username", "alice"), ("password", "newpass123")]),
    )
    .await;
});

snap_mut!(user_writes_xml, user_writes_json, |s| {
    s.case(
        "createUser",
        "ok",
        &admin(&[("username", "bob"), ("password", "bobpass123")]),
    )
    .await;
    s.case(
        "createUser",
        "admin_role",
        &admin(&[
            ("username", "carol"),
            ("password", "carolpass123"),
            ("adminRole", "true"),
        ]),
    )
    .await;
    s.case_with("getUsers", "after_create", &admin(&[]), NONE)
        .await;
    s.case(
        "changePassword",
        "self",
        &alice_pw(&[("username", "alice"), ("password", "selfchanged1")]),
    )
    .await;
    s.case(
        "updateUser",
        "promote",
        &admin(&[("username", "alice"), ("adminRole", "true")]),
    )
    .await;
    s.case(
        "updateUser",
        "new_password",
        &admin(&[("username", "alice"), ("password", "alicenew123")]),
    )
    .await;
    s.case(
        "changePassword",
        "admin_changes_other",
        &admin(&[("username", "alice"), ("password", "alicenewer123")]),
    )
    .await;
    s.case_with("getUsers", "after_changes", &admin(&[]), NONE)
        .await;
    s.case("deleteUser", "ok", &admin(&[("username", "alice")]))
        .await;
    s.case("deleteUser", "unknown", &admin(&[("username", "nobody")]))
        .await;
});

snap_mut!(missing_xml, missing_json, |s| {
    // Track 20 is flagged missing. Put it into alice's playlist, her saved queue and now playing.
    let t20 = track_id(20);
    let playlist = fixture_id(5, 1);
    s.sql(&format!(
        "INSERT INTO playlist_items (id, playlist_id, track_id, sort) \
         VALUES ('{}', '{playlist}', '{t20}', 3)",
        fixture_id(7, 99)
    ))
    .await;
    s.sql(&format!(
        "UPDATE playback_state SET queue = queue || '[{{\"id\":\"{t20}\"}}]'::jsonb"
    ))
    .await;
    let id = t20.to_string();
    s.call(
        "scrobble",
        &alice(&[("id", &id), ("submission", "false"), ("c", "Phone")]),
    )
    .await;
    let playlist = playlist.to_string();
    s.case(
        "getPlaylist",
        "with_missing_track",
        &alice(&[("id", &playlist)]),
    )
    .await;
    s.case("getPlaylists", "with_missing_track", &alice(&[]))
        .await;
    s.case("getPlayQueue", "with_missing_track", &alice(&[]))
        .await;
    s.case("getPlayQueueByIndex", "with_missing_track", &alice(&[]))
        .await;
    s.case("getNowPlaying", "missing_track", &alice(&[])).await;
});

snap!(stubs, |s| {
    let track1 = track_id(1).to_string();
    for endpoint in [
        "getPodcasts",
        "getNewestPodcasts",
        "refreshPodcasts",
        "getShares",
        "createShare",
        "updateShare",
        "deleteShare",
        "getInternetRadioStations",
        "createInternetRadioStation",
        "updateInternetRadioStation",
        "deleteInternetRadioStation",
        "getChatMessages",
        "addChatMessage",
        "getVideos",
    ] {
        s.case(endpoint, "stub", &alice(&[("id", &track1)])).await;
    }
    s.case("getPodcastEpisode", "stub", &alice(&[("id", "1")]))
        .await;
    s.case("getVideoInfo", "stub", &alice(&[("id", "1")])).await;
    s.case("getVideoInfo", "missing_id", &alice(&[])).await;
    s.case("getCaptions", "stub", &alice(&[("id", "1")])).await;
    s.case("getCaptions", "missing_id", &alice(&[])).await;
    s.case("jukeboxControl", "stub", &alice(&[("action", "status")]))
        .await;
    s.case("jukeboxControl", "missing_action", &alice(&[]))
        .await;
    s.case("noSuchEndpoint", "not_implemented", &alice(&[]))
        .await;
});

// Artist and album info and lyrics (spec §18 rows 58-63): stored data only.
snap!(info, |s| {
    let artist1 = artist_id(1).to_string();
    let artist2 = artist_id(2).to_string();
    s.sql(&format!(
        r#"UPDATE artists SET bio = 'Formed in Liverpool & <famous>',
               "similar" = '[{{"name":"La Roux","mbid":null,"match":0.9}},
                             {{"name":"Not In The Library","mbid":null,"match":0.8}}]'
           WHERE id = '{artist1}'"#
    ))
    .await;
    for endpoint in ["getArtistInfo", "getArtistInfo2"] {
        s.case(endpoint, "known_artist", &alice(&[("id", &artist1)]))
            .await;
        s.case(endpoint, "no_bio_no_similar", &alice(&[("id", &artist2)]))
            .await;
        s.case(endpoint, "unknown_artist", &alice(&[("id", UNKNOWN)]))
            .await;
        s.case(endpoint, "missing_id", &alice(&[])).await;
    }
    let album1 = album_id(1).to_string();
    let album2 = album_id(2).to_string();
    for endpoint in ["getAlbumInfo", "getAlbumInfo2"] {
        s.case(endpoint, "with_mbid", &alice(&[("id", &album1)]))
            .await;
        s.case(endpoint, "without_mbid", &alice(&[("id", &album2)]))
            .await;
        s.case(endpoint, "unknown_album", &alice(&[("id", UNKNOWN)]))
            .await;
        s.case(endpoint, "missing_id", &alice(&[])).await;
    }
    s.case(
        "getLyrics",
        "match",
        &alice(&[("artist", "the beatles"), ("title", "come together")]),
    )
    .await;
    s.case(
        "getLyrics",
        "no_match",
        &alice(&[("title", "no such song")]),
    )
    .await;
    s.case("getLyrics", "no_parameters", &alice(&[])).await;
    let track1 = track_id(1).to_string();
    let track2 = track_id(2).to_string();
    s.case(
        "getLyricsBySongId",
        "synced_and_plain",
        &alice(&[("id", &track1)]),
    )
    .await;
    s.case(
        "getLyricsBySongId",
        "cached_miss",
        &alice(&[("id", &track2)]),
    )
    .await;
    s.case("getLyricsBySongId", "missing_id", &alice(&[])).await;
});

/// Every endpoint in the table has at least one snapshot.
#[test]
fn every_endpoint_has_a_snapshot() {
    let names: Vec<String> = std::fs::read_dir(snapshot_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for endpoint in kima_subsonic::routes::ENDPOINTS {
        let prefix = format!("{endpoint}__");
        for ext in [".xml.snap", ".json.snap"] {
            assert!(
                names
                    .iter()
                    .any(|n| n.starts_with(&prefix) && n.ends_with(ext)),
                "no {ext} snapshot for {endpoint}"
            );
        }
    }
}
