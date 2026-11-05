//! The 22 learned mood heads (`moodHeads.json`): each mood's standardisation folded into its weights and bias, so a
//! score is one dot product (a logit). Port of upstream `services/moods/heads.ts` `parseMoodHeads` (v1.12.3).
//!
//! The file is a private asset (owner decision Q3: the training-data licences are unchecked); it is loaded from
//! `KIMA_VIBE_ASSET_DIR`, hash-pinned, and never committed.

use serde_json::Value;

use crate::CLAP_DIMS;
use crate::asset::{AssetError, finite, weights};

/// The moods in the file's order; each is a column of `track_moods` and a pair of columns of `mood_state`.
pub const MOODS: [&str; 22] = [
    "happy",
    "fun_quirky",
    "uplifting",
    "sad",
    "nostalgic",
    "dark",
    "trippy",
    "calm",
    "dreamy",
    "energetic",
    "aggressive",
    "epic",
    "party",
    "groovy",
    "romantic",
    "sexy",
    "summer",
    "night",
    "driving",
    "workout",
    "focus",
    "sleep",
];

/// Moods still scored that no tag, recipe or definition may use.
pub const RETIRED_MOODS: [&str; 1] = ["night"];

#[derive(Debug, Clone, PartialEq)]
pub struct MoodHead {
    pub weights: Vec<f64>,
    pub bias: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MoodHeads {
    /// Identifies the weights; stored with every score so a new file rescores every track.
    pub version: String,
    /// The sound model the moods were fitted on.
    pub model: String,
    pub dims: usize,
    /// In [`MOODS`] order.
    pub heads: Vec<MoodHead>,
}

const PREFIX: &str = "Mood weights";

fn err(msg: impl Into<String>) -> AssetError {
    AssetError(format!("{PREFIX}: {}", msg.into()))
}

/// The weights file, checked: every mood present in order, each with 512 finite weights and a finite bias.
pub fn parse_mood_heads(bytes: &[u8]) -> Result<MoodHeads, AssetError> {
    let file: Value =
        serde_json::from_slice(bytes).map_err(|e| err(format!("not valid JSON ({e})")))?;
    let version = file
        .get("version")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("no version"))?;
    let model = file
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("no model"))?;
    let dims = file
        .get("dims")
        .and_then(Value::as_u64)
        .filter(|&d| d > 0)
        .ok_or_else(|| err("no dims"))? as usize;
    let order_ok = file
        .get("order")
        .and_then(Value::as_array)
        .is_some_and(|o| {
            o.len() == MOODS.len() && o.iter().zip(MOODS).all(|(a, b)| a.as_str() == Some(b))
        });
    if !order_ok {
        return Err(err(format!("order must be {}", MOODS.join(", "))));
    }
    let moods = file
        .get("moods")
        .and_then(Value::as_object)
        .ok_or_else(|| err("no moods"))?;
    let extra: Vec<&str> = moods
        .keys()
        .map(String::as_str)
        .filter(|m| !MOODS.contains(m))
        .collect();
    if !extra.is_empty() {
        return Err(err(format!(
            "moods outside the order: {}",
            extra.join(", ")
        )));
    }
    if dims != CLAP_DIMS {
        return Err(err(format!("dims must be {CLAP_DIMS}")));
    }
    let mut heads = Vec::with_capacity(MOODS.len());
    for mood in MOODS {
        let head = moods
            .get(mood)
            .ok_or_else(|| err(format!("{mood} is missing")))?;
        let w = weights(PREFIX, mood, head.get("w"), dims)?;
        let bias = head
            .get("b")
            .and_then(finite)
            .ok_or_else(|| err(format!("{mood} has no finite bias")))?;
        heads.push(MoodHead { weights: w, bias });
    }
    Ok(MoodHeads {
        version: version.to_owned(),
        model: model.to_owned(),
        dims,
        heads,
    })
}
