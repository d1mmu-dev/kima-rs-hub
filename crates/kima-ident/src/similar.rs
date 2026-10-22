//! Similar artists (spec §12.5; upstream `artistSimilarity.ts`): ListenBrainz by MBID, Deezer by
//! name, merged into the list stored in `artists."similar"`.
//!
//! The library re-rank (`RERANK_MAX_DELTA`) needs M5's matching state; it is ported as delta 0,
//! which leaves the merged order and scores as they are.

use serde::Serialize;

use crate::Mbid;
use crate::listenbrainz::Similar;

/// The most similar artists stored (spec §12.5).
pub const MAX_SIMILAR: usize = 30;

/// One stored similar artist, serialised `{name, mbid, match}` (`mbid` null when unknown).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub name: String,
    pub mbid: Option<Mbid>,
    #[serde(rename = "match")]
    pub r#match: f64,
}

/// ListenBrainz's rows spread over 0.5-0.95 against the strongest:
/// `match = 0.5 + 0.45 × score / strongest` (`listenBrainz.ts:152-156`). The rows come
/// strongest first.
fn from_listenbrainz(lb: &[Similar]) -> Vec<Entry> {
    let strongest = lb
        .iter()
        .map(|s| s.score)
        .fold(0.0_f64, f64::max)
        .max(f64::MIN_POSITIVE);
    lb.iter()
        .map(|s| Entry {
            name: s.name.clone(),
            mbid: s.mbid,
            r#match: 0.5 + 0.45 * s.score / strongest,
        })
        .collect()
}

/// Deezer's related artists, in its order, falling linearly from 0.9 to 0.5 by position
/// (`deezer.ts:211-218`: `max(0.5, 0.9 − 0.4 × i / max(n − 1, 1))`).
fn from_deezer(deezer: &[String]) -> Vec<Entry> {
    let n = deezer.len().saturating_sub(1).max(1) as f64;
    deezer
        .iter()
        .enumerate()
        .map(|(i, name)| Entry {
            name: name.clone(),
            mbid: None,
            r#match: (0.9 - 0.4 * i as f64 / n).max(0.5),
        })
        .collect()
}

/// The stored list (spec §12.5): ListenBrainz alone when it has at least [`MAX_SIMILAR`];
/// otherwise ListenBrainz then Deezer, deduplicated by lower-cased trimmed name (the first,
/// ListenBrainz's, wins; an empty name is dropped), cut to [`MAX_SIMILAR`]. Both empty is empty.
pub fn merge(lb: &[Similar], deezer: &[String]) -> Vec<Entry> {
    let mut lb = from_listenbrainz(lb);
    if lb.len() >= MAX_SIMILAR {
        lb.truncate(MAX_SIMILAR);
        return lb;
    }
    let mut seen = std::collections::HashSet::new();
    lb.into_iter()
        .chain(from_deezer(deezer))
        .filter(|e| {
            let key = e.name.trim().to_lowercase();
            !key.is_empty() && seen.insert(key)
        })
        .take(MAX_SIMILAR)
        .collect()
}
