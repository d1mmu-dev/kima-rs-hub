//! Deezer: an artist's picture and related artists, and an album cover (spec §12.3, §12.5,
//! §12.7; upstream `deezer.ts`).
//!
//! Deezer ranks search hits by popularity, so a hit counts only when its names agree with the
//! query under [`names_agree`] (`deezer.ts:namesAgree`): never a first-hit fallback. Deezer
//! answers its own errors (a quota, say) with HTTP 200 and an `error` object; such an answer
//! fails to read, so it is `Transient("BAD_RESPONSE")` and never cached. An empty search is an
//! absence (spec §7.4). Picture and cover URLs are returned as text and never fetched here.

use kima_net::{Host, Net, Outcome, Req};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::answer::{self, Got, MAX_NAME_CHARS, array};
use crate::text::names_agree;

/// The most related artists kept (spec §12.5).
pub const MAX_RELATED: usize = 30;

/// The most search hits looked at.
const MAX_HITS: usize = 50;

/// An artist Deezer's search verified: its id, its name and its largest picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeezerArtist {
    pub id: u64,
    pub name: String,
    /// `picture_xl || picture_big || picture_medium`.
    pub picture: Option<String>,
}

/// One page of a Deezer list. An `error` object fails it.
#[derive(Deserialize)]
struct Page {
    #[serde(rename = "error", default, deserialize_with = "deezer_error")]
    _error: (),
    #[serde(deserialize_with = "array")]
    data: Vec<Value>,
}

/// Called only when `error` is present: Deezer's 200-with-an-error answer.
fn deezer_error<'de, D: Deserializer<'de>>(d: D) -> Result<(), D::Error> {
    let v = Value::deserialize(d)?;
    let code = v.get("code").and_then(Value::as_i64).unwrap_or(0);
    Err(D::Error::custom(format!(
        "Deezer answered error code {code}"
    )))
}

/// A successful search with no hits.
fn no_hits(v: &Value) -> bool {
    v.get("error").is_none()
        && v.get("data")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
}

fn search(path: &str, q: &str, limit: u32) -> Req {
    Req::get(Host::Deezer, path)
        .query("q", q)
        .query("limit", limit.to_string())
        .absent_when(no_hits)
}

/// The first of `keys` that holds a usable URL.
fn first_url(v: &Value, keys: [&str; 3]) -> Option<String> {
    keys.into_iter().find_map(|k| answer::url(v.get(k)))
}

/// Hits of a search, or none for an absence; failures are passed on.
async fn hits<U>(net: &Net, req: Req) -> Result<Vec<Value>, Outcome<U>> {
    match answer::got(net.get_json::<Page>(req).await)? {
        Got::Answered(page) => Ok(page.data),
        Got::Absent(_) => Ok(Vec::new()),
    }
}

/// `GET /search/artist?q=<name>&limit=1`; the hit only when its name agrees with `name`
/// (`getArtistImage`). A hit without a numeric id is no hit. An empty `name` asks nothing.
pub async fn find_artist(net: &Net, name: &str) -> Outcome<Option<DeezerArtist>> {
    if name.trim().is_empty() {
        return Outcome::Answered(None);
    }
    let hits = match hits(net, search("/search/artist", name, 1)).await {
        Ok(h) => h,
        Err(o) => return o,
    };
    let artist = hits.first().and_then(|hit| {
        let found = hit.get("name")?.as_str()?;
        if !names_agree(found, name) {
            return None;
        }
        Some(DeezerArtist {
            id: hit.get("id")?.as_u64()?,
            name: answer::cut(found, MAX_NAME_CHARS),
            picture: first_url(hit, ["picture_xl", "picture_big", "picture_medium"]),
        })
    });
    Outcome::Answered(artist)
}

/// `GET /artist/{id}/related?limit=30`: the names in Deezer's order, the artist itself left
/// out, at most [`MAX_RELATED`]. Empty when Deezer lists none.
pub async fn related(net: &Net, id: u64) -> Outcome<Vec<String>> {
    let req = Req::get(Host::Deezer, &format!("/artist/{id}/related"))
        .query("limit", MAX_RELATED.to_string());
    net.get_json::<Page>(req).await.map(|page| {
        page.data
            .iter()
            .filter(|a| a.get("id").and_then(Value::as_u64) != Some(id))
            .filter_map(|a| answer::text(a.get("name"), MAX_NAME_CHARS))
            .take(MAX_RELATED)
            .collect()
    })
}

/// Spec §12.7 step 1 (`getAlbumCover`): `/search/album?q=artist:"A" album:"T"&limit=5`, and
/// when that finds nothing `?q=A T`; the first hit whose artist and title both agree;
/// `cover_xl || cover_big || cover_medium`.
pub async fn album_cover(net: &Net, artist: &str, title: &str) -> Outcome<Option<String>> {
    if artist.trim().is_empty() || title.trim().is_empty() {
        return Outcome::Answered(None);
    }
    let structured = format!("artist:\"{artist}\" album:\"{title}\"");
    let mut found = match hits(net, search("/search/album", &structured, 5)).await {
        Ok(h) => h,
        Err(o) => return o,
    };
    if found.is_empty() {
        found = match hits(
            net,
            search("/search/album", &format!("{artist} {title}"), 5),
        )
        .await
        {
            Ok(h) => h,
            Err(o) => return o,
        };
    }
    let hit = found.iter().take(MAX_HITS).find(|a| {
        let agrees = |v: Option<&Value>, wanted: &str| {
            v.and_then(Value::as_str)
                .is_some_and(|found| names_agree(found, wanted))
        };
        agrees(a.pointer("/artist/name"), artist) && agrees(a.get("title"), title)
    });
    Outcome::Answered(hit.and_then(|a| first_url(a, ["cover_xl", "cover_big", "cover_medium"])))
}
