//! Seeding for the M5b mix tests: a whole small library written in one go (artists, albums, tracks, vectors of the
//! engine's identity, mood scores, CLAP readings, DSP analysis) and the stored mood statistics. Real Postgres only.
#![allow(dead_code)]

use kima_db::PgPool;
use kima_db::vibe::MATCHING_SOUND;
use kima_db::vibe::moods::{READINGS, mood_state_version};
use kima_vibe::mood_heads::{MOODS, MoodHead, MoodHeads};
use std::collections::HashMap;
use uuid::Uuid;

/// When every test vector was embedded.
pub const ANALYZED_AT: &str = "2026-10-23T00:00:00Z";
pub const HEADS_VERSION: &str = "test-heads";

/// Heads that only carry the version the stored scores name (no test scores vectors).
pub fn heads() -> MoodHeads {
    MoodHeads {
        version: HEADS_VERSION.into(),
        model: MATCHING_SOUND.model_id.into(),
        dims: 512,
        heads: MOODS
            .iter()
            .map(|_| MoodHead {
                weights: vec![0.0; 512],
                bias: 0.0,
            })
            .collect(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scores {
    Current,
    OlderVector,
    OtherWeights,
    None,
}

/// A song of a test library. Defaults: its own artist, title = name, every mood 0, readings 0.5 (logit 0), 120 BPM
/// on a steady beat (upstream confidence 3 of 5.32), major, present, current scores.
#[derive(Debug, Clone)]
pub struct Song {
    pub name: String,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub moods: Vec<(&'static str, f64)>,
    /// (arousal, instrumental, acoustic); `None` = no reading row (kima stores the four readings together).
    pub readings: Option<(f64, f64, f64)>,
    /// (bpm, confidence on upstream's 0-5.32 scale); `None` = no tempo group.
    pub tempo: Option<(f64, f64)>,
    /// `Some("major")`, `Some("minor")` or `None` (no key group).
    pub key: Option<&'static str>,
    pub missing: bool,
    pub scores: Scores,
}

pub fn song(name: &str) -> Song {
    Song {
        name: name.into(),
        artist: None,
        title: None,
        moods: Vec::new(),
        readings: Some((0.5, 0.5, 0.5)),
        tempo: Some((120.0, 3.0)),
        key: Some("major"),
        missing: false,
        scores: Scores::Current,
    }
}

impl Song {
    pub fn artist(mut self, a: &str) -> Self {
        self.artist = Some(a.into());
        self
    }
    pub fn title(mut self, t: &str) -> Self {
        self.title = Some(t.into());
        self
    }
    pub fn mood(mut self, m: &'static str, v: f64) -> Self {
        self.moods.retain(|(n, _)| *n != m);
        self.moods.push((m, v));
        self
    }
    pub fn arousal(mut self, a: f64) -> Self {
        let r = self.readings.unwrap_or((0.5, 0.5, 0.5));
        self.readings = Some((a, r.1, r.2));
        self
    }
    pub fn instrumental(mut self, i: f64) -> Self {
        let r = self.readings.unwrap_or((0.5, 0.5, 0.5));
        self.readings = Some((r.0, i, r.2));
        self
    }
    pub fn no_readings(mut self) -> Self {
        self.readings = None;
        self
    }
    pub fn bpm(mut self, bpm: f64, confidence: f64) -> Self {
        self.tempo = Some((bpm, confidence));
        self
    }
    pub fn no_tempo(mut self) -> Self {
        self.tempo = None;
        self
    }
    pub fn key(mut self, k: Option<&'static str>) -> Self {
        self.key = k;
        self
    }
    pub fn missing(mut self) -> Self {
        self.missing = true;
        self
    }
    pub fn scores(mut self, s: Scores) -> Self {
        self.scores = s;
        self
    }
}

/// Songs scoring 0 on every mood, all by artist `bg`.
pub fn background(n: usize) -> Vec<Song> {
    (0..n)
        .map(|i| song(&format!("bg{i}")).artist("bg"))
        .collect()
}

/// A song far above the line of a calm recipe in a library of mostly background songs.
pub fn member(name: &str) -> Song {
    song(name).mood("calm", 10.0)
}

/// The ids of a written library: song name -> track id, artist name -> artist id.
pub struct Library {
    pub tracks: HashMap<String, Uuid>,
    pub artists: HashMap<String, Uuid>,
}

impl Library {
    pub fn names(&self, ids: impl IntoIterator<Item = Uuid>) -> std::collections::BTreeSet<String> {
        let by_id: HashMap<Uuid, &String> = self.tracks.iter().map(|(n, id)| (*id, n)).collect();
        ids.into_iter().map(|id| by_id[&id].clone()).collect()
    }
}

/// Ids in list order (so "the lower id" is the earlier song): `<kind><index>` in the low bits of a v7-shaped UUID.
fn id(kind: u8, index: usize) -> Uuid {
    Uuid::from_u128(
        0x0190_0000_0000_7000_8000_0000_0000_0000 | (u128::from(kind) << 40) | index as u128,
    )
}

const NO_ONE: [(&str, &str); 2] = [
    ("Various Artists", "various artists"),
    ("Unknown Artist", "unknown artist"),
];

/// Writes `songs` as the whole library (after clearing any earlier one), with stored statistics of mean 0 and SD 1
/// for every mood and reading unless `stats` (`"<name>_mean"`/`"<name>_sd"`) says otherwise.
pub async fn library(pool: &PgPool, songs: &[Song], stats: &[(&str, f64)]) -> Library {
    sqlx::raw_sql(
        "DELETE FROM mood_state; DELETE FROM track_analysis; DELETE FROM track_clap_readings; \
         DELETE FROM track_moods; DELETE FROM track_embeddings; DELETE FROM tracks; DELETE FROM albums; \
         DELETE FROM artists;",
    )
    .execute(pool)
    .await
    .unwrap();
    let artist_of = |s: &Song| s.artist.clone().unwrap_or_else(|| s.name.clone());
    let mut artist_names: Vec<String> = Vec::new();
    for s in songs {
        let a = artist_of(s);
        if !artist_names.contains(&a) {
            artist_names.push(a);
        }
    }
    let artists: HashMap<String, Uuid> = artist_names
        .iter()
        .enumerate()
        .map(|(i, a)| (a.clone(), id(1, i)))
        .collect();
    let keys: Vec<String> = artist_names
        .iter()
        .map(|a| {
            NO_ONE
                .iter()
                .find(|(n, _)| n == a)
                .map_or_else(|| artists[a].to_string(), |(_, k)| k.to_string())
        })
        .collect();
    let artist_ids: Vec<Uuid> = artist_names.iter().map(|a| artists[a]).collect();
    let album_ids: Vec<Uuid> = (0..artist_names.len()).map(|i| id(2, i)).collect();
    sqlx::query("INSERT INTO artists (id, name, name_key) SELECT * FROM unnest($1::uuid[], $2::text[], $3::text[])")
        .bind(&artist_ids)
        .bind(&artist_names)
        .bind(&keys)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) \
         SELECT al, ar, n, al::text FROM unnest($1::uuid[], $2::uuid[], $3::text[]) AS u(al, ar, n)",
    )
    .bind(&album_ids)
    .bind(&artist_ids)
    .bind(&artist_names)
    .execute(pool)
    .await
    .unwrap();

    let tracks: HashMap<String, Uuid> = songs
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.clone(), id(3, i)))
        .collect();
    let track_ids: Vec<Uuid> = songs.iter().map(|s| tracks[&s.name]).collect();
    let album_of: Vec<Uuid> = songs
        .iter()
        .map(|s| {
            album_ids[artist_names
                .iter()
                .position(|a| *a == artist_of(s))
                .unwrap()]
        })
        .collect();
    let titles: Vec<String> = songs
        .iter()
        .map(|s| s.title.clone().unwrap_or_else(|| s.name.clone()))
        .collect();
    let missing: Vec<bool> = songs.iter().map(|s| s.missing).collect();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint, missing_since) \
         SELECT t, al, 'a', ti, 'flac', t::text, 1, now(), '\\x00', CASE WHEN m THEN now() END \
         FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::bool[]) AS u(t, al, ti, m)",
    )
    .bind(&track_ids)
    .bind(&album_of)
    .bind(&titles)
    .bind(&missing)
    .execute(pool)
    .await
    .unwrap();

    let scored: Vec<&Song> = songs.iter().filter(|s| s.scores != Scores::None).collect();
    let scored_ids: Vec<Uuid> = scored.iter().map(|s| tracks[&s.name]).collect();
    let mut unit = vec![0.0_f32; 512];
    unit[0] = 1.0;
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding, created_at) \
         SELECT t, 0, $2, $3, $4, $5::real[]::vector, $6::timestamptz FROM unnest($1::uuid[]) AS u(t)",
    )
    .bind(&scored_ids)
    .bind(MATCHING_SOUND.model_id)
    .bind(MATCHING_SOUND.model_sha256.to_vec())
    .bind(MATCHING_SOUND.pipeline_version)
    .bind(&unit)
    .bind(ANALYZED_AT)
    .execute(pool)
    .await
    .unwrap();

    // Moods: one row per scored song, 22 columns from a flat array.
    let mood_cols = MOODS
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mood_vals = (0..MOODS.len())
        .map(|k| format!("v[{}]::real", k + 1))
        .collect::<Vec<_>>()
        .join(", ");
    for s in &scored {
        let values: Vec<f64> = MOODS
            .iter()
            .map(|m| {
                s.moods
                    .iter()
                    .find(|(n, _)| n == m)
                    .map_or(0.0, |(_, v)| *v)
            })
            .collect();
        let older = s.scores == Scores::OlderVector;
        let weights = if s.scores == Scores::OtherWeights {
            "older-weights"
        } else {
            HEADS_VERSION
        };
        sqlx::query(&format!(
            "INSERT INTO track_moods (track_id, embedded_at, weights, {mood_cols}) \
             SELECT $1, $2::timestamptz - CASE WHEN $3 THEN interval '1 minute' ELSE interval '0' END, $4, {mood_vals} \
             FROM (SELECT $5::float8[] AS v) x"
        ))
        .bind(tracks[&s.name])
        .bind(ANALYZED_AT)
        .bind(older)
        .bind(weights)
        .bind(&values)
        .execute(pool)
        .await
        .unwrap();
        if let Some((a, i, c)) = s.readings {
            sqlx::query(
                "INSERT INTO track_clap_readings (track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw) \
                 VALUES ($1, $2::timestamptz, $3, $4, 0.5, $5)",
            )
            .bind(tracks[&s.name])
            .bind(ANALYZED_AT)
            .bind(i as f32)
            .bind(c as f32)
            .bind(a as f32)
            .execute(pool)
            .await
            .unwrap();
        }
    }
    for s in songs {
        if s.tempo.is_none() && s.key.is_none() {
            continue;
        }
        let (bpm, conf) = s
            .tempo
            .map_or((None, None), |(b, c)| (Some(b), Some(c / 5.32)));
        sqlx::query(
            "INSERT INTO track_analysis (track_id, file_generation, dsp_tempo_pipeline_version, tempo_bpm, tempo_confidence, \
                 tempo_computed_at, dsp_key_pipeline_version, key_tonic, key_mode, key_strength, key_computed_at) \
             VALUES ($1, 0, CASE WHEN $2::float8 IS NOT NULL THEN 'test' END, $2, $3, CASE WHEN $2::float8 IS NOT NULL THEN now() END, \
                     CASE WHEN $4::text IS NOT NULL THEN 'test' END, CASE WHEN $4::text IS NOT NULL THEN 0::smallint END, $4, \
                     CASE WHEN $4::text IS NOT NULL THEN 0.5::float8 END, CASE WHEN $4::text IS NOT NULL THEN now() END)",
        )
        .bind(tracks[&s.name])
        .bind(bpm)
        .bind(conf)
        .bind(s.key)
        .execute(pool)
        .await
        .unwrap();
    }
    write_state(pool, songs.len() as i32, stats).await;
    Library { tracks, artists }
}

/// The stored statistics row of [`heads`]' version: mean 0 and SD 1 unless `stats` says otherwise.
pub async fn write_state(pool: &PgPool, track_count: i32, stats: &[(&str, f64)]) {
    let names: Vec<String> = MOODS
        .iter()
        .chain(READINGS.iter())
        .flat_map(|n| [format!("{n}_mean"), format!("{n}_sd")])
        .collect();
    let values: Vec<f64> = names
        .iter()
        .map(|c| {
            stats
                .iter()
                .find(|(n, _)| n == c)
                .map_or(if c.ends_with("_sd") { 1.0 } else { 0.0 }, |(_, v)| *v)
        })
        .collect();
    let cols = names
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let vals = (0..names.len())
        .map(|k| format!("v[{}]", k + 1))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(&format!(
        "INSERT INTO mood_state (id, version, fingerprint, track_count, {cols}) \
         SELECT 'default', $1, 'test', $2, {vals} FROM (SELECT $3::float8[] AS v) x"
    ))
    .bind(mood_state_version(&heads()))
    .bind(track_count)
    .bind(&values)
    .execute(pool)
    .await
    .unwrap();
}
