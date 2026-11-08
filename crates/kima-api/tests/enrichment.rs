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
            "moods",
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
    for step in ["moods", "similarity"] {
        assert_eq!(
            keys(&p[step]),
            sorted(&["total", "completed", "pending", "progress", "isBackground"]),
            "{step}"
        );
        assert_eq!(p[step]["isBackground"], true);
    }
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

// ---- POST /api/enrichment/rerun (M-SYNC Task 14; upstream enrichmentRerun.route.test.ts) ----

const SIX_STEPS: &str =
    "step must be one of: identification, artists, analysis, vectors, moods, similarity";

async fn rerun_as(srv: &TestServer, token: &str, body: Value) -> axum_test::TestResponse {
    srv.post("/api/enrichment/rerun")
        .authorization_bearer(token)
        .json(&body)
        .await
}

async fn enrich_jobs(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'enrich' AND state = 'queued'")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// An enriched artist: a bio, similar artists, `completed`, a MusicBrainz genre and one of the
/// person's own.
async fn enriched_artist(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO artists (id, name, name_key, bio, \"similar\", enrichment_status, \
         last_enriched_at, enrichment_failures, user_genres) \
         VALUES ($1, $2, lower($2), 'a bio', '[]'::jsonb, 'completed', now(), 2, ARRAY['mine'])",
    )
    .bind(id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
    let genre = Uuid::now_v7();
    sqlx::query("INSERT INTO genres (id, name, key) VALUES ($1, 'rock', $2)")
        .bind(genre)
        .bind(genre.to_string())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artist_genres (artist_id, genre_id, source, rank) VALUES ($1, $2, 'musicbrainz', 0)")
        .bind(id)
        .bind(genre)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn artist_state(
    pool: &PgPool,
    id: Uuid,
) -> (String, Option<String>, Option<Value>, i16, i64) {
    let r: (String, Option<String>, Option<Value>, i16) = sqlx::query_as(
        "SELECT enrichment_status, bio, \"similar\", enrichment_failures FROM artists WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    let genres: i64 = sqlx::query_scalar("SELECT count(*) FROM artist_genres WHERE artist_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    (r.0, r.1, r.2, r.3, genres)
}

/// A track identified by a search: its `recording` row `done`.
async fn identified_track(pool: &PgPool, n: i32) -> Uuid {
    let t = track(pool, n).await;
    sqlx::query("UPDATE tracks SET recording_mbid = $2, recording_source = 'search' WHERE id = $1")
        .bind(t)
        .bind(Uuid::new_v4())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, source, finished_at) \
         VALUES ($1, 'recording', 'done', 'search', now())",
    )
    .bind(t)
    .execute(pool)
    .await
    .unwrap();
    t
}

async fn recording_status(pool: &PgPool, t: Uuid) -> String {
    sqlx::query_scalar(
        "SELECT status FROM track_stages WHERE track_id = $1 AND stage = 'recording'",
    )
    .bind(t)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn re_runs_from_a_valid_step_and_starts_the_engine(pool: PgPool) {
    let (srv, admin, user) = server(pool.clone()).await;
    // The progress memo is dropped by the re-run, and the coordinator is kicked.
    let before = get(&srv, &user, "/api/enrichment/progress").await;
    assert_eq!(before["trackTags"]["total"], 0);
    track(&pool, 1).await;
    enriched_artist(&pool, "Low").await;
    assert_eq!(enrich_jobs(&pool).await, 0);

    let res = rerun_as(&srv, &admin, json!({"step": "identification"})).await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        body,
        // The track's own artist and "Low".
        json!({"artists": 2, "tracks": 0, "vectors": 0, "message": "Re-running from identification"})
    );
    assert_eq!(enrich_jobs(&pool).await, 1);
    let after = get(&srv, &user, "/api/enrichment/progress").await;
    assert_eq!(after["trackTags"]["total"], 1);

    let body: Value = rerun_as(&srv, &admin, json!({"step": "artists"}))
        .await
        .json();
    assert_eq!(body["message"], "Re-running from artists");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn names_the_six_steps_when_refusing_one(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    for step in [
        json!("everything"),
        json!(""),
        json!(null),
        json!(5),
        json!("toString"),
        json!("constructor"),
        json!("Identification"),
    ] {
        let res = rerun_as(&srv, &admin, json!({ "step": step })).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        let body: Value = res.json();
        assert_eq!(body, json!({"error": SIX_STEPS}), "{step}");
    }
    // No step at all, and no JSON at all, are refused the same way.
    let res = rerun_as(&srv, &admin, json!({})).await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(res.json::<Value>(), json!({"error": SIX_STEPS}));
    let res = srv
        .post("/api/enrichment/rerun")
        .authorization_bearer(&admin)
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(res.json::<Value>(), json!({"error": SIX_STEPS}));
    assert_eq!(enrich_jobs(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refuses_a_member_re_running_nothing(pool: PgPool) {
    let (srv, _, user) = server(pool.clone()).await;
    let a = enriched_artist(&pool, "Low").await;

    let res = rerun_as(&srv, &user, json!({"step": "artists"})).await;

    res.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(artist_state(&pool, a).await.0, "completed");
    assert_eq!(enrich_jobs(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn needs_a_signed_in_user(pool: PgPool) {
    let (srv, _, _) = server(pool.clone()).await;
    let a = enriched_artist(&pool, "Low").await;

    let res = srv
        .post("/api/enrichment/rerun")
        .json(&json!({"step": "artists"}))
        .await;

    res.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(artist_state(&pool, a).await.0, "completed");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artists_step_resets_only_artists(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    let t = identified_track(&pool, 1).await;
    let a = enriched_artist(&pool, "Low").await;
    let various = enriched_artist(&pool, "Various Artists").await;

    let body: Value = rerun_as(&srv, &admin, json!({"step": "artists"}))
        .await
        .json();

    // Artists back to `pending`: no bio, no similar list, no failures, their MusicBrainz genres
    // gone and the person's own list kept; the track's answer and stage are untouched.
    // The track's own artist and "Low"; Various Artists is not counted.
    assert_eq!(body["artists"], 2);
    assert_eq!(
        artist_state(&pool, a).await,
        ("pending".into(), None, None, 0, 0)
    );
    let own: Option<Vec<String>> =
        sqlx::query_scalar("SELECT user_genres FROM artists WHERE id = $1")
            .bind(a)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(own, Some(vec!["mine".to_string()]));
    assert_eq!(artist_state(&pool, various).await.0, "completed");
    assert_eq!(recording_status(&pool, t).await, "done");
    let mbid: Option<Uuid> = sqlx::query_scalar("SELECT recording_mbid FROM tracks WHERE id = $1")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(mbid.is_some());
    assert_eq!(enrich_jobs(&pool).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn identification_step_equals_wipe_and_rerun(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    let seed_state = |pool: PgPool| async move {
        let t = identified_track(&pool, 1).await;
        let a = enriched_artist(&pool, &format!("Low {}", Uuid::new_v4())).await;
        (t, a)
    };
    let (t, a) = seed_state(pool.clone()).await;
    let by_wipe = post(&srv, &admin, "/api/enrichment/wipe-and-rerun").await;
    let wiped = (
        recording_status(&pool, t).await,
        artist_state(&pool, a).await,
    );

    // The same state again, then the same effect through the new route.
    sqlx::query("UPDATE track_stages SET status = 'done' WHERE track_id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET recording_mbid = $2 WHERE id = $1")
        .bind(t)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE artists SET enrichment_status = 'completed', bio = 'a bio'")
        .execute(&pool)
        .await
        .unwrap();
    let by_rerun: Value = rerun_as(&srv, &admin, json!({"step": "identification"}))
        .await
        .json();

    assert_eq!(by_rerun["artists"], by_wipe["artists"]);
    assert_eq!(by_rerun["tracks"], by_wipe["tracks"]);
    assert_eq!(by_rerun["vectors"], by_wipe["vectors"]);
    assert_eq!(wiped.0, "pending");
    assert_eq!(recording_status(&pool, t).await, "pending");
    assert_eq!(artist_state(&pool, a).await.0, "pending");
    assert_eq!(artist_state(&pool, a).await.1, None);
    assert_eq!(by_rerun["message"], "Re-running from identification");
}

/// A vector, scores and statistics, feel and a state, a stored analysis and its stage rows for `t`.
async fn vibe_rows(pool: &PgPool, t: Uuid) {
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding)
         VALUES ($1, 0, 'm', decode(repeat('ab', 32), 'hex'), 'p', ('[1' || repeat(',0', 511) || ']')::vector)",
    )
    .bind(t)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO track_analysis (track_id, file_generation) VALUES ($1, 0)")
        .bind(t)
        .execute(pool)
        .await
        .unwrap();
    for stage in ["analysis", "embedding"] {
        sqlx::query(
            "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, $2, 'done')
             ON CONFLICT (track_id, stage) DO UPDATE SET status = 'done'",
        )
        .bind(t)
        .bind(stage)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO matching_feel (track_id, embedded_at, probe, valence, arousal) SELECT track_id, created_at, 'p', 0.5, 0.5 FROM track_embeddings")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO matching_state (version, fingerprint, track_count, feel_mean_valence, feel_mean_arousal, feel_sd_valence, feel_sd_arousal, sound_spread, genre_spread, genre_weight, genres) VALUES ('v', 'f', 1, 0.5, 0.5, 0.1, 0.1, 0.5, 0.5, 0.5, '{}')")
        .execute(pool)
        .await
        .unwrap();
}

async fn tables(pool: &PgPool) -> Vec<i64> {
    let mut out = vec![];
    for table in [
        "track_embeddings",
        "track_analysis",
        "matching_feel",
        "matching_state",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(pool)
            .await
            .unwrap();
        out.push(n);
    }
    out
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_m5_steps_re_run_what_the_matrix_says(pool: PgPool) {
    let (srv, admin, _) = server(pool.clone()).await;
    let t = identified_track(&pool, 1).await;
    let a = enriched_artist(&pool, "Low").await;

    // (step, name in the reply, [vectors, analysis, feel, state] left afterwards, vectors reported)
    let cases: [(&str, &str, [i64; 4], i64); 4] = [
        ("similarity", "similarity", [1, 1, 0, 0], 0),
        ("moods", "moods", [1, 1, 1, 1], 0),
        ("vectors", "sound vectors", [0, 1, 0, 0], 1),
        ("analysis", "audio analysis", [0, 0, 0, 0], 1),
    ];
    for (step, name, left, vectors) in cases {
        sqlx::query("DELETE FROM track_embeddings")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM track_analysis")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM matching_state")
            .execute(&pool)
            .await
            .unwrap();
        vibe_rows(&pool, t).await;
        let res = rerun_as(&srv, &admin, json!({ "step": step })).await;
        res.assert_status_ok();
        let body: Value = res.json();
        assert_eq!(body["message"], format!("Re-running from {name}"), "{step}");
        assert_eq!(body["vectors"], vectors, "{step}");
        assert_eq!(tables(&pool).await, left, "{step}");
    }
    // None of them touches identification or the artists.
    assert_eq!(recording_status(&pool, t).await, "done");
    assert_eq!(artist_state(&pool, a).await.0, "completed");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reports_a_failed_re_run_as_a_500_and_starts_nothing(pool: PgPool) {
    let (srv, admin, user) = server(pool.clone()).await;
    let a = enriched_artist(&pool, "Low").await;
    // The progress memo exists, so a refused re-run must leave it be.
    get(&srv, &user, "/api/enrichment/progress").await;
    sqlx::query("ALTER TABLE artist_genres RENAME TO artist_genres_gone")
        .execute(&pool)
        .await
        .unwrap();

    let res = rerun_as(&srv, &admin, json!({"step": "artists"})).await;

    res.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "Failed to re-run enrichment"})
    );
    assert_eq!(enrich_jobs(&pool).await, 0);
    // Nothing was half done: the reset is one transaction.
    let status: String = sqlx::query_scalar("SELECT enrichment_status FROM artists WHERE id = $1")
        .bind(a)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "completed");
}
