mod common;

use kima_db::PgPool;
use serde_json::{Value, json};

const KEYS: [&str; 8] = [
    "userId",
    "playbackQuality",
    "wifiOnly",
    "offlineEnabled",
    "maxCacheSizeMb",
    "shareLiveListening",
    "hasListenbrainzToken",
    "liveSharingAvailable",
];

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requires_auth(pool: PgPool) {
    let server = axum_test::TestServer::new(kima_api::router(common::state(pool))).unwrap();
    server
        .get("/api/settings")
        .await
        .assert_status_unauthorized();
    server
        .post("/api/settings")
        .json(&json!({}))
        .await
        .assert_status_unauthorized();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_defaults_without_row_and_without_writing(pool: PgPool) {
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let body: Value = server
        .get("/api/settings")
        .authorization_bearer(&token)
        .await
        .json();
    for k in KEYS {
        assert!(body.get(k).is_some(), "missing {k}");
    }
    assert_eq!(body["userId"], user.id.to_string());
    assert_eq!(body["playbackQuality"], "original");
    assert_eq!(body["wifiOnly"], false);
    assert_eq!(body["offlineEnabled"], false);
    assert_eq!(body["maxCacheSizeMb"], 10240);
    assert_eq!(body["shareLiveListening"], true);
    assert_eq!(body["hasListenbrainzToken"], false);
    assert_eq!(body["liveSharingAvailable"], false);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM user_settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "GET must not write");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn partial_post_keeps_other_fields(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let first: Value = server
        .post("/api/settings")
        .authorization_bearer(&token)
        .json(&json!({ "playbackQuality": "medium", "wifiOnly": true, "unknown": 1 }))
        .await
        .json();
    assert_eq!(first["playbackQuality"], "medium");
    assert_eq!(first["wifiOnly"], true);
    assert_eq!(first["maxCacheSizeMb"], 10240);
    let second: Value = server
        .post("/api/settings")
        .authorization_bearer(&token)
        .json(&json!({ "maxCacheSizeMb": 0, "shareLiveListening": false }))
        .await
        .json();
    assert_eq!(second["playbackQuality"], "medium");
    assert_eq!(second["wifiOnly"], true);
    assert_eq!(second["maxCacheSizeMb"], 0);
    assert_eq!(second["shareLiveListening"], false);
    let got: Value = server
        .get("/api/settings")
        .authorization_bearer(&token)
        .await
        .json();
    assert_eq!(got, second);
    assert!(got.get("unknown").is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn invalid_bodies_are_400_with_details(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    for bad in [
        json!({ "playbackQuality": "ultra" }),
        json!({ "maxCacheSizeMb": -1 }),
        json!({ "maxCacheSizeMb": 1.5 }),
        json!({ "wifiOnly": "yes" }),
        json!({ "listenbrainzToken": 5 }),
        json!([1]),
    ] {
        let res = server
            .post("/api/settings")
            .authorization_bearer(&token)
            .json(&bad)
            .await;
        res.assert_status_bad_request();
        let body: Value = res.json();
        assert_eq!(body["error"], "Invalid settings", "{bad}");
        assert!(body["details"].as_array().is_some_and(|d| !d.is_empty()));
    }
    // Nothing was stored by the failed posts.
    let got: Value = server
        .get("/api/settings")
        .authorization_bearer(&token)
        .await
        .json();
    assert_eq!(got["playbackQuality"], "original");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listenbrainz_token_is_ignored_and_not_echoed(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    let res = server
        .post("/api/settings")
        .authorization_bearer(&token)
        .json(&json!({ "listenbrainzToken": "secret-token-123", "wifiOnly": true }))
        .await;
    res.assert_status_ok();
    let text = res.text();
    assert!(!text.contains("secret-token-123"));
    let body: Value = res.json();
    assert_eq!(body["hasListenbrainzToken"], false);
    assert!(body.get("listenbrainzToken").is_none());
    server
        .post("/api/settings")
        .authorization_bearer(&token)
        .json(&json!({ "listenbrainzToken": null }))
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn settings_are_per_user(pool: PgPool) {
    let state = common::state(pool);
    let (_, ana) = common::user_with_token(&state, "ana").await;
    let (_, ben) = common::user_with_token(&state, "ben").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .post("/api/settings")
        .authorization_bearer(&ana)
        .json(&json!({ "playbackQuality": "low" }))
        .await
        .assert_status_ok();
    let b: Value = server
        .get("/api/settings")
        .authorization_bearer(&ben)
        .await
        .json();
    assert_eq!(b["playbackQuality"], "original");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cleanup_stale_jobs_is_unrouted(pool: PgPool) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();
    server
        .post("/api/settings/cleanup-stale-jobs")
        .authorization_bearer(&token)
        .await
        .assert_status_not_found();
}

// ---- /api/system-settings: the key-value settings (M3a Task 5) ----

mod system {
    use super::common;
    use axum_test::TestServer;
    use kima_api::AppState;
    use kima_core::identify::IdentifySwitch;
    use kima_db::PgPool;
    use serde_json::{Value, json};

    async fn setup(
        pool: PgPool,
        state: impl FnOnce(AppState) -> AppState,
    ) -> (TestServer, AppState, String, String) {
        let state = state(common::state(pool));
        let (_, admin) = common::user_with_token(&state, "ana").await;
        let (_, user) = common::user_with_token(&state, "ben").await;
        let server = TestServer::new(kima_api::router(state.clone())).unwrap();
        (server, state, admin, user)
    }

    async fn get(server: &TestServer, admin: &str) -> Value {
        server
            .get("/api/system-settings")
            .authorization_bearer(admin)
            .await
            .json()
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn get_defaults(pool: PgPool) {
        let (server, _, admin, _) = setup(pool, |s| s).await;
        let body = get(&server, &admin).await;
        assert_eq!(
            body["registrationOpen"], true,
            "the helper opens registration"
        );
        assert_eq!(body["fanartEnabled"], false);
        assert_eq!(body["fanartApiKey"], "");
        assert_eq!(body["hasFanartApiKey"], false);
        assert_eq!(body["acoustidApiKey"], "");
        assert_eq!(body["hasAcoustidApiKey"], false);
        assert_eq!(body["autoEnrichMetadata"], true);
        assert_eq!(body["autoEnrichMetadataLocked"], false);
        assert_eq!(body["enrichmentConcurrency"], 1);
        assert_eq!(body["autotagAutoApply"], "off");
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn acoustid_key_encrypted_at_rest_never_returned(pool: PgPool) {
        let (server, _, admin, _) = setup(pool.clone(), |s| s).await;
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "acoustidApiKey": "abc" }))
            .await
            .assert_status_ok();
        let body = get(&server, &admin).await;
        assert_eq!(body["acoustidApiKey"], "");
        assert_eq!(body["hasAcoustidApiKey"], true);
        assert!(!body.to_string().contains("abc"));
        let blob: Vec<u8> =
            sqlx::query_scalar("SELECT secret_enc FROM settings_kv WHERE key = 'acoustidApiKey'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!blob.windows(3).any(|w| w == b"abc"));
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn post_empty_string_keeps_secret(pool: PgPool) {
        let (server, _, admin, _) = setup(pool, |s| s).await;
        for body in [
            json!({ "fanartApiKey": "k" }),
            json!({ "fanartApiKey": "" }),
            json!({}),
        ] {
            server
                .post("/api/system-settings")
                .authorization_bearer(&admin)
                .json(&body)
                .await
                .assert_status_ok();
            assert_eq!(get(&server, &admin).await["hasFanartApiKey"], true);
        }
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn post_null_clears_secret(pool: PgPool) {
        let (server, _, admin, _) = setup(pool.clone(), |s| s).await;
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "fanartApiKey": "k" }))
            .await
            .assert_status_ok();
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "fanartApiKey": null }))
            .await
            .assert_status_ok();
        assert_eq!(get(&server, &admin).await["hasFanartApiKey"], false);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM settings_kv")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn post_trims_secret(pool: PgPool) {
        let (server, state, admin, _) = setup(pool, |s| s).await;
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "acoustidApiKey": "  abc \n" }))
            .await
            .assert_status_ok();
        let kv = state.settings.get().await.unwrap();
        assert_eq!(kv.acoustid_api_key.as_deref(), Some("abc"));
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn post_too_long_400(pool: PgPool) {
        let (server, _, admin, _) = setup(pool, |s| s).await;
        let res = server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "fanartApiKey": "x".repeat(201) }))
            .await;
        res.assert_status_bad_request();
        res.assert_json(&json!({"error":"Invalid settings","details":"fanartApiKey is too long"}));
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "fanartApiKey": "x".repeat(200) }))
            .await
            .assert_status_ok();
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn post_wrong_types_400(pool: PgPool) {
        let (server, _, admin, _) = setup(pool, |s| s).await;
        let cases = [
            (
                json!({"fanartEnabled": "yes"}),
                "fanartEnabled must be a boolean",
            ),
            (
                json!({"autoEnrichMetadata": 1}),
                "autoEnrichMetadata must be a boolean",
            ),
            (
                json!({"enrichmentConcurrency": 0}),
                "enrichmentConcurrency must be a whole number from 1 to 5",
            ),
            (
                json!({"enrichmentConcurrency": 6}),
                "enrichmentConcurrency must be a whole number from 1 to 5",
            ),
            (
                json!({"enrichmentConcurrency": 2.5}),
                "enrichmentConcurrency must be a whole number from 1 to 5",
            ),
            (
                json!({"enrichmentConcurrency": "2"}),
                "enrichmentConcurrency must be a whole number from 1 to 5",
            ),
            (
                json!({"autotagAutoApply": "all"}),
                "autotagAutoApply must be one of off, db, files",
            ),
            (
                json!({"fanartApiKey": 5}),
                "fanartApiKey must be a string or null",
            ),
            (
                json!({"acoustidApiKey": true}),
                "acoustidApiKey must be a string or null",
            ),
        ];
        for (body, detail) in cases {
            let res = server
                .post("/api/system-settings")
                .authorization_bearer(&admin)
                .json(&body)
                .await;
            res.assert_status_bad_request();
            res.assert_json(&json!({"error":"Invalid settings","details":detail}));
        }
        // Nothing from a rejected body is stored.
        let res = server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({"fanartApiKey": "k", "fanartEnabled": "no"}))
            .await;
        res.assert_status_bad_request();
        assert_eq!(get(&server, &admin).await["hasFanartApiKey"], false);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn post_concurrency_resizes_net(pool: PgPool) {
        use kima_net::{Bases, Net, NetConfig, Source};
        let net = std::sync::Arc::new(Net::new(
            NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
            pool.clone(),
            IdentifySwitch::new(false, true),
        ));
        let (server, _, admin, _) = setup(pool, |s| s.with_net(net.clone())).await;
        assert_eq!(net.concurrency(Source::Wikidata), Some(3));
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "enrichmentConcurrency": 2 }))
            .await
            .assert_status_ok();
        // Scaled, but never above the burst.
        assert_eq!(net.concurrency(Source::Wikidata), Some(5));
        assert_eq!(net.concurrency(Source::MusicBrainz), Some(1));
        assert_eq!(net.concurrency(Source::Deezer), Some(10));
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn post_unknown_fields_ignored(pool: PgPool) {
        let (server, _, admin, _) = setup(pool, |s| s).await;
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "nope": 1, "lastfmApiKey": "x", "enrichmentConcurrency": 3 }))
            .await
            .assert_status_ok();
        let body = get(&server, &admin).await;
        assert_eq!(body["enrichmentConcurrency"], 3);
        assert!(body.get("nope").is_none());
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn post_answers_m1_body(pool: PgPool) {
        let (server, _, admin, _) = setup(pool, |s| s).await;
        let res = server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "acoustidApiKey": "abc", "fanartEnabled": true, "autotagAutoApply": "db" }))
            .await;
        res.assert_json(&json!({ "registrationOpen": true, "libraryRoot": null }));
        let body = get(&server, &admin).await;
        assert_eq!(body["fanartEnabled"], true);
        assert_eq!(body["autotagAutoApply"], "db");
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn non_admin_403(pool: PgPool) {
        let (server, _, _, user) = setup(pool, |s| s).await;
        server
            .get("/api/system-settings")
            .authorization_bearer(&user)
            .await
            .assert_status_forbidden();
        server
            .post("/api/system-settings")
            .authorization_bearer(&user)
            .json(&json!({ "fanartApiKey": "k" }))
            .await
            .assert_status_forbidden();
        server
            .get("/api/system-settings")
            .await
            .assert_status_unauthorized();
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn identify_env_off_locks(pool: PgPool) {
        let (server, state, admin, _) =
            setup(pool, |s| s.with_identify(IdentifySwitch::new(true, true))).await;
        let body = get(&server, &admin).await;
        assert_eq!(body["autoEnrichMetadata"], false);
        assert_eq!(body["autoEnrichMetadataLocked"], true);
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "autoEnrichMetadata": true }))
            .await
            .assert_status_ok();
        assert!(
            state.settings.get().await.unwrap().auto_enrich_metadata,
            "stored"
        );
        assert!(!state.identify.enabled());
        assert_eq!(get(&server, &admin).await["autoEnrichMetadata"], false);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn toggle_flips_switch(pool: PgPool) {
        let (server, state, admin, _) = setup(pool, |s| s).await;
        assert!(state.identify.enabled());
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "autoEnrichMetadata": false }))
            .await
            .assert_status_ok();
        assert!(!state.identify.enabled());
        assert_eq!(get(&server, &admin).await["autoEnrichMetadata"], false);
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "autoEnrichMetadata": true }))
            .await
            .assert_status_ok();
        assert!(state.identify.enabled());
    }

    async fn enrich_jobs(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'enrich'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn turning_switch_on_kicks(pool: PgPool) {
        let (server, state, admin, _) = setup(pool.clone(), |s| {
            s.with_identify(IdentifySwitch::new(false, false))
        })
        .await;
        // Saving with the switch staying off, or turning it off, does not kick.
        for on in [false, false] {
            server
                .post("/api/system-settings")
                .authorization_bearer(&admin)
                .json(&json!({ "autoEnrichMetadata": on }))
                .await
                .assert_status_ok();
        }
        assert_eq!(enrich_jobs(&pool).await, 0);
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "autoEnrichMetadata": true }))
            .await
            .assert_status_ok();
        assert!(state.identify.enabled());
        assert_eq!(enrich_jobs(&pool).await, 1, "turning it on kicks");
        // Already on: a second save does not kick again (the queued one would absorb it anyway).
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "autoEnrichMetadata": true }))
            .await
            .assert_status_ok();
        assert_eq!(enrich_jobs(&pool).await, 1);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn locked_switch_on_does_not_kick(pool: PgPool) {
        let (server, _, admin, _) = setup(pool.clone(), |s| {
            s.with_identify(IdentifySwitch::new(true, false))
        })
        .await;
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "autoEnrichMetadata": true }))
            .await
            .assert_status_ok();
        assert_eq!(
            enrich_jobs(&pool).await,
            0,
            "the effective switch stays off"
        );
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn locked_switch_keeps_stored_value_across_saves(pool: PgPool) {
        let (server, state, admin, _) = setup(pool.clone(), |s| {
            s.with_identify(IdentifySwitch::new(true, true))
        })
        .await;
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "autoEnrichMetadata": true }))
            .await
            .assert_status_ok();
        let mut posted = get(&server, &admin).await;
        assert_eq!(posted["autoEnrichMetadata"], false);
        posted["enrichmentConcurrency"] = json!(4);
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&posted)
            .await
            .assert_status_ok();
        let kv = kima_db::settings_kv::load(&pool, &state.cipher)
            .await
            .unwrap();
        assert!(kv.auto_enrich_metadata, "stored true survives the save");
        assert_eq!(kv.enrichment_concurrency, 4);
        let body = get(&server, &admin).await;
        assert_eq!(body["autoEnrichMetadata"], false);
        assert_eq!(body["autoEnrichMetadataLocked"], true);
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn unlocked_post_of_false_is_stored(pool: PgPool) {
        let (server, state, admin, _) = setup(pool.clone(), |s| s).await;
        server
            .post("/api/system-settings")
            .authorization_bearer(&admin)
            .json(&json!({ "autoEnrichMetadata": false }))
            .await
            .assert_status_ok();
        let kv = kima_db::settings_kv::load(&pool, &state.cipher)
            .await
            .unwrap();
        assert!(!kv.auto_enrich_metadata);
        assert!(!state.identify.enabled());
    }
}
