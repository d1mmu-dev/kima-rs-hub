//! Accept-Encoding negotiation for the two map representations (RFC 9110 §12.5.3).
use axum::http::{HeaderMap, StatusCode, header};

/// Prefer gzip when acceptable; never send identity if the client explicitly excludes it.
/// Malformed field values are a bad request, not an unsupported request-body encoding (415).
pub(super) fn gzip(headers: &HeaderMap) -> Result<bool, StatusCode> {
    let mut gzip = None;
    let mut identity = None;
    let mut wildcard = None;
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let value = value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
        for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let mut parts = item.split(';');
            let name = parts.next().unwrap_or_default().trim();
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            {
                return Err(StatusCode::BAD_REQUEST);
            }
            let mut quality = 1000;
            if let Some(param) = parts.next() {
                let (key, value) = param
                    .trim()
                    .split_once('=')
                    .ok_or(StatusCode::BAD_REQUEST)?;
                if !key.trim().eq_ignore_ascii_case("q") || parts.next().is_some() {
                    return Err(StatusCode::BAD_REQUEST);
                }
                quality = qvalue(value.trim()).ok_or(StatusCode::BAD_REQUEST)?;
            }
            let slot = if name.eq_ignore_ascii_case("gzip") {
                &mut gzip
            } else if name.eq_ignore_ascii_case("identity") {
                &mut identity
            } else if name == "*" {
                &mut wildcard
            } else {
                continue;
            };
            *slot = Some(slot.map_or(quality, |old: u16| old.min(quality)));
        }
    }
    if gzip.or(wildcard).unwrap_or(0) > 0 {
        return Ok(true);
    }
    if identity.unwrap_or(if wildcard == Some(0) { 0 } else { 1000 }) > 0 {
        Ok(false)
    } else {
        Err(StatusCode::NOT_ACCEPTABLE)
    }
}

fn qvalue(value: &str) -> Option<u16> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if !matches!(whole, "0" | "1")
        || fraction.len() > 3
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    if whole == "1" {
        return fraction.bytes().all(|b| b == b'0').then_some(1000);
    }
    Some(
        fraction
            .bytes()
            .enumerate()
            .map(|(i, b)| u16::from(b - b'0') * [100, 10, 1][i])
            .sum(),
    )
}
