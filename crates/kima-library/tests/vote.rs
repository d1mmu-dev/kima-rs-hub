//! One artist per album, elected from all its songs (M-SYNC Task 7, upstream 1.12
//! `musicScanner.albumArtist.test.ts`; D-164). Real files, real scanner, real database.
mod common;

use std::path::Path;

use common::{Tags, make_audio, require_ffmpeg};
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_library::scan::{self, LibraryCtx, PathOutcome, ReadCause};
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

/// A song tagged with an album artist (and the same track artist), or with only a track artist.
fn song(root: &Path, rel: &str, album: &str, album_artist: Option<&str>, artist: &str) {
    make_audio(
        root,
        rel,
        Tags {
            album_artist: album_artist.map(str::to_string),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            title: Some(rel.to_string()),
            ..Default::default()
        },
    );
}

fn flagged(root: &Path, rel: &str, album: &str, artist: &str) {
    make_audio(
        root,
        rel,
        Tags {
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            title: Some(rel.to_string()),
            compilation: true,
            ..Default::default()
        },
    );
}

async fn rescan(pool: &PgPool, root: &Path) -> scan::ScanReport {
    let r = scan::reconcile(&ctx(pool, root)).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    r
}

/// (artist, songs) of every album with this title, sorted by artist.
async fn albums(pool: &PgPool, title: &str) -> Vec<(String, i64)> {
    sqlx::query_as(
        "SELECT ar.name, count(t.id) FROM albums al
         JOIN artists ar ON ar.id = al.artist_id
         JOIN tracks t ON t.album_id = al.id
         WHERE al.title = $1 GROUP BY ar.name ORDER BY ar.name",
    )
    .bind(title)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn artist_count(pool: &PgPool, name: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM artists WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn artist_of(pool: &PgPool, path: &str) -> String {
    sqlx::query_scalar(
        "SELECT ar.name FROM tracks t JOIN albums al ON al.id = t.album_id
         JOIN artists ar ON ar.id = al.artist_id WHERE t.path = $1",
    )
    .bind(path)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn row(artist: &str, songs: i64) -> Vec<(String, i64)> {
    vec![(artist.to_string(), songs)]
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lone_guest_in_album_artist_tag_does_not_split_the_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for n in 1..=3 {
        let rel = format!("Mike/History (1995)/Disc 2/0{n}.flac");
        song(root, &rel, "History", Some("Mike"), "Mike");
    }
    song(
        root,
        "Mike/History (1995)/Disc 2/04.flac",
        "History",
        Some("Janet"),
        "Janet",
    );
    rescan(&pool, root).await;
    assert_eq!(albums(&pool, "History").await, row("Mike", 4));
    assert_eq!(artist_count(&pool, "Janet").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn disc_folders_vote_as_one_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    song(
        root,
        "Band/Double/CD 1/01.flac",
        "Double",
        Some("Band"),
        "Band",
    );
    song(
        root,
        "Band/Double/CD 2/01.flac",
        "Double",
        Some("Band"),
        "Band",
    );
    song(
        root,
        "Band/Double/CD 2/02.flac",
        "Double",
        Some("Guest"),
        "Guest",
    );
    rescan(&pool, root).await;
    assert_eq!(albums(&pool, "Double").await, row("Band", 3));
    assert_eq!(artist_count(&pool, "Guest").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn most_common_covering_a_quarter_wins(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Two of four songs name Lead: half is enough; the others keep nothing of their own.
    song(root, "L/Quarter/01.flac", "Quarter", Some("Lead"), "Lead");
    song(root, "L/Quarter/02.flac", "Quarter", Some("Lead"), "Lead");
    song(root, "L/Quarter/03.flac", "Quarter", Some("Two"), "Two");
    song(root, "L/Quarter/04.flac", "Quarter", Some("Three"), "Three");
    rescan(&pool, root).await;
    assert_eq!(albums(&pool, "Quarter").await, row("Lead", 4));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_leader_keeps_each_songs_own_artist_unless_a_compilation(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (i, a) in ["One", "Two", "Three"].iter().enumerate() {
        song(root, &format!("Mix/Mixtape/0{i}.flac"), "Mixtape", None, a);
        flagged(
            root,
            &format!("Mix/Flagged/0{i}.flac"),
            "Flagged Mixtape",
            a,
        );
    }
    rescan(&pool, root).await;
    assert_eq!(
        albums(&pool, "Mixtape").await,
        vec![("One".into(), 1), ("Three".into(), 1), ("Two".into(), 1)]
    );
    assert_eq!(
        albums(&pool, "Flagged Mixtape").await,
        row("Various Artists", 3)
    );
    let is_comp: bool =
        sqlx::query_scalar("SELECT is_compilation FROM albums WHERE title = 'Flagged Mixtape'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(is_comp);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn compilation_with_no_album_artist_and_a_lead_is_still_various_artists(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (i, a) in ["Lead", "Lead", "Other"].iter().enumerate() {
        flagged(
            root,
            &format!("Comp/Compilation/0{i}.flac"),
            "Compilation",
            a,
        );
    }
    rescan(&pool, root).await;
    assert_eq!(
        albums(&pool, "Compilation").await,
        row("Various Artists", 3)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stray_compilation_flag_on_a_one_artist_album_is_ignored(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for n in 1..=3 {
        flagged(
            root,
            &format!("Solo/Solo Album/0{n}.flac"),
            "Solo Album",
            "Solo",
        );
    }
    rescan(&pool, root).await;
    assert_eq!(albums(&pool, "Solo Album").await, row("Solo", 3));
    // The organizer files a compilation under Various Artists: a stray flag must not do it.
    let is_comp: bool =
        sqlx::query_scalar("SELECT is_compilation FROM albums WHERE title = 'Solo Album'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!is_comp);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn featured_credit_is_the_same_artist_when_deciding_a_compilation(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (n, a) in [(1, "Singer"), (2, "Singer (feat. Guest)"), (3, "Singer")] {
        flagged(root, &format!("Feat/Feat Album/0{n}.flac"), "Feat Album", a);
    }
    rescan(&pool, root).await;
    let got = albums(&pool, "Feat Album").await;
    assert!(got.iter().all(|(a, _)| a != "Various Artists"), "{got:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn loose_songs_and_same_titled_albums_of_different_artists_keep_their_own(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    make_audio(
        root,
        "Loose/a.mp3",
        Tags {
            artist: Some("Loose One".into()),
            title: Some("A".into()),
            ..Default::default()
        },
    );
    make_audio(
        root,
        "Loose/b.mp3",
        Tags {
            artist: Some("Loose Two".into()),
            title: Some("B".into()),
            ..Default::default()
        },
    );
    song(
        root,
        "Loose/c.mp3",
        "Greatest Hits",
        Some("Hits One"),
        "Hits One",
    );
    song(
        root,
        "Loose/d.mp3",
        "Greatest Hits",
        Some("Hits Two"),
        "Hits Two",
    );
    rescan(&pool, root).await;
    assert_eq!(artist_of(&pool, "Loose/a.mp3").await, "Loose One");
    assert_eq!(artist_of(&pool, "Loose/b.mp3").await, "Loose Two");
    assert_eq!(artist_of(&pool, "Loose/c.mp3").await, "Hits One");
    assert_eq!(artist_of(&pool, "Loose/d.mp3").await, "Hits Two");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn singles_song_tagged_a_compilation_files_under_its_folders_artist(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    flagged(
        root,
        "Singles/Folder Artist/Single Comp/01.flac",
        "Single Comp",
        "Folder Artist",
    );
    song(root, "Singles/Playlist/01.flac", "Single A", None, "Alpha");
    song(root, "Singles/Playlist/02.flac", "Single B", None, "Beta");
    rescan(&pool, root).await;
    assert_eq!(albums(&pool, "Single Comp").await, row("Folder Artist", 1));
    assert_eq!(albums(&pool, "Single A").await, row("Alpha", 1));
    assert_eq!(albums(&pool, "Single B").await, row("Beta", 1));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_new_song_is_filed_by_the_votes_of_the_songs_already_scanned(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for n in 1..=3 {
        song(
            root,
            &format!("Grow/Growing/0{n}.flac"),
            "Growing",
            Some("Grow"),
            "Grow",
        );
    }
    rescan(&pool, root).await;
    song(
        root,
        "Grow/Growing/04.flac",
        "Growing",
        Some("Guest"),
        "Guest",
    );
    let r = rescan(&pool, root).await;
    // Only the new song is read: the three scanned ones vote from what the scan stored.
    assert_eq!(r.tags_read, 1);
    assert_eq!(albums(&pool, "Growing").await, row("Grow", 4));
    assert_eq!(artist_count(&pool, "Guest").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_watched_new_song_joins_the_lead_artists_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for n in 1..=3 {
        song(
            root,
            &format!("Grow/Watched/CD 1/0{n}.flac"),
            "Watched",
            Some("Grow"),
            "Grow",
        );
    }
    rescan(&pool, root).await;
    // A new song in the album's other disc folder, found by the watcher.
    song(
        root,
        "Grow/Watched/CD 2/01.flac",
        "Watched",
        Some("Guest"),
        "Guest",
    );
    let out = scan::update_path(&ctx(&pool, root), "Grow/Watched/CD 2/01.flac")
        .await
        .unwrap();
    assert_eq!(out, PathOutcome::Added);
    assert_eq!(albums(&pool, "Watched").await, row("Grow", 4));
}

/// A library scanned before the election has no stored candidates and may hold a stray guest
/// album. An unchanged scan changes nothing; re-reading the stray merges it into the elected
/// album, unless the stray album was applied by autotag (a person's choice).
async fn legacy_stray(pool: &PgPool, root: &Path) -> Uuid {
    for n in 1..=3 {
        song(
            root,
            &format!("Mike/Hist/0{n}.flac"),
            "Hist",
            Some("Mike"),
            "Mike",
        );
    }
    song(root, "Mike/Hist/04.flac", "Hist", Some("Janet"), "Janet");
    rescan(pool, root).await;
    assert_eq!(albums(pool, "Hist").await, row("Mike", 4));

    let (artist, album) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'Janet', 'janet')")
        .bind(artist)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 'Hist', 'hist')",
    )
    .bind(album)
    .bind(artist)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET album_id = $1 WHERE path = 'Mike/Hist/04.flac'")
        .bind(album)
        .execute(pool)
        .await
        .unwrap();
    // What a pre-election scan stored: no candidates.
    sqlx::query("UPDATE tracks SET guessed = guessed - 'credit'")
        .execute(pool)
        .await
        .unwrap();
    album
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn existing_album_not_resplit(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    legacy_stray(&pool, root).await;
    let both = vec![("Janet".to_string(), 1), ("Mike".to_string(), 3)];

    // Nothing changed on disk: nothing is read, nothing merges or splits.
    let r = rescan(&pool, root).await;
    assert_eq!(r.tags_read, 0);
    assert_eq!(albums(&pool, "Hist").await, both);

    // A forced re-read of the stray votes with its siblings' stored artists: Mike leads.
    let paths = ["Mike/Hist/04.flac".to_string()];
    scan::reread_paths(&ctx(&pool, root), &paths, ReadCause::Scan)
        .await
        .unwrap();
    assert_eq!(albums(&pool, "Hist").await, row("Mike", 4));
    assert_eq!(artist_count(&pool, "Janet").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_rereads_unchanged_files_and_merges_stray_guest(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    legacy_stray(&pool, root).await;
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert_eq!(report.tags_read, 4);
    assert_eq!(report.unchanged, 0);
    assert_eq!(albums(&pool, "Hist").await, row("Mike", 4));
    assert_eq!(artist_count(&pool, "Janet").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reread_all_job_dispatches_forced_scan(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    legacy_stray(&pool, root).await;
    let changed = kima_library::jobs::run(
        &ctx(&pool, root),
        &serde_json::json!({"kind": "reread_all"}),
        &Default::default(),
    )
    .await
    .unwrap();
    assert!(changed);
    assert_eq!(albums(&pool, "Hist").await, row("Mike", 4));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_keeps_person_applied_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stray = legacy_stray(&pool, root).await;
    sqlx::query("UPDATE albums SET identity_source = 'autotag', applied_identity = '{\"artistKey\":\"janet\",\"titleKey\":\"hist\",\"year\":null}' WHERE id = $1")
        .bind(stray).execute(&pool).await.unwrap();
    scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert_eq!(
        albums(&pool, "Hist").await,
        vec![("Janet".to_string(), 1), ("Mike".to_string(), 3)]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_preserves_display_edits_and_unreadable_song(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    song(root, "Mike/Hist/01.flac", "Hist", Some("Mike"), "Mike");
    song(root, "Mike/Hist/02.flac", "Hist", Some("Mike"), "Mike");
    rescan(&pool, root).await;
    sqlx::query("UPDATE artists SET display_name = 'My Artist' WHERE name = 'Mike'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE albums SET display_title = 'My Album' WHERE title = 'Hist'")
        .execute(&pool)
        .await
        .unwrap();
    let before: (Uuid, Uuid) =
        sqlx::query_as("SELECT t.id, t.album_id FROM tracks t WHERE t.path = 'Mike/Hist/02.flac'")
            .fetch_one(&pool)
            .await
            .unwrap();
    std::fs::write(root.join("Mike/Hist/02.flac"), b"not audio anymore").unwrap();
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert_eq!(report.tags_read, 2);
    assert_eq!(report.failed.len(), 1, "{report:?}");
    let after: (Uuid, Uuid) =
        sqlx::query_as("SELECT t.id, t.album_id FROM tracks t WHERE t.path = 'Mike/Hist/02.flac'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(after, before);
    assert_eq!(albums(&pool, "Hist").await, row("Mike", 2));
    let edits: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT ar.display_name, al.display_title FROM albums al JOIN artists ar ON ar.id = al.artist_id WHERE al.id = $1"
    ).bind(before.1).fetch_one(&pool).await.unwrap();
    assert_eq!(edits, (Some("My Artist".into()), Some("My Album".into())));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn edited_album_is_never_merged_into(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stray = legacy_stray(&pool, root).await;
    // The stray album was applied by autotag: Janet / Hist is a person's choice.
    sqlx::query(
        "UPDATE albums SET identity_source = 'autotag',
            applied_identity = '{\"artistKey\":\"janet\",\"titleKey\":\"hist\",\"year\":null}'
         WHERE id = $1",
    )
    .bind(stray)
    .execute(&pool)
    .await
    .unwrap();
    let paths = ["Mike/Hist/04.flac".to_string()];
    scan::reread_paths(&ctx(&pool, root), &paths, ReadCause::Scan)
        .await
        .unwrap();
    assert_eq!(
        albums(&pool, "Hist").await,
        vec![("Janet".to_string(), 1), ("Mike".to_string(), 3)]
    );
}
