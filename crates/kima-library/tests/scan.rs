mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{Picard, Tags, make_audio, require_ffmpeg, tag_picard};
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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reconcile_cancellable_stops_between_folders(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let gone = make_audio(
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
    std::fs::remove_file(gone).unwrap();
    make_audio(
        dir.path(),
        "C/D/01.flac",
        tags("C", "D", "new", "2001", "1"),
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let result = scan::reconcile_cancellable(&c, &cancel).await;
    assert!(
        matches!(result, Err(scan::ScanError::Cancelled)),
        "{result:?}"
    );
    assert_eq!(missing_count(&pool).await, 0, "nothing marked missing");
    assert_eq!(count(&pool, "tracks").await, 2, "nothing added");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_job_reports_changes_and_calls_on_change(pool: PgPool) {
    use kima_jobs::worker::{JobCtx, JobError};
    use kima_library::jobs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let cancel = tokio_util::sync::CancellationToken::new();
    let reconcile = serde_json::json!({"kind": "reconcile"});

    let bad = jobs::run(&c, &serde_json::json!({"kind": "nope"}), &cancel).await;
    assert!(matches!(bad, Err(JobError::Permanent(_))), "{bad:?}");
    // An empty library: nothing changed.
    assert!(!jobs::run(&c, &reconcile, &cancel).await.unwrap());
    if !require_ffmpeg() {
        return;
    }
    make_audio(
        dir.path(),
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let on_change: jobs::OnChange = {
        let calls = calls.clone();
        Arc::new(move |_pool| {
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        })
    };
    let handler = jobs::handler_with(c.clone(), on_change);
    let job = kima_jobs::queue::Job {
        id: Uuid::now_v7(),
        queue: jobs::QUEUE.into(),
        payload: reconcile.clone(),
        attempts: 1,
        max_attempts: 3,
    };
    let job_ctx = JobCtx {
        cancel: cancel.clone(),
    };
    handler(job.clone(), job_ctx.clone()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a track was added");
    handler(job, job_ctx).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "nothing changed the second time"
    );
}

// ---- Identity tags, file genres, embedded lyrics and stage reopening (M3a Task 10) ----

const REC: &str = "a8b4c3d2-1e0f-4a9b-8c7d-6e5f4a3b2c1d";
const RTRACK: &str = "b1b2b3b4-c1c2-4d1d-9e1e-f1f2f3f4f5f6";
const REL_A: &str = "10000000-0000-4000-8000-00000000000a";
const REL_B: &str = "20000000-0000-4000-8000-00000000000b";
const RG_A: &str = "30000000-0000-4000-8000-00000000000a";
const RG_B: &str = "40000000-0000-4000-8000-00000000000b";
const A1: &str = "e1e2e3e4-0000-4000-8000-000000000001";
const A2: &str = "e1e2e3e4-0000-4000-8000-000000000002";
const VA_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

fn u(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

/// Writes a fresh file (replacing any old one) with plain tags, then `p` over it, and moves
/// its mtime forward so a rescan always re-reads it.
fn picard_audio(root: &Path, rel: &str, t: Tags, p: &Picard<'_>) {
    let path = make_audio(root, rel, t);
    tag_picard(&path, p);
    bump_mtime(&path);
}

async fn rescan(pool: &PgPool, root: &Path) {
    let r = scan::reconcile(&ctx(pool, root)).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
}

async fn exec(pool: &PgPool, sql: &str, id: Uuid) {
    sqlx::query(sql).bind(id).execute(pool).await.unwrap();
}

async fn album_of(pool: &PgPool, path: &str) -> Uuid {
    sqlx::query_scalar("SELECT album_id FROM tracks WHERE path = $1")
        .bind(path)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn album_mbids(
    pool: &PgPool,
    album: Uuid,
) -> (Option<Uuid>, Option<String>, Option<Uuid>, Option<String>) {
    sqlx::query_as(
        "SELECT release_mbid, release_mbid_source, release_group_mbid, release_group_mbid_source
         FROM albums WHERE id = $1",
    )
    .bind(album)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn artist_mbid(pool: &PgPool, name_key: &str) -> (Option<Uuid>, Option<String>) {
    sqlx::query_as("SELECT mbid, mbid_source FROM artists WHERE name_key = $1")
        .bind(name_key)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn track_genre_names(pool: &PgPool, track: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT g.name FROM track_genres tg JOIN genres g ON g.id = tg.genre_id
         WHERE tg.track_id = $1 AND tg.source = 'file' ORDER BY tg.rank",
    )
    .bind(track)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn seed_stages(pool: &PgPool, track: Uuid) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, attempts, redelivered, last_error,
                                   error_code, error_class, evidence, next_attempt_at)
         VALUES ($1, 'fingerprint', 'done', 1, 0, NULL, NULL, NULL, '{}', NULL),
                ($1, 'recording', 'failed', 3, 2, 'boom', 'HTTP_503', NULL, '{\"x\":1}', now())",
    )
    .bind(track)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_acoustid (track_id, fingerprint, duration_secs) VALUES ($1, 'AQAA', 2)",
    )
    .bind(track)
    .execute(pool)
    .await
    .unwrap();
}

/// (status, attempts, redelivered, evidence) per stage, ordered by stage.
async fn stages(pool: &PgPool, track: Uuid) -> Vec<(String, i16, i16, Option<serde_json::Value>)> {
    sqlx::query_as(
        "SELECT status, attempts, redelivered, evidence FROM track_stages
         WHERE track_id = $1 ORDER BY stage",
    )
    .bind(track)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn acoustid_rows(pool: &PgPool, track: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM track_acoustid WHERE track_id = $1")
        .bind(track)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn picard_tagged_track_stores_ids(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    picard_audio(
        root,
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
        &Picard {
            recording: Some(REC),
            release_track: Some(RTRACK),
            release: Some(REL_A),
            release_group: Some(RG_A),
            artists: vec![A1, A2],
            album_artists: vec![A1],
            isrc: Some("us-rc1-76-07839"),
            disc: Some((1, 2)),
            track: Some((3, 12)),
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    #[allow(clippy::type_complexity)]
    let row: (
        Option<Uuid>,
        Option<Uuid>,
        Option<Uuid>,
        Option<Uuid>,
        Vec<Uuid>,
        Vec<Uuid>,
        Option<i32>,
        Option<i32>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT tag_recording_mbid, tag_release_track_mbid, tag_release_mbid,
                tag_release_group_mbid, tag_artist_mbids, tag_album_artist_mbids,
                disc_total, track_total, isrc, isrc_source
         FROM tracks WHERE path = 'A/B/01.flac'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            Some(u(REC)),
            Some(u(RTRACK)),
            Some(u(REL_A)),
            Some(u(RG_A)),
            vec![u(A1), u(A2)],
            vec![u(A1)],
            Some(2),
            Some(12),
            Some("USRC17607839".to_string()),
            Some("id3".to_string()),
        )
    );
    let album = album_of(&pool, "A/B/01.flac").await;
    assert_eq!(
        album_mbids(&pool, album).await,
        (
            Some(u(REL_A)),
            Some("tag".into()),
            Some(u(RG_A)),
            Some("tag".into())
        )
    );
    assert_eq!(
        artist_mbid(&pool, "a").await,
        (Some(u(A1)), Some("tag".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_mbids_are_mode_ties_smallest(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (n, rel, rg) in [(1, REL_B, RG_B), (2, REL_A, RG_A), (3, REL_A, RG_A)] {
        picard_audio(
            root,
            &format!("A/Three/0{n}.flac"),
            tags("A", "Three", "t", "2000", &n.to_string()),
            &Picard {
                release: Some(rel),
                release_group: Some(rg),
                ..Default::default()
            },
        );
    }
    // A tie: the larger id comes first in the folder.
    for (n, rel, rg) in [(1, REL_B, RG_B), (2, REL_A, RG_A)] {
        picard_audio(
            root,
            &format!("A/Two/0{n}.flac"),
            tags("A", "Two", "t", "2000", &n.to_string()),
            &Picard {
                release: Some(rel),
                release_group: Some(rg),
                ..Default::default()
            },
        );
    }
    rescan(&pool, root).await;
    for album in ["A/Three/01.flac", "A/Two/01.flac"] {
        let id = album_of(&pool, album).await;
        let (rel, _, rg, _) = album_mbids(&pool, id).await;
        assert_eq!((rel, rg), (Some(u(REL_A)), Some(u(RG_A))), "{album}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rescan_keeps_user_and_autotag_album_mbids(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = |rel: &'static str, rg: &'static str| Picard {
        release: Some(rel),
        release_group: Some(rg),
        ..Default::default()
    };
    picard_audio(
        root,
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
        &file(REL_A, RG_A),
    );
    rescan(&pool, root).await;
    let album = album_of(&pool, "A/B/01.flac").await;
    let user = Uuid::now_v7();
    let auto = Uuid::now_v7();
    sqlx::query(
        "UPDATE albums SET release_group_mbid = $2, release_group_mbid_source = 'user',
                           release_mbid = $3, release_mbid_source = 'autotag' WHERE id = $1",
    )
    .bind(album)
    .bind(user)
    .bind(auto)
    .execute(&pool)
    .await
    .unwrap();
    picard_audio(
        root,
        "A/B/01.flac",
        tags("A", "B", "one", "2000", "1"),
        &file(REL_B, RG_B),
    );
    rescan(&pool, root).await;
    assert_eq!(album_of(&pool, "A/B/01.flac").await, album);
    assert_eq!(
        album_mbids(&pool, album).await,
        (
            Some(auto),
            Some("autotag".into()),
            Some(user),
            Some("user".into())
        )
    );
    // The editor's Reset clears the value and its source; the next read fills it from tags.
    exec(
        &pool,
        "UPDATE albums SET release_group_mbid = NULL, release_group_mbid_source = NULL WHERE id = $1",
        album,
    )
    .await;
    bump_mtime(&root.join("A/B/01.flac"));
    rescan(&pool, root).await;
    assert_eq!(
        album_mbids(&pool, album).await,
        (
            Some(auto),
            Some("autotag".into()),
            Some(u(RG_B)),
            Some("tag".into())
        )
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_mbid_from_album_artist_tag(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    picard_audio(
        root,
        "AA/B/01.flac",
        Tags {
            album_artist: Some("AA".into()),
            artist: Some("Someone feat. Other".into()),
            album: Some("B".into()),
            title: Some("t".into()),
            ..Default::default()
        },
        &Picard {
            album_artists: vec![A1],
            artists: vec![A2],
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    assert_eq!(
        artist_mbid(&pool, "aa").await,
        (Some(u(A1)), Some("tag".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_mbid_from_artist_tag_with_featured(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    picard_audio(
        root,
        "X/B/01.flac",
        Tags {
            artist: Some("A feat. B".into()),
            album: Some("B".into()),
            title: Some("t".into()),
            ..Default::default()
        },
        &Picard {
            artists: vec![A1, A2],
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    assert_eq!(
        artist_mbid(&pool, "a").await,
        (Some(u(A1)), Some("tag".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_mbid_skipped_when_counts_disagree(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Two album-artist ids for one album artist.
    picard_audio(
        root,
        "AA/B/01.flac",
        Tags {
            album_artist: Some("AA".into()),
            album: Some("B".into()),
            title: Some("t".into()),
            ..Default::default()
        },
        &Picard {
            album_artists: vec![A1, A2],
            ..Default::default()
        },
    );
    // Two artist ids for an artist credit without featured artists.
    picard_audio(
        root,
        "Solo/B/01.flac",
        Tags {
            artist: Some("Solo".into()),
            album: Some("B".into()),
            title: Some("t".into()),
            ..Default::default()
        },
        &Picard {
            artists: vec![A1, A2],
            ..Default::default()
        },
    );
    // An album-artist tag with only artist ids: the album-artist list decides.
    picard_audio(
        root,
        "Other/B/01.flac",
        Tags {
            album_artist: Some("Other".into()),
            artist: Some("Other".into()),
            album: Some("B".into()),
            title: Some("t".into()),
            ..Default::default()
        },
        &Picard {
            artists: vec![A1],
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    for key in ["aa", "solo", "other"] {
        assert_eq!(artist_mbid(&pool, key).await, (None, None), "{key}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_mbid_never_replaces_user(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let p = Picard {
        album_artists: vec![A1],
        ..Default::default()
    };
    picard_audio(root, "A/B/01.flac", tags("A", "B", "one", "2000", "1"), &p);
    rescan(&pool, root).await;
    let user = Uuid::now_v7();
    sqlx::query("UPDATE artists SET mbid = $1, mbid_source = 'user' WHERE name_key = 'a'")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    picard_audio(root, "A/B/01.flac", tags("A", "B", "one", "2000", "1"), &p);
    rescan(&pool, root).await;
    assert_eq!(
        artist_mbid(&pool, "a").await,
        (Some(user), Some("user".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_isrc_wins_over_musicbrainz(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let t = || tags("A", "B", "one", "2000", "1");
    picard_audio(root, "A/B/01.flac", t(), &Picard::default());
    rescan(&pool, root).await;
    let id = track_id(&pool, "A/B/01.flac").await;
    let isrc = |pool: PgPool| async move {
        sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT isrc, isrc_source FROM tracks WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    exec(
        &pool,
        "UPDATE tracks SET isrc = 'GBAYE0000001', isrc_source = 'musicbrainz' WHERE id = $1",
        id,
    )
    .await;
    // A file without an ISRC leaves the MusicBrainz one.
    bump_mtime(&root.join("A/B/01.flac"));
    rescan(&pool, root).await;
    assert_eq!(
        isrc(pool.clone()).await,
        (Some("GBAYE0000001".into()), Some("musicbrainz".into()))
    );
    picard_audio(
        root,
        "A/B/01.flac",
        t(),
        &Picard {
            isrc: Some("USRC17607839"),
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    assert_eq!(
        isrc(pool.clone()).await,
        (Some("USRC17607839".into()), Some("id3".into()))
    );
    // Removing it from the file clears an `id3` value.
    picard_audio(root, "A/B/01.flac", t(), &Picard::default());
    rescan(&pool, root).await;
    assert_eq!(isrc(pool.clone()).await, (None, None));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_genres_replaced_wholesale(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let t = || tags("A", "B", "one", "2000", "1");
    picard_audio(
        root,
        "A/B/01.flac",
        t(),
        &Picard {
            genres: vec!["Rock", "Pop"],
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    let id = track_id(&pool, "A/B/01.flac").await;
    assert_eq!(track_genre_names(&pool, id).await, vec!["Rock", "Pop"]);
    picard_audio(
        root,
        "A/B/01.flac",
        t(),
        &Picard {
            genres: vec!["Jazz"],
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    assert_eq!(track_genre_names(&pool, id).await, vec!["Jazz"]);
    picard_audio(root, "A/B/01.flac", t(), &Picard::default());
    rescan(&pool, root).await;
    assert!(track_genre_names(&pool, id).await.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_and_artist_file_genres_top5_by_track_count(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let files: [(&str, Vec<&str>); 5] = [
        ("A/One/01.flac", vec!["A", "B", "C", "D", "E", "F", "G"]),
        ("A/One/02.flac", vec!["G", "F"]),
        ("A/One/03.flac", vec!["G"]),
        ("A/Two/01.flac", vec!["B"]),
        ("A/Two/02.flac", vec!["B"]),
    ];
    for (n, (rel, genres)) in files.iter().enumerate() {
        let album = rel.split('/').nth(1).unwrap();
        picard_audio(
            root,
            rel,
            tags("A", album, "t", "2000", &(n + 1).to_string()),
            &Picard {
                genres: genres.clone(),
                ..Default::default()
            },
        );
    }
    rescan(&pool, root).await;
    let album = album_of(&pool, "A/One/01.flac").await;
    let album_genres: Vec<String> = sqlx::query_scalar(
        "SELECT g.name FROM album_genres x JOIN genres g ON g.id = x.genre_id
         WHERE x.album_id = $1 AND x.source = 'file' ORDER BY x.rank",
    )
    .bind(album)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(album_genres, vec!["G", "F", "A", "B", "C"]);
    let artist_genres: Vec<String> = sqlx::query_scalar(
        "SELECT g.name FROM artist_genres x JOIN genres g ON g.id = x.genre_id
         JOIN artists a ON a.id = x.artist_id
         WHERE a.name_key = 'a' AND x.source = 'file' ORDER BY x.rank",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(artist_genres, vec!["B", "G", "F", "A", "C"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedded_lyrics_upserted_then_removed(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let t = || tags("A", "B", "one", "2000", "1");
    let with = Picard {
        uslt: Some("Hello there"),
        ..Default::default()
    };
    picard_audio(root, "A/B/01.flac", t(), &with);
    rescan(&pool, root).await;
    let id = track_id(&pool, "A/B/01.flac").await;
    let row = |pool: PgPool| async move {
        sqlx::query_as::<_, (Option<String>, Option<String>, String, Option<i32>)>(
            "SELECT plain_lyrics, synced_lyrics, source, lrclib_id FROM track_lyrics WHERE track_id = $1",
        )
        .bind(id)
        .fetch_optional(&pool)
        .await
        .unwrap()
    };
    assert_eq!(
        row(pool.clone()).await,
        Some((Some("Hello there".into()), None, "embedded".into(), None))
    );
    // Lyrics gone from the file: the embedded row goes too (D-115).
    picard_audio(root, "A/B/01.flac", t(), &Picard::default());
    rescan(&pool, root).await;
    assert_eq!(row(pool.clone()).await, None);
    // An LRCLIB row is kept while the file has none, and replaced once it gains some.
    exec(
        &pool,
        "INSERT INTO track_lyrics (track_id, plain_lyrics, source, lrclib_id)
         VALUES ($1, 'from lrclib', 'lrclib', 7)",
        id,
    )
    .await;
    bump_mtime(&root.join("A/B/01.flac"));
    rescan(&pool, root).await;
    assert_eq!(
        row(pool.clone()).await,
        Some((Some("from lrclib".into()), None, "lrclib".into(), Some(7)))
    );
    picard_audio(
        root,
        "A/B/01.flac",
        t(),
        &Picard {
            uslt: Some("[00:01.00] synced"),
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    assert_eq!(
        row(pool.clone()).await,
        Some((
            None,
            Some("[00:01.00] synced".into()),
            "embedded".into(),
            None
        ))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn changed_file_reopens_stages_and_drops_fingerprint(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_audio(root, "A/B/01.flac", tags("A", "B", "one", "2000", "1"));
    rescan(&pool, root).await;
    let id = track_id(&pool, "A/B/01.flac").await;
    seed_stages(&pool, id).await;
    // Same content, new mtime: re-read, nothing reopened.
    bump_mtime(&root.join("A/B/01.flac"));
    rescan(&pool, root).await;
    assert_eq!(stages(&pool, id).await[0].0, "done");
    assert_eq!(acoustid_rows(&pool, id).await, 1);

    make_audio(
        root,
        "A/B/01.flac",
        tags("A", "B", "one, edited", "2000", "1"),
    );
    bump_mtime(&root.join("A/B/01.flac"));
    rescan(&pool, root).await;
    assert_eq!(track_id(&pool, "A/B/01.flac").await, id);
    let reason = serde_json::json!({"reason": "file changed"});
    assert_eq!(
        stages(&pool, id).await,
        vec![
            ("pending".into(), 0, 0, Some(reason.clone())),
            ("pending".into(), 0, 0, Some(reason)),
        ]
    );
    let cleared: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM track_stages WHERE track_id = $1 AND next_attempt_at IS NULL
           AND last_error IS NULL AND error_code IS NULL AND error_class IS NULL",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cleared.0, 2);
    assert_eq!(acoustid_rows(&pool, id).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sibling_reread_does_not_reopen(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_audio(root, "A/B/01.flac", tags("A", "B", "one", "2001", "1"));
    make_audio(root, "A/B/02.flac", tags("A", "B", "two", "1999", "2"));
    make_audio(root, "A/B/03.flac", tags("A", "B", "three", "1999", "3"));
    rescan(&pool, root).await;
    let sibling = track_id(&pool, "A/B/03.flac").await;
    seed_stages(&pool, sibling).await;
    // 02 moves to 2001, so the album year moves and 03 is re-read with unchanged content.
    make_audio(root, "A/B/02.flac", tags("A", "B", "two", "2001", "2"));
    bump_mtime(&root.join("A/B/02.flac"));
    let r = scan::reconcile(&ctx(&pool, root)).await.unwrap();
    assert_eq!(r.tags_read, 3, "{r:?}");
    let year: Option<i32> = sqlx::query_scalar(
        "SELECT b.year FROM tracks t JOIN albums b ON b.id = t.album_id WHERE t.id = $1",
    )
    .bind(sibling)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(year, Some(2001));
    let st = stages(&pool, sibling).await;
    assert_eq!(st[0].0, "done");
    assert_eq!(st[1].0, "failed");
    assert_eq!(acoustid_rows(&pool, sibling).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn restore_reopens_file_missing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_audio(root, "A/B/01.flac", tags("A", "B", "one", "2000", "1"));
    make_audio(root, "A/B/02.flac", tags("A", "B", "two", "2000", "2"));
    // Keeps the library non-empty, so the unmounted-disk guard does not hold.
    make_audio(root, "K/K/01.flac", tags("K", "K", "keep", "2000", "1"));
    rescan(&pool, root).await;
    let one = track_id(&pool, "A/B/01.flac").await;
    let two = track_id(&pool, "A/B/02.flac").await;
    let park = tempfile::tempdir().unwrap();
    for (name, id) in [("01.flac", one), ("02.flac", two)] {
        std::fs::rename(root.join("A/B").join(name), park.path().join(name)).unwrap();
        sqlx::query(
            "INSERT INTO track_stages (track_id, stage, status, attempts, error_class, last_error)
             VALUES ($1, 'fingerprint', 'failed', 1, 'FILE_MISSING', 'gone'),
                    ($1, 'recording', 'failed', 4, 'PERMANENT', 'no')",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }
    rescan(&pool, root).await;
    assert_eq!(missing_count(&pool).await, 2);
    // 01 comes back where it was (a restore); 02 comes back elsewhere (a re-point).
    std::fs::rename(park.path().join("01.flac"), root.join("A/B/01.flac")).unwrap();
    std::fs::create_dir_all(root.join("A/C")).unwrap();
    std::fs::rename(park.path().join("02.flac"), root.join("A/C/02.flac")).unwrap();
    let r = scan::reconcile(&ctx(&pool, root)).await.unwrap();
    assert_eq!((r.restored, r.repointed), (1, 1), "{r:?}");
    for id in [one, two] {
        let st = stages(&pool, id).await;
        assert_eq!(st[0].0, "pending", "fingerprint reopened");
        assert_eq!(st[0].1, 0);
        assert_eq!(st[1].0, "failed", "other classes untouched");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn va_artist_created_with_mbid(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_audio(
        root,
        "Comp/Now/01.flac",
        Tags {
            artist: Some("Singer".into()),
            album: Some("Now".into()),
            title: Some("t".into()),
            compilation: true,
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    let row: (Option<Uuid>, String) = sqlx::query_as(
        "SELECT mbid, enrichment_status FROM artists WHERE name_key = 'various artists'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (Some(u(VA_MBID)), "completed".to_string()));
}

/// Carry (Task 17 M1): the recording stage's answer locks track then album; a re-read of the
/// same file must take the track first too, so the two queue up instead of deadlocking.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reread_locks_track_before_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(&pool, dir.path());
    let file = make_audio(dir.path(), "A/B/01.flac", tags("A", "B", "t", "2001", "1"));
    assert_eq!(
        scan::update_path(&c, "A/B/01.flac").await.unwrap(),
        PathOutcome::Added
    );
    let t = track_id(&pool, "A/B/01.flac").await;
    // The file changes on disk.
    let f = std::fs::File::options().write(true).open(&file).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(f);

    // The recording stage's order: track, then album.
    let mut answer = pool.begin().await.unwrap();
    let album = kima_db::identity::lock_track(&mut answer, t)
        .await
        .unwrap()
        .unwrap();
    let reread = {
        let c = c.clone();
        tokio::spawn(async move { scan::update_path(&c, "A/B/01.flac").await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    kima_db::identity::fill_album_blank(&mut answer, album, Some("Label"))
        .await
        .expect("no deadlock: the re-read waits on the track before touching the album");
    answer.commit().await.unwrap();
    assert_eq!(
        reread.await.unwrap().unwrap(),
        PathOutcome::Updated,
        "the re-read finishes after the answer"
    );
}

/// Spec §12.6: `prune_empty` deletes a pruned artist's stored image with its row, and only
/// that artist's.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prune_deletes_artist_image(pool: PgPool) {
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
    let artist_of = |name: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM artists WHERE name = $1")
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let (gone, kept) = (artist_of("A").await, artist_of("C").await);
    for id in [gone, kept] {
        kima_library::artwork::store_artist_image(&c.data_dir, id, b"jpeg bytes").unwrap();
    }

    std::fs::remove_file(p).unwrap();
    scan::reconcile(&c).await.unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() - interval '8 days' WHERE title = 'one'")
        .execute(&pool)
        .await
        .unwrap();
    scan::reconcile(&c).await.unwrap();
    assert!(!kima_library::artwork::artist_image_path(&c.data_dir, gone).exists());
    assert!(kima_library::artwork::artist_image_path(&c.data_dir, kept).exists());
}

/// Spec §12.6: `prune_empty` deletes a pruned album's stored cover (and its variants) with its
/// row, and only that album's.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prune_deletes_album_image(pool: PgPool) {
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
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM albums WHERE title = $1")
                .bind(title)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let (gone, kept) = (album_of("B").await, album_of("D").await);
    for id in [gone, kept] {
        let path = kima_library::artwork::album_image_path(&c.data_dir, id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"jpeg bytes").unwrap();
    }

    std::fs::remove_file(p).unwrap();
    scan::reconcile(&c).await.unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() - interval '8 days' WHERE title = 'one'")
        .execute(&pool)
        .await
        .unwrap();
    scan::reconcile(&c).await.unwrap();
    assert!(!kima_library::artwork::album_image_path(&c.data_dir, gone).exists());
    assert!(kima_library::artwork::album_image_path(&c.data_dir, kept).exists());
}
