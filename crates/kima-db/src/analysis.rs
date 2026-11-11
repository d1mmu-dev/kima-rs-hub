//! Stored MusiCNN + DSP analysis result (`track_analysis`,
//! `0026_track_analysis.sql`). Standalone persistence for already-accepted
//! compute contracts (D-151/D-152 MusiCNN aggregate, D-008/D-153 DSP
//! loudness/key/tempo); no attempt.rs/scheduler wiring and no real-model/
//! Essentia parity gate here (separate tasks). Deliberately decoupled from
//! `kima-ml`'s own result types (same pattern as `ml::Provenance` not
//! reusing a CLAP crate type): this crate stays the lower layer.
//!
//! Each of the four result groups (MusiCNN aggregate, loudness, key, tempo)
//! is stored and fenced independently, because DSP key/tempo can fail (or
//! simply not be attempted yet) independently of MusiCNN succeeding, and
//! vice versa (see the migration's "all-or-nothing per group" CHECK
//! constraints). All four share one `file_generation` fence on the same
//! row: they come from one shared-decode attempt on the same file bytes
//! (D-009), and `invalidate_track_ml()` deletes the whole row on any
//! tracked identity change, so a row can never mix columns from two
//! generations.
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// MusiCNN per-track aggregate (D-151 compute, D-152 aggregation). Every
/// field is already upstream's `round(clamp(mean(...), 0, 1), 3)` value;
/// this module performs no further scaling or rounding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MusicnnAnalysis {
    pub danceability: f32,
    pub valence: f32,
    pub arousal: f32,
    pub mood_happy: f32,
    pub mood_sad: f32,
    pub mood_relaxed: f32,
    pub mood_aggressive: f32,
    pub mood_party: f32,
    pub mood_acoustic: f32,
    pub mood_electronic: f32,
}

/// DSP integrated loudness (LUFS) and loudness range (LU), EBU R128 (D-153).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnessAnalysis {
    pub integrated_lufs: f64,
    pub loudness_range_lu: f64,
}

/// DSP key/tonality (D-008 accepted-with-params).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMode {
    Major,
    Minor,
}

impl KeyMode {
    fn as_db_str(self) -> &'static str {
        match self {
            KeyMode::Major => "major",
            KeyMode::Minor => "minor",
        }
    }

    fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "major" => Some(KeyMode::Major),
            "minor" => Some(KeyMode::Minor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyAnalysis {
    /// Tonic pitch class, 0=C, 1=C#, ..., 11=B.
    pub tonic: i16,
    pub mode: KeyMode,
    /// Pearson correlation of the chroma vector against the winning
    /// Krumhansl-Schmuckler profile, in `[-1.0, 1.0]`.
    pub strength: f64,
}

/// DSP tempo/BPM (D-008 accepted-with-params).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoAnalysis {
    pub bpm: f64,
    /// Normalized autocorrelation peak height, `[0.0, 1.0]`.
    pub confidence: f64,
}

/// The whole stored row, with each group `Option`-wrapped for an
/// independently-succeeded, independently-failed/not-yet-attempted result.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackAnalysis {
    pub file_generation: i64,
    pub musicnn: Option<MusicnnAnalysis>,
    pub loudness: Option<LoudnessAnalysis>,
    pub key: Option<KeyAnalysis>,
    pub tempo: Option<TempoAnalysis>,
}

/// Fence every write on the track still being at the exact generation the
/// caller computed against, under `FOR NO KEY UPDATE` (it blocks the scanner's writes and
/// deletes, not other stages' claims), matching
/// `ml::complete_embedding`'s generation-fencing pattern. `false` means the
/// track moved on (changed file, went missing, or was deleted) since the
/// caller captured `file_generation`: never a quiet partial write.
async fn fence_generation(
    tx: &mut sqlx::PgConnection,
    track_id: Uuid,
    file_generation: i64,
) -> sqlx::Result<bool> {
    let current = sqlx::query_scalar!(
        "SELECT id FROM tracks WHERE id=$1 AND ml_generation=$2 AND missing_since IS NULL FOR NO KEY UPDATE",
        track_id,
        file_generation
    )
    .fetch_optional(&mut *tx)
    .await?;
    Ok(current.is_some())
}

/// Store (or replace) this track's MusiCNN aggregate for `file_generation`.
/// Returns `false` without writing anything if the track has since moved
/// past that generation.
pub async fn store_musicnn(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    pipeline_version: &str,
    result: &MusicnnAnalysis,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    if !fence_generation(&mut tx, track_id, file_generation).await? {
        return Ok(false);
    }
    sqlx::query!(
        "INSERT INTO track_analysis
             (track_id, file_generation, musicnn_pipeline_version, musicnn_danceability,
              musicnn_valence, musicnn_arousal, musicnn_mood_happy, musicnn_mood_sad,
              musicnn_mood_relaxed, musicnn_mood_aggressive, musicnn_mood_party,
              musicnn_mood_acoustic, musicnn_mood_electronic, musicnn_computed_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,clock_timestamp())
         ON CONFLICT (track_id) DO UPDATE SET
             file_generation=EXCLUDED.file_generation,
             musicnn_pipeline_version=EXCLUDED.musicnn_pipeline_version,
             musicnn_danceability=EXCLUDED.musicnn_danceability,
             musicnn_valence=EXCLUDED.musicnn_valence,
             musicnn_arousal=EXCLUDED.musicnn_arousal,
             musicnn_mood_happy=EXCLUDED.musicnn_mood_happy,
             musicnn_mood_sad=EXCLUDED.musicnn_mood_sad,
             musicnn_mood_relaxed=EXCLUDED.musicnn_mood_relaxed,
             musicnn_mood_aggressive=EXCLUDED.musicnn_mood_aggressive,
             musicnn_mood_party=EXCLUDED.musicnn_mood_party,
             musicnn_mood_acoustic=EXCLUDED.musicnn_mood_acoustic,
             musicnn_mood_electronic=EXCLUDED.musicnn_mood_electronic,
             musicnn_computed_at=clock_timestamp(), updated_at=clock_timestamp()",
        track_id,
        file_generation,
        pipeline_version,
        result.danceability,
        result.valence,
        result.arousal,
        result.mood_happy,
        result.mood_sad,
        result.mood_relaxed,
        result.mood_aggressive,
        result.mood_party,
        result.mood_acoustic,
        result.mood_electronic,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Store (or replace) this track's DSP loudness result for `file_generation`.
pub async fn store_loudness(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    pipeline_version: &str,
    result: &LoudnessAnalysis,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    if !fence_generation(&mut tx, track_id, file_generation).await? {
        return Ok(false);
    }
    sqlx::query!(
        "INSERT INTO track_analysis
             (track_id, file_generation, dsp_loudness_pipeline_version, integrated_lufs,
              loudness_range_lu, loudness_computed_at)
         VALUES ($1,$2,$3,$4,$5,clock_timestamp())
         ON CONFLICT (track_id) DO UPDATE SET
             file_generation=EXCLUDED.file_generation,
             dsp_loudness_pipeline_version=EXCLUDED.dsp_loudness_pipeline_version,
             integrated_lufs=EXCLUDED.integrated_lufs,
             loudness_range_lu=EXCLUDED.loudness_range_lu,
             loudness_computed_at=clock_timestamp(), updated_at=clock_timestamp()",
        track_id,
        file_generation,
        pipeline_version,
        result.integrated_lufs,
        result.loudness_range_lu,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Store (or replace) this track's DSP key result for `file_generation`.
pub async fn store_key(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    pipeline_version: &str,
    result: &KeyAnalysis,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    if !fence_generation(&mut tx, track_id, file_generation).await? {
        return Ok(false);
    }
    let mode = result.mode.as_db_str();
    sqlx::query!(
        "INSERT INTO track_analysis
             (track_id, file_generation, dsp_key_pipeline_version, key_tonic, key_mode,
              key_strength, key_computed_at)
         VALUES ($1,$2,$3,$4,$5,$6,clock_timestamp())
         ON CONFLICT (track_id) DO UPDATE SET
             file_generation=EXCLUDED.file_generation,
             dsp_key_pipeline_version=EXCLUDED.dsp_key_pipeline_version,
             key_tonic=EXCLUDED.key_tonic,
             key_mode=EXCLUDED.key_mode,
             key_strength=EXCLUDED.key_strength,
             key_computed_at=clock_timestamp(), updated_at=clock_timestamp()",
        track_id,
        file_generation,
        pipeline_version,
        result.tonic,
        mode,
        result.strength,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Store (or replace) this track's DSP tempo result for `file_generation`.
pub async fn store_tempo(
    pool: &PgPool,
    track_id: Uuid,
    file_generation: i64,
    pipeline_version: &str,
    result: &TempoAnalysis,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    if !fence_generation(&mut tx, track_id, file_generation).await? {
        return Ok(false);
    }
    sqlx::query!(
        "INSERT INTO track_analysis
             (track_id, file_generation, dsp_tempo_pipeline_version, tempo_bpm,
              tempo_confidence, tempo_computed_at)
         VALUES ($1,$2,$3,$4,$5,clock_timestamp())
         ON CONFLICT (track_id) DO UPDATE SET
             file_generation=EXCLUDED.file_generation,
             dsp_tempo_pipeline_version=EXCLUDED.dsp_tempo_pipeline_version,
             tempo_bpm=EXCLUDED.tempo_bpm,
             tempo_confidence=EXCLUDED.tempo_confidence,
             tempo_computed_at=clock_timestamp(), updated_at=clock_timestamp()",
        track_id,
        file_generation,
        pipeline_version,
        result.bpm,
        result.confidence,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

#[derive(sqlx::FromRow)]
struct Row {
    file_generation: i64,
    musicnn_pipeline_version: Option<String>,
    musicnn_danceability: Option<f32>,
    musicnn_valence: Option<f32>,
    musicnn_arousal: Option<f32>,
    musicnn_mood_happy: Option<f32>,
    musicnn_mood_sad: Option<f32>,
    musicnn_mood_relaxed: Option<f32>,
    musicnn_mood_aggressive: Option<f32>,
    musicnn_mood_party: Option<f32>,
    musicnn_mood_acoustic: Option<f32>,
    musicnn_mood_electronic: Option<f32>,
    dsp_loudness_pipeline_version: Option<String>,
    integrated_lufs: Option<f64>,
    loudness_range_lu: Option<f64>,
    dsp_key_pipeline_version: Option<String>,
    key_tonic: Option<i16>,
    key_mode: Option<String>,
    key_strength: Option<f64>,
    dsp_tempo_pipeline_version: Option<String>,
    tempo_bpm: Option<f64>,
    tempo_confidence: Option<f64>,
}

/// Read every stored group for a track at its *current* file generation
/// only (joined against `tracks.ml_generation`, matching `ml::embedding`'s
/// staleness fence); a stale/invalidated row never surfaces here. Returns
/// `None` if nothing has ever been stored for the current generation.
/// Pipeline versions are returned alongside each group so a caller can
/// still reject a result computed by a pipeline version it no longer
/// trusts, without this module hard-coding model-mixing policy.
pub async fn analysis(pool: &PgPool, track_id: Uuid) -> sqlx::Result<Option<TrackAnalysisRow>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT a.file_generation, a.musicnn_pipeline_version, a.musicnn_danceability,
                a.musicnn_valence, a.musicnn_arousal, a.musicnn_mood_happy, a.musicnn_mood_sad,
                a.musicnn_mood_relaxed, a.musicnn_mood_aggressive, a.musicnn_mood_party,
                a.musicnn_mood_acoustic, a.musicnn_mood_electronic,
                a.dsp_loudness_pipeline_version, a.integrated_lufs, a.loudness_range_lu,
                a.dsp_key_pipeline_version, a.key_tonic, a.key_mode, a.key_strength,
                a.dsp_tempo_pipeline_version, a.tempo_bpm, a.tempo_confidence
         FROM track_analysis a
         JOIN tracks t ON t.id = a.track_id AND t.ml_generation = a.file_generation
         WHERE t.id = $1 AND t.missing_since IS NULL",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| {
        let musicnn = r.musicnn_pipeline_version.clone().map(|pipeline_version| {
            (
                pipeline_version,
                MusicnnAnalysis {
                    danceability: r.musicnn_danceability.expect("all-or-nothing group"),
                    valence: r.musicnn_valence.expect("all-or-nothing group"),
                    arousal: r.musicnn_arousal.expect("all-or-nothing group"),
                    mood_happy: r.musicnn_mood_happy.expect("all-or-nothing group"),
                    mood_sad: r.musicnn_mood_sad.expect("all-or-nothing group"),
                    mood_relaxed: r.musicnn_mood_relaxed.expect("all-or-nothing group"),
                    mood_aggressive: r.musicnn_mood_aggressive.expect("all-or-nothing group"),
                    mood_party: r.musicnn_mood_party.expect("all-or-nothing group"),
                    mood_acoustic: r.musicnn_mood_acoustic.expect("all-or-nothing group"),
                    mood_electronic: r.musicnn_mood_electronic.expect("all-or-nothing group"),
                },
            )
        });
        let loudness = r
            .dsp_loudness_pipeline_version
            .clone()
            .map(|pipeline_version| {
                (
                    pipeline_version,
                    LoudnessAnalysis {
                        integrated_lufs: r.integrated_lufs.expect("all-or-nothing group"),
                        loudness_range_lu: r.loudness_range_lu.expect("all-or-nothing group"),
                    },
                )
            });
        let key = r.dsp_key_pipeline_version.clone().map(|pipeline_version| {
            (
                pipeline_version,
                KeyAnalysis {
                    tonic: r.key_tonic.expect("all-or-nothing group"),
                    mode: KeyMode::from_db_str(
                        r.key_mode.as_deref().expect("all-or-nothing group"),
                    )
                    .expect("key_mode CHECK constraint guarantees major|minor"),
                    strength: r.key_strength.expect("all-or-nothing group"),
                },
            )
        });
        let tempo = r
            .dsp_tempo_pipeline_version
            .clone()
            .map(|pipeline_version| {
                (
                    pipeline_version,
                    TempoAnalysis {
                        bpm: r.tempo_bpm.expect("all-or-nothing group"),
                        confidence: r.tempo_confidence.expect("all-or-nothing group"),
                    },
                )
            });
        TrackAnalysisRow {
            file_generation: r.file_generation,
            musicnn,
            loudness,
            key,
            tempo,
        }
    }))
}

/// `analysis`'s return shape: each group alongside the pipeline version it
/// was computed with, so a caller choosing to trust only a specific
/// pipeline version can filter itself without a second round trip.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackAnalysisRow {
    pub file_generation: i64,
    pub musicnn: Option<(String, MusicnnAnalysis)>,
    pub loudness: Option<(String, LoudnessAnalysis)>,
    pub key: Option<(String, KeyAnalysis)>,
    pub tempo: Option<(String, TempoAnalysis)>,
}

/// Convenience re-export of the computed_at timestamp type used by the
/// stored row's `*_computed_at` columns, for callers that want to read it
/// directly via a raw query rather than through `analysis`.
pub type ComputedAt = OffsetDateTime;
