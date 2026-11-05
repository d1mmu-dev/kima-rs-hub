//! Per-track reduction of `MusicnnPatchOutputs` into one aggregate, grounded
//! strictly in the pinned upstream `AudioAnalyzer._extract_ml_features` /
//! `positive_prob` / `HEAD_CLASSES` / `HEAD_POSITIVE_CLASS` (Python,
//! `services/audio-analyzer/analyzer.py` at commit
//! `12faf1cd34ff53054221818b0ccde58b8e9b0020`, SHA-256
//! `61e8d09a050d55c42b29a876ae80ffeee591c522923d42a4f1aca0346ff69559`).
//!
//! Pure computation only: no persistence, no DSP (`bpm`/`key`/`loudness`),
//! no mood-tag generation (`_generate_mood_tags`, which additionally needs
//! `bpm`/`keyScale` that do not come from MusiCNN), and no genre (Essentia's
//! own genre classifier, not a MusiCNN head). Those remain out of scope for
//! this aggregation contract and must not be fabricated here.
//!
//! Upstream per-head reduction (verbatim from `analyzer.py`):
//! - The eight binary heads (`danceability` plus the seven `mood_*` heads)
//!   each reduce to `round(clamp(mean(column_for_positive_class), 0, 1), 3)`
//!   — the mean *of the positive class's probability column* over all
//!   patches, where the column is chosen by `positive_class_index`, itself
//!   derived from the per-head `HEAD_CLASSES`/`HEAD_POSITIVE_CLASS` tables
//!   below (also transcribed verbatim).
//! - `deam` and `emomusic` are 2-column `[valence, arousal]` regressions on
//!   a 1-9 scale. `valence_raw`/`arousal_raw` are the *mean of the two
//!   heads' own per-patch column means* (mean-of-means, not a combined
//!   per-patch mean over both heads together). Both are then "temperature
//!   scaled" around a centre of 5.0 by a factor of 2.5, clamped to `[1,9]`,
//!   then linearly rescaled to `[0,1]` and rounded to 3 decimals.
//! - `danceabilityMl` and `danceability` are upstream's *same* value
//!   (duplicated storage columns); this struct exposes it once as
//!   `danceability`.
//!
//! `round(x, 3)` here is implemented as round-half-away-from-zero on the
//! `f64` accumulator (`(x * 1000.0).round() / 1000.0`), not CPython's
//! round-half-to-even on the exact double value. The two differ only at an
//! exact `*.*5` boundary of the intermediate f64, a measure-zero case for
//! values produced by averaging float32 model output; this is a documented
//! implementation choice, not a different aggregation rule.

use super::{MusicnnComputation, MusicnnPatchOutputs};
use crate::onnx::MusiCnnHead;

/// One track's aggregated MusiCNN result. Every field is already the
/// upstream `round(clamp(...), 3)` value, stored as `f32` (the model's own
/// precision); no further scaling, clamping or rounding should be applied
/// downstream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MusicnnAggregate {
    pub mood_happy: f32,
    pub mood_sad: f32,
    pub mood_relaxed: f32,
    pub mood_aggressive: f32,
    pub mood_party: f32,
    pub mood_acoustic: f32,
    pub mood_electronic: f32,
    /// Upstream's single `danceability`/`danceabilityMl` value.
    pub danceability: f32,
    pub valence: f32,
    pub arousal: f32,
}

/// The aggregation outcome. `NoFullPatch` passes the computation's own
/// sub-patch case straight through — never a fabricated neutral aggregate,
/// matching upstream's `predict()` returning `None` for every head on zero
/// patches (no row is averaged, no track-level feature exists).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MusicnnAnalysisResult {
    NoFullPatch,
    Aggregate(MusicnnAggregate),
}

/// Reduce a full `MusicnnComputation` outcome to its per-track aggregate.
pub fn aggregate(computation: &MusicnnComputation) -> MusicnnAnalysisResult {
    match computation {
        MusicnnComputation::NoFullPatch => MusicnnAnalysisResult::NoFullPatch,
        MusicnnComputation::Patches(patches) => {
            MusicnnAnalysisResult::Aggregate(aggregate_patches(patches))
        }
    }
}

/// Reduce already-unwrapped patch outputs (the non-empty case) directly.
pub fn aggregate_patches(patches: &MusicnnPatchOutputs) -> MusicnnAggregate {
    let heads = patches.heads();
    let positive = |head: MusiCnnHead| positive_prob(&heads[&head], positive_class_index(head));
    let deam = &heads[&MusiCnnHead::Deam];
    let emomusic = &heads[&MusiCnnHead::Emomusic];
    let valence_raw = (mean_column(deam, 0) + mean_column(emomusic, 0)) / 2.0;
    let arousal_raw = (mean_column(deam, 1) + mean_column(emomusic, 1)) / 2.0;
    const CENTER: f64 = 5.0;
    const TEMPERATURE: f64 = 2.5;
    let valence_scaled = (CENTER + (valence_raw - CENTER) * TEMPERATURE).clamp(1.0, 9.0);
    let arousal_scaled = (CENTER + (arousal_raw - CENTER) * TEMPERATURE).clamp(1.0, 9.0);
    let valence = round3(((valence_scaled - 1.0) / 8.0).clamp(0.0, 1.0));
    let arousal = round3(((arousal_scaled - 1.0) / 8.0).clamp(0.0, 1.0));
    MusicnnAggregate {
        mood_happy: positive(MusiCnnHead::MoodHappy),
        mood_sad: positive(MusiCnnHead::MoodSad),
        mood_relaxed: positive(MusiCnnHead::MoodRelaxed),
        mood_aggressive: positive(MusiCnnHead::MoodAggressive),
        mood_party: positive(MusiCnnHead::MoodParty),
        mood_acoustic: positive(MusiCnnHead::MoodAcoustic),
        mood_electronic: positive(MusiCnnHead::MoodElectronic),
        danceability: positive(MusiCnnHead::Danceability),
        valence,
        arousal,
    }
}

/// The column of each binary head's 2-wide softmax output holding the
/// positive class, transcribed verbatim from `analyzer.py`'s
/// `HEAD_CLASSES`/`HEAD_POSITIVE_CLASS`/`positive_class_index`. `Deam` and
/// `Emomusic` are regressions, not binary classifiers, and are never passed
/// here (see `aggregate_patches`'s separate valence/arousal path).
fn positive_class_index(head: MusiCnnHead) -> usize {
    match head {
        // classes=['danceable', 'not_danceable'], positive='danceable'
        MusiCnnHead::Danceability => 0,
        // classes=['happy', 'non_happy'], positive='happy'
        MusiCnnHead::MoodHappy => 0,
        // classes=['non_sad', 'sad'], positive='sad'
        MusiCnnHead::MoodSad => 1,
        // classes=['non_relaxed', 'relaxed'], positive='relaxed'
        MusiCnnHead::MoodRelaxed => 1,
        // classes=['aggressive', 'not_aggressive'], positive='aggressive'
        MusiCnnHead::MoodAggressive => 0,
        // classes=['non_party', 'party'], positive='party'
        MusiCnnHead::MoodParty => 1,
        // classes=['acoustic', 'non_acoustic'], positive='acoustic'
        MusiCnnHead::MoodAcoustic => 0,
        // classes=['electronic', 'non_electronic'], positive='electronic'
        MusiCnnHead::MoodElectronic => 0,
        MusiCnnHead::Deam | MusiCnnHead::Emomusic => {
            unreachable!("Deam/Emomusic are regressions, not binary heads")
        }
    }
}

/// `round(clamp(mean(column), 0, 1), 3)`, upstream's `positive_prob`.
fn positive_prob(rows: &[[f32; 2]], column: usize) -> f32 {
    round3(mean_column(rows, column).clamp(0.0, 1.0))
}

fn mean_column(rows: &[[f32; 2]], column: usize) -> f64 {
    rows.iter().map(|r| f64::from(r[column])).sum::<f64>() / rows.len() as f64
}

fn round3(x: f64) -> f32 {
    ((x * 1000.0).round() / 1000.0) as f32
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn patches(
        rows: usize,
        heads: impl IntoIterator<Item = (MusiCnnHead, Vec<[f32; 2]>)>,
    ) -> MusicnnPatchOutputs {
        MusicnnPatchOutputs::try_new(vec![[0.0; 200]; rows], heads.into_iter().collect()).unwrap()
    }

    fn all_heads_constant(rows: usize, value: [f32; 2]) -> BTreeMap<MusiCnnHead, Vec<[f32; 2]>> {
        MusiCnnHead::all()
            .iter()
            .map(|&h| (h, vec![value; rows]))
            .collect()
    }

    #[test]
    fn binary_heads_reduce_to_rounded_clamped_mean_of_positive_column() {
        // mood_happy positive column is 0: patches average to 0.123456 -> round to 0.123.
        let mut heads = all_heads_constant(1, [0.0, 0.0]);
        heads.insert(
            MusiCnnHead::MoodHappy,
            vec![[0.1, 0.9], [0.14, 0.86], [0.13, 0.87]],
        );
        heads.insert(MusiCnnHead::Deam, vec![[5.0, 5.0]; 1]);
        heads.insert(MusiCnnHead::Emomusic, vec![[5.0, 5.0]; 1]);
        let MusicnnAggregate { mood_happy, .. } =
            aggregate_patches(&patches(3, heads_with_matching_rows(heads, 3)));
        // mean(0.1, 0.14, 0.13) = 0.123333... -> round(.,3) = 0.123
        assert_eq!(mood_happy, 0.123);
    }

    fn heads_with_matching_rows(
        mut heads: BTreeMap<MusiCnnHead, Vec<[f32; 2]>>,
        rows: usize,
    ) -> BTreeMap<MusiCnnHead, Vec<[f32; 2]>> {
        for (head, values) in heads.iter_mut() {
            if values.len() != rows {
                let fill = values.last().copied().unwrap_or([0.0, 0.0]);
                *values = vec![fill; rows];
            }
            let _ = head;
        }
        heads
    }

    #[test]
    fn positive_column_selection_matches_upstream_head_classes_table() {
        // One patch per head; put a distinguishing pair [low, high] through every head
        // and check which column each reports, matching positive_class_index.
        let pair = [0.2_f32, 0.8_f32];
        let mut heads = all_heads_constant(1, pair);
        heads.insert(MusiCnnHead::Deam, vec![[5.0, 5.0]]);
        heads.insert(MusiCnnHead::Emomusic, vec![[5.0, 5.0]]);
        let agg = aggregate_patches(&patches(1, heads));
        // column 0 heads report 0.2, column 1 heads report 0.8.
        assert_eq!(agg.danceability, 0.2); // column 0
        assert_eq!(agg.mood_happy, 0.2); // column 0
        assert_eq!(agg.mood_aggressive, 0.2); // column 0
        assert_eq!(agg.mood_acoustic, 0.2); // column 0
        assert_eq!(agg.mood_electronic, 0.2); // column 0
        assert_eq!(agg.mood_sad, 0.8); // column 1
        assert_eq!(agg.mood_relaxed, 0.8); // column 1
        assert_eq!(agg.mood_party, 0.8); // column 1
    }

    #[test]
    fn binary_head_mean_is_clamped_into_zero_one_even_if_inputs_would_exceed_it() {
        // Softmax outputs are already in [0,1], but the clamp is upstream's explicit
        // defensive max(0,min(1,...)); exercise it directly with out-of-range rows.
        let mut heads = all_heads_constant(2, [0.0, 0.0]);
        heads.insert(MusiCnnHead::Danceability, vec![[1.5, -0.5], [1.5, -0.5]]);
        heads.insert(MusiCnnHead::Deam, vec![[5.0, 5.0]; 2]);
        heads.insert(MusiCnnHead::Emomusic, vec![[5.0, 5.0]; 2]);
        let agg = aggregate_patches(&patches(2, heads));
        assert_eq!(agg.danceability, 1.0);
    }

    #[test]
    fn valence_arousal_use_mean_of_means_then_temperature_scale_then_rescale() {
        // deam mean valence=6.0 arousal=4.0 over 2 patches; emomusic mean valence=4.0 arousal=6.0.
        let mut heads = all_heads_constant(2, [0.0, 0.0]);
        heads.insert(MusiCnnHead::Deam, vec![[7.0, 3.0], [5.0, 5.0]]);
        heads.insert(MusiCnnHead::Emomusic, vec![[3.0, 7.0], [5.0, 5.0]]);
        let agg = aggregate_patches(&patches(2, heads));
        // deam: valence mean 6.0, arousal mean 4.0. emomusic: valence mean 4.0, arousal mean 6.0.
        // valence_raw = (6.0+4.0)/2 = 5.0 = center -> scaled stays 5.0 -> (5-1)/8 = 0.5
        assert_eq!(agg.valence, 0.5);
        assert_eq!(agg.arousal, 0.5);
    }

    #[test]
    fn valence_arousal_temperature_scaling_and_clamp_to_one_nine_before_rescale() {
        // Push valence_raw far from center so the *9 temperature* clamp (not just the
        // final 0-1 clamp) is what bounds the result.
        let mut heads = all_heads_constant(1, [0.0, 0.0]);
        heads.insert(MusiCnnHead::Deam, vec![[9.0, 1.0]]);
        heads.insert(MusiCnnHead::Emomusic, vec![[9.0, 1.0]]);
        let agg = aggregate_patches(&patches(1, heads));
        // valence_raw = 9.0; scaled = 5.0 + (9.0-5.0)*2.5 = 15.0 -> clamp to 9.0 -> (9-1)/8=1.0
        assert_eq!(agg.valence, 1.0);
        // arousal_raw = 1.0; scaled = 5.0 + (1.0-5.0)*2.5 = -5.0 -> clamp to 1.0 -> (1-1)/8=0.0
        assert_eq!(agg.arousal, 0.0);
    }

    #[test]
    fn no_full_patch_passes_through_without_a_fabricated_aggregate() {
        assert_eq!(
            aggregate(&MusicnnComputation::NoFullPatch),
            MusicnnAnalysisResult::NoFullPatch
        );
    }

    #[test]
    fn full_patches_wrap_the_same_aggregate_as_aggregate_patches() {
        let heads = all_heads_constant(1, [0.4, 0.6]);
        let mut heads = heads;
        heads.insert(MusiCnnHead::Deam, vec![[5.0, 5.0]]);
        heads.insert(MusiCnnHead::Emomusic, vec![[5.0, 5.0]]);
        let p = patches(1, heads);
        let direct = aggregate_patches(&p);
        let MusicnnAnalysisResult::Aggregate(wrapped) = aggregate(&MusicnnComputation::Patches(p))
        else {
            panic!("expected Aggregate")
        };
        assert_eq!(direct, wrapped);
    }
}
