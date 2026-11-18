//! Shared JSON checks for the private asset files (feel probe, mood heads).

use serde_json::Value;

/// A refused asset file; the text is the message shown in the capability report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AssetError(pub String);

pub(crate) fn finite(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}

/// `dims` finite numbers, or the upstream-shaped message naming `what`.
pub(crate) fn weights(
    prefix: &str,
    what: &str,
    v: Option<&Value>,
    dims: usize,
) -> Result<Vec<f64>, AssetError> {
    let arr = match v.and_then(Value::as_array) {
        Some(a) if a.len() == dims => a,
        _ => return Err(AssetError(format!("{prefix}: {what} needs {dims} weights"))),
    };
    arr.iter()
        .map(|x| {
            finite(x).ok_or_else(|| {
                AssetError(format!(
                    "{prefix}: {what} has a weight that is not a finite number"
                ))
            })
        })
        .collect()
}
