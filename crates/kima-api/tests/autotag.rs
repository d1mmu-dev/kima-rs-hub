mod common;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_queues_and_sets_state(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let (s, t, _) = setup(&pool).await;
    for queued in [1, 0] {
        let r = s
            .post("/api/library/autotag/search")
            .authorization_bearer(&t)
            .json(&json!({"albumIds":[a.album,a.album]}))
            .await;
        r.assert_status(StatusCode::ACCEPTED);
        r.assert_json(&json!({"queued":queued}));
    }
    let state: String = sqlx::query_scalar("SELECT state FROM autotag_albums WHERE album_id=$1")
        .bind(a.album)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    let payload: Value = sqlx::query_scalar("SELECT payload FROM jobs WHERE queue='autotag'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload, json!({"kind":"search","albumId":a.album}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_all_manual_validation_and_identify_switch(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let b = seed(&pool, Seed::new("Beta", "Second", None, "Two")).await;
    sqlx::query("UPDATE albums SET release_mbid=$2 WHERE id=$1")
        .bind(b.album)
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    let (s, t, _) = setup(&pool).await;
    for body in [
        json!({}),
        json!({"all":false}),
        json!({"albumIds":[]}),
        json!({"all":true,"albumIds":[a.album]}),
        json!({"albumIds":[a.album],"query":{"artist":"","album":"x"}}),
        json!({"albumIds":[a.album,b.album],"releaseMbid":Uuid::now_v7()}),
        json!({"all":true,"query":{"artist":"A","album":"B"}}),
        json!({"albumIds":[a.album],"releaseMbid":"nope"}),
        json!({"albumIds":[a.album],"releaseMbid":Uuid::now_v7(),"query":{"artist":"A","album":"B"}}),
        json!({"albumIds":[a.album,"nope"]}),
        json!({"albumIds":[a.album],"unknown":true}),
    ] {
        s.post("/api/library/autotag/search")
            .authorization_bearer(&t)
            .json(&body)
            .await
            .assert_status_bad_request();
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let r = s
        .post("/api/library/autotag/search")
        .authorization_bearer(&t)
        .json(&json!({"all":true}))
        .await;
    r.assert_status(StatusCode::ACCEPTED);
    r.assert_json(&json!({"queued":1}));
    let release = Uuid::now_v7();
    for _ in 0..2 {
        s.post("/api/library/autotag/search").authorization_bearer(&t).json(&json!({"albumIds":[a.album],"releaseMbid":format!("https://musicbrainz.org/release/{release}")})).await.assert_json(&json!({"queued":1}));
    }
    let mut state = common::state(pool.clone());
    state.identify = kima_core::identify::IdentifySwitch::new(true, true);
    let token = t;
    let off = TestServer::new(kima_api::router(state)).unwrap();
    let r = off
        .post("/api/library/autotag/search")
        .authorization_bearer(&token)
        .json(&json!({"albumIds":[a.album]}))
        .await;
    r.assert_status(StatusCode::CONFLICT);
    r.assert_json(&json!({"error":"Outbound lookups are turned off"}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apply_202_then_job_done_db_target(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let c = db::store_search(&pool, a.album, &[candidate(a.track, "Renamed", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = common::state(pool.clone()).with_library_root(root.path().to_path_buf());
    let ctx = kima_library::scan::LibraryCtx {
        pool: pool.clone(),
        root: state.library_root.clone().unwrap(),
        events: state.events.clone(),
        data_dir: root.path().to_path_buf(),
        ignore: state.library.ignore.clone(),
    };
    let (_, t) = common::user_with_token(&state, "admin").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    let r = s
        .post(&format!("/api/library/autotag/albums/{}/apply", a.album))
        .authorization_bearer(&t)
        .json(&json!({"candidateId":c,"target":"db"}))
        .await;
    r.assert_status(StatusCode::ACCEPTED);
    let v: Value = r.json();
    let run: Uuid = serde_json::from_value(v["runId"].clone()).unwrap();
    let job: Value = sqlx::query_scalar("SELECT payload FROM jobs WHERE id=$1")
        .bind(serde_json::from_value::<Uuid>(v["jobId"].clone()).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(job, json!({"kind":"autotag-apply","runId":run}));
    kima_library::autotag::apply::run_job(
        &ctx,
        run,
        &tokio_util::sync::CancellationToken::new(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        db::get_run(&pool, run).await.unwrap().unwrap().state,
        "done"
    );
    assert_eq!(
        db::local_album(&pool, a.album)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Renamed"
    );
    for endpoint in ["search", &format!("albums/{}/skip", a.album)] {
        let r = s
            .post(&format!("/api/library/autotag/{endpoint}"))
            .authorization_bearer(&t)
            .json(&json!({"albumIds":[a.album]}))
            .await;
        if endpoint == "search" {
            r.assert_json(&json!({"queued":0}));
        } else {
            r.assert_status(StatusCode::CONFLICT);
        }
    }
    let r = s
        .post(&format!("/api/library/autotag/albums/{}/apply", a.album))
        .authorization_bearer(&t)
        .json(&json!({"candidateId":c,"target":"db"}))
        .await;
    r.assert_status_bad_request();
    r.assert_json(&json!({"error":kima_library::autotag::APPLIED_MATCH_EXISTS}));
    let r = s
        .post(&format!("/api/library/autotag/runs/{run}/undo"))
        .authorization_bearer(&t)
        .await;
    r.assert_status(StatusCode::ACCEPTED);
    kima_library::autotag::undo::run_job(
        &ctx,
        run,
        &tokio_util::sync::CancellationToken::new(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        db::get_run(&pool, run).await.unwrap().unwrap().state,
        "undone"
    );
    assert_eq!(
        db::local_album(&pool, a.album)
            .await
            .unwrap()
            .unwrap()
            .title,
        "First"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apply_conflict_busy_and_unavailable(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let b = seed(&pool, Seed::new("Alpha", "Taken", Some(2001), "Two")).await;
    let c = db::store_search(&pool, a.album, &[candidate(a.track, "Taken", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = common::state(pool.clone()).with_library_root(root.path().to_path_buf());
    let (_, t) = common::user_with_token(&state, "admin").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    let path = format!("/api/library/autotag/albums/{}/apply", a.album);
    let r = s
        .post(&path)
        .authorization_bearer(&t)
        .json(&json!({"candidateId":c,"target":"db"}))
        .await;
    r.assert_status(StatusCode::CONFLICT);
    let v: Value = r.json();
    assert_eq!(v["conflictAlbumId"], json!(b.album));
    assert!(v["error"].as_str().unwrap().contains("Taken"));
    let r = s
        .post(&path)
        .authorization_bearer(&t)
        .json(&json!({"candidateId":c,"target":"copy"}))
        .await;
    r.assert_status_bad_request();
    r.assert_json(&json!({"error":kima_library::autotag::COPY_FIRST}));
    sqlx::query("INSERT INTO autotag_runs (id,album_id,target,reorganize,state) VALUES ($1,$2,'db',false,'queued')").bind(Uuid::now_v7()).bind(a.album).execute(&pool).await.unwrap();
    let r = s
        .post(&path)
        .authorization_bearer(&t)
        .json(&json!({"candidateId":c,"target":"db"}))
        .await;
    r.assert_status(StatusCode::CONFLICT);
    r.assert_json(&json!({"error":kima_library::autotag::apply::ALBUM_BUSY}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn skip_sets_state_and_preserves_protected_album(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let (s, t, _) = setup(&pool).await;
    let path = format!("/api/library/autotag/albums/{}/skip", a.album);
    s.post(&path)
        .authorization_bearer(&t)
        .await
        .assert_json(&json!({"state":"skipped"}));
    assert_eq!(
        get(&s, &t, &format!("albums/{}", a.album)).await["state"],
        "skipped"
    );
    sqlx::query("INSERT INTO autotag_runs (id,album_id,target,reorganize,state,album_before,error) VALUES ($1,$2,'db',false,'undo_failed','{}','keep me')").bind(Uuid::now_v7()).bind(a.album).execute(&pool).await.unwrap();
    db::set_state(&pool, a.album, db::AlbumState::Failed, Some("keep me"))
        .await
        .unwrap();
    s.post(&path)
        .authorization_bearer(&t)
        .await
        .assert_status(StatusCode::CONFLICT);
    let v = get(&s, &t, &format!("albums/{}", a.album)).await;
    assert_eq!(v["state"], "failed");
    assert_eq!(v["error"], "keep me");
    s.post("/api/library/autotag/albums/nope/skip")
        .authorization_bearer(&t)
        .await
        .assert_status_not_found();
    s.post(&format!(
        "/api/library/autotag/albums/{}/skip",
        Uuid::now_v7()
    ))
    .authorization_bearer(&t)
    .await
    .assert_status_not_found();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_202_and_state_conflicts(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let root = tempfile::tempdir().unwrap();
    let state = common::state(pool.clone()).with_library_root(root.path().to_path_buf());
    let (_, t) = common::user_with_token(&state, "admin").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    for state in [
        "queued",
        "writing",
        "organizing",
        "undoing",
        "undone",
        "failed",
        "done",
        "undo_failed",
    ] {
        let run = Uuid::now_v7();
        let c = db::store_search(&pool, a.album, &[candidate(a.track, "First", 0.01)])
            .await
            .unwrap()
            .unwrap();
        let applied = kima_ident::autotag::start::applied_values(
            &db::candidate(&pool, c).await.unwrap().unwrap(),
            &db::local_album(&pool, a.album).await.unwrap().unwrap(),
            false,
        )
        .unwrap();
        sqlx::query("INSERT INTO autotag_runs (id,album_id,target,reorganize,state,applied) VALUES ($1,$2,'db',false,$3,$4)").bind(run).bind(a.album).bind(state).bind(serde_json::to_value(applied).unwrap()).execute(&pool).await.unwrap();
        let r = s
            .post(&format!("/api/library/autotag/runs/{run}/undo"))
            .authorization_bearer(&t)
            .await;
        if ["done", "undo_failed"].contains(&state) {
            r.assert_status(StatusCode::ACCEPTED);
            let v: Value = r.json();
            assert!(v["jobId"].is_string());
            assert_eq!(
                db::get_run(&pool, run).await.unwrap().unwrap().state,
                "undoing"
            );
            let r = s
                .post(&format!("/api/library/autotag/runs/{run}/undo"))
                .authorization_bearer(&t)
                .await;
            r.assert_status(StatusCode::CONFLICT);
        } else {
            r.assert_status(StatusCode::CONFLICT);
        }
        sqlx::query("DELETE FROM autotag_runs WHERE id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
    }
    s.post("/api/library/autotag/runs/nope/undo")
        .authorization_bearer(&t)
        .await
        .assert_status_not_found();
    s.post(&format!(
        "/api/library/autotag/runs/{}/undo",
        Uuid::now_v7()
    ))
    .authorization_bearer(&t)
    .await
    .assert_status_not_found();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_collision_has_album_id_and_preserves_failed_run(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let c = db::store_search(&pool, a.album, &[candidate(a.track, "Renamed", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = common::state(pool.clone()).with_library_root(root.path().to_path_buf());
    let ctx = kima_library::scan::LibraryCtx {
        pool: pool.clone(),
        root: state.library_root.clone().unwrap(),
        events: state.events.clone(),
        data_dir: root.path().to_path_buf(),
        ignore: state.library.ignore.clone(),
    };
    let (_, t) = common::user_with_token(&state, "admin").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    let r = s
        .post(&format!("/api/library/autotag/albums/{}/apply", a.album))
        .authorization_bearer(&t)
        .json(&json!({"candidateId":c,"target":"db"}))
        .await;
    r.assert_status(StatusCode::ACCEPTED);
    let v: Value = r.json();
    let run: Uuid = serde_json::from_value(v["runId"].clone()).unwrap();
    kima_library::autotag::apply::run_job(
        &ctx,
        run,
        &tokio_util::sync::CancellationToken::new(),
        true,
    )
    .await
    .unwrap();
    let b = seed(&pool, Seed::new("Alpha", "First", Some(2001), "Other")).await;
    sqlx::query("UPDATE autotag_runs SET state='undo_failed',error='keep original' WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    let r = s
        .post(&format!("/api/library/autotag/runs/{run}/undo"))
        .authorization_bearer(&t)
        .await;
    r.assert_status(StatusCode::CONFLICT);
    let v: Value = r.json();
    assert_eq!(v["conflictAlbumId"], json!(b.album));
    let run = db::get_run(&pool, run).await.unwrap().unwrap();
    assert_eq!(run.state, "undo_failed");
    assert_eq!(run.error.as_deref(), Some("keep original"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_enqueue_failure_is_atomic(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let c = db::store_search(&pool, a.album, &[candidate(a.track, "First", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let applied = kima_ident::autotag::start::applied_values(
        &db::candidate(&pool, c).await.unwrap().unwrap(),
        &db::local_album(&pool, a.album).await.unwrap().unwrap(),
        false,
    )
    .unwrap();
    let run = Uuid::now_v7();
    sqlx::query("INSERT INTO autotag_runs (id,album_id,target,reorganize,state,applied,error,summary) VALUES ($1,$2,'db',false,'undo_failed',$3,'original','{\"needsUndo\":true}')").bind(run).bind(a.album).bind(serde_json::to_value(applied).unwrap()).execute(&pool).await.unwrap();
    sqlx::query(
        "ALTER TABLE jobs ADD CONSTRAINT refuse_undo CHECK (payload->>'kind' <> 'autotag-undo')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = common::state(pool.clone()).with_library_root(root.path().to_path_buf());
    let (_, t) = common::user_with_token(&state, "admin").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    let before = db::get_run(&pool, run).await.unwrap().unwrap();
    s.post(&format!("/api/library/autotag/runs/{run}/undo"))
        .authorization_bearer(&t)
        .await
        .assert_status_internal_server_error();
    assert_eq!(db::get_run(&pool, run).await.unwrap().unwrap(), before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mutation_auth_input_and_search_protections(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let b = seed(&pool, Seed::new("Beta", "Second", Some(2001), "Two")).await;
    let c = db::store_search(&pool, a.album, &[candidate(a.track, "First", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let foreign = db::store_search(&pool, b.album, &[candidate(b.track, "Second", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = common::state(pool.clone()).with_library_root(root.path().to_path_buf());
    let (_, t) = common::user_with_token(&state, "admin").await;
    let (_, u) = common::user_with_token(&state, "user").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    let path = format!("/api/library/autotag/albums/{}/apply", a.album);
    for p in [
        "/api/library/autotag/search".into(),
        path.clone(),
        format!("/api/library/autotag/albums/{}/skip", a.album),
        format!("/api/library/autotag/runs/{}/undo", Uuid::now_v7()),
    ] {
        s.post(&p)
            .authorization_bearer(&u)
            .json(&json!({}))
            .await
            .assert_status(StatusCode::FORBIDDEN);
        s.post(&p)
            .json(&json!({}))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    for body in [
        json!({}),
        json!({"candidateId":c,"target":"bogus"}),
        json!({"candidateId":c,"target":"db","reorganize":true}),
        json!({"candidateId":c,"target":"db","mbidsOnly":"yes"}),
        json!({"candidateId":c,"target":"db","unknown":1}),
    ] {
        s.post(&path)
            .authorization_bearer(&t)
            .json(&body)
            .await
            .assert_status_bad_request();
    }
    for id in [
        "nope".into(),
        foreign.to_string(),
        Uuid::now_v7().to_string(),
    ] {
        s.post(&path)
            .authorization_bearer(&t)
            .json(&json!({"candidateId":id,"target":"db"}))
            .await
            .assert_status_not_found();
    }
    s.post("/api/library/autotag/albums/nope/apply")
        .authorization_bearer(&t)
        .json(&json!({"candidateId":c,"target":"db"}))
        .await
        .assert_status_not_found();
    let run = Uuid::now_v7();
    for run_state in [
        "queued",
        "writing",
        "organizing",
        "undoing",
        "failed",
        "undo_failed",
    ] {
        sqlx::query("INSERT INTO autotag_runs (id,album_id,target,reorganize,state,album_before) VALUES ($1,$2,'db',false,$3,'{}') ON CONFLICT (id) DO UPDATE SET state=$3").bind(run).bind(a.album).bind(run_state).execute(&pool).await.unwrap();
        db::set_state(&pool, a.album, db::AlbumState::Failed, Some("keep me"))
            .await
            .unwrap();
        for body in [
            json!({"all":true}),
            json!({"albumIds":[a.album],"query":{"artist":"A","album":"B"}}),
        ] {
            let r = s
                .post("/api/library/autotag/search")
                .authorization_bearer(&t)
                .json(&body)
                .await;
            r.assert_status(StatusCode::ACCEPTED);
            // all may queue B on its first call, never A.
            let n: i64 =
                sqlx::query_scalar("SELECT count(*) FROM jobs WHERE payload->>'albumId'=$1")
                    .bind(a.album.to_string())
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(n, 0);
        }
        s.post(&format!("/api/library/autotag/albums/{}/skip", a.album))
            .authorization_bearer(&t)
            .await
            .assert_status(StatusCode::CONFLICT);
        let v = get(&s, &t, &format!("albums/{}", a.album)).await;
        assert_eq!(v["state"], "failed");
        assert_eq!(v["error"], "keep me");
        assert_eq!(v["candidates"][0]["id"], json!(c));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_latest_failed_written_single_connection_and_dedup(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let c = db::store_search(&pool, a.album, &[candidate(a.track, "First", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let applied = kima_ident::autotag::start::applied_values(
        &db::candidate(&pool, c).await.unwrap().unwrap(),
        &db::local_album(&pool, a.album).await.unwrap().unwrap(),
        true,
    )
    .unwrap();
    let old = Uuid::now_v7();
    let latest = Uuid::now_v7();
    for (id, state, date) in [
        (old, "done", "2026-01-01"),
        (latest, "failed", "2026-01-02"),
    ] {
        sqlx::query("INSERT INTO autotag_runs (id,album_id,target,reorganize,state,applied,album_before,created_at) VALUES ($1,$2,'db',false,$3,$4,NULL,$5::text::timestamptz)").bind(id).bind(a.album).bind(state).bind(serde_json::to_value(&applied).unwrap()).bind(date).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO autotag_journal (run_id,track_id,root,path,state,old_tags,new_tags) VALUES ($1,$2,'library','one.mp3','written','{}','{}')").bind(latest).bind(a.track).execute(&pool).await.unwrap();
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = common::state(single).with_library_root(root.path().to_path_buf());
    let (_, t) = common::user_with_token(&state, "admin").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    let r = s
        .post(&format!("/api/library/autotag/runs/{old}/undo"))
        .authorization_bearer(&t)
        .await;
    r.assert_status(StatusCode::CONFLICT);
    r.assert_json(&json!({"error":kima_library::autotag::UNDO_NEWER_FIRST}));
    let path = format!("/api/library/autotag/runs/{latest}/undo");
    let request = || async { s.post(&path).authorization_bearer(&t).await };
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(request(), request())
    })
    .await
    .unwrap();
    let mut statuses = [a.status_code(), b.status_code()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::ACCEPTED, StatusCode::CONFLICT]);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE payload->>'runId'=$1")
        .bind(latest.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_db::{PgPool, autotag as db};
use serde_json::{Value, json};
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn candidate_order_uses_rounded_score_and_release_ties(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let mut values = Vec::new();
    // Deliberately reverse the tie-breaker order at storage time.
    for (title, distance, status, format, country, date) in [
        (
            "unofficial",
            0.0101,
            "Bootleg",
            "Digital Media",
            "XW",
            "1990",
        ),
        ("cd", 0.0102, "Official", "CD", "XW", "1990"),
        ("us", 0.0103, "Official", "Digital Media", "US", "1990"),
        ("later", 0.0104, "Official", "Digital Media", "XW", "2002"),
        ("first", 0.01049, "Official", "Digital Media", "XW", "2001"),
    ] {
        let mut c = candidate(a.track, title, distance);
        c.release["status"] = json!(status);
        c.release["media"][0]["format"] = json!(format);
        c.release["country"] = json!(country);
        c.release["date"] = json!(date);
        values.push(c);
    }
    db::store_search(&pool, a.album, &values).await.unwrap();
    let (s, t, _) = setup(&pool).await;
    let v = get(&s, &t, &format!("albums/{}", a.album)).await;
    let titles = v["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["first", "later", "us", "cd", "unofficial"]);
    // Selection follows the stored best id, not a silent re-score/re-selection.
    assert_eq!(v["selected"]["candidateId"], v["candidates"][4]["id"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_candidates_missing_album_and_foreign_candidate(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let b = seed(&pool, Seed::new("Beta", "Second", Some(2001), "Two")).await;
    let foreign = db::store_search(&pool, b.album, &[candidate(b.track, "Second", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let (s, t, _) = setup(&pool).await;
    let v = get(&s, &t, &format!("albums/{}", a.album)).await;
    assert_eq!(v["state"], "new");
    assert_eq!(v["selected"], Value::Null);
    assert_eq!(v["candidates"], json!([]));
    assert_eq!(v["conflict"], Value::Null);
    for id in [
        foreign.to_string(),
        "nope".to_string(),
        Uuid::now_v7().to_string(),
    ] {
        s.get(&format!(
            "/api/library/autotag/albums/{}?candidateId={id}",
            a.album
        ))
        .authorization_bearer(&t)
        .await
        .assert_status_not_found();
    }
    mark_missing(&pool, a.track).await;
    for id in [a.album, Uuid::now_v7()] {
        let r = s
            .get(&format!("/api/library/autotag/albums/{id}"))
            .authorization_bearer(&t)
            .await;
        r.assert_status_not_found();
        r.assert_json(&json!({"error":"Album not found"}));
    }
    s.get(&format!("/api/library/autotag/runs/{}", Uuid::now_v7()))
        .authorization_bearer(&t)
        .await
        .assert_status_not_found();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn selected_exposes_missing_extra_and_failed_state(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let extra = seed(
        &pool,
        Seed::new("Alpha", "First", Some(2001), "Extra").at(1, 2),
    )
    .await;
    let mut c = candidate(a.track, "First", 0.01);
    c.extra_tracks = json!([extra.track]);
    c.missing_tracks = json!([{"medium":2,"position":1,"title":"Missing","length":123000}]);
    db::store_search(&pool, a.album, &[c.clone()])
        .await
        .unwrap();
    db::set_state(&pool, a.album, db::AlbumState::Failed, Some("needs undo"))
        .await
        .unwrap();
    let (s, t, _) = setup(&pool).await;
    let v = get(&s, &t, &format!("albums/{}", a.album)).await;
    assert_eq!(v["state"], "failed");
    assert_eq!(v["error"], "needs undo");
    assert_eq!(v["selected"]["missing"], c.missing_tracks);
    assert_eq!(v["selected"]["extra"], c.extra_tracks);
    assert!(
        v["selected"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["trackId"] == json!(extra.track))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn run_paging_defaults_caps_and_stable_order(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let mut ids = Vec::new();
    for _ in 0..23 {
        let id = Uuid::now_v7();
        ids.push(id);
        sqlx::query("INSERT INTO autotag_runs (id,album_id,target,reorganize,state,created_at) VALUES ($1,$2,'db',false,'done','2026-01-01T00:00:00Z')").bind(id).bind(a.album).execute(&pool).await.unwrap();
    }
    ids.sort();
    ids.reverse();
    let run = ids[0];
    let organizer = Uuid::now_v7();
    sqlx::query("INSERT INTO organizer_runs (id,mode,state,source_root,target_root) VALUES ($1,'move','done','/music','/music')").bind(organizer).execute(&pool).await.unwrap();
    sqlx::query("UPDATE autotag_runs SET organizer_run_id=$1,reorganize=true WHERE id=$2")
        .bind(organizer)
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let row = db::journal::NewJournalRow {
        track_id: None,
        root: "library".into(),
        path: "deleted.mp3".into(),
        before_sha: None,
        audio_md5: None,
        old_tags: json!({}),
        new_tags: json!({}),
        id3_version: None,
    };
    db::journal::insert_planned(&mut tx, run, &vec![row; 203])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (s, t, _) = setup(&pool).await;
    let v = get(&s, &t, "runs").await;
    assert_eq!(v["runs"].as_array().unwrap().len(), 20);
    assert_eq!(v["runs"][0]["id"], json!(ids[0]));
    let v = get(&s, &t, "runs?offset=1&limit=1").await;
    assert_eq!(v["runs"][0]["id"], json!(ids[1]));
    for query in ["", "?limit=999"] {
        let v = get(&s, &t, &format!("runs/{run}{query}")).await;
        assert_eq!(v["organizerRunId"], json!(organizer));
        assert_eq!(v["total"], 203);
        assert_eq!(v["rows"].as_array().unwrap().len(), 200);
        assert_eq!(v["rows"][0]["trackId"], Value::Null);
    }
    let v = get(&s, &t, &format!("runs/{run}?offset=200&limit=1")).await;
    assert_eq!(v["total"], 203);
    assert_eq!(v["rows"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn capabilities_copy_and_identify_switch(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO organizer_runs (id,mode,state,source_root,target_root) VALUES ($1,'copy','done',$2,$3)")
        .bind(id).bind(root.path().to_str().unwrap()).bind(target.path().to_str().unwrap()).execute(&pool).await.unwrap();
    let mut state = common::state(pool.clone()).with_library_root(root.path().to_path_buf());
    state.identify = kima_core::identify::IdentifySwitch::new(true, true);
    let (_, token) = common::user_with_token(&state, "admin").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    let v = get(&s, &token, "capabilities").await;
    assert_eq!(v["identifyEnabled"], false);
    assert_eq!(
        v["copy"],
        json!({"available":true,"reason":null,"copyRunId":id})
    );
    sqlx::query("UPDATE organizer_runs SET switched_at=now() WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&s, &token, "capabilities").await["copy"]["available"],
        false
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
}

async fn setup(pool: &PgPool) -> (TestServer, String, String) {
    let state = common::state(pool.clone());
    let (_, admin) = common::user_with_token(&state, "admin").await;
    let (_, user) = common::user_with_token(&state, "user").await;
    (
        TestServer::new(kima_api::router(state)).unwrap(),
        admin,
        user,
    )
}
async fn get(s: &TestServer, token: &str, path: &str) -> Value {
    let r = s
        .get(&format!("/api/library/autotag/{path}"))
        .authorization_bearer(token)
        .await;
    r.assert_status_ok();
    r.json()
}
fn candidate(track: Uuid, title: &str, distance: f64) -> db::NewCandidate {
    let release = Uuid::now_v7();
    let group = Uuid::now_v7();
    let recording = Uuid::now_v7();
    let rt = Uuid::now_v7();
    db::NewCandidate {
        release_mbid: release,
        release_group_mbid: group,
        origin: db::Origin::Search,
        distance,
        recommendation: db::Recommendation::Strong,
        penalties: json!({"album":0.01}),
        mapping: json!([{"trackId":track,"medium":1,"position":1,"releaseTrackMbid":rt,"distance":0.01}]),
        extra_tracks: json!([]),
        missing_tracks: json!([]),
        release: json!({"id":release,"title":title,"status":"Official","date":"2001-02-03","country":"XW","barcode":"123","label-info":[{"label":{"name":"Label"},"catalog-number":"CAT"}],"artist-credit":[{"name":"Alpha","artist":{"id":Uuid::now_v7(),"name":"Alpha","sort-name":"Alpha"}}],"media":[{"position":1,"format":"CD","track-count":1,"tracks":[{"id":rt,"position":1,"title":"New track","length":180000,"recording":{"id":recording,"title":"New track"}}]}]}),
        release_group: json!({"id":group,"first-release-date":"2001"}),
    }
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn capabilities_shapes(pool: PgPool) {
    let root = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let state = common::state(pool).with_library_root(root.path().to_path_buf());
    let (_, admin) = common::user_with_token(&state, "admin").await;
    let s = TestServer::new(kima_api::router(state)).unwrap();
    let v = get(&s, &admin, "capabilities").await;
    assert_eq!(
        v,
        json!({"identifyEnabled":true,"files":{"available":false,"reason":kima_library::autotag::FILES_READ_ONLY},"copy":{"available":false,"reason":kima_library::autotag::COPY_FIRST,"copyRunId":null},"db":{"available":true}})
    );
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        get(&s, &admin, "capabilities").await["files"],
        json!({"available":true,"reason":null})
    );
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn albums_list_new_state_and_filters(pool: PgPool) {
    let a = seed(&pool, Seed::new("Beyoncé", "Été", Some(2001), "One")).await;
    let b = seed(&pool, Seed::new("Other", "Second", None, "Two")).await;
    let gone = seed(&pool, Seed::new("Gone", "Missing", None, "Three")).await;
    mark_missing(&pool, gone.track).await;
    db::store_search(&pool, b.album, &[candidate(b.track, "Second", 0.01)])
        .await
        .unwrap();
    db::list_albums(&pool, &db::AlbumFilter::default(), 0, 50)
        .await
        .unwrap();
    let (s, t, _) = setup(&pool).await;
    let v = get(&s, &t, "albums?state=new&q=BEYONCE").await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["albums"][0]["albumId"], json!(a.album));
    assert_eq!(v["albums"][0]["state"], "new");
    assert_eq!(v["albums"][0]["trackCount"], 1);
    assert_eq!(v["albums"][0]["issues"], json!([]));
    assert_eq!(
        get(
            &s,
            &t,
            "albums?state=proposed&recommendation=strong&q=second"
        )
        .await["total"],
        1
    );
    assert_eq!(get(&s, &t, "albums?recommendation=none").await["total"], 0);
    let v = get(&s, &t, "albums?state=proposed").await;
    assert_eq!(v["albums"][0]["distance"], 0.01);
    assert!(v["albums"][0]["releaseMbid"].is_string());
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn albums_list_paging_limit_capped(pool: PgPool) {
    for i in 0..203 {
        seed(
            &pool,
            Seed::new("Alpha", &format!("Album {i:03}"), Some(2001), "One"),
        )
        .await;
    }
    let (s, t, _) = setup(&pool).await;
    assert_eq!(
        get(&s, &t, "albums").await["albums"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
    let v = get(&s, &t, "albums?offset=1&limit=1000").await;
    assert_eq!(v["total"], 203);
    assert_eq!(v["albums"].as_array().unwrap().len(), 200);
    assert_eq!(v["albums"][0]["album"], "Album 001");
    let v = get(&s, &t, "albums?offset=999").await;
    assert_eq!(v["total"], 203);
    assert_eq!(v["albums"], json!([]));
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_detail_shape_conflict_stale_and_selection(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let collision = seed(&pool, Seed::new("Alpha", "Renamed", Some(2001), "Other")).await;
    let best = db::store_search(
        &pool,
        a.album,
        &[
            candidate(a.track, "Renamed", 0.01),
            candidate(a.track, "Alternative", 0.02),
        ],
    )
    .await
    .unwrap()
    .unwrap();
    sqlx::query("UPDATE autotag_candidates SET fetched_at=now()-interval '31 days' WHERE id=$1")
        .bind(best)
        .execute(&pool)
        .await
        .unwrap();
    let (s, t, _) = setup(&pool).await;
    let v = get(&s, &t, &format!("albums/{}", a.album)).await;
    assert_eq!(v["album"]["id"], json!(a.album));
    assert_eq!(v["album"]["tracks"][0]["duration"], 180);
    assert_eq!(
        v["conflict"],
        json!({"albumId":collision.album,"artist":"Alpha","title":"Renamed","year":2001})
    );
    assert_eq!(v["selected"]["candidateId"], json!(best));
    assert_eq!(v["userMbidKept"], false);
    let c = &v["candidates"][0];
    let keys = [
        "id",
        "releaseMbid",
        "releaseGroupMbid",
        "title",
        "artist",
        "date",
        "country",
        "media",
        "label",
        "catalogNumber",
        "barcode",
        "trackCount",
        "distance",
        "recommendation",
        "penalties",
        "origin",
        "fetchedAt",
        "stale",
    ];
    assert_eq!(c.as_object().unwrap().len(), keys.len());
    for k in keys {
        assert!(c.get(k).is_some(), "{k}");
    }
    assert_eq!(c["stale"], true);
    assert_eq!(v["candidates"][1]["stale"], false);
    assert_eq!(c["label"], "Label");
    assert_eq!(c["catalogNumber"], "CAT");
    let local = db::local_album(&pool, a.album).await.unwrap().unwrap();
    let row = db::candidate(&pool, best).await.unwrap().unwrap();
    let release = serde_json::from_value(row.release).unwrap();
    let group = serde_json::from_value(row.release_group).unwrap();
    let mapping = kima_ident::autotag::score::Mapping {
        pairs: serde_json::from_value(row.mapping).unwrap(),
        ..Default::default()
    };
    assert_eq!(
        v["selected"]["changes"],
        serde_json::to_value(kima_ident::autotag::proposal::changes(
            &local,
            &kima_ident::autotag::proposal::applied(&release, &group, &mapping)
        ))
        .unwrap()
    );
    assert_eq!(
        v["selected"]["mapping"][0]["local"],
        json!({"disc":1,"trackNo":1,"title":"One","duration":180})
    );
    assert_eq!(v["selected"]["mapping"][0]["release"]["title"], "New track");
    let other = v["candidates"][1]["id"].as_str().unwrap();
    sqlx::query("UPDATE autotag_albums SET chosen_candidate=$2 WHERE album_id=$1")
        .bind(a.album)
        .bind(other.parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&s, &t, &format!("albums/{}", a.album)).await["selected"]["candidateId"],
        other
    );
    assert_eq!(
        get(&s, &t, &format!("albums/{}?candidateId={best}", a.album)).await["selected"]["candidateId"],
        json!(best)
    );
    sqlx::query("UPDATE albums SET release_mbid=$2,release_mbid_source='user' WHERE id=$1")
        .bind(a.album)
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&s, &t, &format!("albums/{}", a.album)).await["userMbidKept"],
        true
    );
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn attention_report_for_includes_folder_siblings(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "A", Some(2001), "One")).await;
    let b = seed(&pool, Seed::new("Beta", "B", Some(2001), "Two")).await;
    sqlx::query(
        "UPDATE tracks SET path=CASE WHEN id=$1 THEN 'shared/a.mp3' ELSE 'shared/b.mp3' END",
    )
    .bind(a.track)
    .execute(&pool)
    .await
    .unwrap();
    let (s, t, _) = setup(&pool).await;
    let v = get(&s, &t, "albums?q=Alpha").await;
    assert_eq!(v["albums"][0]["issues"], json!(["folderSplit"]));
    let report = kima_db::attention::report_for(&pool, &[a.album])
        .await
        .unwrap();
    assert_eq!(report.len(), 1);
    assert!(!report.contains_key(&b.album));
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn non_admin_403_and_non_uuid_404(pool: PgPool) {
    let (s, t, u) = setup(&pool).await;
    for p in ["capabilities", "albums", "albums/nope", "runs", "runs/nope"] {
        s.get(&format!("/api/library/autotag/{p}"))
            .authorization_bearer(&u)
            .await
            .assert_status(StatusCode::FORBIDDEN);
        s.get(&format!("/api/library/autotag/{p}"))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    for (p, error) in [
        ("albums/nope", "Album not found"),
        ("runs/nope", "Run not found"),
    ] {
        let r = s
            .get(&format!("/api/library/autotag/{p}"))
            .authorization_bearer(&t)
            .await;
        r.assert_status_not_found();
        r.assert_json(&json!({"error":error}));
    }
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn runs_list_and_detail(pool: PgPool) {
    let a = seed(&pool, Seed::new("Alpha", "First", Some(2001), "One")).await;
    let c = db::store_search(&pool, a.album, &[candidate(a.track, "First", 0.01)])
        .await
        .unwrap()
        .unwrap();
    let run = Uuid::now_v7();
    sqlx::query("INSERT INTO autotag_runs (id,album_id,candidate_id,target,reorganize,state,summary,error) VALUES ($1,$2,$3,'db',false,'failed',$4,'failure')").bind(run).bind(a.album).bind(c).bind(json!({"needsUndo":true})).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    db::journal::insert_planned(
        &mut tx,
        run,
        &[db::journal::NewJournalRow {
            track_id: Some(a.track),
            root: "library".into(),
            path: "one.mp3".into(),
            before_sha: None,
            audio_md5: None,
            old_tags: json!({"TITLE":["old"],"ARTIST":["same"],"REMOVED":["x"]}),
            new_tags: json!({"TITLE":["new"],"ARTIST":["same"],"ADDED":["y"]}),
            id3_version: None,
        }],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (s, t, _) = setup(&pool).await;
    let v = get(&s, &t, "runs").await;
    assert_eq!(v.as_object().unwrap().len(), 1);
    assert_eq!(v["runs"].as_array().unwrap().len(), 1);
    assert_eq!(v["runs"][0]["album"], "First");
    assert_eq!(v["runs"][0]["summary"]["needsUndo"], true);
    let v = get(&s, &t, &format!("runs/{run}")).await;
    assert_eq!(v["id"], json!(run));
    assert_eq!(v["total"], 1);
    assert_eq!(v["rows"][0]["trackId"], json!(a.track));
    assert_eq!(
        v["rows"][0]["changes"],
        json!([{"field":"ADDED","old":null,"new":["y"]},{"field":"REMOVED","old":["x"],"new":null},{"field":"TITLE","old":["old"],"new":["new"]}])
    );
    assert_eq!(
        get(&s, &t, &format!("runs/{run}?offset=9&limit=1")).await["total"],
        1
    );
    assert_eq!(get(&s, &t, "runs?offset=9").await["runs"], json!([]));
}
