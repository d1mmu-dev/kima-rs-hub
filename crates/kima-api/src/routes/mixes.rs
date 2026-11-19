//! `/api/mixes` (M5b Task 9; upstream `routes/mixes.ts`): the day's Home mixes per profile and zone, one mix with
//! its track rows (owner Q6 = B, D-264), a refresh, and saving a mix as a playlist. The lists are kept in the
//! in-process listener store (owner Q1 = A, D-260) per profile and zone for an hour, as upstream keeps them in Redis.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use kima_api_types::mixes::{MixAlbum, MixArtist, MixTrack};
use kima_core::profile::ProfileId;
use kima_db::PgPool;
use kima_db::mixes::daily::{DailyDraw, DailyInputs, generate_all_mixes};
use kima_db::mixes::generators::SimilarArtists;
use kima_db::mixes::night::{night_zone, server_zone};
use kima_db::mixes::save::{SaveMix, save_mix_playlist};
use kima_db::mixes::track_rows::{TrackRow, track_rows};
use kima_ident::similar;
use kima_ident::stages::{moods, similarity};
use kima_mixes::day::server_mix_day;
use kima_mixes::mix::ESSENTIA_KEYS;
use kima_mixes::store::FamiliarTiming;
use kima_vibe::feel_probe::FeelProbe;
use kima_vibe::mood_heads::MoodHeads;
use serde::Serialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::playlists::announce;
use super::plays::iso;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/mixes", get(list))
        // `GET /mixes/refresh` is upstream's `GET /:id` with id `refresh` (Express falls through), not a 405.
        .route("/mixes/refresh", post(refresh).get(one_refresh))
        .route("/mixes/{id}/save", post(save))
        .route("/mixes/{id}", get(one))
}

const MIX_NOT_FOUND: &str = "Mix not found";

fn failure(message: &'static str, error: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %error, "{message}");
    ApiError::Failed(message)
}

/// Upstream `requestedZone`: `?tz=` when sent.
fn requested_zone(params: &HashMap<String, String>) -> Option<&str> {
    params.get("tz").map(String::as_str)
}

/// The stored similar artists of a library artist as library artists (upstream `getSimilarArtists(id, 10)`: best
/// match first), resolved by MBID then normalised name (D-190, D-272).
struct StoredSimilar;

impl SimilarArtists for StoredSimilar {
    fn library_artists<'a>(
        &'a self,
        pool: &'a PgPool,
        stored: &'a Value,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = sqlx::Result<Vec<Uuid>>> + Send + 'a>> {
        Box::pin(async move {
            let mut entries = similar::stored_entries(stored);
            entries.sort_by(|a, b| b.r#match.total_cmp(&a.r#match));
            Ok(similar::resolve(pool, &entries, limit)
                .await?
                .into_iter()
                .filter(|found| found.in_library)
                .filter_map(|found| found.id.parse::<Uuid>().ok())
                .collect())
        })
    }
}

/// What the generators read from the running engine: the mood heads and the feel probe, each `None` while
/// unavailable.
async fn engine(state: &AppState) -> (Option<Arc<MoodHeads>>, Option<Arc<FeelProbe>>) {
    let ctx = state.ident_ctx();
    let heads = moods::runtime(&ctx).await.map(|rt| rt.heads().clone());
    let probe = similarity::runtime(&ctx).await.map(|rt| rt.probe.clone());
    (heads, probe)
}

/// Upstream `generateAllMixes` for this profile and zone; `refresh` is its `forceRandom`.
async fn generate(state: &AppState, profile: ProfileId, zone: &str, refresh: bool) -> Value {
    let (heads, probe) = engine(state).await;
    let inputs = DailyInputs {
        heads: heads.as_deref(),
        probe: probe.as_deref(),
        store: &state.listeners,
        similar: &StoredSimilar,
        familiar: FamiliarTiming::default(),
    };
    let today = server_mix_day();
    let random = refresh.then(|| {
        let ms = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
        (ms, rand::random::<f64>())
    });
    let draw = DailyDraw {
        profile,
        today: &today,
        zone,
        random,
    };
    let mixes = generate_all_mixes(&state.pool, inputs, draw).await;
    serde_json::to_value(mixes).unwrap_or_else(|_| json!([]))
}

/// Upstream `getMixesForUser`: the list kept for this profile and zone while under an hour old, else a new one,
/// kept.
async fn mixes_for(
    state: &AppState,
    profile: ProfileId,
    requested: Option<&str>,
) -> sqlx::Result<Arc<Value>> {
    let zone = night_zone(&state.pool, requested, &server_zone()).await?;
    let key = profile.into_uuid();
    if let Some(hit) = state.listeners.mix_list(key, &zone, Instant::now()) {
        return Ok(hit);
    }
    let mixes = Arc::new(generate(state, profile, &zone, false).await);
    state
        .listeners
        .put_mix_list(key, &zone, mixes.clone(), Instant::now());
    Ok(mixes)
}

fn find_mix<'a>(mixes: &'a Value, id: &str) -> Option<&'a serde_json::Map<String, Value>> {
    mixes
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .find(|m| m.get("id").and_then(Value::as_str) == Some(id))
}

fn mix_track_ids(mix: &serde_json::Map<String, Value>) -> Vec<Uuid> {
    mix.get("trackIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str()?.parse().ok())
        .collect()
}

async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let mixes = mixes_for(&state, user.profile, requested_zone(&params))
        .await
        .map_err(|e| failure("Failed to get mixes", e))?;
    Ok(Json((*mixes).clone()))
}

async fn refresh(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let zone = night_zone(&state.pool, requested_zone(&params), &server_zone())
        .await
        .map_err(|e| failure("Failed to refresh mixes", e))?;
    let mixes = Arc::new(generate(&state, user.profile, &zone, true).await);
    state.listeners.put_mix_list(
        user.profile.into_uuid(),
        &zone,
        mixes.clone(),
        Instant::now(),
    );
    Ok(Json(
        json!({ "message": "Mixes refreshed", "mixes": *mixes }),
    ))
}

async fn one(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    one_mix(&state, &user, &id, &params).await
}

async fn one_refresh(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    one_mix(&state, &user, "refresh", &params).await
}

/// Upstream `GET /:id`: the mix with its track rows.
async fn one_mix(
    state: &AppState,
    user: &AuthUser,
    id: &str,
    params: &HashMap<String, String>,
) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to get mix";
    let mixes = mixes_for(state, user.profile, requested_zone(params))
        .await
        .map_err(|e| failure(ERROR, e))?;
    let mix = find_mix(&mixes, id).ok_or(ApiError::NotFound(MIX_NOT_FOUND))?;
    let tracks = mix_tracks(&state.pool, &mix_track_ids(mix))
        .await
        .map_err(|e| failure(ERROR, e))?;
    Ok(Json(WithTracks { mix, tracks }).into_response())
}

/// A mix with its track rows, serialised straight (not through `Value`, which would sort the rows' keys out of
/// upstream's model order).
#[derive(Serialize)]
pub(crate) struct WithTracks<'a> {
    #[serde(flatten)]
    pub mix: &'a serde_json::Map<String, Value>,
    pub tracks: Vec<MixTrack>,
}

/// The name a save gives the playlist: upstream's `customName || mix.name` (a falsy name is the mix's), `None` for
/// a name Prisma would refuse (not a string).
fn playlist_name(body: &Value, mix_name: &str) -> Option<String> {
    match body.get("name") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Some(mix_name.to_string()),
        Some(Value::String(s)) if s.is_empty() => Some(mix_name.to_string()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => Some(mix_name.to_string()),
        Some(_) => None,
    }
}

async fn save(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    const ERROR: &str = "Failed to save mix as playlist";
    let body: Value = if body.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::Invalid(e.to_string()))?
    };
    let mixes = mixes_for(&state, user.profile, requested_zone(&params))
        .await
        .map_err(|e| failure(ERROR, e))?;
    let mix = find_mix(&mixes, &id).ok_or(ApiError::NotFound(MIX_NOT_FOUND))?;
    let mix_name = mix.get("name").and_then(Value::as_str).unwrap_or_default();
    let name = playlist_name(&body, mix_name).ok_or(ApiError::Failed(ERROR))?;
    match save_mix_playlist(&state.pool, user.profile, &id, &name, &mix_track_ids(mix))
        .await
        .map_err(|e| failure(ERROR, e))?
    {
        SaveMix::Saved {
            id,
            name,
            track_count,
        } => {
            announce(&state, user.profile, id, false);
            Ok(Json(
                json!({ "id": id, "name": name, "trackCount": track_count }),
            ))
        }
        SaveMix::Exists { id, name } => Err(ApiError::ConflictWith {
            message: "Mix already saved as playlist".into(),
            extra: json!({ "playlistId": id, "name": name }),
        }),
        SaveMix::NothingLeft => Err(ApiError::Unprocessable(
            "None of this mix's songs are in the library any more",
        )),
    }
}

/// Upstream `loadTracksInMixOrder`, as reply rows.
pub(crate) async fn mix_tracks(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<Vec<MixTrack>> {
    Ok(track_rows(pool, ids)
        .await?
        .into_iter()
        .map(mix_track)
        .collect())
}

/// kima's `track_stages.status` as upstream's `analysisStatus`/`vibeAnalysisStatus` (D-264).
fn stage_status(status: &str) -> String {
    match status {
        "working" => "processing",
        "done" => "completed",
        other => other,
    }
    .to_string()
}

/// One reply row (Appendix D of the M5b plan, D-264).
fn mix_track(r: TrackRow) -> MixTrack {
    let analysis_running = r.analysis_status.as_deref() == Some("working");
    let analysis_done = r.analysis_status.as_deref() == Some("done");
    let vibe_running = r.vibe_status.as_deref() == Some("working");
    let cover = r.album_has_image.then(|| r.album_id.to_string());
    let user_cover = cover
        .clone()
        .filter(|_| r.album_image_source.as_deref() == Some("user"));
    let album_overrides = r.album_display_title.is_some()
        || r.album_display_year.is_some()
        || r.album_user_genres.is_some()
        || user_cover.is_some();
    MixTrack {
        id: r.id.to_string(),
        album_id: r.album_id.to_string(),
        title: r.title,
        track_no: r.track_no,
        duration: r.duration_secs,
        mime: None,
        file_path: r.path,
        file_modified: iso(r.mtime),
        file_size: r.size,
        bpm: r.tempo_bpm,
        // Owner Q7 = A: kima's 0-1 confidence back on Essentia's 0-5.32 scale.
        bpm_confidence: r.tempo_confidence.map(|c| c * 5.32),
        beats_count: None,
        key: r
            .key_tonic
            .and_then(|k| ESSENTIA_KEYS.get(usize::try_from(k).ok()?))
            .map(|k| (*k).to_string()),
        key_scale: r.key_mode,
        key_strength: r.key_strength,
        energy: None,
        loudness: r.integrated_lufs,
        dynamic_range: r.loudness_range_lu,
        danceability: r.musicnn_danceability,
        valence: None,
        arousal: None,
        deam_valence: None,
        deam_arousal: None,
        clap_valence_raw: r.valence_raw,
        clap_arousal_raw: r.arousal_raw,
        instrumentalness: r.instrumentalness,
        acousticness: r.acousticness,
        speechiness: None,
        danceability_ml: r.musicnn_danceability,
        mood_tags: r.mood_tags,
        essentia_genres: None,
        analysis_status: r.analysis_status.as_deref().map(stage_status),
        analysis_version: None,
        analysis_mode: None,
        analyzed_at: r.analysis_finished_at.filter(|_| analysis_done).map(iso),
        analysis_error: r.analysis_error,
        analysis_retry_count: r.analysis_attempts.map(i32::from),
        updated_at: iso(r.updated_at),
        has_user_overrides: r.display_title.is_some(),
        display_title: r.display_title,
        display_track_no: None,
        analysis_started_at: r.analysis_started_at.filter(|_| analysis_running).map(iso),
        vibe_analysis_status: r.vibe_status.as_deref().map(stage_status),
        vibe_analysis_started_at: r.vibe_started_at.filter(|_| vibe_running).map(iso),
        vibe_analysis_error: r.vibe_error,
        vibe_analysis_retry_count: r.vibe_attempts.map(i32::from),
        vibe_analysis_status_updated_at: r.vibe_updated_at.map(iso),
        isrc: r.isrc,
        isrc_source: r.isrc_source,
        acoustid_fingerprint: None,
        recording_mbid: r.recording_mbid.map(|m| m.to_string()),
        corrupt: false,
        missing_since: r.missing_since.map(iso),
        scan_status: None,
        scan_error: None,
        scan_started_at: None,
        disc_number: Some(r.disc),
        disc_subtitle: None,
        album: MixAlbum {
            id: r.album_id.to_string(),
            rg_mbid: r.album_rg_mbid.map(|m| m.to_string()),
            rg_mbid_source: r.album_rg_mbid_source,
            artist_id: r.album_artist_id.to_string(),
            title: r.album_title,
            identity_key: None,
            sort_name: r.album_sort_name,
            year: r.album_year,
            cover_url: cover,
            primary_type: None,
            label: r.album_label,
            genres: None,
            last_synced: None,
            location: "LIBRARY".into(),
            display_title: r.album_display_title,
            display_year: r.album_display_year,
            has_user_overrides: album_overrides,
            user_cover_url: user_cover,
            user_genres: r.album_user_genres,
            original_year: None,
            artist: MixArtist {
                id: r.album_artist_id.to_string(),
                name: r.artist_name,
                mbid: r.artist_mbid.map(|m| m.to_string()),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_falsy_name_is_the_mix_name_a_non_string_is_refused() {
        for body in [
            json!({}),
            json!({ "name": null }),
            json!({ "name": "" }),
            json!({ "name": false }),
            json!({ "name": 0 }),
        ] {
            assert_eq!(
                playlist_name(&body, "Mix").as_deref(),
                Some("Mix"),
                "{body}"
            );
        }
        assert_eq!(
            playlist_name(&json!({ "name": "Mine" }), "Mix").as_deref(),
            Some("Mine")
        );
        for body in [
            json!({ "name": 3 }),
            json!({ "name": true }),
            json!({ "name": [] }),
        ] {
            assert_eq!(playlist_name(&body, "Mix"), None, "{body}");
        }
    }

    #[test]
    fn stage_status_maps_kima_values() {
        assert_eq!(stage_status("pending"), "pending");
        assert_eq!(stage_status("working"), "processing");
        assert_eq!(stage_status("done"), "completed");
        assert_eq!(stage_status("failed"), "failed");
    }
}
