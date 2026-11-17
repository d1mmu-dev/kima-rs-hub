//! M5 Task 18 Step 2: real-model vibe end-to-end through the real router.
//!
//! 30 generated WAVs (tones and noise, so no audio rights question) -> real native music CLAP
//! audio embedding -> the coordinator's embedding, mood and similarity phases -> `similar`,
//! `alchemy`, `path`, `search`, `map`, `mood-summary` and `mood` answer through `kima_api::router`.
//!
//! IMPLEMENTED, NOT RUN. It needs the M4 music-checkpoint swap (the native encoder must be
//! `MATCHING_SOUND`, not today's `hf_audio`), private SHA-256-pinned vibe assets and an
//! explicitly authorized isolated database. It never skips: every missing prerequisite panics
//! before any database connection. No downloads, no owner library. See
//! `docs/testing/vibe-e2e.md` for the exact command.
//!
//! Required environment:
//! - `KIMA_CLAP_MODEL_DIR`: absolute directory with the verified native music audio/text export.
//! - `KIMA_VIBE_ASSET_DIR`: absolute directory with the pinned `feel_probe.json`/`mood_heads.json`.
//! - `ORT_DYLIB_PATH`: trusted ONNX Runtime library.
//! - `DATABASE_URL` (127.0.0.1, not the development port) and `PGDATABASE` naming the same
//!   empty database, plus `KIMA_VIBE_E2E_AUTHORIZED_DATABASE` equal to it (explicit consent).
//!
//! No `#[sqlx::test]`: that harness would create databases before any guard could run.

mod common;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use axum_test::TestServer;
use kima_db::PgPool;
use kima_ident::coordinator;
use kima_ident::stages::{embedding, vibe_assets};
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DEV_PORT: u16 = 5456;
const TRACKS: usize = 30;
const RATE: u32 = 48_000;
const SECONDS: u32 = 12;

#[derive(Debug)]
struct Prerequisites {
    model_dir: PathBuf,
    asset_dir: PathBuf,
    url: String,
    database: String,
}

fn absolute_dir(lookup: &dyn Fn(&str) -> Option<String>, key: &str) -> PathBuf {
    let dir = PathBuf::from(lookup(key).unwrap_or_else(|| panic!("{key} must be set")));
    assert!(dir.is_absolute(), "{key} must be absolute");
    assert!(dir.is_dir(), "{key} must name an existing directory");
    dir
}

/// Every prerequisite is checked before any database connection; any gap panics.
fn prerequisites(lookup: &dyn Fn(&str) -> Option<String>) -> Prerequisites {
    let model_dir = absolute_dir(lookup, "KIMA_CLAP_MODEL_DIR");
    let asset_dir = absolute_dir(lookup, "KIMA_VIBE_ASSET_DIR");
    let ort = lookup("ORT_DYLIB_PATH").expect("ORT_DYLIB_PATH must be set");
    assert!(Path::new(&ort).is_file(), "ORT_DYLIB_PATH must name a file");
    let url = lookup("DATABASE_URL").expect("DATABASE_URL must be set");
    let database = lookup("PGDATABASE").expect("PGDATABASE must be set");
    assert_eq!(
        lookup("KIMA_VIBE_E2E_AUTHORIZED_DATABASE").as_deref(),
        Some(database.as_str()),
        "SAFETY REFUSAL: KIMA_VIBE_E2E_AUTHORIZED_DATABASE must name PGDATABASE"
    );
    assert!(
        vibe_assets::native_audio_is_matching_sound(),
        "the native audio encoder is not the matching music checkpoint (hf_audio is forbidden)"
    );
    let assets = vibe_assets::load(Some(&asset_dir), &vibe_assets::VibePins::compiled());
    assert!(
        assets.probe.is_some() && assets.heads.is_some(),
        "vibe assets must verify against the compiled SHA-256 pins: {:?}",
        assets.report
    );
    Prerequisites {
        model_dir,
        asset_dir,
        url,
        database,
    }
}

/// Verifies the server's own identity, then creates and migrates a throwaway database.
async fn isolated_db(pre: &Prerequisites) -> (PgPool, PgPool, String) {
    let opts = PgConnectOptions::from_str(&pre.url).expect("DATABASE_URL must parse");
    assert_eq!(opts.get_host(), "127.0.0.1", "SAFETY REFUSAL: host");
    assert_ne!(
        opts.get_port(),
        DEV_PORT,
        "SAFETY REFUSAL: development port"
    );
    assert_eq!(
        opts.get_database(),
        Some(pre.database.as_str()),
        "SAFETY REFUSAL: DATABASE_URL and PGDATABASE disagree"
    );
    assert_ne!(pre.database, "kima", "SAFETY REFUSAL: development name");
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts.clone())
        .await
        .expect("connect to the authorized cluster");
    let current: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(current, pre.database, "SAFETY REFUSAL: connected database");
    let has_schema: bool = sqlx::query_scalar(
        "SELECT to_regclass('public.tracks') IS NOT NULL OR to_regclass('public._sqlx_migrations') IS NOT NULL",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert!(!has_schema, "SAFETY REFUSAL: target already holds a schema");
    let name = format!("kima_vibe_e2e_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE \"{name}\""))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(opts.database(&name))
        .await
        .unwrap();
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    (pool, admin, name)
}

/// Deterministic xorshift for the noise tracks.
struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// Track `n`: 20 harmonic tones over three octaves, then 10 white/low-passed noises.
fn sample(n: usize, i: u32, noise: &mut Noise, low: &mut f64) -> f64 {
    let t = f64::from(i) / f64::from(RATE);
    if n < 20 {
        let f = 110.0 * 2f64.powf(n as f64 / 7.0);
        let tremolo = 1.0 - 0.3 * (t * (1.0 + n as f64 / 4.0) * std::f64::consts::TAU).sin();
        (1..=3)
            .map(|h| (t * f * f64::from(h) * std::f64::consts::TAU).sin() / f64::from(h))
            .sum::<f64>()
            * 0.3
            * tremolo
    } else {
        let alpha = 0.02 + (n - 20) as f64 * 0.1;
        *low += alpha * (noise.next() - *low);
        *low * 0.6
    }
}

fn write_wav(path: &Path, n: usize) {
    let frames = RATE * SECONDS;
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15 ^ n as u64);
    let mut low = 0.0;
    let mut bytes = Vec::with_capacity(44 + frames as usize * 2);
    let data_len = frames * 2;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let s = sample(n, i, &mut noise, &mut low).clamp(-1.0, 1.0);
        bytes.extend_from_slice(&((s * f64::from(i16::MAX)) as i16).to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

async fn insert_track(pool: &PgPool, n: usize) -> Uuid {
    let artist = Uuid::new_v4();
    let name = format!("Synthetic artist {}", n % 6);
    sqlx::query("INSERT INTO artists (id, name, name_key, enrichment_status) VALUES ($1, $2, $3, 'completed')")
        .bind(artist)
        .bind(&name)
        .bind(artist.to_string())
        .execute(pool)
        .await
        .unwrap();
    let album = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, $3, $4)")
        .bind(album)
        .bind(artist)
        .bind(format!("Synthetic album {n}"))
        .bind(album.to_string())
        .execute(pool)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint, duration_secs)
         VALUES ($1, $2, $3, $4, 'wav', $5, 1, now(), '\\x00', $6)",
    )
    .bind(id)
    .bind(album)
    .bind(&name)
    .bind(format!("Synthetic {n:02}"))
    .bind(format!("synthetic-{n:02}.wav"))
    .bind(SECONDS as i32)
    .execute(pool)
    .await
    .unwrap();
    id
}

#[test]
fn prerequisites_fail_closed_when_environment_is_unset_or_checkpoint_is_wrong() {
    let empty = |_: &str| None;
    assert!(std::panic::catch_unwind(|| prerequisites(&empty)).is_err());
    // Everything else present: today's hf_audio encoder must still refuse to run.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ort.so");
    std::fs::write(&file, b"not a runtime").unwrap();
    let root = dir.path().to_str().unwrap().to_owned();
    let ort = file.to_str().unwrap().to_owned();
    let full = move |key: &str| match key {
        "KIMA_CLAP_MODEL_DIR" | "KIMA_VIBE_ASSET_DIR" => Some(root.clone()),
        "ORT_DYLIB_PATH" => Some(ort.clone()),
        "DATABASE_URL" => Some("postgres://u@127.0.0.1:1/isolated".into()),
        "PGDATABASE" | "KIMA_VIBE_E2E_AUTHORIZED_DATABASE" => Some("isolated".into()),
        _ => None,
    };
    let refusal = std::panic::catch_unwind(|| prerequisites(&full)).unwrap_err();
    let message = refusal
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| refusal.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap();
    assert!(message.contains("hf_audio is forbidden"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the M4 native music checkpoint, pinned private vibe assets and an authorized isolated database; NOT RUN"]
async fn real_music_clap_vibe_end_to_end_on_thirty_generated_tracks() {
    let pre = prerequisites(&|key| std::env::var(key).ok());
    let (pool, admin, name) = isolated_db(&pre).await;
    let library = tempfile::tempdir().unwrap();
    let root = library.path().canonicalize().unwrap();
    let mut ids = Vec::new();
    for n in 0..TRACKS {
        write_wav(&root.join(format!("synthetic-{n:02}.wav")), n);
        ids.push(insert_track(&pool, n).await);
    }
    let snapshots = root.join(".kima-snapshots");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&snapshots).unwrap();
        std::fs::set_permissions(&snapshots, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let state = common::state(pool.clone()).with_library_root(root.clone());
    let mut ctx = state.ident_ctx();
    ctx.config.fingerprinting = false;
    ctx.config.vibe.asset_dir = Some(pre.asset_dir.clone());
    ctx.config.embedding = kima_ident::EmbeddingConfig {
        model_dir: Some(pre.model_dir.clone()),
        snapshot_dir: Some(snapshots),
        ..Default::default()
    };
    embedding::check_snapshot_setup(&mut ctx.config.embedding, &root).unwrap();
    embedding::initialize(&ctx).await.unwrap();
    let assets = vibe_assets::initialize(&ctx).await.unwrap();
    assert!(assets.probe.is_some() && assets.heads.is_some());
    assert!(vibe_assets::matching_audio_available(&ctx));

    let mut embedded = 0;
    for _ in 0..20 {
        *ctx.last_cycle.lock().unwrap() = None;
        let report = coordinator::run_cycle(&ctx, &CancellationToken::new())
            .await
            .expect("coordinator cycle");
        assert_eq!(report.embedding.failed, 0, "{report:?}");
        assert_eq!(report.errors, 0, "{report:?}");
        embedded += report.embedding.done;
        if !report.any_work() && embedded == TRACKS {
            break;
        }
    }
    assert_eq!(
        embedded, TRACKS,
        "every generated track gets a real music vector"
    );
    let mapped: i64 = sqlx::query_scalar("SELECT count(*) FROM matching_map")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        mapped, TRACKS as i64,
        "the similarity phase laid out every track"
    );

    let state = state.with_ident(ctx);
    let (_, token) = common::user_with_token(&state, "vibe-e2e").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let get = |path: String| {
        let srv = &srv;
        let token = &token;
        async move {
            let res = srv.get(&path).authorization_bearer(token).await;
            (res.status_code().as_u16(), res.json::<Value>())
        }
    };
    let (status, features) = get("/api/system/features".into()).await;
    assert_eq!(
        (status, features["vibeEmbeddings"].clone()),
        (200, json!(true))
    );
    let (status, similar) = get(format!("/api/vibe/similar/{}", ids[0])).await;
    assert_eq!(status, 200, "{similar}");
    let tracks = similar["tracks"].as_array().unwrap();
    assert!(!tracks.is_empty() && tracks.len() <= 20);
    assert!(tracks.iter().all(|t| t["id"] != ids[0].to_string()));
    assert!(tracks.iter().all(|t| {
        t["similarity"]
            .as_f64()
            .is_some_and(|s| (0.0..=1.0).contains(&s))
    }));
    let (status, map) = get("/api/vibe/map".into()).await;
    assert_eq!((status, map["trackCount"].clone()), (200, json!(TRACKS)));
    let (status, summary) = get("/api/vibe/mood-summary".into()).await;
    assert_eq!(status, 200, "{summary}");
    let (status, mood) = get(format!("/api/vibe/mood/{}", ids[25])).await;
    assert_eq!(status, 200);
    assert!(mood["dominantMood"].is_string());
    for (path, body) in [
        ("/api/vibe/search", json!({"query":"a bright sine tone"})),
        ("/api/vibe/alchemy", json!({"add":[ids[0], ids[1]]})),
        (
            "/api/vibe/path",
            json!({"startTrackId":ids[0],"endTrackId":ids[TRACKS - 1]}),
        ),
    ] {
        let res = srv
            .post(path)
            .authorization_bearer(&token)
            .json(&body)
            .await;
        let data: Value = res.json();
        assert_eq!(res.status_code().as_u16(), 200, "{path}: {data}");
    }
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE \"{name}\" WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
}
