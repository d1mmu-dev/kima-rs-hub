//! Organizer apply, resume, undo and switch root (spec §5.2, Review Focus 4), against real
//! files and a real database.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use common::{Tags, make_audio, require_ffmpeg};
use kima_core::config::WatchMode;
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_db::organizer as db;
use kima_library::organize::apply::{self, Action, Hooks, OrganizeError};
use kima_library::scan::{self, LibraryCtx};
use kima_library::watch::{self, IgnoreSet, WatchStatus};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Lib {
    ctx: LibraryCtx,
    root: PathBuf,
    target: PathBuf,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn tags(artist: &str, album: &str, date: &str, track: &str, title: &str) -> Tags {
    Tags {
        album_artist: Some(artist.into()),
        artist: Some(artist.into()),
        album: Some(album.into()),
        title: Some(title.into()),
        date: Some(date.into()),
        track: Some(track.into()),
        ..Default::default()
    }
}

/// (relative path, tags) of a small messy library, and the targets the organizer gives them.
fn messy() -> Vec<(&'static str, Tags, &'static str)> {
    vec![
        (
            "messy/a.flac",
            tags("Alpha", "First", "2001", "1", "One"),
            "Alpha/2001 - First/01 - One.flac",
        ),
        (
            "messy/b.flac",
            tags("Alpha", "First", "2001", "2", "Two"),
            "Alpha/2001 - First/02 - Two.flac",
        ),
        (
            "other/c.mp3",
            tags("Beta", "Second", "2005", "1", "Three"),
            "Beta/2005 - Second/01 - Three.mp3",
        ),
        (
            "other/deep/d.mp3",
            tags("Beta", "Second", "2005", "2", "Four"),
            "Beta/2005 - Second/02 - Four.mp3",
        ),
    ]
}

async fn library(pool: &PgPool, files: &[(&str, Tags, &str)]) -> Lib {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let target = out.path().canonicalize().unwrap().join("organized");
    for (rel, t, _) in files {
        make_audio(&root, rel, t.clone());
    }
    let ctx = LibraryCtx {
        pool: pool.clone(),
        root: root.clone().into(),
        events: EventBus::new(256),
        data_dir: root.join(".kima-data"),
        ignore: IgnoreSet::new(),
    };
    let report = scan::reconcile(&ctx).await.unwrap();
    assert_eq!(report.added, files.len(), "{report:?}");
    Lib {
        ctx,
        root,
        target,
        _dirs: (dir, out),
    }
}

fn sha(p: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(p).unwrap()).to_vec()
}

/// Every non-hidden file under `root`: relative path -> SHA-256.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    if !root.exists() {
        return BTreeMap::new();
    }
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e.path().strip_prefix(root).unwrap();
            (rel.to_string_lossy().replace('\\', "/"), sha(e.path()))
        })
        .collect()
}

/// Every file, hidden ones included (staging names, `.kima-part`), except kima's data folder.
fn all_files(root: &Path) -> Vec<String> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".kima-data")
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            e.path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

async fn new_run(lib: &Lib, mode: &str) -> Uuid {
    let target = if mode == "copy" {
        &lib.target
    } else {
        &lib.root
    };
    std::fs::create_dir_all(target).unwrap();
    let run = db::create_run(
        &lib.ctx.pool,
        Uuid::now_v7(),
        mode,
        &lib.root.display().to_string(),
        &target.display().to_string(),
        None,
    )
    .await
    .unwrap();
    apply::run_action(&lib.ctx, run.id, Action::Preview, &Hooks::default())
        .await
        .unwrap();
    assert_eq!(state(lib, run.id).await, "previewed");
    run.id
}

async fn state(lib: &Lib, run: Uuid) -> String {
    db::get_run(&lib.ctx.pool, run)
        .await
        .unwrap()
        .unwrap()
        .state
}

async fn start(lib: &Lib, run: Uuid, from: &[&str], to: &str) {
    assert!(db::transition(&lib.ctx.pool, run, from, to).await.unwrap());
}

async fn apply_with(lib: &Lib, run: Uuid, hooks: &Hooks) -> Result<(), OrganizeError> {
    apply::run_action(&lib.ctx, run, Action::Apply, hooks).await
}

async fn apply_run(lib: &Lib, run: Uuid) {
    start(lib, run, &["previewed"], "applying").await;
    apply_with(lib, run, &Hooks::default()).await.unwrap();
}

async fn undo_run(lib: &Lib, run: Uuid) {
    start(lib, run, &["done", "failed"], "undoing").await;
    apply::run_action(&lib.ctx, run, Action::Undo, &Hooks::default())
        .await
        .unwrap();
}

async fn rows(lib: &Lib, run: Uuid) -> Vec<db::JournalRow> {
    db::all_rows(&lib.ctx.pool, run).await.unwrap()
}

/// Track id -> path.
async fn track_paths(pool: &PgPool) -> HashMap<Uuid, String> {
    sqlx::query_as::<_, (Uuid, String)>("SELECT id, path FROM tracks")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .collect()
}

fn targets(files: &[(&str, Tags, &str)]) -> Vec<String> {
    let mut t: Vec<String> = files.iter().map(|(_, _, to)| to.to_string()).collect();
    t.sort();
    t
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn preview_touches_nothing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    std::fs::write(lib.root.join("messy/cover.jpg"), b"jpeg").unwrap();
    let before = tree(&lib.root);
    let before_files = all_files(&lib.root);
    let run = new_run(&lib, "move").await;
    assert_eq!(tree(&lib.root), before);
    assert_eq!(all_files(&lib.root), before_files);

    let rows = rows(&lib, run).await;
    let mut planned: Vec<(String, String)> = rows
        .iter()
        .map(|r| (r.from_path.clone(), r.to_path.clone()))
        .collect();
    planned.sort();
    let mut want: Vec<(String, String)> = files
        .iter()
        .map(|(f, _, t)| (f.to_string(), t.to_string()))
        .collect();
    want.push((
        "messy/cover.jpg".into(),
        "Alpha/2001 - First/cover.jpg".into(),
    ));
    want.sort();
    assert_eq!(planned, want);
    assert!(
        rows.iter()
            .all(|r| r.state == "planned" && r.sha256.is_none())
    );
    let sidecar = rows
        .iter()
        .find(|r| r.from_path.ends_with("cover.jpg"))
        .unwrap();
    assert_eq!(sidecar.track_id, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_apply_creates_tree_and_keeps_source(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let before = tree(&lib.root);
    let paths_before = track_paths(&pool).await;
    let run = new_run(&lib, "copy").await;
    apply_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "done");

    // The library is untouched, files and rows alike.
    assert_eq!(tree(&lib.root), before);
    assert_eq!(track_paths(&pool).await, paths_before);
    // The copy has the organized layout, the same content, and the source mtimes.
    let copied = tree(&lib.target);
    assert_eq!(copied.keys().cloned().collect::<Vec<_>>(), targets(&files));
    for (from, _, to) in &files {
        assert_eq!(copied[*to], before[*from], "{to}");
        let (a, b) = (
            std::fs::metadata(lib.root.join(from)).unwrap(),
            std::fs::metadata(lib.target.join(to)).unwrap(),
        );
        assert_eq!(a.modified().unwrap(), b.modified().unwrap());
    }
    assert!(
        rows(&lib, run)
            .await
            .iter()
            .all(|r| r.state == "done" && r.sha256.is_some())
    );
    assert!(all_files(&lib.target).iter().all(|f| !f.contains(".kima-")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_verify_detects_corruption(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let before = tree(&lib.root);
    let run = new_run(&lib, "copy").await;
    start(&lib, run, &["previewed"], "applying").await;
    let hooks = Hooks {
        after_copy: Some(Arc::new(|part: &Path| {
            if part.to_string_lossy().contains("02 - Two") {
                let mut bytes = std::fs::read(part).unwrap();
                bytes[100] ^= 0xff;
                std::fs::write(part, bytes).unwrap();
            }
        })),
        ..Default::default()
    };
    apply_with(&lib, run, &hooks).await.unwrap();

    let rows = rows(&lib, run).await;
    let bad = rows
        .iter()
        .find(|r| r.to_path.contains("02 - Two"))
        .unwrap();
    assert_eq!(bad.state, "failed");
    assert_eq!(bad.note.as_deref(), Some("the copy did not verify"));
    let to = lib.target.join(&bad.to_path);
    assert!(!to.exists());
    let part = format!("{}.kima-part", to.display());
    assert!(!Path::new(&part).exists());
    assert_eq!(
        rows.iter().filter(|r| r.state == "done").count(),
        files.len() - 1
    );
    assert_eq!(tree(&lib.root), before);
}

async fn add_play(pool: &PgPool, track: Uuid) {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(pool)
        .await
        .unwrap();
    let user = Uuid::now_v7();
    let kima_db::users::Registration::Created(_) =
        kima_db::users::register(pool, user, "ana", "hash")
            .await
            .unwrap()
    else {
        panic!("registration failed")
    };
    sqlx::query("INSERT INTO plays (id, user_id, track_id) VALUES ($1, $2, $3)")
        .bind(Uuid::now_v7())
        .bind(user)
        .bind(track)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn move_apply_updates_paths_and_keeps_plays(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    // `other/` keeps a file that is neither audio nor a sidecar: it must stay, with its folder.
    std::fs::write(lib.root.join("other/notes.txt"), b"keep me").unwrap();
    let before = tree(&lib.root);
    let ids: HashMap<String, Uuid> = track_paths(&pool)
        .await
        .into_iter()
        .map(|(id, p)| (p, id))
        .collect();
    let played = ids["messy/a.flac"];
    add_play(&pool, played).await;

    let run = new_run(&lib, "move").await;
    apply_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "done");

    let after = tree(&lib.root);
    let paths = track_paths(&pool).await;
    for (from, _, to) in &files {
        assert_eq!(after[*to], before[*from], "{to}");
        assert!(!lib.root.join(from).exists(), "{from} still there");
        assert_eq!(&paths[&ids[*from]], to, "track row of {from}");
    }
    assert_eq!(after.len(), before.len());
    // Emptied folders are gone; the one with notes.txt stays.
    assert!(!lib.root.join("messy").exists());
    assert!(!lib.root.join("other/deep").exists());
    assert_eq!(
        std::fs::read(lib.root.join("other/notes.txt")).unwrap(),
        b"keep me"
    );
    // Same track id, so the play is still attached.
    let plays: i64 = sqlx::query_scalar("SELECT count(*) FROM plays WHERE track_id = $1")
        .bind(played)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(plays, 1);
    // A reconcile agrees with the rows: nothing added, nothing missing.
    let report = scan::reconcile(&lib.ctx).await.unwrap();
    assert_eq!(
        (report.added, report.missing, report.repointed),
        (0, 0, 0),
        "{report:?}"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn move_across_filesystems_copies_verifies_and_deletes_source(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let before = tree(&lib.root);
    let run = new_run(&lib, "move").await;
    start(&lib, run, &["previewed"], "applying").await;
    let hooks = Hooks {
        force_cross_fs: true,
        ..Default::default()
    };
    apply_with(&lib, run, &hooks).await.unwrap();
    let after = tree(&lib.root);
    for (from, _, to) in &files {
        assert_eq!(after[*to], before[*from]);
        assert!(!lib.root.join(from).exists());
    }
    assert_eq!(after.len(), before.len());
    assert!(all_files(&lib.root).iter().all(|f| !f.contains(".kima-")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cross_fs_move_keeps_the_source_when_the_copy_is_bad(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let before = tree(&lib.root);
    let run = new_run(&lib, "move").await;
    start(&lib, run, &["previewed"], "applying").await;
    let hooks = Hooks {
        force_cross_fs: true,
        after_copy: Some(Arc::new(|part: &Path| {
            let mut bytes = std::fs::read(part).unwrap();
            bytes[10] ^= 1;
            std::fs::write(part, bytes).unwrap();
        })),
        ..Default::default()
    };
    apply_with(&lib, run, &hooks).await.unwrap();
    assert_eq!(tree(&lib.root), before);
    assert!(rows(&lib, run).await.iter().all(|r| r.state == "failed"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn never_overwrites(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "move").await;
    // Something appears at a planned target after the preview.
    let squatter = lib.root.join("Alpha/2001 - First/01 - One.flac");
    std::fs::create_dir_all(squatter.parent().unwrap()).unwrap();
    std::fs::write(&squatter, b"not yours").unwrap();
    let source = sha(&lib.root.join("messy/a.flac"));

    apply_run(&lib, run).await;
    assert_eq!(std::fs::read(&squatter).unwrap(), b"not yours");
    assert_eq!(sha(&lib.root.join("messy/a.flac")), source);
    let rows = rows(&lib, run).await;
    let row = rows.iter().find(|r| r.from_path == "messy/a.flac").unwrap();
    assert_eq!(row.state, "skipped");
    assert_eq!(row.note.as_deref(), Some("target exists"));
    assert_eq!(
        rows.iter().filter(|r| r.state == "done").count(),
        files.len() - 1
    );
    let paths = track_paths(&pool).await;
    assert!(paths.values().any(|p| p == "messy/a.flac"));

    // Copy mode too.
    let copy = new_run(&lib, "copy").await;
    let taken = lib.target.join("Beta/2005 - Second/01 - Three.mp3");
    std::fs::create_dir_all(taken.parent().unwrap()).unwrap();
    std::fs::write(&taken, b"mine").unwrap();
    apply_run(&lib, copy).await;
    assert_eq!(std::fs::read(&taken).unwrap(), b"mine");
    let rows = db::all_rows(&pool, copy).await.unwrap();
    let row = rows.iter().find(|r| r.to_path.ends_with("01 - Three.mp3"));
    // The preview saw the file was there and planned around it, or the row was skipped.
    assert!(row.is_none_or(|r| r.state == "skipped"));
}

/// A library of `n` tracks across a few albums, all in one messy folder.
fn many(n: usize) -> Vec<(String, Tags, String)> {
    (0..n)
        .map(|i| {
            let album = format!("Album {}", i % 3);
            let no = i / 3 + 1;
            let title = format!("Song {i}");
            (
                format!("dump/{i:02}.flac"),
                tags("Gamma", &album, "1999", &no.to_string(), &title),
                format!("Gamma/1999 - {album}/{no:02} - {title}.flac"),
            )
        })
        .collect()
}

fn as_refs(files: &[(String, Tags, String)]) -> Vec<(&str, Tags, &str)> {
    files
        .iter()
        .map(|(a, t, b)| (a.as_str(), t.clone(), b.as_str()))
        .collect()
}

/// Review Focus 4: kima dies part-way through a move. A restart resumes from the journal;
/// nothing is moved twice, overwritten or lost, and undo restores every original path.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn crash_mid_move_resumes_cleanly(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let owned = many(10);
    let files = as_refs(&owned);
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let original_paths = track_paths(&pool).await;

    // Two crashes: one at a clean row boundary, one after a rename but before its commit.
    for (stop_after, mid_row) in [(4usize, false), (6usize, true)] {
        let run = new_run(&lib, "move").await;
        start(&lib, run, &["previewed"], "applying").await;
        let hooks = Hooks {
            stop_after: Some(stop_after),
            stop_mid_row: mid_row,
            ..Default::default()
        };
        let err = apply_with(&lib, run, &hooks).await.unwrap_err();
        assert!(matches!(err, OrganizeError::Interrupted));
        assert_eq!(state(&lib, run).await, "applying");

        // Mid-way: every file still exists exactly once, somewhere.
        let mid = tree(&lib.root);
        assert_eq!(mid.len(), original.len());
        let mut mid_sums: Vec<_> = mid.values().cloned().collect();
        let mut sums: Vec<_> = original.values().cloned().collect();
        mid_sums.sort();
        sums.sort();
        assert_eq!(mid_sums, sums, "a file was lost or duplicated mid-way");

        // Restart: the job runs again from the journal.
        apply_with(&lib, run, &Hooks::default()).await.unwrap();
        assert_eq!(state(&lib, run).await, "done");
        let after = tree(&lib.root);
        assert_eq!(after.len(), original.len(), "file count changed");
        for (from, _, to) in &files {
            assert_eq!(after.get(*to), original.get(*from), "{from} -> {to}");
            assert!(!lib.root.join(from).exists());
        }
        let rows = rows(&lib, run).await;
        assert_eq!(rows.len(), files.len());
        assert!(rows.iter().all(|r| r.state == "done"), "{rows:?}");
        let paths = track_paths(&pool).await;
        for (id, from) in &original_paths {
            let to = files.iter().find(|(f, _, _)| f == from).unwrap().2;
            assert_eq!(paths[id], to);
        }
        assert!(all_files(&lib.root).iter().all(|f| !f.contains(".kima-")));

        // Undo, also interrupted once, restores every original path.
        start(&lib, run, &["done"], "undoing").await;
        let hooks = Hooks {
            stop_after: Some(3),
            stop_mid_row: true,
            ..Default::default()
        };
        let err = apply::run_action(&lib.ctx, run, Action::Undo, &hooks)
            .await
            .unwrap_err();
        assert!(matches!(err, OrganizeError::Interrupted));
        apply::run_action(&lib.ctx, run, Action::Undo, &Hooks::default())
            .await
            .unwrap();
        assert_eq!(state(&lib, run).await, "undone");
        assert_eq!(tree(&lib.root), original);
        assert_eq!(track_paths(&pool).await, original_paths);
        assert!(!lib.root.join("Gamma").exists());
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_move_restores_all(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    std::fs::write(lib.root.join("messy/cover.jpg"), b"jpeg").unwrap();
    let original = tree(&lib.root);
    let original_paths = track_paths(&pool).await;
    let run = new_run(&lib, "move").await;
    apply_run(&lib, run).await;
    assert!(lib.root.join("Alpha/2001 - First/cover.jpg").exists());
    undo_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "undone");
    assert_eq!(tree(&lib.root), original);
    assert_eq!(track_paths(&pool).await, original_paths);
    assert!(!lib.root.join("Alpha").exists());
    assert!(!lib.root.join("Beta").exists());
    assert!(rows(&lib, run).await.iter().all(|r| r.state == "undone"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_copy_removes_only_matching_copies(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let run = new_run(&lib, "copy").await;
    apply_run(&lib, run).await;
    // The owner edits one copy afterwards.
    let edited = lib.target.join("Beta/2005 - Second/01 - Three.mp3");
    std::fs::write(&edited, b"edited").unwrap();

    undo_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "undone");
    assert_eq!(
        tree(&lib.target).keys().cloned().collect::<Vec<_>>(),
        vec!["Beta/2005 - Second/01 - Three.mp3".to_string()]
    );
    assert_eq!(std::fs::read(&edited).unwrap(), b"edited");
    assert_eq!(tree(&lib.root), original);
    let rows = rows(&lib, run).await;
    let kept = rows
        .iter()
        .find(|r| r.to_path.ends_with("01 - Three.mp3"))
        .unwrap();
    assert_eq!(kept.state, "skipped");
    assert_eq!(
        rows.iter().filter(|r| r.state == "undone").count(),
        files.len() - 1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_root_repoints_and_reconcile_is_noop(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let ids: HashMap<String, Uuid> = track_paths(&pool)
        .await
        .into_iter()
        .map(|(id, p)| (p, id))
        .collect();
    let run = new_run(&lib, "copy").await;
    apply_run(&lib, run).await;
    apply::run_action(&lib.ctx, run, Action::SwitchRoot, &Hooks::default())
        .await
        .unwrap();

    assert_eq!(lib.ctx.root.get(), lib.target);
    let settings = kima_db::settings::get(&pool).await.unwrap();
    assert_eq!(
        settings.library_root,
        Some(lib.target.display().to_string())
    );
    let paths = track_paths(&pool).await;
    for (from, _, to) in &files {
        assert_eq!(&paths[&ids[*from]], to);
    }
    assert!(
        db::get_run(&pool, run)
            .await
            .unwrap()
            .unwrap()
            .switched_at
            .is_some()
    );
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE payload ->> 'kind' = 'reconcile' AND state = 'queued'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued, 1);

    let report = scan::reconcile(&lib.ctx).await.unwrap();
    assert_eq!(report.added, 0, "{report:?}");
    assert_eq!(report.missing, 0, "{report:?}");
    assert_eq!(report.unchanged, files.len(), "{report:?}");

    // A switched copy cannot be undone.
    start(&lib, run, &["done"], "undoing").await;
    let err = apply::run_action(&lib.ctx, run, Action::Undo, &Hooks::default()).await;
    assert!(matches!(err, Err(OrganizeError::Refused(_))), "{err:?}");
    assert_eq!(state(&lib, run).await, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn watcher_ignores_organizer_moves(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let shutdown = CancellationToken::new();
    let debounce = Duration::from_millis(200);
    let handle = watch::spawn_with(
        lib.ctx.clone(),
        WatchMode::Auto,
        Duration::from_secs(60),
        debounce,
        Duration::from_secs(30),
        lib.ctx.ignore.clone(),
        Arc::new(RwLock::new(WatchStatus::default())),
        shutdown.clone(),
    );
    tokio::time::sleep(debounce * 2).await;
    assert!(handle.status().active);

    let run = new_run(&lib, "move").await;
    apply_run(&lib, run).await;
    tokio::time::sleep(debounce * 8).await;
    let jobs: Vec<String> =
        sqlx::query_scalar("SELECT payload::text FROM jobs WHERE queue = 'library' AND payload ->> 'kind' <> 'reconcile' ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        jobs.is_empty(),
        "the watcher reacted to organizer moves: {jobs:?}"
    );

    // An ordinary change right afterwards outside the moved paths is still seen.
    make_audio(
        &lib.root,
        "fresh/new.flac",
        tags("Delta", "D", "2010", "1", "New"),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM jobs WHERE payload ->> 'path' = 'fresh/new.flac'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if n > 0 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "new file not seen");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    shutdown.cancel();
    handle.join().await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn watcher_follows_a_switched_root(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let shutdown = CancellationToken::new();
    let handle = watch::spawn_with(
        lib.ctx.clone(),
        WatchMode::Auto,
        Duration::from_secs(60),
        Duration::from_millis(200),
        Duration::from_secs(30),
        lib.ctx.ignore.clone(),
        Arc::new(RwLock::new(WatchStatus::default())),
        shutdown.clone(),
    );
    let run = new_run(&lib, "copy").await;
    apply_run(&lib, run).await;
    apply::run_action(&lib.ctx, run, Action::SwitchRoot, &Hooks::default())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    make_audio(
        &lib.target,
        "New/2020 - N/01 - N.flac",
        tags("New", "N", "2020", "1", "N"),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM jobs WHERE payload ->> 'path' = 'New/2020 - N/01 - N.flac'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if n > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the new root is not watched"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    shutdown.cancel();
    handle.join().await;
}

/// A→B while B→A: the plan does not see cycles; apply stages one file and finishes the loop.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn swap_cycle_moves_through_staging(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    // Each file sits where the other one belongs.
    let files = vec![
        (
            "Alpha/2001 - First/01 - One.flac",
            tags("Alpha", "First", "2001", "2", "Two"),
            "Alpha/2001 - First/02 - Two.flac",
        ),
        (
            "Alpha/2001 - First/02 - Two.flac",
            tags("Alpha", "First", "2001", "1", "One"),
            "Alpha/2001 - First/01 - One.flac",
        ),
    ];
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let original_paths = track_paths(&pool).await;
    let ids: HashMap<String, Uuid> = original_paths
        .iter()
        .map(|(id, p)| (p.clone(), *id))
        .collect();
    let run = new_run(&lib, "move").await;
    assert_eq!(rows(&lib, run).await.len(), 2);
    apply_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "done");

    let after = tree(&lib.root);
    for (from, _, to) in &files {
        assert_eq!(after[*to], original[*from], "{from} -> {to}");
    }
    let paths = track_paths(&pool).await;
    for (from, _, to) in &files {
        assert_eq!(&paths[&ids[*from]], to);
    }
    assert_eq!(all_files(&lib.root).len(), 2, "{:?}", all_files(&lib.root));
    assert!(rows(&lib, run).await.iter().all(|r| r.state == "done"));

    undo_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "undone");
    assert_eq!(tree(&lib.root), original);
    assert_eq!(track_paths(&pool).await, original_paths);
    assert_eq!(all_files(&lib.root).len(), 2);
}

/// A swap cycle interrupted while one file is parked under its staging name.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn swap_cycle_resumes_from_staging(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    // Stopped before the first row, mid-staging, and mid-second-pass.
    let files = vec![
        (
            "Alpha/2001 - First/01 - One.flac",
            tags("Alpha", "First", "2001", "2", "Two"),
            "Alpha/2001 - First/02 - Two.flac",
        ),
        (
            "Alpha/2001 - First/02 - Two.flac",
            tags("Alpha", "First", "2001", "1", "One"),
            "Alpha/2001 - First/01 - One.flac",
        ),
    ];
    for stop in 0..3 {
        let lib = library(&pool, &files).await;
        let original = tree(&lib.root);
        let run = new_run(&lib, "move").await;
        start(&lib, run, &["previewed"], "applying").await;
        let hooks = Hooks {
            stop_after: Some(stop),
            stop_mid_row: true,
            ..Default::default()
        };
        assert!(apply_with(&lib, run, &hooks).await.is_err());
        let mut sums: Vec<_> = tree_with_hidden(&lib.root);
        let mut want: Vec<_> = original.values().cloned().collect();
        sums.sort();
        want.sort();
        assert_eq!(sums, want, "stop {stop}: a file was lost or duplicated");
        apply_with(&lib, run, &Hooks::default()).await.unwrap();
        let after = tree(&lib.root);
        for (from, _, to) in &files {
            assert_eq!(after[*to], original[*from], "stop {stop}: {from} -> {to}");
        }
        assert_eq!(all_files(&lib.root).len(), 2);
        // Clean up for the next round: drop this library's rows.
        sqlx::query("DELETE FROM organizer_runs")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM tracks")
            .execute(&pool)
            .await
            .unwrap();
    }
}

/// Checksums of every file, staging names included.
fn tree_with_hidden(root: &Path) -> Vec<Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".kima-data")
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| sha(e.path()))
        .collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn case_only_rename(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = vec![(
        "alpha/2001 - first/01 - one.flac",
        tags("Alpha", "First", "2001", "1", "One"),
        "Alpha/2001 - First/01 - One.flac",
    )];
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let run = new_run(&lib, "move").await;
    let rows_before = rows(&lib, run).await;
    assert_eq!(rows_before.len(), 1);
    apply_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "done");
    let after = tree(&lib.root);
    assert_eq!(
        after.keys().cloned().collect::<Vec<_>>(),
        vec!["Alpha/2001 - First/01 - One.flac".to_string()]
    );
    assert_eq!(after.values().next(), original.values().next());
    assert!(!lib.root.join("alpha").exists());
    let paths = track_paths(&pool).await;
    assert_eq!(
        paths.values().next().unwrap(),
        "Alpha/2001 - First/01 - One.flac"
    );
    undo_run(&lib, run).await;
    assert_eq!(tree(&lib.root), original);
}

/// A copy killed after a file was placed but before its row was committed resumes without
/// copying twice.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn crash_mid_copy_resumes_cleanly(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let run = new_run(&lib, "copy").await;
    start(&lib, run, &["previewed"], "applying").await;
    let hooks = Hooks {
        stop_after: Some(1),
        stop_mid_row: true,
        ..Default::default()
    };
    assert!(matches!(
        apply_with(&lib, run, &hooks).await,
        Err(OrganizeError::Interrupted)
    ));
    assert_eq!(tree(&lib.target).len(), 2);
    apply_with(&lib, run, &Hooks::default()).await.unwrap();
    assert_eq!(state(&lib, run).await, "done");
    let copied = tree(&lib.target);
    assert_eq!(copied.keys().cloned().collect::<Vec<_>>(), targets(&files));
    for (from, _, to) in &files {
        assert_eq!(copied[*to], original[*from]);
    }
    assert!(rows(&lib, run).await.iter().all(|r| r.state == "done"));
    assert_eq!(tree(&lib.root), original);
}

/// Killed between linking a file into place and unlinking its old name: both names are the
/// same file. The resume drops the old name instead of reporting "target exists".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn crash_between_link_and_unlink_resumes(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let run = new_run(&lib, "move").await;
    start(&lib, run, &["previewed"], "applying").await;
    // Reproduce the moment: checksum journalled, hard link made, old name still there.
    let row = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.from_path == "messy/a.flac")
        .unwrap();
    let (src, dst) = (lib.root.join(&row.from_path), lib.root.join(&row.to_path));
    db::set_sha(&pool, row.id, &sha(&src)).await.unwrap();
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::hard_link(&src, &dst).unwrap();

    apply_with(&lib, run, &Hooks::default()).await.unwrap();
    assert_eq!(state(&lib, run).await, "done");
    let after = tree(&lib.root);
    assert_eq!(after.len(), original.len());
    for (from, _, to) in &files {
        assert_eq!(after[*to], original[*from]);
        assert!(!lib.root.join(from).exists());
    }
    assert!(rows(&lib, run).await.iter().all(|r| r.state == "done"));
}

fn swap_files() -> Vec<(&'static str, Tags, &'static str)> {
    vec![
        (
            "Alpha/2001 - First/01 - One.flac",
            tags("Alpha", "First", "2001", "2", "Two"),
            "Alpha/2001 - First/02 - Two.flac",
        ),
        (
            "Alpha/2001 - First/02 - Two.flac",
            tags("Alpha", "First", "2001", "1", "One"),
            "Alpha/2001 - First/01 - One.flac",
        ),
    ]
}

async fn is_missing(pool: &PgPool, track: Uuid) -> bool {
    sqlx::query_scalar("SELECT missing_since IS NOT NULL FROM tracks WHERE id = $1")
        .bind(track)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_refuses_when_track_moved_since(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "move").await;
    apply_run(&lib, run).await;
    // After the apply, the owner moves one organized file and the scanner follows it.
    let moved_to = "Elsewhere/one.flac";
    std::fs::create_dir_all(lib.root.join("Elsewhere")).unwrap();
    std::fs::rename(
        lib.root.join("Alpha/2001 - First/01 - One.flac"),
        lib.root.join(moved_to),
    )
    .unwrap();
    let row = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.from_path == "messy/a.flac")
        .unwrap();
    sqlx::query("UPDATE tracks SET path = $2 WHERE id = $1")
        .bind(row.track_id.unwrap())
        .bind(moved_to)
        .execute(&pool)
        .await
        .unwrap();
    let kept = tree(&lib.root)[moved_to].clone();

    undo_run(&lib, run).await;
    let all = rows(&lib, run).await;
    let refused = all.iter().find(|r| r.id == row.id).unwrap();
    assert_eq!(refused.state, "skipped");
    assert_eq!(
        refused.note.as_deref(),
        Some("could not undo: the track moved since the apply")
    );
    assert_eq!(tree(&lib.root)[moved_to], kept);
    assert!(!lib.root.join("messy/a.flac").exists());
    assert_eq!(
        db::track_path(&pool, row.track_id.unwrap())
            .await
            .unwrap()
            .as_deref(),
        Some(moved_to)
    );
    for (from, _, _) in files.iter().filter(|(f, _, _)| *f != "messy/a.flac") {
        assert!(lib.root.join(from).exists(), "{from}");
    }

    // An undo after the library root changed is refused outright.
    let run2 = new_run(&lib, "move").await;
    apply_run(&lib, run2).await;
    let elsewhere = tempfile::tempdir().unwrap();
    lib.ctx.root.set(elsewhere.path().to_path_buf());
    start(&lib, run2, &["done"], "undoing").await;
    let err = apply::run_action(&lib.ctx, run2, Action::Undo, &Hooks::default()).await;
    assert!(
        matches!(&err, Err(OrganizeError::Refused(m)) if m.contains("library root changed")),
        "{err:?}"
    );
    lib.ctx.root.set(lib.root.clone());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_refuses_after_root_switched(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "copy").await;
    apply_run(&lib, run).await;
    apply::run_action(&lib.ctx, run, Action::SwitchRoot, &Hooks::default())
        .await
        .unwrap();
    let copied = tree(&lib.target);
    // Even without the switch stamp, the job sees that the copy is the library.
    sqlx::query("UPDATE organizer_runs SET switched_at = NULL WHERE id = $1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    start(&lib, run, &["done"], "undoing").await;
    let err = apply::run_action(&lib.ctx, run, Action::Undo, &Hooks::default()).await;
    assert!(
        matches!(&err, Err(OrganizeError::Refused(m)) if m.contains("library uses this copy")),
        "{err:?}"
    );
    assert_eq!(tree(&lib.target), copied);
}

/// A staged file whose target is taken, and whose own place is taken too, stays parked; the
/// scanner must not flag its track missing.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn parked_file_is_not_marked_missing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = swap_files();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "move").await;
    start(&lib, run, &["previewed"], "applying").await;
    // First pass done (one staged, one moved), then stop before the second pass.
    let hooks = Hooks {
        stop_after: Some(2),
        ..Default::default()
    };
    assert!(apply_with(&lib, run, &hooks).await.is_err());
    let staged = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.state == "staged")
        .expect("one row staged");
    // Someone takes the staged file's target meanwhile.
    std::fs::write(lib.root.join(&staged.to_path), b"squatter").unwrap();
    apply_with(&lib, run, &Hooks::default()).await.unwrap();
    assert_eq!(state(&lib, run).await, "failed");
    let row = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.id == staged.id)
        .unwrap();
    assert_eq!(row.state, "staged");
    assert_eq!(
        std::fs::read(lib.root.join(&staged.to_path)).unwrap(),
        b"squatter"
    );
    let track = row.track_id.unwrap();
    let parked = db::track_path(&pool, track).await.unwrap().unwrap();
    assert!(parked.contains(".kima-stage-"), "{parked}");
    assert!(lib.root.join(&parked).is_file());

    let report = scan::reconcile(&lib.ctx).await.unwrap();
    assert_eq!(report.missing, 0, "{report:?}");
    assert!(!is_missing(&pool, track).await);
}

/// A staged file whose target is taken goes back where it came from when that is free.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn parked_file_goes_back_when_its_place_is_free(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = vec![(
        "alpha/2001 - first/01 - one.flac",
        tags("Alpha", "First", "2001", "1", "One"),
        "Alpha/2001 - First/01 - One.flac",
    )];
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let run = new_run(&lib, "move").await;
    start(&lib, run, &["previewed"], "applying").await;
    let hooks = Hooks {
        stop_after: Some(1),
        ..Default::default()
    };
    assert!(apply_with(&lib, run, &hooks).await.is_err());
    let target = lib.root.join("Alpha/2001 - First/01 - One.flac");
    std::fs::write(&target, b"squatter").unwrap();
    apply_with(&lib, run, &Hooks::default()).await.unwrap();
    let row = rows(&lib, run).await.pop().unwrap();
    assert_eq!(row.state, "skipped");
    assert_eq!(std::fs::read(&target).unwrap(), b"squatter");
    let mut want = original.clone();
    want.insert("Alpha/2001 - First/01 - One.flac".into(), sha(&target));
    assert_eq!(tree(&lib.root), want);
    assert_eq!(
        db::track_path(&pool, row.track_id.unwrap())
            .await
            .unwrap()
            .as_deref(),
        Some("alpha/2001 - first/01 - one.flac")
    );
    assert!(all_files(&lib.root).iter().all(|f| !f.contains(".kima-")));
}

/// Killed while staging, between linking the staging name and unlinking the old name.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn crash_between_link_and_unlink_while_staging_resumes(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = swap_files();
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let run = new_run(&lib, "move").await;
    start(&lib, run, &["previewed"], "applying").await;
    // The first row in plan order is the one that gets staged. Reproduce the moment: its
    // checksum journalled, the staging name linked, the old name still there.
    let row = rows(&lib, run).await.into_iter().next().unwrap();
    let src = lib.root.join(&row.from_path);
    let (dir, name) = row.to_path.rsplit_once('/').unwrap();
    let stage = lib
        .root
        .join(dir)
        .join(format!(".kima-stage-{}-{name}", row.id));
    db::set_sha(&pool, row.id, &sha(&src)).await.unwrap();
    std::fs::hard_link(&src, &stage).unwrap();

    apply_with(&lib, run, &Hooks::default()).await.unwrap();
    assert_eq!(state(&lib, run).await, "done");
    let after = tree(&lib.root);
    for (from, _, to) in &files {
        assert_eq!(after[*to], original[*from], "{from} -> {to}");
    }
    assert_eq!(all_files(&lib.root).len(), 2, "{:?}", all_files(&lib.root));
    assert!(rows(&lib, run).await.iter().all(|r| r.state == "done"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_root_refuses_tracks_added_after_the_preview(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "copy").await;
    apply_run(&lib, run).await;
    make_audio(
        &lib.root,
        "late/new.flac",
        tags("Late", "L", "2020", "1", "New"),
    );
    assert_eq!(scan::reconcile(&lib.ctx).await.unwrap().added, 1);
    let err = apply::run_action(&lib.ctx, run, Action::SwitchRoot, &Hooks::default()).await;
    assert!(matches!(err, Err(OrganizeError::Invalid(_))), "{err:?}");
    assert_eq!(lib.ctx.root.get(), lib.root);
    assert_eq!(
        kima_db::settings::get(&pool).await.unwrap().library_root,
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refuses_symlinked_target_folders(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "move").await;
    // After the preview, the artist folder becomes a link out of the library.
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), lib.root.join("Alpha")).unwrap();
    apply_run(&lib, run).await;
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    for r in rows(&lib, run).await {
        if r.to_path.starts_with("Alpha/") {
            assert_eq!(r.state, "skipped", "{r:?}");
            assert_eq!(
                r.note.as_deref(),
                Some("a folder on the target path is a symbolic link")
            );
            assert!(lib.root.join(&r.from_path).exists());
        } else {
            assert_eq!(r.state, "done", "{r:?}");
        }
    }

    // Copy mode too.
    std::fs::create_dir_all(&lib.target).unwrap();
    std::os::unix::fs::symlink(outside.path(), lib.target.join("Beta")).unwrap();
    let copy = new_run(&lib, "copy").await;
    apply_run(&lib, copy).await;
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    let refused = db::all_rows(&pool, copy)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.to_path.starts_with("Beta/"))
        .inspect(|r| assert_eq!(r.state, "skipped", "{r:?}"))
        .count();
    assert_eq!(refused, 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unchanged_rows_are_journaled_and_left_alone(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = vec![
        (
            "Alpha/2001 - First/01 - One.flac",
            tags("Alpha", "First", "2001", "1", "One"),
            "Alpha/2001 - First/01 - One.flac",
        ),
        (
            "messy/b.flac",
            tags("Alpha", "First", "2001", "2", "Two"),
            "Alpha/2001 - First/02 - Two.flac",
        ),
    ];
    let lib = library(&pool, &files).await;
    let original = tree(&lib.root);
    let run = new_run(&lib, "move").await;
    let in_place = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.from_path == "Alpha/2001 - First/01 - One.flac")
        .unwrap();
    assert_eq!(in_place.state, "unchanged");
    assert_eq!(in_place.from_path, in_place.to_path);
    let summary = db::get_run(&pool, run).await.unwrap().unwrap().summary;
    assert_eq!(summary["unchanged"], 1);
    assert_eq!(summary["planned"], 1);

    apply_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "done");
    let after = rows(&lib, run).await;
    let row = after.iter().find(|r| r.id == in_place.id).unwrap();
    assert_eq!(row.state, "unchanged");
    assert!(row.sha256.is_none());
    undo_run(&lib, run).await;
    assert_eq!(state(&lib, run).await, "undone");
    assert_eq!(tree(&lib.root), original);
    let row = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.id == in_place.id)
        .unwrap();
    assert_eq!(row.state, "unchanged");
}

/// Killed after a cross-filesystem copy into the staging name, before the old name was
/// dropped; then something rewrites the source. The resume must keep both files.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn changed_source_after_a_staging_copy_is_kept(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = swap_files();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "move").await;
    start(&lib, run, &["previewed"], "applying").await;
    let row = rows(&lib, run).await.into_iter().next().unwrap();
    let src = lib.root.join(&row.from_path);
    let (dir, name) = row.to_path.rsplit_once('/').unwrap();
    let stage = lib
        .root
        .join(dir)
        .join(format!(".kima-stage-{}-{name}", row.id));
    // The copy step finished (a separate file, not a link) and the checksum is journalled.
    db::set_sha(&pool, row.id, &sha(&src)).await.unwrap();
    std::fs::copy(&src, &stage).unwrap();
    let copied = std::fs::read(&stage).unwrap();
    // Then the source is rewritten.
    std::fs::write(&src, b"rewritten after the copy").unwrap();

    apply_with(&lib, run, &Hooks::default()).await.unwrap();
    assert_eq!(std::fs::read(&src).unwrap(), b"rewritten after the copy");
    assert_eq!(std::fs::read(&stage).unwrap(), copied);
    let row = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.id == row.id)
        .unwrap();
    assert_eq!(row.state, "failed");
    assert_eq!(
        row.note.as_deref(),
        Some("the source changed after it was copied; both files were kept")
    );
}

/// Same for a direct placement: the copy sits at the target, the source was rewritten.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn changed_source_after_a_direct_copy_is_kept(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "move").await;
    start(&lib, run, &["previewed"], "applying").await;
    let row = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.from_path == "messy/a.flac")
        .unwrap();
    let (src, dst) = (lib.root.join(&row.from_path), lib.root.join(&row.to_path));
    db::set_sha(&pool, row.id, &sha(&src)).await.unwrap();
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(&src, &dst).unwrap();
    std::fs::write(&src, b"rewritten").unwrap();

    apply_with(&lib, run, &Hooks::default()).await.unwrap();
    assert_eq!(std::fs::read(&src).unwrap(), b"rewritten");
    assert!(dst.exists());
    let row = rows(&lib, run)
        .await
        .into_iter()
        .find(|r| r.id == row.id)
        .unwrap();
    assert_eq!(row.state, "failed");
}

/// A refused undo puts the run back where it was, with the reason.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refused_undo_does_not_leave_the_run_undoing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let files = messy();
    let lib = library(&pool, &files).await;
    let run = new_run(&lib, "move").await;
    apply_run(&lib, run).await;
    let elsewhere = tempfile::tempdir().unwrap();
    lib.ctx.root.set(elsewhere.path().to_path_buf());
    start(&lib, run, &["done"], "undoing").await;
    // Through the job entry point, as the worker runs it.
    apply::run_job(&lib.ctx, run, Action::Undo).await.unwrap();
    let r = db::get_run(&pool, run).await.unwrap().unwrap();
    assert_eq!(r.state, "done");
    assert!(
        r.error
            .as_deref()
            .is_some_and(|e| e.contains("library root changed")),
        "{:?}",
        r.error
    );

    // From `failed`, it goes back to `failed`.
    db::fail_run(&pool, run, "earlier trouble").await.unwrap();
    start(&lib, run, &["failed"], "undoing").await;
    apply::run_job(&lib.ctx, run, Action::Undo).await.unwrap();
    assert_eq!(state(&lib, run).await, "failed");
    lib.ctx.root.set(lib.root.clone());
    // And once the root is back, the undo works.
    start(&lib, run, &["failed"], "undoing").await;
    apply::run_job(&lib.ctx, run, Action::Undo).await.unwrap();
    assert_eq!(state(&lib, run).await, "undone");
}
