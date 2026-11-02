mod common;

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
