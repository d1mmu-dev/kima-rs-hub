//! The AcoustID lookup (spec §8.4; upstream `acoustidService.ts`) over [`kima_net::Net`], the
//! fingerprint pick, and the client key order.
//!
//! The client key is a secret. It is only ever placed in the `client` pair of the form body,
//! which `Net` strips from cache keys and the cache's `request` column and scrubs from every
//! failure message and recording; nothing here logs, formats or stores it.
//!
//! Answers are untrusted: every field is optional or lenient (a score that is not a number reads
//! as NaN and is below the floor, a duration that is not a positive finite number reads as
//! unknown, a recording whose id is not an MBID is dropped), and the pick is total.

use kima_db::settings_kv::{KvSettings, SettingsCache};
use kima_net::{Evidence, Failure, Host, Net, Outcome, Req};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::Mbid;
use crate::text::{lengths_agree, titles_agree, version_of};

/// The built-in client key, last in the key order. Empty until a kima-rs application is
/// registered with AcoustID: upstream's key belongs to kima-hub's application (spec §27 Q1,
/// D-124), so without a setting or `ACOUSTID_CLIENT_KEY` there is no key and nothing is asked.
pub const DEFAULT_CLIENT_KEY: &str = "";

/// The confidence floor: below it AcoustID is effectively guessing (`acoustidService.ts`).
pub const MIN_SCORE: f64 = 0.5;

/// Why an answer picked nothing (upstream's absent evidence).
const NO_MATCH_REASON: &str =
    "no result above the confidence floor has a recording of this track's length";

/// The client key: the stored setting, then `env` (`ACOUSTID_CLIENT_KEY`), then
/// [`DEFAULT_CLIENT_KEY`]. A value that is empty once trimmed is no key.
pub fn client_key(kv: &KvSettings, env: Option<&str>) -> Option<String> {
    [
        kv.acoustid_api_key.as_deref(),
        env,
        Some(DEFAULT_CLIENT_KEY),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|k| !k.is_empty())
    .map(str::to_string)
}

/// [`client_key`] over the cached settings. A settings read that fails is treated as no stored
/// key, so the env key (or the default) is used and the failure never propagates (upstream
/// `resolveApiKey`).
pub async fn resolve_client_key(settings: &SettingsCache, env: Option<&str>) -> Option<String> {
    match settings.get().await {
        Ok(kv) => client_key(&kv, env),
        Err(e) => {
            // A database error; it cannot carry the key, which is only in the decrypted settings.
            tracing::debug!(error = %e, "settings read failed; using the env or default AcoustID key");
            client_key(&KvSettings::default(), env)
        }
    }
}

// ---- answer types ----

/// A recording an AcoustID result names. `duration` is in seconds, a positive finite number or
/// unknown.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AcoustidRecording {
    pub id: Mbid,
    #[serde(default, deserialize_with = "lenient_opt_text")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "lenient_duration")]
    pub duration: Option<f64>,
}

/// One result: an AcoustID track id, its score (NaN when missing or not a number) and its
/// recordings (`None` for a fingerprint-only match).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AcoustidResult {
    #[serde(default, deserialize_with = "lenient_text")]
    pub id: String,
    #[serde(default = "nan", deserialize_with = "lenient_score")]
    pub score: f64,
    #[serde(default, deserialize_with = "lenient_opt_vec")]
    pub recordings: Option<Vec<AcoustidRecording>>,
}

/// What [`pick_fingerprint_match`] chose.
#[derive(Debug, Clone, PartialEq)]
pub struct Picked {
    pub recording: Mbid,
    pub score: f64,
    pub acoustid_id: String,
}

impl Picked {
    /// The stage's evidence for this match, `{score, acoustidId}` (spec §8.4).
    pub fn evidence(&self) -> Value {
        json!({ "score": self.score, "acoustidId": self.acoustid_id })
    }
}

/// `/lookup`'s answer. Deserialising fails unless `status` is `"ok"`, so an error answer is
/// `Transient("BAD_RESPONSE")` from `Net` and never cached.
#[derive(Deserialize)]
struct LookupResponse {
    #[serde(rename = "status", deserialize_with = "ok_status")]
    _status: (),
    #[serde(default, deserialize_with = "lenient_vec")]
    results: Vec<AcoustidResult>,
}

fn nan() -> f64 {
    f64::NAN
}

fn ok_status<'de, D: Deserializer<'de>>(d: D) -> Result<(), D::Error> {
    match Value::deserialize(d)? {
        Value::String(s) if s == "ok" => Ok(()),
        Value::String(s) => {
            let shown: String = s.chars().take(40).collect();
            Err(D::Error::custom(format!(
                "AcoustID returned status '{shown}'"
            )))
        }
        _ => Err(D::Error::custom("AcoustID returned status 'unreadable'")),
    }
}

fn lenient_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(lenient_opt_vec(d)?.unwrap_or_default())
}

/// An array with its unreadable items dropped; anything but an array is `None`.
fn lenient_opt_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    d: D,
) -> Result<Option<Vec<T>>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => Some(
            items
                .into_iter()
                .filter_map(|i| serde_json::from_value(i).ok())
                .collect(),
        ),
        _ => None,
    })
}

fn lenient_text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(lenient_opt_text(d)?.unwrap_or_default())
}

fn lenient_opt_text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => Some(s),
        _ => None,
    })
}

fn lenient_score<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Value::deserialize(d)?.as_f64().unwrap_or(f64::NAN))
}

fn lenient_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(Value::deserialize(d)?
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0))
}

// ---- the lookup ----

/// Form values: everything but RFC 3986's unreserved characters is escaped (a space is `%20`).
/// A base64url fingerprint is all unreserved, so it goes as it is.
const FORM: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The body, built by hand so `meta`'s space is `%20` (the server does not read `+` as a space).
fn request_body(key: &str, fingerprint: &str, duration_secs: i32) -> String {
    format!(
        "client={}&duration={duration_secs}&fingerprint={}&meta=recordings%20releasegroups",
        utf8_percent_encode(key, FORM),
        utf8_percent_encode(fingerprint, FORM),
    )
}

/// An `ok` answer with no results: absent, and cached as such.
fn no_results(v: &Value) -> bool {
    v.get("status").and_then(Value::as_str) == Some("ok")
        && v.get("results")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
}

/// `POST /lookup` for `fingerprint` (trimmed) of a track `duration_secs` long. No key is
/// `Permanent("NO_CLIENT_KEY")` and an empty fingerprint or a duration that is not positive is
/// `Permanent("NO_FINGERPRINT")`, both without a request; an answer whose `status` is not `"ok"`
/// is `Transient("BAD_RESPONSE")`; no results is `Absent`.
pub async fn lookup(
    net: &Net,
    key: &str,
    fingerprint: &str,
    duration_secs: i32,
) -> Outcome<Vec<AcoustidResult>> {
    let key = key.trim();
    if key.is_empty() {
        return Outcome::Permanent(Failure::new(
            "NO_CLIENT_KEY",
            "no AcoustID client key configured; the question was never asked",
        ));
    }
    let fingerprint = fingerprint.trim();
    if fingerprint.is_empty() || duration_secs <= 0 {
        return Outcome::Permanent(Failure::new(
            "NO_FINGERPRINT",
            format!(
                "cannot ask: fingerprint {}, duration {duration_secs}",
                if fingerprint.is_empty() {
                    "missing"
                } else {
                    "present"
                }
            ),
        ));
    }
    let req = Req::post_form(
        Host::AcoustId,
        "/lookup",
        request_body(key, fingerprint, duration_secs),
    )
    .absent_when(no_results);
    match net.post_form_json::<LookupResponse>(req).await {
        // Results that were all unreadable are no results.
        Outcome::Answered(r) if r.results.is_empty() => {
            Outcome::Absent(Evidence(json!({ "status": "ok", "results": [] })))
        }
        other => other.map(|r| r.results),
    }
}

// ---- the pick ----

/// A recording's length in whole seconds for [`lengths_agree`], rounded half up as
/// `musicbrainz::ms_to_secs` rounds (D-124).
fn whole_secs(d: f64) -> i32 {
    // `lenient_duration` keeps positive finite values only; the clamp keeps the cast in range.
    d.round().clamp(1.0, f64::from(i32::MAX)) as i32
}

fn no_match(results: &[AcoustidResult], duration_secs: i32) -> Evidence {
    let top = results
        .first()
        .map(|r| r.score)
        .filter(|s| s.is_finite())
        .map_or(Value::Null, |s| json!(s));
    Evidence(json!({
        "askedDuration": duration_secs,
        "topScore": top,
        "reason": NO_MATCH_REASON,
    }))
}

/// Spec §8.4 (`pickFingerprintMatch`): the first result at or above [`MIN_SCORE`] with a
/// recording whose length does not disagree with the track's; among its fitting recordings, those
/// whose title agrees with `title` (same version) when there are any, else all of them; of those
/// the closest length, the first listed on a tie (an unknown length is farthest). No artist
/// check: the audio is the evidence. Linear in the number of recordings; a duration that is not
/// positive picks nothing.
pub fn pick_fingerprint_match(
    results: &[AcoustidResult],
    duration_secs: i32,
    title: Option<&str>,
) -> Result<Picked, Evidence> {
    if duration_secs <= 0 {
        return Err(no_match(results, duration_secs));
    }
    let title = title.filter(|t| !t.is_empty());
    let wanted = title.map(|t| (t, version_of(t)));
    let asked = f64::from(duration_secs);
    let gap = |r: &AcoustidRecording| r.duration.map_or(f64::INFINITY, |d| (d - asked).abs());
    let closest = |a: &&AcoustidRecording, b: &&AcoustidRecording| gap(a).total_cmp(&gap(b));
    for result in results {
        // A NaN score (missing, not a number) is below the floor.
        if result.score.is_nan() || result.score < MIN_SCORE {
            continue;
        }
        let Some(recordings) = &result.recordings else {
            continue;
        };
        let fits = || {
            recordings.iter().filter(|r| {
                lengths_agree(Some(duration_secs), r.duration.map(whole_secs)) != Some(false)
            })
        };
        let titled = wanted.as_ref().and_then(|(t, version)| {
            fits()
                .filter(|r| {
                    r.title.as_deref().is_some_and(|rt| {
                        !rt.is_empty() && titles_agree(rt, t) && version_of(rt) == *version
                    })
                })
                .min_by(closest)
        });
        if let Some(r) = titled.or_else(|| fits().min_by(closest)) {
            return Ok(Picked {
                recording: r.id,
                score: result.score,
                acoustid_id: result.id.clone(),
            });
        }
    }
    Err(no_match(results, duration_secs))
}

/// [`lookup`] then [`pick_fingerprint_match`]: the recording a fingerprint is, or `Absent` with
/// `{askedDuration, topScore, reason}`.
pub async fn identify(
    net: &Net,
    key: &str,
    fingerprint: &str,
    duration_secs: i32,
    title: Option<&str>,
) -> Outcome<Picked> {
    match lookup(net, key, fingerprint, duration_secs).await {
        Outcome::Answered(results) => {
            match pick_fingerprint_match(&results, duration_secs, title) {
                Ok(p) => Outcome::Answered(p),
                Err(e) => Outcome::Absent(e),
            }
        }
        Outcome::Absent(_) => Outcome::Absent(no_match(&[], duration_secs)),
        Outcome::RateLimited { until } => Outcome::RateLimited { until },
        Outcome::Transient(f) => Outcome::Transient(f),
        Outcome::Permanent(f) => Outcome::Permanent(f),
    }
}
