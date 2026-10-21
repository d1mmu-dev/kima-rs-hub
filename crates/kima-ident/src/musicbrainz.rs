//! The MusicBrainz lookups identification relies on (spec §8.3; upstream `musicbrainz.ts`), over
//! [`kima_net::Net`]. Every request adds `fmt=json`; entity lookups (`/release-group/{id}`,
//! `/release/{id}`, `/recording/{id}`, `/artist/{id}`, `/isrc/{code}`) read a 404 as `Absent`,
//! searches and browses never do, so one URL is always read one way.
//!
//! Answers are untrusted: every field but an entity's `id` is optional or defaulted, a field of the
//! wrong type reads as missing, and an item of a list that does not parse (a bad id, a stray
//! string) is dropped rather than failing the whole answer. Body size is capped by `Net`.

use std::sync::LazyLock;

use kima_net::{Evidence, Host, Net, Outcome, Req};
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::Mbid;
use crate::text::{self, Candidate, Expected};

// ---- lenient field readers ----

fn lenient_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|i| serde_json::from_value(i).ok())
            .collect(),
        _ => Vec::new(),
    })
}

fn lenient_text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        _ => String::new(),
    })
}

fn lenient_opt_text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => Some(s),
        _ => None,
    })
}

/// A non-negative whole number, as a number or a numeric string; larger than `u32::MAX` is
/// `u32::MAX`; anything else (null, negative, fractional, junk) is `None`.
fn number(v: &Value) -> Option<u32> {
    let n = match v {
        Value::Number(n) => n.as_u64()?,
        Value::String(s) => s.trim().parse::<u64>().ok()?,
        _ => return None,
    };
    Some(u32::try_from(n).unwrap_or(u32::MAX))
}

fn lenient_opt_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(number(&Value::deserialize(d)?))
}

fn lenient_u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    Ok(number(&Value::deserialize(d)?).unwrap_or(0))
}

// ---- types (MusicBrainz's kebab-case field names) ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CreditArtist {
    pub id: Mbid,
    #[serde(default, deserialize_with = "lenient_text")]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_text")]
    pub sort_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Credit {
    #[serde(default, deserialize_with = "lenient_text")]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_text")]
    pub joinphrase: String,
    pub artist: CreditArtist,
}

/// A curated genre or a crowd tag with its vote count. A nameless entry is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Genre {
    pub name: String,
    #[serde(default, deserialize_with = "lenient_u32")]
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Recording {
    pub id: Mbid,
    #[serde(default, deserialize_with = "lenient_text")]
    pub title: String,
    /// Milliseconds.
    #[serde(default, deserialize_with = "lenient_opt_u32")]
    pub length: Option<u32>,
    /// The search score, 0-100 (searches only).
    #[serde(default, deserialize_with = "lenient_opt_u32")]
    pub score: Option<u32>,
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub disambiguation: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub artist_credit: Vec<Credit>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub isrcs: Vec<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub genres: Vec<Genre>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub tags: Vec<Genre>,
}

/// MusicBrainz milliseconds to the whole seconds [`text::lengths_agree`] compares: rounded to the
/// nearest second, half up (`271_500` ms is 272 s). Computed in `u64`, so `u32::MAX` ms is
/// 4 294 967 s with no overflow. Upstream compares `ms / 1000` as a float (D-125).
pub fn ms_to_secs(ms: u32) -> i32 {
    let secs = (u64::from(ms) + 500) / 1000;
    i32::try_from(secs).unwrap_or(i32::MAX)
}

impl Recording {
    /// The recording as a matching candidate (upstream `toCandidate`).
    pub fn candidate(&self) -> Candidate {
        Candidate {
            id: self.id,
            title: self.title.clone(),
            length_secs: self.length.map(ms_to_secs),
            score: self.score,
            credit_names: credit_names(&self.artist_credit),
            isrcs: self.isrcs.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleaseSummary {
    pub id: Mbid,
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub date: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleaseGroup {
    pub id: Mbid,
    #[serde(default, deserialize_with = "lenient_text")]
    pub title: String,
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub primary_type: Option<String>,
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub first_release_date: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub releases: Vec<ReleaseSummary>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub genres: Vec<Genre>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Label {
    #[serde(default, deserialize_with = "lenient_text")]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LabelInfo {
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub catalog_number: Option<String>,
    #[serde(default)]
    pub label: Option<Label>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Track {
    pub id: Mbid,
    #[serde(default, deserialize_with = "lenient_u32")]
    pub position: u32,
    #[serde(default, deserialize_with = "lenient_text")]
    pub title: String,
    #[serde(default, deserialize_with = "lenient_opt_u32")]
    pub length: Option<u32>,
    /// The track's own credit; empty means the recording's applies.
    #[serde(default, deserialize_with = "lenient_vec")]
    pub artist_credit: Vec<Credit>,
    pub recording: Recording,
}

impl Track {
    /// The track's credit names, else the recording's (upstream
    /// `creditNames(t["artist-credit"] || t.recording["artist-credit"])`).
    pub fn credit_names(&self) -> Vec<String> {
        if self.artist_credit.is_empty() {
            credit_names(&self.recording.artist_credit)
        } else {
            credit_names(&self.artist_credit)
        }
    }

    /// The tracklist row as a candidate: the recording's id, length and ISRCs, the track's title
    /// (else the recording's) and credit names (`getAlbumCatalogue`).
    pub fn candidate(&self) -> Candidate {
        let mut c = self.recording.candidate();
        if !self.title.is_empty() {
            c.title = self.title.clone();
        }
        c.credit_names = self.credit_names();
        c
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Medium {
    #[serde(default, deserialize_with = "lenient_u32")]
    pub position: u32,
    #[serde(default, deserialize_with = "lenient_u32")]
    pub track_count: u32,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Release {
    pub id: Mbid,
    #[serde(default, deserialize_with = "lenient_text")]
    pub title: String,
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub date: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub label_info: Vec<LabelInfo>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub media: Vec<Medium>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub genres: Vec<Genre>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub artist_credit: Vec<Credit>,
}

impl Release {
    /// Every track of every medium, in order.
    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.media.iter().flat_map(|m| m.tracks.iter())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Alias {
    #[serde(default, deserialize_with = "lenient_text")]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_text")]
    pub sort_name: String,
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub locale: Option<String>,
    #[serde(default, rename = "type", deserialize_with = "lenient_opt_text")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Artist {
    pub id: Mbid,
    #[serde(default, deserialize_with = "lenient_text")]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_text")]
    pub sort_name: String,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub aliases: Vec<Alias>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub genres: Vec<Genre>,
}

/// A release group with the release that stands for it, tracklist included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalogue {
    pub release_group: ReleaseGroup,
    pub release: Release,
}

#[derive(Deserialize)]
struct Recordings {
    #[serde(default, deserialize_with = "lenient_vec")]
    recordings: Vec<Recording>,
}

#[derive(Deserialize)]
struct Artists {
    #[serde(default, deserialize_with = "lenient_vec")]
    artists: Vec<Artist>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ReleaseGroups {
    #[serde(default, deserialize_with = "lenient_vec")]
    release_groups: Vec<ReleaseGroup>,
    #[serde(default, deserialize_with = "lenient_u32")]
    release_group_count: u32,
}

// ---- pure helpers ----

/// Every name a credit gives: each credit's `name`, `artist.name` and `artist.sort-name`, then the
/// whole credit line (`name` + `joinphrase`, trimmed); empty names dropped, repeats kept once
/// (`creditNames`, `musicbrainz.ts:24-31`).
pub fn credit_names(credits: &[Credit]) -> Vec<String> {
    let line: String = credits
        .iter()
        .map(|c| format!("{}{}", c.name, c.joinphrase))
        .collect();
    let mut out: Vec<String> = Vec::new();
    let names = credits
        .iter()
        .flat_map(|c| [&c.name, &c.artist.name, &c.artist.sort_name])
        .map(String::as_str)
        .chain(std::iter::once(line.trim()));
    for n in names {
        if !n.is_empty() && !out.iter().any(|o| o == n) {
            out.push(n.to_string());
        }
    }
    out
}

/// The `n` most-voted names: entries with no votes dropped (upstream `voteCounts`), then by
/// `count` descending, ties by name.
pub fn top_genres(gs: &[Genre], n: usize) -> Vec<String> {
    let mut voted: Vec<&Genre> = gs.iter().filter(|g| g.count > 0).collect();
    voted.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    voted.into_iter().take(n).map(|g| g.name.clone()).collect()
}

/// The release that stands for its group: official first, then the earliest date (a date of
/// only a year or a month is padded with `-99-99`, so it sorts after the full dates within it;
/// none is `9999`), then id ascending (`chooseRelease`, `musicbrainz.ts:40-47`).
pub fn choose_release(rs: &[ReleaseSummary]) -> Option<&ReleaseSummary> {
    let sortable = |d: &Option<String>| -> String {
        let d = d.as_deref().filter(|d| !d.is_empty()).unwrap_or("9999");
        format!("{d}-99-99").chars().take(10).collect()
    };
    rs.iter().min_by(|a, b| {
        let official = |r: &ReleaseSummary| r.status.as_deref() != Some("Official");
        official(a)
            .cmp(&official(b))
            .then_with(|| sortable(&a.date).cmp(&sortable(&b.date)))
            .then_with(|| a.id.cmp(&b.id))
    })
}

static OTHER_VERSION_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)(live|demo|acoustic|remix)(?-u:\b)").expect("regex"));
static DATED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9]{4}[-‐][0-9]{2}[-‐][0-9]{2}").expect("regex"));

/// A disambiguation that marks another version of a song: live, a demo, acoustic, a remix, or a
/// dated performance (`isOtherVersion`, `musicbrainz.ts:49-55`). Upstream uses it only in the
/// album-by-name search (`searchRecording`); [`identify_recording`] never applies it.
pub fn is_other_version(disambiguation: &str) -> bool {
    let text = disambiguation.to_lowercase();
    OTHER_VERSION_WORD.is_match(&text) || DATED.is_match(&text)
}

/// The exact then fuzzy name queries (`recordingQueries`, `musicbrainz.ts:540-545`); none when
/// the normalised title is empty. Lucene-escaped here; `Req` percent-encodes once when sending.
fn recording_queries(title: &str, artist: &str) -> Vec<String> {
    let t = text::escape_lucene(&text::normalize_for_search(title));
    let a = text::escape_lucene(&text::normalize_for_search(artist));
    if t.trim().is_empty() {
        return Vec::new();
    }
    vec![
        format!(r#"recording:"{t}" AND artist:"{a}""#),
        format!("{t} AND artist:{a}"),
    ]
}

fn absent<T>(evidence: Value) -> Outcome<T> {
    Outcome::Absent(Evidence(evidence))
}

/// A non-answer as another type's outcome. Only called on a non-`Answered` outcome.
fn retype<T, U>(o: Outcome<T>) -> Outcome<U> {
    match o {
        Outcome::Answered(_) => unreachable!("retype is never given an answer"),
        Outcome::Absent(e) => Outcome::Absent(e),
        Outcome::RateLimited { until } => Outcome::RateLimited { until },
        Outcome::Transient(f) => Outcome::Transient(f),
        Outcome::Permanent(f) => Outcome::Permanent(f),
    }
}

fn is_400<T>(o: &Outcome<T>) -> bool {
    matches!(o, Outcome::Permanent(f) if f.code == "HTTP_400")
}

/// `GET <path>?<query…>&fmt=json`.
fn get(path: &str, query: &[(&str, &str)]) -> Req {
    query
        .iter()
        .fold(Req::get(Host::MusicBrainz, path), |r, (k, v)| {
            r.query(k, *v)
        })
        .query("fmt", "json")
}

// ---- requests ----

/// The release group and the release that stands for it, with its tracklist:
/// `GET /release-group/{rg}?inc=releases+genres`, then
/// `GET /release/{id}?inc=recordings+isrcs+labels+genres+artist-credits` for `tagged_release`
/// when given (D-117), else for [`choose_release`]. A group with no releases or a release with no
/// tracks is `Absent`.
pub async fn catalogue(net: &Net, rg: Mbid, tagged_release: Option<Mbid>) -> Outcome<Catalogue> {
    let group: ReleaseGroup = match net
        .get_json(
            get(
                &format!("/release-group/{rg}"),
                &[("inc", "releases+genres")],
            )
            .absent_on_404(),
        )
        .await
    {
        Outcome::Answered(g) => g,
        other => return retype(other),
    };
    let release_id = match tagged_release.or_else(|| choose_release(&group.releases).map(|r| r.id))
    {
        Some(id) => id,
        None => {
            return absent(json!({"rgMbid": rg, "reason": "release group has no releases"}));
        }
    };
    let release: Release = match net
        .get_json(
            get(
                &format!("/release/{release_id}"),
                &[("inc", "recordings+isrcs+labels+genres+artist-credits")],
            )
            .absent_on_404(),
        )
        .await
    {
        Outcome::Answered(r) => r,
        other => return retype(other),
    };
    if release.tracks().next().is_none() {
        return absent(json!({
            "rgMbid": rg, "releaseMbid": release_id, "reason": "release has no tracklist",
        }));
    }
    Outcome::Answered(Catalogue {
        release_group: group,
        release,
    })
}

/// Every recording MusicBrainz files under an ISRC: `GET /isrc/{code}?inc=artist-credits`. A
/// value that is not an ISRC (`kima_library::tags::normalize_isrc`) asks nothing; a 400, a 404
/// and zero recordings are `Absent` (`lookupRecordingsByIsrc`, `musicbrainz.ts:839-857`).
pub async fn isrc_recordings(net: &Net, isrc: &str) -> Outcome<Vec<Recording>> {
    let Some(code) = kima_library::tags::normalize_isrc(isrc) else {
        return absent(json!({"isrc": isrc, "reason": "not an ISRC"}));
    };
    let out: Outcome<Recordings> = net
        .get_json(get(&format!("/isrc/{code}"), &[("inc", "artist-credits")]).absent_on_404())
        .await;
    if is_400(&out) {
        return absent(json!({"isrc": code, "reason": "MusicBrainz refused the code"}));
    }
    match out {
        Outcome::Answered(r) if r.recordings.is_empty() => absent(json!({"isrc": code, "hits": 0})),
        other => other.map(|r| r.recordings),
    }
}

async fn search_one(net: &Net, query: &str) -> Outcome<Vec<Recording>> {
    net.get_json::<Recordings>(get("/recording", &[("query", query), ("limit", "50")]))
        .await
        .map(|r| r.recordings)
}

/// The two name searches in turn (`GET /recording?query=…&limit=50`): the first that has hits
/// answers. A 400 tries the next query; a rate limit stops at once; another failure is kept and
/// answered when no query had hits. An empty normalised title asks nothing.
pub async fn search_recordings(net: &Net, title: &str, artist: &str) -> Outcome<Vec<Recording>> {
    let queries = recording_queries(title, artist);
    if queries.is_empty() {
        return absent(json!({"title": title, "artist": artist, "reason": "nothing to search by"}));
    }
    let mut failure: Option<Outcome<Vec<Recording>>> = None;
    for q in &queries {
        match search_one(net, q).await {
            Outcome::Answered(hits) if !hits.is_empty() => return Outcome::Answered(hits),
            Outcome::Answered(_) | Outcome::Absent(_) => {}
            o if is_400(&o) => {}
            o @ Outcome::RateLimited { .. } => return o,
            o => {
                failure.get_or_insert(o);
            }
        }
    }
    failure.unwrap_or_else(|| {
        absent(json!({"title": title, "artist": artist, "hits": 0, "reason": "no recordings"}))
    })
}

/// The recording a track is, by name (`identifyRecording`, `musicbrainz.ts:865-897`): per query,
/// `pick_recording` over all its hits, the first pick wins. A hit's disambiguation is not
/// consulted: MusicBrainz marks a live recording only there, so filtering on it would make the
/// right live recording unfindable (spec §8.3 rung 4). Failures as [`search_recordings`].
pub async fn identify_recording(net: &Net, e: &Expected) -> Outcome<Recording> {
    let artist = e.artists.first().map_or("", String::as_str);
    let queries = recording_queries(&e.title, artist);
    if queries.is_empty() {
        return absent(
            json!({"title": e.title, "artist": artist, "reason": "nothing to search by"}),
        );
    }
    let mut failure: Option<Outcome<Recording>> = None;
    let mut offered = 0usize;
    for q in &queries {
        match search_one(net, q).await {
            Outcome::Answered(hits) => {
                offered += hits.len();
                let candidates: Vec<Candidate> = hits.iter().map(Recording::candidate).collect();
                if let Some(pick) = text::pick_recording(&candidates, e) {
                    let at = candidates.iter().position(|c| std::ptr::eq(c, pick));
                    if let Some(i) = at {
                        return Outcome::Answered(hits[i].clone());
                    }
                }
            }
            Outcome::Absent(_) => {}
            o if is_400(&o) => {}
            o @ Outcome::RateLimited { .. } => return retype(o),
            o => {
                failure.get_or_insert(retype(o));
            }
        }
    }
    failure.unwrap_or_else(|| {
        absent(json!({
            "title": e.title, "artist": artist, "offered": offered,
            "reason": "no candidate is this track",
        }))
    })
}

/// A recording's ISRCs, genres and tags from one request:
/// `GET /recording/{id}?inc=isrcs+genres+tags` (`getRecordingIsrcAndGenres`, `:901`). A second
/// call is served from the response cache.
pub async fn recording_isrcs_and_genres(net: &Net, id: Mbid) -> Outcome<Recording> {
    net.get_json(get(&format!("/recording/{id}"), &[("inc", "isrcs+genres+tags")]).absent_on_404())
        .await
}

/// What [`artist`] asks MusicBrainz to include.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistInc {
    Aliases,
    Genres,
}

impl ArtistInc {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtistInc::Aliases => "aliases",
            ArtistInc::Genres => "genres",
        }
    }
}

/// `GET /artist/{id}?inc=aliases` or `?inc=genres`.
pub async fn artist(net: &Net, id: Mbid, inc: ArtistInc) -> Outcome<Artist> {
    net.get_json(get(&format!("/artist/{id}"), &[("inc", inc.as_str())]).absent_on_404())
        .await
}

/// `GET /artist?query=<q>&limit=10`. `query` is a Lucene query the caller has escaped.
pub async fn search_artists(net: &Net, query: &str) -> Outcome<Vec<Artist>> {
    net.get_json::<Artists>(get("/artist", &[("query", query), ("limit", "10")]))
        .await
        .map(|a| a.artists)
}

/// One page of an artist's albums, EPs and singles, with MusicBrainz's total:
/// `GET /release-group?artist=<id>&type=album|ep|single&limit=100&offset=<n>`.
pub async fn artist_release_groups(
    net: &Net,
    artist: Mbid,
    offset: u32,
) -> Outcome<(Vec<ReleaseGroup>, u32)> {
    let artist = artist.to_string();
    let offset = offset.to_string();
    net.get_json::<ReleaseGroups>(get(
        "/release-group",
        &[
            ("artist", &artist),
            ("type", "album|ep|single"),
            ("limit", "100"),
            ("offset", &offset),
        ],
    ))
    .await
    .map(|p| (p.release_groups, p.release_group_count))
}

/// The most pages [`all_artist_release_groups`] reads (10 000 groups), whatever the count says.
pub const MAX_RELEASE_GROUP_PAGES: u32 = 100;

/// Every album, EP and single of an artist, paged 100 at a time to the end: until a page is
/// empty or the total is reached (`getAllReleaseGroups`, `musicbrainz.ts:329-354`). None is
/// `Absent`; any failed page is the answer.
pub async fn all_artist_release_groups(net: &Net, artist: Mbid) -> Outcome<Vec<ReleaseGroup>> {
    let mut groups = Vec::new();
    for page in 0..MAX_RELEASE_GROUP_PAGES {
        match artist_release_groups(net, artist, page * 100).await {
            Outcome::Answered((items, total)) => {
                let empty = items.is_empty();
                groups.extend(items);
                if empty || groups.len() >= total as usize {
                    break;
                }
            }
            other => return retype(other),
        }
    }
    if groups.is_empty() {
        return absent(json!({"artistMbid": artist, "hits": 0}));
    }
    Outcome::Answered(groups)
}
