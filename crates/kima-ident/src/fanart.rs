//! fanart.tv: an artist image and an album cover (spec §12.3 step 4, §12.7 step 3; upstream
//! `fanart.ts`), and the settings page's key test (spec §15.2).
//!
//! The API key is a secret. It only ever travels as the `api_key` query pair, which `Net` strips
//! from cache keys and the cache's `request` column, shows as `REDACTED` in `Req`'s `Debug`, and
//! scrubs from failure messages; nothing here logs, formats or stores it. URLs in answers are
//! returned as text and never fetched here.

use std::time::Duration;

use kima_db::settings_kv::{KvSettings, SettingsCache};
use kima_net::{Failure, Host, Net, Outcome, Req};
use serde_json::Value;

use crate::Mbid;
use crate::answer::{self, Got, items};

/// The artist the key test asks for (The Beatles; upstream's test MBID).
pub const TEST_MBID: &str = "b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d";

/// How long the key test waits (spec §15.2).
pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Artist image kinds, in order of preference (`fanart.ts:ARTIST_IMAGE_KINDS`).
pub const ARTIST_IMAGE_KINDS: [&str; 3] = ["artistbackground", "artistthumb", "hdmusiclogo"];

/// The key enrichment uses (spec §15.1): the stored key only while `fanartEnabled`, else `env`
/// (`FANART_API_KEY`). A value that is empty once trimmed is no key.
pub fn key(kv: &KvSettings, env: Option<&str>) -> Option<String> {
    let stored = kv.fanart_api_key.as_deref().filter(|_| kv.fanart_enabled);
    [stored, env]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|k| !k.is_empty())
        .map(str::to_string)
}

/// [`key`] over the cached settings. A settings read that fails is treated as no stored key, so
/// the env key is used and the failure never propagates.
pub async fn resolve_key(settings: &SettingsCache, env: Option<&str>) -> Option<String> {
    match settings.get().await {
        Ok(kv) => key(&kv, env),
        Err(e) => {
            // A database error; it cannot carry the key, which is only in the decrypted settings.
            tracing::debug!(error = %e, "settings read failed; using the env fanart key");
            key(&KvSettings::default(), env)
        }
    }
}

fn no_key<T>() -> Outcome<T> {
    Outcome::Permanent(Failure::new(
        "NO_API_KEY",
        "no fanart.tv key configured; the question was never asked",
    ))
}

/// `GET /music/{path}?api_key=…`; 404 is an absence (the same for every read of one URL).
fn request(path: &str, key: &str) -> Req {
    Req::get(Host::Fanart, path)
        .query("api_key", key)
        .absent_on_404()
}

/// The first image URL of `kind`, a bare file name prefixed with the asset host.
fn image(body: &Value, mbid: Mbid, kind: &str) -> Option<String> {
    let url = answer::url(items(body.get(kind)).first()?.get("url"))?;
    Some(if url.starts_with("http") {
        url
    } else {
        format!("https://assets.fanart.tv/fanart/music/{mbid}/{kind}/{url}")
    })
}

/// `GET /music/{mbid}`: the first image of the first of [`ARTIST_IMAGE_KINDS`] the artist has.
/// A 404 or no image is `None`; no key asks nothing (`Permanent("NO_API_KEY")`).
pub async fn artist_image(net: &Net, key: &str, mbid: Mbid) -> Outcome<Option<String>> {
    let key = key.trim();
    if key.is_empty() {
        return no_key();
    }
    let body = match answer::got(
        net.get_json::<Value>(request(&format!("/music/{mbid}"), key))
            .await,
    ) {
        Ok(Got::Answered(body)) => body,
        Ok(Got::Absent(_)) => return Outcome::Answered(None),
        Err(o) => return o,
    };
    Outcome::Answered(
        ARTIST_IMAGE_KINDS
            .into_iter()
            .find_map(|kind| image(&body, mbid, kind)),
    )
}

/// `GET /music/albums/{rg}`: `albums.{rg}.albumcover[0].url`, else `cdart[0].url`. A 404 or no
/// image is `None`; no key asks nothing.
pub async fn album_cover(net: &Net, key: &str, rg: Mbid) -> Outcome<Option<String>> {
    let key = key.trim();
    if key.is_empty() {
        return no_key();
    }
    let req = request(&format!("/music/albums/{rg}"), key);
    let body = match answer::got(net.get_json::<Value>(req).await) {
        Ok(Got::Answered(body)) => body,
        Ok(Got::Absent(_)) => return Outcome::Answered(None),
        Err(o) => return o,
    };
    let Some(album) = body.get("albums").and_then(|a| a.get(rg.to_string())) else {
        return Outcome::Answered(None);
    };
    let first = |kind: &str| answer::url(items(album.get(kind)).first()?.get("url"));
    Outcome::Answered(first("albumcover").or_else(|| first("cdart")))
}

/// What the key test came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestResult {
    /// fanart.tv answered.
    Ok,
    /// 401 or 403: the key is wrong.
    Rejected,
    /// Anything else: a timeout, a 5xx, a 404, a pause, an unreadable answer.
    NoAnswer,
}

/// The key test's request: The Beatles, [`TEST_TIMEOUT`] for the call and the wait for a slot,
/// never cached (each press asks).
pub fn test_request(key: &str) -> Req {
    Req::get(Host::Fanart, &format!("/music/{TEST_MBID}"))
        .query("api_key", key)
        .timeout(TEST_TIMEOUT)
        .budget(TEST_TIMEOUT)
        .max_wait(TEST_TIMEOUT)
        .no_cache()
}

/// Asks fanart.tv for The Beatles with `key` (spec §15.2). An empty key is `Rejected` without a
/// request.
pub async fn test_key(net: &Net, key: &str) -> TestResult {
    let key = key.trim();
    if key.is_empty() {
        return TestResult::Rejected;
    }
    match net.get_json::<Value>(test_request(key)).await {
        Outcome::Answered(_) => TestResult::Ok,
        Outcome::Permanent(f) if f.code == "HTTP_401" || f.code == "HTTP_403" => {
            TestResult::Rejected
        }
        _ => TestResult::NoAnswer,
    }
}
