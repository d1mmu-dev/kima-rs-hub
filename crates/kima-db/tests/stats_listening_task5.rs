use kima_db::PgPool;
use kima_db::stats::{listening::listening_stats, listens::Period};
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn empty_user_has_empty_listening_stats(pool: PgPool) {
    let stats = listening_stats(&pool, Uuid::now_v7(), "UTC", Period::All)
        .await
        .unwrap();
    assert_eq!(stats.plays, 0);
    assert_eq!(stats.estimated_seconds, 0);
    assert_eq!(stats.clock, [[0; 24]; 7]);
    assert!(stats.peak.is_none());
    assert!(stats.by_source.is_empty());
    assert!(stats.drift.is_empty());
    assert!(stats.top_artists.is_empty());
    assert!(stats.top_tracks.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listening_values_isolate_users_and_respect_boundaries(pool: PgPool) {
    let user = Uuid::now_v7();
    let other = Uuid::now_v7();
    for (id, name) in [(user, "one"), (other, "two")] {
        sqlx::query("INSERT INTO users (id,username,password_hash) VALUES ($1,$2,'hash')")
            .bind(id)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
    }
    let mut tracks = Vec::new();
    for i in 0..9 {
        let artist = Uuid::now_v7();
        let album = Uuid::now_v7();
        let track = Uuid::now_v7();
        sqlx::query("INSERT INTO artists (id,name,name_key,display_name) VALUES ($1,$2,$2,$3)")
            .bind(artist)
            .bind(format!("artist{i}"))
            .bind(format!("display{i}"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO albums (id,artist_id,title,title_key) VALUES ($1,$2,$3,$3)")
            .bind(album)
            .bind(artist)
            .bind(format!("album{i}"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,display_title,codec,path,size,mtime,fingerprint,duration_secs,missing_since) VALUES ($1,$2,'a',$3,$4,'flac',$5,0,now(),decode('','hex'),$6,CASE WHEN $7 THEN now() ELSE NULL END)").bind(track).bind(album).bind(format!("track{i}")).bind(format!("title{i}")).bind(format!("/t{i}")).bind(if i == 1 {700} else {180}).bind(i == 0).execute(&pool).await.unwrap();
        tracks.push(track);
    }
    for (who, track, timestamp, source) in [
        (user, tracks[0], "2026-03-31 22:30:00+00", "LIBRARY"),
        (user, tracks[0], "2026-03-31 22:32:59+00", "LIBRARY"),
        (user, tracks[0], "2026-03-31 22:33:00+00", "SUBSONIC"),
        (user, tracks[1], "2026-03-31 22:30:00+00", "LIBRARY"),
        (other, tracks[0], "2026-03-31 22:30:00+00", "SUBSONIC"),
    ] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at,source) VALUES ($1,$2,$3,$4::timestamptz,$5)").bind(Uuid::now_v7()).bind(who).bind(track).bind(timestamp).bind(source).execute(&pool).await.unwrap();
    }
    let stats = listening_stats(&pool, user, "Europe/Warsaw", Period::All)
        .await
        .unwrap();
    assert_eq!(
        (
            stats.plays,
            stats.estimated_seconds,
            stats.distinct_tracks,
            stats.distinct_albums,
            stats.distinct_artists
        ),
        (3, 960, 2, 2, 2)
    );
    assert_eq!(stats.clock[3][0], 3);
    assert_eq!(
        stats.peak.as_ref().map(|p| (p.weekday, p.hour, p.plays)),
        Some((3, 0, 3))
    );
    assert_eq!(
        stats
            .by_source
            .iter()
            .find(|(s, _)| s == "SUBSONIC")
            .unwrap()
            .1,
        1
    );
    assert!(stats.first_play.is_some());
    assert!(stats.last_play.unwrap() > stats.first_play.unwrap());
    assert_eq!(stats.drift[0].month, "2026-04");
    assert_eq!(stats.drift[0].plays, 3);
    assert_eq!(stats.top_tracks[0].title, "title0");
    assert_eq!(stats.top_artists[0].name, "display0");
    assert_eq!(
        listening_stats(&pool, user, "UTC", Period::Days30)
            .await
            .unwrap()
            .plays,
        0
    );
    assert_eq!(
        listening_stats(&pool, user, "UTC", Period::All)
            .await
            .unwrap()
            .drift[0]
            .month,
        "2026-03"
    );
    assert_eq!(
        listening_stats(&pool, other, "UTC", Period::All)
            .await
            .unwrap()
            .plays,
        1
    );
    for track in &tracks[2..] {
        sqlx::query("INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,'2026-03-31 22:30:00+00')").bind(Uuid::now_v7()).bind(user).bind(track).execute(&pool).await.unwrap();
    }
    let stats = listening_stats(&pool, user, "UTC", Period::All)
        .await
        .unwrap();
    assert_eq!((stats.top_tracks.len(), stats.top_artists.len()), (8, 8));
    assert_eq!(stats.top_tracks[1].title, "title1");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tied_peak_chooses_earliest_cell(pool: PgPool) {
    let user = Uuid::now_v7();
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    let track = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id,username,password_hash) VALUES ($1,'tie','hash')")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artists (id,name,name_key) VALUES ($1,'a','a')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id,artist_id,title,title_key) VALUES ($1,$2,'b','b')")
        .bind(album)
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,codec,path,size,mtime,fingerprint,duration_secs) VALUES ($1,$2,'a','t','flac','/tie',0,now(),decode('','hex'),180)").bind(track).bind(album).execute(&pool).await.unwrap();
    for date in ["2026-03-01 23:30:00+00", "2026-03-02 00:30:00+00"] {
        sqlx::query(
            "INSERT INTO plays (id,user_id,track_id,played_at) VALUES ($1,$2,$3,$4::timestamptz)",
        )
        .bind(Uuid::now_v7())
        .bind(user)
        .bind(track)
        .bind(date)
        .execute(&pool)
        .await
        .unwrap();
    }
    let stats = listening_stats(&pool, user, "UTC", Period::All)
        .await
        .unwrap();
    assert_eq!(
        stats.peak.map(|p| (p.weekday, p.hour, p.plays)),
        Some((0, 23, 1))
    );
}
