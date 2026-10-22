use std::time::Duration;

use kima_db::PgPool;
use kima_db::library::{self, Guessed, ResolvedTrack};
use time::OffsetDateTime;

fn track(artist: &str, album: &str, year: Option<i32>, title: &str, path: &str) -> ResolvedTrack {
    ResolvedTrack {
        album_artist: artist.to_string(),
        album_artist_key: artist.to_lowercase(),
        track_artist: artist.to_string(),
        album: album.to_string(),
        album_key: album.to_lowercase(),
        album_year: year,
        track_year: year,
        is_compilation: false,
        disc: 1,
        track_no: 1,
        title: title.to_string(),
        duration_secs: 200,
        codec: "flac".to_string(),
        bitrate: None,
        path: path.to_string(),
        size: 1000,
        mtime: OffsetDateTime::now_utc(),
        fingerprint: [7; 32],
        guessed: Guessed::default(),
        ids: Default::default(),
        isrc: None,
        disc_total: None,
        track_total: None,
        artist_mbid_candidate: None,
    }
}

async fn upsert(pool: &PgPool, t: &ResolvedTrack) -> uuid::Uuid {
    let mut tx = pool.begin().await.unwrap();
    let id = library::upsert_track(&mut tx, t).await.unwrap().track;
    tx.commit().await.unwrap();
    id
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn same_artist_case_insensitive_one_row(pool: PgPool) {
    upsert(&pool, &track("radiohead", "OK", None, "a", "/m/1")).await;
    upsert(&pool, &track("Radiohead", "OK", None, "b", "/m/2")).await;
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM artists")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(names, vec!["Radiohead".to_string()]);
    // A worse spelling arriving later does not downgrade the display name.
    upsert(&pool, &track("RADIOHEAD", "OK", None, "c", "/m/3")).await;
    let name: String = sqlx::query_scalar("SELECT name FROM artists")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Radiohead");
    assert_eq!(count(&pool, "albums").await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_identity_includes_year(pool: PgPool) {
    upsert(&pool, &track("A", "Hits", Some(1999), "a", "/m/1")).await;
    upsert(&pool, &track("A", "Hits", Some(2009), "b", "/m/2")).await;
    upsert(&pool, &track("A", "Hits", Some(1999), "c", "/m/3")).await;
    assert_eq!(count(&pool, "albums").await, 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_without_year_merges(pool: PgPool) {
    upsert(&pool, &track("A", "Hits", None, "a", "/m/cd1/1")).await;
    upsert(&pool, &track("A", "Hits", None, "b", "/m/cd2/1")).await;
    assert_eq!(count(&pool, "albums").await, 1);
    assert_eq!(count(&pool, "tracks").await, 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn upsert_by_path_updates_in_place(pool: PgPool) {
    let id = upsert(&pool, &track("A", "X", None, "Old", "/m/1")).await;
    sqlx::query("UPDATE tracks SET missing_since = now()")
        .execute(&pool)
        .await
        .unwrap();
    let id2 = upsert(&pool, &track("A", "X", None, "New", "/m/1")).await;
    assert_eq!(id, id2);
    assert_eq!(count(&pool, "tracks").await, 1);
    let row = library::find_by_path(&pool, "/m/1").await.unwrap().unwrap();
    assert_eq!(row.title, "New");
    assert!(
        row.missing_since.is_none(),
        "re-seen file is no longer missing"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn repoint_only_matches_missing(pool: PgPool) {
    let id = upsert(&pool, &track("A", "X", None, "T", "/m/old")).await;
    let fp = [7u8; 32];
    assert_eq!(
        library::repoint_by_fingerprint(&pool, &fp, "/m/new")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        library::mark_missing(&pool, &["/m/old".to_string()])
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        library::repoint_by_fingerprint(&pool, &fp, "/m/new")
            .await
            .unwrap(),
        Some(id)
    );
    let row = library::find_by_path(&pool, "/m/new")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.id, id);
    assert!(row.missing_since.is_none());
    assert!(
        library::find_by_path(&pool, "/m/old")
            .await
            .unwrap()
            .is_none()
    );
    // Present again, so a second repoint finds nothing.
    assert_eq!(
        library::repoint_by_fingerprint(&pool, &fp, "/m/x")
            .await
            .unwrap(),
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn purge_respects_grace(pool: PgPool) {
    upsert(&pool, &track("A", "X", None, "six", "/m/6")).await;
    upsert(&pool, &track("A", "X", None, "eight", "/m/8")).await;
    upsert(&pool, &track("A", "X", None, "here", "/m/ok")).await;
    sqlx::query("UPDATE tracks SET missing_since = now() - interval '6 days' WHERE path = '/m/6'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() - interval '8 days' WHERE path = '/m/8'")
        .execute(&pool)
        .await
        .unwrap();
    let week = Duration::from_secs(7 * 86400);
    assert_eq!(library::purge_missing(&pool, week).await.unwrap(), 1);
    assert!(
        library::find_by_path(&pool, "/m/6")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        library::find_by_path(&pool, "/m/8")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        library::find_by_path(&pool, "/m/ok")
            .await
            .unwrap()
            .is_some()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prune_empty_removes_orphans(pool: PgPool) {
    upsert(&pool, &track("A", "X", None, "t", "/m/1")).await;
    upsert(&pool, &track("B", "Y", None, "t", "/m/2")).await;
    sqlx::query("DELETE FROM tracks WHERE path = '/m/1'")
        .execute(&pool)
        .await
        .unwrap();
    library::prune_empty(&pool).await.unwrap();
    assert_eq!(count(&pool, "albums").await, 1);
    assert_eq!(count(&pool, "artists").await, 1);
}

async fn hits(pool: &PgPool, q: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM tracks WHERE search @@ plainto_tsquery('simple', $1))
              + (SELECT count(*) FROM albums WHERE search @@ plainto_tsquery('simple', $1))
              + (SELECT count(*) FROM artists WHERE search @@ plainto_tsquery('simple', $1))",
    )
    .bind(q)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_vector_updates(pool: PgPool) {
    let id = upsert(
        &pool,
        &track("Portishead", "Dummy", None, "Glory Box", "/m/1"),
    )
    .await;
    assert_eq!(hits(&pool, "glory").await, 1);
    assert_eq!(hits(&pool, "dummy").await, 2); // album, plus the track via its album title
    assert_eq!(hits(&pool, "portishead").await, 3);
    sqlx::query("UPDATE tracks SET title = 'Roads' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(hits(&pool, "glory").await, 0);
    assert_eq!(hits(&pool, "roads").await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn repoint_skips_when_path_taken(pool: PgPool) {
    upsert(&pool, &track("A", "X", None, "T", "/m/old")).await;
    let mut other = track("A", "X", None, "U", "/m/taken");
    other.fingerprint = [9; 32];
    upsert(&pool, &other).await;
    library::mark_missing(&pool, &["/m/old".to_string()])
        .await
        .unwrap();
    assert_eq!(
        library::repoint_by_fingerprint(&pool, &[7; 32], "/m/taken")
            .await
            .unwrap(),
        None
    );
    let row = library::find_by_path(&pool, "/m/old")
        .await
        .unwrap()
        .unwrap();
    assert!(row.missing_since.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn repoint_moves_one_of_several_same_fingerprint(pool: PgPool) {
    let a = upsert(&pool, &track("A", "X", None, "T1", "/m/a")).await;
    let b = upsert(&pool, &track("A", "X", None, "T2", "/m/b")).await;
    sqlx::query("UPDATE tracks SET missing_since = now() - interval '2 days' WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() - interval '1 day' WHERE id = $1")
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    let fp = [7u8; 32];
    // Oldest-missing first.
    assert_eq!(
        library::repoint_by_fingerprint(&pool, &fp, "/m/n1")
            .await
            .unwrap(),
        Some(a)
    );
    assert_eq!(
        library::repoint_by_fingerprint(&pool, &fp, "/m/n2")
            .await
            .unwrap(),
        Some(b)
    );
    assert_eq!(
        library::repoint_by_fingerprint(&pool, &fp, "/m/n3")
            .await
            .unwrap(),
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rename_refreshes_dependent_search_vectors(pool: PgPool) {
    upsert(
        &pool,
        &track("Portishead", "Dummy", None, "Glory Box", "/m/1"),
    )
    .await;
    sqlx::query("UPDATE artists SET name = 'Beth Gibbons'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(hits(&pool, "portishead").await, 1); // only the track, which keeps its own artist_name
    assert_eq!(hits(&pool, "gibbons").await, 2); // artist + album (track carries its own artist_name)
    sqlx::query("UPDATE albums SET title = 'Third'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(hits(&pool, "dummy").await, 0);
    assert_eq!(hits(&pool, "third").await, 2); // album + track
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn scan_index_filters_to_one_folder(pool: PgPool) {
    upsert(&pool, &track("A", "B", Some(1999), "1", "A/B/01.flac")).await;
    upsert(&pool, &track("A", "B", Some(1999), "2", "A/B/CD2/01.flac")).await;
    upsert(&pool, &track("A", "C", None, "3", "A/BB/01.flac")).await;
    upsert(&pool, &track("A", "D", None, "4", "root.flac")).await;
    let paths = |rows: Vec<library::IndexedTrack>| {
        let mut p: Vec<String> = rows.into_iter().map(|r| r.path).collect();
        p.sort();
        p
    };
    assert_eq!(
        paths(library::scan_index(&pool, Some("A/B/")).await.unwrap()),
        vec!["A/B/01.flac"]
    );
    assert_eq!(
        paths(library::scan_index(&pool, Some("")).await.unwrap()),
        vec!["root.flac"]
    );
    assert_eq!(library::scan_index(&pool, None).await.unwrap().len(), 4);
    let row = &library::scan_index(&pool, Some("A/B/")).await.unwrap()[0];
    assert_eq!(
        (
            row.artist_key.as_str(),
            row.album_key.as_str(),
            row.album_year
        ),
        ("a", "b", Some(1999))
    );
    assert!(!row.missing && !row.year_conflict);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn clear_missing_restores_rows(pool: PgPool) {
    upsert(&pool, &track("A", "B", None, "1", "/m/1")).await;
    let paths = vec!["/m/1".to_string()];
    assert_eq!(library::mark_missing(&pool, &paths).await.unwrap(), 1);
    assert!(library::scan_index(&pool, None).await.unwrap()[0].missing);
    assert_eq!(library::clear_missing(&pool, &paths).await.unwrap(), 1);
    assert_eq!(library::clear_missing(&pool, &paths).await.unwrap(), 0);
    assert!(!library::scan_index(&pool, None).await.unwrap()[0].missing);
}

// ---- M3a Task 10: upsert result, genres, lyrics ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn upsert_reports_content_change(pool: PgPool) {
    let mut t = track("A", "B", None, "x", "/m/1");
    let mut tx = pool.begin().await.unwrap();
    let first = library::upsert_track(&mut tx, &t).await.unwrap();
    assert!(!first.content_changed, "a new row has nothing to compare");
    let same = library::upsert_track(&mut tx, &t).await.unwrap();
    assert_eq!(
        (same.track, same.album, same.artist),
        (first.track, first.album, first.artist)
    );
    assert!(!same.content_changed);
    t.fingerprint = [8; 32];
    assert!(
        library::upsert_track(&mut tx, &t)
            .await
            .unwrap()
            .content_changed
    );
    tx.commit().await.unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_genres_skip_markers_and_repeats(pool: PgPool) {
    use kima_db::genres::{self, GenreSource};
    let id = upsert(&pool, &track("A", "B", None, "x", "/m/1")).await;
    let names: Vec<String> = ["Rock", "_internal", "rock", " Hip  Hop ", ""]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut conn = pool.acquire().await.unwrap();
    genres::replace_track_genres(&mut conn, id, GenreSource::MusicBrainz, &names)
        .await
        .unwrap();
    let rows: Vec<(String, String, i16)> = sqlx::query_as(
        "SELECT g.name, g.key, tg.rank FROM track_genres tg JOIN genres g ON g.id = tg.genre_id
         WHERE tg.track_id = $1 ORDER BY tg.rank",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("Rock".into(), "rock".into(), 0),
            ("Hip  Hop".into(), "hip hop".into(), 1)
        ]
    );
    // Another source of the same track is independent; clearing one leaves the other.
    genres::replace_track_genres(&mut conn, id, GenreSource::File, &["Jazz".to_string()])
        .await
        .unwrap();
    genres::replace_track_genres(&mut conn, id, GenreSource::MusicBrainz, &[])
        .await
        .unwrap();
    assert_eq!(count(&pool, "track_genres").await, 1);
    assert_eq!(genres::prune_unused(&pool).await.unwrap(), 2);
    assert_eq!(count(&pool, "genres").await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_embedded_keeps_other_sources(pool: PgPool) {
    use kima_db::lyrics;
    let id = upsert(&pool, &track("A", "B", None, "x", "/m/1")).await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("INSERT INTO track_lyrics (track_id, source) VALUES ($1, 'none')")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    lyrics::delete_embedded(&mut conn, id).await.unwrap();
    assert_eq!(count(&pool, "track_lyrics").await, 1);
    lyrics::upsert_embedded(&mut conn, id, Some("words"), None)
        .await
        .unwrap();
    let src: String = sqlx::query_scalar("SELECT source FROM track_lyrics")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(src, "embedded");
    lyrics::delete_embedded(&mut conn, id).await.unwrap();
    assert_eq!(count(&pool, "track_lyrics").await, 0);
}

/// Carry (Task 17 M2): genre rows are inserted in key order, so two transactions naming the
/// same new genres in opposite orders queue up instead of deadlocking.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn genre_inserts_in_opposite_orders_do_not_deadlock(pool: PgPool) {
    use kima_db::genres::{self, GenreSource};
    let t1 = upsert(&pool, &track("A", "B", None, "x", "/m/1")).await;
    let t2 = upsert(&pool, &track("A", "B", None, "y", "/m/2")).await;
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    let mut a = pool.begin().await.unwrap();
    genres::replace_track_genres(&mut a, t1, GenreSource::File, &names(&["alpha"]))
        .await
        .unwrap();
    let other = {
        let pool = pool.clone();
        let n = names(&["beta", "alpha"]);
        tokio::spawn(async move {
            let mut b = pool.begin().await.unwrap();
            genres::replace_track_genres(&mut b, t2, GenreSource::File, &n).await?;
            b.commit().await
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    genres::replace_track_genres(&mut a, t1, GenreSource::MusicBrainz, &names(&["beta"]))
        .await
        .expect("no deadlock");
    a.commit().await.unwrap();
    other.await.unwrap().expect("no deadlock");
    assert_eq!(count(&pool, "genres").await, 2);
    assert_eq!(count(&pool, "track_genres").await, 4);
}

/// Carry (Task 10/7): pruning cannot delete a genre a concurrent transaction just took: the
/// prune waits for it, then sees its reference.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prune_waits_for_a_concurrent_genre_reference(pool: PgPool) {
    use kima_db::genres::{self, GenreSource};
    let t1 = upsert(&pool, &track("A", "B", None, "x", "/m/1")).await;
    // "rock" exists and nothing refers to it.
    let mut conn = pool.acquire().await.unwrap();
    genres::replace_track_genres(&mut conn, t1, GenreSource::File, &["Rock".to_string()])
        .await
        .unwrap();
    genres::replace_track_genres(&mut conn, t1, GenreSource::File, &[])
        .await
        .unwrap();
    drop(conn);

    let mut a = pool.begin().await.unwrap();
    genres::replace_track_genres(&mut a, t1, GenreSource::File, &["rock".to_string()])
        .await
        .unwrap();
    let prune = {
        let pool = pool.clone();
        tokio::spawn(async move { genres::prune_unused(&pool).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.commit().await.unwrap();
    assert_eq!(prune.await.unwrap().unwrap(), 0);
    assert_eq!(count(&pool, "track_genres").await, 1);
    assert_eq!(count(&pool, "genres").await, 1);
}
