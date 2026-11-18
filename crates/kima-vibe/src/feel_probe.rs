//! The feel probe (`feelProbe.json`): two linear probes (valence, arousal) read off the CLAP sound vector. Upstream's
//! file is derived from DEAM (CC BY-NC), so it is a private asset (owner decision Q3), loaded from
//! `KIMA_VIBE_ASSET_DIR`, hash-pinned, never committed.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::CLAP_DIMS;
use crate::asset::{AssetError, finite, weights};

#[derive(Debug, Clone, PartialEq)]
pub struct ProbeAxis {
    /// Present in the file, unused by the SQL (upstream too).
    pub alpha: f64,
    pub bias: f64,
    pub weights: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeelProbe {
    /// The sound model the probe was fitted on (informational; the engine's gate is `MATCHING_SOUND`).
    pub model: String,
    pub valence: ProbeAxis,
    pub arousal: ProbeAxis,
    /// First 16 hex digits of the SHA-256 of the file bytes: the re-run key stored with every feel row (D-181;
    /// upstream hashes a re-serialisation, which is not byte-stable across languages).
    pub probe_id: String,
}

const PREFIX: &str = "Feel probe";

fn err(msg: impl Into<String>) -> AssetError {
    AssetError(format!("{PREFIX}: {}", msg.into()))
}

fn axis(file: &Value, name: &str) -> Result<ProbeAxis, AssetError> {
    let a = file
        .get(name)
        .filter(|v| v.is_object())
        .ok_or_else(|| err(format!("no {name}")))?;
    Ok(ProbeAxis {
        alpha: a
            .get("alpha")
            .and_then(finite)
            .ok_or_else(|| err(format!("{name} has no finite alpha")))?,
        bias: a
            .get("bias")
            .and_then(finite)
            .ok_or_else(|| err(format!("{name} has no finite bias")))?,
        weights: weights(PREFIX, name, a.get("weights"), CLAP_DIMS)?,
    })
}

/// Parses the bytes that were hash-verified; the probe id is derived from those same bytes.
pub fn parse_feel_probe(bytes: &[u8]) -> Result<FeelProbe, AssetError> {
    let file: Value =
        serde_json::from_slice(bytes).map_err(|e| err(format!("not valid JSON ({e})")))?;
    let model = file
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("no model"))?;
    let valence = axis(&file, "valence")?;
    let arousal = axis(&file, "arousal")?;
    let digest = Sha256::digest(bytes);
    let probe_id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Ok(FeelProbe {
        model: model.to_owned(),
        valence,
        arousal,
        probe_id,
    })
}
