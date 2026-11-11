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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn safe_zone_keeps_postgres_zones(pool: PgPool) {
    use kima_db::stats::listens::safe_zone;
    for (input, expected) in [
        (Some("Europe/London"), "Europe/London"),
        (Some("America/Chicago"), "America/Chicago"),
        (Some("Not/AZone"), "UTC"),
        (Some("'; DROP TABLE x; --"), "UTC"),
        (None, "UTC"),
        (Some(""), "UTC"),
        (
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "UTC",
        ),
    ] {
        assert_eq!(safe_zone(&pool, input).await.unwrap(), expected);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn walk_counts_listens_not_rows(pool: PgPool) {
    use kima_db::stats::listens::{Period, listen_rows};
    let user = Uuid::now_v7();
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, 'walk', 'hash')")
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
    for (i, (duration, offsets, expected)) in [
        (200, vec![0, 44, 126, 173], 1),
        (214, vec![0, 44, 126, 173, 230], 2),
        (100, vec![0, 100], 2),
        (100, vec![0, 99], 1),
        (5, vec![0, 10, 40], 2),
    ]
    .into_iter()
    .enumerate()
    {
        let track = Uuid::now_v7();
        sqlx::query("INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint, duration_secs) VALUES ($1, $2, 'a', 't', 'flac', $3, 0, now(), decode('', 'hex'), $4)").bind(track).bind(album).bind(format!("/walk{i}")).bind(duration).execute(&pool).await.unwrap();
        let mut last_id = Uuid::nil();
        for offset in offsets {
            last_id = Uuid::now_v7();
            sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now() + $4 * interval '1 second')").bind(last_id).bind(user).bind(track).bind(offset as f64).execute(&pool).await.unwrap();
        }
        let sql = format!("{} SELECT count(*) FROM p", listen_rows(Period::All, true));
        let count: i64 = sqlx::query_scalar(&sql)
            .bind(user)
            .bind(track)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, expected, "track {i}");
        if i == 1 {
            let sql = format!(
                "{} SELECT id FROM p ORDER BY played_at DESC LIMIT 1",
                listen_rows(Period::All, true)
            );
            let actual: Uuid = sqlx::query_scalar(&sql)
                .bind(user)
                .bind(track)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(actual, last_id, "second listen starts at +230 seconds");
        }
    }
    // With all tracks selected, interleaved rows remain partitioned by track.
    let tracks: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tracks WHERE path IN ('/walk0', '/walk3') ORDER BY path",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let sql = format!(
        "{} SELECT track_id, count(*) FROM p WHERE track_id = ANY($2) GROUP BY track_id ORDER BY track_id",
        listen_rows(Period::All, false)
    );
    let counts: Vec<(Uuid, i64)> = sqlx::query_as(&sql)
        .bind(user)
        .bind(&tracks)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(counts.len(), 2);
    assert!(counts.iter().all(|(_, count)| *count == 1));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn period_window_applies_after_the_walk(pool: PgPool) {
    use kima_db::stats::listens::{Period, listen_rows};
    let user = Uuid::now_v7();
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    let track = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, 'period', 'hash')")
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
    sqlx::query("INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint, duration_secs) VALUES ($1, $2, 'a', 't', 'flac', '/period', 0, now(), decode('', 'hex'), 259200)").bind(track).bind(album).execute(&pool).await.unwrap();
    for days in [31, 29] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now() - $4 * interval '1 day')").bind(Uuid::now_v7()).bind(user).bind(track).bind(days as f64).execute(&pool).await.unwrap();
    }
    for (period, expected) in [(Period::All, 1), (Period::Days30, 0), (Period::Months12, 1)] {
        let count: i64 = sqlx::query_scalar(&format!(
            "{} SELECT count(*) FROM p",
            listen_rows(period, false)
        ))
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, expected, "{period:?}");
    }

    // A listen that starts before the twelve-month boundary must not be
    // reopened by a later raw row inside that boundary.
    sqlx::query("DELETE FROM plays WHERE user_id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    for offset in ["-1 hour", "1 hour"] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now() - interval '12 months' + $4::interval)")
            .bind(Uuid::now_v7()).bind(user).bind(track).bind(offset)
            .execute(&pool).await.unwrap();
    }
    for (period, expected) in [(Period::All, 1), (Period::Months12, 0)] {
        let count: i64 = sqlx::query_scalar(&format!(
            "{} SELECT count(*) FROM p",
            listen_rows(period, false)
        ))
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, expected, "{period:?}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn overview_counts_raw_rows_for_shelf_and_listens_for_signature(pool: PgPool) {
    use kima_db::stats::{listens::Period, overview::overview};
    let user = Uuid::now_v7();
    let other = Uuid::now_v7();
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    let track = Uuid::now_v7();
    for (id, name) in [(user, "a"), (other, "b")] {
        sqlx::query("INSERT INTO users (id,username,password_hash) VALUES ($1,$2,'hash')")
            .bind(id)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO artists (id,name,name_key) VALUES ($1,'a','a')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id,artist_id,title,title_key,year,display_year) VALUES ($1,$2,'a','a',1905,1994)").bind(album).bind(artist).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,codec,path,size,mtime,fingerprint,duration_secs) VALUES ($1,$2,'a','t','flac','/t',0,now(),decode('','hex'),180)").bind(track).bind(album).execute(&pool).await.unwrap();
    for (id, seconds) in [(user, 0), (user, 20), (other, 0)] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now()-interval '60 days'+$4*interval '1 second')").bind(Uuid::now_v7()).bind(id).bind(track).bind(seconds as f64).execute(&pool).await.unwrap();
    }
    let all = overview(&pool, user, Period::All).await.unwrap();
    assert_eq!((all.total.tracks, all.heard.tracks), (1, 1));
    assert_eq!((all.decades[0].decade, all.decades[0].heard), (1990, 1));
    assert_eq!(
        (
            all.concentration.for_half,
            all.concentration.for_eighty,
            all.concentration.distinct
        ),
        (Some(1), Some(1), Some(1))
    );
    assert_eq!(all.habit.moves, 0);
    assert_eq!(
        (
            all.era[0].label.as_str(),
            all.era[0].median,
            all.era[0].plays
        ),
        ("before 2000", 1994, 1)
    );
    let recent = overview(&pool, user, Period::Days30).await.unwrap();
    assert_eq!(recent.heard.tracks, 1);
    assert!(recent.concentration.distinct.is_none());
    assert!(recent.era.is_empty());
}
