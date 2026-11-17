//! M5 Task 18 Step 2: real-model vibe end-to-end through the real router.
//!
//! 30 generated WAVs (tones and noise, so no audio rights question) -> real native music CLAP
//! audio embedding -> the coordinator's embedding, mood and similarity phases -> `similar`,
//! `alchemy`, `path`, `search`, `map`, `mood-summary` and `mood` answer through `kima_api::router`.
//!
//! IMPLEMENTED, NOT RUN. It needs the M4 music-checkpoint swap (the native encoder must be
//! `MATCHING_SOUND`, not today's `hf_audio`), private SHA-256-pinned vibe assets and an
//! explicitly authorized isolated database. It never skips: every missing prerequisite (including
//! the SHA-256 of every CLAP model file and the snapshot/reflink setup) panics before any database
//! connection. No downloads, no owner library. The throwaway database it creates is dropped on
//! success and on panic, and only that database. See `docs/testing/vibe-e2e.md` for the command.
//!
//! Required environment:
//! - `KIMA_CLAP_MODEL_DIR`: absolute directory with the verified native music audio/text export.
//! - `KIMA_VIBE_ASSET_DIR`: absolute directory with the pinned `feel_probe.json`/`mood_heads.json`.
//! - `ORT_DYLIB_PATH`: trusted ONNX Runtime library.
//! - `DATABASE_URL` (127.0.0.1, not the development port) and `PGDATABASE` naming the same
//!   empty database, plus `KIMA_VIBE_E2E_AUTHORIZED_DATABASE` equal to it (explicit consent).
//! - `KIMA_NET_DENY_REAL=1`: the harness refuses to run with the real network reachable.
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
    assert!(
        lookup("KIMA_NET_DENY_REAL").is_some_and(|v| v.trim() == "1"),
        "SAFETY REFUSAL: KIMA_NET_DENY_REAL=1 must be set"
    );
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
    verify_model_dir(&model_dir);
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

/// Every file the native audio and text capabilities load must verify against its compiled
/// SHA-256 pin, before any database exists.
fn verify_model_dir(model_dir: &Path) {
    use kima_ml::onnx::OnnxModel;
    for descriptor in [
        OnnxModel::ClapAudio.descriptor(),
        OnnxModel::ClapText.descriptor(),
        kima_ml::text_tokenizer::descriptor(),
    ] {
        if let Err(error) = kima_ml::verify_artifact(model_dir, &descriptor) {
            panic!(
                "KIMA_CLAP_MODEL_DIR: {} does not verify against its pin: {error:?}",
                descriptor.basename
            );
        }
    }
}

/// Counts every schema object a fresh database would not have: relations, sequences, views
/// and types in any non-system schema (including `public`), extra schemas, functions and
/// extensions other than the built-in `plpgsql`.
const FOREIGN_OBJECTS: &str = "
    WITH user_ns AS (
        SELECT oid, nspname FROM pg_namespace
        WHERE nspname NOT IN ('pg_catalog', 'information_schema')
          AND nspname NOT LIKE 'pg\\_toast%' AND nspname NOT LIKE 'pg\\_temp\\_%'
    )
    SELECT (SELECT count(*) FROM pg_class WHERE relnamespace IN (SELECT oid FROM user_ns))
         + (SELECT count(*) FROM pg_type WHERE typnamespace IN (SELECT oid FROM user_ns))
         + (SELECT count(*) FROM pg_proc WHERE pronamespace IN (SELECT oid FROM user_ns))
         + (SELECT count(*) FROM user_ns WHERE nspname <> 'public')
         + (SELECT count(*) FROM pg_extension WHERE extname <> 'plpgsql')";

const OWNED_PREFIX: &str = "kima_vibe_e2e_";

/// The throwaway database this run created, marked with its own run comment.
#[derive(Clone)]
struct OwnedDatabase {
    name: String,
    marker: String,
}

/// Verifies the server's own identity and that the authorized database is empty, then creates
/// a uniquely named throwaway database. Returns the admin pool and what this run owns.
async fn create_owned_db(pre: &Prerequisites) -> (PgPool, PgConnectOptions, OwnedDatabase) {
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
    let foreign: i64 = sqlx::query_scalar(FOREIGN_OBJECTS)
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(
        foreign, 0,
        "SAFETY REFUSAL: the authorized database is not empty"
    );
    let run = Uuid::new_v4().simple().to_string();
    let owned = OwnedDatabase {
        name: format!("{OWNED_PREFIX}{run}"),
        marker: format!("kima vibe_e2e run {run}"),
    };
    sqlx::query(&format!("CREATE DATABASE \"{}\"", owned.name))
        .execute(&admin)
        .await
        .expect("create the throwaway database");
    (admin, opts, owned)
}

/// Marks the throwaway database right after creation, so cleanup can prove it is ours.
async fn mark_owned(admin: &PgPool, owned: &OwnedDatabase) {
    sqlx::query(&format!(
        "COMMENT ON DATABASE \"{}\" IS '{}'",
        owned.name, owned.marker
    ))
    .execute(admin)
    .await
    .expect("mark the throwaway database");
}

/// Drops the throwaway database, and only it: the name must carry this harness's prefix and
/// the database's comment must still be this run's marker (an unmarked database is ours only
/// if marking itself failed, so it is dropped only when it has no comment at all).
async fn drop_owned(admin: &PgPool, owned: &OwnedDatabase) {
    assert!(owned.name.starts_with(OWNED_PREFIX));
    let comment: Option<Option<String>> = sqlx::query_scalar(
        "SELECT shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = $1",
    )
    .bind(&owned.name)
    .fetch_optional(admin)
    .await
    .expect("look up the throwaway database");
    match comment {
        None => {}
        Some(c) if c.is_none() || c.as_deref() == Some(owned.marker.as_str()) => {
            sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", owned.name))
                .execute(admin)
                .await
                .expect("drop the throwaway database");
        }
        Some(other) => panic!(
            "SAFETY REFUSAL: {} carries a foreign marker {other:?}; left in place",
            owned.name
        ),
    }
}

/// Runs `body`, catching a panic raised while it is polled, always runs `cleanup`, then
/// re-raises the body's panic (the router's test futures are not `Send`, so no task spawn).
async fn run_then_clean_up<B, C>(body: B, cleanup: C)
where
    B: std::future::Future<Output = ()>,
    C: std::future::Future<Output = ()>,
{
    let mut body = std::pin::pin!(body);
    let outcome = std::future::poll_fn(|cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body.as_mut().poll(cx))) {
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Ok(std::task::Poll::Ready(())) => std::task::Poll::Ready(Ok(())),
            Err(payload) => std::task::Poll::Ready(Err(payload)),
        }
    })
    .await;
    cleanup.await;
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
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
        "KIMA_NET_DENY_REAL" => Some("1".into()),
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

#[test]
fn prerequisites_refuse_without_the_network_deny_switch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap().to_owned();
    for deny in [None, Some("0"), Some("")] {
        let root = root.clone();
        let env = move |key: &str| match key {
            "KIMA_CLAP_MODEL_DIR" | "KIMA_VIBE_ASSET_DIR" => Some(root.clone()),
            "KIMA_NET_DENY_REAL" => deny.map(str::to_owned),
            _ => None,
        };
        let refusal = std::panic::catch_unwind(|| prerequisites(&env)).unwrap_err();
        let message = refusal
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| refusal.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap();
        assert!(
            message.contains("KIMA_NET_DENY_REAL"),
            "{deny:?}: {message}"
        );
    }
}

#[test]
fn model_dir_without_pinned_files_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let refusal = std::panic::catch_unwind(|| verify_model_dir(dir.path())).unwrap_err();
    let message = refusal.downcast_ref::<String>().cloned().unwrap();
    assert!(
        message.contains("does not verify against its pin"),
        "{message}"
    );
}

#[tokio::test]
async fn cleanup_runs_when_the_body_panics_and_the_panic_is_kept() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let cleaned = Arc::new(AtomicBool::new(false));
    let flag = cleaned.clone();
    let outcome = tokio::spawn(run_then_clean_up(
        async { panic!("body failed") },
        async move { flag.store(true, Ordering::SeqCst) },
    ))
    .await;
    assert!(
        cleaned.load(Ordering::SeqCst),
        "cleanup ran after the panic"
    );
    let payload = outcome.unwrap_err().into_panic();
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"body failed"));
    let cleaned = Arc::new(AtomicBool::new(false));
    let flag = cleaned.clone();
    run_then_clean_up(async {}, async move { flag.store(true, Ordering::SeqCst) }).await;
    assert!(cleaned.load(Ordering::SeqCst), "cleanup ran after success");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the M4 native music checkpoint, pinned private vibe assets and an authorized isolated database; NOT RUN"]
async fn real_music_clap_vibe_end_to_end_on_thirty_generated_tracks() {
    let pre = prerequisites(&|key| std::env::var(key).ok());
    // Library, snapshot store and its reflink/mount check are settled before any database exists.
    let library = tempfile::tempdir().unwrap();
    let root = library.path().canonicalize().unwrap();
    for n in 0..TRACKS {
        write_wav(&root.join(format!("synthetic-{n:02}.wav")), n);
    }
    let snapshots = root.join(".kima-snapshots");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&snapshots).unwrap();
        std::fs::set_permissions(&snapshots, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut embedding_config = kima_ident::EmbeddingConfig {
        model_dir: Some(pre.model_dir.clone()),
        snapshot_dir: Some(snapshots),
        ..Default::default()
    };
    embedding::check_snapshot_setup(&mut embedding_config, &root)
        .expect("snapshot store must support reflinks on the library's filesystem");

    let (admin, opts, owned) = create_owned_db(&pre).await;
    let target = opts.database(&owned.name);
    let asset_dir = pre.asset_dir.clone();
    let (marker_admin, marker_owned) = (admin.clone(), owned.clone());
    run_then_clean_up(
        async move {
            mark_owned(&marker_admin, &marker_owned).await;
            run_end_to_end(target, asset_dir, embedding_config, root).await;
            drop(library);
        },
        async {
            drop_owned(&admin, &owned).await;
        },
    )
    .await;
}

async fn run_end_to_end(
    target: PgConnectOptions,
    asset_dir: PathBuf,
    embedding_config: kima_ident::EmbeddingConfig,
    root: PathBuf,
) {
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(target)
        .await
        .expect("connect to the throwaway database");
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    let mut ids = Vec::new();
    for n in 0..TRACKS {
        ids.push(insert_track(&pool, n).await);
    }
    let state = common::state(pool.clone()).with_library_root(root);
    let mut ctx = state.ident_ctx();
    ctx.config.fingerprinting = false;
    ctx.config.vibe.asset_dir = Some(asset_dir);
    ctx.config.embedding = embedding_config;
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
}
