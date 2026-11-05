//! Bridges `serde_json` and vertigo's `JsJson`.
//!
//! vertigo 0.13 moves request and response bodies as `JsJson` (not as raw text, which the plan assumed),
//! so every DTO goes `T -> serde_json::Value -> JsJson` on the way out and back on the way in. Plain
//! `serde` stays the only DTO contract (spec §4.2); no `AutoJsJson` derive is needed.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Number, Value};
use vertigo::{JsJson, JsJsonNumber, JsObject};

/// Integers up to 2^53 survive the `f64` that `JsJson` numbers carry.
const MAX_EXACT: f64 = 9_007_199_254_740_992.0;

pub fn value_to_js(value: Value) -> JsJson {
    match value {
        Value::Null => JsJson::Null,
        Value::Bool(b) => JsJson::bool(b),
        Value::Number(n) => JsJson::Number(JsJsonNumber(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => JsJson::String(s),
        Value::Array(items) => JsJson::List(items.into_iter().map(value_to_js).collect()),
        Value::Object(map) => JsJson::Object(
            map.into_iter()
                .map(|(k, v)| (k, value_to_js(v)))
                .collect::<JsObject>(),
        ),
    }
}

pub fn js_to_value(json: JsJson) -> Result<Value, String> {
    Ok(match json {
        JsJson::True => Value::Bool(true),
        JsJson::False => Value::Bool(false),
        JsJson::Null | JsJson::Undefined => Value::Null,
        JsJson::String(s) => Value::String(s),
        JsJson::Number(JsJsonNumber(f)) => {
            if f.is_finite() && f.fract() == 0.0 && f.abs() <= MAX_EXACT {
                Value::Number(Number::from(f as i64))
            } else {
                Value::Number(Number::from_f64(f).ok_or("non-finite number in response")?)
            }
        }
        JsJson::List(items) => Value::Array(
            items
                .into_iter()
                .map(js_to_value)
                .collect::<Result<_, _>>()?,
        ),
        JsJson::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| js_to_value(v).map(|v| (k, v)))
                .collect::<Result<_, _>>()?,
        ),
        JsJson::Vec(_) => return Err("binary payload in a JSON response".into()),
    })
}

pub fn encode<T: Serialize>(body: &T) -> Result<JsJson, String> {
    serde_json::to_value(body)
        .map(value_to_js)
        .map_err(|e| format!("encoding request: {e}"))
}

pub fn decode<T: DeserializeOwned>(json: JsJson) -> Result<T, String> {
    let value = js_to_value(json)?;
    serde_json::from_value(value).map_err(|e| format!("unexpected response shape: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kima_api_types::library::{AlbumsResponse, ArtistsResponse, TracksResponse};
    use kima_api_types::playback_state::PlaybackState;

    #[test]
    fn value_survives_the_js_json_round_trip() {
        let v: Value = serde_json::json!({
            "a": [1, 2.5, -3, null, true, "ż"], "b": {"c": 20, "d": 20.0}, "e": ""
        });
        let back = js_to_value(value_to_js(v.clone())).unwrap();
        // 20.0 comes back as the integer 20 (JS has one number type); everything else is identical.
        assert_eq!(back["a"], v["a"]);
        assert_eq!(back["b"]["c"], v["b"]["c"]);
        assert_eq!(back["b"]["d"], serde_json::json!(20));
    }

    /// The real fixtures decode into the DTOs through the same path the client uses.
    #[test]
    fn real_fixtures_decode_through_js_json() {
        fn through<T: DeserializeOwned>(fixture: &str) -> T {
            decode(value_to_js(serde_json::from_str(fixture).unwrap())).unwrap()
        }
        let artists: ArtistsResponse = through(include_str!(
            "../../../kima-api-types/tests/fixtures/library_artists_response.json"
        ));
        assert!(!artists.artists.is_empty());
        let albums: AlbumsResponse = through(include_str!(
            "../../../kima-api-types/tests/fixtures/library_albums_response.json"
        ));
        assert!(!albums.albums.is_empty());
        let tracks: TracksResponse = through(include_str!(
            "../../../kima-api-types/tests/fixtures/library_tracks_response.json"
        ));
        assert_eq!(tracks.tracks[0].duration, 20);
        let state: PlaybackState = through(include_str!(
            "../../../kima-api-types/tests/fixtures/playback_state_response.json"
        ));
        assert_eq!(state.queue.unwrap()[0].duration, 20.0);
    }

    #[test]
    fn shape_mismatch_is_an_error_not_a_panic() {
        let err = decode::<ArtistsResponse>(value_to_js(serde_json::json!({"artists": 3})));
        assert!(err.unwrap_err().contains("unexpected response shape"));
    }
}
