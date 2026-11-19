//! The track rows of a mix and of a mood page (M5b Tasks 9 and 10; upstream `routes/mixes.ts` `loadTracksInMixOrder`,
//! owner Q6 = B, D-264): every column the reply maps onto upstream's Prisma `Track`, its `album` and the album's
//! `artist`, read in one query. The API shapes them (`kima_api_types::mixes::MixTrack`); a column kima does not
//! have is not read here at all.

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::vibe::ELIGIBLE;

/// One playable track with what its reply row carries.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct TrackRow {
    pub id: Uuid,
    pub album_id: Uuid,
    pub title: String,
    pub track_no: i32,
    pub duration_secs: i32,
    /// Relative to the music root, as upstream's `filePath`.
    pub path: String,
    pub mtime: OffsetDateTime,
    pub size: i64,
    // The current `track_analysis` row (of the file the track holds now).
    pub tempo_bpm: Option<f64>,
    /// 0 to 1 (kima's scale).
    pub tempo_confidence: Option<f64>,
    /// Pitch class 0 = C .. 11 = B.
    pub key_tonic: Option<i16>,
    pub key_mode: Option<String>,
    pub key_strength: Option<f64>,
    pub integrated_lufs: Option<f64>,
    pub loudness_range_lu: Option<f64>,
    pub musicnn_danceability: Option<f64>,
    // The current CLAP reading.
    pub valence_raw: Option<f64>,
    pub arousal_raw: Option<f64>,
    pub instrumentalness: Option<f64>,
    pub acousticness: Option<f64>,
    pub mood_tags: Vec<String>,
    // The `analysis` stage row.
    pub analysis_status: Option<String>,
    pub analysis_started_at: Option<OffsetDateTime>,
    pub analysis_finished_at: Option<OffsetDateTime>,
    pub analysis_error: Option<String>,
    pub analysis_attempts: Option<i16>,
    pub updated_at: OffsetDateTime,
    pub display_title: Option<String>,
    // The `embedding` stage row.
    pub vibe_status: Option<String>,
    pub vibe_started_at: Option<OffsetDateTime>,
    pub vibe_error: Option<String>,
    pub vibe_attempts: Option<i16>,
    pub vibe_updated_at: Option<OffsetDateTime>,
    pub isrc: Option<String>,
    pub isrc_source: Option<String>,
    pub recording_mbid: Option<Uuid>,
    pub missing_since: Option<OffsetDateTime>,
    pub disc: i32,
    // The album.
    pub album_rg_mbid: Option<Uuid>,
    pub album_rg_mbid_source: Option<String>,
    pub album_artist_id: Uuid,
    pub album_title: String,
    pub album_sort_name: String,
    pub album_year: Option<i32>,
    pub album_has_image: bool,
    pub album_image_source: Option<String>,
    pub album_label: Option<String>,
    pub album_display_title: Option<String>,
    pub album_display_year: Option<i32>,
    pub album_user_genres: Option<Vec<String>>,
    // The album's artist.
    pub artist_name: String,
    pub artist_mbid: Option<Uuid>,
}

/// Upstream `loadTracksInMixOrder`: the playable tracks among `ids`, in the order `ids` lists them (a repeated id
/// repeats), unknown and missing ones left out.
pub async fn track_rows(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<Vec<TrackRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let eligible: &str = &ELIGIBLE;
    sqlx::query_as(&format!(
        "SELECT t.id, t.album_id, t.title, t.track_no, t.duration_secs, t.path, t.mtime, t.size,
                ta.tempo_bpm, ta.tempo_confidence, ta.key_tonic, ta.key_mode, ta.key_strength,
                ta.integrated_lufs, ta.loudness_range_lu, ta.musicnn_danceability::float8 AS musicnn_danceability,
                r.valence_raw::float8 AS valence_raw, r.arousal_raw::float8 AS arousal_raw,
                r.instrumentalness::float8 AS instrumentalness, r.acousticness::float8 AS acousticness,
                t.mood_tags,
                sa.status AS analysis_status, sa.started_at AS analysis_started_at,
                sa.finished_at AS analysis_finished_at, sa.last_error AS analysis_error,
                sa.attempts AS analysis_attempts,
                t.updated_at, t.display_title,
                se.status AS vibe_status, se.started_at AS vibe_started_at, se.last_error AS vibe_error,
                se.attempts AS vibe_attempts, se.updated_at AS vibe_updated_at,
                t.isrc, t.isrc_source, t.recording_mbid, t.missing_since, t.disc,
                a.release_group_mbid AS album_rg_mbid, a.release_group_mbid_source AS album_rg_mbid_source,
                a.artist_id AS album_artist_id, a.title AS album_title, a.sort_name AS album_sort_name,
                a.year AS album_year, (a.image_path IS NOT NULL) AS album_has_image,
                a.image_source AS album_image_source, a.label AS album_label,
                a.display_title AS album_display_title, a.display_year AS album_display_year,
                a.user_genres AS album_user_genres,
                ar.name AS artist_name, ar.mbid AS artist_mbid
         FROM unnest($1::uuid[]) WITH ORDINALITY u(id, ord)
         JOIN tracks t ON t.id = u.id
         JOIN albums a ON a.id = t.album_id
         JOIN artists ar ON ar.id = a.artist_id
         LEFT JOIN track_analysis ta ON ta.track_id = t.id AND ta.file_generation = t.ml_generation
         LEFT JOIN (SELECT te.track_id, te.created_at FROM {eligible}) e ON e.track_id = t.id
         LEFT JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at
         LEFT JOIN track_stages sa ON sa.track_id = t.id AND sa.stage = 'analysis'
         LEFT JOIN track_stages se ON se.track_id = t.id AND se.stage = 'embedding'
         WHERE t.missing_since IS NULL
         ORDER BY u.ord"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await
}
