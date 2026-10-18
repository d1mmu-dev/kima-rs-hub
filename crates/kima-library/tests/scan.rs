mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{Tags, make_audio, require_ffmpeg};
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_db::library::Guessed;
use kima_library::scan::{self, LibraryCtx, PathOutcome};
use uuid::Uuid;

fn ctx(pool: &PgPool, root: &Path) -> LibraryCtx {
    LibraryCtx {
        pool: pool.clone(),
        root: root.to_path_buf().into(),
        events: EventBus::new(64),
        data_dir: root.join(".kima-data"),
        ignore: Default::default(),
    }
}

fn tags(artist: &str, album: &str, title: &str, date: &str, track: &str) -> Tags {
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

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn track_id(pool: &PgPool, path: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM tracks WHERE path = $1")
        .bind(path)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn guessed(pool: &PgPool, path: &str) -> Guessed {
    let v: serde_json::Value = sqlx::query_scalar("SELECT guessed FROM tracks WHERE path = $1")
        .bind(path)
        .fetch_one(pool)
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

async fn missing_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM tracks WHERE missing_since IS NOT NULL")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Plays arrive in Task 14; a stand-in table that cascades like the real one will, so a
/// purged track would take its plays with it.
async fn add_play(pool: &PgPool, track: Uuid) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS test_plays (
             track_id uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO test_plays (track_id) VALUES ($1)")
        .bind(track)
        .execute(pool)
        .await
        .unwrap();
}

async fn plays_of(pool: &PgPool, track: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM test_plays WHERE track_id = $1")
        .bind(track)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Moves the file's mtime forward so a rewrite is always seen as a change.
fn bump_mtime(path: &Path) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reconcile_indexes_messy_tree(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_audio(
        root,
        "Radiohead/OK Computer/01.flac",
        tags("Radiohead", "OK Computer", "Airbag", "1997", "1"),
    );
    make_audio(
        root,
        "Radiohead/OK Computer/02.flac",
        tags("Radiohead", "OK Computer", "Paranoid Android", "1997", "2"),
    );
    make_audio(root, "Artist/1999 - Album/01 - x.mp3", Tags::default());
    for (n, artist) in [(1, "Singer A"), (2, "Singer B")] {
        make_audio(
            root,
            &format!("Compilations/Now 1/0{n}.flac"),
            Tags {
                artist: Some(artist.into()),
                album: Some("Now 1".into()),
                title: Some(format!("Song {n}")),
                track: Some(n.to_string()),
                compilation: true,
                ..Default::default()
            },
        );
    }
    std::fs::write(root.join("Radiohead/OK Computer/notes.txt"), "hi").unwrap();
    // Hidden folders and AppleDouble files are skipped, not reported as corrupt.
    make_audio(
        root,
        ".trash/old.flac",
        tags("Old", "Old", "x", "1990", "1"),
    );
    std::fs::write(
        root.join("Radiohead/OK Computer/._01.flac"),
        b"resource fork",
    )
    .unwrap();
    // Symlinks are not followed.
    std::os::unix::fs::symlink(
        root.join("Radiohead/OK Computer/01.flac"),
        root.join("Radiohead/link.flac"),
    )
    .unwrap();

    let report = scan::reconcile(&ctx(&pool, root)).await.unwrap();
    assert_eq!(report.added, 5, "{report:?}");
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(count(&pool, "artists").await, 3);
    assert_eq!(count(&pool, "albums").await, 3);
    assert_eq!(count(&pool, "tracks").await, 5);

    let g = guessed(&pool, "Artist/1999 - Album/01 - x.mp3").await;
    assert!(
        g.artist_from_path && g.album_from_path && g.year_from_path,
        "{g:?}"
    );
    assert!(g.track_from_name && g.title_from_name, "{g:?}");
    let (artist, album, year, title, no): (String, String, Option<i32>, String, i32) =
        sqlx::query_as(
            "SELECT a.name, b.title, b.year, t.title, t.track_no FROM tracks t
             JOIN albums b ON b.id = t.album_id JOIN artists a ON a.id = b.artist_id
             WHERE t.path = 'Artist/1999 - Album/01 - x.mp3'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        (artist.as_str(), album.as_str(), year, title.as_str(), no),
        ("Artist", "Album", Some(1999), "x", 1)
    );
    assert_eq!(
        guessed(&pool, "Radiohead/OK Computer/01.flac").await,
        Guessed::default()
    );

    let (va, comp): (String, bool) = sqlx::query_as(
        "SELECT a.name, b.is_compilation FROM albums b JOIN artists a ON a.id = b.artist_id
         WHERE b.title = 'Now 1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((va.as_str(), comp), ("Various Artists", true));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unchanged_files_are_skipped_on_rescan(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    make_audio(
        dir.path(),
        "A/B/02.flac",
        tags("A", "B", "two", "2000", "2"),
    );
    let c = ctx(&pool, dir.path());
    let first = scan::reconcile(&c).await.unwrap();
    assert_eq!((first.added, first.tags_read), (2, 2));
    let second = scan::reconcile(&c).await.unwrap();
    assert_eq!(second.updated, 0, "{second:?}");
    assert_eq!(second.added, 0);
    assert_eq!(second.unchanged, 2);
    assert_eq!(
        second.tags_read, 0,
        "tags::read must not run for unchanged files"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn edited_tags_update_row(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "Old", "2000", "1"),
    );
    scan::reconcile(&c).await.unwrap();
    let id = track_id(&pool, "A/B/01.flac").await;

    let p = make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "New Title", "2000", "1"),
    );
    bump_mtime(&p);
    let report = scan::reconcile(&c).await.unwrap();
    assert_eq!(report.updated, 1, "{report:?}");
    let title: String = sqlx::query_scalar("SELECT title FROM tracks WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title, "New Title");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deleted_file_marked_missing_not_deleted(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let p = make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    make_audio(
        dir.path(),
        "A/B/02.flac",
        tags("A", "B", "two", "2000", "2"),
    );
    scan::reconcile(&c).await.unwrap();
    std::fs::remove_file(p).unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert_eq!((report.missing, report.purged), (1, 0), "{report:?}");
    assert_eq!(count(&pool, "tracks").await, 2);
    assert_eq!(missing_count(&pool).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn moved_file_is_repointed_and_keeps_plays(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let p = make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    scan::reconcile(&c).await.unwrap();
    let id = track_id(&pool, "A/B/01.flac").await;
    add_play(&pool, id).await;

    std::fs::create_dir_all(dir.path().join("Elsewhere")).unwrap();
    std::fs::rename(p, dir.path().join("Elsewhere/renamed.flac")).unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert_eq!(
        (report.repointed, report.added, report.missing),
        (1, 0, 0),
        "{report:?}"
    );
    assert_eq!(track_id(&pool, "Elsewhere/renamed.flac").await, id);
    assert_eq!(count(&pool, "tracks").await, 1);
    assert_eq!(missing_count(&pool).await, 0);
    assert_eq!(plays_of(&pool, id).await, 1);
}

/// Review Focus 2: the library disk is unplugged (an empty mount point is left behind),
/// stays away for longer than the grace period, and comes back.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unplug_and_replug_keeps_history(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let disk = tempfile::tempdir().unwrap();
    let mount = disk.path().join("music");
    let c = ctx(&pool, &mount);
    for n in 1..=3 {
        make_audio(
            &mount,
            &format!("A/B/0{n}.flac"),
            tags("A", "B", &format!("t{n}"), "2000", &n.to_string()),
        );
    }
    make_audio(&mount, "C/D/01.mp3", tags("C", "D", "x", "2010", "1"));
    scan::reconcile(&c).await.unwrap();
    let paths = ["A/B/01.flac", "A/B/02.flac", "A/B/03.flac", "C/D/01.mp3"];
    let mut ids = Vec::new();
    for p in paths {
        let id = track_id(&pool, p).await;
        add_play(&pool, id).await;
        ids.push(id);
    }

    // Unplugged: the mount point is still there but empty. Nothing is marked missing.
    let away = disk.path().join("away");
    std::fs::rename(&mount, &away).unwrap();
    std::fs::create_dir(&mount).unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert!(report.root_empty_guard, "{report:?}");
    assert_eq!((report.missing, report.purged), (0, 0), "{report:?}");
    assert_eq!(missing_count(&pool).await, 0);

    // A single-file event for one of the vanished files changes nothing either.
    assert_eq!(
        scan::update_path(&c, "A/B/01.flac").await.unwrap(),
        PathOutcome::RootUnavailable
    );
    assert_eq!(missing_count(&pool).await, 0);

    // Weeks later, still unplugged: still nothing missing, nothing purged.
    let report = scan::reconcile(&c).await.unwrap();
    assert!(report.root_empty_guard && report.purged == 0, "{report:?}");
    assert_eq!(count(&pool, "tracks").await, 4);
    assert_eq!(count(&pool, "albums").await, 2);

    // Plugged back in: everything is intact and unchanged.
    std::fs::remove_dir(&mount).unwrap();
    std::fs::rename(&away, &mount).unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert!(!report.root_empty_guard, "{report:?}");
    assert_eq!((report.unchanged, report.added), (4, 0), "{report:?}");
    assert_eq!(missing_count(&pool).await, 0);
    for (p, id) in paths.iter().zip(&ids) {
        assert_eq!(track_id(&pool, p).await, *id, "{p}");
        assert_eq!(plays_of(&pool, *id).await, 1, "{p}");
    }
}

/// Only some files deleted: they are marked missing, kept for the grace period, then purged.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_tracks_purged_after_grace(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let p = make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    make_audio(
        dir.path(),
        "C/D/01.flac",
        tags("C", "D", "two", "2001", "1"),
    );
    scan::reconcile(&c).await.unwrap();
    let gone = track_id(&pool, "A/B/01.flac").await;
    add_play(&pool, gone).await;
    std::fs::remove_file(p).unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert_eq!(
        (report.missing, report.root_empty_guard),
        (1, false),
        "{report:?}"
    );

    sqlx::query("UPDATE tracks SET missing_since = now() - interval '6 days' WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert_eq!(report.purged, 0, "{report:?}");
    assert_eq!(plays_of(&pool, gone).await, 1);

    sqlx::query("UPDATE tracks SET missing_since = now() - interval '8 days' WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert_eq!(report.purged, 1, "{report:?}");
    assert_eq!(count(&pool, "tracks").await, 1);
    // The emptied album and artist are pruned too.
    assert_eq!(count(&pool, "albums").await, 1);
    assert_eq!(count(&pool, "artists").await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn purge_removes_artwork_folder(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let p = make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    make_audio(
        dir.path(),
        "C/D/01.flac",
        tags("C", "D", "two", "2001", "1"),
    );
    scan::reconcile(&c).await.unwrap();
    let album_of = |title: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT a.id FROM albums a JOIN tracks t ON t.album_id = a.id WHERE t.title = $1",
            )
            .bind(title)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let (gone, kept) = (album_of("one").await, album_of("two").await);
    for id in [gone, kept] {
        let folder = c.data_dir.join("artwork").join(id.to_string());
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("orig.jpg"), b"x").unwrap();
    }

    std::fs::remove_file(p).unwrap();
    scan::reconcile(&c).await.unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() - interval '8 days' WHERE title = 'one'")
        .execute(&pool)
        .await
        .unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert_eq!(report.purged, 1, "{report:?}");
    let art = c.data_dir.join("artwork");
    assert!(!art.join(gone.to_string()).exists());
    assert!(art.join(kept.to_string()).join("orig.jpg").exists());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn update_path_root_unavailable(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("music");
    let c = ctx(&pool, &root);
    make_audio(&root, "A/B/01.flac", tags("A", "B", "one", "2000", "1"));
    assert_eq!(
        scan::update_path(&c, "A/B/01.flac").await.unwrap(),
        PathOutcome::Added
    );
    // Root gone entirely.
    let away = dir.path().join("away");
    std::fs::rename(&root, &away).unwrap();
    assert_eq!(
        scan::update_path(&c, "A/B/01.flac").await.unwrap(),
        PathOutcome::RootUnavailable
    );
    // Root present but empty.
    std::fs::create_dir(&root).unwrap();
    assert_eq!(
        scan::update_path(&c, "A/B/01.flac").await.unwrap(),
        PathOutcome::RootUnavailable
    );
    // Placeholders left on an unmounted mount point do not count as a library.
    std::fs::write(root.join(".DS_Store"), "x").unwrap();
    std::fs::create_dir(root.join("lost+found")).unwrap();
    std::fs::write(root.join("lost+found/notes.txt"), "x").unwrap();
    make_audio(&root, ".Trash/old.flac", tags("A", "B", "x", "2000", "9"));
    assert_eq!(
        scan::update_path(&c, "A/B/01.flac").await.unwrap(),
        PathOutcome::RootUnavailable
    );
    assert_eq!(missing_count(&pool).await, 0);
    // A root with other audio: the file really is gone.
    make_audio(&root, "Other/x.flac", tags("O", "O", "x", "2000", "1"));
    assert_eq!(
        scan::update_path(&c, "A/B/01.flac").await.unwrap(),
        PathOutcome::MarkedMissing
    );
}

/// Single-file updates (or an older run) already marked the whole library missing; the root
/// is now empty. The guard still trips, so the grace period never starts the purge.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn guard_holds_when_everything_is_already_missing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("music");
    let c = ctx(&pool, &root);
    make_audio(&root, "A/B/01.flac", tags("A", "B", "one", "2000", "1"));
    make_audio(&root, "C/D/01.flac", tags("C", "D", "two", "2001", "1"));
    scan::reconcile(&c).await.unwrap();
    let id = track_id(&pool, "A/B/01.flac").await;
    add_play(&pool, id).await;

    std::fs::remove_dir_all(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() - interval '30 days'")
        .execute(&pool)
        .await
        .unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert!(report.root_empty_guard, "{report:?}");
    assert_eq!(report.purged, 0, "{report:?}");
    assert_eq!(count(&pool, "tracks").await, 2);
    assert_eq!(plays_of(&pool, id).await, 1);
}

/// The re-point and the metadata upsert share one transaction: when the upsert fails, the
/// track stays missing at its old path instead of being half-moved.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_upsert_rolls_back_the_repoint(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let p = make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    make_audio(
        dir.path(),
        "A/B/02.flac",
        tags("A", "B", "two", "2000", "2"),
    );
    scan::reconcile(&c).await.unwrap();
    let id = track_id(&pool, "A/B/01.flac").await;

    // Fails the upsert's UPDATE (which sets `title`) but not the re-point (which does not).
    sqlx::query(
        "CREATE FUNCTION boom() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'injected failure'; END $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER boom BEFORE UPDATE OF title ON tracks FOR EACH ROW
         WHEN (NEW.path LIKE 'Elsewhere/%') EXECUTE FUNCTION boom()",
    )
    .execute(&pool)
    .await
    .unwrap();

    std::fs::create_dir_all(dir.path().join("Elsewhere")).unwrap();
    std::fs::rename(p, dir.path().join("Elsewhere/01.flac")).unwrap();
    let report = scan::reconcile(&c).await.unwrap();
    assert_eq!(report.repointed, 0, "{report:?}");
    assert!(
        report
            .failed
            .iter()
            .any(|(path, e)| path == "Elsewhere/01.flac" && e.contains("injected failure")),
        "{report:?}"
    );
    let (path, missing): (String, bool) =
        sqlx::query_as("SELECT path, missing_since IS NOT NULL FROM tracks WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((path.as_str(), missing), ("A/B/01.flac", true));
    assert_eq!(count(&pool, "tracks").await, 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unreadable_root_marks_nothing(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("music");
    let c = ctx(&pool, &root);
    make_audio(&root, "A/B/01.flac", tags("A", "B", "one", "2000", "1"));
    make_audio(&root, "C/D/01.flac", tags("C", "D", "one", "2000", "1"));
    scan::reconcile(&c).await.unwrap();

    // An unreadable sub-folder: its tracks are not marked missing, and it is reported.
    let sub = root.join("C");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
    let report = scan::reconcile(&c).await;
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.unwrap();
    assert_eq!(report.missing, 0, "{report:?}");
    assert!(report.failed.iter().any(|(p, _)| p == "C"), "{report:?}");

    // A missing root is an error, not "everything was deleted".
    let gone = dir.path().join("gone");
    std::fs::rename(&root, &gone).unwrap();
    assert!(scan::reconcile(&c).await.is_err());
    assert_eq!(missing_count(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn corrupt_file_is_reported_not_fatal(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    std::fs::write(dir.path().join("A/B/02.mp3"), b"this is not audio").unwrap();
    let report = scan::reconcile(&ctx(&pool, dir.path())).await.unwrap();
    assert_eq!(report.added, 1, "{report:?}");
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].0, "A/B/02.mp3");
    assert_eq!(count(&pool, "tracks").await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_year_is_folder_mode(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for (n, y) in [(1, "1999"), (2, "1999"), (3, "2001")] {
        make_audio(
            dir.path(),
            &format!("A/B/0{n}.flac"),
            tags("A", "B", &format!("t{n}"), y, &n.to_string()),
        );
    }
    // A tie goes to the earliest year.
    for (n, y) in [(1, "2005"), (2, "2003")] {
        make_audio(
            dir.path(),
            &format!("C/D/0{n}.flac"),
            tags("C", "D", &format!("t{n}"), y, &n.to_string()),
        );
    }
    scan::reconcile(&ctx(&pool, dir.path())).await.unwrap();
    let years: Vec<(String, Option<i32>)> =
        sqlx::query_as("SELECT title, year FROM albums ORDER BY title")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        years,
        vec![("B".to_string(), Some(1999)), ("D".to_string(), Some(2003))]
    );
    assert!(!guessed(&pool, "A/B/01.flac").await.year_conflict);
    assert!(guessed(&pool, "A/B/03.flac").await.year_conflict);
    assert!(guessed(&pool, "C/D/01.flac").await.year_conflict);
    let own: Option<i32> = sqlx::query_scalar("SELECT year FROM tracks WHERE path = 'A/B/03.flac'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(own, Some(2001));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn update_path_single_file(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let rel = "A/B/01.flac";
    let p = make_audio(dir.path(), rel, tags("A", "B", "one", "2000", "1"));
    assert_eq!(
        scan::update_path(&c, rel).await.unwrap(),
        PathOutcome::Added
    );
    let id = track_id(&pool, rel).await;
    assert_eq!(
        scan::update_path(&c, rel).await.unwrap(),
        PathOutcome::Unchanged
    );

    make_audio(dir.path(), rel, tags("A", "B", "changed", "2000", "1"));
    bump_mtime(&p);
    assert_eq!(
        scan::update_path(&c, rel).await.unwrap(),
        PathOutcome::Updated
    );
    assert_eq!(track_id(&pool, rel).await, id);

    // Another audio file keeps the root "available" (an audio-less root is treated as
    // unmounted; see update_path_root_unavailable).
    make_audio(
        dir.path(),
        "Z/keep.flac",
        tags("Z", "Z", "keep", "2000", "1"),
    );
    std::fs::remove_file(&p).unwrap();
    assert_eq!(
        scan::update_path(&c, rel).await.unwrap(),
        PathOutcome::MarkedMissing
    );
    assert_eq!(missing_count(&pool).await, 1);

    // Back under a new name: same track.
    let moved = "A/B/01 - one.flac";
    make_audio(dir.path(), moved, tags("A", "B", "changed", "2000", "1"));
    // ffmpeg output is deterministic, so the fingerprint matches the vanished file.
    let outcome = scan::update_path(&c, moved).await.unwrap();
    assert_eq!(outcome, PathOutcome::Repointed);
    assert_eq!(track_id(&pool, moved).await, id);

    std::fs::write(dir.path().join("A/B/notes.txt"), "x").unwrap();
    assert_eq!(
        scan::update_path(&c, "A/B/notes.txt").await.unwrap(),
        PathOutcome::Ignored
    );
    assert_eq!(
        scan::update_path(&c, "../escape.flac").await.unwrap(),
        PathOutcome::Ignored
    );
    make_audio(
        dir.path(),
        "A/B/._02.flac",
        tags("A", "B", "x", "2000", "2"),
    );
    assert_eq!(
        scan::update_path(&c, "A/B/._02.flac").await.unwrap(),
        PathOutcome::Ignored
    );
    std::fs::write(dir.path().join("A/B/bad.mp3"), b"junk").unwrap();
    assert!(matches!(
        scan::update_path(&c, "A/B/bad.mp3").await.unwrap(),
        PathOutcome::Failed(_)
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn update_path_new_file_joins_existing_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    for n in 1..=2 {
        make_audio(
            dir.path(),
            &format!("A/B/0{n}.flac"),
            tags("A", "B", &format!("t{n}"), "1999", &n.to_string()),
        );
    }
    scan::reconcile(&c).await.unwrap();
    make_audio(dir.path(), "A/B/03.flac", tags("A", "B", "t3", "2004", "3"));
    assert_eq!(
        scan::update_path(&c, "A/B/03.flac").await.unwrap(),
        PathOutcome::Added
    );
    assert_eq!(count(&pool, "albums").await, 1);
    assert!(guessed(&pool, "A/B/03.flac").await.year_conflict);

    // An earlier year in a two-two split wins the tie: the whole folder moves together.
    make_audio(dir.path(), "A/B/04.flac", tags("A", "B", "t4", "1990", "4"));
    make_audio(dir.path(), "A/B/05.flac", tags("A", "B", "t5", "1990", "5"));
    scan::update_path(&c, "A/B/04.flac").await.unwrap();
    scan::update_path(&c, "A/B/05.flac").await.unwrap();
    let albums: Vec<Option<i32>> = sqlx::query_scalar("SELECT year FROM albums")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(albums, vec![Some(1990)]);
    let tracks_on_album: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tracks t JOIN albums b ON b.id = t.album_id WHERE b.year = 1990",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(tracks_on_album, 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reconcile_publishes_progress(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    let c = ctx(&pool, dir.path());
    let mut rx = c.events.subscribe();
    scan::reconcile(&c).await.unwrap();
    let mut last = None;
    while let Ok(ev) = rx.try_recv() {
        assert!(ev.user_id.is_none());
        last = Some(ev.data);
    }
    assert_eq!(
        last.unwrap(),
        serde_json::json!({"type":"library:progress","job":"scan","done":1,"total":1})
    );
}
