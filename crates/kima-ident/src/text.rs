//! Upstream's text rules for identification (spec §8.2), ported function by function with
//! upstream's tests. They run on untrusted tag and MusicBrainz text: every regex is compiled once,
//! the `regex` crate is linear-time, and nothing slices strings by byte offset.
//!
//! JavaScript's `\s` and Postgres' whitespace class are spelled out ([`JS_S`], [`PG_S`]) rather
//! than using Rust's `\s` (Unicode `White_Space`), and words matched case-insensitively use ASCII
//! case folding (`(?i-u:...)`) as a JavaScript regex without the `u` flag does.

use kima_library::resolve::parse_credit;
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

use crate::Mbid;

/// JavaScript's `\s`.
const JS_S: &str = concat!(
    r"[\t\n\x0B\x0C\r \xA0\x{1680}\x{2000}-\x{200A}",
    r"\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]"
);
/// The whitespace class Postgres `\s` matches (upstream `pgTextRules.ts` `PG_SPACE`).
const PG_S: &str =
    r"[\t\n\x0B\x0C\r \x{1680}\x{2000}-\x{2006}\x{2008}-\x{200A}\x{2028}\x{2029}\x{205F}\x{3000}]";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{B}' | '\u{C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn is_pg_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{B}'
            | '\u{C}'
            | '\r'
            | ' '
            | '\u{1680}'
            | '\u{2000}'..='\u{2006}'
            | '\u{2008}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

fn pg_trim(s: &str) -> &str {
    s.trim_matches(is_pg_space)
}

static PG_SPACE_RUN: LazyLock<Regex> = LazyLock::new(|| re(&format!("{PG_S}+")));

fn pg_collapse_space(s: &str) -> String {
    PG_SPACE_RUN.replace_all(s, " ").into_owned()
}

/// Postgres `[^[:alnum:]]` removal. glibc's `alnum` is close to Unicode `Alphabetic` plus
/// numbers (it keeps Hebrew points and Indic vowel signs), so this keeps `char::is_alphanumeric`.
fn strip_non_alnum(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

static NON_SPACING_MARKS: LazyLock<Regex> = LazyLock::new(|| re(r"\p{Mn}+"));

/// Latin letters NFKD does not decompose (spec §8.2).
fn fold_letter(c: char) -> Option<&'static str> {
    Some(match c {
        'ß' => "ss",
        'æ' => "ae",
        'Æ' => "AE",
        'œ' => "oe",
        'Œ' => "OE",
        'ø' => "o",
        'Ø' => "O",
        'đ' => "d",
        'Đ' => "D",
        'ł' => "l",
        'Ł' => "L",
        'þ' => "th",
        'Þ' => "TH",
        'ð' => "d",
        'ı' => "i",
        'ĸ' => "k",
        'ŋ' => "n",
        'ŧ' => "t",
        'ﬁ' => "fi",
        'ﬂ' => "fl",
        _ => return None,
    })
}

/// Accent and compatibility folding for identity keys: NFKD, drop non-spacing marks, NFKC, then
/// the 20-letter table. Stands in for upstream's 1613-entry `unaccent` table (D-106).
pub fn fold_identity(s: &str) -> String {
    let decomposed: String = s.nfkd().collect();
    let stripped = NON_SPACING_MARKS.replace_all(&decomposed, "");
    let mut out = String::with_capacity(stripped.len());
    for c in stripped.nfkc() {
        match fold_letter(c) {
            Some(f) => out.push_str(f),
            None => out.push(c),
        }
    }
    out
}

static AMPERSAND: LazyLock<Regex> = LazyLock::new(|| re(&format!("{JS_S}*&{JS_S}*")));

/// Case and accent-insensitive artist name that keeps word boundaries (`artistIdentity.ts:45-56`).
pub fn normalize_artist_name(s: &str) -> String {
    let lowered = fold_identity(pg_trim(s)).to_lowercase();
    let anded = AMPERSAND.replace_all(&lowered, " and ");
    pg_trim(&pg_collapse_space(&anded)).to_string()
}

/// The artist dedupe key: [`normalize_artist_name`] with every non-letter/number removed
/// (`artistIdentity.ts:63-65`). "Dead Mau5" → "deadmau5".
pub fn artist_identity_key(s: &str) -> String {
    strip_non_alnum(&normalize_artist_name(s))
}

static LEADING_ARTICLE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        "^(?i-u:the|a|an|le|la|les|los|las|die|der|das){PG_S}+"
    ))
});

/// Article-stripped, folded, lower-cased sort value (`artistIdentity.ts:artistSortName`).
/// "The Beatles" → "beatles"; an article that is the whole name stays.
pub fn artist_sort_name(s: &str) -> String {
    let base = pg_collapse_space(&fold_identity(pg_trim(s)).to_lowercase());
    let stripped = pg_trim(&LEADING_ARTICLE.replace(&base, "")).to_string();
    if stripped.is_empty() { base } else { stripped }
}

/// The album dedupe key within its artist (`albumIdentity.ts:albumIdentityKey`): case,
/// punctuation and accents fold, edition markers stay.
pub fn album_identity_key(s: &str) -> String {
    strip_non_alnum(&fold_identity(pg_trim(s)).to_lowercase())
}

/// `albumIdentity.ts:19-36`, verbatim.
const GENERIC_TITLES: [&str; 16] = [
    "unknown album",
    "unknown",
    "untitled",
    "greatest hits",
    "the greatest hits",
    "best of",
    "the best of",
    "hits",
    "singles",
    "demos",
    "live",
    "ep",
    "compilation",
    "various",
    "self titled",
    "s/t",
];

/// Whether a title is too generic to identify a release group across artists
/// (`albumIdentity.ts:isGenericAlbumTitle`). A title with an empty key is generic.
pub fn is_generic_album_title(s: &str) -> bool {
    let lowered = fold_identity(pg_trim(s)).to_lowercase();
    let normalised = pg_collapse_space(&lowered);
    let normalised = pg_trim(&normalised);
    normalised.is_empty()
        || album_identity_key(s).is_empty()
        || GENERIC_TITLES.contains(&normalised)
}

static LATIN_MARKS: LazyLock<Regex> = LazyLock::new(|| re(r"(\p{Script=Latin})\p{M}+"));
static NOT_LMN: LazyLock<Regex> = LazyLock::new(|| re(r"[^\p{L}\p{M}\p{N}]"));

/// Letters, marks and numbers of any script, lower-cased, with only the marks on Latin letters
/// dropped (`albumIdentity.ts:foldTitle`): "Café" folds to "cafe", a dakuten or a Devanagari vowel
/// sign stays.
pub fn fold_title(s: &str) -> String {
    let decomposed: String = s.nfkd().collect();
    let unmarked = LATIN_MARKS.replace_all(&decomposed, "$1");
    let composed: String = unmarked.nfkc().collect();
    NOT_LMN
        .replace_all(&composed.to_lowercase(), "")
        .into_owned()
}

static PARENS: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"{JS_S}*\([^)]*\){JS_S}*")));
static BRACKETS: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"{JS_S}*\[[^\]]*\]{JS_S}*")));
static EDITION_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        "{JS_S}*-{JS_S}*([0-9]{{4}}{JS_S}+)?\
         (?i-u:(deluxe|remastered|remaster|edition|version|expanded|bonus|explicit|clean|single|\
         radio edit|remix|acoustic|live|mono|stereo))\
         ({JS_S}+[0-9]{{4}})?{JS_S}*(?i-u:(edition|version|mix))?{JS_S}*"
    ))
});
static YEAR_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| re(&format!("{JS_S}*-{JS_S}*[0-9]{{4}}{JS_S}*$")));
static JS_SPACE_RUN: LazyLock<Regex> = LazyLock::new(|| re(&format!("{JS_S}+")));

/// The title without edition markers, for matching only (`musicbrainz.ts:555-568`).
pub fn normalize_for_search(s: &str) -> String {
    let s = PARENS.replace_all(s, " ");
    let s = BRACKETS.replace_all(&s, " ");
    let s = EDITION_SUFFIX.replace_all(&s, " ");
    let s = YEAR_SUFFIX.replace_all(&s, " ");
    let s = JS_SPACE_RUN.replace_all(&s, " ");
    s.trim_matches(is_js_space).to_string()
}

/// Whether two titles name the same record (`albumIdentity.ts:195-204`): identical text, or equal
/// after [`fold_title`], or equal without edition markers. An empty fold never agrees.
pub fn titles_agree(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let exact = |t: &str| {
        let n: String = t.nfkc().collect();
        n.to_lowercase().trim_matches(is_js_space).to_string()
    };
    let (ea, eb) = (exact(a), exact(b));
    if !ea.is_empty() && ea == eb {
        return true;
    }
    let same = |x: String, y: String| !x.is_empty() && x == y;
    let bare = |t: &str| fold_title(&normalize_for_search(t));
    same(fold_title(a), fold_title(b)) || same(bare(a), bare(b))
}

const VERSION_WORDS: [&str; 15] = [
    "remix",
    "mix",
    "dub",
    "edit",
    "live",
    "acoustic",
    "demo",
    "instrumental",
    "reprise",
    "rework",
    "unplugged",
    "session",
    "version",
    "karaoke",
    "cover",
];

static NOT_LN_RUN: LazyLock<Regex> = LazyLock::new(|| re(r"[^\p{L}\p{N}]+"));

/// The version words a title carries (`recordingMatch.ts:42-51`), so two versions differ.
pub fn version_of(title: &str) -> BTreeSet<&'static str> {
    let folded: String = title.to_lowercase().nfkc().collect();
    NOT_LN_RUN
        .split(&folded)
        .filter_map(|w| VERSION_WORDS.iter().copied().find(|v| *v == w))
        .collect()
}

/// The most two lengths of one recording may differ (`recordingMatch.ts:40`).
pub const LENGTH_TOLERANCE_SEC: i64 = 5;

/// `Some(true)` when both lengths are known and within [`LENGTH_TOLERANCE_SEC`], `Some(false)`
/// when both are known and differ, `None` when either is unknown or not positive (`:54-57`).
pub fn lengths_agree(a: Option<i32>, b: Option<i32>) -> Option<bool> {
    let a = a.filter(|v| *v > 0)?;
    let b = b.filter(|v| *v > 0)?;
    Some((i64::from(a) - i64::from(b)).abs() <= LENGTH_TOLERANCE_SEC)
}

fn name_keys(name: &str) -> impl Iterator<Item = String> {
    [
        artist_identity_key(name),
        artist_identity_key(&artist_sort_name(name)),
    ]
    .into_iter()
    .filter(|k| !k.is_empty())
}

/// Whether any locally credited name is one of the candidate's credited names, by identity key
/// or the key of its sort name (`recordingMatch.ts:59-69`).
pub fn artists_agree(local: &[String], credits: &[String]) -> bool {
    let offered: std::collections::HashSet<String> =
        credits.iter().flat_map(|c| name_keys(c)).collect();
    local
        .iter()
        .any(|n| name_keys(n).any(|k| offered.contains(&k)))
}

/// What the track says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub title: String,
    /// Every local name: see [`local_names`].
    pub artists: Vec<String>,
    pub length_secs: Option<i32>,
    pub isrc: Option<String>,
}

/// A recording a source offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: Mbid,
    pub title: String,
    pub length_secs: Option<i32>,
    /// The source's own relevance score, higher is better.
    pub score: Option<u32>,
    /// Each credit's name, artist name and sort name, plus the whole credit line.
    pub credit_names: Vec<String>,
    pub isrcs: Vec<String>,
}

/// Title, version and artist agree, and the lengths do not disagree (`recordingMatch.ts:72-79`).
pub fn recording_agrees(c: &Candidate, e: &Expected) -> bool {
    titles_agree(&c.title, &e.title)
        && version_of(&c.title) == version_of(&e.title)
        && artists_agree(&e.artists, &c.credit_names)
        && lengths_agree(e.length_secs, c.length_secs) != Some(false)
}

/// The agreeing candidate with a known length first, then the higher score, then the smaller
/// length gap; the earliest wins a tie (`recordingMatch.ts:86-103`).
pub fn pick_recording<'a>(cs: &'a [Candidate], e: &Expected) -> Option<&'a Candidate> {
    let gap = |c: &Candidate| match (c.length_secs, e.length_secs) {
        (Some(a), Some(b)) => Some((i64::from(a) - i64::from(b)).abs()),
        _ => None,
    };
    cs.iter()
        .filter(|c| recording_agrees(c, e))
        .min_by_key(|c| {
            let g = gap(c);
            (
                g.is_none(),
                std::cmp::Reverse(c.score.unwrap_or(0)),
                g.unwrap_or(0),
            )
        })
}

/// The names a track is credited to locally: the track artist split by `parse_credit` (primary,
/// then featured), then the album artist; empty names dropped, duplicates kept once.
pub fn local_names(track_artist: &str, album_artist: &str) -> Vec<String> {
    let credit = parse_credit(track_artist, None);
    let mut out: Vec<String> = Vec::new();
    let all = std::iter::once(credit.primary)
        .chain(credit.featured)
        .chain(std::iter::once(album_artist.trim().to_string()));
    for name in all {
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Backslash-escapes Lucene's special characters (`musicbrainz.ts:547-549`).
pub fn escape_lucene(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '+' | '-'
                | '&'
                | '|'
                | '!'
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | '^'
                | '"'
                | '~'
                | '*'
                | '?'
                | ':'
                | '\\'
                | '/'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `utils/normalization.ts:normalizeString`: lower-case, NFD, drop U+0300–U+036F, keep only
/// JavaScript `\w` (ASCII letters, digits, `_`) and `\s`, collapse spaces, trim.
pub fn normalize_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.to_lowercase().nfd() {
        if ('\u{300}'..='\u{36F}').contains(&c) {
            continue;
        }
        if is_js_space(c) {
            pending_space = true;
        } else if c.is_ascii_alphanumeric() || c == '_' {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
    out
}

static FEAT_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"{JS_S}*[\[(](?i-u:feat|ft|featuring|with)\.?{JS_S}+[^\])]*[\])]"
    ))
});
static EDITION_BRACKET: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        "{JS_S}*[\\[(](?:[0-9]{{4}} )?\
         (?i-u:remaster|remastered|deluxe|expanded|anniversary|bonus|special|mono|stereo)\
         (?:{JS_S}+(?i-u:edition))?[^\\])]*[\\])]"
    ))
});
static DASH_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        "{JS_S}*-{JS_S}*(?:[0-9]{{4}} )?(?i-u:remaster|remastered|single|live|acoustic)\
         (?:{JS_S}+(?i-u:version))?$"
    ))
});

/// Collaboration and edition noise stripped from an artist name before a search
/// (`artistCorrection.ts:stripArtistSuffixes`). Fewer than two UTF-16 units left keeps the
/// trimmed original, as upstream measures `length`.
pub fn strip_artist_suffixes(s: &str) -> String {
    let c = FEAT_SUFFIX.replace_all(s, "");
    let c = EDITION_BRACKET.replace_all(&c, "");
    let c = DASH_SUFFIX.replace(&c, "");
    let cleaned = c.trim_matches(is_js_space);
    if cleaned.encode_utf16().count() < 2 {
        s.trim_matches(is_js_space).to_string()
    } else {
        cleaned.to_string()
    }
}

/// `deezer.ts:namesAgree`: equal after [`normalize_string`]; when either folds to nothing, only
/// identical text agrees.
pub fn names_agree(found: &str, wanted: &str) -> bool {
    let a = normalize_string(found);
    let b = normalize_string(wanted);
    if a.is_empty() || b.is_empty() {
        return found == wanted;
    }
    a == b
}
