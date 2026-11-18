//! Similar-artist reranking: synthetic audio profiles, real PostgreSQL, no private assets.
use kima_db::PgPool;
use kima_db::vibe::{MATCHING_SOUND, state::build_matching};
use kima_ident::Mbid;
use kima_ident::similar::{Entry, rerank};
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use uuid::Uuid;

fn probe() -> FeelProbe {
    let axis = || ProbeAxis {
        alpha: 1.0,
        bias: 0.5,
        weights: vec![0.0; 512],
    };
    FeelProbe {
        model: "synthetic".into(),
        valence: axis(),
        arousal: axis(),
        probe_id: "rerank-test".into(),
    }
}
fn entry(name: &str, score: f64) -> Entry {
    Entry {
        name: name.into(),
        mbid: None,
        r#match: score,
    }
}
async fn artist(pool: &PgPool, name: &str, mbid: Option<Uuid>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO artists(id, name, name_key, mbid) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(name)
    .bind(name.to_lowercase())
    .bind(mbid)
    .fetch_one(pool)
    .await
    .unwrap()
}
async fn embedded(pool: &PgPool, artist: Uuid, direction: f32) {
    let album: Uuid = sqlx::query_scalar(
        "INSERT INTO albums(id, artist_id, title, title_key) VALUES ($1, $2, $3, $3) RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(artist)
    .bind(Uuid::new_v4().to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    let track: Uuid = sqlx::query_scalar("INSERT INTO tracks(id, album_id, artist_name, title, disc, track_no, path, codec, duration_secs, size, mtime, fingerprint) VALUES ($1, $2, 'artist', 't', 1, 1, $3, 'mp3', 30, 1, now(), $4) RETURNING id")
        .bind(Uuid::now_v7()).bind(album).bind(format!("{}.mp3", Uuid::new_v4())).bind(vec![1_u8;32]).fetch_one(pool).await.unwrap();
    let mut vector = vec![0_f32; 512];
    if direction == 0.0 {
        vector[1] = 1.0;
    } else {
        vector[0] = direction;
    }
    sqlx::query("INSERT INTO track_embeddings(track_id, file_generation, model_id, model_sha256, pipeline_version, embedding) VALUES ($1, 0, $2, $3, $4, $5::real[]::vector)")
        .bind(track).bind(MATCHING_SOUND.model_id).bind(MATCHING_SOUND.model_sha256.to_vec()).bind(MATCHING_SOUND.pipeline_version).bind(vector).execute(pool).await.unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delta_is_plus_0_1_at_closeness_1_and_minus_0_1_at_closeness_0_or_less(pool: PgPool) {
    let seed = artist(&pool, "seed", None).await;
    let near = artist(&pool, "near", None).await;
    let far = artist(&pool, "far", None).await;
    embedded(&pool, seed, 1.0).await;
    embedded(&pool, near, 1.0).await;
    embedded(&pool, far, -1.0).await;
    build_matching(&pool, &probe()).await.unwrap();
    let results = vec![entry("far", 0.7), entry("near", 0.7)];
    let got = rerank(&pool, Some(&probe()), None, "seed", &results).await;
    assert_eq!(got[0].entry.name, "near");
    assert!((got[0].entry.r#match - 0.8).abs() < 1e-12);
    assert!((got[1].entry.r#match - 0.6).abs() < 1e-12);
    assert!(got.iter().all(|r| r.in_library));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_state_means_no_adjustment(pool: PgPool) {
    artist(&pool, "owned", None).await;
    let input = vec![entry("owned", 0.8), entry("unknown", 0.7)];
    for probe in [None, Some(probe())] {
        let got = rerank(&pool, probe.as_ref(), None, "seed", &input).await;
        assert_eq!(
            got.iter().map(|r| r.entry.clone()).collect::<Vec<_>>(),
            input
        );
        assert!(got[0].in_library);
        assert!(!got[1].in_library);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn match_clamped_to_0_1(pool: PgPool) {
    let seed = artist(&pool, "seed", None).await;
    let far = artist(&pool, "far", None).await;
    let orthogonal = artist(&pool, "orthogonal", None).await;
    embedded(&pool, seed, 1.0).await;
    embedded(&pool, far, -1.0).await;
    embedded(&pool, orthogonal, 0.0).await;
    build_matching(&pool, &probe()).await.unwrap();
    let out = rerank(
        &pool,
        Some(&probe()),
        None,
        "seed",
        &[
            entry("seed", 0.95),
            entry("far", 0.05),
            entry("orthogonal", 0.5),
        ],
    )
    .await;
    assert_eq!(
        out.iter()
            .map(|r| (r.entry.name.as_str(), r.entry.r#match))
            .collect::<Vec<_>>(),
        [("seed", 1.0), ("orthogonal", 0.4), ("far", 0.0)]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unprofiled_artist_not_adjusted_and_ties_keep_source_order(pool: PgPool) {
    let seed = artist(&pool, "seed", None).await;
    embedded(&pool, seed, 1.0).await;
    artist(&pool, "unprofiled", None).await;
    build_matching(&pool, &probe()).await.unwrap();
    let input = vec![entry("z", 0.9), entry("unprofiled", 0.9), entry("a", 0.9)];
    let out = rerank(&pool, Some(&probe()), None, "seed", &input).await;
    assert_eq!(
        out.iter().map(|r| r.entry.clone()).collect::<Vec<_>>(),
        input
    );
    assert_eq!(
        out.iter().map(|r| r.in_library).collect::<Vec<_>>(),
        [false, true, false]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_match_null_when_no_spread(pool: PgPool) {
    let seed = artist(&pool, "seed", None).await;
    let zero = artist(&pool, "zero", None).await;
    embedded(&pool, seed, 1.0).await;
    embedded(&pool, zero, 1.0).await;
    embedded(&pool, zero, -1.0).await;
    build_matching(&pool, &probe()).await.unwrap();
    let state = kima_db::vibe::rank::load_matching_state(&pool, &probe())
        .await
        .unwrap()
        .unwrap();
    let profiles = kima_db::vibe::rank::artist_profiles(&pool, &probe(), &state, &[seed, zero])
        .await
        .unwrap();
    assert_eq!(
        kima_vibe::formula::artist_match(
            &profiles[&seed],
            &profiles[&zero],
            &state.genres,
            state.genre_weight_sq()
        ),
        None
    );
    let out = rerank(&pool, Some(&probe()), None, "seed", &[entry("zero", 0.7)]).await;
    assert_eq!(out[0].entry.r#match, 0.7);
    assert!(out[0].in_library);
    // A zero-spread seed is equally unadjustable.
    let out = rerank(&pool, Some(&probe()), None, "zero", &[entry("seed", 0.7)]).await;
    assert_eq!(out[0].entry.r#match, 0.7);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn seed_mbid_else_name_results_mbid_then_name(pool: PgPool) {
    let mbid = Uuid::new_v4();
    let seed = artist(&pool, "real seed", Some(mbid)).await;
    let wrong = artist(&pool, "wrong seed", None).await;
    let result_mbid = Uuid::new_v4();
    let near = artist(&pool, "near", Some(result_mbid)).await;
    let name_collision = artist(&pool, "collision", None).await;
    embedded(&pool, seed, 1.0).await;
    embedded(&pool, near, 1.0).await;
    embedded(&pool, wrong, -1.0).await;
    embedded(&pool, name_collision, -1.0).await;
    build_matching(&pool, &probe()).await.unwrap();
    let input = [
        Entry {
            name: "collision".into(),
            mbid: Some(Mbid(result_mbid)),
            r#match: 0.7,
        },
        Entry {
            name: "near".into(),
            mbid: Some(Mbid(Uuid::new_v4())),
            r#match: 0.7,
        },
    ];
    let out = rerank(
        &pool,
        Some(&probe()),
        Some(Mbid(mbid)),
        "wrong seed",
        &input,
    )
    .await;
    assert!(
        out.iter()
            .all(|r| r.in_library && (r.entry.r#match - 0.8).abs() < 1e-12)
    );
    // Unknown MBID must NOT fall back to the otherwise matching seed name.
    let out = rerank(
        &pool,
        Some(&probe()),
        Some(Mbid(Uuid::new_v4())),
        "real seed",
        &input,
    )
    .await;
    assert_eq!(
        out.iter().map(|r| r.entry.clone()).collect::<Vec<_>>(),
        input
    );
    let out = rerank(&pool, Some(&probe()), None, "REAL SEED", &input).await;
    assert_eq!(
        out.iter().map(|r| r.entry.clone()).collect::<Vec<_>>(),
        input
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failure_returns_original_list_with_in_library_false(pool: PgPool) {
    artist(&pool, "owned", None).await;
    // Failure after membership lookup, not only an initial connection failure.
    sqlx::query("ALTER TABLE matching_state RENAME TO matching_state_unavailable")
        .execute(&pool)
        .await
        .unwrap();
    let input = vec![entry("owned", 0.5), entry("unknown", 0.9)];
    let out = rerank(&pool, Some(&probe()), None, "seed", &input).await;
    assert_eq!(
        out.iter().map(|r| r.entry.clone()).collect::<Vec<_>>(),
        input
    );
    assert!(out.iter().all(|r| !r.in_library));
    pool.close().await;
    let out = rerank(&pool, None, None, "seed", &input).await;
    assert_eq!(
        out.iter().map(|r| r.entry.clone()).collect::<Vec<_>>(),
        input
    );
    assert!(out.iter().all(|r| !r.in_library));
    assert!(rerank(&pool, None, None, "seed", &[]).await.is_empty());
}
