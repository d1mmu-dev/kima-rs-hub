mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, seed};
use kima_api::AppState;
use kima_db::PgPool;
use kima_library::scan::{LibraryCtx, LibraryRoot};
use serde_json::{Value, json};

struct Setup {
    server: TestServer,
    state: AppState,
    ctx: LibraryCtx,
    admin: String,
    user: String,
    root: PathBuf,
    target: PathBuf,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

/// Two seeded tracks with dummy files at their paths, an admin and a plain user.
async fn setup(pool: PgPool, with_target: bool) -> Setup {
    let music = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let root = music.path().canonicalize().unwrap();
    let target = out.path().canonicalize().unwrap().join("organized");
    let shared = LibraryRoot::new(root.clone());
    let state = common::state(pool.clone())
        .with_shared_library_root(shared.clone())
        .with_organize_target(with_target.then(|| target.clone()));
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let (_, user) = common::user_with_token(&state, "bob").await;
    for (title, no) in [("One", 1), ("Two", 2)] {
        seed(
            &pool,
            Seed::new("Alpha", "First", Some(2001), title).at(1, no),
        )
        .await;
        let rel = format!("Alpha/First/1-{no} {title}.mp3");
        std::fs::create_dir_all(root.join("Alpha/First")).unwrap();
        std::fs::write(root.join(&rel), format!("audio {title}")).unwrap();
    }
    let ctx = LibraryCtx {
        pool: pool.clone(),
        root: shared,
        events: state.events.clone(),
        data_dir: root.join(".kima-data"),
        ignore: state.library.ignore.clone(),
    };
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    Setup {
        server,
        state,
        ctx,
        admin,
        user,
        root,
        target,
        _dirs: (music, out),
    }
}

/// Runs every queued `library` job, as the worker would.
async fn run_jobs(ctx: &LibraryCtx) {
    loop {
        let jobs = kima_jobs::queue::claim(&ctx.pool, "library", 10, Duration::from_secs(60))
            .await
            .unwrap();
        if jobs.is_empty() {
            break;
        }
        for job in jobs {
            kima_library::jobs::run(ctx, &job.payload, &Default::default())
                .await
                .unwrap();
            kima_jobs::queue::complete(&ctx.pool, &job).await.unwrap();
        }
    }
}

async fn get_run(s: &Setup, run: &str) -> Value {
    let res = s
        .server
        .get(&format!("/api/library/organizer/runs/{run}"))
        .authorization_bearer(&s.admin)
        .await;
    res.assert_status_ok();
    res.json()
}

async fn post(s: &Setup, path: &str, body: Value) -> axum_test::TestResponse {
    s.server
        .post(path)
        .authorization_bearer(&s.admin)
        .json(&body)
        .await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_only(pool: PgPool) {
    let s = setup(pool, true).await;
    let forbidden = json!({"error": "Admin access required"});
    for path in [
        "/api/library/organizer/capabilities",
        "/api/library/organizer/runs",
        "/api/library/organizer/runs/0192f0d6-0000-7000-8000-000000000000",
    ] {
        let res = s.server.get(path).authorization_bearer(&s.user).await;
        res.assert_status(StatusCode::FORBIDDEN);
        res.assert_json(&forbidden);
        s.server
            .get(path)
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    let id = "0192f0d6-0000-7000-8000-000000000000";
    for path in [
        "/api/library/organizer/preview".to_string(),
        format!("/api/library/organizer/runs/{id}/apply"),
        format!("/api/library/organizer/runs/{id}/undo"),
        format!("/api/library/organizer/runs/{id}/switch-root"),
    ] {
        let res = s
            .server
            .post(&path)
            .authorization_bearer(&s.user)
            .json(&json!({"mode": "copy"}))
            .await;
        res.assert_status(StatusCode::FORBIDDEN);
        res.assert_json(&forbidden);
    }
    // Nothing was created.
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM organizer_runs")
        .fetch_one(&s.state.pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn capabilities_report_copy_and_move(pool: PgPool) {
    let s = setup(pool.clone(), true).await;
    let res = s
        .server
        .get("/api/library/organizer/capabilities")
        .authorization_bearer(&s.admin)
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["copyAvailable"], true);
    assert_eq!(body["moveAvailable"], true);
    assert_eq!(body["targetRoot"], s.target.display().to_string());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_unavailable_without_a_target(pool: PgPool) {
    let s = setup(pool, false).await;
    let body: Value = s
        .server
        .get("/api/library/organizer/capabilities")
        .authorization_bearer(&s.admin)
        .await
        .json();
    assert_eq!(body["copyAvailable"], false);
    let res = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "copy"}),
    )
    .await;
    res.assert_status(StatusCode::CONFLICT);
    let res = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "sideways"}),
    )
    .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    res.assert_json(&json!({"error": "mode must be copy or move"}));
}

fn make_read_only(dir: &Path, read_only: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if read_only { 0o555 } else { 0o755 };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn move_unavailable_on_readonly_root(pool: PgPool) {
    let s = setup(pool, true).await;
    make_read_only(&s.root, true);
    if kima_library::organize::apply::writable(&s.root) {
        // Running as root: permissions do not stop writes.
        make_read_only(&s.root, false);
        eprintln!("running with root privileges; skipping the read-only check");
        return;
    }
    let body: Value = s
        .server
        .get("/api/library/organizer/capabilities")
        .authorization_bearer(&s.admin)
        .await
        .json();
    assert_eq!(body["moveAvailable"], false);
    assert_eq!(body["copyAvailable"], true);
    let res = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "move"}),
    )
    .await;
    res.assert_status(StatusCode::CONFLICT);
    res.assert_json(&json!({"error": "Library folder is read-only; use copy mode"}));
    // Copy mode still works from a read-only library.
    let res = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "copy"}),
    )
    .await;
    res.assert_status(StatusCode::ACCEPTED);
    make_read_only(&s.root, false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_preview_apply_and_switch_root_over_http(pool: PgPool) {
    let s = setup(pool.clone(), true).await;
    let res = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "copy"}),
    )
    .await;
    res.assert_status(StatusCode::ACCEPTED);
    let run = res.json::<Value>()["runId"].as_str().unwrap().to_string();
    // A second run cannot start while the first is still planning.
    let busy = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "copy"}),
    )
    .await;
    busy.assert_status(StatusCode::CONFLICT);
    run_jobs(&s.ctx).await;

    let body = get_run(&s, &run).await;
    assert_eq!(body["state"], "previewed");
    assert_eq!(body["mode"], "copy");
    assert_eq!(body["total"], 2);
    let mut to: Vec<String> = body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["to"].as_str().unwrap().to_string())
        .collect();
    to.sort();
    assert_eq!(
        to,
        vec![
            "Alpha/2001 - First/01 - One.mp3".to_string(),
            "Alpha/2001 - First/02 - Two.mp3".to_string()
        ]
    );
    assert!(!s.target.join("Alpha").exists(), "preview wrote files");
    // Rows carry what the preview table groups and warns by.
    let row = &body["rows"][0];
    assert_eq!(row["album"], "First");
    assert_eq!(row["artist"], "Alpha");
    assert!(row["albumId"].is_string());
    assert_eq!(row["guessed"], json!([]));
    assert_eq!(row["state"], "planned");
    // Switching before the copy is done is refused.
    post(
        &s,
        &format!("/api/library/organizer/runs/{run}/switch-root"),
        json!({}),
    )
    .await
    .assert_status(StatusCode::CONFLICT);

    let res = post(
        &s,
        &format!("/api/library/organizer/runs/{run}/apply"),
        json!({}),
    )
    .await;
    res.assert_status(StatusCode::ACCEPTED);
    // Applying twice is refused.
    post(
        &s,
        &format!("/api/library/organizer/runs/{run}/apply"),
        json!({}),
    )
    .await
    .assert_status(StatusCode::CONFLICT);
    run_jobs(&s.ctx).await;
    let body = get_run(&s, &run).await;
    assert_eq!(body["state"], "done", "{body}");
    assert_eq!(
        std::fs::read(s.target.join("Alpha/2001 - First/01 - One.mp3")).unwrap(),
        b"audio One"
    );
    assert!(s.root.join("Alpha/First/1-1 One.mp3").exists());

    // A track that arrived after the preview has no copy: switching would lose it.
    let late = seed(&pool, Seed::new("Late", "Comer", None, "New")).await;
    let res = post(
        &s,
        &format!("/api/library/organizer/runs/{run}/switch-root"),
        json!({}),
    )
    .await;
    res.assert_status(StatusCode::CONFLICT);
    res.assert_json(&json!({
        "error": "Some tracks were not copied; switching would leave them missing"
    }));
    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(late.track)
        .execute(&pool)
        .await
        .unwrap();

    let res = post(
        &s,
        &format!("/api/library/organizer/runs/{run}/switch-root"),
        json!({}),
    )
    .await;
    res.assert_status(StatusCode::ACCEPTED);
    run_jobs(&s.ctx).await;
    assert_eq!(s.state.library_root(), Some(s.target.clone()));
    let settings = kima_db::settings::get(&pool).await.unwrap();
    assert_eq!(settings.library_root, Some(s.target.display().to_string()));
    let body = get_run(&s, &run).await;
    assert!(body["switchedAt"].is_string());
    // The copy is now the library: undo would delete it.
    post(
        &s,
        &format!("/api/library/organizer/runs/{run}/undo"),
        json!({}),
    )
    .await
    .assert_status(StatusCode::CONFLICT);

    let list: Value = s
        .server
        .get("/api/library/organizer/runs")
        .authorization_bearer(&s.admin)
        .await
        .json();
    assert_eq!(list["runs"][0]["id"], run);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn preview_rows_carry_attention_codes(pool: PgPool) {
    let s = setup(pool.clone(), false).await;
    let t = seed(
        &pool,
        Seed::new("Gamma", "Nameless", None, "Untitled").at(1, 0),
    )
    .await;
    std::fs::create_dir_all(s.root.join("Gamma/Nameless")).unwrap();
    std::fs::write(s.root.join("Gamma/Nameless/1-0 Untitled.mp3"), b"x").unwrap();
    let res = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "move"}),
    )
    .await;
    let run = res.json::<Value>()["runId"].as_str().unwrap().to_string();
    run_jobs(&s.ctx).await;
    let body = get_run(&s, &run).await;
    let row = body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["trackId"] == json!(t.track))
        .unwrap()
        .clone();
    assert_eq!(row["album"], "Nameless");
    assert_eq!(row["albumId"], json!(t.album));
    assert_eq!(
        row["guessed"],
        json!(["missingYear", "missingTrackNumbers"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn move_apply_and_undo_over_http(pool: PgPool) {
    let s = setup(pool.clone(), false).await;
    let res = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "move"}),
    )
    .await;
    res.assert_status(StatusCode::ACCEPTED);
    let run = res.json::<Value>()["runId"].as_str().unwrap().to_string();
    run_jobs(&s.ctx).await;
    post(
        &s,
        &format!("/api/library/organizer/runs/{run}/apply"),
        json!({}),
    )
    .await
    .assert_status(StatusCode::ACCEPTED);
    run_jobs(&s.ctx).await;
    assert_eq!(get_run(&s, &run).await["state"], "done");
    assert!(s.root.join("Alpha/2001 - First/02 - Two.mp3").exists());
    assert!(!s.root.join("Alpha/First").exists());
    // Move runs have no copy to switch to.
    post(
        &s,
        &format!("/api/library/organizer/runs/{run}/switch-root"),
        json!({}),
    )
    .await
    .assert_status(StatusCode::CONFLICT);

    // With the library root switched elsewhere, a move cannot be undone.
    s.ctx.root.set(s.target.clone());
    let res = post(
        &s,
        &format!("/api/library/organizer/runs/{run}/undo"),
        json!({}),
    )
    .await;
    res.assert_status(StatusCode::CONFLICT);
    res.assert_json(&json!({
        "error": "The library folder changed since this run; it cannot be undone"
    }));
    s.ctx.root.set(s.root.clone());

    post(
        &s,
        &format!("/api/library/organizer/runs/{run}/undo"),
        json!({}),
    )
    .await
    .assert_status(StatusCode::ACCEPTED);
    run_jobs(&s.ctx).await;
    assert_eq!(get_run(&s, &run).await["state"], "undone");
    assert_eq!(
        std::fs::read(s.root.join("Alpha/First/1-2 Two.mp3")).unwrap(),
        b"audio Two"
    );
    let paths: Vec<String> = sqlx::query_scalar("SELECT path FROM tracks ORDER BY path")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        paths,
        vec!["Alpha/First/1-1 One.mp3", "Alpha/First/1-2 Two.mp3"]
    );

    let missing = s
        .server
        .get("/api/library/organizer/runs/not-a-uuid")
        .authorization_bearer(&s.admin)
        .await;
    missing.assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apply_refused_after_partial_undo(pool: PgPool) {
    let s = setup(pool.clone(), false).await;
    let res = post(
        &s,
        "/api/library/organizer/preview",
        json!({"mode": "move"}),
    )
    .await;
    let run = res.json::<Value>()["runId"].as_str().unwrap().to_string();
    run_jobs(&s.ctx).await;
    assert_eq!(get_run(&s, &run).await["lastAction"], "apply");
    post(
        &s,
        &format!("/api/library/organizer/runs/{run}/apply"),
        json!({}),
    )
    .await
    .assert_status(StatusCode::ACCEPTED);
    run_jobs(&s.ctx).await;
    // An undo got part way and failed.
    sqlx::query(
        "UPDATE organizer_journal SET state = 'undo_failed'
         WHERE id = (SELECT min(id) FROM organizer_journal WHERE run_id = $1::uuid AND state = 'done')",
    )
    .bind(&run)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE organizer_runs SET state = 'failed' WHERE id = $1::uuid")
        .bind(&run)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(get_run(&s, &run).await["lastAction"], "undo");
    let res = post(
        &s,
        &format!("/api/library/organizer/runs/{run}/apply"),
        json!({}),
    )
    .await;
    res.assert_status(StatusCode::CONFLICT);
    res.assert_json(&json!({
        "error": "This run was partly undone; undo it fully or start a new preview"
    }));
}
