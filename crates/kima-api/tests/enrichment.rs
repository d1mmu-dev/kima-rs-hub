//! `/api/enrichment` controls, progress, status, failures and concurrency (spec §16.2).

mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, seed};
use kima_db::PgPool;
use serde_json::{Value, json};
use uuid::Uuid;

/// A server, an admin token and a plain user's token.
async fn server(pool: PgPool) -> (TestServer, String, String) {
    let state = common::state(pool);
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let (_, user) = common::user_with_token(&state, "bob").await;
    (
        TestServer::new(kima_api::router(state)).unwrap(),
        admin,
        user,
    )
}

async fn get(srv: &TestServer, token: &str, path: &str) -> Value {
    let res = srv.get(path).authorization_bearer(token).await;
    res.assert_status_ok();
    res.json()
}

async fn post(srv: &TestServer, token: &str, path: &str) -> Value {
    let res = srv.post(path).authorization_bearer(token).await;
    res.assert_status_ok();
    res.json()
}

async fn set_status(pool: &PgPool, status: &str, idle_for_mins: i64) {
    sqlx::query(
        "UPDATE enrichment_state SET status = $1, halted = false,
                last_activity = now() - make_interval(mins => $2::int)",
    )
    .bind(status)
    .bind(idle_for_mins as i32)
    .execute(pool)
    .await
    .unwrap();
}

async fn track(pool: &PgPool, n: i32) -> Uuid {
    seed(
        pool,
        Seed::new("Artist", "Album", None, &format!("Song {n}")).at(1, n),
    )
    .await
    .track
}

async fn stage(pool: &PgPool, track: Uuid, stage: &str, status: &str, next_in_secs: Option<i32>) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, attempts, last_error, error_code,
                                   next_attempt_at)
         VALUES ($1, $2, $3, 1, 'boom', 'HTTP_503',
                 CASE WHEN $4::int IS NULL THEN NULL
                      ELSE now() + make_interval(secs => $4::int) END)
         ON CONFLICT (track_id, stage) DO UPDATE
           SET status = $3, attempts = 1, last_error = 'boom', error_code = 'HTTP_503',
               next_attempt_at = CASE WHEN $4::int IS NULL THEN NULL
                      ELSE now() + make_interval(secs => $4::int) END",
    )
    .bind(track)
    .bind(stage)
    .bind(status)
    .bind(next_in_secs)
    .execute(pool)
    .await
    .unwrap();
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut k: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    k.sort();
    k
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedding_api_progress_status_and_failure_counts_agree(pool: PgPool) {
    let t = track(&pool, 1).await;
    let done = track(&pool, 2).await;
    stage(&pool, t, "embedding", "failed", Some(60)).await;
    stage(&pool, done, "embedding", "done", None).await;
    let (srv, _, user) = server(pool).await;
    let p = get(&srv, &user, "/api/enrichment/progress").await;
    assert_eq!(p["clapEmbeddings"]["completed"], 1);
    assert_eq!(p["clapEmbeddings"]["failed"], 1);
    assert_eq!(p["clapEmbeddings"]["retrying"], 1);
    assert_eq!(p["clapEmbeddings"]["progress"], 50);
    let s = get(&srv, &user, "/api/enrichment/status").await;
    assert_eq!(
        s["audio"],
        json!({"total":2,"completed":1,"processing":0,"failed":1})
    );
    let c = get(&srv, &user, "/api/enrichment/failures/counts").await;
    assert_eq!(c["audio"], 1);
    assert_eq!(c["total"], 1);
    for path in [
        "/api/enrichment/failures",
        "/api/enrichment/failures?entityType=audio",
    ] {
        let f = get(&srv, &user, path).await;
        assert_eq!(f["total"], 1);
        assert_eq!(f["failures"][0]["entityType"], "audio");
        assert_eq!(f["failures"][0]["metadata"]["stage"], "embedding");
    }
}

#[tokio::test]
#[ignore = "private audio+text CLAP models, trusted ORT, unprivileged reflink filesystem and guarded isolated DB required"]
async fn runtime_snapshot_failure_reaches_http_ml_capabilities_audio_only() {
    // No sqlx test harness: identity must be checked before ANY setup/migration.
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;
    let options =
        PgConnectOptions::from_str(&std::env::var("DATABASE_URL").expect("DATABASE_URL required"))
            .unwrap();
    let database = std::env::var("PGDATABASE").expect("PGDATABASE required");
    assert_eq!(options.get_host(), "127.0.0.1");
    assert_ne!(options.get_port(), 5456);
    assert_eq!(options.get_database(), Some(database.as_str()));
    assert!(
        database.starts_with("kima_fix_"),
        "run-owned empty database required"
    );
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .unwrap();
    let identity: (String, bool) =
        sqlx::query_as("SELECT current_database()::text, NOT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_schema = 'public')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        identity,
        (database, true),
        "refuse preexisting application database"
    );
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    use kima_ident::stages::embedding;
    use std::os::unix::fs::PermissionsExt;
    use tokio_util::sync::CancellationToken;
    let model_dir = std::env::var_os("KIMA_CLAP_MODEL_DIR").expect("KIMA_CLAP_MODEL_DIR required");
    let root = tempfile::tempdir().unwrap();
    let snapshot_dir = root.path().join("snapshots");
    std::fs::create_dir(&snapshot_dir).unwrap();
    std::fs::set_permissions(&snapshot_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let mut state = common::state(pool.clone()).with_library_root(root.path().to_path_buf());
    let mut ctx = state.ident_ctx();
    ctx.config.embedding = kima_ident::EmbeddingConfig {
        model_dir: Some(model_dir.into()),
        snapshot_dir: Some(snapshot_dir.clone()),
        ..Default::default()
    };
    embedding::check_snapshot_setup(&mut ctx.config.embedding, root.path()).unwrap();
    let init = embedding::initialize(&ctx).await.unwrap();
    let init = serde_json::to_value(init).unwrap();
    assert_eq!(
        init["capabilities"]["clap_audio_embedding"]["state"], "available",
        "{init}"
    );
    assert_eq!(
        init["capabilities"]["clap_text_embedding"]["state"], "available",
        "{init}"
    );
    embedding::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    state.ident = Some(ctx.clone());
    let (_, user) = common::user_with_token(&state, "ml-reader").await;
    let srv = TestServer::new(kima_api::router(state)).unwrap();
    let before = get(&srv, &user, "/api/enrichment/status").await;
    assert_eq!(
        before["mlCapabilities"]["capabilities"]["clap_audio_embedding"]["state"],
        "available"
    );
    let t = track(&pool, 1).await;
    std::fs::write(root.path().join("track.wav"), b"source bytes").unwrap();
    sqlx::query("UPDATE tracks SET path='track.wav' WHERE id=$1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    let _restore = Restore(snapshot_dir.clone());
    std::fs::set_permissions(&snapshot_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(
        std::fs::File::create(snapshot_dir.join("probe")).is_err(),
        "must run unprivileged"
    );
    let batch = embedding::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert!(batch.halted);
    assert_eq!(batch.failed, 1);
    let after = get(&srv, &user, "/api/enrichment/status").await;
    println!("production-router status before failure: {before}");
    println!("production-router status after failure: {after}");
    let caps = &after["mlCapabilities"]["capabilities"];
    assert_eq!(
        caps["clap_audio_embedding"]["state"], "unavailable",
        "{after}"
    );
    assert!(caps["clap_audio_embedding"].to_string().contains("restart"));
    assert_eq!(caps["clap_text_embedding"]["state"], "available");
    embedding::embed_text(&ctx, "a piano").await.unwrap();
    assert_eq!(
        embedding::run_batch(&ctx, &CancellationToken::new())
            .await
            .unwrap()
            .claimed,
        0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn progress_shape_exact(pool: PgPool) {
    let (srv, _, user) = server(pool).await;
    let p = get(&srv, &user, "/api/enrichment/progress").await;
    assert_eq!(
        keys(&p),
        sorted(&[
            "source",
            "artists",
            "trackTags",
            "audioAnalysis",
            "clapEmbeddings",
            "fingerprints",
            "similarity",
            "coreComplete",
            "isFullyComplete"
        ])
    );
    assert_eq!(p["source"], "TrackStage");
    assert_eq!(
        keys(&p["artists"]),
        sorted(&[
            "total",
            "completed",
            "pending",
            "owed",
            "failed",
            "progress"
        ])
    );
    assert_eq!(
        keys(&p["trackTags"]),
        sorted(&[
            "total",
            "enriched",
            "pending",
            "waiting",
            "waitingMissing",
            "progress"
        ])
    );
    assert_eq!(
        keys(&p["audioAnalysis"]),
        sorted(&[
            "total",
            "completed",
            "pending",
            "queued",
            "processing",
            "failed",
            "permanentlyFailed",
            "waitingMissing",
            "progress",
            "isBackground"
        ])
    );
    assert_eq!(
        keys(&p["clapEmbeddings"]),
        sorted(&[
            "total",
            "completed",
            "pending",
            "processing",
            "failed",
            "waitingMissing",
            "retrying",
            "progress",
            "isBackground"
        ])
    );
    assert_eq!(
        keys(&p["fingerprints"]),
        sorted(&[
            "total",
            "completed",
            "pending",
            "processing",
            "failed",
            "waitingMissing",
            "progress",
            "isBackground"
        ])
    );
    assert_eq!(
        keys(&p["similarity"]),
        sorted(&["total", "completed", "pending", "progress", "isBackground"])
    );
    assert_eq!(p["fingerprints"]["isBackground"], true);
    assert_eq!(p["audioAnalysis"]["isBackground"], true);
    assert_eq!(p["similarity"]["isBackground"], true);
    assert!(p["coreComplete"].is_boolean() && p["isFullyComplete"].is_boolean());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn progress_memoised_5s(pool: PgPool) {
    let (srv, admin, user) = server(pool.clone()).await;
    let first = get(&srv, &user, "/api/enrichment/progress").await;
    assert_eq!(first["trackTags"]["total"], 0);
    track(&pool, 1).await;
    let second = get(&srv, &user, "/api/enrichment/progress").await;
    assert_eq!(second, first, "answered from the memo");
    // A control that changes the counts drops the memo.
    post(&srv, &admin, "/api/enrichment/wipe-and-rerun").await;
    let third = get(&srv, &user, "/api/enrichment/progress").await;
    assert_eq!(third["trackTags"]["total"], 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn status_idle_shape(pool: PgPool) {
    let (srv, _, user) = server(pool).await;
    let s = get(&srv, &user, "/api/enrichment/status").await;
    assert_eq!(s["status"], "idle");
    assert_eq!(s["currentPhase"], Value::Null);
    assert_eq!(s["hung"], false);
    for k in [
        "startedAt",
        "pausedAt",
        "stoppedAt",
        "lastError",
        "lastActivity",
        "artists",
        "tracks",
        "audio",
    ] {
        assert!(s.get(k).is_some(), "missing {k}");
    }
    assert_eq!(
        keys(&s["artists"]),
        sorted(&["total", "completed", "failed"])
    );
    assert_eq!(
        keys(&s["tracks"]),
        sorted(&["total", "completed", "failed"])
    );
    assert_eq!(
        keys(&s["audio"]),
        sorted(&["total", "completed", "failed", "processing"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn status_reports_hung(pool: PgPool) {
    let (srv, _, user) = server(pool.clone()).await;
    set_status(&pool, "running", 20).await;
    let s = get(&srv, &user, "/api/enrichment/status").await;
    assert_eq!(s["status"], "running");
    assert_eq!(s["hung"], true);
    set_status(&pool, "running", 1).await;
    let s = get(&srv, &user, "/api/enrichment/status").await;
    assert_eq!(s["hung"], false);
    // Only a running pipeline can be hung.
    set_status(&pool, "paused", 60).await;
    let s = get(&srv, &user, "/api/enrichment/status").await;
    assert_eq!(s["hung"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn pause_wrong_state_400_message(pool: PgPool) {
    let (srv, admin, _) = server(pool).await;
    let res = srv
        .post("/api/enrichment/pause")
        .authorization_bearer(&admin)
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "Cannot pause enrichment in idle state"})
    );
    let res = srv
        .post("/api/enrichment/resume")
        .authorization_bearer(&admin)
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "Cannot resume enrichment in idle state"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn pause_resume_stop_answers_state(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    set_status(&pool, "running", 0).await;
    let a = post(&srv, &admin, "/api/enrichment/pause").await;
    assert_eq!(a["message"], "Enrichment paused");
    assert_eq!(a["state"]["status"], "paused");
    assert!(a["state"]["pausedAt"].is_string());
    let a = post(&srv, &admin, "/api/enrichment/resume").await;
    assert_eq!(a["message"], "Enrichment resumed");
    assert_eq!(a["state"]["status"], "running");
    let a = post(&srv, &admin, "/api/enrichment/stop").await;
    assert_eq!(a["message"], "Enrichment stopping...");
    assert_eq!(a["state"]["status"], "stopping");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sync_answer_shape_and_counts(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    track(&pool, 1).await;
    track(&pool, 2).await;
    let a = post(&srv, &admin, "/api/enrichment/sync").await;
    assert_eq!(a["message"], "Incremental sync started");
    assert_eq!(a["description"], "Processing new and pending items only");
    assert_eq!(a["result"]["artists"], 1);
    assert_eq!(a["result"]["tracks"], 2);
    assert_eq!(a["result"]["audioQueued"], 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retry_answer_queued(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    let t = track(&pool, 1).await;
    stage(&pool, t, "recording", "failed", Some(3600)).await;
    let a = post(&srv, &admin, "/api/enrichment/retry-failed-and-blocked").await;
    assert_eq!(a["message"], "Failed and blocked work re-queued");
    assert_eq!(a["queued"], 1);
    assert_eq!(a["description"], "1 stage questions handed to the queue");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redecide_answer_reopened(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    let t = track(&pool, 1).await;
    stage(&pool, t, "recording", "done", None).await;
    let a = post(&srv, &admin, "/api/enrichment/re-decide-everything").await;
    assert_eq!(a["message"], "Every stage question re-opened");
    assert_eq!(a["reopened"], 1);
    assert_eq!(a["description"], "1 stage records reopened");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_answer_shape(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    track(&pool, 1).await;
    let a = post(&srv, &admin, "/api/enrichment/wipe-and-rerun").await;
    assert_eq!(keys(&a), sorted(&["artists", "tracks", "vectors"]));
    assert_eq!(a["artists"], 1);
    assert_eq!(a["vectors"], 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failures_paging_and_filter(pool: PgPool) {
    let (srv, _, user) = server(pool.clone()).await;
    for n in 1..=3 {
        let t = track(&pool, n).await;
        stage(&pool, t, "recording", "failed", Some(60)).await;
    }
    let all = get(&srv, &user, "/api/enrichment/failures").await;
    assert_eq!(all["total"], 3);
    assert_eq!(all["failures"].as_array().unwrap().len(), 3);
    let f = &all["failures"][0];
    assert_eq!(f["entityType"], "track");
    assert_eq!(f["errorCode"], "HTTP_503");
    assert_eq!(f["maxRetries"], 4);
    let page = get(&srv, &user, "/api/enrichment/failures?limit=2&offset=0").await;
    assert_eq!(page["failures"].as_array().unwrap().len(), 2);
    assert_eq!(page["total"], 3);
    let page = get(&srv, &user, "/api/enrichment/failures?limit=2&offset=2").await;
    assert_eq!(page["failures"].as_array().unwrap().len(), 1);
    let scan = get(&srv, &user, "/api/enrichment/failures?entityType=scan").await;
    assert_eq!(scan["total"], 0);
    let track_only = get(
        &srv,
        &user,
        "/api/enrichment/failures?entityType=track&includeSkipped=true&includeResolved=true",
    )
    .await;
    assert_eq!(track_only["total"], 3);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failure_counts_shape(pool: PgPool) {
    let (srv, _, user) = server(pool.clone()).await;
    let t = track(&pool, 1).await;
    stage(&pool, t, "recording", "failed", Some(60)).await;
    let c = get(&srv, &user, "/api/enrichment/failures/counts").await;
    assert_eq!(
        c,
        json!({"artist":0,"track":1,"audio":0,"vibe":0,"scan":0,"podcast":0,"total":1})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrency_put_clamps_and_estimates(pool: PgPool) {
    let (srv, admin, user) = server(pool).await;
    let res = srv
        .put("/api/enrichment/concurrency")
        .authorization_bearer(&admin)
        .json(&json!({"concurrency": 9}))
        .await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({
            "concurrency": 5,
            "estimatedSpeed": "~50 artists/min, ~300 tracks/min",
            "artistsPerMin": 50,
            "tracksPerMin": 300
        })
    );
    let res = srv
        .put("/api/enrichment/concurrency")
        .authorization_bearer(&admin)
        .json(&json!({"concurrency": 2.7}))
        .await;
    assert_eq!(res.json::<Value>()["concurrency"], 2);
    let res = srv
        .put("/api/enrichment/concurrency")
        .authorization_bearer(&admin)
        .json(&json!({"concurrency": -4}))
        .await;
    assert_eq!(res.json::<Value>()["concurrency"], 1);
    let got = get(&srv, &user, "/api/enrichment/concurrency").await;
    assert_eq!(got["concurrency"], 1);
    assert_eq!(got["estimatedSpeed"], "~10 artists/min, ~60 tracks/min");
    assert_eq!(got["artistsPerMin"], 10);
    assert_eq!(got["tracksPerMin"], 60);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrency_put_invalid_400(pool: PgPool) {
    let (srv, admin, _) = server(pool).await;
    let want = json!({"error": "Missing or invalid 'concurrency' parameter"});
    for body in [
        json!({}),
        json!({"concurrency": "3"}),
        json!({"concurrency": 0}),
        json!({"concurrency": null}),
    ] {
        let res = srv
            .put("/api/enrichment/concurrency")
            .authorization_bearer(&admin)
            .json(&body)
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>(), want, "{body}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn writes_need_admin_403(pool: PgPool) {
    let (srv, _, user) = server(pool).await;
    for path in [
        "/api/enrichment/pause",
        "/api/enrichment/resume",
        "/api/enrichment/stop",
        "/api/enrichment/sync",
        "/api/enrichment/retry-failed-and-blocked",
        "/api/enrichment/re-decide-everything",
        "/api/enrichment/wipe-and-rerun",
    ] {
        let res = srv.post(path).authorization_bearer(&user).await;
        res.assert_status(StatusCode::FORBIDDEN);
        assert_eq!(
            res.json::<Value>(),
            json!({"error": "Admin access required"}),
            "{path}"
        );
    }
    let res = srv
        .put("/api/enrichment/concurrency")
        .authorization_bearer(&user)
        .json(&json!({"concurrency": 2}))
        .await;
    res.assert_status(StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reads_need_auth_401(pool: PgPool) {
    let (srv, _, _) = server(pool).await;
    for path in [
        "/api/enrichment/progress",
        "/api/enrichment/status",
        "/api/enrichment/failures",
        "/api/enrichment/failures/counts",
        "/api/enrichment/concurrency",
    ] {
        srv.get(path).await.assert_status(StatusCode::UNAUTHORIZED);
    }
    srv.post("/api/enrichment/pause")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
