//! Seeding helpers shared by the vibe database tests. Real Postgres only; the database is never mocked.
#![allow(dead_code)]

use kima_db::PgPool;
use kima_db::vibe::MATCHING_SOUND;
use uuid::Uuid;

pub struct Seeded {
    pub artist: Uuid,
    pub album: Uuid,
    pub track: Uuid,
}

pub async fn artist(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, $1::text)")
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
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $1::text)",
    )
    .bind(id)
    .bind(artist)
    .execute(pool)
    .await
    .unwrap();
    id
}

pub async fn track_in(pool: &PgPool, album: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
         VALUES ($1, $2, 'a', 't', 'flac', $1::text, 1, now(), '\\x00')",
    )
    .bind(id)
    .bind(album)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// An artist with one album holding one track.
pub async fn track(pool: &PgPool) -> Seeded {
    let artist = artist(pool, "a").await;
    let album = album(pool, artist).await;
    let track = track_in(pool, album).await;
    Seeded {
        artist,
        album,
        track,
    }
}

/// A unit vector along axis `i` (a distinct, valid CLAP-shaped vector).
pub fn axis(i: usize) -> Vec<f32> {
    let mut v = vec![0.0_f32; 512];
    v[i % 512] = 1.0;
    v
}

pub fn identity() -> (&'static str, Vec<u8>, &'static str) {
    (
        MATCHING_SOUND.model_id,
        MATCHING_SOUND.model_sha256.to_vec(),
        MATCHING_SOUND.pipeline_version,
    )
}

/// Stores `vector` for `track` under the gate's own model identity.
pub async fn embed(pool: &PgPool, track: Uuid, vector: &[f32]) {
    let (model, sha, pipeline) = identity();
    embed_as(pool, track, model, &sha, pipeline, vector)
        .await
        .unwrap();
}

pub async fn embed_as(
    pool: &PgPool,
    track: Uuid,
    model: &str,
    sha: &[u8],
    pipeline: &str,
    vector: &[f32],
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding)
         VALUES ($1, 0, $2, $3, $4, $5::real[]::vector)
         ON CONFLICT (track_id) DO UPDATE SET model_id=EXCLUDED.model_id, model_sha256=EXCLUDED.model_sha256,
             pipeline_version=EXCLUDED.pipeline_version, embedding=EXCLUDED.embedding, created_at=clock_timestamp()",
    )
    .bind(track)
    .bind(model)
    .bind(sha)
    .bind(pipeline)
    .bind(vector)
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn current_database(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .unwrap()
}

pub async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}
