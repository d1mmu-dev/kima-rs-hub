//! A format-neutral response tree, its XML and JSON writers and the envelope (spec §8.3).

use std::borrow::Cow;

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use quick_xml::Writer;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use serde_json::{Map, Number, Value as Json};

use crate::error::SubsonicError;
use crate::params::Params;

pub const API_VERSION: &str = "1.16.1";
pub const SERVER_TYPE: &str = "Kima";
const XMLNS: &str = "http://subsonic.org/restapi";

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::Str(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::Str(v.to_owned())
    }
}
impl From<&String> for Value {
    fn from(v: &String) -> Self {
        Self::Str(v.clone())
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Self::Int(v.into())
    }
}
impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Self::Int(v.into())
    }
}
impl From<u16> for Value {
    fn from(v: u16) -> Self {
        Self::Int(v.into())
    }
}
impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Self::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}
impl From<usize> for Value {
    fn from(v: usize) -> Self {
        Self::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl Value {
    fn to_xml_string(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Int(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Bool(b) => b.to_string(),
        }
    }

    fn to_json(&self) -> Json {
        match self {
            Self::Str(s) => Json::String(s.clone()),
            Self::Int(i) => Json::Number((*i).into()),
            Self::Float(f) => Number::from_f64(*f).map_or(Json::Null, Json::Number),
            Self::Bool(b) => Json::Bool(*b),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Child {
    One(Node),
    /// Nodes that all serialize under the list's name; a JSON array even when empty.
    List(&'static str, Vec<Node>),
    /// Bare values (the extensions' `versions`): `<versions>1</versions>` and `[1]`.
    Scalars(&'static str, Vec<Value>),
}

impl Child {
    fn name(&self) -> &'static str {
        match self {
            Self::One(node) => node.name,
            Self::List(name, _) | Self::Scalars(name, _) => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    name: &'static str,
    attrs: Vec<(&'static str, Value)>,
    text: Option<String>,
    children: Vec<Child>,
}

impl Node {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            attrs: Vec::new(),
            text: None,
            children: Vec::new(),
        }
    }

    /// A body with no element of its own: its children go straight into the envelope.
    /// Their names must not clash with the envelope's own children (`error`); that is a
    /// debug-build panic.
    pub fn fragment() -> Self {
        Self::new("")
    }

    pub fn attr(mut self, key: &'static str, value: impl Into<Value>) -> Self {
        self.attrs.push((key, value.into()));
        self
    }

    pub fn attr_opt(self, key: &'static str, value: Option<impl Into<Value>>) -> Self {
        match value {
            Some(value) => self.attr(key, value),
            None => self,
        }
    }

    /// The same node under another element name (`song` as `entry` or `child`).
    pub fn renamed(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn child(mut self, node: Node) -> Self {
        self.push(Child::One(node));
        self
    }

    pub fn list(mut self, name: &'static str, nodes: Vec<Node>) -> Self {
        self.push(Child::List(name, nodes));
        self
    }

    pub fn scalars(mut self, name: &'static str, values: Vec<Value>) -> Self {
        self.push(Child::Scalars(name, values));
        self
    }

    fn push(&mut self, child: Child) {
        debug_assert!(
            !self.children.iter().any(|c| c.name() == child.name()),
            "node `{}` already has a child named `{}`",
            self.name,
            child.name()
        );
        self.children.push(child);
    }
}

/// `f=json` and `f=jsonp` (without a callback, as upstream) give JSON; anything else XML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Xml,
    Json,
}

impl Format {
    pub fn from_param(f: Option<&str>) -> Self {
        match f {
            Some("json" | "jsonp") => Self::Json,
            _ => Self::Xml,
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Xml => "text/xml; charset=utf-8",
            Self::Json => "application/json",
        }
    }
}

/// Drops the characters XML 1.0 forbids (C0 controls other than tab, newline and carriage
/// return, and U+FFFE/U+FFFF). Real-world ID3 tags contain them.
pub fn xml_safe(s: &str) -> Cow<'_, str> {
    let allowed = |c: char| {
        matches!(
            c,
            '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}'
        )
    };
    if s.chars().all(allowed) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.chars().filter(|&c| allowed(c)).collect())
    }
}

fn escape(s: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in xml_safe(s).chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            '"' if attribute => out.push_str("&quot;"),
            '\'' if attribute => out.push_str("&apos;"),
            '\t' if attribute => out.push_str("&#9;"),
            '\n' if attribute => out.push_str("&#10;"),
            c => out.push(c),
        }
    }
    out
}

fn write_text(w: &mut Writer<Vec<u8>>, text: &str) {
    // Writing into a Vec cannot fail.
    w.write_event(Event::Text(BytesText::from_escaped(escape(text, false))))
        .unwrap();
}

fn write_element(w: &mut Writer<Vec<u8>>, name: &str, node: &Node) {
    let mut start = BytesStart::new(name);
    for (key, value) in &node.attrs {
        let escaped = escape(&value.to_xml_string(), true);
        start.push_attribute(Attribute {
            key: quick_xml::name::QName(key.as_bytes()),
            value: Cow::Owned(escaped.into_bytes()),
        });
    }
    let has_content = node.text.is_some()
        || node.children.iter().any(|c| match c {
            Child::One(_) => true,
            Child::List(_, nodes) => !nodes.is_empty(),
            Child::Scalars(_, values) => !values.is_empty(),
        });
    if !has_content {
        w.write_event(Event::Empty(start)).unwrap();
        return;
    }
    w.write_event(Event::Start(start)).unwrap();
    if let Some(text) = &node.text {
        write_text(w, text);
    }
    for child in &node.children {
        match child {
            Child::One(node) => write_element(w, node.name, node),
            Child::List(name, nodes) => {
                for node in nodes {
                    write_element(w, name, node);
                }
            }
            Child::Scalars(name, values) => {
                for value in values {
                    w.write_event(Event::Start(BytesStart::new(*name))).unwrap();
                    write_text(w, &value.to_xml_string());
                    w.write_event(Event::End(BytesEnd::new(*name))).unwrap();
                }
            }
        }
    }
    w.write_event(Event::End(BytesEnd::new(name))).unwrap();
}

pub fn write_xml(node: &Node) -> String {
    let mut writer = Writer::new(Vec::new());
    write_element(&mut writer, node.name, node);
    let body = String::from_utf8(writer.into_inner()).expect("quick-xml writes UTF-8");
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>{body}")
}

/// The node as a JSON object: attributes are keys, `text` is `"value"`, a `One` child is an
/// object and a `List` is always an array.
pub fn write_json(node: &Node) -> Json {
    let mut object = Map::new();
    for (key, value) in &node.attrs {
        object.insert((*key).to_owned(), value.to_json());
    }
    if let Some(text) = &node.text {
        object.insert("value".to_owned(), Json::String(text.clone()));
    }
    for child in &node.children {
        match child {
            Child::One(node) => {
                object.insert(node.name.to_owned(), write_json(node));
            }
            Child::List(name, nodes) => {
                object.insert(
                    (*name).to_owned(),
                    Json::Array(nodes.iter().map(write_json).collect()),
                );
            }
            Child::Scalars(name, values) => {
                object.insert(
                    (*name).to_owned(),
                    Json::Array(values.iter().map(Value::to_json).collect()),
                );
            }
        }
    }
    Json::Object(object)
}

/// Where the writers emit a list: `/`-joined element paths from the root, each ending in the
/// list's element name. `Child::List` and `Child::Scalars` always give a JSON array, so the XML
/// parser in the structural test (spec §10.1) needs this to tell a one-item list from an object,
/// and to put back the empty arrays XML leaves out. A path is exact: `artist` is a list under
/// `starred` but one object under the root (`getArtist`). Keep it in step with the handlers.
const LIST_NAMES: &[&str] = &[
    "subsonic-response/openSubsonicExtensions",
    "subsonic-response/openSubsonicExtensions/versions",
    "subsonic-response/musicFolders/musicFolder",
    "subsonic-response/artists/index",
    "subsonic-response/artists/index/artist",
    "subsonic-response/indexes/index",
    "subsonic-response/indexes/index/artist",
    "subsonic-response/artist/album",
    "subsonic-response/directory/child",
    "subsonic-response/album/song",
    "subsonic-response/albumList2/album",
    "subsonic-response/albumList/album",
    "subsonic-response/searchResult3/artist",
    "subsonic-response/searchResult3/album",
    "subsonic-response/searchResult3/song",
    "subsonic-response/searchResult2/artist",
    "subsonic-response/searchResult2/album",
    "subsonic-response/searchResult2/song",
    "subsonic-response/searchResult/match",
    "subsonic-response/randomSongs/song",
    "subsonic-response/songsByGenre/song",
    "subsonic-response/topSongs/song",
    "subsonic-response/similarSongs/song",
    "subsonic-response/similarSongs2/song",
    "subsonic-response/genres/genre",
    "subsonic-response/nowPlaying/entry",
    "subsonic-response/playQueue/entry",
    "subsonic-response/playQueueByIndex/entry",
    "subsonic-response/bookmarks/bookmark",
    "subsonic-response/playlists/playlist",
    "subsonic-response/playlist/entry",
    "subsonic-response/starred/artist",
    "subsonic-response/starred/album",
    "subsonic-response/starred/song",
    "subsonic-response/starred2/artist",
    "subsonic-response/starred2/album",
    "subsonic-response/starred2/song",
    "subsonic-response/users/user",
    "subsonic-response/users/user/folder",
    "subsonic-response/user/folder",
    "subsonic-response/transcodeDecision/transcodeReason",
];

/// The paths the writers always emit as lists (see `LIST_NAMES`).
pub fn list_names() -> &'static [&'static str] {
    LIST_NAMES
}

fn envelope(failed: Option<&SubsonicError>, body: Option<Node>) -> Node {
    let mut root = Node::new("subsonic-response")
        .attr("xmlns", XMLNS)
        .attr("status", if failed.is_some() { "failed" } else { "ok" })
        .attr("version", API_VERSION)
        .attr("type", SERVER_TYPE)
        .attr("serverVersion", env!("CARGO_PKG_VERSION"))
        .attr("openSubsonic", true);
    if let Some(error) = failed {
        root = root.child(
            Node::new("error")
                .attr("code", error.code)
                .attr("message", error.message.as_str()),
        );
    }
    match body {
        // A fragment's children sit directly in the envelope (`openSubsonicExtensions`).
        Some(body) if body.name.is_empty() => {
            for child in body.children {
                root.push(child);
            }
        }
        Some(body) => root = root.child(body),
        None => {}
    }
    root
}

fn serialize(format: Format, root: &Node) -> String {
    match format {
        Format::Xml => write_xml(root),
        Format::Json => {
            let mut wrapper = Map::new();
            wrapper.insert("subsonic-response".to_owned(), write_json(root));
            Json::Object(wrapper).to_string()
        }
    }
}

/// The body of an `ok` answer.
pub fn ok(format: Format, body: Option<Node>) -> String {
    serialize(format, &envelope(None, body))
}

/// The body of a `failed` answer.
pub fn failed(format: Format, error: &SubsonicError) -> String {
    serialize(format, &envelope(Some(error), None))
}

fn respond(format: Format, body: String) -> Response {
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    response
}

pub fn render_error(format: Format, error: &SubsonicError) -> Response {
    respond(format, failed(format, error))
}

/// A successful answer. `None` is the empty `ok`.
#[derive(Debug, Clone)]
pub struct Answer(pub Option<Node>);

impl Answer {
    pub fn ok() -> Self {
        Self(None)
    }

    pub fn node(node: Node) -> Self {
        Self(Some(node))
    }
}

/// An empty 200 carrying the answer; `render_layer` writes the envelope.
impl IntoResponse for Answer {
    fn into_response(self) -> Response {
        let mut response = Response::new(axum::body::Body::empty());
        response.extensions_mut().insert(self);
        response
    }
}

/// Turns `Answer` and `SubsonicError` responses into the envelope in the request's format.
/// Responses without either (media) pass through. Needs `params_layer` outside it.
pub async fn render_layer(request: Request, next: Next) -> Response {
    let format = request
        .extensions()
        .get::<Params>()
        .map_or(Format::Xml, Params::format);
    let mut response = next.run(request).await;
    if let Some(Answer(node)) = response.extensions_mut().remove::<Answer>() {
        return respond(format, ok(format, node));
    }
    if let Some(error) = response.extensions_mut().remove::<SubsonicError>() {
        return render_error(format, &error);
    }
    response
}

#[cfg(test)]
mod tests {
    use quick_xml::Reader;
    use quick_xml::events::Event as XmlEvent;

    use super::*;

    #[test]
    fn xml_envelope_attribute_order() {
        assert_eq!(
            ok(Format::Xml, None),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"http://subsonic.org/restapi\" \
             status=\"ok\" version=\"1.16.1\" type=\"Kima\" serverVersion=\"0.1.0\" openSubsonic=\"true\"/>"
        );
    }

    #[test]
    fn json_types_kept() {
        let node = Node::new("song")
            .attr("track", 3)
            .attr("starred", true)
            .attr("score", 1.5)
            .attr("title", "x");
        let json = write_json(&node);
        assert_eq!(json["track"], serde_json::json!(3));
        assert_eq!(json["starred"], serde_json::json!(true));
        assert_eq!(json["score"], serde_json::json!(1.5));
        assert_eq!(json["title"], serde_json::json!("x"));
        let env: Json = serde_json::from_str(&ok(Format::Json, Some(node))).unwrap();
        assert_eq!(env["subsonic-response"]["openSubsonic"], true);
        assert_eq!(env["subsonic-response"]["song"]["track"], 3);
    }

    #[test]
    fn json_empty_list_is_array() {
        let node = Node::new("album").list("song", vec![]);
        assert_eq!(write_json(&node)["song"], serde_json::json!([]));
    }

    #[test]
    fn xml_empty_list_writes_nothing() {
        let node = Node::new("album").attr("id", "a").list("song", vec![]);
        assert_eq!(
            write_xml(&node),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><album id=\"a\"/>"
        );
    }

    #[test]
    fn list_nodes_use_the_list_name_in_xml() {
        let node = Node::new("album").list(
            "song",
            vec![Node::new("x").attr("id", 1), Node::new("x").attr("id", 2)],
        );
        assert!(write_xml(&node).ends_with("<album><song id=\"1\"/><song id=\"2\"/></album>"));
    }

    #[test]
    fn text_is_value_in_json() {
        let node = Node::new("lyrics").attr("artist", "a").text("la la");
        assert_eq!(write_json(&node)["value"], "la la");
        assert!(write_xml(&node).ends_with("<lyrics artist=\"a\">la la</lyrics>"));
    }

    #[test]
    fn scalars_are_typed_in_json_and_elements_in_xml() {
        let node = Node::new("ext")
            .attr("name", "formPost")
            .scalars("versions", vec![1.into()]);
        assert_eq!(write_json(&node)["versions"], serde_json::json!([1]));
        assert!(write_xml(&node).ends_with("<ext name=\"formPost\"><versions>1</versions></ext>"));
    }

    #[test]
    fn failed_envelope_has_error() {
        let error = SubsonicError::not_found("Song not found");
        let json: Json = serde_json::from_str(&failed(Format::Json, &error)).unwrap();
        let root = &json["subsonic-response"];
        assert_eq!(root["status"], "failed");
        assert_eq!(root["error"]["code"], 70);
        assert_eq!(root["error"]["message"], "Song not found");
        assert!(
            failed(Format::Xml, &error).contains("<error code=\"70\" message=\"Song not found\"/>")
        );
    }

    #[test]
    fn hostile_text_is_well_formed() {
        let hostile = "a&b<c>\"d'e\u{1}f\u{1b}g";
        let node = Node::new("song").attr("title", hostile).text(hostile);
        let xml = write_xml(&node);
        let mut reader = Reader::from_str(&xml);
        let (mut attr, mut text) = (None, None);
        loop {
            match reader.read_event().expect("well-formed") {
                XmlEvent::Start(e) => {
                    let a = e.attributes().next().unwrap().unwrap();
                    attr = Some(a.unescape_value().unwrap().into_owned());
                }
                XmlEvent::Text(t) => text = Some(t.unescape().unwrap().into_owned()),
                XmlEvent::Eof => break,
                _ => {}
            }
        }
        assert_eq!(attr.unwrap(), "a&b<c>\"d'efg");
        assert_eq!(text.unwrap(), "a&b<c>\"d'efg");
        assert_eq!(write_json(&node)["title"], hostile);
    }

    #[test]
    fn whitespace_in_attributes_survives() {
        let node = Node::new("song").attr("title", "a\nb\tc\r");
        let xml = write_xml(&node);
        let mut reader = Reader::from_str(&xml);
        loop {
            match reader.read_event().unwrap() {
                XmlEvent::Empty(e) => {
                    let a = e.attributes().next().unwrap().unwrap();
                    assert_eq!(a.unescape_value().unwrap(), "a\nb\tc\r");
                    break;
                }
                XmlEvent::Eof => panic!("no element"),
                _ => {}
            }
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "already has a child")]
    fn debug_asserts_unique_child_names() {
        let _ = Node::new("a").child(Node::new("b")).child(Node::new("b"));
    }
}
