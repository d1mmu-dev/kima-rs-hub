//! M5 Task 13: real-router contract tests; all vectors/heads are synthetic.
mod common;

use axum_test::TestServer;
use kima_db::PgPool;
use serde_json::{Value, json};

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn map_empty_library_200_empty_and_routes_require_auth(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "listener").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    for path in [
        "/api/vibe/map",
        "/api/vibe/mood-summary",
        "/api/vibe/mood/unknown",
    ] {
        srv.get(path).await.assert_status_unauthorized();
    }
    let res = srv.get("/api/vibe/map").authorization_bearer(&token).await;
    res.assert_status_ok();
    let data: Value = res.json();
    assert_eq!(data["tracks"], json!([]));
    assert_eq!(data["trackCount"], 0);
    assert_eq!(data["computedAt"], "1970-01-01T00:00:00.000Z");
    assert!(data["moodSignature"].is_string());
}

use common::library::{Seed, Seeded, seed};
use kima_db::vibe::MATCHING_SOUND;
use kima_ident::stages::{
    moods,
    vibe_assets::{self, VibeAssets},
};
use kima_vibe::{
    feel_probe::{FeelProbe, ProbeAxis},
    mood_heads::{MOODS, MoodHead, MoodHeads},
};
use std::sync::Arc;

fn probe() -> Arc<FeelProbe> {
    Arc::new(FeelProbe {
        model: "synthetic".into(),
        probe_id: "route-probe".into(),
        valence: ProbeAxis {
            alpha: 1.0,
            bias: 0.5,
            weights: vec![0.0; 512],
        },
        arousal: ProbeAxis {
            alpha: 1.0,
            bias: 0.5,
            weights: vec![0.0; 512],
        },
    })
}
fn heads() -> Arc<MoodHeads> {
    Arc::new(MoodHeads {
        model: "synthetic".into(),
        dims: 512,
        version: "route-heads".into(),
        heads: MOODS
            .iter()
            .map(|m| {
                let mut weights = vec![0.0; 512];
                if *m == "happy" {
                    weights[0] = 1.0;
                }
                MoodHead { weights, bias: 0.0 }
            })
            .collect(),
    })
}
fn with_assets(state: kima_api::AppState, probe: bool, heads_enabled: bool) -> kima_api::AppState {
    let ctx = state.ident_ctx();
    vibe_assets::install(
        &ctx,
        VibeAssets {
            probe: probe.then(self::probe),
            heads: heads_enabled.then(heads),
            report: kima_ml::CapabilityEvaluationReport {
                capabilities: Default::default(),
            },
        },
    );
    state.with_ident(ctx)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mood_snapshot_map_summary_and_detail_agree(pool: PgPool) {
    let happy = mapped(&pool, "Happy", [0.0; 3]).await;
    let group = seed(
        &pool,
        Seed::new("Happy artist", "Happy album", None, "Unmapped"),
    )
    .await;
    sqlx::query("UPDATE tracks SET album_id=$2 WHERE id=$1")
        .bind(happy.track)
        .bind(group.album)
        .execute(&pool)
        .await
        .unwrap();
    for n in 0..5 {
        let s = mapped(&pool, &format!("Neutral {n}"), [0.0; 3]).await;
        let mut v = vec![0.0f32; 512];
        v[1] = 1.0;
        sqlx::query("UPDATE track_embeddings SET embedding=$2::real[]::vector WHERE track_id=$1")
            .bind(s.track)
            .bind(v)
            .execute(&pool)
            .await
            .unwrap();
    }
    let state = with_assets(common::state(pool.clone()), true, true);
    let (_, token) = common::user_with_token(&state, "listener").await;
    moods::run_pass(state.ident.as_ref().unwrap())
        .await
        .unwrap();
    let snap = moods::snapshot(state.ident.as_ref().unwrap()).unwrap();
    assert_eq!(snap.views[&happy.track].dominant, "moodHappy");
    let extra = mapped(&pool, "Unscored", [0.0; 3]).await;
    let srv = TestServer::new(kima_api::router(state.clone())).unwrap();
    let res = srv.get("/api/vibe/map").authorization_bearer(&token).await;
    res.assert_status_ok();
    let data: Value = res.json();
    assert_eq!(data["moodSignature"], snap.signature);
    let t = data["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == happy.track.to_string())
        .unwrap();
    assert_eq!(t["dominantMood"], "moodHappy");
    assert_eq!(t["moodScore"], snap.views[&happy.track].score);
    assert_eq!(t["moods"].as_object().unwrap().len(), 5);
    assert_eq!(
        (t["x"].as_f64(), t["y"].as_f64(), t["z"].as_f64()),
        (Some(0.5), Some(0.5), Some(0.5))
    );
    let res = srv
        .get("/api/vibe/mood-summary")
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    let summary: Value = res.json();
    assert_eq!(
        summary["shares"],
        json!([{"key":"neutral","fraction":5.0/6.0},{"key":"moodHappy","fraction":1.0/6.0}])
    );
    assert_eq!(
        summary["albums"],
        json!({group.album.to_string():"moodHappy"})
    );
    assert_eq!(
        summary["artists"],
        json!({group.artist.to_string():"moodHappy"})
    );
    let _: kima_api_types::vibe::MoodSummary = serde_json::from_value(summary).unwrap();
    for (id, expected) in [
        (happy.track.to_string(), "moodHappy"),
        (extra.track.to_string(), "unanalyzed"),
        ("not-a-uuid".into(), "unanalyzed"),
    ] {
        let res = srv
            .get(&format!("/api/vibe/mood/{id}"))
            .authorization_bearer(&token)
            .await;
        res.assert_status_ok();
        assert_eq!(res.json::<Value>(), json!({"dominantMood":expected}));
        let _: kima_api_types::vibe::TrackMood = res.json();
    }
    let before = srv
        .get("/api/vibe/map")
        .authorization_bearer(&token)
        .add_header("accept-encoding", "gzip")
        .await;
    sqlx::query("UPDATE mood_state SET fingerprint='changed-synthetic-snapshot'")
        .execute(&pool)
        .await
        .unwrap();
    let rt = moods::runtime(state.ident.as_ref().unwrap()).await.unwrap();
    rt.snapshots.refresh(&pool, rt.heads()).await.unwrap();
    let after = srv
        .get("/api/vibe/map")
        .authorization_bearer(&token)
        .add_header("accept-encoding", "gzip")
        .await;
    assert_ne!(
        before.as_bytes(),
        after.as_bytes(),
        "mood signature invalidates gzip"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn map_and_summary_202_only_while_layout_possible(pool: PgPool) {
    let s = mapped(&pool, "Pending", [0.0; 3]).await;
    sqlx::query("DELETE FROM matching_map")
        .execute(&pool)
        .await
        .unwrap();
    let state = with_assets(common::state(pool.clone()), true, false);
    let (_, token) = common::user_with_token(&state, "listener").await;
    let srv = TestServer::new(kima_api::router(state.clone())).unwrap();
    for path in ["/api/vibe/map", "/api/vibe/mood-summary"] {
        let res = srv.get(path).authorization_bearer(&token).await;
        res.assert_status(axum::http::StatusCode::ACCEPTED);
        assert_eq!(res.json::<Value>(), json!({"computing":true}));
    }
    let mut disabled = state.ident.clone().unwrap();
    disabled.config.embedding.disabled = true;
    let srv = TestServer::new(kima_api::router(state.with_ident(disabled))).unwrap();
    srv.get("/api/vibe/map")
        .authorization_bearer(&token)
        .await
        .assert_status_ok();
    common::library::mark_missing(&pool, s.track).await;
    let state = with_assets(common::state(pool), true, false);
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    srv.get("/api/vibe/map")
        .authorization_bearer(&token)
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn edited_titles_preserves_null_empty_and_persons_title(pool: PgPool) {
    let a = mapped(&pool, "Raw A", [0.0; 3]).await;
    let b = mapped(&pool, "Raw B", [0.0; 3]).await;
    let c = mapped(&pool, "Raw C", [0.0; 3]).await;
    sqlx::query("UPDATE tracks SET display_title='Personal title' WHERE id=$1")
        .bind(b.track)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET display_title='' WHERE id=$1")
        .bind(c.track)
        .execute(&pool)
        .await
        .unwrap();
    let unknown = uuid::Uuid::now_v7();
    let titles = kima_db::vibe::read::edited_titles(&pool, &[a.track, b.track, c.track, unknown])
        .await
        .unwrap();
    assert_eq!(titles.len(), 3);
    assert_eq!(titles[&a.track], None);
    assert_eq!(titles[&b.track], Some("Personal title".into()));
    assert_eq!(titles[&c.track], Some(String::new()));
    assert!(!titles.contains_key(&unknown));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn map_filters_missing_wrong_model_and_stale_rows(pool: PgPool) {
    let current = mapped(&pool, "Current", [0.0; 3]).await;
    let missing = mapped(&pool, "Missing", [1.0; 3]).await;
    let wrong = mapped(&pool, "Wrong", [2.0; 3]).await;
    let stale = mapped(&pool, "Stale", [3.0; 3]).await;
    common::library::mark_missing(&pool, missing.track).await;
    sqlx::query("UPDATE track_embeddings SET model_id='wrong' WHERE track_id=$1")
        .bind(wrong.track)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE matching_map SET embedded_at=embedded_at-interval '1 second' WHERE track_id=$1",
    )
    .bind(stale.track)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO track_clap_readings (track_id,embedded_at,instrumentalness,acousticness,valence_raw,arousal_raw) SELECT track_id,created_at-interval '1 second',0.3,0.4,0.75,0.6 FROM track_embeddings WHERE track_id=$1").bind(current.track).execute(&pool).await.unwrap();
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "listener").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let res = srv.get("/api/vibe/map").authorization_bearer(&token).await;
    res.assert_status_ok();
    let data: Value = res.json();
    assert_eq!(data["trackCount"], 1);
    assert_eq!(data["tracks"][0]["id"], current.track.to_string());
    assert_eq!(data["tracks"][0]["valence"], Value::Null);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_global_reads_allow_nonmain_profiles_without_personal_data_or_feature_enablement(
    pool: PgPool,
) {
    use kima_core::profile::ProfileKind;
    use kima_db::profiles::{self, NewProfile};
    mapped(&pool, "Global", [0.0; 3]).await;
    let state = common::state(pool);
    let (admin, main) = common::user_with_token(&state, "admin").await;
    let (other, other_main) = common::user_with_token(&state, "other").await;
    let mut tokens = vec![main, other_main];
    for (user, kind) in [
        (&admin, ProfileKind::Kids),
        (&admin, ProfileKind::Standard),
        (&other, ProfileKind::Standard),
    ] {
        let p = profiles::create(
            &state.pool,
            user.account(),
            NewProfile {
                name: format!("Secret {kind:?}"),
                kind,
                avatar: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        tokens.push(state.jwt.issue_access(
            user.account(),
            p.id,
            &user.username,
            &user.role,
            user.token_version,
        ));
    }
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    for path in [
        "/api/vibe/map",
        "/api/vibe/mood-summary",
        "/api/vibe/mood/unknown",
    ] {
        let mut expected = None;
        for token in &tokens {
            let res = srv.get(path).authorization_bearer(token).await;
            res.assert_status_ok();
            let data: Value = res.json();
            assert!(!data.to_string().contains("Secret"));
            if let Some(expected) = &expected {
                assert_eq!(&data, expected);
            } else {
                expected = Some(data);
            }
        }
        srv.get(path).await.assert_status_unauthorized();
    }
    let features = srv
        .get("/api/system/features")
        .authorization_bearer(&tokens[0])
        .await
        .json::<Value>();
    assert_eq!(features["vibeEmbeddings"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn read_errors_keep_endpoint_specific_upstream_messages(pool: PgPool) {
    let state = common::state(pool.clone());
    let (_, token) = common::user_with_token(&state, "listener").await;
    sqlx::query("DROP TABLE matching_map")
        .execute(&pool)
        .await
        .unwrap();
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    for (path, message) in [
        ("/api/vibe/map", "Failed to read the vibe map"),
        ("/api/vibe/mood-summary", "Failed to read the mood summary"),
    ] {
        let res = srv.get(path).authorization_bearer(&token).await;
        res.assert_status_internal_server_error();
        assert_eq!(res.json::<Value>(), json!({"error":message}));
    }
    // Heads installed, no snapshot yet: this read must build it on demand and map DB errors.
    let state = with_assets(common::state(pool.clone()), false, true);
    sqlx::query("DROP TABLE mood_state")
        .execute(&pool)
        .await
        .unwrap();
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let res = srv
        .get("/api/vibe/mood/unknown")
        .authorization_bearer(&token)
        .await;
    res.assert_status_internal_server_error();
    assert_eq!(
        res.json::<Value>(),
        json!({"error":"Failed to read the track's mood"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
#[ignore = "manual 100k synthetic map latency measurement on the isolated verification database"]
async fn map_100k_synthetic_latency(pool: PgPool) {
    let group = seed(
        &pool,
        Seed::new("Synthetic artist", "Synthetic album", None, "Unmapped"),
    )
    .await;
    // This benchmark's SQLx-owned child needs vectors for eligibility, not an ANN index.
    sqlx::query("DROP INDEX track_embeddings_cosine_idx")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,codec,path,size,mtime,fingerprint) SELECT md5('vibe-bench-'||n)::uuid,$1,'Synthetic artist','Track '||n,'mp3','bench/'||n,1,now(),decode(repeat('01',32),'hex') FROM generate_series(1,100000) n").bind(group.album).execute(&pool).await.unwrap();
    let mut vector = vec![0.0f32; 512];
    vector[0] = 1.0;
    sqlx::query("INSERT INTO track_embeddings (track_id,file_generation,model_id,model_sha256,pipeline_version,embedding) SELECT id,0,$1,$2,$3,$4::real[]::vector FROM tracks WHERE path LIKE 'bench/%'")
        .bind(MATCHING_SOUND.model_id).bind(MATCHING_SOUND.model_sha256.to_vec()).bind(MATCHING_SOUND.pipeline_version).bind(vector).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO matching_map (track_id,embedded_at,x,y,z,colour,laid_out) SELECT track_id,created_at,(row_number() OVER (ORDER BY track_id)%101)::float8,(row_number() OVER (ORDER BY track_id)%97)::float8,(row_number() OVER (ORDER BY track_id)%89)::float8,0.5,true FROM track_embeddings").execute(&pool).await.unwrap();
    sqlx::query("ANALYZE").execute(&pool).await.unwrap();
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "bench").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    for encoding in ["gzip", "gzip", "identity"] {
        let started = std::time::Instant::now();
        let res = srv
            .get("/api/vibe/map")
            .authorization_bearer(&token)
            .add_header("accept-encoding", encoding)
            .await;
        let seconds = started.elapsed().as_secs_f64();
        res.assert_status_ok();
        println!(
            "100k map encoding={encoding} seconds={seconds:.6} bytes={}",
            res.as_bytes().len()
        );
        let data: Value = if encoding == "gzip" {
            serde_json::from_reader(flate2::read::GzDecoder::new(res.as_bytes().as_ref())).unwrap()
        } else {
            res.json()
        };
        assert_eq!(data["trackCount"], 100000);
        assert_eq!(data["tracks"].as_array().unwrap().len(), 100000);
    }
}

async fn mapped(pool: &PgPool, title: &str, xyz: [f64; 3]) -> Seeded {
    let s = seed(pool, Seed::new("Map artist", "Map album", None, title)).await;
    let mut v = vec![0.0f32; 512];
    v[0] = 1.0;
    sqlx::query("INSERT INTO track_embeddings (track_id,file_generation,model_id,model_sha256,pipeline_version,embedding) VALUES ($1,0,$2,$3,$4,$5::real[]::vector)")
        .bind(s.track).bind(MATCHING_SOUND.model_id).bind(MATCHING_SOUND.model_sha256.to_vec()).bind(MATCHING_SOUND.pipeline_version).bind(v)
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO matching_map (track_id,embedded_at,x,y,z,colour,laid_out) SELECT track_id,created_at,$2,$3,$4,0.4,true FROM track_embeddings WHERE track_id=$1")
        .bind(s.track).bind(xyz[0]).bind(xyz[1]).bind(xyz[2]).execute(pool).await.unwrap();
    s
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn map_shape_one_scale_sorted_rows_and_readings(pool: PgPool) {
    let a = mapped(&pool, "A", [-4.0, 2.0, 8.0]).await;
    let b = mapped(&pool, "B", [4.0, 6.0, 10.0]).await;
    sqlx::query("INSERT INTO track_clap_readings (track_id,embedded_at,instrumentalness,acousticness,valence_raw,arousal_raw) SELECT track_id,created_at,0.3,0.4,0.75,0.6 FROM track_embeddings WHERE track_id=$1")
        .bind(a.track).execute(&pool).await.unwrap();
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "listener").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let res = srv.get("/api/vibe/map").authorization_bearer(&token).await;
    res.assert_status_ok();
    let data: Value = res.json();
    assert_eq!(data["trackCount"], 2);
    let tracks = data["tracks"].as_array().unwrap();
    let mut ids = vec![a.track.to_string(), b.track.to_string()];
    ids.sort();
    assert_eq!(
        tracks
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids
    );
    let row = tracks
        .iter()
        .find(|t| t["id"] == a.track.to_string())
        .unwrap();
    assert_eq!(
        (row["x"].as_f64(), row["y"].as_f64(), row["z"].as_f64()),
        (Some(0.0), Some(0.25), Some(0.375))
    );
    assert_eq!(row["artist"], "Map artist");
    assert_eq!(row["albumId"], a.album.to_string());
    assert_eq!(row["artistId"], a.artist.to_string());
    assert_eq!(row["title"], "A");
    assert_eq!(row["energy"], Value::Null);
    assert_eq!(row["valence"], 0.75);
    assert_eq!(row["coverUrl"], Value::Null);
    assert_eq!(row["moods"], json!({}));
    assert_eq!(row["dominantMood"], "unanalyzed");
    assert_eq!(row["moodScore"], 0.0);
    assert_eq!(
        tracks
            .iter()
            .find(|t| t["id"] == b.track.to_string())
            .unwrap()["valence"],
        Value::Null
    );
    let dto: kima_api_types::vibe::MapResponse = serde_json::from_value(data.clone()).unwrap();
    assert_eq!(serde_json::to_value(dto).unwrap(), data);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn map_gzip_cache_tracks_content_not_just_layout_version(pool: PgPool) {
    let s = mapped(&pool, "Before", [0.0; 3]).await;
    let state = common::state(pool.clone());
    let (_, token) = common::user_with_token(&state, "listener").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let first = srv
        .get("/api/vibe/map")
        .authorization_bearer(&token)
        .add_header("accept-encoding", "gzip")
        .await;
    first.assert_status_ok();
    first.assert_header("content-encoding", "gzip");
    first.assert_header("vary", "Accept-Encoding");
    first.assert_header("content-type", "application/json; charset=utf-8");
    let bytes = first.as_bytes().to_vec();
    let again = srv
        .get("/api/vibe/map")
        .authorization_bearer(&token)
        .add_header("accept-encoding", "gzip")
        .await;
    assert_eq!(again.as_bytes(), bytes.as_slice());
    // Neither update bumps tracks.updated_at, and albums/artists have no updated_at.
    sqlx::query("UPDATE tracks SET title='After' WHERE id=$1")
        .bind(s.track)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE albums SET image_path='new-cover.jpg' WHERE id=$1")
        .bind(s.album)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE artists SET name='Renamed' WHERE id=$1")
        .bind(s.artist)
        .execute(&pool)
        .await
        .unwrap();
    let changed = srv
        .get("/api/vibe/map")
        .authorization_bearer(&token)
        .add_header("accept-encoding", "gzip")
        .await;
    assert_ne!(changed.as_bytes(), bytes.as_slice());
    let decoded: Value =
        serde_json::from_reader(flate2::read::GzDecoder::new(changed.as_bytes().as_ref())).unwrap();
    assert_eq!(decoded["tracks"][0]["title"], "After");
    assert_eq!(decoded["tracks"][0]["artist"], "Renamed");
    assert_eq!(decoded["tracks"][0]["coverUrl"], s.album.to_string());
    let plain = srv
        .get("/api/vibe/map")
        .authorization_bearer(&token)
        .add_header("accept-encoding", "gzip;q=0")
        .await;
    assert!(plain.maybe_header("content-encoding").is_none());
    assert_eq!(plain.json::<Value>(), decoded);
    kima_db::vibe::state::build_matching(&pool, &probe())
        .await
        .unwrap();
    sqlx::query("UPDATE matching_state SET map_updated_at='2026-01-02T03:04:05.678Z'")
        .execute(&pool)
        .await
        .unwrap();
    let versioned = srv
        .get("/api/vibe/map")
        .authorization_bearer(&token)
        .add_header("accept-encoding", "gzip")
        .await;
    assert_ne!(versioned.as_bytes(), changed.as_bytes());
    let data: Value =
        serde_json::from_reader(flate2::read::GzDecoder::new(versioned.as_bytes().as_ref()))
            .unwrap();
    assert_eq!(data["computedAt"], "2026-01-02T03:04:05.678Z");
}
