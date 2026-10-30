//! The autotag apply job (spec §19.9): files, copy and database targets, journal, crash
//! recovery, the reorganize chain.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{Tags, make_audio, require_ffmpeg};
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_db::autotag::journal::{self as jdb, JournalRow};
use kima_db::autotag::{
    self as adb, Applied, AppliedAlbum, AppliedTrack, NewCandidate, NewRun, Origin, Recommendation,
    Target,
};
use kima_db::organizer as odb;
use kima_library::autotag::apply::ALBUM_BUSY;
use kima_library::autotag::apply::{self, ApplyHooks, AutotagError, FailPoint};
use kima_library::autotag::capability;
use kima_library::autotag::{
    APPLIED_MATCH_EXISTS, CHANGED_OUTSIDE, COPY_FIRST, FILES_READ_ONLY, REORGANIZE_FILES_ONLY,
    TRACKS_CHANGED,
};
use kima_library::organize::apply::{
    self as organize, Action, COPY_IN_USE, Hooks, OrganizeError, REORGANIZE_COLLISION,
};
use kima_library::scan::{self, LibraryCtx, ReadCause};
use kima_library::tagwrite::{NativeTags, audio_md5, container_of, read_native};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DIR: &str = "Old Artist/Old Album";

fn ctx_with(pool: &PgPool, root: &Path, events: EventBus) -> LibraryCtx {
    LibraryCtx {
        pool: pool.clone(),
        root: root.to_path_buf().into(),
        events,
        data_dir: root.join(".kima-data"),
        ignore: Default::default(),
    }
}

fn ctx(pool: &PgPool, root: &Path) -> LibraryCtx {
    ctx_with(pool, root, EventBus::new(256))
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

fn rel(n: usize, ext: &str) -> String {
    format!("{DIR}/0{n}.{ext}")
}

/// `n` files of "Old Artist – Old Album (date)" in `ext`, indexed.
async fn album_fixture(pool: &PgPool, root: &Path, ext: &str, n: usize, date: &str) -> Uuid {
    for i in 1..=n {
        make_audio(
            root,
            &rel(i, ext),
            tags(
                "Old Artist",
                "Old Album",
                &format!("old {i}"),
                date,
                &i.to_string(),
            ),
        );
    }
    rescan(pool, root).await;
    album_of(pool, &rel(1, ext)).await
}

async fn rescan(pool: &PgPool, root: &Path) {
    let r = scan::reconcile(&ctx(pool, root)).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
}

async fn album_of(pool: &PgPool, path: &str) -> Uuid {
    sqlx::query_scalar("SELECT album_id FROM tracks WHERE path = $1")
        .bind(path)
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

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

fn sha(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}

fn shas(root: &Path, rels: &[String]) -> Vec<Vec<u8>> {
    rels.iter().map(|r| sha(&root.join(r))).collect()
}

fn bump_mtime(path: &Path) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();
}

/// Every `.kima-tag-*` file under `root`.
fn leftovers(root: &Path) -> Vec<PathBuf> {
    walk(root)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".kima-tag-"))
        })
        .collect()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn release() -> Uuid {
    Uuid::from_u128(0x5555_5555_5555_4555_8555_5555_5555_5555)
}

/// The §19.6 values of "New Artist – New Album" (released `date`, first released `original`,
/// year `year`) for the tracks at `paths`, in order: matched, numbered 1..n of n.
async fn applied_for(
    pool: &PgPool,
    paths: &[String],
    date: &str,
    original: &str,
    year: i32,
) -> Applied {
    let mut tracks = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let n = i as u32 + 1;
        tracks.push(AppliedTrack {
            track_id: track_id(pool, p).await,
            matched: true,
            disc: Some(1),
            disc_total: None,
            track_no: Some(n),
            track_total: Some(paths.len() as u32),
            title: Some(format!("Song {n}")),
            artist: Some("New Artist".into()),
            artist_sort: Some("Artist, New".into()),
            artist_mbids: vec![Uuid::from_u128(0xa1)],
            recording_mbid: Some(Uuid::from_u128(0x100 + n as u128)),
            release_track_mbid: Some(Uuid::from_u128(0x200 + n as u128)),
        });
    }
    Applied {
        album: AppliedAlbum {
            title: "New Album".into(),
            artist: "New Artist".into(),
            artist_sort: "Artist, New".into(),
            artist_mbids: vec![Uuid::from_u128(0xa1)],
            release_mbid: release(),
            release_group_mbid: Uuid::from_u128(0x6666),
            date: Some(date.into()),
            original_date: Some(original.into()),
            year: Some(year),
            compilation: false,
            genres: vec!["Rock".into()],
            disc_total: 1,
        },
        tracks,
        mbids_only: false,
    }
}

/// Stores a candidate for `applied` and creates the run (`queued`), as the request will.
async fn make_run(pool: &PgPool, album: Uuid, applied: &Applied, target: Target) -> Uuid {
    make_run_with(pool, album, applied, target, false).await
}

/// [`make_run`] with `reorganize`.
async fn make_run_with(
    pool: &PgPool,
    album: Uuid,
    applied: &Applied,
    target: Target,
    reorganize: bool,
) -> Uuid {
    let mapping: Vec<Value> = applied
        .tracks
        .iter()
        .map(|t| json!({"trackId": t.track_id, "medium": 1, "position": t.track_no}))
        .collect();
    let candidate = adb::store_search(
        pool,
        album,
        &[NewCandidate {
            release_mbid: applied.album.release_mbid,
            release_group_mbid: applied.album.release_group_mbid,
            origin: Origin::Search,
            distance: 0.01,
            recommendation: Recommendation::Strong,
            penalties: json!({}),
            mapping: json!(mapping),
            extra_tracks: json!([]),
            missing_tracks: json!([]),
            release: json!({}),
            release_group: json!({}),
        }],
    )
    .await
    .unwrap()
    .unwrap();
    let id = Uuid::now_v7();
    let mut tx = pool.begin().await.unwrap();
    adb::create_run(
        &mut tx,
        &NewRun {
            id,
            album_id: album,
            candidate_id: candidate,
            applied: applied.clone(),
            target,
            reorganize,
            created_by: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn apply_run(ctx: &LibraryCtx, run: Uuid, hooks: &ApplyHooks) -> Result<(), AutotagError> {
    apply::run_apply(ctx, run, hooks, &CancellationToken::new()).await
}

async fn run_row(pool: &PgPool, run: Uuid) -> adb::RunRow {
    adb::get_run(pool, run).await.unwrap().unwrap()
}

async fn album_state(pool: &PgPool, album: Uuid) -> String {
    sqlx::query_scalar("SELECT state FROM autotag_albums WHERE album_id = $1")
        .bind(album)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn album_title(pool: &PgPool, album: Uuid) -> String {
    sqlx::query_scalar("SELECT title FROM albums WHERE id = $1")
        .bind(album)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn journal(pool: &PgPool, run: Uuid) -> Vec<JournalRow> {
    jdb::rows(pool, run).await.unwrap()
}

fn native(v: &Value) -> NativeTags {
    serde_json::from_value(v.clone()).unwrap()
}

/// Every touched key of the row's file reads back its journalled new value.
fn assert_reads_back(root: &Path, row: &JournalRow) {
    let path = root.join(&row.path);
    let new = native(&row.new_tags);
    let keys: Vec<String> = new.keys().cloned().collect();
    let got = read_native(&path, container_of(&path).unwrap(), &keys).unwrap();
    assert_eq!(got, new, "{}", row.path);
}

/// Rewrites the album title in place, as another tagger would.
fn retag_elsewhere(path: &Path) {
    use lofty::config::WriteOptions;
    use lofty::file::TaggedFileExt;
    use lofty::prelude::*;
    use lofty::probe::Probe;
    let mut tagged = Probe::open(path).unwrap().read().unwrap();
    let tag = tagged.primary_tag_mut().expect("fixture has a tag");
    tag.set_title("Changed Elsewhere".to_string());
    tag.save_to_path(path, WriteOptions::default()).unwrap();
}

/// `fingerprint` done with a stored AcoustID fingerprint.
async fn seed_fingerprint(pool: &PgPool, track: Uuid) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, attempts, evidence)
         VALUES ($1, 'fingerprint', 'done', 1, '{}')",
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

/// Makes `root` read-only for this process: `false` (after a note) when it stays writable,
/// as for root, which mode bits do not stop. Under `CI` that is a failure, so the read-only
/// tests cannot pass there by doing nothing.
fn make_read_only(root: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o555)).unwrap();
    if !kima_library::organize::apply::writable(root) {
        return true;
    }
    make_writable(root);
    assert!(
        std::env::var_os("CI").is_none(),
        "the read-only tests need a user that mode bits apply to (not root)"
    );
    eprintln!("the root stays writable for this user (root?); skipping");
    false
}

fn make_writable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Inserts an album "New Artist – <title> (2001)" straight into the database (on the artist
/// row of that name, made if missing), from another thread with its own connection (the hooks
/// are synchronous).
fn insert_conflicting_album(pool: &PgPool, title: &'static str) {
    let opts = pool.connect_options().as_ref().clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            use sqlx::Connection;
            let mut conn = sqlx::PgConnection::connect_with(&opts).await.unwrap();
            sqlx::query(
                "INSERT INTO artists (id, name, name_key) VALUES ($1, 'New Artist', 'new artist')
                 ON CONFLICT (name_key) DO NOTHING",
            )
            .bind(Uuid::now_v7())
            .execute(&mut conn)
            .await
            .unwrap();
            let artist: Uuid =
                sqlx::query_scalar("SELECT id FROM artists WHERE name_key = 'new artist'")
                    .fetch_one(&mut conn)
                    .await
                    .unwrap();
            sqlx::query(
                "INSERT INTO albums (id, artist_id, title, title_key, year)
                 VALUES ($1, $2, $3, lower($3), 2001)",
            )
            .bind(Uuid::now_v7())
            .bind(artist)
            .bind(title)
            .execute(&mut conn)
            .await
            .unwrap();
        });
    })
    .join()
    .unwrap();
}

/// A `before_copy` hook: another program re-tags `02.flac` just before it is copied.
fn retag_second(path: &Path) {
    if path.file_name().is_some_and(|n| n == "02.flac") {
        retag_elsewhere(path);
    }
}

async fn album_error(pool: &PgPool, album: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT error FROM autotag_albums WHERE album_id = $1")
        .bind(album)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn paths(ext: &str, n: usize) -> Vec<String> {
    (1..=n).map(|i| rel(i, ext)).collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn files_apply_writes_and_reorganizes_nothing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let first = track_id(&pool, &rels[0]).await;
    seed_fingerprint(&pool, first).await;
    let md5s: Vec<[u8; 16]> = rels
        .iter()
        .map(|r| audio_md5(&root.join(r)).unwrap())
        .collect();
    let applied = applied_for(&pool, &rels, "2001-02-03", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();

    let rows = journal(&pool, run).await;
    assert_eq!(rows.len(), 3);
    for (row, md5) in rows.iter().zip(&md5s) {
        assert_eq!(row.state, "written", "{}", row.path);
        assert_reads_back(root, row);
        let path = root.join(&row.path);
        assert_eq!(&audio_md5(&path).unwrap(), md5, "audio unchanged");
        assert_eq!(row.after_sha.as_deref(), Some(sha(&path).as_slice()));
        assert!(row.applied_seq.is_some());
    }
    assert_eq!(
        native(&rows[0].new_tags).get("TITLE"),
        Some(&vec!["Song 1".to_string()])
    );
    // The album row updated in place; the files where they were; no organizer run.
    for r in &rels {
        assert_eq!(album_of(&pool, r).await, album);
        assert!(root.join(r).is_file());
    }
    assert_eq!(album_title(&pool, album).await, "New Album");
    assert_eq!(count(&pool, "SELECT count(*) FROM albums").await, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM organizer_runs").await, 0);
    let title: String = sqlx::query_scalar("SELECT title FROM tracks WHERE path = $1")
        .bind(&rels[1])
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title, "Song 2");
    // Stages not reopened, the fingerprint kept.
    let fp: String = sqlx::query_scalar(
        "SELECT status FROM track_stages WHERE track_id = $1 AND stage = 'fingerprint'",
    )
    .bind(first)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fp, "done");
    assert_eq!(count(&pool, "SELECT count(*) FROM track_acoustid").await, 1);
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(r.organizer_run_id, None);
    assert_eq!(
        r.summary,
        Some(json!({"written": 3, "skipped": 0, "failed": 0, "organized": 0}))
    );
    assert_eq!(album_state(&pool, album).await, "applied");
    assert!(leftovers(root).is_empty());

    // A finished run's job does nothing: a run is never applied twice.
    let before = shas(root, &rels);
    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    assert_eq!(shas(root, &rels), before);
    assert_eq!(journal(&pool, run).await, rows);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn files_refused_on_read_only_root(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    let before = shas(root, &rels);

    if !make_read_only(root) {
        return;
    }
    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    make_writable(root);

    assert!(
        matches!(&res, Err(AutotagError::Refused(m)) if m == FILES_READ_ONLY),
        "{res:?}"
    );
    let r = run_row(&pool, run).await;
    assert_eq!(
        (r.state.as_str(), r.error.as_deref()),
        ("failed", Some(FILES_READ_ONLY))
    );
    assert_eq!(album_state(&pool, album).await, "proposed");
    assert!(journal(&pool, run).await.is_empty());
    assert!(leftovers(root).is_empty());
    assert_eq!(shas(root, &rels), before);
    assert_eq!(album_title(&pool, album).await, "Old Album");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn files_refused_for_wma_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_audio(
        root,
        &rel(4, "wav"),
        tags("Old Artist", "Old Album", "old 4", "2001", "4"),
    );
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let mut rels = paths("flac", 3);
    rels.push(rel(4, "wav"));
    assert_eq!(
        album_of(&pool, &rels[3]).await,
        album,
        "the WAV is on the album"
    );
    let before = shas(root, &rels);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;

    let run = make_run(&pool, album, &applied, Target::Files).await;
    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    let msg = "Tag writing is not supported for .wav files";
    assert!(
        matches!(&res, Err(AutotagError::Refused(m)) if m == msg),
        "{res:?}"
    );
    assert_eq!(run_row(&pool, run).await.error.as_deref(), Some(msg));
    assert!(journal(&pool, run).await.is_empty());
    assert_eq!(shas(root, &rels), before);

    // Database-only works for the same album.
    let run = make_run(&pool, album, &applied, Target::Db).await;
    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    assert_eq!(run_row(&pool, run).await.state, "done");
    assert_eq!(album_title(&pool, album).await, "New Album");
    assert_eq!(shas(root, &rels), before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn db_apply_writes_no_file_and_survives_rescan(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "mp3", 3, "2001").await;
    let rels = paths("mp3", 3);
    let before = shas(root, &rels);
    // A reissue: the identity's year is the original one, the release date another.
    let applied = applied_for(&pool, &rels, "2009-05-01", "1999", 1999).await;
    let run = make_run(&pool, album, &applied, Target::Db).await;

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();

    assert_eq!(shas(root, &rels), before, "no file written");
    assert!(journal(&pool, run).await.is_empty());
    assert!(leftovers(root).is_empty());
    let overrides: i64 =
        sqlx::query_scalar("SELECT count(*) FROM track_tag_overrides WHERE run_id = $1")
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(overrides, 3);
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(
        r.summary,
        Some(json!({"written": 3, "skipped": 0, "failed": 0, "organized": 0}))
    );

    // A forced reconcile re-reads every file: the applied values stay.
    for r in &rels {
        bump_mtime(&root.join(r));
    }
    let report = scan::reconcile(&ctx(&pool, root)).await.unwrap();
    assert_eq!(report.tags_read, 3, "{report:?}");
    for (i, r) in rels.iter().enumerate() {
        assert_eq!(album_of(&pool, r).await, album);
        let title: String = sqlx::query_scalar("SELECT title FROM tracks WHERE path = $1")
            .bind(r)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(title, format!("Song {}", i + 1));
    }
    assert_eq!(album_title(&pool, album).await, "New Album");
    let year: Option<i32> = sqlx::query_scalar("SELECT year FROM albums WHERE id = $1")
        .bind(album)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(year, Some(1999));
    assert_eq!(count(&pool, "SELECT count(*) FROM albums").await, 1);
    assert_eq!(shas(root, &rels), before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apply_refused_when_tracks_changed(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    // A track added after the match was computed.
    make_audio(
        root,
        &rel(4, "flac"),
        tags("Old Artist", "Old Album", "old 4", "2001", "4"),
    );
    rescan(&pool, root).await;
    assert_eq!(album_of(&pool, &rel(4, "flac")).await, album);
    let all: Vec<String> = paths("flac", 4);
    let before = shas(root, &all);

    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    assert!(
        matches!(&res, Err(AutotagError::Refused(m)) if m == TRACKS_CHANGED),
        "{res:?}"
    );
    let r = run_row(&pool, run).await;
    assert_eq!(
        (r.state.as_str(), r.error.as_deref()),
        ("failed", Some(TRACKS_CHANGED))
    );
    assert_eq!(album_state(&pool, album).await, "proposed");
    assert!(journal(&pool, run).await.is_empty());
    assert_eq!(shas(root, &all), before, "nothing touched");
    assert!(leftovers(root).is_empty());
    assert_eq!(album_title(&pool, album).await, "Old Album");
    assert_eq!(
        count(&pool, "SELECT count(*) FROM track_tag_overrides").await,
        0
    );

    // The same for the database target: nothing is stored.
    let run = make_run(&pool, album, &applied, Target::Db).await;
    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    assert!(
        matches!(&res, Err(AutotagError::Refused(m)) if m == TRACKS_CHANGED),
        "{res:?}"
    );
    assert_eq!(album_title(&pool, album).await, "Old Album");
    assert_eq!(
        count(&pool, "SELECT count(*) FROM track_tag_overrides").await,
        0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_changed_after_planning_fails_row_only(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    let victim = root.join(&rels[1]);
    let hooked = victim.clone();
    let hooks = ApplyHooks {
        after_journal: Some(Arc::new(move || retag_elsewhere(&hooked))),
        ..Default::default()
    };
    apply_run(&ctx(&pool, root), run, &hooks).await.unwrap();
    let changed = sha(&victim);

    let rows = journal(&pool, run).await;
    let states: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|r| (r.state.as_str(), r.note.as_deref()))
        .collect();
    assert_eq!(
        states,
        vec![
            ("written", None),
            ("failed", Some(CHANGED_OUTSIDE)),
            ("written", None)
        ]
    );
    assert_eq!(rows[1].after_sha, None);
    assert_reads_back(root, &rows[0]);
    assert_reads_back(root, &rows[2]);
    // The other tagger's content is left exactly as it wrote it.
    let title = read_native(
        &victim,
        container_of(&victim).unwrap(),
        &["TITLE".to_string()],
    )
    .unwrap();
    assert_eq!(title["TITLE"], vec!["Changed Elsewhere".to_string()]);
    assert_eq!(sha(&victim), changed);
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(
        r.summary,
        Some(json!({"written": 2, "skipped": 0, "failed": 1, "organized": 0}))
    );
    assert!(leftovers(root).is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn collision_refused_in_job_before_files(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_audio(
        root,
        "New Artist/New Album/01.flac",
        tags("New Artist", "New Album", "other", "2001", "1"),
    );
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let other = album_of(&pool, "New Artist/New Album/01.flac").await;
    assert_ne!(other, album);
    let before = shas(root, &rels);
    let applied = applied_for(&pool, &rels, "2001-02-03", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    let msg = "Another album already has this identity: New Artist \u{2013} New Album (2001)";
    assert!(
        matches!(&res, Err(AutotagError::Refused(m)) if m == msg),
        "{res:?}"
    );
    let r = run_row(&pool, run).await;
    assert_eq!(
        (r.state.as_str(), r.error.as_deref()),
        ("failed", Some(msg))
    );
    assert!(
        journal(&pool, run).await.is_empty(),
        "refused before the journal"
    );
    assert_eq!(shas(root, &rels), before);
    assert!(leftovers(root).is_empty());
    assert_eq!(album_title(&pool, album).await, "Old Album");
    assert_eq!(album_state(&pool, album).await, "proposed");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn crash_after_rename_recovered_as_written(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    let mut hooks = ApplyHooks::default();
    hooks.write.stop_after_rename = true;
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    let rows = journal(&pool, run).await;
    assert!(rows.iter().all(|r| r.state == "planned"));
    assert_ne!(
        rows[0].before_sha.as_deref(),
        Some(sha(&root.join(&rows[0].path)).as_slice()),
        "the first file was renamed over before the crash"
    );
    assert_eq!(run_row(&pool, run).await.state, "writing", "resumable");

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    let rows = journal(&pool, run).await;
    for row in &rows {
        assert_eq!(row.state, "written", "{}", row.path);
        assert_reads_back(root, row);
        assert_eq!(
            row.after_sha.as_deref(),
            Some(sha(&root.join(&row.path)).as_slice())
        );
    }
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(
        r.summary,
        Some(json!({"written": 3, "skipped": 0, "failed": 0, "organized": 0}))
    );
    assert!(leftovers(root).is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn leftover_tag_copies_deleted_on_resume(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    let hooks = ApplyHooks {
        stop_after_files: Some(1),
        ..Default::default()
    };
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    // Copies a crash would leave: one of the written row, one of a planned row.
    let rows = journal(&pool, run).await;
    assert_eq!(rows[0].state, "written");
    for row in [&rows[0], &rows[2]] {
        let copy = root.join(DIR).join(format!(".kima-tag-{}.flac", row.id));
        std::fs::copy(root.join(&row.path), &copy).unwrap();
    }
    assert_eq!(leftovers(root).len(), 2);
    // Names that are not this run's copies are never touched.
    let foreign = [
        root.join(DIR).join(".kima-tag-999999999.flac"),
        root.join(DIR).join(".kima-tag-notes.txt"),
    ];
    for f in &foreign {
        std::fs::write(f, b"not ours").unwrap();
    }

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    let mut left = leftovers(root);
    left.sort();
    let mut want = foreign.to_vec();
    want.sort();
    assert_eq!(left, want, "only the run's copies are deleted");
    assert!(
        journal(&pool, run)
            .await
            .iter()
            .all(|r| r.state == "written")
    );
    assert_eq!(run_row(&pool, run).await.state, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn planned_row_at_before_sha_is_redone(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "mp3", 3, "2001").await;
    let rels = paths("mp3", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    let hooks = ApplyHooks {
        stop_after_files: Some(1),
        ..Default::default()
    };
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    let rows = journal(&pool, run).await;
    let states: Vec<&str> = rows.iter().map(|r| r.state.as_str()).collect();
    assert_eq!(states, vec!["written", "planned", "planned"]);
    for row in &rows[1..] {
        assert_eq!(
            row.before_sha.as_deref(),
            Some(sha(&root.join(&row.path)).as_slice())
        );
    }

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    let rows = journal(&pool, run).await;
    for row in &rows {
        assert_eq!(row.state, "written", "{}", row.path);
        assert_reads_back(root, row);
    }
    let seqs: Vec<i64> = rows.iter().map(|r| r.applied_seq.unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    assert_eq!(run_row(&pool, run).await.state, "done");
    assert_eq!(album_title(&pool, album).await, "New Album");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn progress_events_carry_run_id(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    let bus = EventBus::new(1024);
    let mut rx = bus.subscribe();

    apply_run(&ctx_with(&pool, root, bus), run, &ApplyHooks::default())
        .await
        .unwrap();

    let mut sent = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if e.data["type"] == "autotag:progress" {
            assert_eq!(e.user_id, None);
            sent.push(e.data);
        }
    }
    assert!(!sent.is_empty());
    for e in &sent {
        assert_eq!(e["job"], "apply");
        assert_eq!(e["runId"], json!(run));
        assert_eq!(e["albumId"], json!(album));
        assert_eq!(e["total"], 3);
    }
    let last = sent.last().unwrap();
    assert_eq!(
        (last["done"].clone(), last["state"].clone()),
        (json!(3), json!("done"))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reissue_year_keeps_tracks_on_the_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2009").await;
    let rels = paths("flac", 3);
    // Originally 1999, this release 2009: the files get DATE=2009, the identity year 1999.
    let applied = applied_for(&pool, &rels, "2009-05-01", "1999", 1999).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    let rows = journal(&pool, run).await;
    assert_eq!(
        native(&rows[0].new_tags).get("DATE"),
        Some(&vec!["2009-05-01".to_string()])
    );

    // A forced reconcile, then the tag write's own re-read again.
    for r in &rels {
        bump_mtime(&root.join(r));
    }
    rescan(&pool, root).await;
    let report = scan::reread_paths(&ctx(&pool, root), &rels, ReadCause::TagWrite)
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    for r in &rels {
        assert_eq!(album_of(&pool, r).await, album, "{r}");
    }
    assert_eq!(count(&pool, "SELECT count(*) FROM albums").await, 1);
    let year: Option<i32> = sqlx::query_scalar("SELECT year FROM albums WHERE id = $1")
        .bind(album)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(year, Some(1999));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn interrupted_runs_requeued_once(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    // A job that stopped mid-way, its claim gone.
    let hooks = ApplyHooks {
        stop_after_files: Some(1),
        ..Default::default()
    };
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");

    assert_eq!(apply::resume_interrupted(&pool).await.unwrap(), 1);
    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM jobs WHERE queue = 'library' AND payload ->> 'kind' = 'autotag-apply'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(payload, json!({"kind": "autotag-apply", "runId": run}));
    assert_eq!(
        apply::resume_interrupted(&pool).await.unwrap(),
        0,
        "a job is pending"
    );

    // The queued job finishes the run: nothing is left to resume.
    let job = kima_library::jobs::run(&ctx(&pool, root), &payload, &CancellationToken::new()).await;
    assert!(job.is_ok(), "{job:?}");
    assert_eq!(run_row(&pool, run).await.state, "done");
    sqlx::query("DELETE FROM jobs")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(apply::resume_interrupted(&pool).await.unwrap(), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cancelled_apply_stays_resumable(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let before = shas(root, &rels);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    let cancel = CancellationToken::new();
    cancel.cancel();
    let res = apply::run_apply(&ctx(&pool, root), run, &ApplyHooks::default(), &cancel).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    assert_eq!(shas(root, &rels), before, "stopped before the first file");
    assert!(
        journal(&pool, run)
            .await
            .iter()
            .all(|r| r.state == "planned")
    );
    assert_eq!(run_row(&pool, run).await.state, "writing", "not failed");

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    assert_eq!(run_row(&pool, run).await.state, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn collision_after_writes_leaves_run_needing_undo(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001-02-03", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    // The conflicting album appears after the pre-check, while the files are written.
    let p = pool.clone();
    let hooks = ApplyHooks {
        after_journal: Some(Arc::new(move || insert_conflicting_album(&p, "New Album"))),
        ..Default::default()
    };
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    let msg = "Another album already has this identity: New Artist \u{2013} New Album (2001)";
    assert!(
        matches!(&res, Err(AutotagError::Refused(m)) if m == msg),
        "{res:?}"
    );

    let rows = journal(&pool, run).await;
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row.state, "written");
        assert_reads_back(root, row);
    }
    let r = run_row(&pool, run).await;
    assert_eq!(
        (r.state.as_str(), r.error.as_deref()),
        ("failed", Some(msg))
    );
    assert_eq!(
        r.summary,
        Some(
            json!({"written": 3, "skipped": 0, "failed": 0, "organized": 0,
                    "rolledBack": false, "needsUndo": true, "identityApplied": false})
        )
    );
    assert_eq!(album_state(&pool, album).await, "failed", "needs attention");
    assert_eq!(album_error(&pool, album).await.as_deref(), Some(msg));
    // The database is untouched: no identity, no override, no re-read, no merge.
    assert_eq!(album_title(&pool, album).await, "Old Album");
    let (source, before): (String, Option<Value>) = sqlx::query_as(
        "SELECT identity_source, album_before FROM albums a
                        JOIN autotag_runs r ON r.album_id = a.id WHERE r.id = $1",
    )
    .bind(run)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((source.as_str(), before), ("scan", None));
    assert_eq!(
        count(&pool, "SELECT count(*) FROM track_tag_overrides").await,
        0
    );
    for r in &rels {
        assert_eq!(album_of(&pool, r).await, album);
        let title: String = sqlx::query_scalar("SELECT title FROM tracks WHERE path = $1")
            .bind(r)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(title.starts_with("old "), "not re-read: {title}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resumed_on_read_only_root_fails_remaining_rows(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    let hooks = ApplyHooks {
        stop_after_files: Some(1),
        ..Default::default()
    };
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    let untouched = shas(root, &rels[1..]);
    let planned = journal(&pool, run).await;

    // The process restarts with the library mounted read-only.
    if !make_read_only(root) {
        return;
    }
    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    make_writable(root);
    res.unwrap();

    let rows = journal(&pool, run).await;
    let states: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|r| (r.state.as_str(), r.note.as_deref()))
        .collect();
    assert_eq!(
        states,
        vec![
            ("written", None),
            ("failed", Some(FILES_READ_ONLY)),
            ("failed", Some(FILES_READ_ONLY))
        ]
    );
    assert_eq!(shas(root, &rels[1..]), untouched);
    // The written row is restorable: its hash and original values are in the journal.
    assert_eq!(
        rows[0].after_sha.as_deref(),
        Some(sha(&root.join(&rels[0])).as_slice())
    );
    assert_eq!(rows[0].old_tags, planned[0].old_tags);
    assert_ne!(rows[0].old_tags, rows[0].new_tags);
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done", "undoable");
    assert_eq!(
        r.summary,
        Some(json!({"written": 1, "skipped": 0, "failed": 2, "organized": 0}))
    );
    assert_eq!(album_title(&pool, album).await, "New Album");
    assert_eq!(album_of(&pool, &rels[0]).await, album);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn applied_match_must_be_undone_first(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let first = make_run(&pool, album, &applied, Target::Db).await;
    apply_run(&ctx(&pool, root), first, &ApplyHooks::default())
        .await
        .unwrap();
    let before = shas(root, &rels);
    let overrides = || async {
        sqlx::query_as::<_, (Uuid, Value)>(
            "SELECT run_id, fields FROM track_tag_overrides ORDER BY track_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let kept = overrides().await;
    assert_eq!(kept.len(), 3);

    for target in [Target::Files, Target::Copy, Target::Db] {
        let run = make_run(&pool, album, &applied, target).await;
        let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
        assert!(
            matches!(&res, Err(AutotagError::Refused(m)) if m == APPLIED_MATCH_EXISTS),
            "{target:?}: {res:?}"
        );
        assert!(journal(&pool, run).await.is_empty());
        assert_eq!(shas(root, &rels), before);
        assert_eq!(overrides().await, kept, "the first run's overrides stay");
        assert_eq!(
            album_state(&pool, album).await,
            "applied",
            "the first match stands"
        );
    }

    // Once the first match is undone (album row restored, overrides gone), a new apply goes
    // through.
    undo_identity_only(&pool, first).await;
    let run = make_run(&pool, album, &applied, Target::Db).await;
    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    assert_eq!(run_row(&pool, run).await.state, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_changed_during_write_fails_row(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;

    let mut hooks = ApplyHooks::default();
    hooks.write.before_copy = Some(retag_second);
    apply_run(&ctx(&pool, root), run, &hooks).await.unwrap();

    let rows = journal(&pool, run).await;
    assert_eq!(
        (rows[1].state.as_str(), rows[1].note.as_deref()),
        ("failed", Some(CHANGED_OUTSIDE))
    );
    assert_eq!(rows[0].state, "written");
    assert_eq!(rows[2].state, "written");
    let victim = root.join(&rels[1]);
    let title = read_native(
        &victim,
        container_of(&victim).unwrap(),
        &["TITLE".to_string()],
    )
    .unwrap();
    assert_eq!(title["TITLE"], vec!["Changed Elsewhere".to_string()]);
    assert!(leftovers(root).is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn busy_album_refusal_leaves_album_state(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let working = make_run(&pool, album, &applied, Target::Db).await;
    let run = make_run(&pool, album, &applied, Target::Db).await;

    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    assert!(
        matches!(&res, Err(AutotagError::Refused(m)) if m == ALBUM_BUSY),
        "{res:?}"
    );
    assert_eq!(run_row(&pool, run).await.state, "failed");
    assert_eq!(run_row(&pool, working).await.state, "queued");
    assert_eq!(
        album_state(&pool, album).await,
        "applying",
        "the working run's state"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resume_after_external_change_fails_row(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    let hooks = ApplyHooks {
        stop_after_files: Some(1),
        ..Default::default()
    };
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    // While kima-rs was down another tagger rewrote the second file.
    let victim = root.join(&rels[1]);
    retag_elsewhere(&victim);
    let changed = sha(&victim);

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    let rows = journal(&pool, run).await;
    let states: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|r| (r.state.as_str(), r.note.as_deref()))
        .collect();
    assert_eq!(
        states,
        vec![
            ("written", None),
            ("failed", Some(CHANGED_OUTSIDE)),
            ("written", None)
        ]
    );
    assert_eq!(sha(&victim), changed, "never written over");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resume_with_other_audio_fails_row(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    let mut hooks = ApplyHooks::default();
    hooks.write.stop_after_rename = true;
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    // The first file carries the new tags, but not the audio the journal planned.
    let first = journal(&pool, run).await[0].id;
    sqlx::query("UPDATE autotag_journal SET audio_md5 = $2 WHERE id = $1")
        .bind(first)
        .bind(vec![0u8; 16])
        .execute(&pool)
        .await
        .unwrap();

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    let rows = journal(&pool, run).await;
    assert_eq!(
        (rows[0].state.as_str(), rows[0].note.as_deref()),
        ("failed", Some(CHANGED_OUTSIDE))
    );
    assert_eq!(rows[1].state, "written");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn already_tagged_files_skipped(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    // No applied year: the files apply stores no override. Its identity is then undone in the
    // database only (a re-match requires undoing first), so the files still carry its tags.
    let mut applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    applied.album.year = None;
    let first = make_run(&pool, album, &applied, Target::Files).await;
    apply_run(&ctx(&pool, root), first, &ApplyHooks::default())
        .await
        .unwrap();
    undo_identity_only(&pool, first).await;
    let after_first = shas(root, &rels);

    let run = make_run(&pool, album, &applied, Target::Files).await;
    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    let rows = journal(&pool, run).await;
    for row in &rows {
        assert_eq!(
            (row.state.as_str(), row.note.as_deref()),
            ("skipped", Some("already tagged"))
        );
        assert_eq!(row.applied_seq, None);
    }
    assert_eq!(shas(root, &rels), after_first, "nothing rewritten");
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(
        r.summary,
        Some(json!({"written": 0, "skipped": 3, "failed": 0, "organized": 0}))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn final_attempt_failure_settles_run(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let before = shas(root, &rels);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    let hooks = ApplyHooks {
        fail_at: Some(FailPoint::BeforeWrites),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let c = ctx(&pool, root);

    let res = apply::run_job_with(&c, run, &hooks, &cancel, false).await;
    assert!(
        matches!(res, Err(kima_jobs::worker::JobError::Retry(_))),
        "{res:?}"
    );
    assert_eq!(run_row(&pool, run).await.state, "writing", "retried later");

    let res = apply::run_job_with(&c, run, &hooks, &cancel, true).await;
    assert!(
        matches!(res, Err(kima_jobs::worker::JobError::Permanent(_))),
        "{res:?}"
    );
    let r = run_row(&pool, run).await;
    assert_eq!(
        (r.state.as_str(), r.error.as_deref()),
        ("failed", Some("forced failure (test hook)"))
    );
    assert_eq!(
        album_state(&pool, album).await,
        "proposed",
        "nothing was written"
    );
    assert_eq!(shas(root, &rels), before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resumed_read_only_keeps_file_renamed_before_crash(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    // A crash after the first rename, before its commit.
    let mut hooks = ApplyHooks::default();
    hooks.write.stop_after_rename = true;
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    let planned = journal(&pool, run).await;
    assert!(planned.iter().all(|r| r.state == "planned"));
    let untouched = shas(root, &rels[1..]);

    // The process restarts with the library mounted read-only.
    if !make_read_only(root) {
        return;
    }
    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    make_writable(root);
    res.unwrap();

    let rows = journal(&pool, run).await;
    let states: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|r| (r.state.as_str(), r.note.as_deref()))
        .collect();
    assert_eq!(
        states,
        vec![
            ("written", None),
            ("failed", Some(FILES_READ_ONLY)),
            ("failed", Some(FILES_READ_ONLY))
        ]
    );
    // Restorable: the renamed file's hash and original values are journalled.
    assert_eq!(
        rows[0].after_sha.as_deref(),
        Some(sha(&root.join(&rels[0])).as_slice())
    );
    assert!(rows[0].applied_seq.is_some());
    assert_eq!(rows[0].old_tags, planned[0].old_tags);
    assert_reads_back(root, &rows[0]);
    assert_eq!(shas(root, &rels[1..]), untouched);
    assert_eq!(run_row(&pool, run).await.state, "done");
}

/// `undo_identity` of a done run (album row, artist step, overrides), files left as they are.
async fn undo_identity_only(pool: &PgPool, run: Uuid) {
    let row = run_row(pool, run).await;
    let mut tx = pool.begin().await.unwrap();
    adb::undo_identity(&mut tx, &row).await.unwrap();
    tx.commit().await.unwrap();
}

/// Run A applied (files, no year), then run B wrote its files but collided under the locks:
/// B needs undo.
async fn album_with_needs_undo_run(pool: &PgPool, root: &Path) -> (Uuid, Uuid, Applied) {
    let album = album_fixture(pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let mut a = applied_for(pool, &rels, "2001", "2001", 2001).await;
    a.album.year = None;
    let first = make_run(pool, album, &a, Target::Files).await;
    apply_run(&ctx(pool, root), first, &ApplyHooks::default())
        .await
        .unwrap();
    assert_eq!(album_state(pool, album).await, "applied");
    // A re-match requires undoing first (the database side is enough here).
    undo_identity_only(pool, first).await;

    let mut b = applied_for(pool, &rels, "2001", "2001", 2001).await;
    b.album.title = "Other Album".into();
    let run_b = make_run(pool, album, &b, Target::Files).await;
    let p = pool.clone();
    let hooks = ApplyHooks {
        after_journal: Some(Arc::new(move || {
            insert_conflicting_album(&p, "Other Album")
        })),
        ..Default::default()
    };
    let res = apply_run(&ctx(pool, root), run_b, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Refused(_))), "{res:?}");
    assert_eq!(album_state(pool, album).await, "failed");
    (album, run_b, b)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn needs_undo_run_blocks_applies_and_keeps_album_failed(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (album, run_b, applied) = album_with_needs_undo_run(&pool, root).await;
    let b_error = run_row(&pool, run_b).await.error;
    assert!(b_error.is_some());
    let rels = paths("flac", 3);
    let before = shas(root, &rels);

    // A run refused with nothing written (here: the copy target, refused since B needs undo):
    // the album stays failed with B's message, not `applied`.
    let run = make_run(&pool, album, &applied, Target::Copy).await;
    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    assert!(matches!(&res, Err(AutotagError::Refused(_))), "{res:?}");
    assert_eq!(album_state(&pool, album).await, "failed");
    assert_eq!(album_error(&pool, album).await, b_error);

    // A new apply is refused while B needs undo; nothing is touched.
    for target in [Target::Files, Target::Copy, Target::Db] {
        let run = make_run(&pool, album, &applied, target).await;
        let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
        assert!(
            matches!(&res, Err(AutotagError::Refused(m)) if m == APPLIED_MATCH_EXISTS),
            "{target:?}: {res:?}"
        );
        assert!(journal(&pool, run).await.is_empty());
        assert_eq!(shas(root, &rels), before);
        assert_eq!(album_state(&pool, album).await, "failed");
        assert_eq!(album_error(&pool, album).await, b_error);
    }
    assert_eq!(
        count(&pool, "SELECT count(*) FROM track_tag_overrides").await,
        0
    );
}

async fn album_before_set(pool: &PgPool, run: Uuid) -> bool {
    run_row(pool, run).await.album_before.is_some()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn final_attempt_after_identity_needs_undo(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    for (target, ext) in [(Target::Db, "mp3"), (Target::Files, "flac")] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        sqlx::query("TRUNCATE albums, artists, tracks, autotag_runs, jobs CASCADE")
            .execute(&pool)
            .await
            .unwrap();
        let album = album_fixture(&pool, root, ext, 3, "2001").await;
        let rels = paths(ext, 3);
        let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
        let run = make_run(&pool, album, &applied, target).await;
        let hooks = ApplyHooks {
            fail_at: Some(FailPoint::AfterIdentity),
            ..Default::default()
        };
        let c = ctx(&pool, root);
        let res = apply::run_job_with(&c, run, &hooks, &CancellationToken::new(), true).await;
        assert!(
            matches!(res, Err(kima_jobs::worker::JobError::Permanent(_))),
            "{target:?}: {res:?}"
        );

        let r = run_row(&pool, run).await;
        assert_eq!(r.state, "failed", "{target:?}");
        let summary = r.summary.unwrap();
        assert_eq!(summary["needsUndo"], json!(true), "{target:?}");
        assert_eq!(summary["identityApplied"], json!(true), "{target:?}");
        assert!(album_before_set(&pool, run).await, "{target:?}");
        assert_eq!(album_state(&pool, album).await, "failed", "{target:?}");
        assert_eq!(album_title(&pool, album).await, "New Album", "{target:?}");
        let overrides: i64 =
            sqlx::query_scalar("SELECT count(*) FROM track_tag_overrides WHERE run_id = $1")
                .bind(run)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(overrides, 3, "{target:?}");

        // A later apply is refused until this run is undone.
        let later = make_run(&pool, album, &applied, Target::Db).await;
        let res = apply_run(&c, later, &ApplyHooks::default()).await;
        assert!(
            matches!(&res, Err(AutotagError::Refused(m)) if m == APPLIED_MATCH_EXISTS),
            "{target:?}: {res:?}"
        );
        assert_eq!(album_state(&pool, album).await, "failed", "{target:?}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn final_attempt_after_writes_needs_undo(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Files).await;
    let hooks = ApplyHooks {
        fail_at: Some(FailPoint::BeforeIdentity),
        ..Default::default()
    };
    let res = apply::run_job_with(
        &ctx(&pool, root),
        run,
        &hooks,
        &CancellationToken::new(),
        true,
    )
    .await;
    assert!(
        matches!(res, Err(kima_jobs::worker::JobError::Permanent(_))),
        "{res:?}"
    );

    let rows = journal(&pool, run).await;
    assert!(rows.iter().all(|r| r.state == "written"));
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "failed");
    assert_eq!(
        r.summary,
        Some(
            json!({"written": 3, "skipped": 0, "failed": 0, "organized": 0,
                    "rolledBack": false, "needsUndo": true, "identityApplied": false})
        )
    );
    assert!(!album_before_set(&pool, run).await);
    assert_eq!(album_state(&pool, album).await, "failed");
    assert_eq!(album_title(&pool, album).await, "Old Album");
    assert_eq!(
        count(&pool, "SELECT count(*) FROM track_tag_overrides").await,
        0
    );
}

// ---------------------------------------------------------------------------------------------
// Task 17: the copy target, the reorganize chain, switch-root

/// A copy-mode organizer run of the whole library at `root` into `target`, previewed and
/// applied (`done`).
async fn copy_library(pool: &PgPool, root: &Path, target: &Path) -> Uuid {
    copy_run_scoped(pool, root, target, None).await
}

async fn copy_run_scoped(
    pool: &PgPool,
    root: &Path,
    target: &Path,
    albums: Option<&[Uuid]>,
) -> Uuid {
    std::fs::create_dir_all(target).unwrap();
    let (src, dst) = (root.display().to_string(), target.display().to_string());
    let id = Uuid::now_v7();
    match albums {
        Some(a) => odb::create_scoped_run(pool, id, "copy", &src, &dst, None, a, "user")
            .await
            .unwrap(),
        None => odb::create_run(pool, id, "copy", &src, &dst, None)
            .await
            .unwrap(),
    };
    let c = ctx(pool, root);
    organize::run_action(&c, id, Action::Preview, &Hooks::default())
        .await
        .unwrap();
    assert!(odb::start_apply(pool, id).await.unwrap());
    organize::run_action(&c, id, Action::Apply, &Hooks::default())
        .await
        .unwrap();
    assert_eq!(organizer_state(pool, id).await, "done");
    id
}

async fn organizer_state(pool: &PgPool, run: Uuid) -> String {
    odb::get_run(pool, run).await.unwrap().unwrap().state
}

/// Track id → copy path (relative to the copy run's target), in `rels` order.
async fn copy_paths(pool: &PgPool, copy_run: Uuid, rels: &[String]) -> Vec<String> {
    let mut ids = Vec::new();
    for r in rels {
        ids.push(track_id(pool, r).await);
    }
    let copies = odb::copy_rows_for_tracks(pool, copy_run, &ids)
        .await
        .unwrap();
    ids.iter().map(|t| copies[t].clone()).collect()
}

fn mtimes(root: &Path, rels: &[String]) -> Vec<SystemTime> {
    rels.iter()
        .map(|r| std::fs::metadata(root.join(r)).unwrap().modified().unwrap())
        .collect()
}

async fn overrides_of(pool: &PgPool, run: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM track_tag_overrides WHERE run_id = $1")
        .bind(run)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn track_path(pool: &PgPool, track: Uuid) -> String {
    sqlx::query_scalar("SELECT path FROM tracks WHERE id = $1")
        .bind(track)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn refused_with(res: &Result<(), AutotagError>, msg: &str) -> bool {
    matches!(res, Err(AutotagError::Refused(m)) if m == msg)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_refused_without_done_copy_run(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 2, "2001").await;
    let rels = paths("flac", 2);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let c = ctx(&pool, root);
    let refused = |what: &'static str| {
        let (pool, c, applied) = (pool.clone(), c.clone(), applied.clone());
        async move {
            let caps = capability::capabilities(&c).await.unwrap();
            assert!(!caps.copy.available, "{what}");
            assert_eq!(caps.copy.reason.as_deref(), Some(COPY_FIRST), "{what}");
            assert_eq!(caps.copy_run_id, None, "{what}");
            let run = make_run(&pool, album, &applied, Target::Copy).await;
            let res = apply_run(&c, run, &ApplyHooks::default()).await;
            assert!(refused_with(&res, COPY_FIRST), "{what}: {res:?}");
            assert!(journal(&pool, run).await.is_empty(), "{what}");
            let r = run_row(&pool, run).await;
            assert_eq!(r.state, "failed", "{what}");
            assert_eq!(r.copy_run_id, None, "{what}");
            assert_eq!(overrides_of(&pool, run).await, 0, "{what}");
        }
    };

    // No copy run at all.
    refused("no copy run").await;
    let originals = shas(root, &rels);

    // A copy run only previewed.
    let previewed = Uuid::now_v7();
    std::fs::create_dir_all(out.path().join("a")).unwrap();
    odb::create_run(
        &pool,
        previewed,
        "copy",
        &root.display().to_string(),
        &out.path().join("a").display().to_string(),
        None,
    )
    .await
    .unwrap();
    organize::run_action(&c, previewed, Action::Preview, &Hooks::default())
        .await
        .unwrap();
    refused("previewed copy run").await;

    // A done copy run the library was switched to (it is the library now, not a copy).
    let switched = copy_library(&pool, root, &out.path().join("b")).await;
    sqlx::query("UPDATE organizer_runs SET switched_at = now() WHERE id = $1")
        .bind(switched)
        .execute(&pool)
        .await
        .unwrap();
    refused("switched copy run").await;

    // A done copy run of this album only (scoped: not a copy of the library).
    copy_run_scoped(&pool, root, &out.path().join("c"), Some(&[album])).await;
    refused("scoped copy run").await;

    assert_eq!(shas(root, &rels), originals, "no file written");
    assert_eq!(album_title(&pool, album).await, "Old Album");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_never_opens_an_original_for_writing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (root, target) = (dir.path(), out.path().join("organized"));
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let copy_run = copy_library(&pool, root, &target).await;
    let copies = copy_paths(&pool, copy_run, &rels).await;
    let (originals, original_mtimes) = (shas(root, &rels), mtimes(root, &rels));
    let copies_before = shas(&target, &copies);
    assert_eq!(originals, copies_before, "verified copies");

    let c = ctx(&pool, root);
    let caps = capability::capabilities(&c).await.unwrap();
    assert!(caps.copy.available, "{caps:?}");
    assert_eq!(caps.copy_run_id, Some(copy_run));
    let local = adb::local_album(&pool, album).await.unwrap().unwrap();
    assert_eq!(
        capability::album_refusal(&c, &local, Target::Copy)
            .await
            .unwrap(),
        None
    );

    let applied = applied_for(&pool, &rels, "2001-02-03", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Copy).await;
    apply_run(&c, run, &ApplyHooks::default()).await.unwrap();

    // The originals: not one byte, not even the modification time.
    assert_eq!(shas(root, &rels), originals);
    assert_eq!(mtimes(root, &rels), original_mtimes);
    assert!(leftovers(root).is_empty());
    assert!(leftovers(&target).is_empty());
    // The copies carry the tags, journalled against the copy root.
    let rows = journal(&pool, run).await;
    assert_eq!(rows.len(), 3);
    let mut journalled: Vec<String> = rows.iter().map(|r| r.path.clone()).collect();
    journalled.sort();
    let mut want = copies.clone();
    want.sort();
    assert_eq!(journalled, want);
    for row in &rows {
        assert_eq!(row.root, "copy");
        assert_eq!(row.state, "written", "{}", row.path);
        assert_reads_back(&target, row);
        let i = copies.iter().position(|p| p == &row.path).unwrap();
        assert_eq!(row.before_sha.as_deref(), Some(copies_before[i].as_slice()));
        assert_eq!(
            row.after_sha.as_deref(),
            Some(sha(&target.join(&row.path)).as_slice())
        );
    }
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(r.copy_run_id, Some(copy_run));
    assert_eq!(r.organizer_run_id, None);
    assert_eq!(
        r.summary,
        Some(json!({"written": 3, "skipped": 0, "failed": 0, "organized": 0}))
    );
    // The web app shows the match through the overrides; the tracks stay where they were.
    assert_eq!(overrides_of(&pool, run).await, 3);
    assert_eq!(album_title(&pool, album).await, "New Album");
    for (i, r) in rels.iter().enumerate() {
        assert_eq!(album_of(&pool, r).await, album);
        let title: String = sqlx::query_scalar("SELECT title FROM tracks WHERE path = $1")
            .bind(r)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(title, format!("Song {}", i + 1));
    }
    assert_eq!(album_state(&pool, album).await, "applied");
    assert_eq!(organizer_state(&pool, copy_run).await, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_refused_when_album_not_fully_copied(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (root, target) = (dir.path(), out.path().join("organized"));
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let copy_run = copy_library(&pool, root, &target).await;
    let copies = copy_paths(&pool, copy_run, &rels).await;
    // One track's copy did not happen.
    sqlx::query(
        "UPDATE organizer_journal SET state = 'skipped' WHERE run_id = $1 AND track_id = $2",
    )
    .bind(copy_run)
    .bind(track_id(&pool, &rels[1]).await)
    .execute(&pool)
    .await
    .unwrap();
    let (originals, copied) = (shas(root, &rels), shas(&target, &copies));

    let c = ctx(&pool, root);
    assert!(
        capability::capabilities(&c).await.unwrap().copy.available,
        "the copy run itself is usable"
    );
    let local = adb::local_album(&pool, album).await.unwrap().unwrap();
    assert_eq!(
        capability::album_refusal(&c, &local, Target::Copy)
            .await
            .unwrap()
            .as_deref(),
        Some(COPY_FIRST)
    );
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Copy).await;
    let res = apply_run(&c, run, &ApplyHooks::default()).await;
    assert!(refused_with(&res, COPY_FIRST), "{res:?}");
    assert!(journal(&pool, run).await.is_empty());
    assert_eq!(shas(root, &rels), originals);
    assert_eq!(shas(&target, &copies), copied);
    assert_eq!(overrides_of(&pool, run).await, 0);
    assert_eq!(album_title(&pool, album).await, "Old Album");
}

/// "New Artist/2001 - New Album/0n - Song n.<ext>", where the organizer files track `n`.
fn organized(n: usize, ext: &str) -> String {
    format!("New Artist/2001 - New Album/0{n} - Song {n}.{ext}")
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reorganize_chains_scoped_move(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Another album, messily named too: the scoped move must leave it alone.
    let other_rels: Vec<String> = (1..=2).map(|i| format!("misc/other/{i}.flac")).collect();
    for (i, r) in other_rels.iter().enumerate() {
        make_audio(
            root,
            r,
            tags(
                "Other Artist",
                "Other Album",
                "x",
                "1990",
                &(i + 1).to_string(),
            ),
        );
    }
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let ids: Vec<Uuid> = {
        let mut v = Vec::new();
        for r in &rels {
            v.push(track_id(&pool, r).await);
        }
        v
    };
    let other_album = album_of(&pool, &other_rels[0]).await;
    let other_shas = shas(root, &other_rels);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run_with(&pool, album, &applied, Target::Files, true).await;

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();

    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(
        r.summary,
        Some(json!({"written": 3, "skipped": 0, "failed": 0, "organized": 3}))
    );
    let org = r.organizer_run_id.expect("organizer run stored");
    let o = odb::get_run(&pool, org).await.unwrap().unwrap();
    assert_eq!(
        (o.mode.as_str(), o.state.as_str(), o.origin.as_str()),
        ("move", "done", "autotag")
    );
    assert_eq!(o.album_ids, Some(vec![album]));
    assert_eq!(count(&pool, "SELECT count(*) FROM organizer_runs").await, 1);
    // The album's files at their organized paths, tagged; the old ones gone.
    let rows = journal(&pool, run).await;
    for (i, id) in ids.iter().enumerate() {
        let want = organized(i + 1, "flac");
        assert_eq!(track_path(&pool, *id).await, want);
        assert!(root.join(&want).is_file(), "{want}");
        assert!(!root.join(&rels[i]).exists(), "{}", rels[i]);
        let row = rows.iter().find(|x| x.track_id == Some(*id)).unwrap();
        let moved = JournalRow {
            path: want.clone(),
            ..row.clone()
        };
        assert_reads_back(root, &moved);
    }
    // The other album: same paths, same bytes.
    for r in &other_rels {
        assert_eq!(album_of(&pool, r).await, other_album);
    }
    assert_eq!(shas(root, &other_rels), other_shas);
    assert_eq!(album_state(&pool, album).await, "applied");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reorganize_resumes_its_organizer_run(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run_with(&pool, album, &applied, Target::Files, true).await;
    // Stops (as a crash would) after the first file moved.
    let hooks = ApplyHooks {
        organize: Hooks {
            stop_after: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    let res = apply_run(&ctx(&pool, root), run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "organizing");
    let org = r.organizer_run_id.expect("stored with the organizer run");
    assert_eq!(organizer_state(&pool, org).await, "applying");

    // Startup: the autotag run is re-queued; the organizer leaves its run to it.
    assert_eq!(organize::resume_interrupted(&pool).await.unwrap(), 0);
    assert_eq!(apply::resume_interrupted(&pool).await.unwrap(), 1);

    // The resumed job goes straight to the re-filing: steps 1-4 are not run again.
    let bus = EventBus::new(1024);
    let mut rx = bus.subscribe();
    apply_run(&ctx_with(&pool, root, bus), run, &ApplyHooks::default())
        .await
        .unwrap();
    let mut states = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if e.data["type"] == "autotag:progress" {
            assert_eq!(e.data["runId"], json!(run));
            states.push(e.data["state"].as_str().unwrap_or("").to_string());
        }
    }
    assert_eq!(
        states.first().map(String::as_str),
        Some("organizing"),
        "{states:?}"
    );
    assert_eq!(
        states.last().map(String::as_str),
        Some("done"),
        "{states:?}"
    );
    assert!(!states.iter().any(|s| s == "writing"), "{states:?}");
    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(r.organizer_run_id, Some(org), "the same organizer run");
    assert_eq!(count(&pool, "SELECT count(*) FROM organizer_runs").await, 1);
    assert_eq!(organizer_state(&pool, org).await, "done");
    assert_eq!(r.summary.as_ref().unwrap()["organized"], json!(3));
    for n in 1..=3 {
        assert!(root.join(organized(n, "flac")).is_file());
    }
    assert_eq!(album_state(&pool, album).await, "applied");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reorganize_failure_keeps_the_tags(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 2, "2001").await;
    let rels = paths("flac", 2);
    // Something already holds the first track's organized name.
    let taken = root.join(organized(1, "flac"));
    std::fs::create_dir_all(taken.parent().unwrap()).unwrap();
    std::fs::write(&taken, b"not ours").unwrap();
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run_with(&pool, album, &applied, Target::Files, true).await;

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();

    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done", "{:?}", r.error);
    assert_eq!(
        r.summary,
        Some(
            json!({"written": 2, "skipped": 0, "failed": 0, "organized": 0,
                    "organizeError": REORGANIZE_COLLISION})
        )
    );
    // The tags and the identity stand; nothing moved; the plan waits for a review.
    for row in journal(&pool, run).await {
        assert_eq!(row.state, "written");
        assert_reads_back(root, &row);
    }
    for r in &rels {
        assert_eq!(album_of(&pool, r).await, album);
    }
    assert_eq!(std::fs::read(&taken).unwrap(), b"not ours");
    assert_eq!(album_title(&pool, album).await, "New Album");
    assert_eq!(album_state(&pool, album).await, "applied");
    let org = r.organizer_run_id.unwrap();
    assert_eq!(organizer_state(&pool, org).await, "previewed");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reorganize_refused_for_copy_and_db(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 2, "2001").await;
    let rels = paths("flac", 2);
    copy_library(&pool, root, &out.path().join("organized")).await;
    let originals = shas(root, &rels);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    for target in [Target::Db, Target::Copy] {
        let run = make_run_with(&pool, album, &applied, target, true).await;
        let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
        assert!(
            refused_with(&res, REORGANIZE_FILES_ONLY),
            "{target:?}: {res:?}"
        );
        assert!(journal(&pool, run).await.is_empty());
        assert_eq!(overrides_of(&pool, run).await, 0);
        assert_eq!(run_row(&pool, run).await.organizer_run_id, None);
    }
    assert_eq!(shas(root, &rels), originals);
    for r in &rels {
        assert!(root.join(r).is_file());
    }
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM organizer_runs WHERE mode = 'move'"
        )
        .await,
        0
    );
    assert_eq!(album_title(&pool, album).await, "Old Album");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn applied_identity_alone_blocks_rematch(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 2, "2001").await;
    let rels = paths("flac", 2);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let first = make_run(&pool, album, &applied, Target::Db).await;
    apply_run(&ctx(&pool, root), first, &ApplyHooks::default())
        .await
        .unwrap();
    // A done run that left no override (as a `files` run without a year does): only the album
    // row says a match is applied.
    let mut conn = pool.acquire().await.unwrap();
    adb::delete_overrides(&mut conn, first).await.unwrap();
    drop(conn);
    let before = shas(root, &rels);
    for target in [Target::Files, Target::Copy, Target::Db] {
        let run = make_run(&pool, album, &applied, target).await;
        let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
        assert!(
            refused_with(&res, APPLIED_MATCH_EXISTS),
            "{target:?}: {res:?}"
        );
        assert!(journal(&pool, run).await.is_empty());
        assert_eq!(overrides_of(&pool, run).await, 0);
        assert_eq!(album_state(&pool, album).await, "applied");
    }
    assert_eq!(shas(root, &rels), before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_root_deletes_copy_overrides(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (root, target) = (dir.path(), out.path().join("organized"));
    // Album B gets a database-only match; its overrides are not the copy's.
    let other_rels: Vec<String> = (1..=2).map(|i| format!("misc/other/{i}.flac")).collect();
    for (i, r) in other_rels.iter().enumerate() {
        make_audio(
            root,
            r,
            tags(
                "Other Artist",
                "Other Album",
                "x",
                "1990",
                &(i + 1).to_string(),
            ),
        );
    }
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let other = album_of(&pool, &other_rels[0]).await;
    let copy_run = copy_library(&pool, root, &target).await;
    let c = ctx(&pool, root);

    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Copy).await;
    apply_run(&c, run, &ApplyHooks::default()).await.unwrap();
    let mut other_applied = applied_for(&pool, &other_rels, "1990", "1990", 1990).await;
    other_applied.album.title = "Other New".into();
    other_applied.album.release_mbid = Uuid::from_u128(0x7777);
    let db_run = make_run(&pool, other, &other_applied, Target::Db).await;
    apply_run(&c, db_run, &ApplyHooks::default()).await.unwrap();
    assert_eq!(overrides_of(&pool, run).await, 3);
    assert_eq!(overrides_of(&pool, db_run).await, 2);

    organize::run_action(&c, copy_run, Action::SwitchRoot, &Hooks::default())
        .await
        .unwrap();
    assert!(
        odb::get_run(&pool, copy_run)
            .await
            .unwrap()
            .unwrap()
            .switched_at
            .is_some()
    );
    assert_eq!(
        overrides_of(&pool, run).await,
        0,
        "the copies' tags are the truth"
    );
    assert_eq!(overrides_of(&pool, db_run).await, 2, "another run's stay");
    assert_eq!(run_row(&pool, run).await.state, "done");

    // The tagged copies are the library: a full re-read keeps the match without overrides.
    let report = scan::reconcile(&ctx(&pool, &target)).await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    let copies: Vec<String> = sqlx::query_scalar(
        "SELECT path FROM tracks WHERE album_id = $1 AND missing_since IS NULL ORDER BY path",
    )
    .bind(album)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(copies.len(), 3);
    assert_eq!(album_title(&pool, album).await, "New Album");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_refused_while_copy_apply_in_progress(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (root, target) = (dir.path(), out.path().join("organized"));
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let copy_run = copy_library(&pool, root, &target).await;
    let originals = shas(root, &rels);
    let c = ctx(&pool, root);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Copy).await;
    let hooks = ApplyHooks {
        stop_after_files: Some(1),
        ..Default::default()
    };
    let res = apply_run(&c, run, &hooks).await;
    assert!(matches!(res, Err(AutotagError::Interrupted)), "{res:?}");
    assert_eq!(run_row(&pool, run).await.copy_run_id, Some(copy_run));

    let res = organize::run_action(&c, copy_run, Action::SwitchRoot, &Hooks::default()).await;
    assert!(
        matches!(&res, Err(OrganizeError::Refused(m)) if m == COPY_IN_USE),
        "{res:?}"
    );
    assert!(
        odb::get_run(&pool, copy_run)
            .await
            .unwrap()
            .unwrap()
            .switched_at
            .is_none()
    );
    assert_eq!(c.root.get(), root);

    // The resumed apply finishes into the same copy; then the switch goes through.
    apply_run(&c, run, &ApplyHooks::default()).await.unwrap();
    let rows = journal(&pool, run).await;
    assert!(
        rows.iter()
            .all(|r| r.state == "written" && r.root == "copy")
    );
    assert_eq!(shas(root, &rels), originals);
    organize::run_action(&c, copy_run, Action::SwitchRoot, &Hooks::default())
        .await
        .unwrap();
    assert_eq!(overrides_of(&pool, run).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn final_attempt_while_organizing_ends_done(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let album = album_fixture(&pool, root, "flac", 2, "2001").await;
    let rels = paths("flac", 2);
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run_with(&pool, album, &applied, Target::Files, true).await;
    let hooks = ApplyHooks {
        fail_at: Some(FailPoint::BeforeOrganize),
        ..Default::default()
    };
    let (c, cancel) = (ctx(&pool, root), CancellationToken::new());

    let res = apply::run_job_with(&c, run, &hooks, &cancel, false).await;
    assert!(
        matches!(res, Err(kima_jobs::worker::JobError::Retry(_))),
        "{res:?}"
    );
    assert_eq!(
        run_row(&pool, run).await.state,
        "organizing",
        "retried later"
    );

    // The last attempt: the tags stand, so the run ends `done` with the reason.
    let res = apply::run_job_with(&c, run, &hooks, &cancel, true).await;
    assert!(matches!(res, Ok(true)), "{res:?}");
    let r = run_row(&pool, run).await;
    assert_eq!((r.state.as_str(), r.error.as_deref()), ("done", None));
    assert_eq!(
        r.summary,
        Some(
            json!({"written": 2, "skipped": 0, "failed": 0, "organized": 0,
                    "organizeError": "forced failure (test hook)"})
        )
    );
    assert_eq!(r.organizer_run_id, None);
    assert_eq!(album_state(&pool, album).await, "applied");
    assert_eq!(album_title(&pool, album).await, "New Album");
    for row in journal(&pool, run).await {
        assert_eq!(row.state, "written");
        assert_reads_back(root, &row);
    }
}

// ---------------------------------------------------------------------------------------------
// Task 17 fix round 1

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_unchanged_rows_not_offered(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (root, target) = (dir.path(), out.path().join("organized"));
    let album = album_fixture(&pool, root, "flac", 2, "2001").await;
    let rels = paths("flac", 2);
    let first = copy_library(&pool, root, &target).await;
    let copies = copy_paths(&pool, first, &rels).await;
    // A second copy run into the same target: every row `unchanged` (verified), none `done`.
    let second = copy_library(&pool, root, &target).await;
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM organizer_journal WHERE run_id = $1 AND track_id IS NOT NULL",
    )
    .bind(second)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(states.iter().all(|s| s == "unchanged"), "{states:?}");
    let copied = shas(&target, &copies);

    let c = ctx(&pool, root);
    let local = adb::local_album(&pool, album).await.unwrap().unwrap();
    assert_eq!(
        capability::album_refusal(&c, &local, Target::Copy)
            .await
            .unwrap()
            .as_deref(),
        Some(COPY_FIRST)
    );
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Copy).await;
    let res = apply_run(&c, run, &ApplyHooks::default()).await;
    assert!(refused_with(&res, COPY_FIRST), "{res:?}");
    assert!(journal(&pool, run).await.is_empty());
    assert_eq!(shas(&target, &copies), copied, "no copy rewritten");

    // So the second copy can still become the library.
    organize::run_action(&c, second, Action::SwitchRoot, &Hooks::default())
        .await
        .unwrap();
    assert!(
        odb::get_run(&pool, second)
            .await
            .unwrap()
            .unwrap()
            .switched_at
            .is_some()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn copy_overrides_only_for_tagged_copies(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (root, target) = (dir.path(), out.path().join("organized"));
    let album = album_fixture(&pool, root, "flac", 3, "2001").await;
    let rels = paths("flac", 3);
    let copy_run = copy_library(&pool, root, &target).await;
    let copies = copy_paths(&pool, copy_run, &rels).await;
    let second = track_id(&pool, &rels[1]).await;
    // The second copy is re-tagged elsewhere between the journal and its write.
    let changed = target.join(&copies[1]);
    let hooks = ApplyHooks {
        after_journal: Some(Arc::new(move || retag_elsewhere(&changed))),
        ..Default::default()
    };
    let applied = applied_for(&pool, &rels, "2001", "2001", 2001).await;
    let run = make_run(&pool, album, &applied, Target::Copy).await;
    apply_run(&ctx(&pool, root), run, &hooks).await.unwrap();

    let rows = journal(&pool, run).await;
    let failed: Vec<_> = rows.iter().filter(|r| r.state == "failed").collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].track_id, Some(second));
    assert_eq!(failed[0].note.as_deref(), Some(CHANGED_OUTSIDE));
    let with_override: Vec<Uuid> =
        sqlx::query_scalar("SELECT track_id FROM track_tag_overrides WHERE run_id = $1")
            .bind(run)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(with_override.len(), 2);
    assert!(
        !with_override.contains(&second),
        "the untagged copy shows no match"
    );
    assert_eq!(run_row(&pool, run).await.state, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn scoped_reorganize_leaves_a_shared_folders_sidecar(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // One folder: two tracks of the album, one of another album, and a cover.
    let mine: Vec<String> = (1..=2).map(|i| format!("mix/a{i}.flac")).collect();
    for (i, r) in mine.iter().enumerate() {
        make_audio(
            root,
            r,
            tags(
                "Old Artist",
                "Old Album",
                "old",
                "2001",
                &(i + 1).to_string(),
            ),
        );
    }
    make_audio(
        root,
        "mix/b1.flac",
        tags("Other Artist", "Other", "x", "1990", "1"),
    );
    std::fs::write(root.join("mix/cover.jpg"), b"jpeg").unwrap();
    rescan(&pool, root).await;
    let album = album_of(&pool, &mine[0]).await;
    let other = track_id(&pool, "mix/b1.flac").await;
    let applied = applied_for(&pool, &mine, "2001", "2001", 2001).await;
    let run = make_run_with(&pool, album, &applied, Target::Files, true).await;

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();

    let r = run_row(&pool, run).await;
    assert_eq!(r.state, "done");
    assert_eq!(r.summary.as_ref().unwrap()["organized"], json!(2));
    for n in 1..=2 {
        assert!(root.join(organized(n, "flac")).is_file());
    }
    // The cover stays with the album that is not re-filed.
    assert_eq!(std::fs::read(root.join("mix/cover.jpg")).unwrap(), b"jpeg");
    assert!(!root.join("New Artist/2001 - New Album/cover.jpg").exists());
    assert_eq!(track_path(&pool, other).await, "mix/b1.flac");
    assert!(root.join("mix/b1.flac").is_file());
}
