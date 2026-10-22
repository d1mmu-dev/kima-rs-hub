//! Similar artists from ListenBrainz's similarity model (spec §12.5; upstream
//! `listenBrainz.ts:getSimilarArtists`), read from the Labs API with the algorithm its own artist
//! pages use.
//!
//! The answer is untrusted: a row counts only with an MBID, a name and a positive finite score,
//! and the artist itself is left out. An empty list (or one with nothing left) is `Absent`, which
//! the cache keeps for a day; an answer that is not a list is `Transient("BAD_RESPONSE")`.

use kima_net::{Evidence, Host, Net, Outcome, Req};
use serde_json::{Value, json};

use crate::Mbid;
use crate::answer::{self, MAX_NAME_CHARS};

/// The model ListenBrainz's artist pages show (listenbrainz-server `entity_pages.py`).
pub const ALGORITHM: &str =
    "session_based_days_7500_session_300_contribution_3_threshold_10_limit_100_filter_True_skip_30";

/// The most similar artists kept (spec §12.5).
pub const MAX_SIMILAR: usize = 30;

/// One similar artist and the model's raw score (Task 21 spreads it over 0.5-0.95).
#[derive(Debug, Clone, PartialEq)]
pub struct Similar {
    pub name: String,
    pub mbid: Option<Mbid>,
    pub score: f64,
}

/// A successful answer with no rows.
fn empty(v: &Value) -> bool {
    v.as_array().is_some_and(Vec::is_empty)
}

/// A score as `Number(score)` reads it: a number or a numeric string.
fn score(v: Option<&Value>) -> Option<f64> {
    let s = match v? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    (s.is_finite() && s > 0.0).then_some(s)
}

/// `GET /similar-artists/json?artist_mbids=<mbid>&algorithm=<ALGORITHM>`: rows with an MBID, a
/// name and a score above 0, not `mbid` itself, strongest first (the first listed on a tie), at
/// most [`MAX_SIMILAR`]. Nothing left is `Absent`.
pub async fn similar(net: &Net, mbid: Mbid) -> Outcome<Vec<Similar>> {
    let req = Req::get(Host::ListenBrainz, "/similar-artists/json")
        .query("artist_mbids", mbid.to_string())
        .query("algorithm", ALGORITHM)
        .absent_when(empty);
    let rows = match answer::got(net.get_json::<Vec<Value>>(req).await) {
        Ok(answer::Got::Answered(rows)) => rows,
        Ok(answer::Got::Absent(e)) => return Outcome::Absent(e),
        Err(o) => return o,
    };
    let mut out: Vec<Similar> = rows
        .iter()
        .filter_map(|r| {
            let id = Mbid::parse(r.get("artist_mbid")?.as_str()?)?;
            if id == mbid {
                return None;
            }
            Some(Similar {
                name: answer::text(r.get("name"), MAX_NAME_CHARS)?,
                mbid: Some(id),
                score: score(r.get("score"))?,
            })
        })
        .collect();
    // Stable: equal scores keep ListenBrainz's order.
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(MAX_SIMILAR);
    if out.is_empty() {
        return Outcome::Absent(Evidence(json!({ "rows": rows.len(), "kept": 0 })));
    }
    Outcome::Answered(out)
}
