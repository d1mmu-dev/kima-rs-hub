//! Task 18: the `vibeEmbeddings` predicate (spec §5.8) and the degradation matrix (spec §4.4,
//! §4.3) through the real router. All vectors and assets are synthetic.
use super::route_contracts::matching_library;
use super::*;

fn no_similar() -> Value {
    json!({"error":"No similar tracks found","message":"This track may not have been analyzed yet, or no analyzer is running"})
}

/// Map rows for every stored vector, so the map has something current to draw.
async fn lay_out(pool: &PgPool) {
    sqlx::query("INSERT INTO matching_map (track_id,embedded_at,x,y,z,colour,laid_out) SELECT track_id,created_at,0,0,0,0.4,true FROM track_embeddings")
        .execute(pool)
        .await
        .unwrap();
}

struct Client {
    srv: TestServer,
    token: String,
}

impl Client {
    async fn new(state: kima_api::AppState) -> Self {
        let (_, token) = common::user_with_token(&state, &uuid::Uuid::new_v4().to_string()).await;
        Self {
            srv: TestServer::new(kima_api::router(state)).unwrap(),
            token,
        }
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let res = self.srv.get(path).authorization_bearer(&self.token).await;
        (res.status_code().as_u16(), res.json())
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let res = self
            .srv
            .post(path)
            .authorization_bearer(&self.token)
            .json(&body)
            .await;
        (res.status_code().as_u16(), res.json())
    }

    async fn search(&self) -> (u16, Value) {
        self.post("/api/vibe/search", json!({"query":"rain"})).await
    }
}

fn synthetic_text(mut state: kima_api::AppState) -> kima_api::AppState {
    state.vibe_text_embedder = Arc::new(SyntheticText);
    state
}

fn disabled(state: kima_api::AppState) -> kima_api::AppState {
    let mut ctx = state.ident_ctx();
    ctx.config.embedding.disabled = true;
    state.with_ident(ctx)
}

fn tracks(body: &Value) -> usize {
    body["tracks"].as_array().unwrap().len()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn features_vibe_embeddings_predicate(pool: PgPool) {
    // No assets, assets without eligible rows, eligible rows without assets and the disabled
    // flag all keep the flag off; only a verified probe plus eligible rows turn it on.
    for (probe, eligible, off, expected) in [
        (false, false, false, false),
        (true, false, false, false),
        (false, true, false, false),
        (true, true, true, false),
        (true, true, false, true),
    ] {
        sqlx::query("DELETE FROM tracks")
            .execute(&pool)
            .await
            .unwrap();
        if eligible {
            mapped(&pool, "eligible", [0.; 3]).await;
        }
        let mut state = with_assets(common::state(pool.clone()), probe, false);
        if off {
            state = disabled(state);
        }
        let client = Client::new(state).await;
        let (status, body) = client.get("/api/system/features").await;
        assert_eq!(status, 200);
        assert_eq!(
            body["vibeEmbeddings"], expected,
            "probe={probe} eligible={eligible} disabled={off}"
        );
    }
}

/// Row 1: `DISABLE_CLAP` / audio model unavailable: no new vectors, existing ones keep working.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn degradation_audio_unavailable_keeps_existing_vectors(pool: PgPool) {
    let rows = matching_library(&pool, 8).await;
    lay_out(&pool).await;
    for off in [false, true] {
        let mut state = synthetic_text(with_assets(common::state(pool.clone()), true, false));
        if off {
            state = disabled(state);
        }
        let ctx = state.ident_ctx();
        assert!(!vibe_assets::matching_audio_available(&ctx));
        let client = Client::new(state).await;
        let (status, body) = client
            .get(&format!("/api/vibe/similar/{}", rows[0].track))
            .await;
        assert_eq!(status, 200, "disabled={off}: {body}");
        assert!(tracks(&body) > 0);
        let (status, body) = client
            .post("/api/vibe/alchemy", json!({"add":[rows[0].track]}))
            .await;
        assert_eq!(status, 200, "{body}");
        let (status, body) = client
            .post(
                "/api/vibe/path",
                json!({"startTrackId":rows[0].track,"endTrackId":rows[7].track}),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        let (status, body) = client.search().await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(tracks(&body), 8);
        let (status, body) = client.get("/api/vibe/map").await;
        assert_eq!(status, 200);
        assert_eq!(body["trackCount"], 8);
        let (_, features) = client.get("/api/system/features").await;
        assert_eq!(features["vibeEmbeddings"], !off);
    }
}

/// Row 2: CLAP text model/tokenizer missing: search 503, everything else works.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn degradation_text_model_missing_only_search_unavailable(pool: PgPool) {
    let rows = matching_library(&pool, 8).await;
    lay_out(&pool).await;
    let client = Client::new(with_assets(common::state(pool), true, true)).await;
    assert_eq!(
        client.search().await,
        (
            503,
            json!({"error":"Text embedding service unavailable","message":"The CLAP text model is not available"})
        )
    );
    let (status, body) = client
        .get(&format!("/api/vibe/similar/{}", rows[0].track))
        .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = client.get("/api/vibe/map").await;
    assert_eq!((status, body["trackCount"].clone()), (200, json!(8)));
    let (status, _) = client.get("/api/vibe/mood-summary").await;
    assert_eq!(status, 200);
    let (status, _) = client
        .get(&format!("/api/vibe/mood/{}", rows[0].track))
        .await;
    assert_eq!(status, 200);
}

/// Row 3: no feel probe: no formula, so similar/alchemy/path/map/state are off; text search
/// needs no probe and keeps working.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn degradation_missing_probe_turns_matching_and_map_off(pool: PgPool) {
    let rows = matching_library(&pool, 8).await;
    lay_out(&pool).await;
    let client = Client::new(synthetic_text(with_assets(
        common::state(pool),
        false,
        true,
    )))
    .await;
    assert_eq!(
        client
            .get(&format!("/api/vibe/similar/{}", rows[0].track))
            .await,
        (404, no_similar())
    );
    assert_eq!(
        client
            .post("/api/vibe/alchemy", json!({"add":[rows[0].track]}))
            .await,
        (
            400,
            json!({"error":"Some tracks have not been through the similarity step yet","missingAdd":[rows[0].track],"missingSubtract":[]})
        )
    );
    assert_eq!(
        client
            .post(
                "/api/vibe/path",
                json!({"startTrackId":rows[0].track,"endTrackId":rows[7].track}),
            )
            .await,
        (
            400,
            json!({"error":"One or both tracks have not been through the similarity step yet"})
        )
    );
    let (status, map) = client.get("/api/vibe/map").await;
    assert_eq!(
        status, 200,
        "a stale layout is neither served nor reported as computing"
    );
    assert_eq!(
        (map["tracks"].clone(), map["trackCount"].clone()),
        (json!([]), json!(0))
    );
    assert_eq!(map["computedAt"], "1970-01-01T00:00:00.000Z");
    assert_eq!(
        client.get("/api/vibe/mood-summary").await,
        (200, json!({"shares":[],"albums":{},"artists":{}}))
    );
    let (status, body) = client.search().await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(tracks(&body), 8);
    let (_, features) = client.get("/api/system/features").await;
    assert_eq!(features["vibeEmbeddings"], false);
}

/// Row 4: mood heads only missing: moods off (`unanalyzed`), similarity unaffected.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn degradation_missing_mood_heads_only_turns_moods_off(pool: PgPool) {
    let rows = matching_library(&pool, 8).await;
    lay_out(&pool).await;
    let client = Client::new(with_assets(common::state(pool), true, false)).await;
    assert_eq!(
        client
            .get(&format!("/api/vibe/mood/{}", rows[0].track))
            .await,
        (200, json!({"dominantMood":"unanalyzed"}))
    );
    assert_eq!(
        client.get("/api/vibe/mood-summary").await,
        (200, json!({"shares":[],"albums":{},"artists":{}}))
    );
    let (status, map) = client.get("/api/vibe/map").await;
    assert_eq!(status, 200);
    assert_eq!(map["trackCount"], 8);
    assert!(
        map["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["dominantMood"] == "unanalyzed")
    );
    let (status, body) = client
        .get(&format!("/api/vibe/similar/{}", rows[0].track))
        .await;
    assert_eq!(status, 200, "{body}");
    let (_, features) = client.get("/api/system/features").await;
    assert_eq!(features["vibeEmbeddings"], true);
}

/// §4.3 / D-194: a library embedded only by M4's general checkpoint (`hf_audio`, its pinned
/// bytes `37df17af…`, pipeline `clap-audio-v1`) has zero eligible tracks after the music swap;
/// assets present do not change that, and the old rows are never relabelled.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn degradation_hf_audio_library_has_no_vibe_engine(pool: PgPool) {
    const M4_GENERAL_SHA256: &str =
        "37df17afb9881569c4f50e5b691167aa54586418059701a74a54934514969972";
    let m4_sha: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&M4_GENERAL_SHA256[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    let s = seed(&pool, Seed::new("hf", "hf", None, "hf")).await;
    let mut v = vec![0.0f32; 512];
    v[0] = 1.0;
    sqlx::query("INSERT INTO track_embeddings (track_id,file_generation,model_id,model_sha256,pipeline_version,embedding) VALUES ($1,0,$2,$3,$4,$5::real[]::vector)")
        .bind(s.track)
        .bind("hf_audio")
        .bind(&m4_sha)
        .bind("clap-audio-v1")
        .bind(v)
        .execute(&pool)
        .await
        .unwrap();
    lay_out(&pool).await;
    let client = Client::new(synthetic_text(with_assets(
        common::state(pool.clone()),
        true,
        true,
    )))
    .await;
    assert_eq!(
        client.get(&format!("/api/vibe/similar/{}", s.track)).await,
        (404, no_similar())
    );
    assert_eq!(
        client.search().await,
        (
            503,
            json!({"error":"Vibe search needs the music CLAP model"})
        )
    );
    let (status, map) = client.get("/api/vibe/map").await;
    assert_eq!((status, map["trackCount"].clone()), (200, json!(0)));
    let (_, features) = client.get("/api/system/features").await;
    assert_eq!(features["vibeEmbeddings"], false);
    let (model_id, sha, pipeline): (String, Vec<u8>, String) = sqlx::query_as(
        "SELECT model_id, model_sha256, pipeline_version FROM track_embeddings WHERE track_id = $1",
    )
    .bind(s.track)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (model_id.as_str(), sha, pipeline.as_str()),
        ("hf_audio", m4_sha, "clap-audio-v1"),
        "the general-model row stays as written"
    );
}

/// Missing or unprovisioned vibe assets never stop the server: the `vibe_assets::initialize`
/// call `kima-server`'s `main` makes (run in-process here, not the binary) succeeds, reports the
/// vibe capabilities unavailable and the API answers.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn server_boots_without_vibe_assets(pool: PgPool) {
    let empty = tempfile::tempdir().unwrap();
    for dir in [None, Some(empty.path().to_path_buf())] {
        let state = common::state(pool.clone());
        let mut ctx = state.ident_ctx();
        ctx.config.vibe.asset_dir = dir.clone();
        let assets = vibe_assets::initialize(&ctx).await.unwrap();
        assert!(assets.probe.is_none() && assets.heads.is_none(), "{dir:?}");
        for capability in [
            kima_ml::Capability::VibeAssets,
            kima_ml::Capability::VibeMoodHeads,
        ] {
            assert!(
                matches!(
                    assets.report.capabilities.get(&capability),
                    Some(kima_ml::CapabilityStatus::Unavailable { .. })
                ),
                "{dir:?} {capability:?}"
            );
        }
        let client = Client::new(state.with_ident(ctx)).await;
        let res = client.srv.get("/api/health").await;
        res.assert_status_ok();
        let (status, features) = client.get("/api/system/features").await;
        assert_eq!(
            (status, features["vibeEmbeddings"].clone()),
            (200, json!(false))
        );
        let (status, status_body) = client.get("/api/enrichment/status").await;
        assert_eq!(status, 200);
        let caps = &status_body["mlCapabilities"]["capabilities"];
        assert_eq!(caps["vibe_assets"]["state"], "unavailable", "{status_body}");
        assert_eq!(caps["vibe_mood_heads"]["state"], "unavailable");
    }
}

/// Missing vibe assets turn off only the dependent vibe capability.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn other_capabilities_unaffected_by_missing_assets(pool: PgPool) {
    let row = mapped(&pool, "Independent", [0.0; 3]).await;
    let client = Client::new(synthetic_text(common::state(pool.clone()))).await;
    let (status, features) = client.get("/api/system/features").await;
    assert_eq!(status, 200);
    let expected = common::state(pool).fpcalc;
    assert_eq!(
        features,
        json!({
            "musicCNN": false,
            "vibeEmbeddings": false,
            "audiobookshelfEnabled": false,
            "fpcalc": expected,
            "library": true,
            "podcasts": true,
            "audiobooks": false,
            "discover": false,
            "mixes": false,
            "downloads": false,
            "imports": false,
            "stats": true,
        })
    );
    let (status, library) = client.get("/api/library/tracks").await;
    assert_eq!(status, 200, "{library}");
    assert!(library.to_string().contains(&row.track.to_string()));
    let (status, _) = client.get("/api/enrichment/progress").await;
    assert_eq!(status, 200);
    // Text search ranks stored music vectors and needs neither probe nor heads.
    let (status, body) = client.search().await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(tracks(&body), 1);
    assert_eq!(
        client.get(&format!("/api/vibe/mood/{}", row.track)).await,
        (200, json!({"dominantMood":"unanalyzed"}))
    );
}
