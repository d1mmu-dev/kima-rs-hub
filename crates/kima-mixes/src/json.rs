//! A JSON value that keeps object keys in JavaScript's `Object.entries` order, so recipe files are read as upstream
//! reads them (`serde_json::Value` sorts keys unless the workspace-wide `preserve_order` feature is on). Integer-like
//! keys come first in ascending order, then the rest in the order written; a repeated key keeps its first place and
//! takes the last value.

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn parse(text: &str) -> Result<Json, String> {
        serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))
    }

    /// The entries of an object, in `Object.entries` order.
    pub(crate) fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }
}

/// A canonical array index (`"0"`, `"17"`, below 2^32 - 1), which `Object.entries` lists first.
fn array_index(key: &str) -> Option<u32> {
    if key == "0" {
        return Some(0);
    }
    if key.is_empty() || key.starts_with('0') || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&n| n != u32::MAX)
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Number(v as f64))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Number(v as f64))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Json, E> {
        Ok(Json::Number(v))
    }
    fn visit_str<E>(self, v: &str) -> Result<Json, E> {
        Ok(Json::String(v.to_string()))
    }
    fn visit_string<E>(self, v: String) -> Result<Json, E> {
        Ok(Json::String(v))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut entries: Vec<(String, Json)> = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key, value)),
            }
        }
        let (mut indexes, rest): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|(k, _)| array_index(k).is_some());
        indexes.sort_by_key(|(k, _)| array_index(k));
        indexes.extend(rest);
        Ok(Json::Object(indexes))
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

/// A number as JavaScript prints it in a template string, for error texts (`0`, `0.5`, `Infinity`).
pub(crate) fn js_number(n: f64) -> String {
    if n.is_infinite() {
        return if n > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if n.is_nan() {
        return "NaN".into();
    }
    if n == n.trunc() && n.abs() < 1e21 {
        return format!("{}", n as i64);
    }
    format!("{n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_in_object_entries_order() {
        let v = Json::parse(r#"{"b": 1, "10": 2, "a": 3, "2": 4, "02": 5, "b": 6}"#).unwrap();
        let keys: Vec<&str> = v
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(keys, ["2", "10", "b", "a", "02"]);
        assert_eq!(v.as_object().unwrap()[2].1, Json::Number(6.0));
    }

    #[test]
    fn numbers_print_as_javascript_does() {
        assert_eq!(js_number(0.0), "0");
        assert_eq!(js_number(1.0), "1");
        assert_eq!(js_number(0.5), "0.5");
        assert_eq!(js_number(f64::INFINITY), "Infinity");
    }
}
