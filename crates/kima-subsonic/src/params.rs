//! Request parameters: the query string and a form-encoded POST body, merged (OpenSubsonic
//! `formPost`), with repeated keys collected in order.

use std::str::FromStr;

use axum::body::{Body, to_bytes};
use axum::extract::{FromRequestParts, Request};
use axum::http::Method;
use axum::http::header::CONTENT_TYPE;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;

use crate::error::SubsonicError;
use crate::response::{Format, render_error};

/// Largest form body read.
const BODY_LIMIT: usize = 1024 * 1024;

/// Keys in order of first appearance, each with its values in order. Query values come before
/// body values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params {
    entries: Vec<(String, Vec<String>)>,
}

impl Params {
    pub fn parse(query: Option<&str>, form_body: Option<&[u8]>) -> Self {
        let mut params = Self::default();
        let query = query.unwrap_or("").as_bytes();
        for (key, value) in
            form_urlencoded::parse(query).chain(form_urlencoded::parse(form_body.unwrap_or(&[])))
        {
            params.push(key.into_owned(), value.into_owned());
        }
        params
    }

    fn push(&mut self, key: String, value: String) {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some((_, values)) => values.push(value),
            None => self.entries.push((key, vec![value])),
        }
    }

    /// The first value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.all(key).first().map(String::as_str)
    }

    pub fn all(&self, key: &str) -> &[String] {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map_or(&[], |(_, values)| values.as_slice())
    }

    /// Missing or empty is error 10.
    pub fn required(&self, key: &str) -> Result<&str, SubsonicError> {
        match self.get(key) {
            Some(value) if !value.is_empty() => Ok(value),
            _ => Err(SubsonicError::missing(key)),
        }
    }

    /// Absent (or empty) is `None`; present but unparsable is error 10.
    pub fn int<T: FromStr>(&self, key: &str) -> Result<Option<T>, SubsonicError> {
        match self.get(key) {
            None | Some("") => Ok(None),
            Some(value) => value
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| SubsonicError::invalid(key)),
        }
    }

    /// Accepts `true`, `false`, `1` and `0`.
    pub fn bool(&self, key: &str) -> Result<Option<bool>, SubsonicError> {
        match self.get(key) {
            None | Some("") => Ok(None),
            Some("true" | "1") => Ok(Some(true)),
            Some("false" | "0") => Ok(Some(false)),
            Some(_) => Err(SubsonicError::invalid(key)),
        }
    }

    pub fn format(&self) -> Format {
        Format::from_param(self.get("f"))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = SubsonicError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, SubsonicError> {
        parts
            .extensions
            .get::<Params>()
            .cloned()
            .ok_or_else(|| SubsonicError::internal("params_layer is missing"))
    }
}

/// Parses the parameters once and stores them in the request extensions.
pub async fn params_layer(request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let query = parts.uri.query();
    let is_form = parts.method == Method::POST
        && parts
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("application/x-www-form-urlencoded")
            });
    let (body, form) = if is_form {
        match to_bytes(body, BODY_LIMIT).await {
            Ok(bytes) => (Body::from(bytes.clone()), Some(bytes)),
            Err(_) => {
                let format = Params::parse(query, None).format();
                return render_error(format, &SubsonicError::generic("Request body too large"));
            }
        }
    } else {
        (body, None)
    };
    let params = Params::parse(query, form.as_deref());
    let mut request = Request::from_parts(parts, body);
    request.extensions_mut().insert(params);
    next.run(request).await
}

/// `enc:<hex>` is the hex of the UTF-8 password; anything else (including bad hex) is the
/// password as sent, as upstream's `decodeSubsonicPassword`.
pub fn decode_password(raw: &str) -> String {
    if let Some(digits) = raw.strip_prefix("enc:")
        && let Ok(bytes) = hex::decode(digits)
        && !bytes.is_empty()
    {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    raw.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_keys_kept_in_order() {
        let p = Params::parse(Some("id=a&x=1&id=b&id=c"), None);
        assert_eq!(p.all("id"), ["a", "b", "c"]);
        assert_eq!(p.get("id"), Some("a"));
        assert!(p.all("nope").is_empty());
    }

    #[test]
    fn post_body_merges_after_query() {
        let p = Params::parse(Some("id=a&u=bob"), Some(b"id=b&id=c"));
        assert_eq!(p.all("id"), ["a", "b", "c"]);
        assert_eq!(p.get("u"), Some("bob"));
    }

    #[test]
    fn values_are_percent_decoded() {
        let p = Params::parse(Some("q=a%20b+c%26"), None);
        assert_eq!(p.get("q"), Some("a b c&"));
    }

    #[test]
    fn enc_password_decodes() {
        assert_eq!(decode_password("enc:73656372657431"), "secret1");
        assert_eq!(decode_password("plain"), "plain");
    }

    #[test]
    fn bad_enc_hex_is_raw() {
        assert_eq!(decode_password("enc:xyz"), "enc:xyz");
        assert_eq!(decode_password("enc:abc"), "enc:abc");
        assert_eq!(decode_password("enc:"), "enc:");
    }

    #[test]
    fn required_missing_or_empty_is_10() {
        let p = Params::parse(Some("a=&b=x"), None);
        assert_eq!(p.required("a").unwrap_err().code, 10);
        assert_eq!(
            p.required("c").unwrap_err().message,
            "Required parameter is missing: c"
        );
        assert_eq!(p.required("b").unwrap(), "x");
    }

    #[test]
    fn int_unparsable_is_10() {
        let p = Params::parse(Some("n=12&bad=x1&neg=-3"), None);
        assert_eq!(p.int::<u32>("n").unwrap(), Some(12));
        assert_eq!(p.int::<u32>("none").unwrap(), None);
        assert_eq!(p.int::<i64>("neg").unwrap(), Some(-3));
        let err = p.int::<u32>("bad").unwrap_err();
        assert_eq!(err.code, 10);
        assert_eq!(err.message, "Invalid parameter: bad");
        assert!(p.int::<u32>("neg").is_err());
    }

    #[test]
    fn bool_accepts_four_spellings() {
        let p = Params::parse(Some("a=true&b=false&c=1&d=0&e=yes"), None);
        assert_eq!(p.bool("a").unwrap(), Some(true));
        assert_eq!(p.bool("b").unwrap(), Some(false));
        assert_eq!(p.bool("c").unwrap(), Some(true));
        assert_eq!(p.bool("d").unwrap(), Some(false));
        assert_eq!(p.bool("z").unwrap(), None);
        assert_eq!(p.bool("e").unwrap_err().code, 10);
    }

    #[test]
    fn format_json_and_jsonp() {
        assert_eq!(Params::parse(Some("f=json"), None).format(), Format::Json);
        assert_eq!(Params::parse(Some("f=jsonp"), None).format(), Format::Json);
        assert_eq!(Params::parse(Some("f=xml"), None).format(), Format::Xml);
        assert_eq!(Params::parse(Some("f=other"), None).format(), Format::Xml);
        assert_eq!(Params::parse(None, None).format(), Format::Xml);
    }
}
