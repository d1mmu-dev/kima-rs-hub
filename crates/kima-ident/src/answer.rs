//! What the enrichment source clients share: reading untrusted answers and passing an outcome
//! that is not an answer or an absence on unchanged.

use kima_net::{Evidence, Outcome};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// The longest name kept from an answer (characters); longer names are cut.
pub(crate) const MAX_NAME_CHARS: usize = 500;

/// The longest URL taken from an answer (characters); a longer one is dropped, not cut, since a
/// cut URL points somewhere else.
pub(crate) const MAX_URL_CHARS: usize = 2048;

/// The two outcomes a client reads; anything else is passed on.
pub(crate) enum Got<T> {
    Answered(T),
    Absent(Evidence),
}

/// `Ok` for an answer or an absence, else the outcome (rate-limited, transient, permanent),
/// retyped for the caller to return.
pub(crate) fn got<T, U>(o: Outcome<T>) -> Result<Got<T>, Outcome<U>> {
    match o {
        Outcome::Answered(t) => Ok(Got::Answered(t)),
        Outcome::Absent(e) => Ok(Got::Absent(e)),
        Outcome::RateLimited { until } => Err(Outcome::RateLimited { until }),
        Outcome::Transient(f) => Err(Outcome::Transient(f)),
        Outcome::Permanent(f) => Err(Outcome::Permanent(f)),
    }
}

/// The first `max` characters of `s` (always on a character boundary).
pub(crate) fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// A string that is not empty once trimmed, trimmed and cut to `max` characters.
pub(crate) fn text(v: Option<&Value>, max: usize) -> Option<String> {
    let s = v?.as_str()?.trim();
    (!s.is_empty()).then(|| cut(s, max))
}

/// A URL-ish string: not empty once trimmed and at most [`MAX_URL_CHARS`]. It is only ever
/// returned as text; nothing here fetches it.
pub(crate) fn url(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.trim();
    (!s.is_empty() && s.chars().count() <= MAX_URL_CHARS).then(|| s.to_string())
}

/// The array under `v`, or nothing when it is missing or not an array.
pub(crate) fn items(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// A field that must be an array (a missing one fails the answer).
pub(crate) fn array<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Value>, D::Error> {
    match Value::deserialize(d)? {
        Value::Array(items) => Ok(items),
        _ => Err(D::Error::custom("expected a list")),
    }
}
