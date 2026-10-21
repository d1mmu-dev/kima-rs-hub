use kima_db::PgPool;
use sqlx::Row;
use uuid::Uuid;

const VA_UPDATE: &str = "UPDATE artists SET mbid = '89ad4ac3-39f7-470e-963a-56509c546377', \
     enrichment_status = 'completed' WHERE name_key = 'various artists'";

async fn artist(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, lower($2))")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn album(pool: &PgPool, artist_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $3)")
        .bind(id)
        .bind(artist_id)
        .bind(id.to_string())
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn track(pool: &PgPool, album_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint) \
         VALUES ($1, $2, 'a', 't', 'flac', $3, 1, now(), '\\x00')",
    )
    .bind(id)
    .bind(album_id)
    .bind(id.to_string())
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn genre(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO genres (id, name, key) VALUES ($1, $2, lower($2))")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn va_artist_gets_mbid_and_completed(pool: PgPool) {
    artist(&pool, "Various Artists").await;
    sqlx::raw_sql(VA_UPDATE).execute(&pool).await.unwrap();
    let row = sqlx::query(
        "SELECT mbid::text AS mbid, enrichment_status, mbid_source FROM artists \
         WHERE name_key = 'various artists'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row.get::<String, _>("mbid"),
        "89ad4ac3-39f7-470e-963a-56509c546377"
    );
    assert_eq!(row.get::<String, _>("enrichment_status"), "completed");
    assert_eq!(row.get::<Option<String>, _>("mbid_source"), None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn source_checks_reject_unknown_values(pool: PgPool) {
    let ar = artist(&pool, "A").await;
    let al = album(&pool, ar).await;
    let t = track(&pool, al).await;
    let bad = [
        sqlx::query("UPDATE artists SET mbid_source = 'x' WHERE id = $1").bind(ar),
        sqlx::query("UPDATE tracks SET recording_source = 'x' WHERE id = $1").bind(t),
        sqlx::query(
            "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'recording', 'x')",
        )
        .bind(t),
        sqlx::query("INSERT INTO track_lyrics (track_id, source) VALUES ($1, 'x')").bind(t),
    ];
    for q in bad {
        assert!(q.execute(&pool).await.is_err());
    }
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'recording', 'pending')",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isrc_check_rejects_lowercase_and_short(pool: PgPool) {
    let t = track(&pool, album(&pool, artist(&pool, "A").await).await).await;
    for bad in ["gbaaa1234567", "GBAAA123", "GBAAA12345678"] {
        let r = sqlx::query("UPDATE tracks SET isrc = $2 WHERE id = $1")
            .bind(t)
            .bind(bad)
            .execute(&pool)
            .await;
        assert!(r.is_err(), "{bad} accepted");
    }
    sqlx::query("UPDATE tracks SET isrc = 'GBAAA1234567' WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn enrichment_state_has_one_row(pool: PgPool) {
    let status: String = sqlx::query_scalar("SELECT status FROM enrichment_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "idle");
    assert!(
        sqlx::query("INSERT INTO enrichment_state DEFAULT VALUES")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn effective_genres_prefer_musicbrainz(pool: PgPool) {
    let ar = artist(&pool, "A").await;
    let al = album(&pool, ar).await;
    let t_both = track(&pool, al).await;
    let t_file = track(&pool, al).await;
    let rock = genre(&pool, "rock").await;
    let jazz = genre(&pool, "jazz").await;
    for (ent, col, tbl) in [
        (t_both, "track_id", "track_genres"),
        (al, "album_id", "album_genres"),
        (ar, "artist_id", "artist_genres"),
    ] {
        for (g, src) in [(rock, "file"), (jazz, "musicbrainz")] {
            sqlx::query(&format!(
                "INSERT INTO {tbl} ({col}, genre_id, source, rank) VALUES ($1, $2, $3, 0)"
            ))
            .bind(ent)
            .bind(g)
            .bind(src)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    sqlx::query(
        "INSERT INTO track_genres (track_id, genre_id, source, rank) VALUES ($1, $2, 'file', 0)",
    )
    .bind(t_file)
    .bind(rock)
    .execute(&pool)
    .await
    .unwrap();
    for (view, col, ent) in [
        ("track_genres_effective", "track_id", t_both),
        ("album_genres_effective", "album_id", al),
        ("artist_genres_effective", "artist_id", ar),
    ] {
        let got: Vec<Uuid> =
            sqlx::query_scalar(&format!("SELECT genre_id FROM {view} WHERE {col} = $1"))
                .bind(ent)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(got, vec![jazz], "{view}");
    }
    let got: Vec<Uuid> =
        sqlx::query_scalar("SELECT genre_id FROM track_genres_effective WHERE track_id = $1")
            .bind(t_file)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(got, vec![rock]);

    // Album and artist with only file rows keep them.
    let ar2 = artist(&pool, "B").await;
    let al2 = album(&pool, ar2).await;
    sqlx::query(
        "INSERT INTO album_genres (album_id, genre_id, source, rank) VALUES ($1, $2, 'file', 0)",
    )
    .bind(al2)
    .bind(rock)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO artist_genres (artist_id, genre_id, source, rank) VALUES ($1, $2, 'file', 0)",
    )
    .bind(ar2)
    .bind(rock)
    .execute(&pool)
    .await
    .unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM album_genres_effective WHERE album_id = $1) \
              + (SELECT count(*) FROM artist_genres_effective WHERE artist_id = $2)",
    )
    .bind(al2)
    .bind(ar2)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deleting_track_cascades(pool: PgPool) {
    let t = track(&pool, album(&pool, artist(&pool, "A").await).await).await;
    let g = genre(&pool, "rock").await;
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'recording', 'pending')",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_genres (track_id, genre_id, source, rank) VALUES ($1, $2, 'file', 0)",
    )
    .bind(t)
    .bind(g)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO track_lyrics (track_id, source) VALUES ($1, 'none')")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track_acoustid (track_id, fingerprint, duration_secs) VALUES ($1, 'fp', 10)",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    for tbl in [
        "track_stages",
        "track_genres",
        "track_lyrics",
        "track_acoustid",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {tbl}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{tbl}");
    }
}
