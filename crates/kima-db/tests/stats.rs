use kima_db::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ignore_rows_cascade_with_track_and_user(pool: PgPool) {
    let user = Uuid::now_v7();
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    let track = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, 'ana', 'hash')")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'a', 'a')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 'b', 'b')")
        .bind(album)
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint) VALUES ($1, $2, 'a', 't', 'flac', '/t', 0, now(), decode('', 'hex'))")
        .bind(track).bind(album).execute(&pool).await.unwrap();
    for delete_track in [true, false] {
        sqlx::query("INSERT INTO play_audit_ignore (user_id, track_id) VALUES ($1, $2)")
            .bind(user)
            .bind(track)
            .execute(&pool)
            .await
            .unwrap();
        if delete_track {
            sqlx::query("DELETE FROM tracks WHERE id = $1")
                .bind(track)
                .execute(&pool)
                .await
                .unwrap();
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(user)
                .execute(&pool)
                .await
                .unwrap();
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM play_audit_ignore")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        if delete_track {
            sqlx::query("INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint) VALUES ($1, $2, 'a', 't', 'flac', '/t', 0, now(), decode('', 'hex'))")
                .bind(track).bind(album).execute(&pool).await.unwrap();
        }
    }
}
