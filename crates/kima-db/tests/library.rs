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
        year,
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
    }
}

async fn upsert(pool: &PgPool, t: &ResolvedTrack) -> uuid::Uuid {
    let mut tx = pool.begin().await.unwrap();
    let id = library::upsert_track(&mut tx, t).await.unwrap();
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
