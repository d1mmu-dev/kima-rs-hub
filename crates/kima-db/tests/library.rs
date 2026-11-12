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

/// Scanner and playlist must take the same UUID lock order even if heap order differs.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_scan_and_playlist_lock_two_tracks_without_deadlock(pool: PgPool) {
    use tokio::time::{Instant, sleep, timeout};

    let b = upsert(&pool, &track("A", "X", None, "B", "/m/b")).await;
    let a = upsert(&pool, &track("A", "X", None, "A", "/m/a")).await;
    let first = uuid::Uuid::from_u128(1);
    let second = uuid::Uuid::from_u128(2);
    // Insert B before A, then give B the larger ID: a heap scan updates B first.
    // Disable index and bitmap scans only in this scanner transaction.
    sqlx::query("UPDATE tracks SET id = $1 WHERE id = $2")
        .bind(second)
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET id = $1 WHERE id = $2")
        .bind(first)
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    let mut playlist = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tracks WHERE id = $1 FOR SHARE")
        .bind(first)
        .fetch_one(&mut *playlist)
        .await
        .unwrap();
    let scan_pool = pool.clone();
    let scanner = tokio::spawn(async move {
        let mut tx = scan_pool.begin().await.unwrap();
        sqlx::query("SET LOCAL enable_indexscan = off")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("SET LOCAL enable_bitmapscan = off")
            .execute(&mut *tx)
            .await
            .unwrap();
        let n = library::mark_missing_in(&mut tx, &["/m/a".into(), "/m/b".into()]).await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>(n)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname = current_database()
             AND wait_event_type = 'Lock' AND query LIKE '%missing_since = now()%'
             AND pid <> pg_backend_pid())",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if waiting {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "scanner never waited for playlist track lock"
        );
        sleep(Duration::from_millis(10)).await;
    }
    let locked = timeout(Duration::from_secs(5), sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM tracks WHERE id = ANY($1) AND missing_since IS NULL ORDER BY id FOR SHARE"
    ).bind(&[first, second][..]).fetch_all(&mut *playlist)).await.unwrap();
    assert_eq!(locked.unwrap(), vec![first, second]);
    playlist.commit().await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(5), scanner)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        2
    );
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
    assert_eq!(
        library::retire_missing(&pool, week, None)
            .await
            .unwrap()
            .tracks,
        1
    );
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
    let a: uuid::Uuid = sqlx::query_scalar("SELECT id FROM artists WHERE name = 'A'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let pruned = library::prune_empty(&pool).await.unwrap();
    assert_eq!(pruned.albums.len(), 1);
    assert_eq!(pruned.artists, vec![a]);
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

// ---- M-SYNC Task 5: removal log, mass-missing guard, history hand-off (D-040, D-161) ----

const WEEK: Duration = Duration::from_secs(7 * 86400);

async fn make_missing(pool: &PgPool, id: uuid::Uuid, days: i32) {
    sqlx::query(
        "UPDATE tracks SET missing_since = now() - make_interval(days => $2) WHERE id = $1",
    )
    .bind(id)
    .bind(days)
    .execute(pool)
    .await
    .unwrap();
}

async fn user(pool: &PgPool, name: &str) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, $2, 'x')")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn playlist(pool: &PgPool, owner: uuid::Uuid, tracks: &[uuid::Uuid]) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO playlists (id, user_id, name) VALUES ($1, $2, 'P')")
        .bind(id)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    for (i, t) in tracks.iter().enumerate() {
        sqlx::query(
            "INSERT INTO playlist_items (id, playlist_id, track_id, sort) VALUES ($1, $2, $3, $4)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(id)
        .bind(t)
        .bind(i as i32)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

async fn star(pool: &PgPool, u: uuid::Uuid, t: uuid::Uuid) {
    sqlx::query("INSERT INTO stars (id, user_id, track_id) VALUES ($1, $2, $3)")
        .bind(uuid::Uuid::now_v7())
        .bind(u)
        .bind(t)
        .execute(pool)
        .await
        .unwrap();
}

async fn rate(pool: &PgPool, u: uuid::Uuid, t: uuid::Uuid, r: i16) {
    sqlx::query("INSERT INTO ratings (id, user_id, track_id, rating) VALUES ($1, $2, $3, $4)")
        .bind(uuid::Uuid::now_v7())
        .bind(u)
        .bind(t)
        .bind(r)
        .execute(pool)
        .await
        .unwrap();
}

async fn bookmark(pool: &PgPool, u: uuid::Uuid, t: uuid::Uuid, ms: i64) {
    sqlx::query(
        "INSERT INTO subsonic_bookmarks (user_id, track_id, position_ms) VALUES ($1, $2, $3)",
    )
    .bind(u)
    .bind(t)
    .bind(ms)
    .execute(pool)
    .await
    .unwrap();
}

async fn play(pool: &PgPool, u: uuid::Uuid, t: uuid::Uuid, heard: bool) {
    sqlx::query("INSERT INTO plays (id, user_id, track_id, heard) VALUES ($1, $2, $3, $4)")
        .bind(uuid::Uuid::now_v7())
        .bind(u)
        .bind(t)
        .bind(heard)
        .execute(pool)
        .await
        .unwrap();
}

async fn count_for(pool: &PgPool, table: &str, t: uuid::Uuid) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE track_id = $1"))
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn log_rows(pool: &PgPool) -> Vec<(String, String, String, String, String)> {
    sqlx::query_as(
        "SELECT file_path, title, artist_name, album_title, reason FROM removal_log ORDER BY file_path",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn purge_writes_removal_log_row(pool: PgPool) {
    let old = upsert(&pool, &track("A", "X", None, "Eight", "/m/8")).await;
    let young = upsert(&pool, &track("A", "X", None, "Six", "/m/6")).await;
    upsert(&pool, &track("A", "X", None, "Here", "/m/ok")).await;
    make_missing(&pool, old, 8).await;
    make_missing(&pool, young, 6).await;
    let r = library::retire_missing(&pool, WEEK, None).await.unwrap();
    assert_eq!(r.tracks, 1);
    assert_eq!(r.albums.len(), 1);
    assert_eq!(
        log_rows(&pool).await,
        vec![(
            "/m/8".to_string(),
            "Eight".to_string(),
            "A".to_string(),
            "X".to_string(),
            "file-missing-grace".to_string()
        )]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mass_missing_stands_down(pool: PgPool) {
    // 20 tracks, 3 missing past the grace: small, purged.
    let mut ids = Vec::new();
    for i in 0..20 {
        ids.push(
            upsert(
                &pool,
                &track("A", "X", None, &format!("t{i}"), &format!("/m/{i}")),
            )
            .await,
        );
    }
    for id in &ids[..3] {
        make_missing(&pool, *id, 8).await;
    }
    let r = library::retire_missing(&pool, WEEK, None).await.unwrap();
    assert_eq!((r.tracks, r.stood_down), (3, false));
    assert_eq!(count(&pool, "tracks").await, 17);

    // 12 of 20 missing (>= 10 and > 10 %): nothing goes.
    for id in &ids[3..15] {
        make_missing(&pool, *id, 8).await;
    }
    let r = library::retire_missing(&pool, WEEK, None).await.unwrap();
    assert_eq!((r.tracks, r.stood_down), (0, true));
    assert_eq!(count(&pool, "tracks").await, 17);
    assert!(log_rows(&pool).await.len() == 3);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mass_missing_floor_lets_a_few_go_in_a_big_library(pool: PgPool) {
    let mut ids = Vec::new();
    for i in 0..200 {
        ids.push(
            upsert(
                &pool,
                &track("A", "X", None, &format!("t{i}"), &format!("/m/{i}")),
            )
            .await,
        );
    }
    for id in &ids[..9] {
        make_missing(&pool, *id, 8).await;
    }
    let r = library::retire_missing(&pool, WEEK, None).await.unwrap();
    assert_eq!((r.tracks, r.stood_down), (9, false));
    assert_eq!(count(&pool, "tracks").await, 191);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn guard_counts_young_flags_too(pool: PgPool) {
    let mut ids = Vec::new();
    for i in 0..20 {
        ids.push(
            upsert(
                &pool,
                &track("A", "X", None, &format!("t{i}"), &format!("/m/{i}")),
            )
            .await,
        );
    }
    make_missing(&pool, ids[0], 8).await;
    for id in &ids[1..11] {
        make_missing(&pool, *id, 1).await;
    }
    let r = library::retire_missing(&pool, WEEK, None).await.unwrap();
    assert_eq!((r.tracks, r.stood_down), (0, true));
}

struct Upgrade {
    u: uuid::Uuid,
    mp3: uuid::Uuid,
    flac: uuid::Uuid,
}

async fn upgrade_fixture(pool: &PgPool) -> Upgrade {
    let mut a = track("A", "X", None, "Song", "/m/01 Song.mp3");
    a.codec = "mp3".into();
    let mp3 = upsert(pool, &a).await;
    let b = track("A", "X", None, "song", "/m/01 song.flac");
    let flac = upsert(pool, &b).await;
    for i in 0..9 {
        upsert(
            pool,
            &track("A", "X", None, &format!("o{i}"), &format!("/m/o{i}")),
        )
        .await;
    }
    // A different slot, so the album keeps tracks and the guard has a big enough library.
    let mut t = track("B", "Y", None, "z", "/m/z");
    t.track_no = 5;
    upsert(pool, &t).await;
    let u = user(pool, "ana").await;
    Upgrade { u, mp3, flac }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replaced_file_hands_history_to_copy(pool: PgPool) {
    let Upgrade { u, mp3, flac } = upgrade_fixture(&pool).await;
    play(&pool, u, mp3, true).await;
    star(&pool, u, mp3).await;
    rate(&pool, u, mp3, 4).await;
    bookmark(&pool, u, mp3, 1234).await;
    let p = playlist(&pool, u, &[mp3]).await;
    let ben = user(&pool, "ben").await;
    sqlx::query("INSERT INTO play_audit_ignore (user_id, track_id, ignored_at) VALUES ($1, $3, '2020-01-01'), ($2, $3, '2021-01-01'), ($2, $4, '2022-01-01')")
        .bind(u).bind(ben).bind(mp3).bind(flac).execute(&pool).await.unwrap();
    make_missing(&pool, mp3, 8).await;

    let r = library::retire_missing(&pool, WEEK, None).await.unwrap();
    let ignores: Vec<(uuid::Uuid, uuid::Uuid, OffsetDateTime)> = sqlx::query_as(
        "SELECT user_id, track_id, ignored_at FROM play_audit_ignore ORDER BY ignored_at",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ignores.len(), 2);
    assert_eq!(
        (ignores[0].0, ignores[0].1, ignores[0].2.year()),
        (u, flac, 2020)
    );
    assert_eq!(
        (ignores[1].0, ignores[1].1, ignores[1].2.year()),
        (ben, flac, 2022)
    );
    assert_eq!(r.tracks, 1);
    assert_eq!(r.playlists, vec![(u, p)]);

    let heard: Vec<bool> = sqlx::query_scalar("SELECT heard FROM plays WHERE track_id = $1")
        .bind(flac)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(heard, vec![true]);
    assert_eq!(count_for(&pool, "stars", flac).await, 1);
    assert_eq!(count_for(&pool, "ratings", flac).await, 1);
    assert_eq!(count_for(&pool, "subsonic_bookmarks", flac).await, 1);
    let place: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT track_id FROM playlist_items WHERE playlist_id = $1")
            .bind(p)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(place, vec![flac]);
    let log = log_rows(&pool).await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].4, "replaced-by-another-file");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_destination_ignore_does_not_abort_retirement(pool: PgPool) {
    use tokio::time::{Instant, sleep, timeout};

    let Upgrade { u, mp3, flac } = upgrade_fixture(&pool).await;
    sqlx::query("INSERT INTO play_audit_ignore VALUES ($1, $2, '2020-01-01')")
        .bind(u)
        .bind(mp3)
        .execute(&pool)
        .await
        .unwrap();
    make_missing(&pool, mp3, 8).await;
    sqlx::raw_sql("CREATE FUNCTION gate_ignore() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.ignored_at = '2020-01-01'::timestamptz THEN PERFORM pg_advisory_xact_lock(784399); END IF; RETURN NEW; END $$; CREATE TRIGGER gate_ignore BEFORE INSERT OR UPDATE ON play_audit_ignore FOR EACH ROW EXECUTE FUNCTION gate_ignore()")
        .execute(&pool).await.unwrap();
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(784399)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let mut writer = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO play_audit_ignore VALUES ($1, $2, '2022-01-01')")
        .bind(u)
        .bind(flac)
        .execute(&mut *writer)
        .await
        .unwrap();
    let retire_pool = pool.clone();
    let retirement =
        tokio::spawn(async move { library::retire_missing(&retire_pool, WEEK, None).await });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event = 'advisory' AND query LIKE '%play_audit_ignore%' AND pid <> pg_backend_pid())")
            .fetch_one(&pool).await.unwrap();
        if waiting {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "retirement never reached ignore trigger"
        );
        sleep(Duration::from_millis(10)).await;
    }
    writer.commit().await.unwrap();
    gate.commit().await.unwrap();
    let result = timeout(Duration::from_secs(10), retirement)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_ok(),
        "retirement failed after concurrent destination insert: {result:?}"
    );
    assert_eq!(result.unwrap().tracks, 1);
    let rows: Vec<(uuid::Uuid, OffsetDateTime)> =
        sqlx::query_as("SELECT track_id, ignored_at FROM play_audit_ignore WHERE user_id = $1")
            .bind(u)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0, rows[0].1.year()), (flac, 2022));
    let old_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE id = $1)")
        .bind(mp3)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!old_exists);
    assert_eq!(log_rows(&pool).await.len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_retirement_rolls_back_ignore_handover(pool: PgPool) {
    let Upgrade { u, mp3, flac } = upgrade_fixture(&pool).await;
    sqlx::query("INSERT INTO play_audit_ignore VALUES ($1, $2, '2020-01-01')")
        .bind(u)
        .bind(mp3)
        .execute(&pool)
        .await
        .unwrap();
    play(&pool, u, mp3, true).await;
    make_missing(&pool, mp3, 8).await;
    sqlx::raw_sql("CREATE FUNCTION fail_removal() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected failure'; END $$; CREATE TRIGGER fail_removal BEFORE INSERT ON removal_log FOR EACH ROW EXECUTE FUNCTION fail_removal()")
        .execute(&pool).await.unwrap();
    assert!(library::retire_missing(&pool, WEEK, None).await.is_err());
    let rows: Vec<(uuid::Uuid, OffsetDateTime)> =
        sqlx::query_as("SELECT track_id, ignored_at FROM play_audit_ignore WHERE user_id = $1")
            .bind(u)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0, rows[0].1.year()), (mp3, 2020));
    assert_eq!(count_for(&pool, "plays", mp3).await, 1);
    assert_eq!(count_for(&pool, "plays", flac).await, 0);
    assert!(log_rows(&pool).await.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn history_on_the_copy_wins(pool: PgPool) {
    let Upgrade { u, mp3, flac } = upgrade_fixture(&pool).await;
    star(&pool, u, mp3).await;
    star(&pool, u, flac).await;
    rate(&pool, u, mp3, 2).await;
    rate(&pool, u, flac, 5).await;
    bookmark(&pool, u, mp3, 1).await;
    bookmark(&pool, u, flac, 2).await;
    let p = playlist(&pool, u, &[mp3, flac]).await;
    make_missing(&pool, mp3, 8).await;

    library::retire_missing(&pool, WEEK, None).await.unwrap();
    assert_eq!(count_for(&pool, "stars", flac).await, 1);
    let rating: i16 = sqlx::query_scalar("SELECT rating FROM ratings WHERE track_id = $1")
        .bind(flac)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rating, 5);
    let ms: i64 =
        sqlx::query_scalar("SELECT position_ms FROM subsonic_bookmarks WHERE track_id = $1")
            .bind(flac)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ms, 2);
    // The playlist held both: it keeps the copy once, in its own place, and is dense again.
    let items: Vec<(uuid::Uuid, i32)> = sqlx::query_as(
        "SELECT track_id, sort FROM playlist_items WHERE playlist_id = $1 ORDER BY sort",
    )
    .bind(p)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(items, vec![(flac, 0)]);
    assert_eq!(count(&pool, "stars").await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_copy_no_handoff(pool: PgPool) {
    let Upgrade { u, mp3, flac } = upgrade_fixture(&pool).await;
    // The "copy" has another title: not the same song.
    sqlx::query("UPDATE tracks SET title = 'Other' WHERE id = $1")
        .bind(flac)
        .execute(&pool)
        .await
        .unwrap();
    star(&pool, u, mp3).await;
    make_missing(&pool, mp3, 8).await;
    library::retire_missing(&pool, WEEK, None).await.unwrap();
    assert_eq!(count_for(&pool, "stars", flac).await, 0);
    assert_eq!(count(&pool, "stars").await, 0);
    assert_eq!(log_rows(&pool).await[0].4, "file-missing-grace");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_missing_copy_is_no_copy(pool: PgPool) {
    let Upgrade { u, mp3, flac } = upgrade_fixture(&pool).await;
    star(&pool, u, mp3).await;
    make_missing(&pool, mp3, 8).await;
    make_missing(&pool, flac, 1).await;
    library::retire_missing(&pool, WEEK, None).await.unwrap();
    assert_eq!(count(&pool, "stars").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn purged_track_reports_playlists_holding_it(pool: PgPool) {
    let Upgrade { u, mp3, .. } = upgrade_fixture(&pool).await;
    let other = user(&pool, "bo").await;
    let p1 = playlist(&pool, u, &[mp3]).await;
    let p2 = playlist(&pool, other, &[mp3]).await;
    playlist(&pool, other, &[]).await;
    make_missing(&pool, mp3, 8).await;
    let mut r = library::retire_missing(&pool, WEEK, None).await.unwrap();
    r.playlists.sort();
    let mut want = vec![(u, p1), (other, p2)];
    want.sort();
    assert_eq!(r.playlists, want);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn purge_nulls_playback_state_and_lists_transcode_artifacts(pool: PgPool) {
    let Upgrade { u, mp3, .. } = upgrade_fixture(&pool).await;
    sqlx::query(
        "INSERT INTO playback_state (user_id, playback_type, track_id) VALUES ($1, 'track', $2)",
    )
    .bind(u)
    .bind(mp3.to_string())
    .execute(&pool)
    .await
    .unwrap();
    let dir = std::env::temp_dir().join(format!("kima-removal-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let mine = dir.join(format!("{mp3}-high-100-5.mp3"));
    let other = dir.join(format!("{}-high-100-5.mp3", uuid::Uuid::now_v7()));
    std::fs::write(&mine, b"x").unwrap();
    std::fs::write(&other, b"x").unwrap();
    make_missing(&pool, mp3, 8).await;

    let r = library::retire_missing(&pool, WEEK, Some(&dir))
        .await
        .unwrap();
    assert_eq!(r.artifacts, vec![mine.clone()]);
    let state: Option<String> =
        sqlx::query_scalar("SELECT track_id FROM playback_state WHERE user_id = $1")
            .bind(u)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, None);
    let art: serde_json::Value = sqlx::query_scalar("SELECT artifacts FROM removal_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        art,
        serde_json::json!([{"table": "transcodes", "path": mine.to_string_lossy()}])
    );
    // The rows' job is the database; unlinking after the commit is the caller's.
    assert!(mine.exists() && other.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn removal_log_lists_newest_first_with_paging(pool: PgPool) {
    for (i, name) in ["a", "b", "c"].into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO removal_log (id, file_path, title, artist_name, album_title, removed_at)
             VALUES ($1, $2, 't', 'a', 'b', now() - make_interval(mins => $3))",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(name)
        .bind(i as i32)
        .execute(&pool)
        .await
        .unwrap();
    }
    let all = kima_db::removal_log::list(&pool, 100, 0).await.unwrap();
    let paths: Vec<&str> = all.iter().map(|r| r.file_path.as_str()).collect();
    assert_eq!(paths, ["a", "b", "c"]);
    assert_eq!(kima_db::removal_log::count(&pool).await.unwrap(), 3);
    let page = kima_db::removal_log::list(&pool, 1, 1).await.unwrap();
    assert_eq!(page[0].file_path, "b");
}

// M-SYNC Task 11 (upstream `cb21c307`): an album's release-group id may come from AcoustID, the
// weakest source (`acoustid` < `tag` < `autotag` < `user`).
mod acoustid_group {
    use super::*;
    use kima_db::identity;
    use uuid::Uuid;

    const A: Uuid = uuid::uuid!("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    const B: Uuid = uuid::uuid!("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");

    async fn tagged(pool: &PgPool, rg: Option<Uuid>) -> (Uuid, Uuid) {
        let mut t = track("Act", "Rec", Some(2000), "One", "a/1.flac");
        t.ids.release_group = rg;
        let id = upsert(pool, &t).await;
        let album = sqlx::query_scalar("SELECT album_id FROM tracks WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        (id, album)
    }

    async fn refresh(pool: &PgPool, album: Uuid) {
        let mut conn = pool.acquire().await.unwrap();
        library::refresh_album_mbids(&mut conn, album)
            .await
            .unwrap();
    }

    async fn group(pool: &PgPool, album: Uuid) -> (Option<Uuid>, Option<String>) {
        sqlx::query_as(
            "SELECT release_group_mbid, release_group_mbid_source FROM albums WHERE id = $1",
        )
        .bind(album)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn set(pool: &PgPool, album: Uuid, rg: Uuid) -> bool {
        let mut conn = pool.acquire().await.unwrap();
        identity::set_acoustid_group(&mut conn, album, rg)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn acoustid_writes_an_album_with_no_id(pool: PgPool) {
        let (_, album) = tagged(&pool, None).await;
        assert!(set(&pool, album, A).await);
        assert_eq!(
            group(&pool, album).await,
            (Some(A), Some("acoustid".into()))
        );
        // A later vote may change it.
        assert!(set(&pool, album, B).await);
        assert_eq!(
            group(&pool, album).await,
            (Some(B), Some("acoustid".into()))
        );
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn file_tag_replaces_acoustid_id(pool: PgPool) {
        let (track_id, album) = tagged(&pool, None).await;
        assert!(set(&pool, album, A).await);
        sqlx::query("UPDATE tracks SET tag_release_group_mbid = $2 WHERE id = $1")
            .bind(track_id)
            .bind(B)
            .execute(&pool)
            .await
            .unwrap();
        refresh(&pool, album).await;
        assert_eq!(group(&pool, album).await, (Some(B), Some("tag".into())));
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn file_tag_confirming_acoustid_id_takes_source_tag(pool: PgPool) {
        let (track_id, album) = tagged(&pool, None).await;
        assert!(set(&pool, album, A).await);
        sqlx::query("UPDATE tracks SET tag_release_group_mbid = $2 WHERE id = $1")
            .bind(track_id)
            .bind(A)
            .execute(&pool)
            .await
            .unwrap();
        refresh(&pool, album).await;
        assert_eq!(group(&pool, album).await, (Some(A), Some("tag".into())));
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn tag_and_user_ids_are_never_replaced_by_acoustid(pool: PgPool) {
        let (_, album) = tagged(&pool, Some(A)).await;
        refresh(&pool, album).await;
        assert_eq!(group(&pool, album).await, (Some(A), Some("tag".into())));
        assert!(!set(&pool, album, B).await);
        assert_eq!(group(&pool, album).await, (Some(A), Some("tag".into())));
        for source in ["autotag", "user"] {
            sqlx::query("UPDATE albums SET release_group_mbid_source = $2 WHERE id = $1")
                .bind(album)
                .bind(source)
                .execute(&pool)
                .await
                .unwrap();
            assert!(!set(&pool, album, B).await, "{source}");
            assert_eq!(group(&pool, album).await, (Some(A), Some(source.into())));
        }
    }

    #[sqlx::test(migrator = "kima_db::MIGRATOR")]
    async fn no_tags_keeps_acoustid_id(pool: PgPool) {
        let (_, album) = tagged(&pool, None).await;
        assert!(set(&pool, album, A).await);
        refresh(&pool, album).await;
        assert_eq!(
            group(&pool, album).await,
            (Some(A), Some("acoustid".into()))
        );
        // Tag ids that were there and are gone: a *tag* id goes, an AcoustID one stays.
        let (_, tagged_album) = tagged_other(&pool).await;
        refresh(&pool, tagged_album).await;
        assert_eq!(group(&pool, tagged_album).await.1.as_deref(), Some("tag"));
        sqlx::query("UPDATE tracks SET tag_release_group_mbid = NULL WHERE album_id = $1")
            .bind(tagged_album)
            .execute(&pool)
            .await
            .unwrap();
        refresh(&pool, tagged_album).await;
        assert_eq!(group(&pool, tagged_album).await, (None, None));
    }

    async fn tagged_other(pool: &PgPool) -> (Uuid, Uuid) {
        let mut t = track("Other", "Disc", Some(1999), "Two", "b/1.flac");
        t.ids.release_group = Some(B);
        let id = upsert(pool, &t).await;
        let album = sqlx::query_scalar("SELECT album_id FROM tracks WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        (id, album)
    }
}
