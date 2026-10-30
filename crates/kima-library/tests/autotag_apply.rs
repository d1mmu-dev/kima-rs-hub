//! The autotag apply job (spec §19.9): files and database targets, journal, crash recovery.

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
use kima_library::autotag::apply::{self, ApplyHooks, AutotagError};
use kima_library::autotag::{CHANGED_OUTSIDE, FILES_READ_ONLY, TRACKS_CHANGED};
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
            reorganize: false,
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
    use std::os::unix::fs::PermissionsExt;
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

    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o555)).unwrap();
    if kima_library::organize::apply::writable(root) {
        // Running as root: mode bits do not stop the probe.
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("the root stays writable for this user; skipping");
        return;
    }
    let res = apply_run(&ctx(&pool, root), run, &ApplyHooks::default()).await;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();

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

    apply_run(&ctx(&pool, root), run, &ApplyHooks::default())
        .await
        .unwrap();
    assert!(leftovers(root).is_empty(), "{:?}", leftovers(root));
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
