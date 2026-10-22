//! An artist's bio and image from Wikidata and the English Wikipedia (spec §12.2; upstream
//! `wikidata.ts:46-94`).
//!
//! Three requests: the SPARQL item for the MBID, the item's entity data, and the Wikipedia
//! summary of its English article. No item is an answer with nothing in it (cached as an
//! absence of the SPARQL request); a missing article (404) is no bio; any other failure is the
//! lookup's failure, so the caller keeps what it has. Answers are untrusted: every field is
//! optional, the item id must be `Q<digits>` before it is put into a path, the bio is cut to
//! [`MAX_BIO_CHARS`], and the image file name is hashed and percent-encoded, never fetched here.

use std::sync::LazyLock;

use kima_net::{Host, Net, Outcome, Req};
use md5::{Digest, Md5};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::Mbid;
use crate::answer::{self, Got, array, items};

/// The longest bio kept (characters). Upstream keeps the whole extract; a summary extract is a
/// few paragraphs, so this only bounds an answer that is not one.
pub const MAX_BIO_CHARS: usize = 5000;

/// The longest Commons file name taken (characters; Commons allows 240 bytes).
const MAX_FILE_CHARS: usize = 255;

/// The longest Wikipedia title taken (characters; Wikipedia allows 255 bytes).
const MAX_TITLE_CHARS: usize = 255;

/// What Wikidata knows of an artist. Both empty when it has no item for the MBID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WikiInfo {
    /// The English Wikipedia summary (`extract`).
    pub bio: Option<String>,
    /// The Commons URL of the item's `P18` image.
    pub image_url: Option<String>,
}

/// `encodeURIComponent`'s set: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is escaped.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

static ITEM_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Q[1-9][0-9]{0,11}$").expect("item id pattern"));

#[derive(Deserialize)]
struct Sparql {
    results: Bindings,
}

#[derive(Deserialize)]
struct Bindings {
    #[serde(deserialize_with = "array")]
    bindings: Vec<Value>,
}

#[derive(Deserialize)]
struct Entities {
    entities: Map<String, Value>,
}

/// SPARQL with no binding: Wikidata has no item for the MBID (cached as an absence).
fn no_binding(v: &Value) -> bool {
    v.pointer("/results/bindings")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
}

fn sparql_request(mbid: Mbid) -> Req {
    // The MBID is a typed id: only its hyphenated hex form reaches the query text.
    Req::get(Host::WikidataSparql, "/sparql")
        .query(
            "query",
            format!("SELECT ?item WHERE {{ ?item wdt:P434 \"{mbid}\" }} LIMIT 1"),
        )
        .query("format", "json")
        .absent_when(no_binding)
}

/// The item id of the first binding, when it is `Q<digits>`.
fn item_id(bindings: &[Value]) -> Option<String> {
    let uri = bindings.first()?.pointer("/item/value")?.as_str()?;
    let id = uri.rsplit('/').next()?;
    ITEM_ID.is_match(id).then(|| id.to_string())
}

/// The English Wikipedia article's title, as a path segment.
fn summary_path(title: &str) -> String {
    format!(
        "/api/rest_v1/page/summary/{}",
        utf8_percent_encode(title, URI_COMPONENT)
    )
}

/// The Commons URL of `file` (spec §12.2): the MD5 of the name with spaces as underscores picks
/// the directories `{m[0]}/{m[0..2]}`, and the name is percent-encoded as `encodeURIComponent`
/// encodes it.
pub fn commons_url(file: &str) -> String {
    let name = file.replace(' ', "_");
    let hash = hex::encode(Md5::digest(name.as_bytes()));
    format!(
        "https://upload.wikimedia.org/wikipedia/commons/{}/{}/{}",
        &hash[..1],
        &hash[..2],
        utf8_percent_encode(&name, URI_COMPONENT)
    )
}

/// The `P18` image of an entity as a Commons URL: the first `preferred` claim, else the first
/// `normal` one; a `deprecated` claim is never taken. A value that is not a non-empty string of
/// at most 255 characters is no image.
pub fn image_of(entity: &Value) -> Option<String> {
    let claims = items(entity.pointer("/claims/P18"));
    let ranked = |rank: &str| {
        claims
            .iter()
            .find(|c| c.get("rank").and_then(Value::as_str) == Some(rank))
    };
    let claim = ranked("preferred").or_else(|| ranked("normal"))?;
    let file = claim.pointer("/mainsnak/datavalue/value")?.as_str()?.trim();
    if file.is_empty() || file.chars().count() > MAX_FILE_CHARS {
        return None;
    }
    Some(commons_url(file))
}

/// The bio and image of the artist with `mbid` (spec §12.2). No Wikidata item →
/// `Answered(WikiInfo::default())`; no English article, or a summary that is 404 → no bio; any
/// other failure of any of the three requests is returned as it is.
pub async fn lookup(net: &Net, mbid: Mbid) -> Outcome<WikiInfo> {
    let bindings = match answer::got(net.get_json::<Sparql>(sparql_request(mbid)).await) {
        Ok(Got::Answered(s)) => s.results.bindings,
        Ok(Got::Absent(_)) => return Outcome::Answered(WikiInfo::default()),
        Err(o) => return o,
    };
    // An id that is not `Q<digits>` is treated as no item: it never reaches a path.
    let Some(id) = item_id(&bindings) else {
        return Outcome::Answered(WikiInfo::default());
    };
    let req = Req::get(
        Host::Wikidata,
        &format!("/wiki/Special:EntityData/{id}.json"),
    );
    let mut entities = match answer::got(net.get_json::<Entities>(req).await) {
        Ok(Got::Answered(e)) => e.entities,
        Ok(Got::Absent(_)) => return Outcome::Answered(WikiInfo::default()),
        Err(o) => return o,
    };
    // A redirected item answers under its new id; then the one entity there is is it.
    let entity = match entities.remove(&id) {
        Some(e) => e,
        None if entities.len() == 1 => entities
            .into_iter()
            .next()
            .map(|(_, e)| e)
            .unwrap_or_default(),
        None => return Outcome::Answered(WikiInfo::default()),
    };
    let image_url = image_of(&entity);
    let title = answer::text(entity.pointer("/sitelinks/enwiki/title"), usize::MAX)
        .filter(|t| t.chars().count() <= MAX_TITLE_CHARS);
    let bio = match title {
        None => None,
        Some(title) => {
            let req = Req::get(Host::Wikipedia, &summary_path(&title)).absent_on_404();
            match answer::got(net.get_json::<Value>(req).await) {
                Ok(Got::Answered(summary)) => answer::text(summary.get("extract"), MAX_BIO_CHARS),
                Ok(Got::Absent(_)) => None,
                Err(o) => return o,
            }
        }
    };
    Outcome::Answered(WikiInfo { bio, image_url })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_ids_are_checked() {
        let b = |v: &str| vec![serde_json::json!({"item": {"value": v}})];
        assert_eq!(
            item_id(&b("http://www.wikidata.org/entity/Q1299")).as_deref(),
            Some("Q1299")
        );
        for bad in [
            "http://x/Q",
            "http://x/Q01",
            "http://x/P18",
            "http://x/Q1.json",
            "Q1 ",
            "",
        ] {
            assert_eq!(item_id(&b(bad)), None, "{bad}");
        }
        assert_eq!(item_id(&[]), None);
    }

    #[test]
    fn the_sparql_text_is_the_specs() {
        let m = Mbid::parse("b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d").unwrap();
        let shown = format!("{:?}", sparql_request(m));
        assert!(
            shown.contains(
                r#"SELECT ?item WHERE { ?item wdt:P434 \"b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d\" } LIMIT 1"#
            ),
            "{shown}"
        );
    }
}
