//! Beets-style string and track distance (spec §19.3). Pure functions: a distance is a weighted
//! mean of penalties in `0..=1`, and only the keys that were added count.

use crate::musicbrainz::{Credit, Track, ms_to_secs};
use crate::text::fold_identity;
use kima_db::autotag::LocalTrack;
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

pub const W_ALBUM: f64 = 3.0;
pub const W_ARTIST: f64 = 3.0;
pub const W_ALBUM_ID: f64 = 5.0;
pub const W_YEAR: f64 = 1.0;
pub const W_MEDIUMS: f64 = 1.0;
pub const W_TRACKS: f64 = 2.0;
pub const W_MISSING_TRACKS: f64 = 0.9;
pub const W_UNMATCHED_TRACKS: f64 = 0.6;
pub const W_TRACK_TITLE: f64 = 3.0;
pub const W_TRACK_ARTIST: f64 = 2.0;
pub const W_TRACK_INDEX: f64 = 1.0;
pub const W_TRACK_LENGTH: f64 = 2.0;
pub const W_TRACK_ID: f64 = 5.0;

/// Beets `track_length_grace` and `track_length_max`.
pub const LENGTH_GRACE_SECS: i32 = 10;
pub const LENGTH_MAX_SECS: i32 = 30;

/// Beets' `SD_PATTERNS`, in order, with their weights. Applied to lower-cased text.
static PATTERNS: LazyLock<Vec<(Regex, f64)>> = LazyLock::new(|| {
    [
        (r"^the ", 0.1),
        (r"[\[\(]?(ep|single)[\]\)]?", 0.0),
        (r"[\[\(]?(featuring|feat|ft)[\. :].+", 0.1),
        (r"\(.*?\)", 0.3),
        (r"\[.*?\]", 0.3),
        (r"(, )?(pt\.|part) .+", 0.2),
    ]
    .into_iter()
    .map(|(p, w)| (Regex::new(p).expect("static regex"), w))
    .collect()
});

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Everything but `\p{L}` and `\p{N}` (spec §19.3), so non-Latin text is compared, never emptied.
static NOT_LETTER_OR_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]").expect("static regex"));

fn basic(a: &str, b: &str) -> f64 {
    let a: Vec<char> = NOT_LETTER_OR_NUMBER.replace_all(a, "").chars().collect();
    let b: Vec<char> = NOT_LETTER_OR_NUMBER.replace_all(b, "").chars().collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    levenshtein(&a, &b) as f64 / longest as f64
}

/// `fold_identity`, except that kana keep their (semi-)voiced marks: NFKD splits `ば` into `は` and
/// U+3099, and folding would drop the mark so that `ば` and `は` compare equal.
fn fold_keeping_kana(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut buf = [0u8; 4];
    for c in s.chars() {
        if matches!(c, '\u{3040}'..='\u{30FF}') {
            out.push(c);
        } else {
            out.push_str(&fold_identity(c.encode_utf8(&mut buf)));
        }
    }
    out.nfc().collect()
}

fn prepare(s: &str) -> String {
    let s = fold_keeping_kana(&s.to_lowercase());
    let s = s.trim();
    let s = match s.strip_suffix(", the") {
        Some(rest) => format!("the {rest}"),
        None => s.to_string(),
    };
    s.replace('&', "and")
}

/// Distance between two strings in `0..=1` (beets `string_dist`, §19.3).
pub fn string_dist(a: &str, b: &str) -> f64 {
    let (mut a, mut b) = (prepare(a), prepare(b));
    let mut base = basic(&a, &b);
    for (pattern, weight) in PATTERNS.iter() {
        let a2 = pattern.replace_all(&a, "").into_owned();
        let b2 = pattern.replace_all(&b, "").into_owned();
        if a2 == a && b2 == b {
            continue;
        }
        let case = basic(&a2, &b2);
        let delta = (base - case).max(0.0);
        if delta == 0.0 {
            continue;
        }
        a = a2;
        b = b2;
        base = case + delta * weight;
    }
    base
}

/// Beets' `Distance`: weighted penalties, normalised over the keys that were added.
#[derive(Debug, Default, Clone)]
pub struct Distance {
    entries: Vec<(&'static str, f64, f64)>,
}

impl Distance {
    pub fn add(&mut self, key: &'static str, weight: f64, penalty: f64) {
        self.entries.push((key, weight, penalty));
    }

    fn total_weight(&self) -> f64 {
        self.entries.iter().map(|e| e.1).sum()
    }

    /// `Σ weight × penalty / Σ weight`; 0 when nothing was added.
    pub fn value(&self) -> f64 {
        let total = self.total_weight();
        if total <= 0.0 {
            return 0.0;
        }
        self.entries.iter().map(|e| e.1 * e.2).sum::<f64>() / total
    }

    /// Each key's share of [`Distance::value`]; the shares sum to it.
    pub fn shares(&self) -> BTreeMap<&'static str, f64> {
        let total = self.total_weight();
        let mut out = BTreeMap::new();
        if total <= 0.0 {
            return out;
        }
        for (key, weight, penalty) in &self.entries {
            *out.entry(*key).or_insert(0.0) += weight * penalty / total;
        }
        out
    }
}

/// 0 when the local year is the release or original year; else scaled by how far it is,
/// relative to the age of the original; 1 when there is no original year.
pub fn year_penalty(
    local: i32,
    release_year: i32,
    original_year: Option<i32>,
    this_year: i32,
) -> f64 {
    if local == release_year || original_year == Some(local) {
        return 0.0;
    }
    match original_year {
        Some(orig) => {
            let span = f64::from((this_year - orig).abs().max(1));
            (f64::from((local - release_year).abs()) / span).min(1.0)
        }
        None => 1.0,
    }
}

pub fn length_penalty(local_secs: i32, release_ms: u32) -> f64 {
    let diff = (local_secs - ms_to_secs(release_ms)).abs();
    (f64::from((diff - LENGTH_GRACE_SECS).max(0)) / f64::from(LENGTH_MAX_SECS)).min(1.0)
}

/// A release track with its index across the release (1-based, over all media); the track's own
/// `position` is its place on its medium.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseTrackRef<'a> {
    pub track: &'a Track,
    pub absolute_index: u32,
}

fn joined(credits: &[Credit]) -> String {
    credits
        .iter()
        .map(|c| format!("{}{}", c.name, c.joinphrase))
        .collect()
}

/// The distance between a local track and a release track (§19.3 track table).
pub fn track_distance(
    local: &LocalTrack,
    rt: &ReleaseTrackRef<'_>,
    release_credit: &str,
    release_is_va: bool,
) -> f64 {
    let track = rt.track;
    let mut d = Distance::default();
    d.add(
        "track_title",
        W_TRACK_TITLE,
        string_dist(&local.title, &track.title),
    );
    let credit = if track.artist_credit.is_empty() {
        joined(&track.recording.artist_credit)
    } else {
        joined(&track.artist_credit)
    };
    if release_is_va || credit.trim() != release_credit.trim() {
        d.add(
            "track_artist",
            W_TRACK_ARTIST,
            string_dist(&local.artist_name, &credit),
        );
    }
    if local.track_no > 0 {
        let no = u32::try_from(local.track_no).ok();
        let hit = no == Some(track.position) || no == Some(rt.absolute_index);
        d.add("track_index", W_TRACK_INDEX, if hit { 0.0 } else { 1.0 });
    }
    let release_ms = track
        .length
        .filter(|&ms| ms > 0)
        .or(track.recording.length.filter(|&ms| ms > 0));
    if let (true, Some(ms)) = (local.duration_secs > 0, release_ms) {
        d.add(
            "track_length",
            W_TRACK_LENGTH,
            length_penalty(local.duration_secs, ms),
        );
    }
    if let Some(id) = local.recording_mbid {
        let same = id == track.recording.id.0;
        d.add("track_id", W_TRACK_ID, if same { 0.0 } else { 1.0 });
    }
    d.value()
}
