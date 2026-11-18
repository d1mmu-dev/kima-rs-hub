//! Mood arithmetic: a learned mood's score, a track's moods against its library, and the collection page's tally.
//! Port of upstream `moodBucketService.ts` (`MoodDistribution`, `classifyMoods`, `moodOutput`) and `vibeMap.ts`
//! (`summariseMoods`), v1.12.3. No database: the rows and statistics come from `kima-db`.

use std::collections::BTreeMap;

use crate::mood_heads::MoodHead;

/// A track has a mood when its score is at least this many library standard deviations above the library's mean.
/// Upstream's `RECIPE_LINE` (`mixes/recipes.ts`); M5b's recipes import this constant rather than repeat it.
pub const RECIPE_LINE: f64 = 1.25;

/// Analysed, but no mood stands out for this track.
pub const NEUTRAL: &str = "neutral";
/// No current mood scores for this track (the vector, the scores or the statistics are missing).
pub const UNANALYZED: &str = "unanalyzed";

/// One of the five moods the clients know. `id` and `field` are the names the apps key on (kept from upstream's old
/// Essentia heads); `learned` is the learned mood behind it (a column of `track_moods`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientMood {
    pub id: &'static str,
    pub field: &'static str,
    pub learned: &'static str,
    pub name: &'static str,
}

/// The five client moods, in the legend's order (spec 8.2).
pub const CLIENT_MOODS: [ClientMood; 5] = [
    ClientMood {
        id: "happy",
        field: "moodHappy",
        learned: "happy",
        name: "Upbeat",
    },
    ClientMood {
        id: "sad",
        field: "moodSad",
        learned: "sad",
        name: "Melancholic",
    },
    ClientMood {
        id: "chill",
        field: "moodRelaxed",
        learned: "calm",
        name: "Calm",
    },
    ClientMood {
        id: "aggressive",
        field: "moodAggressive",
        learned: "aggressive",
        name: "Intense",
    },
    ClientMood {
        id: "party",
        field: "moodParty",
        learned: "party",
        name: "Energetic",
    },
];

/// A track's five client-mood scores (logits), in [`CLIENT_MOODS`] order; `None` when it has no score.
pub type MoodValues = [Option<f64>; 5];

/// Mean and population standard deviation of one mood over the library.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoodStats {
    pub mean: f64,
    pub sd: f64,
}

/// The logit a head gives an embedding: the dot product over the embedding's length, plus the bias. The heads were
/// fitted on unit vectors and the writer lets a stored vector's length differ from 1 slightly, hence the division.
/// This is the f64 reference the SQL scoring is tested against.
pub fn score(head: &MoodHead, embedding: &[f64]) -> f64 {
    let norm = embedding.iter().map(|x| x * x).sum::<f64>().sqrt();
    let dot: f64 = embedding
        .iter()
        .zip(&head.weights)
        .map(|(a, b)| a * b)
        .sum();
    dot / norm + head.bias
}

/// Each mood's sorted library values (for ranking a value within the library) and the library's statistics.
#[derive(Debug, Clone)]
pub struct MoodDistribution {
    sorted: [Vec<f64>; 5],
    size: usize,
    stats: [MoodStats; 5],
}

fn analysed(row: &MoodValues) -> bool {
    row.iter().all(Option::is_some)
}

impl MoodDistribution {
    /// Rows missing any of the five moods are left out of the distribution.
    pub fn new(rows: &[MoodValues], stats: [MoodStats; 5]) -> Self {
        let complete: Vec<&MoodValues> = rows.iter().filter(|r| analysed(r)).collect();
        let sorted = std::array::from_fn(|i| {
            let mut v: Vec<f64> = complete.iter().filter_map(|r| r[i]).collect();
            v.sort_by(f64::total_cmp);
            v
        });
        Self {
            sorted,
            size: complete.len(),
            stats,
        }
    }

    /// Rows the distribution covers.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Share of the library below `v` on mood `i`, counting ties as half (midpoint of the lower and upper bound).
    pub fn rank(&self, i: usize, v: f64) -> f64 {
        let s = &self.sorted[i];
        if s.is_empty() {
            return 0.0;
        }
        let lower = s.partition_point(|x| *x < v);
        let upper = s.partition_point(|x| *x <= v);
        (lower + upper) as f64 / 2.0 / s.len() as f64
    }

    /// How many library standard deviations `v` sits above the mean of mood `i`. A mood with no spread ranks no one
    /// above anyone: `-inf`, under any line, as the recipe engine and the song tags treat it.
    pub fn z(&self, i: usize, v: f64) -> f64 {
        let MoodStats { mean, sd } = self.stats[i];
        if sd > 0.0 {
            (v - mean) / sd
        } else {
            f64::NEG_INFINITY
        }
    }
}

/// One track's decision.
#[derive(Debug, Clone, PartialEq)]
pub struct MoodView {
    /// A client mood field, [`NEUTRAL`] or [`UNANALYZED`].
    pub dominant: &'static str,
    /// Library rank (0..1) of the dominant mood; 0 for neutral and unanalysed.
    pub score: f64,
    /// Library rank per mood field, in table order; empty when unanalysed.
    pub ranks: Vec<(&'static str, f64)>,
    /// Every mood the track has (at least [`RECIPE_LINE`] standard deviations above the mean), in table order.
    pub moods: Vec<&'static str>,
}

/// Decides a track's moods against its library. The dominant mood is the one with the highest library rank among
/// those at the line; on equal ranks the first in table order stays.
pub fn classify(row: &MoodValues, dist: &MoodDistribution) -> MoodView {
    if !analysed(row) || dist.size == 0 {
        return MoodView {
            dominant: UNANALYZED,
            score: 0.0,
            ranks: vec![],
            moods: vec![],
        };
    }
    let mut ranks = Vec::with_capacity(5);
    let mut moods = Vec::new();
    let mut dominant = NEUTRAL;
    let mut best = 0.0;
    for (i, mood) in CLIENT_MOODS.iter().enumerate() {
        let value = row[i].expect("analysed rows have all five moods");
        let r = dist.rank(i, value);
        ranks.push((mood.field, r));
        if dist.z(i, value) >= RECIPE_LINE {
            moods.push(mood.field);
            if r > best {
                dominant = mood.field;
                best = r;
            }
        }
    }
    MoodView {
        dominant,
        score: best,
        ranks,
        moods,
    }
}

/// What the vibe map serves for a track (`moodOutput`).
#[derive(Debug, Clone, PartialEq)]
pub struct MoodOutput {
    pub dominant_mood: &'static str,
    pub mood_score: f64,
    pub moods: Vec<(&'static str, f64)>,
}

pub fn mood_output(view: Option<&MoodView>) -> MoodOutput {
    match view {
        None => MoodOutput {
            dominant_mood: UNANALYZED,
            mood_score: 0.0,
            moods: vec![],
        },
        Some(v) => MoodOutput {
            dominant_mood: v.dominant,
            mood_score: v.score,
            moods: v.ranks.clone(),
        },
    }
}

/// A track as the collection page's tally sees it.
#[derive(Debug, Clone, Copy)]
pub struct SummaryTrack<'a> {
    pub dominant: &'a str,
    pub album_id: &'a str,
    pub artist_id: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Share {
    pub key: String,
    pub fraction: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MoodSummary {
    /// Each mood's share of the analysed tracks, biggest first, equal counts in legend order, neutral after the moods.
    pub shares: Vec<Share>,
    /// The mood most of an album's tracks share; a group whose winner is neutral is left out.
    pub albums: BTreeMap<String, String>,
    pub artists: BTreeMap<String, String>,
}

/// The legend's position of a key: the client fields in table order, anything else (neutral) after them.
fn legend_position(key: &str) -> usize {
    CLIENT_MOODS
        .iter()
        .position(|m| m.field == key)
        .unwrap_or(CLIENT_MOODS.len())
}

/// Tallies tracks' moods into the band and the per-group winners. Unanalysed (and empty) tracks are left out of
/// everything; any other value that is not a client mood field counts as neutral.
pub fn summarise(tracks: &[SummaryTrack<'_>]) -> MoodSummary {
    type Tally = BTreeMap<&'static str, usize>;
    fn key_of(dominant: &str) -> &'static str {
        CLIENT_MOODS
            .iter()
            .find(|m| m.field == dominant)
            .map_or(NEUTRAL, |m| m.field)
    }
    let mut total = Tally::new();
    let mut by_album: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut by_artist: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut count = 0usize;
    for t in tracks {
        if t.dominant.is_empty() || t.dominant == UNANALYZED {
            continue;
        }
        let key = key_of(t.dominant);
        count += 1;
        *total.entry(key).or_default() += 1;
        *by_album
            .entry(t.album_id)
            .or_default()
            .entry(key)
            .or_default() += 1;
        *by_artist
            .entry(t.artist_id)
            .or_default()
            .entry(key)
            .or_default() += 1;
    }
    let ordered = |tally: &Tally| -> Vec<(&'static str, usize)> {
        let mut v: Vec<_> = tally.iter().map(|(k, c)| (*k, *c)).collect();
        // Distinct keys have distinct legend positions, so the order is total.
        v.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(legend_position(a.0).cmp(&legend_position(b.0)))
        });
        v
    };
    let winners = |groups: &BTreeMap<&str, Tally>| -> BTreeMap<String, String> {
        groups
            .iter()
            .filter_map(|(id, t)| {
                let best = ordered(t).first().map(|b| b.0)?;
                (best != NEUTRAL).then(|| ((*id).to_owned(), best.to_owned()))
            })
            .collect()
    };
    MoodSummary {
        shares: ordered(&total)
            .into_iter()
            .map(|(key, c)| Share {
                key: key.to_owned(),
                fraction: c as f64 / count as f64,
            })
            .collect(),
        albums: winners(&by_album),
        artists: winners(&by_artist),
    }
}
