//! One artist per album, elected from all its songs (M-SYNC Task 7, upstream 1.12
//! `musicScanner.albumArtist.test.ts`; D-164). Real files, real scanner, real database.
mod common;

use std::path::Path;

use common::{Picard, Tags, make_audio, require_ffmpeg, tag_picard};
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
    let before: Vec<(String, Uuid, Uuid)> =
        sqlx::query_as("SELECT t.path, t.id, t.album_id FROM tracks t ORDER BY t.path")
            .fetch_all(&pool)
            .await
            .unwrap();
    let again = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(again.failed.is_empty(), "{again:?}");
    let after: Vec<(String, Uuid, Uuid)> =
        sqlx::query_as("SELECT t.path, t.id, t.album_id FROM tracks t ORDER BY t.path")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        after, before,
        "a second re-file keeps row identities and filing"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn second_refile_of_an_unchanged_library_writes_nothing(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    legacy_stray(&pool, root).await;
    let first = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(
        first.updated > 0,
        "the first re-file files the stray: {first:?}"
    );
    let rows = "SELECT path, updated_at, to_jsonb(t)::text FROM tracks t ORDER BY path";
    let before: Vec<(String, time::OffsetDateTime, String)> =
        sqlx::query_as(rows).fetch_all(&pool).await.unwrap();

    let again = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(again.failed.is_empty(), "{again:?}");
    assert_eq!(again.tags_read, 4, "a re-file still reads every file");
    assert_eq!(again.updated, 0, "nothing changed on disk: {again:?}");
    let after: Vec<(String, time::OffsetDateTime, String)> =
        sqlx::query_as(rows).fetch_all(&pool).await.unwrap();
    assert_eq!(after, before, "no row is rewritten, updated_at included");
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
async fn refile_keeps_last_song_and_edits_on_edited_source_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = legacy_stray(&pool, root).await;
    sqlx::query(
        "UPDATE albums SET display_title = 'My Hist', display_year = 1995,
        user_genres = ARRAY['Jazz'], image_source = 'user', image_path = 'my-cover' WHERE id = $1",
    )
    .bind(source)
    .execute(&pool)
    .await
    .unwrap();
    let before: Uuid = sqlx::query_scalar("SELECT id FROM tracks WHERE path = 'Mike/Hist/04.flac'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let filing: (Uuid, Uuid, String) = sqlx::query_as(
        "SELECT t.id, al.id, ar.name FROM tracks t
         JOIN albums al ON al.id = t.album_id JOIN artists ar ON ar.id = al.artist_id
         WHERE t.path = 'Mike/Hist/04.flac'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(filing, (before, source, "Janet".into()));
    #[derive(Debug, PartialEq, sqlx::FromRow)]
    struct Edits {
        display_title: Option<String>,
        display_year: Option<i32>,
        user_genres: Option<Vec<String>>,
        image_source: Option<String>,
        image_path: Option<String>,
    }
    let edits: Edits = sqlx::query_as(
        "SELECT display_title, display_year, user_genres, image_source, image_path FROM albums WHERE id = $1",
    ).bind(source).fetch_one(&pool).await.unwrap();
    assert_eq!(
        edits,
        Edits {
            display_title: Some("My Hist".into()),
            display_year: Some(1995),
            user_genres: Some(vec!["Jazz".into()]),
            image_source: Some("user".into()),
            image_path: Some("my-cover".into()),
        }
    );
    assert_eq!(
        albums(&pool, "Hist").await,
        vec![("Janet".into(), 1), ("Mike".into(), 3)]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_keeps_moved_last_song_on_edited_source_album(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = legacy_stray(&pool, root).await;
    sqlx::query(
        "UPDATE albums SET display_title = 'My Hist', user_genres = ARRAY['Jazz'] WHERE id = $1",
    )
    .bind(source)
    .execute(&pool)
    .await
    .unwrap();
    let before: Uuid = sqlx::query_scalar("SELECT id FROM tracks WHERE path = 'Mike/Hist/04.flac'")
        .fetch_one(&pool)
        .await
        .unwrap();
    std::fs::rename(
        root.join("Mike/Hist/04.flac"),
        root.join("Mike/Hist/05.flac"),
    )
    .unwrap();
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let (id, album): (Uuid, Uuid) =
        sqlx::query_as("SELECT id, album_id FROM tracks WHERE path = 'Mike/Hist/05.flac'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (id, album),
        (before, source),
        "the moved row must retain its edited source album"
    );
    let edits: (Option<String>, Option<Vec<String>>) =
        sqlx::query_as("SELECT display_title, user_genres FROM albums WHERE id = $1")
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(edits, (Some("My Hist".into()), Some(vec!["Jazz".into()])));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_leaves_edited_albums_alone(pool: PgPool) {
    assert!(require_ffmpeg(), "real audio fixtures require ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    song(root, "Mike/Hist/01.flac", "Hist", Some("Mike"), "Mike");
    song(root, "Mike/Hist/02.flac", "Hist", Some("Mike"), "Mike");
    rescan(&pool, root).await;
    let before: Vec<(Uuid, Uuid)> = sqlx::query_as("SELECT id, album_id FROM tracks ORDER BY path")
        .fetch_all(&pool)
        .await
        .unwrap();
    let album = before[0].1;
    let identity = serde_json::json!({"artistKey": "mike", "titleKey": "hist", "year": null});
    sqlx::query("UPDATE albums SET identity_source = 'autotag', applied_identity = $1, display_title = 'My Album', display_year = 1995, user_genres = ARRAY['Jazz'], image_source = 'user', image_path = 'my-cover' WHERE id = $2")
        .bind(&identity).bind(album).execute(&pool).await.unwrap();
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(report.tags_read, 2);
    assert_eq!(
        report.unchanged, 0,
        "forced reads are not incremental skips"
    );
    let after: Vec<(Uuid, Uuid)> = sqlx::query_as("SELECT id, album_id FROM tracks ORDER BY path")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(after, before);
    let edits: serde_json::Value = sqlx::query_scalar("SELECT jsonb_build_object('identity', identity_source, 'applied', applied_identity, 'title', display_title, 'year', display_year, 'genres', user_genres, 'imageSource', image_source, 'imagePath', image_path) FROM albums WHERE id = $1")
        .bind(album).fetch_one(&pool).await.unwrap();
    assert_eq!(
        edits,
        serde_json::json!({
            "identity": "autotag", "applied": identity, "title": "My Album",
            "year": 1995, "genres": ["Jazz"], "imageSource": "user", "imagePath": "my-cover"
        })
    );
    assert_eq!(artist_count(&pool, "Mike").await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_moved_edited_track_refreshes_album_metadata(pool: PgPool) {
    assert!(require_ffmpeg(), "real audio fixtures require ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    song(root, "Band/Album/01.flac", "Album", Some("Band"), "Band");
    tag_picard(
        &root.join("Band/Album/01.flac"),
        &Picard {
            release: Some("12345678-1234-4234-8234-1234567890ab"),
            genres: vec!["Jazz"],
            ..Default::default()
        },
    );
    rescan(&pool, root).await;
    let (id, album): (Uuid, Uuid) = sqlx::query_as("SELECT id, album_id FROM tracks")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mbid = Uuid::from_u128(0x123456781234423482341234567890ab);
    sqlx::query("UPDATE albums SET display_title = 'My Album' WHERE id = $1")
        .bind(album)
        .execute(&pool)
        .await
        .unwrap();
    std::fs::rename(
        root.join("Band/Album/01.flac"),
        root.join("Band/Album/02.flac"),
    )
    .unwrap();
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(report.repointed, 1);
    let after: (Uuid, Uuid, bool) = sqlx::query_as(
        "SELECT id, album_id, missing_since IS NULL FROM tracks WHERE path = 'Band/Album/02.flac'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after, (id, album, true));
    let metadata: (Option<String>, Option<Uuid>) =
        sqlx::query_as("SELECT display_title, release_mbid FROM albums WHERE id = $1")
            .bind(album)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(metadata, (Some("My Album".into()), Some(mbid)));
    let album_genres: Vec<String> = sqlx::query_scalar("SELECT g.name FROM album_genres x JOIN genres g ON g.id = x.genre_id WHERE x.album_id = $1 AND x.source = 'file' ORDER BY x.rank")
        .bind(album).fetch_all(&pool).await.unwrap();
    assert_eq!(album_genres, vec!["Jazz"]);
    let artist_genres: Vec<String> = sqlx::query_scalar("SELECT g.name FROM artist_genres x JOIN genres g ON g.id = x.genre_id JOIN albums al ON al.artist_id = x.artist_id WHERE al.id = $1 AND x.source = 'file' ORDER BY x.rank")
        .bind(album).fetch_all(&pool).await.unwrap();
    assert_eq!(artist_genres, vec!["Jazz"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_moved_autotag_only_identity_is_preserved(pool: PgPool) {
    assert!(require_ffmpeg(), "real audio fixtures require ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = legacy_stray(&pool, root).await;
    let identity = r#"{"artistKey":"janet","titleKey":"hist","year":null}"#;
    sqlx::query(
        "UPDATE albums SET identity_source = 'autotag', applied_identity = $1::jsonb WHERE id = $2",
    )
    .bind(identity)
    .bind(source)
    .execute(&pool)
    .await
    .unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT id FROM tracks WHERE path = 'Mike/Hist/04.flac'")
        .fetch_one(&pool)
        .await
        .unwrap();
    std::fs::rename(
        root.join("Mike/Hist/04.flac"),
        root.join("Mike/Hist/05.flac"),
    )
    .unwrap();
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let after: (Uuid, Uuid, String, Option<serde_json::Value>) = sqlx::query_as("SELECT t.id, t.album_id, al.identity_source, al.applied_identity FROM tracks t JOIN albums al ON al.id = t.album_id WHERE t.path = 'Mike/Hist/05.flac'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(
        after,
        (
            id,
            source,
            "autotag".into(),
            Some(serde_json::from_str(identity).unwrap())
        )
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_processes_unrelated_new_song_beside_moved_edited_track(pool: PgPool) {
    assert!(require_ffmpeg(), "real audio fixtures require ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = legacy_stray(&pool, root).await;
    sqlx::query("UPDATE albums SET display_title = 'My Hist' WHERE id = $1")
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    std::fs::rename(
        root.join("Mike/Hist/04.flac"),
        root.join("Mike/Hist/05.flac"),
    )
    .unwrap();
    song(
        root,
        "Mike/Hist/new.flac",
        "Unrelated",
        Some("Other"),
        "Other",
    );
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(report.added, 1, "{report:?}");
    assert_eq!(artist_of(&pool, "Mike/Hist/new.flac").await, "Other");
    assert_eq!(albums(&pool, "Unrelated").await, row("Other", 1));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_does_not_elect_from_incomplete_unreadable_folder(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (n, artist) in [(1, "Mike"), (2, "Mike"), (3, "Janet"), (4, "Janet")] {
        song(
            root,
            &format!("Mike/Hist/0{n}.flac"),
            "Hist",
            Some(artist),
            artist,
        );
    }
    rescan(&pool, root).await;
    // A legacy split: without unreadable Mike's vote Janet wins 2:1.
    let (artist, stray) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'Janet', 'janet')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 'Hist', 'hist')",
    )
    .bind(stray)
    .bind(artist)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE tracks SET album_id = $1 WHERE path IN ('Mike/Hist/03.flac', 'Mike/Hist/04.flac')",
    )
    .bind(stray)
    .execute(&pool)
    .await
    .unwrap();
    std::fs::write(root.join("Mike/Hist/01.flac"), b"unreadable tags").unwrap();
    let before: Vec<(String, Uuid)> =
        sqlx::query_as("SELECT path, album_id FROM tracks ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].0, "Mike/Hist/01.flac");
    let after: Vec<(String, Uuid)> =
        sqlx::query_as("SELECT path, album_id FROM tracks ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        after, before,
        "an incomplete vote must not re-file the folder"
    );
    assert_eq!(
        albums(&pool, "Hist").await,
        vec![("Janet".into(), 2), ("Mike".into(), 2)]
    );
}

#[cfg(unix)]
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_does_not_elect_when_disc_folder_cannot_be_walked(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (n, artist) in [
        (1, "Mike"),
        (2, "Mike"),
        (3, "Mike"),
        (4, "Janet"),
        (5, "Janet"),
    ] {
        let disc = if n <= 2 { "CD 1" } else { "CD 2" };
        song(
            root,
            &format!("Mike/Hist/{disc}/0{n}.flac"),
            "Hist",
            Some(artist),
            artist,
        );
    }
    rescan(&pool, root).await;
    let (artist, stray) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'Janet', 'janet')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 'Hist', 'hist')",
    )
    .bind(stray)
    .bind(artist)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET album_id = $1 WHERE path IN ('Mike/Hist/CD 2/04.flac', 'Mike/Hist/CD 2/05.flac')")
        .bind(stray).execute(&pool).await.unwrap();
    let before: Vec<(String, Uuid, Uuid)> =
        sqlx::query_as("SELECT path, id, album_id FROM tracks ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    let disc = root.join("Mike/Hist/CD 1");
    let original = std::fs::metadata(&disc).unwrap().permissions();
    std::fs::set_permissions(&disc, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&disc).is_ok() {
        std::fs::set_permissions(&disc, original).unwrap();
        eprintln!("SKIPPED: this process can still read chmod 000 (root/capability)");
        return;
    }
    let result = scan::refile_cancellable(&ctx(&pool, root), &Default::default()).await;
    std::fs::set_permissions(&disc, original).unwrap();
    let report = result.unwrap();
    assert!(
        report
            .failed
            .iter()
            .any(|(path, _)| path == "Mike/Hist/CD 1"),
        "{report:?}"
    );
    let after: Vec<(String, Uuid, Uuid)> =
        sqlx::query_as("SELECT path, id, album_id FROM tracks ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        after, before,
        "an unreadable disc's missing votes must not change the album"
    );
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

#[cfg(unix)]
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_shields_disc_with_walker_metadata_errors(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (n, artist) in [
        (1, "Mike"),
        (2, "Mike"),
        (3, "Mike"),
        (4, "Janet"),
        (5, "Janet"),
    ] {
        let disc = if n <= 2 { "CD 1" } else { "CD 2" };
        song(
            root,
            &format!("Mike/Hist/{disc}/0{n}.flac"),
            "Hist",
            Some(artist),
            artist,
        );
    }
    rescan(&pool, root).await;
    let (artist, stray) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'Janet', 'janet')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 'Hist', 'hist')",
    )
    .bind(stray)
    .bind(artist)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET album_id = $1 WHERE path IN ('Mike/Hist/CD 2/04.flac', 'Mike/Hist/CD 2/05.flac')")
        .bind(stray).execute(&pool).await.unwrap();
    let before: Vec<(String, Uuid, Uuid)> =
        sqlx::query_as("SELECT path, id, album_id FROM tracks ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    let disc = root.join("Mike/Hist/CD 1");
    let original = std::fs::metadata(&disc).unwrap().permissions();
    std::fs::set_permissions(&disc, std::fs::Permissions::from_mode(0o400)).unwrap();
    assert!(
        std::fs::read_dir(&disc).is_ok(),
        "directory listing must succeed"
    );
    assert!(
        std::fs::metadata(disc.join("01.flac")).is_err(),
        "non-root process must be unable to stat the song"
    );
    let result = scan::refile_cancellable(&ctx(&pool, root), &Default::default()).await;
    std::fs::set_permissions(&disc, original).unwrap();
    let report = result.unwrap();
    assert!(
        report
            .failed
            .iter()
            .any(|(path, _)| path == "Mike/Hist/CD 1/01.flac"),
        "{report:?}"
    );
    assert_eq!(report.missing, 0, "{report:?}");
    let after: Vec<(String, Uuid, Uuid)> =
        sqlx::query_as("SELECT path, id, album_id FROM tracks ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        after, before,
        "an unreadable disc's missing votes must not change the album"
    );
}

/// A protected album whose first song was marked missing more than the grace period ago,
/// although the file is back at its original path. Returns (album, returned track, present
/// sibling track).
async fn protected_with_expired_missing(
    pool: &PgPool,
    root: &Path,
    protect: &str,
) -> (Uuid, Uuid, Uuid) {
    for n in 1..=3 {
        song(
            root,
            &format!("Band/Album/0{n}.flac"),
            "Album",
            Some("Band"),
            "Band",
        );
    }
    // Other present rows, so the library is not empty and the mass-missing guard stays off.
    for n in 1..=3 {
        song(
            root,
            &format!("Keep/K/0{n}.flac"),
            "K",
            Some("Keep"),
            "Keep",
        );
    }
    rescan(pool, root).await;
    let (back, album): (Uuid, Uuid) =
        sqlx::query_as("SELECT id, album_id FROM tracks WHERE path = 'Band/Album/01.flac'")
            .fetch_one(pool)
            .await
            .unwrap();
    let present: Uuid =
        sqlx::query_scalar("SELECT id FROM tracks WHERE path = 'Band/Album/02.flac'")
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query(protect)
        .bind(album)
        .execute(pool)
        .await
        .unwrap();
    let grace = scan::MISSING_GRACE.as_secs() as i64 + 24 * 3600;
    sqlx::query(
        "UPDATE tracks SET missing_since = now() - make_interval(secs => $2) WHERE id = $1",
    )
    .bind(back)
    .bind(grace as f64)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, attempts, error_class, last_error)
         VALUES ($1, 'fingerprint', 'failed', 1, 'FILE_MISSING', 'gone')",
    )
    .bind(back)
    .execute(pool)
    .await
    .unwrap();
    (album, back, present)
}

/// The returned row survives the re-file: same id and album, present again, its file-missing
/// stage reopened; the present protected sibling is not written.
async fn assert_restored(pool: &PgPool, root: &Path, album: Uuid, back: Uuid, present: Uuid) {
    let stamp = "SELECT updated_at::text FROM tracks WHERE id = $1";
    let present_before: String = sqlx::query_scalar(stamp)
        .bind(present)
        .fetch_one(pool)
        .await
        .unwrap();
    let report = scan::refile_cancellable(&ctx(pool, root), &Default::default())
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(
        report.purged, 0,
        "a present file must not be retired: {report:?}"
    );
    let row: Option<(Uuid, Uuid, bool)> = sqlx::query_as(
        "SELECT id, album_id, missing_since IS NULL FROM tracks WHERE path = 'Band/Album/01.flac'",
    )
    .fetch_optional(pool)
    .await
    .unwrap();
    assert_eq!(row, Some((back, album, true)), "{report:?}");
    assert_eq!(report.restored, 1, "{report:?}");
    let stage: String = sqlx::query_scalar(
        "SELECT status FROM track_stages WHERE track_id = $1 AND stage = 'fingerprint'",
    )
    .bind(back)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(stage, "pending", "file-missing stage reopened");
    let present_after: String = sqlx::query_scalar(stamp)
        .bind(present)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        present_after, present_before,
        "present protected rows are not written"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_restores_returned_track_on_edited_album(pool: PgPool) {
    assert!(require_ffmpeg(), "real audio fixtures require ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (album, back, present) = protected_with_expired_missing(
        &pool,
        root,
        "WITH al AS (
             UPDATE albums SET display_title = 'My Album', display_year = 1995,
                 user_genres = ARRAY['Jazz'], image_source = 'user', image_path = 'my-cover'
             WHERE id = $1 RETURNING artist_id
         )
         UPDATE artists SET display_name = 'My Band' FROM al WHERE artists.id = al.artist_id",
    )
    .await;
    assert_restored(&pool, root, album, back, present).await;
    let edits: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('title', al.display_title, 'year', al.display_year,
             'genres', al.user_genres, 'imageSource', al.image_source,
             'imagePath', al.image_path, 'artist', ar.display_name)
         FROM albums al JOIN artists ar ON ar.id = al.artist_id WHERE al.id = $1",
    )
    .bind(album)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        edits,
        serde_json::json!({
            "title": "My Album", "year": 1995, "genres": ["Jazz"],
            "imageSource": "user", "imagePath": "my-cover", "artist": "My Band"
        })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_restores_returned_track_on_autotag_only_album(pool: PgPool) {
    assert!(require_ffmpeg(), "real audio fixtures require ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (album, back, present) = protected_with_expired_missing(
        &pool,
        root,
        r#"UPDATE albums SET identity_source = 'autotag',
               applied_identity = '{"artistKey":"band","titleKey":"album","year":null}'::jsonb
           WHERE id = $1"#,
    )
    .await;
    assert_restored(&pool, root, album, back, present).await;
    let identity: (String, serde_json::Value) =
        sqlx::query_as("SELECT identity_source, applied_identity FROM albums WHERE id = $1")
            .bind(album)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        identity,
        (
            "autotag".into(),
            serde_json::json!({"artistKey": "band", "titleKey": "album", "year": null})
        )
    );
}

/// Marks the row at `path` missing for longer than the grace period (its file stays on disk)
/// and returns its id.
async fn expire_missing(pool: &PgPool, path: &str) -> Uuid {
    let grace = scan::MISSING_GRACE.as_secs() as f64 + 24.0 * 3600.0;
    sqlx::query_scalar(
        "UPDATE tracks SET missing_since = now() - make_interval(secs => $2)
         WHERE path = $1 RETURNING id",
    )
    .bind(path)
    .bind(grace)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The row at `path` is still `id` and present: a re-file never retires a file it walked.
async fn assert_survives(pool: &PgPool, report: &scan::ScanReport, path: &str, id: Uuid) {
    assert_eq!(
        report.purged, 0,
        "a present file must not be retired: {report:?}"
    );
    let row: Option<(Uuid, bool)> =
        sqlx::query_as("SELECT id, missing_since IS NULL FROM tracks WHERE path = $1")
            .bind(path)
            .fetch_optional(pool)
            .await
            .unwrap();
    assert_eq!(row, Some((id, true)), "{report:?}");
    assert_eq!(report.restored, 1, "{report:?}");
}

/// Present rows outside the album under test, so the mass-missing guard stays off.
fn keep_album(root: &Path) {
    for n in 1..=3 {
        song(
            root,
            &format!("Keep/K/0{n}.flac"),
            "K",
            Some("Keep"),
            "Keep",
        );
    }
}

#[cfg(unix)]
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_restores_returned_track_when_sibling_disc_cannot_be_walked(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    assert!(require_ffmpeg(), "real audio fixtures require ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for n in 1..=4 {
        let disc = if n <= 2 { "CD1" } else { "CD2" };
        song(
            root,
            &format!("Band/Album/{disc}/0{n}.flac"),
            "Album",
            Some("Band"),
            "Band",
        );
    }
    keep_album(root);
    rescan(&pool, root).await;
    let back = expire_missing(&pool, "Band/Album/CD1/01.flac").await;
    let disc = root.join("Band/Album/CD2");
    let original = std::fs::metadata(&disc).unwrap().permissions();
    std::fs::set_permissions(&disc, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&disc).is_ok() {
        std::fs::set_permissions(&disc, original).unwrap();
        eprintln!("SKIPPED: this process can still read chmod 000 (root/capability)");
        return;
    }
    let result = scan::refile_cancellable(&ctx(&pool, root), &Default::default()).await;
    std::fs::set_permissions(&disc, original).unwrap();
    let report = result.unwrap();
    assert!(
        report
            .failed
            .iter()
            .any(|(path, _)| path == "Band/Album/CD2"),
        "{report:?}"
    );
    assert_survives(&pool, &report, "Band/Album/CD1/01.flac", back).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_restores_returned_track_when_a_group_file_is_unreadable(pool: PgPool) {
    assert!(require_ffmpeg(), "real audio fixtures require ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for n in 1..=3 {
        song(
            root,
            &format!("Band/Album/0{n}.flac"),
            "Album",
            Some("Band"),
            "Band",
        );
    }
    keep_album(root);
    rescan(&pool, root).await;
    let back = expire_missing(&pool, "Band/Album/01.flac").await;
    std::fs::write(root.join("Band/Album/02.flac"), b"unreadable tags").unwrap();
    let report = scan::refile_cancellable(&ctx(&pool, root), &Default::default())
        .await
        .unwrap();
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].0, "Band/Album/02.flac");
    assert_survives(&pool, &report, "Band/Album/01.flac", back).await;
}
