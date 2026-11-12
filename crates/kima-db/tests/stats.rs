use kima_db::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn playable_db_counts_listens_and_respects_user(pool: PgPool) {
    use kima_db::stats::{listens::Period, playable::playable_slice};
    let user = Uuid::now_v7();
    let other = Uuid::now_v7();
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id,username,password_hash) VALUES ($1,'ana','h'),($2,'ben','h')",
    )
    .bind(user)
    .bind(other)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO artists (id,name,name_key,display_name) VALUES ($1,'artist','artist','Edited artist')").bind(artist).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO albums (id,artist_id,title,title_key,year,display_year,display_title) VALUES ($1,$2,'album','album',1980,1999,'Edited album')").bind(album).bind(artist).execute(&pool).await.unwrap();
    for (id, title) in [(a, "A"), (b, "B")] {
        sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,display_title,duration_secs,codec,path,size,mtime,fingerprint) VALUES ($1,$2,'artist',$3,$3,180,'flac',$3,0,now(),decode('','hex'))").bind(id).bind(album).bind(title).execute(&pool).await.unwrap();
    }
    for (who, track, offset) in [(user, a, 0), (user, a, -30), (user, b, -300), (other, b, 0)] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,now()+$4 * interval '1 second')").bind(Uuid::now_v7()).bind(who).bind(track).bind(offset as f64).execute(&pool).await.unwrap();
    }
    let rows = playable_slice(&pool, user, "top", f64::NAN, 40, Period::All)
        .await
        .unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a, b]);
    assert_eq!(rows[0].artist_name, "Edited artist");
    assert_eq!(rows[0].album_title, "Edited album");
    assert!(
        playable_slice(&pool, user, "never", f64::NAN, 40, Period::All)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        playable_slice(&pool, other, "never", f64::NAN, 40, Period::All)
            .await
            .unwrap()[0]
            .id,
        a
    );
    assert_eq!(
        playable_slice(&pool, user, "decade", 1990.0, 40, Period::All)
            .await
            .unwrap()
            .len(),
        2
    );
}

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

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn walk_agrees_with_audit_grouping(pool: PgPool) {
    use kima_db::stats::{
        audit,
        listens::{Period, listen_rows},
    };
    let user = Uuid::now_v7();
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,username,password_hash) VALUES ($1,'audit-walk','h')")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artists(id,name,name_key) VALUES($1,'a','a')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums(id,artist_id,title,title_key) VALUES($1,$2,'a','a')")
        .bind(album)
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    let mut tracks = Vec::new();
    for (i, duration) in [5, 30, 31, 100, 214, 0].into_iter().enumerate() {
        let t = Uuid::now_v7();
        tracks.push((t, duration));
        sqlx::query("INSERT INTO tracks(id,album_id,artist_name,title,codec,path,size,mtime,fingerprint,duration_secs) VALUES($1,$2,'a','t','flac',$3,0,now(),decode('','hex'),$4)").bind(t).bind(album).bind(format!("/audit{i}")).bind(duration).execute(&pool).await.unwrap();
    }
    let mut rng = 42_u64;
    for case in 0..200 {
        sqlx::query("DELETE FROM plays WHERE user_id=$1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
        let mut next = || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            rng
        };
        let count = 1 + (next() % 40);
        let ntracks = 1 + (next() % 4) as usize;
        let start = (next() % 6) as usize;
        let mut at = 0_i64;
        for j in 0..count {
            let (t, duration) = tracks[(start + (next() as usize % ntracks)) % tracks.len()];
            at += if j % 7 == 0 {
                i64::from(duration.max(30))
            } else {
                (next() % 401) as i64
            };
            sqlx::query("INSERT INTO plays(id,user_id,track_id,played_at) VALUES($1,$2,$3,'2026-03-01'::timestamptz+$4*interval '1 second')").bind(Uuid::now_v7()).bind(user).bind(t).bind(at as f64).execute(&pool).await.unwrap();
        }
        let rows = audit::audit_rows(&pool, user).await.unwrap();
        let listens = audit::listens_of(&rows);
        let sql = format!(
            "{} SELECT track_id,count(*)::bigint FROM p GROUP BY track_id ORDER BY track_id",
            listen_rows(Period::All, false)
        );
        let db: Vec<(Uuid, i64)> = sqlx::query_as(&sql)
            .bind(user)
            .fetch_all(&pool)
            .await
            .unwrap();
        let mut counts = std::collections::BTreeMap::new();
        for l in listens {
            *counts.entry(l.track_id).or_insert(0_i64) += 1;
        }
        assert_eq!(db, counts.into_iter().collect::<Vec<_>>(), "case {case}");
    }
}

// ---- Task 10: transaction boundary and concurrency of collapse/remove ----

/// A user and `n` 180 s tracks.
async fn audit_fixture(pool: &PgPool, n: usize) -> (Uuid, Vec<Uuid>) {
    audit_fixture_with(pool, &vec![180; n]).await
}

/// A user and one track per entry of `durations` (seconds).
async fn audit_fixture_with(pool: &PgPool, durations: &[i32]) -> (Uuid, Vec<Uuid>) {
    let user = Uuid::now_v7();
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id,username,password_hash) VALUES ($1,'audit','h')")
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artists (id,name,name_key) VALUES ($1,'artist','artist')")
        .bind(artist)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id,artist_id,title,title_key) VALUES ($1,$2,'album','album')")
        .bind(album)
        .bind(artist)
        .execute(pool)
        .await
        .unwrap();
    let mut tracks = Vec::new();
    for (i, duration) in durations.iter().enumerate() {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,duration_secs,codec,path,size,mtime,fingerprint) VALUES ($1,$2,'artist',$3,$4,'flac',$3,0,now(),decode('','hex'))")
            .bind(id)
            .bind(album)
            .bind(format!("t{i}"))
            .bind(duration)
            .execute(pool)
            .await
            .unwrap();
        tracks.push(id);
    }
    (user, tracks)
}

async fn audit_play<'e>(
    conn: impl sqlx::PgExecutor<'e>,
    user: Uuid,
    track: Uuid,
    second: i64,
    heard: bool,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at,heard) VALUES ($1,$2,$3,'2026-03-01T12:00:00Z'::timestamptz + $4 * interval '1 second',$5)")
        .bind(id)
        .bind(user)
        .bind(track)
        .bind(second as f64)
        .bind(heard)
        .execute(conn)
        .await
        .unwrap();
    id
}

/// Hold `plays` in SHARE mode: a request's reads go through, its `DELETE` waits
/// for the lock before it takes its snapshot, so whatever this transaction
/// writes before committing is visible to that `DELETE`.
async fn hold_plays(pool: &PgPool) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE plays IN SHARE MODE")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

/// Wait until `n` sessions sit on that lock with their `DELETE`.
async fn parked(pool: &PgPool, n: i64) {
    for _ in 0..500 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
             AND wait_event_type = 'Lock' AND wait_event = 'relation' \
             AND query LIKE 'DELETE FROM plays%'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting == n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("{n} DELETE statements never waited on the plays lock");
}

async fn all_plays(pool: &PgPool) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM plays ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn collapse_never_deletes_a_play_recorded_after_its_read(pool: PgPool) {
    use kima_db::stats::audit;
    let (user, tracks) = audit_fixture(&pool, 1).await;
    let mut ids = Vec::new();
    for second in [0, 10, 20, 30] {
        ids.push(audit_play(&pool, user, tracks[0], second, false).await);
    }
    let mut holder = hold_plays(&pool).await;
    let task = tokio::spawn({
        let pool = pool.clone();
        async move { audit::collapse(&pool, user).await.unwrap() }
    });
    parked(&pool, 1).await;
    // A late (back-dated) row inside the same listen, committed after the read
    // and before the DELETE's snapshot.
    let late = audit_play(&mut *holder, user, tracks[0], 15, false).await;
    holder.commit().await.unwrap();
    assert_eq!(task.await.unwrap(), 2);
    let mut expected = vec![ids[0], ids[3], late];
    expected.sort();
    assert_eq!(all_plays(&pool).await, expected);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn remove_never_deletes_a_play_recorded_after_its_read(pool: PgPool) {
    use kima_db::stats::audit;
    let (user, tracks) = audit_fixture(&pool, 2).await;
    audit_play(&pool, user, tracks[0], 0, false).await;
    audit_play(&pool, user, tracks[0], 10, false).await;
    let next = audit_play(&pool, user, tracks[1], 20, true).await;
    let mut holder = hold_plays(&pool).await;
    let task = tokio::spawn({
        let pool = pool.clone();
        let wanted = vec![tracks[0]];
        async move { audit::remove(&pool, user, &wanted).await.unwrap() }
    });
    parked(&pool, 1).await;
    let late = audit_play(&mut *holder, user, tracks[0], 5, true).await;
    holder.commit().await.unwrap();
    assert_eq!(task.await.unwrap(), (2, 1));
    let mut expected = vec![next, late];
    expected.sort();
    assert_eq!(all_plays(&pool).await, expected);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_collapses_delete_each_row_once(pool: PgPool) {
    use kima_db::stats::audit;
    let (user, tracks) = audit_fixture(&pool, 1).await;
    let mut ids = Vec::new();
    for second in [0, 10, 20, 30] {
        ids.push(audit_play(&pool, user, tracks[0], second, false).await);
    }
    let holder = hold_plays(&pool).await;
    let spawn = || {
        let pool = pool.clone();
        tokio::spawn(async move { audit::collapse(&pool, user).await.unwrap() })
    };
    let (first, second) = (spawn(), spawn());
    parked(&pool, 2).await;
    holder.commit().await.unwrap();
    let mut removed = vec![first.await.unwrap(), second.await.unwrap()];
    removed.sort();
    assert_eq!(removed, vec![0, 2]);
    assert_eq!(all_plays(&pool).await, vec![ids[0], ids[3]]);
}

/// A play at an exact `timestamptz` literal (sub-second precision).
async fn audit_play_at(pool: &PgPool, user: Uuid, track: Uuid, at: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,$4::timestamptz)",
    )
    .bind(id)
    .bind(user)
    .bind(track)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Review scenario (D-178): rows 99.9995 s apart on a 100 s track. Measured in
/// microseconds they are one listen; truncated to milliseconds each, 100.000 s,
/// i.e. two. The SQL walk and the audit must give the same answer.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sub_millisecond_walk_and_audit_grouping_agree(pool: PgPool) {
    use kima_db::stats::{
        audit,
        listens::{Period, listen_rows},
    };
    let (user, tracks) = audit_fixture_with(&pool, &[100]).await;
    audit_play_at(&pool, user, tracks[0], "2026-03-01T12:00:00.000600Z").await;
    audit_play_at(&pool, user, tracks[0], "2026-03-01T12:01:40.000100Z").await;
    let sql = format!(
        "{} SELECT count(*)::bigint FROM p",
        listen_rows(Period::All, false)
    );
    let walk: i64 = sqlx::query_scalar(&sql)
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    let rows = audit::audit_rows(&pool, user).await.unwrap();
    let listens = audit::listens_of(&rows);
    assert_eq!(walk, 1);
    assert_eq!(listens.len() as i64, walk);
    assert_eq!(listens[0].play_ids.len(), 2);
}

/// Same scenario: B is the second row of a heard listen, so `remove` must not
/// treat it as a skipped listen of its own.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sub_millisecond_remove_keeps_a_row_inside_a_listen(pool: PgPool) {
    use kima_db::stats::audit;
    let (user, tracks) = audit_fixture_with(&pool, &[100, 180]).await;
    let a = audit_play_at(&pool, user, tracks[0], "2026-03-01T12:00:00.000600Z").await;
    let b = audit_play_at(&pool, user, tracks[0], "2026-03-01T12:01:40.000100Z").await;
    let c = audit_play_at(&pool, user, tracks[1], "2026-03-01T12:01:50Z").await;
    assert_eq!(
        audit::remove(&pool, user, &[tracks[0]]).await.unwrap(),
        (0, 0)
    );
    let mut expected = vec![a, b, c];
    expected.sort();
    assert_eq!(all_plays(&pool).await, expected);
}
