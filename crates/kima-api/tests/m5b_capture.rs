//! Capture harness for the M5b wire fixtures (plan `2026-11-19-m5b-mixes-radio.md`, Task 19), driven by
//! `tools/capture-m5b-fixtures.inc.sh` (`KIMA_CAPTURE_SET=m5b tools/capture-api-fixtures.sh`). Not a test: both
//! functions are `#[ignore]`d and refuse to run outside that script's run-owned capture database.
//!
//! - `m5b_capture_seed` writes, into the scanned synthetic library, what kima-rs cannot compute without private models:
//!   sound vectors of the engine's identity, mood scores, CLAP readings, DSP tempo/key, mood tags, the stored mood
//!   statistics, album/artist cover tokens, owner genres and similar artists. Plain SQL, the same rows the mix tests
//!   seed (`kima-db/tests/mixes_common`).
//! - `m5b_capture_serve` serves the real router on loopback with synthetic mood heads installed (the production server
//!   never loads heads while they are blocked, owner M5 Q3) and a one-release synthetic catalog (kima-rs's own catalog
//!   is empty, D-267), so the recipe paths and the release window answer 2xx. It runs until the script kills it.
//!
//! `seed_then_heads_answer_every_recipe_page` (a normal SQLx test) runs the same seed and harness state on a
//! 40-song test library and requires the pages the capture needs to answer 2xx, so a broken seed shows up here
//! rather than in the parent's Docker run.

mod common;

use std::sync::Arc;

use axum_test::TestServer;
use common::library::{Seed, seed};
use kima_api_types::releases::{ChoiceItem, NoteItem, Release, ReleaseItem};
use kima_db::PgPool;
use kima_db::vibe::MATCHING_SOUND;
use kima_db::vibe::moods::{READINGS, mood_state_version};
use kima_ident::stages::vibe_assets::{self, VibeAssets};
use kima_vibe::mood_heads::{MOODS, MoodHead, MoodHeads};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

/// The synthetic heads' version (their weights are never used: the mix paths read stored scores).
const HEADS_VERSION: &str = "m5b-capture-heads";
/// When every seeded vector was embedded.
const EMBEDDED_AT: &str = "2026-11-01T00:00:00Z";
/// Ports of databases this harness must never touch (dev, kima-web dev, live app, shared test cluster).
const FORBIDDEN_PORTS: [u16; 4] = [5456, 55433, 52064, 58826];

fn heads() -> MoodHeads {
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

/// The capture script's database, or a panic before anything is written: the script's switch and network guard
/// set, a loopback `kima` database on a port that is not a known application or shared test cluster, holding the
/// one capture account and nothing else.
async fn capture_pool() -> PgPool {
    assert_eq!(
        std::env::var("KIMA_M5B_CAPTURE").as_deref(),
        Ok("1"),
        "run by tools/capture-m5b-fixtures.inc.sh only"
    );
    assert_eq!(std::env::var("KIMA_NET_DENY_REAL").as_deref(), Ok("1"));
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let opts: PgConnectOptions = url.parse().expect("DATABASE_URL parses");
    assert_eq!(
        opts.get_host(),
        "127.0.0.1",
        "loopback capture database only"
    );
    assert!(
        !FORBIDDEN_PORTS.contains(&opts.get_port()),
        "not a capture database port"
    );
    assert_eq!(opts.get_database(), Some("kima"));
    assert_eq!(std::env::var("PGDATABASE").as_deref(), Ok("kima"));
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await
        .expect("connect");
    let (db, user): (String, String) =
        sqlx::query_as("SELECT current_database(), current_user::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((db.as_str(), user.as_str()), ("kima", "kima"));
    let users: Vec<String> = sqlx::query_scalar("SELECT username FROM users")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        users,
        ["fixtureadmin"],
        "only the capture account may exist"
    );
    pool
}

/// One of four synthetic mood groups, by the track's place in id order: calm, upbeat, intense, melancholic.
struct Group {
    moods: &'static [(&'static str, f64)],
    tags: &'static [&'static str],
    /// (arousal, instrumental, acoustic) readings, 0..1.
    readings: (f32, f32, f32),
    /// (bpm, confidence on kima's 0..1 scale).
    tempo: (f64, f64),
    mode: &'static str,
}

const GROUPS: [Group; 4] = [
    Group {
        moods: &[
            ("calm", 4.0),
            ("dreamy", 2.0),
            ("sleep", 2.0),
            ("focus", 1.0),
        ],
        tags: &["calm", "dreamy"],
        readings: (0.1, 0.8, 0.8),
        tempo: (80.0, 0.6),
        mode: "major",
    },
    Group {
        moods: &[
            ("happy", 4.0),
            ("uplifting", 3.0),
            ("energetic", 4.0),
            ("party", 4.0),
            ("groovy", 2.0),
            ("workout", 3.0),
            ("fun_quirky", 2.0),
            ("summer", 2.0),
            ("driving", 2.0),
        ],
        tags: &["happy", "energetic", "party"],
        readings: (0.9, 0.2, 0.2),
        tempo: (128.0, 0.8),
        mode: "major",
    },
    Group {
        moods: &[
            ("aggressive", 4.0),
            ("dark", 3.0),
            ("epic", 3.0),
            ("energetic", 2.0),
            ("workout", 2.0),
        ],
        tags: &["aggressive", "dark"],
        readings: (0.9, 0.3, 0.1),
        tempo: (140.0, 0.7),
        mode: "minor",
    },
    Group {
        moods: &[
            ("sad", 4.0),
            ("nostalgic", 3.0),
            ("romantic", 2.0),
            ("dreamy", 1.0),
        ],
        tags: &["sad", "nostalgic"],
        readings: (0.1, 0.4, 0.6),
        tempo: (70.0, 0.5),
        mode: "minor",
    },
];

#[tokio::test]
#[ignore = "capture harness: tools/capture-m5b-fixtures.inc.sh"]
async fn m5b_capture_seed() {
    let pool = capture_pool().await;
    let n = seed_capture(&pool).await;
    println!("m5b capture seed: {n} tracks");
}

/// Writes the seed (see the module doc) over the present tracks; returns how many.
async fn seed_capture(pool: &PgPool) -> usize {
    let tracks: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM tracks WHERE missing_since IS NULL ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert!(
        tracks.len() >= 40,
        "the m5b library is scanned ({} tracks)",
        tracks.len()
    );
    let mut tx = pool.begin().await.unwrap();
    let mut unit = vec![0.0_f32; 512];
    unit[0] = 1.0;
    let mood_cols = MOODS
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mood_vals = (0..MOODS.len())
        .map(|k| format!("v[{}]::real", k + 1))
        .collect::<Vec<_>>()
        .join(", ");
    for (i, track) in tracks.iter().enumerate() {
        let g = &GROUPS[i % GROUPS.len()];
        sqlx::query(
            "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, \
                 embedding, created_at)
             SELECT id, ml_generation, $2, $3, $4, $5::real[]::vector, $6::timestamptz FROM tracks WHERE id = $1
             ON CONFLICT (track_id) DO NOTHING",
        )
        .bind(track)
        .bind(MATCHING_SOUND.model_id)
        .bind(MATCHING_SOUND.model_sha256.to_vec())
        .bind(MATCHING_SOUND.pipeline_version)
        .bind(&unit)
        .bind(EMBEDDED_AT)
        .execute(&mut *tx)
        .await
        .unwrap();
        let values: Vec<f64> = MOODS
            .iter()
            .map(|m| {
                g.moods
                    .iter()
                    .find(|(n, _)| n == m)
                    .map_or(0.0, |(_, v)| *v)
            })
            .collect();
        sqlx::query(&format!(
            "INSERT INTO track_moods (track_id, embedded_at, weights, {mood_cols})
             SELECT $1, e.created_at, $2, {mood_vals} FROM track_embeddings e, (SELECT $3::float8[] AS v) x
             WHERE e.track_id = $1 ON CONFLICT (track_id) DO NOTHING"
        ))
        .bind(track)
        .bind(HEADS_VERSION)
        .bind(&values)
        .execute(&mut *tx)
        .await
        .unwrap();
        let (arousal, instrumental, acoustic) = g.readings;
        sqlx::query(
            "INSERT INTO track_clap_readings (track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw)
             SELECT $1, created_at, $2, $3, 0.6, $4 FROM track_embeddings WHERE track_id = $1
             ON CONFLICT (track_id) DO NOTHING",
        )
        .bind(track)
        .bind(instrumental)
        .bind(acoustic)
        .bind(arousal)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track_analysis (track_id, file_generation, dsp_tempo_pipeline_version, tempo_bpm, tempo_confidence,
                 tempo_computed_at, dsp_key_pipeline_version, key_tonic, key_mode, key_strength, key_computed_at)
             SELECT id, ml_generation, 'm5b-capture', $2, $3, now(), 'm5b-capture', $4::smallint, $5, 0.7, now()
             FROM tracks WHERE id = $1
             ON CONFLICT (track_id) DO UPDATE SET dsp_tempo_pipeline_version = EXCLUDED.dsp_tempo_pipeline_version,
                 tempo_bpm = EXCLUDED.tempo_bpm, tempo_confidence = EXCLUDED.tempo_confidence,
                 tempo_computed_at = EXCLUDED.tempo_computed_at,
                 dsp_key_pipeline_version = EXCLUDED.dsp_key_pipeline_version, key_tonic = EXCLUDED.key_tonic,
                 key_mode = EXCLUDED.key_mode, key_strength = EXCLUDED.key_strength,
                 key_computed_at = EXCLUDED.key_computed_at",
        )
        .bind(track)
        .bind(g.tempo.0)
        .bind(g.tempo.1)
        .bind((i % 12) as i16)
        .bind(g.mode)
        .execute(&mut *tx)
        .await
        .unwrap();
        let tags: Vec<String> = g.tags.iter().map(|t| t.to_string()).collect();
        sqlx::query("UPDATE tracks SET mood_tags = $2 WHERE id = $1")
            .bind(track)
            .bind(&tags)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    // The stored statistics the recipes rank against: mean 0, SD 1 for every mood and reading.
    let names: Vec<String> = MOODS
        .iter()
        .chain(READINGS.iter())
        .flat_map(|n| [format!("{n}_mean"), format!("{n}_sd")])
        .collect();
    let values: Vec<f64> = names
        .iter()
        .map(|c| if c.ends_with("_sd") { 1.0 } else { 0.0 })
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
    sqlx::query("DELETE FROM mood_state")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO mood_state (id, version, fingerprint, track_count, {cols})
         SELECT 'default', $1, 'm5b-capture', $2, {vals} FROM (SELECT $3::float8[] AS v) x"
    ))
    .bind(mood_state_version(&heads()))
    .bind(tracks.len() as i32)
    .bind(&values)
    .execute(&mut *tx)
    .await
    .unwrap();
    // Covers: every album and library artist gets a stored image, so cover tokens are populated (D-265).
    sqlx::query(
        "UPDATE albums SET image_path = id::text || '.jpg', image_source = 'coverartarchive' WHERE image_path IS NULL",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE artists SET image_path = id::text || '.jpg', image_source = 'local' WHERE image_path IS NULL",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    // The m5b artists' own genres, as an owner's edit would store them (genre radio and stations).
    sqlx::query(
        "UPDATE artists SET user_genres = ARRAY[CASE WHEN name LIKE 'Nineties%' THEN 'Rock' ELSE 'Electronic' END]
         WHERE name LIKE 'Nineties%' OR name LIKE 'Noughties%'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    // Similar artists of every library artist: one kima does not hold (recommended by name) and one it holds as an
    // artist row without songs (recommended with its id, cover and album count).
    let away = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO artists (id, name, name_key, mbid, image_path, image_source)
         VALUES ($1, 'Faraway Band', 'faraway band', 'f0000000-0000-4000-8000-000000000001', $1::text || '.jpg', 'local')",
    )
    .bind(away)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, release_group_mbid)
         VALUES ($1, $2, 'Far Away', 'far away', 'f0000000-0000-4000-8000-000000000002')",
    )
    .bind(Uuid::now_v7())
    .bind(away)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE artists SET "similar" = '[{"name": "Faraway Band", "mbid": "f0000000-0000-4000-8000-000000000001", "match": 0.9},
                                             {"name": "Nowhere Collective", "mbid": null, "match": 0.6}]'::jsonb
           WHERE id <> $1"#,
    )
    .bind(away)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    tracks.len()
}

/// The synthetic catalog: one release that asks one choice.
fn catalog() -> Vec<Release> {
    vec![Release {
        version: "0.2.0".into(),
        title: "Kima 0.2.0".into(),
        summary: "A synthetic release for the wire fixtures.".into(),
        date: "2026-11-01".into(),
        note: None,
        items: vec![
            ReleaseItem::Note(NoteItem {
                key: "capture.note".into(),
                title: "Mixes and radio".into(),
                summary: "Daily mixes, mood mixes and radio stations.".into(),
                body: "Synthetic text.".into(),
                ..Default::default()
            }),
            ReleaseItem::Choice(ChoiceItem {
                key: "capture.choice".into(),
                title: "A synthetic choice".into(),
                summary: "Asked so the window has something to answer.".into(),
                body: "Synthetic text.".into(),
                discloses: Some("Nothing leaves this machine.".into()),
                default_value: false,
                ..Default::default()
            }),
        ],
    }]
}

/// The router's state with the synthetic heads and catalog installed.
async fn harness_state(pool: PgPool, secret: &str) -> kima_api::AppState {
    let heads = heads();
    // The stored statistics are this harness's heads' (the seed wrote them).
    let version: String = sqlx::query_scalar("SELECT version FROM mood_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, mood_state_version(&heads));
    let state = kima_api::AppState::new(pool, secret).with_releases(catalog());
    let ctx = state.ident_ctx();
    vibe_assets::install(
        &ctx,
        VibeAssets {
            probe: None,
            heads: Some(Arc::new(heads)),
            report: kima_ml::CapabilityEvaluationReport {
                capabilities: Default::default(),
            },
        },
    );
    state.with_ident(ctx)
}

#[tokio::test]
#[ignore = "capture harness: tools/capture-m5b-fixtures.inc.sh"]
async fn m5b_capture_serve() {
    let pool = capture_pool().await;
    let port: u16 = std::env::var("KIMA_M5B_CAPTURE_PORT")
        .expect("KIMA_M5B_CAPTURE_PORT")
        .parse()
        .expect("a port");
    let state = harness_state(pool, &Uuid::new_v4().to_string()).await;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind the capture port");
    println!("m5b capture harness listening on 127.0.0.1:{port}");
    axum::serve(
        listener,
        kima_api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
}

/// The seed and the harness state on a 40-song library like the capture's (two decades, four artists each): every
/// mood page, the mood and workout stations and the release window answer 2xx, the pages with songs.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn seed_then_heads_answer_every_recipe_page(pool: PgPool) {
    let artists = [
        "Nineties A",
        "Nineties B",
        "Nineties C",
        "Nineties D",
        "Noughties A",
        "Noughties B",
        "Noughties C",
        "Noughties D",
    ];
    for (a, artist) in artists.iter().enumerate() {
        let year = if a < 4 { 1994 } else { 2003 };
        for n in 1..=5 {
            let title = format!("{artist} {n}");
            seed(
                &pool,
                Seed::new(artist, &format!("{artist} Album"), Some(year), &title).at(1, n),
            )
            .await;
        }
    }
    assert_eq!(seed_capture(&pool).await, 40);

    // The live state first (no heads, the shipped empty catalog), as the capture's first server answers.
    let live = common::state(pool.clone());
    let (_, live_token) = common::user_with_token(&live, "fixtureadmin").await;
    let live = TestServer::new(kima_api::router(live)).unwrap();
    let mixes = live
        .get("/api/mixes?tz=Europe/Warsaw")
        .authorization_bearer(&live_token)
        .await
        .json::<serde_json::Value>();
    let first = &mixes[0];
    assert!(
        first["trackCount"].as_u64().unwrap() >= 8,
        "a mix without heads: {mixes}"
    );
    assert!(
        !first["coverUrls"].as_array().unwrap().is_empty(),
        "{mixes}"
    );
    let ids: Vec<String> = sqlx::query_scalar("SELECT id::text FROM tracks ORDER BY id LIMIT 3")
        .fetch_all(&pool)
        .await
        .unwrap();
    for id in &ids {
        live.post("/api/plays")
            .authorization_bearer(&live_token)
            .json(&serde_json::json!({"trackId": id, "secondsListened": 180}))
            .await
            .assert_status_ok();
    }
    let recs = live
        .get("/api/recommendations/for-you")
        .authorization_bearer(&live_token)
        .await
        .json::<serde_json::Value>();
    let names: Vec<&str> = recs["artists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Faraway Band", "Nowhere Collective"], "{recs}");
    assert_eq!(recs["artists"][0]["albumCount"], 1, "{recs}");
    assert!(recs["artists"][0]["coverArt"].is_string(), "{recs}");
    for kind in ["discovery", "favorites", "all"] {
        let radio = live
            .get(&format!("/api/library/radio?type={kind}&limit=10"))
            .authorization_bearer(&live_token)
            .await
            .json::<serde_json::Value>();
        assert!(
            !radio["tracks"].as_array().unwrap().is_empty(),
            "{kind}: {radio}"
        );
    }
    let bucket = live
        .get("/api/mixes/mood/buckets/chill")
        .authorization_bearer(&live_token)
        .await;
    bucket.assert_status(axum::http::StatusCode::BAD_REQUEST);

    // Same secret and account: the same sign-in works on the harness.
    let state = harness_state(pool, common::SECRET).await;
    let token = live_token;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let get = |path: String| {
        let server = &server;
        let token = token.clone();
        async move {
            let res = server.get(&path).authorization_bearer(&token).await;
            (res.status_code().as_u16(), res.json::<serde_json::Value>())
        }
    };
    for mood in ["happy", "sad", "chill", "aggressive", "party"] {
        let (status, body) = get(format!("/api/mixes/mood/buckets/{mood}")).await;
        assert_eq!(status, 200, "{mood}: {body}");
        assert!(body["trackCount"].as_u64().unwrap() >= 8, "{mood}: {body}");
        assert!(
            !body["coverUrls"].as_array().unwrap().is_empty(),
            "{mood}: covers"
        );
        assert!(
            !body["tracks"][0]["moodTags"].as_array().unwrap().is_empty(),
            "{mood}: tags"
        );
    }
    let (status, presets) = get("/api/mixes/mood/buckets/presets".into()).await;
    assert_eq!(status, 200);
    assert!(
        presets
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["trackCount"].as_u64().unwrap() > 0),
        "{presets}"
    );
    let (status, stations) = get("/api/library/radio/stations".into()).await;
    assert_eq!(status, 200);
    let value = stations["stations"][0]["value"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, mood) = get(format!(
        "/api/library/radio?type=mood&value={value}&seed=capture&offset=0"
    ))
    .await;
    assert_eq!(status, 200, "{mood}");
    assert!(
        !mood["tracks"].as_array().unwrap().is_empty(),
        "mood station {value}: {mood}"
    );
    let (status, workout) =
        get("/api/library/radio?type=workout&seed=capture&offset=0".into()).await;
    assert_eq!(status, 200, "{workout}");
    let (status, decades) = get("/api/library/decades".into()).await;
    assert_eq!(
        (status, decades["decades"].as_array().unwrap().len()),
        (200, 2),
        "{decades}"
    );
    let (status, row) = get("/api/library/radio/row?scope=all".into()).await;
    assert_eq!(status, 200);
    let genres = row["stations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["type"] == "genre")
        .count();
    assert_eq!(genres, 2, "{row}");
    let (status, current) = get("/api/whats-new/current".into()).await;
    assert_eq!(status, 200, "{current}");
    let (status, mixes) = get("/api/mixes?tz=Europe/Warsaw".into()).await;
    assert_eq!(status, 200);
    assert!(!mixes.as_array().unwrap().is_empty(), "{mixes}");
}
