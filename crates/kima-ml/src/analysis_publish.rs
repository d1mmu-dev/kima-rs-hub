//! Async fenced publication adapter: bridges independently-computed
//! analysis-stage results (MusiCNN per-track aggregate, D-151/D-152; DSP
//! loudness/key/tempo, D-008/D-153) to `kima_db::analysis`'s per-group
//! `store_*` functions (migration 0026, D-154). Mirrors `publish.rs`'s
//! fenced async wrapper + `spawn_blocking` bridge pattern used for the CLAP
//! embedding stage, adapted for `track_analysis`'s four independently-
//! nullable groups: each group is published on its own, so one group's
//! storage rejection or absence must never prevent publishing the others.
//!
//! This module does not acquire claims, choose eligibility, run inference,
//! or register with any scheduler/claim/lease workflow for the 'analysis'
//! stage (today only 'embedding' has one, in `kima_db::ml`) — that remains
//! a separate, explicitly out-of-scope slice. It only turns already-
//! computed, already-validated results plus a caller-supplied
//! `file_generation` into independently-fenced `store_*` calls, mapping
//! each group's storage rejection to an error so a stale generation is
//! never reported as a successful publish for that group.
#![cfg(feature = "pg-publish")]

use kima_db::PgPool;
use kima_db::analysis;
use uuid::Uuid;

use crate::dsp::{
    KeyAnalysis as DspKeyAnalysis, KeyMode as DspKeyMode, LoudnessAnalysis as DspLoudnessAnalysis,
    TempoAnalysis as DspTempoAnalysis,
};
use crate::musicnn::MusicnnAggregate;

#[derive(Debug, thiserror::Error)]
pub enum AnalysisPublishError {
    #[error("storage error publishing analysis group: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("analysis group write rejected: stale, missing or deleted track generation")]
    Rejected,
}

/// `kima_db::analysis::store_*` returning `false` means the track moved
/// past the caller's `file_generation` (changed file, went missing, or was
/// deleted) since it was computed: never a quiet success. Keep this
/// mapping in one place, matching `publish.rs::classify`.
fn classify(accepted: bool) -> Result<(), AnalysisPublishError> {
    if accepted {
        Ok(())
    } else {
        Err(AnalysisPublishError::Rejected)
    }
}

/// Redeclared locally rather than importing `kima_db::analysis`'s type into
/// public call sites' field values, matching this schema's own convention
/// (`kima_db::analysis`'s doc comment: not reusing a `kima-ml` type, same
/// pattern as `ml::Provenance`) applied in the other direction: these
/// conversions are the one place that crosses the boundary.
pub fn to_db_musicnn(result: &MusicnnAggregate) -> analysis::MusicnnAnalysis {
    analysis::MusicnnAnalysis {
        danceability: result.danceability,
        valence: result.valence,
        arousal: result.arousal,
        mood_happy: result.mood_happy,
        mood_sad: result.mood_sad,
        mood_relaxed: result.mood_relaxed,
        mood_aggressive: result.mood_aggressive,
        mood_party: result.mood_party,
        mood_acoustic: result.mood_acoustic,
        mood_electronic: result.mood_electronic,
    }
}

pub fn to_db_loudness(result: &DspLoudnessAnalysis) -> analysis::LoudnessAnalysis {
    analysis::LoudnessAnalysis {
        integrated_lufs: result.integrated_lufs,
        loudness_range_lu: result.loudness_range_lu,
    }
}

pub fn to_db_key(result: &DspKeyAnalysis) -> analysis::KeyAnalysis {
    analysis::KeyAnalysis {
        // `DspKeyAnalysis::tonic` is `u8` in `0..=11` (pitch class); the
        // stored column is `smallint` (i16), so this widening never loses
        // information or wraps.
        tonic: i16::from(result.tonic),
        mode: match result.mode {
            DspKeyMode::Major => analysis::KeyMode::Major,
            DspKeyMode::Minor => analysis::KeyMode::Minor,
        },
        strength: result.strength,
    }
}

pub fn to_db_tempo(result: &DspTempoAnalysis) -> analysis::TempoAnalysis {
    analysis::TempoAnalysis {
        bpm: result.bpm,
        confidence: result.confidence,
    }
}

/// Fenced, independent publication of the MusiCNN aggregate group. No
/// transaction or connection is held beyond `store_musicnn`'s own short
/// transaction, and none spans decode or inference: call this only after
/// both have finished.
pub async fn publish_musicnn(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    pipeline_version: &str,
    result: &MusicnnAggregate,
) -> Result<(), AnalysisPublishError> {
    let accepted = analysis::store_musicnn(
        pool,
        track_id,
        file_generation,
        pipeline_version,
        &to_db_musicnn(result),
    )
    .await?;
    classify(accepted)
}

/// Fenced, independent publication of the DSP loudness group.
pub async fn publish_loudness(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    pipeline_version: &str,
    result: &DspLoudnessAnalysis,
) -> Result<(), AnalysisPublishError> {
    let accepted = analysis::store_loudness(
        pool,
        track_id,
        file_generation,
        pipeline_version,
        &to_db_loudness(result),
    )
    .await?;
    classify(accepted)
}

/// Fenced, independent publication of the DSP key group.
pub async fn publish_key(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    pipeline_version: &str,
    result: &DspKeyAnalysis,
) -> Result<(), AnalysisPublishError> {
    let accepted = analysis::store_key(
        pool,
        track_id,
        file_generation,
        pipeline_version,
        &to_db_key(result),
    )
    .await?;
    classify(accepted)
}

/// Fenced, independent publication of the DSP tempo group.
pub async fn publish_tempo(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    pipeline_version: &str,
    result: &DspTempoAnalysis,
) -> Result<(), AnalysisPublishError> {
    let accepted = analysis::store_tempo(
        pool,
        track_id,
        file_generation,
        pipeline_version,
        &to_db_tempo(result),
    )
    .await?;
    classify(accepted)
}

/// One attempt's computed results, each independently optional: a group a
/// caller did not compute (model unavailable, DSP failed, not attempted)
/// is simply absent here, not a fabricated error. Each present group
/// carries its own `pipeline_version` string, matching `kima_db::analysis`'s
/// own per-group versioning (no shared/implied version across groups).
#[derive(Debug, Default)]
pub struct AnalysisGroupInputs<'a> {
    pub musicnn: Option<(&'a str, &'a MusicnnAggregate)>,
    pub loudness: Option<(&'a str, &'a DspLoudnessAnalysis)>,
    pub key: Option<(&'a str, &'a DspKeyAnalysis)>,
    pub tempo: Option<(&'a str, &'a DspTempoAnalysis)>,
}

/// Per-group publication outcome: `None` means that group was absent from
/// `AnalysisGroupInputs` (never attempted), `Some(Err(_))` means it was
/// attempted and rejected/failed. Mirrors the independence of the
/// underlying `track_analysis` schema's per-group nullability.
#[derive(Debug, Default)]
pub struct AnalysisPublishReport {
    pub musicnn: Option<Result<(), AnalysisPublishError>>,
    pub loudness: Option<Result<(), AnalysisPublishError>>,
    pub key: Option<Result<(), AnalysisPublishError>>,
    pub tempo: Option<Result<(), AnalysisPublishError>>,
}

/// Publish every present group independently: one group's failure (storage
/// error or stale-generation rejection) never skips or aborts the others,
/// since each branch below runs unconditionally regardless of a sibling's
/// outcome (no `?` crossing group boundaries). The caller owns eligibility,
/// claims and a stable computed-against `file_generation`; this function
/// neither re-checks nor extends them, and acquires no claim/lease for any
/// 'analysis' stage workflow (none exists yet — see module docs).
pub async fn publish_track_analysis(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    inputs: &AnalysisGroupInputs<'_>,
) -> AnalysisPublishReport {
    let musicnn = match inputs.musicnn {
        Some((pipeline_version, result)) => {
            Some(publish_musicnn(pool, track_id, file_generation, pipeline_version, result).await)
        }
        None => None,
    };
    let loudness = match inputs.loudness {
        Some((pipeline_version, result)) => {
            Some(publish_loudness(pool, track_id, file_generation, pipeline_version, result).await)
        }
        None => None,
    };
    let key = match inputs.key {
        Some((pipeline_version, result)) => {
            Some(publish_key(pool, track_id, file_generation, pipeline_version, result).await)
        }
        None => None,
    };
    let tempo = match inputs.tempo {
        Some((pipeline_version, result)) => {
            Some(publish_tempo(pool, track_id, file_generation, pipeline_version, result).await)
        }
        None => None,
    };
    AnalysisPublishReport {
        musicnn,
        loudness,
        key,
        tempo,
    }
}

/// Bridge for a synchronous attempt-runner callback, matching this
/// workspace's existing convention of running blocking/sync work from async
/// call sites via `spawn_blocking` (see `publish.rs::publish_clap_embedding_blocking`).
/// Call this only from inside a `tokio::task::spawn_blocking` closure: that
/// thread is not part of the async worker pool, so blocking it on `handle`
/// here cannot starve other tasks. Calling it directly on a Tokio worker
/// thread risks a panic or deadlock and is not supported.
pub fn publish_track_analysis_blocking(
    handle: &tokio::runtime::Handle,
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    inputs: &AnalysisGroupInputs<'_>,
) -> AnalysisPublishReport {
    handle.block_on(publish_track_analysis(
        pool,
        track_id,
        file_generation,
        inputs,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn musicnn() -> MusicnnAggregate {
        MusicnnAggregate {
            mood_happy: 0.1,
            mood_sad: 0.2,
            mood_relaxed: 0.3,
            mood_aggressive: 0.4,
            mood_party: 0.5,
            mood_acoustic: 0.6,
            mood_electronic: 0.7,
            danceability: 0.5,
            valence: 0.5,
            arousal: 0.5,
        }
    }

    #[test]
    fn classify_maps_storage_acceptance_to_ok() {
        assert!(classify(true).is_ok());
    }

    #[test]
    fn classify_maps_storage_rejection_to_rejected_error_not_success() {
        assert!(matches!(
            classify(false),
            Err(AnalysisPublishError::Rejected)
        ));
    }

    #[test]
    fn to_db_musicnn_copies_every_field_without_rescaling() {
        let m = musicnn();
        let db = to_db_musicnn(&m);
        assert_eq!(db.danceability, m.danceability);
        assert_eq!(db.mood_electronic, m.mood_electronic);
    }

    #[test]
    fn to_db_key_widens_tonic_and_preserves_mode() {
        let k = DspKeyAnalysis {
            tonic: 11,
            mode: DspKeyMode::Minor,
            strength: 0.5,
        };
        let db = to_db_key(&k);
        assert_eq!(db.tonic, 11_i16);
        assert_eq!(db.mode, analysis::KeyMode::Minor);
    }

    #[test]
    fn empty_inputs_publish_nothing() {
        // AnalysisGroupInputs::default() has every group absent; this is a
        // compile/shape check that absence is representable without a DB.
        let inputs = AnalysisGroupInputs::default();
        assert!(inputs.musicnn.is_none());
        assert!(inputs.loudness.is_none());
        assert!(inputs.key.is_none());
        assert!(inputs.tempo.is_none());
    }
}
