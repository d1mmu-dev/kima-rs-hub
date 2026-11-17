//! Task 15 real-router contracts, included from vibe.rs to reuse synthetic assets.
use super::*;
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn matching_database_errors_keep_upstream_500_bodies(pool: PgPool) {
    let rows = matching_library(&pool, 3).await;
    let state = with_assets(common::state(pool.clone()), true, false);
    let (_, token) = common::user_with_token(&state, "errors").await;
    sqlx::query("DROP TABLE matching_state")
        .execute(&pool)
        .await
        .unwrap();
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let a = srv
        .get(&format!("/api/vibe/similar/{}", rows[0].track))
        .authorization_bearer(&token)
        .await;
    let b = srv
        .post("/api/vibe/alchemy")
        .authorization_bearer(&token)
        .json(&json!({"add":[rows[0].track]}))
        .await;
    let c = srv
        .post("/api/vibe/path")
        .authorization_bearer(&token)
        .json(&json!({"startTrackId":rows[0].track,"endTrackId":rows[2].track}))
        .await;
    for (res, message) in [
        (a, "Failed to find similar tracks"),
        (b, "Failed to compute alchemy blend"),
        (c, "Failed to generate song path"),
    ] {
        res.assert_status_internal_server_error();
        assert_eq!(res.json::<Value>(), json!({"error":message}));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn matching_reads_are_library_global_for_all_profile_kinds(pool: PgPool) {
    use kima_core::profile::ProfileKind;
    use kima_db::profiles::{self, NewProfile};
    let rows = matching_library(&pool, 8).await;
    let state = with_assets(common::state(pool.clone()), true, false);
    let (owner, main) = common::user_with_token(&state, "match-owner").await;
    let (other, other_main) = common::user_with_token(&state, "match-other").await;
    let mut tokens = vec![main, other_main];
    for (user, kind) in [
        (&owner, ProfileKind::Kids),
        (&owner, ProfileKind::Standard),
        (&other, ProfileKind::Kids),
        (&other, ProfileKind::Standard),
    ] {
        let p = profiles::create(
            &pool,
            user.account(),
            NewProfile {
                name: format!("Private {kind:?}"),
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
    let mut expected: Vec<Value> = Vec::new();
    for token in &tokens {
        let a = srv
            .get(&format!("/api/vibe/similar/{}", rows[0].track))
            .authorization_bearer(token)
            .await;
        let b = srv
            .post("/api/vibe/alchemy")
            .authorization_bearer(token)
            .json(&json!({"add":[rows[0].track]}))
            .await;
        let c = srv
            .post("/api/vibe/path")
            .authorization_bearer(token)
            .json(&json!({"startTrackId":rows[0].track,"endTrackId":rows[7].track}))
            .await;
        a.assert_status_ok();
        b.assert_status_ok();
        c.assert_status_ok();
        let data = vec![a.json::<Value>(), b.json::<Value>(), c.json::<Value>()];
        assert!(!format!("{data:?}").contains("Private"));
        if expected.is_empty() {
            expected = data;
        } else {
            assert_eq!(data, expected);
        }
    }
    let features = srv
        .get("/api/system/features")
        .authorization_bearer(&tokens[0])
        .await
        .json::<Value>();
    assert_eq!(features["vibeEmbeddings"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unembedded_seed_with_engine_on_and_disabled_engine_with_embedded_seed(pool: PgPool) {
    let rows = matching_library(&pool, 3).await;
    let plain = seed(&pool, Seed::new("plain", "plain", None, "plain")).await;
    for (enabled, id) in [(true, plain.track), (false, rows[0].track)] {
        let state = with_assets(common::state(pool.clone()), enabled, false);
        let (_, token) = common::user_with_token(&state, if enabled { "on" } else { "off" }).await;
        let srv = TestServer::new(kima_api::router(state)).unwrap();
        let res = srv
            .get(&format!("/api/vibe/similar/{id}"))
            .authorization_bearer(&token)
            .await;
        res.assert_status_not_found();
        assert_eq!(
            res.json::<Value>(),
            json!({"error":"No similar tracks found","message":"This track may not have been analyzed yet, or no analyzer is running"})
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn add_takes_cap_places_but_subtract_does_not(pool: PgPool) {
    let rows = matching_library(&pool, 10).await;
    // Seed album has two candidates; subtract album also has two candidates.
    for (track, album) in [
        (rows[1].track, rows[0].album),
        (rows[2].track, rows[0].album),
        (rows[7].track, rows[9].album),
        (rows[8].track, rows[9].album),
    ] {
        sqlx::query("UPDATE tracks SET album_id=$2 WHERE id=$1")
            .bind(track)
            .bind(album)
            .execute(&pool)
            .await
            .unwrap();
    }
    kima_db::vibe::state::build_matching(&pool, &probe())
        .await
        .unwrap();
    let state = with_assets(common::state(pool), true, false);
    let (_, token) = common::user_with_token(&state, "caps").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let res = srv
        .post("/api/vibe/alchemy")
        .authorization_bearer(&token)
        .json(&json!({"add":[rows[0].track],"subtract":[rows[9].track],"limit":100}))
        .await;
    res.assert_status_ok();
    let data: Value = res.json();
    let tracks = data["tracks"].as_array().unwrap();
    assert_eq!(
        tracks
            .iter()
            .filter(|t| t["album"]["id"] == rows[0].album.to_string())
            .count(),
        1
    );
    assert_eq!(
        tracks
            .iter()
            .filter(|t| t["album"]["id"] == rows[9].album.to_string())
            .count(),
        2
    );
    assert!(
        !tracks
            .iter()
            .any(|t| t["id"] == rows[0].track.to_string() || t["id"] == rows[9].track.to_string())
    );
}

async fn matching_library(pool: &PgPool, count: usize) -> Vec<Seeded> {
    let mut rows = Vec::new();
    for n in 0..count {
        let name = format!("Matching {n}");
        let row = seed(pool, Seed::new(&name, &name, None, &name)).await;
        let angle = n as f32 / count as f32 * 2.0;
        let mut v = vec![0.0f32; 512];
        v[0] = angle.cos();
        v[1] = angle.sin();
        sqlx::query("INSERT INTO track_embeddings (track_id,file_generation,model_id,model_sha256,pipeline_version,embedding) VALUES ($1,0,$2,$3,$4,$5::real[]::vector)")
            .bind(row.track).bind(MATCHING_SOUND.model_id).bind(MATCHING_SOUND.model_sha256.to_vec()).bind(MATCHING_SOUND.pipeline_version).bind(v)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("UPDATE tracks SET display_title='Edited', duration_secs=123 WHERE id=$1")
            .bind(row.track)
            .execute(pool)
            .await
            .unwrap();
        rows.push(row);
    }
    kima_db::vibe::state::build_matching(pool, &probe())
        .await
        .unwrap();
    rows
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn similar_alchemy_limits_scores_titles_caps_and_cancellation(pool: PgPool) {
    use kima_db::vibe::rank::{WeightedInput, check_blend, matching_context, rank_matches};
    use kima_vibe::formula::SELF_SCORE;
    let rows = matching_library(&pool, 106).await;
    for row in &rows[1..3] {
        sqlx::query("UPDATE tracks SET album_id=$2 WHERE id=$1")
            .bind(row.track)
            .bind(rows[0].album)
            .execute(&pool)
            .await
            .unwrap();
    }
    kima_db::vibe::state::build_matching(&pool, &probe())
        .await
        .unwrap();
    let state = with_assets(common::state(pool.clone()), true, false);
    let (_, token) = common::user_with_token(&state, "matcher").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let a = rows[0].track;
    let b = rows[105].track;
    let ctx = matching_context(&pool, &probe(), &[a, b])
        .await
        .unwrap()
        .unwrap();
    for (limit, count) in [
        (json!(null), 20),
        (json!(200), 100),
        (json!("3junk"), 3),
        (json!(0), 20),
        (json!(-9), 1),
        (json!("\u{feff}3"), 3),
        (json!("\u{0085}3"), 20),
    ] {
        let res = srv
            .post("/api/vibe/alchemy")
            .authorization_bearer(&token)
            .json(&json!({"add":[a],"limit":limit}))
            .await;
        res.assert_status_ok();
        let data: Value = res.json();
        assert_eq!(data["tracks"].as_array().unwrap().len(), count);
        let query = match &limit {
            Value::String(s) => s.clone(),
            _ => limit.to_string(),
        };
        let res = srv
            .get(&format!("/api/vibe/similar/{a}"))
            .add_query_param("limit", query)
            .authorization_bearer(&token)
            .await;
        res.assert_status_ok();
        let similar: Value = res.json();
        let dto: kima_api_types::vibe::SimilarResponse =
            serde_json::from_value(similar.clone()).unwrap();
        assert_eq!(serde_json::to_value(dto).unwrap(), similar);
        let dto: kima_api_types::vibe::AlchemyResponse =
            serde_json::from_value(data.clone()).unwrap();
        assert_eq!(serde_json::to_value(dto).unwrap(), data);
        assert_eq!(similar["sourceTrackId"], a.to_string());
        let single = [WeightedInput {
            track_id: a,
            weight: 1.0,
        }];
        let single_scale = (check_blend(&ctx, &single).unwrap() * SELF_SCORE).sqrt();
        let similar_rows = similar["tracks"].as_array().unwrap();
        assert_eq!(
            similar_rows.iter().map(|t| &t["id"]).collect::<Vec<_>>(),
            data["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| &t["id"])
                .collect::<Vec<_>>()
        );
        let actual = data["tracks"].as_array().unwrap();
        assert!(!actual.iter().any(|t| t["id"] == a.to_string()));
        assert!(
            actual
                .iter()
                .filter(|t| t["album"]["id"] == rows[0].album.to_string())
                .count()
                <= 1
        );
        let expected = rank_matches(
            &pool,
            &ctx,
            &[WeightedInput {
                track_id: a,
                weight: 1.0,
            }],
            count as i64,
            &[a],
        )
        .await
        .unwrap();
        for ((t, s), e) in actual.iter().zip(similar_rows).zip(expected) {
            assert!((s["distance"].as_f64().unwrap() - (1.0 - e.score / SELF_SCORE)).abs() < 1e-10);
            assert_eq!(t["id"], e.track.id.to_string());
            assert_eq!(t["displayTitle"], "Edited");
            assert_eq!(t["duration"], 123);
            assert!(t.get("trackNo").is_none());
            assert!(
                (t["distance"].as_f64().unwrap() - (1.0 - e.score / single_scale)).abs() < 1e-10
            );
            assert!(
                (t["similarity"].as_f64().unwrap() - (e.score / single_scale).clamp(0.0, 1.0))
                    .abs()
                    < 1e-10
            );
        }
    }
    let inputs = [
        WeightedInput {
            track_id: a,
            weight: 1.0,
        },
        WeightedInput {
            track_id: b,
            weight: -1.0,
        },
    ];
    let scale = (check_blend(&ctx, &inputs).unwrap() * SELF_SCORE).sqrt();
    let expected = rank_matches(&pool, &ctx, &inputs, 100, &[a]).await.unwrap();
    let res = srv
        .post("/api/vibe/alchemy")
        .authorization_bearer(&token)
        .json(&json!({"add":[a],"subtract":[b],"limit":100}))
        .await;
    res.assert_status_ok();
    let data: Value = res.json();
    assert_eq!(data["tracks"].as_array().unwrap().len(), expected.len());
    for (t, e) in data["tracks"].as_array().unwrap().iter().zip(&expected) {
        assert_eq!(t["id"], e.track.id.to_string());
        assert!((t["distance"].as_f64().unwrap() - (1.0 - e.score / scale)).abs() < 1e-10);
    }
    let res = srv
        .post("/api/vibe/alchemy")
        .authorization_bearer(&token)
        .json(&json!({"add":[a],"subtract":[a]}))
        .await;
    res.assert_status_bad_request();
    assert_eq!(
        res.json::<Value>(),
        json!({"error":"The tracks to add and to subtract cancel each other out"})
    );
    let missing = Uuid::now_v7();
    let res = srv
        .post("/api/vibe/alchemy")
        .authorization_bearer(&token)
        .json(&json!({"add":[a,missing],"subtract":[b,missing]}))
        .await;
    res.assert_status_bad_request();
    assert_eq!(
        res.json::<Value>(),
        json!({"error":"Some tracks have not been through the similarity step yet","missingAdd":[missing],"missingSubtract":[missing]})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn path_length_mode_flat_shape_and_too_similar(pool: PgPool) {
    let rows = matching_library(&pool, 62).await;
    let state = with_assets(common::state(pool), true, false);
    let (_, token) = common::user_with_token(&state, "journey").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let a = rows[0].track;
    let b = rows[61].track;
    for (length, count) in [
        (json!(1), Some(5)),
        (json!(500), Some(50)),
        (json!("5"), Some(5)),
        (json!("0x10"), Some(16)),
        (json!("0Xf"), Some(15)),
        (json!("0b1000"), Some(8)),
        (json!("0o12"), Some(10)),
        (json!("\u{feff}+1.2e1\u{2029}"), Some(12)),
        (json!("Infinity"), Some(50)),
        (json!("-Infinity"), Some(5)),
        (json!("-0"), Some(5)),
        (json!(""), None),
        (json!("+0x10"), None),
        (json!("-0b10"), None),
        (json!("inf"), None),
        (json!("NaN"), None),
        (json!("0"), Some(5)),
        (json!("\u{feff}  "), Some(5)),
        (json!(0), None),
        (json!(false), None),
        (json!(true), Some(5)),
        (json!("nonsense"), None),
        (json!(null), None),
    ] {
        for mode in [json!("discovery"), json!("Discovery"), json!(null)] {
            let res = srv
                .post("/api/vibe/path")
                .authorization_bearer(&token)
                .json(&json!({"startTrackId":a,"endTrackId":b,"length":length,"mode":mode}))
                .await;
            res.assert_status_ok();
            let data: Value = res.json();
            let dto: kima_api_types::vibe::PathResponse =
                serde_json::from_value(data.clone()).unwrap();
            assert_eq!(serde_json::to_value(dto).unwrap(), data);
            assert_eq!(
                data["metadata"]["mode"],
                if mode == "discovery" {
                    "discovery"
                } else {
                    "smooth"
                }
            );
            let expected = kima_vibe::song_path::waypoint_count(
                data["metadata"]["embeddingDistance"].as_f64().unwrap(),
                count,
            );
            assert_eq!(data["path"].as_array().unwrap().len(), expected);
            assert_eq!(data["metadata"]["totalTracks"], expected + 2);
            assert_eq!(data["startTrack"]["id"], a.to_string());
            assert_eq!(data["endTrack"]["id"], b.to_string());
            for t in std::iter::once(&data["startTrack"])
                .chain(data["path"].as_array().unwrap())
                .chain(std::iter::once(&data["endTrack"]))
            {
                assert_eq!(t["displayTitle"], "Edited");
                assert_eq!(t["duration"], 123);
                assert!(t.get("albumId").is_some());
                assert!(t.get("artistName").is_some());
                assert!(t.get("album").is_none());
            }
        }
    }
    let res = srv
        .post("/api/vibe/path")
        .authorization_bearer(&token)
        .json(&json!({"startTrackId":a,"endTrackId":rows[1].track}))
        .await;
    res.assert_status_bad_request();
    assert_eq!(
        res.json::<Value>(),
        json!({"error":"These tracks are too similar for a meaningful journey. Try \"Similar Tracks\" instead."})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn validation_order_unembedded_seed_and_engine_off(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "route-validation").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let id = Uuid::now_v7().to_string();
    for body in [
        json!({}),
        json!({"startTrackId":3,"endTrackId":3}),
        json!({"startTrackId":"","endTrackId":id}),
    ] {
        let res = srv
            .post("/api/vibe/path")
            .authorization_bearer(&token)
            .json(&body)
            .await;
        res.assert_status_bad_request();
        assert_eq!(
            res.json::<Value>(),
            json!({"error":"startTrackId and endTrackId are required strings"})
        );
    }
    for (body, error) in [
        (
            json!({"startTrackId":"bad","endTrackId":"bad"}),
            "Invalid track ID format",
        ),
        (
            json!({"startTrackId":id,"endTrackId":id}),
            "Start and end tracks must be different",
        ),
        (
            json!({"startTrackId":id,"endTrackId":Uuid::now_v7()}),
            "One or both tracks have not been through the similarity step yet",
        ),
    ] {
        let res = srv
            .post("/api/vibe/path")
            .authorization_bearer(&token)
            .json(&body)
            .await;
        res.assert_status_bad_request();
        assert_eq!(res.json::<Value>(), json!({"error":error}));
    }
    for (body, error) in [
        (json!({}), "At least one track ID in 'add' is required"),
        (
            json!({"add":[],"subtract":"bad"}),
            "At least one track ID in 'add' is required",
        ),
        (
            json!({"add":vec!["bad";11],"subtract":"bad"}),
            "'add' may contain at most 10 tracks",
        ),
        (
            json!({"add":["bad"],"subtract":"bad"}),
            "'subtract' must be an array of at most 10 tracks",
        ),
        (
            json!({"add":["bad"],"subtract":["bad"]}),
            "'add' must contain valid track IDs",
        ),
        (
            json!({"add":[id],"subtract":["bad"]}),
            "'subtract' must contain valid track IDs",
        ),
        (
            json!({"add":[id],"subtract":vec![id.clone();11]}),
            "'subtract' must be an array of at most 10 tracks",
        ),
    ] {
        let res = srv
            .post("/api/vibe/alchemy")
            .authorization_bearer(&token)
            .json(&body)
            .await;
        res.assert_status_bad_request();
        assert_eq!(res.json::<Value>(), json!({"error":error}));
    }
    let other = Uuid::now_v7();
    let res = srv
        .post("/api/vibe/alchemy")
        .authorization_bearer(&token)
        .json(&json!({"add":[id],"subtract":[other]}))
        .await;
    res.assert_status_bad_request();
    assert_eq!(
        res.json::<Value>(),
        json!({"error":"Some tracks have not been through the similarity step yet","missingAdd":[id],"missingSubtract":[other]})
    );
    for seed in [&id, "not-a-uuid"] {
        let res = srv
            .get(&format!("/api/vibe/similar/{seed}"))
            .authorization_bearer(&token)
            .await;
        res.assert_status_not_found();
        assert_eq!(
            res.json::<Value>(),
            json!({"error":"No similar tracks found","message":"This track may not have been analyzed yet, or no analyzer is running"})
        );
    }
    srv.post("/api/vibe/alchemy")
        .json(&json!({}))
        .await
        .assert_status_unauthorized();
    srv.post("/api/vibe/path")
        .json(&json!({}))
        .await
        .assert_status_unauthorized();
    srv.get(&format!("/api/vibe/similar/{id}"))
        .await
        .assert_status_unauthorized();
}
