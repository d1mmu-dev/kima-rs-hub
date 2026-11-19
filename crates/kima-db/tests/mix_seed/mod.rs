//! Seeding for the M5b mix tests from Task 8 on: artists, albums, tracks with their own titles, listeners and plays at
//! chosen times, analysis rows and sound vectors of the engine's identity. Real Postgres only.
#![allow(dead_code)]

use kima_core::profile::ProfileId;
use kima_db::PgPool;
use kima_db::vibe::MATCHING_SOUND;
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use uuid::Uuid;

pub async fn artist(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, lower($2))")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

pub async fn album(pool: &PgPool, artist: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, image_path) VALUES ($1, $2, $1::text, $1::text, 'c.jpg')",
    )
    .bind(id)
    .bind(artist)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// A present track titled `title` (its id when `None`).
pub async fn track_titled(pool: &PgPool, album: Uuid, title: Option<&str>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint, duration_secs)
         VALUES ($1, $2, 'a', COALESCE($3, $1::text), 'flac', $1::text, 1, now(), '\\x00', 200)",
    )
    .bind(id)
    .bind(album)
    .bind(title)
    .execute(pool)
    .await
    .unwrap();
    id
}

pub async fn track(pool: &PgPool, album: Uuid) -> Uuid {
    track_titled(pool, album, None).await
}

/// `artists` artists of `per_artist` songs each; the songs by artist.
pub async fn library(pool: &PgPool, artists: usize, per_artist: usize) -> Vec<Vec<Uuid>> {
    let mut out = Vec::new();
    for a in 0..artists {
        let ar = artist(pool, &format!("artist {a} {}", Uuid::now_v7())).await;
        let al = album(pool, ar).await;
        let mut songs = Vec::new();
        for _ in 0..per_artist {
            songs.push(track(pool, al).await);
        }
        out.push(songs);
    }
    out
}

pub fn flat(v: &[Vec<Uuid>]) -> Vec<Uuid> {
    v.iter().flatten().copied().collect()
}

pub async fn listener(pool: &PgPool, name: &str) -> ProfileId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, $2, 'x')")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    ProfileId::new(id)
}

pub async fn sibling(pool: &PgPool, of: ProfileId) -> ProfileId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO profiles (id, user_id, name, kind) VALUES ($1, $2, 'kid', 'kids')")
        .bind(id)
        .bind(of.into_uuid())
        .execute(pool)
        .await
        .unwrap();
    ProfileId::new(id)
}

/// A play at `at` (an SQL timestamptz expression), heard or not.
pub async fn play_at(pool: &PgPool, who: ProfileId, track: Uuid, at: &str, heard: bool) {
    sqlx::query(&format!(
        "INSERT INTO plays (id, profile_id, track_id, played_at, heard) VALUES ($1, $2, $3, {at}, $4)"
    ))
    .bind(Uuid::now_v7())
    .bind(who.into_uuid())
    .bind(track)
    .bind(heard)
    .execute(pool)
    .await
    .unwrap();
}

/// `times` heard plays at 23:00 UTC on recent days (one per day back from yesterday), night in UTC.
pub async fn night_plays(pool: &PgPool, who: ProfileId, track: Uuid, times: usize) {
    for d in 0..times {
        play_at(
            pool,
            who,
            track,
            &format!("date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' - interval '{} days' + interval '23 hours'", d + 1),
            true,
        )
        .await;
    }
}

/// A finished analysis with a tempo on kima's 0-1 confidence.
pub async fn tempo(pool: &PgPool, track: Uuid, bpm: f64, confidence: f64) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, finished_at) VALUES ($1, 'analysis', 'done', now())
         ON CONFLICT (track_id, stage) DO UPDATE SET status = 'done'",
    )
    .bind(track)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_analysis (track_id, file_generation, dsp_tempo_pipeline_version, tempo_bpm, tempo_confidence, tempo_computed_at)
         VALUES ($1, 0, 'test', $2, $3, now())",
    )
    .bind(track)
    .bind(bpm)
    .bind(confidence)
    .execute(pool)
    .await
    .unwrap();
}

/// A unit vector: mostly along axis `cluster`, a little along `512 - 1 - i % 64` so songs of a cluster differ.
pub fn clustered(cluster: usize, i: usize) -> Vec<f32> {
    let mut v = vec![0.0_f32; 512];
    v[cluster] = 1.0;
    v[511 - (i % 64)] += 0.2 + (i % 7) as f32 * 0.05;
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
    v.iter().map(|a| a / n).collect()
}

/// A sound vector of the engine's identity.
pub async fn embed(pool: &PgPool, track: Uuid, vector: &[f32]) {
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding)
         VALUES ($1, 0, $2, $3, $4, $5::real[]::vector)",
    )
    .bind(track)
    .bind(MATCHING_SOUND.model_id)
    .bind(MATCHING_SOUND.model_sha256.to_vec())
    .bind(MATCHING_SOUND.pipeline_version)
    .bind(vector)
    .execute(pool)
    .await
    .unwrap();
}

fn axis(scale: f64, bias: f64) -> ProbeAxis {
    ProbeAxis {
        alpha: 1.0,
        bias,
        weights: (0..512)
            .map(|i| (((i * 7) % 13) as f64 / 13.0 - 0.5) * scale)
            .collect(),
    }
}

/// A synthetic feel probe (as the vibe ranking tests use).
pub fn probe() -> FeelProbe {
    FeelProbe {
        model: "synthetic".into(),
        valence: axis(0.02, 0.5),
        arousal: axis(-0.03, 0.4),
        probe_id: "0123456789abcdef".into(),
    }
}

/// Builds the matching state over every embedded track.
pub async fn build_matching(pool: &PgPool) -> FeelProbe {
    let p = probe();
    kima_db::vibe::state::build_matching(pool, &p)
        .await
        .unwrap();
    p
}
