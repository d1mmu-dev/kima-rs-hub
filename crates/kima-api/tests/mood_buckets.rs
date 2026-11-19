//! `/api/mixes/mood/buckets/*` (M5b Task 10; port of upstream `routes/__tests__/mixesMoodServe.route.test.ts`,
//! `mixesValidation.test.ts`, `services/__tests__/moodBucketService.test.ts` and
//! `moodBucketMixPool.integration.test.ts` at the pin `c7527577`): the five mood presets, a mood page's mix drawn from
//! its recipe, skipping what the profile was already served, and the saved mood mix. A page mix needs the mood heads
//! (installed here as test heads with stored mood statistics); without them (the live state) every page answers
//! "not enough tracks".

mod common;
#[path = "../../kima-db/tests/mixes_common/mod.rs"]
mod mixes_common;

use std::collections::BTreeSet;

use axum::http::StatusCode;
use axum_test::{TestResponse, TestServer};
use common::mix_keys::assert_rows_shape;
use kima_core::profile::{ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile};
use kima_ident::stages::vibe_assets::{self, VibeAssets};
use mixes_common::{Library, Scores, background, library, member, song};
use serde_json::{Value, json};
use uuid::Uuid;

const VALID_MOODS: [&str; 5] = ["happy", "sad", "chill", "aggressive", "party"];

struct Env {
    server: TestServer,
    state: kima_api::AppState,
    ana: String,
    ana_profile: ProfileId,
    kid: String,
    kid_profile: ProfileId,
    lib: Library,
}

/// The mood heads the stored statistics were written for (`mixes_common::heads`), installed as the engine's.
fn with_heads(state: kima_api::AppState) -> kima_api::AppState {
    let ctx = state.ident_ctx();
    vibe_assets::install(
        &ctx,
        VibeAssets {
            probe: None,
            heads: Some(std::sync::Arc::new(mixes_common::heads())),
            report: kima_ml::CapabilityEvaluationReport {
                capabilities: Default::default(),
            },
        },
    );
    state.with_ident(ctx)
}

/// `calm` members m0.. (each its own artist) among 100 background songs, plus one background song whose vector has no
/// mood scores of these heads yet (pending).
async fn env(pool: PgPool, heads: bool, calm: usize) -> Env {
    let mut songs = background(100);
    songs.extend((0..calm).map(|i| member(&format!("m{i}"))));
    songs.push(song("waiting").artist("bg").scores(Scores::OtherWeights));
    let lib = library(&pool, &songs, &[]).await;
    let state = common::state(pool.clone());
    let state = if heads { with_heads(state) } else { state };
    let (ana_user, ana) = common::user_with_token(&state, "ana").await;
    let kid_profile = profiles::create(
        &pool,
        ana_user.account(),
        NewProfile {
            name: "Kid".into(),
            kind: ProfileKind::Kids,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap()
    .id;
    let kid = state.jwt.issue_access(
        ana_user.account(),
        kid_profile,
        &ana_user.username,
        &ana_user.role,
        ana_user.token_version,
    );
    Env {
        server: TestServer::new(kima_api::router(state.clone())).unwrap(),
        state,
        ana,
        ana_profile: ana_user.main_profile(),
        kid,
        kid_profile,
        lib,
    }
}

impl Env {
    fn m(&self, i: usize) -> Uuid {
        self.lib.tracks[&format!("m{i}")]
    }

    fn members(&self) -> BTreeSet<Uuid> {
        self.lib
            .tracks
            .iter()
            .filter(|(n, _)| n.starts_with('m'))
            .map(|(_, id)| *id)
            .collect()
    }

    async fn bucket(&self, token: &str, mood: &str) -> TestResponse {
        self.server
            .get(&format!("/api/mixes/mood/buckets/{mood}"))
            .authorization_bearer(token)
            .await
    }

    async fn save(&self, token: &str, mood: &str, body: Option<Value>) -> TestResponse {
        let req = self
            .server
            .post(&format!("/api/mixes/mood/buckets/{mood}/save"))
            .authorization_bearer(token);
        match body {
            Some(b) => req.json(&b).await,
            None => req.await,
        }
    }

    async fn presets(&self, token: &str) -> Vec<Value> {
        let res = self
            .server
            .get("/api/mixes/mood/buckets/presets")
            .authorization_bearer(token)
            .await;
        res.assert_status_ok();
        res.json::<Vec<Value>>()
    }
}

fn ids_of(v: &Value) -> Vec<Uuid> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().parse().unwrap())
        .collect()
}

async fn saved_row(pool: &PgPool, profile: ProfileId) -> Option<(String, Vec<Uuid>, Vec<Uuid>)> {
    sqlx::query_as(
        "SELECT mood, track_ids, served_track_ids FROM user_mood_mix WHERE profile_id = $1",
    )
    .bind(profile.into_uuid())
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn presets_five_moods_counts_and_pending(pool: PgPool) {
    let e = env(pool, true, 30).await;
    let presets = e.presets(&e.ana).await;
    assert_eq!(
        presets
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        VALID_MOODS
    );
    let chill = &presets[2];
    assert_eq!(
        *chill,
        json!({ "id": "chill", "name": "Calm", "color": "from-teal-400 to-cyan-500", "icon": "Wind",
                "trackCount": 30, "pendingTracks": 1 })
    );
    assert_eq!(presets[0]["name"], "Upbeat");
    assert_eq!(presets[0]["icon"], "Smile");
    for p in &presets {
        assert_eq!(p["pendingTracks"], 1, "{p}");
        let keys: BTreeSet<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            BTreeSet::from(["id", "name", "color", "icon", "trackCount", "pendingTracks"])
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn presets_show_0_when_the_page_recipe_has_fewer_than_8_members(pool: PgPool) {
    // recipes.integration: "shows 0 on a mood preset whose page recipe has fewer than 8 members, the count its page
    // plays".
    let e = env(pool, true, 5).await;
    let presets = e.presets(&e.ana).await;
    assert_eq!(presets[2]["trackCount"], 0);
    let res = e.bucket(&e.ana, "chill").await;
    res.assert_status(StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn presets_zero_when_heads_absent(pool: PgPool) {
    let e = env(pool, false, 30).await;
    for p in e.presets(&e.ana).await {
        assert_eq!(p["trackCount"], 0, "{p}");
        // No heads, no mood step: nothing is waiting to be scored (D-275).
        assert_eq!(p["pendingTracks"], 0, "{p}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn invalid_mood_400_with_valid_moods(pool: PgPool) {
    let e = env(pool, true, 30).await;
    let want = json!({ "error": "Invalid mood: blue", "validMoods": VALID_MOODS });
    let res = e.bucket(&e.ana, "blue").await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(res.json::<Value>(), want);
    let res = e.save(&e.ana, "blue", Some(json!({}))).await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(res.json::<Value>(), want);
    // `presets` is not a mood to save.
    let res = e.save(&e.ana, "presets", None).await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(res.json::<Value>()["error"], "Invalid mood: presets");
    for path in [
        "/api/mixes/mood/buckets/presets",
        "/api/mixes/mood/buckets/chill",
    ] {
        e.server
            .get(path)
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn not_enough_tracks_400_with_suggestion(pool: PgPool) {
    let e = env(pool.clone(), false, 30).await;
    let want = json!({ "error": "Not enough tracks for mood: chill", "suggestion": "Wait for more tracks to be analyzed" });
    let res = e.bucket(&e.ana, "chill").await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(res.json::<Value>(), want);
    // A save without songs (or with an empty list) draws a fresh mix: none either.
    for body in [None, Some(json!({})), Some(json!({ "trackIds": [] }))] {
        let res = e.save(&e.ana, "chill", body).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>(), want);
    }
    assert_eq!(saved_row(&pool, e.ana_profile).await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bucket_mix_is_the_page_recipe_draw(pool: PgPool) {
    let e = env(pool.clone(), true, 30).await;
    let res = e.bucket(&e.ana, "chill").await;
    res.assert_status_ok();
    let text = res.text();
    let mix: Value = serde_json::from_str(&text).unwrap();
    let id = mix["id"].as_str().unwrap();
    let ms = id.strip_prefix("mood-chill-").expect(id);
    assert!(ms.parse::<u64>().is_ok(), "{id}");
    assert_eq!(mix["mood"], "chill");
    assert_eq!(mix["name"], "Calm Mix");
    assert_eq!(mix["description"], "Tracks that match your calm vibe");
    let ids = ids_of(&mix["trackIds"]);
    assert_eq!(ids.len(), 15);
    assert_eq!(mix["trackCount"], 15);
    assert!(ids.iter().all(|t| e.members().contains(t)), "members only");
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), 15);
    let keys: BTreeSet<&str> = mix
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "id",
            "mood",
            "name",
            "description",
            "trackIds",
            "coverUrls",
            "trackCount",
            "color",
            "tracks"
        ])
    );
    // No saved mix: the serving is not remembered, and no row appears.
    assert_eq!(saved_row(&pool, e.ana_profile).await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn served_ids_skipped_while_8_remain(pool: PgPool) {
    let e = env(pool.clone(), true, 30).await;
    // A saved mix of this mood makes the servings remembered.
    let res = e
        .save(
            &e.ana,
            "chill",
            Some(json!({ "trackIds": [e.m(0).to_string()] })),
        )
        .await;
    res.assert_status_ok();
    let first = e.bucket(&e.ana, "chill").await.json::<Value>();
    let first_ids = ids_of(&first["trackIds"]);
    assert_eq!(first_ids.len(), 15);
    assert!(!first_ids.contains(&e.m(0)), "the saved song is skipped");
    let second = e.bucket(&e.ana, "chill").await.json::<Value>();
    let second_ids = ids_of(&second["trackIds"]);
    // 30 members less the 15 served and the saved one: 14 left, at least 8, so none of them repeats.
    assert_eq!(second_ids.len(), 14);
    assert!(
        second_ids
            .iter()
            .all(|t| !first_ids.contains(t) && *t != e.m(0))
    );
    // Then too few are left unserved: the draw starts over from every member rather than a thin mix.
    let third = e.bucket(&e.ana, "chill").await.json::<Value>();
    assert_eq!(ids_of(&third["trackIds"]).len(), 15);
    // The memory is per profile: the sibling profile saved nothing, so it is told nothing was served.
    let kid = e.bucket(&e.kid, "chill").await.json::<Value>();
    assert_eq!(ids_of(&kid["trackIds"]).len(), 15);
    assert_eq!(saved_row(&pool, e.kid_profile).await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn record_served_update_only(pool: PgPool) {
    let e = env(pool.clone(), true, 30).await;
    e.bucket(&e.ana, "chill").await.assert_status_ok();
    assert_eq!(
        saved_row(&pool, e.ana_profile).await,
        None,
        "no row created"
    );
    e.save(
        &e.ana,
        "chill",
        Some(json!({ "trackIds": [e.m(0).to_string()] })),
    )
    .await
    .assert_status_ok();
    let served = ids_of(&e.bucket(&e.ana, "chill").await.json::<Value>()["trackIds"]);
    let (mood, saved, remembered) = saved_row(&pool, e.ana_profile).await.unwrap();
    assert_eq!((mood.as_str(), saved), ("chill", vec![e.m(0)]));
    assert_eq!(remembered, served);
    // A serving of another mood is not remembered against this one.
    let party = e.bucket(&e.ana, "party").await;
    if party.status_code() == StatusCode::OK {
        assert_eq!(saved_row(&pool, e.ana_profile).await.unwrap().2, served);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_with_track_ids_keeps_order_drops_missing(pool: PgPool) {
    let e = env(pool.clone(), false, 30).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(e.m(5))
        .execute(&pool)
        .await
        .unwrap();
    let sent = json!([
        e.m(3).to_string(),
        Uuid::now_v7().to_string(),
        "not-an-id",
        e.m(1).to_string(),
        e.m(5).to_string(),
    ]);
    let before = time::OffsetDateTime::now_utc();
    let res = e
        .save(&e.ana, "chill", Some(json!({ "trackIds": sent })))
        .await;
    res.assert_status_ok();
    let text = res.text();
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["success"], true);
    let mix = &body["mix"];
    assert_eq!(ids_of(&mix["trackIds"]), [e.m(3), e.m(1)]);
    assert_eq!(mix["trackCount"], 2);
    assert_eq!(mix["mood"], "chill");
    assert_eq!(mix["name"], "Your Calm Mix");
    assert_eq!(mix["description"], "Based on your calm preferences");
    let generated = time::OffsetDateTime::parse(
        mix["generatedAt"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert!(generated >= before - time::Duration::milliseconds(1));
    let ms = generated.unix_timestamp_nanos() / 1_000_000;
    assert_eq!(mix["id"], json!(format!("your-mood-mix-{ms}")));
    let keys: BTreeSet<&str> = mix
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "id",
            "mood",
            "name",
            "description",
            "trackIds",
            "coverUrls",
            "trackCount",
            "color",
            "generatedAt",
            "tracks"
        ])
    );
    assert_eq!(
        saved_row(&pool, e.ana_profile).await.unwrap(),
        (
            "chill".to_string(),
            vec![e.m(3), e.m(1)],
            vec![e.m(3), e.m(1)]
        )
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_none_playable_400(pool: PgPool) {
    let e = env(pool.clone(), false, 30).await;
    e.save(
        &e.ana,
        "chill",
        Some(json!({ "trackIds": [e.m(2).to_string()] })),
    )
    .await
    .assert_status_ok();
    let res = e
        .save(
            &e.ana,
            "sad",
            Some(json!({ "trackIds": [Uuid::now_v7().to_string(), "nope"] })),
        )
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        res.json::<Value>(),
        json!({ "error": "None of those songs can be saved: each is missing, corrupt or not in the library" })
    );
    // The earlier mix is left as it was.
    assert_eq!(
        saved_row(&pool, e.ana_profile).await.unwrap(),
        ("chill".to_string(), vec![e.m(2)], vec![e.m(2)])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_validation_messages(pool: PgPool) {
    let e = env(pool, false, 30).await;
    let sixteen: Vec<String> = (0..16).map(|i| e.m(i).to_string()).collect();
    for (body, error) in [
        (json!({ "trackIds": "x" }), "trackIds must be an array"),
        (json!({ "trackIds": null }), "trackIds must be an array"),
        (json!({ "trackIds": {} }), "trackIds must be an array"),
        (
            json!({ "trackIds": [e.m(0).to_string(), 1] }),
            "Each trackId must be a string",
        ),
        (
            json!({ "trackIds": sixteen }),
            "trackIds must not exceed 15 tracks",
        ),
    ] {
        let res = e.save(&e.ana, "chill", Some(body.clone())).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>(), json!({ "error": error }), "{body}");
    }
    // Exactly the limit is fine.
    let fifteen: Vec<String> = (0..15).map(|i| e.m(i).to_string()).collect();
    let res = e
        .save(&e.ana, "chill", Some(json!({ "trackIds": fifteen })))
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["mix"]["trackCount"], 15);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_drops_mix_cache(pool: PgPool) {
    let e = env(pool, false, 30).await;
    let list = |e: &Env| {
        let req = e.server.get("/api/mixes").authorization_bearer(&e.ana);
        async move { req.await.json::<Vec<Value>>() }
    };
    let before = list(&e).await;
    assert!(before.iter().all(|m| m["type"] != "mood"));
    let saved = e
        .save(
            &e.ana,
            "chill",
            Some(json!({ "trackIds": [e.m(0).to_string()] })),
        )
        .await
        .json::<Value>();
    let after = list(&e).await;
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after.last().unwrap()["id"], saved["mix"]["id"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn saved_mix_reread_on_home_list(pool: PgPool) {
    let e = env(pool.clone(), false, 30).await;
    let saved = e
        .save(
            &e.ana,
            "chill",
            Some(json!({ "trackIds": [e.m(0).to_string(), e.m(1).to_string()] })),
        )
        .await
        .json::<Value>();
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(e.m(0))
        .execute(&pool)
        .await
        .unwrap();
    e.state.listeners.drop_mix_lists(e.ana_profile.into_uuid());
    let list = e
        .server
        .get("/api/mixes")
        .authorization_bearer(&e.ana)
        .await
        .json::<Vec<Value>>();
    let home = list.last().unwrap();
    assert_eq!(home["id"], saved["mix"]["id"]);
    assert_eq!(home["type"], "mood");
    assert_eq!(home["mood"], "chill");
    assert_eq!(home["name"], "Your Calm Mix");
    assert_eq!(ids_of(&home["trackIds"]), [e.m(1)]);
    assert_eq!(home["trackCount"], 1);
    // The sibling profile's Home has no saved mix.
    let kid = e
        .server
        .get("/api/mixes")
        .authorization_bearer(&e.kid)
        .await
        .json::<Vec<Value>>();
    assert!(kid.iter().all(|m| m["type"] != "mood"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bucket_track_rows_use_the_appendix_d_shape(pool: PgPool) {
    let e = env(pool, true, 30).await;
    let res = e.bucket(&e.ana, "chill").await;
    res.assert_status_ok();
    let text = res.text();
    let mix: Value = serde_json::from_str(&text).unwrap();
    let rows = mix["tracks"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r["id"].as_str().unwrap().parse::<Uuid>().unwrap())
            .collect::<Vec<_>>(),
        ids_of(&mix["trackIds"])
    );
    assert_rows_shape(&text, text.find("\"tracks\":").unwrap(), rows);

    let res = e
        .save(
            &e.ana,
            "chill",
            Some(json!({ "trackIds": [e.m(4).to_string()] })),
        )
        .await;
    res.assert_status_ok();
    let text = res.text();
    let body: Value = serde_json::from_str(&text).unwrap();
    let rows = body["mix"]["tracks"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], json!(e.m(4).to_string()));
    assert_rows_shape(&text, text.find("\"tracks\":").unwrap(), rows);
}
