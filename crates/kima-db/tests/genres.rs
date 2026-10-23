//! Effective genres and the library genre list (spec §13).

use kima_db::PgPool;
use kima_db::genres::{self, GenreSource};
use uuid::Uuid;

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

async fn album(pool: &PgPool, artist: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $3)")
        .bind(id)
        .bind(artist)
        .bind(id.to_string())
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn tracks(
    pool: &PgPool,
    album: Uuid,
    artist_name: &str,
    n: usize,
    missing: usize,
) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for i in 0..n {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint, missing_since)
             VALUES ($1, $2, $3, 't', 'mp3', $4, 1, now(), $5, CASE WHEN $6 THEN now() END)",
        )
        .bind(id)
        .bind(album)
        .bind(artist_name)
        .bind(format!("/{id}"))
        .bind(id.as_bytes().to_vec())
        .bind(i < missing)
        .execute(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    ids
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_effective_musicbrainz_then_file(pool: PgPool) {
    let a = artist(&pool, "A").await;
    let b = album(&pool, a).await;
    let t = tracks(&pool, b, "A", 2, 0).await;
    let mut c = pool.acquire().await.unwrap();
    genres::replace_track_genres(&mut c, t[0], GenreSource::File, &s(&["Rock", "Pop"]))
        .await
        .unwrap();
    genres::replace_track_genres(&mut c, t[1], GenreSource::File, &s(&["Jazz"]))
        .await
        .unwrap();
    genres::replace_track_genres(
        &mut c,
        t[1],
        GenreSource::MusicBrainz,
        &s(&["Blues", "Soul"]),
    )
    .await
    .unwrap();
    drop(c);
    let m = genres::effective_for_tracks(&pool, &t).await.unwrap();
    assert_eq!(m[&t[0]], s(&["Rock", "Pop"]));
    assert_eq!(m[&t[1]], s(&["Blues", "Soul"]));
    assert!(
        genres::effective_for_tracks(&pool, &[])
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_and_artist_effective_same_rule(pool: PgPool) {
    let a1 = artist(&pool, "A1").await;
    let a2 = artist(&pool, "A2").await;
    let b1 = album(&pool, a1).await;
    let b2 = album(&pool, a2).await;
    let mut c = pool.acquire().await.unwrap();
    genres::replace_album_genres(&mut c, b1, GenreSource::File, &s(&["Rock"]))
        .await
        .unwrap();
    genres::replace_album_genres(&mut c, b1, GenreSource::MusicBrainz, &s(&["Punk", "Ska"]))
        .await
        .unwrap();
    genres::replace_album_genres(&mut c, b2, GenreSource::File, &s(&["Folk", "Rock"]))
        .await
        .unwrap();
    genres::replace_artist_genres(&mut c, a1, GenreSource::File, &s(&["Rock"]))
        .await
        .unwrap();
    genres::replace_artist_genres(&mut c, a1, GenreSource::MusicBrainz, &s(&["Punk"]))
        .await
        .unwrap();
    genres::replace_artist_genres(&mut c, a2, GenreSource::File, &s(&["Folk"]))
        .await
        .unwrap();
    drop(c);
    let al = genres::effective_for_albums(&pool, &[b1, b2])
        .await
        .unwrap();
    assert_eq!(al[&b1], s(&["Punk", "Ska"]));
    assert_eq!(al[&b2], s(&["Folk", "Rock"]));
    let ar = genres::effective_for_artists(&pool, &[a1, a2])
        .await
        .unwrap();
    assert_eq!(ar[&a1], s(&["Punk"]));
    assert_eq!(ar[&a2], s(&["Folk"]));
    // The `tags` alias is MusicBrainz only.
    let mb = genres::musicbrainz_for_albums(&pool, &[b1, b2])
        .await
        .unwrap();
    assert_eq!(mb[&b1], s(&["Punk", "Ska"]));
    assert!(!mb.contains_key(&b2));
    let mb = genres::musicbrainz_for_artists(&pool, &[a1, a2])
        .await
        .unwrap();
    assert_eq!(mb[&a1], s(&["Punk"]));
    assert!(!mb.contains_key(&a2));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn underscore_genres_never_stored(pool: PgPool) {
    let a = artist(&pool, "A").await;
    let b = album(&pool, a).await;
    let t = tracks(&pool, b, "A", 1, 0).await;
    let mut c = pool.acquire().await.unwrap();
    genres::replace_track_genres(
        &mut c,
        t[0],
        GenreSource::File,
        &s(&["_internal", "Rock", " _x"]),
    )
    .await
    .unwrap();
    drop(c);
    let m = genres::effective_for_tracks(&pool, &t).await.unwrap();
    assert_eq!(m[&t[0]], s(&["Rock"]));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM genres WHERE name LIKE '\\_%'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn merged_puts_user_first_unique() {
    assert_eq!(
        genres::merged(&s(&["Jazz", "rock"]), &s(&["Rock", "Pop", "jazz", "Pop"])),
        s(&["Jazz", "rock", "Pop"])
    );
    assert_eq!(genres::merged(&[], &s(&["A", "", "a"])), s(&["A"]));
}

/// An artist with one album of `n` tracks (`missing` of them missing), file genres `names`.
async fn seeded(
    pool: &PgPool,
    artist_name: &str,
    n: usize,
    missing: usize,
    names: &[&str],
) -> Uuid {
    let a = artist(pool, artist_name).await;
    let b = album(pool, a).await;
    tracks(pool, b, artist_name, n, missing).await;
    let mut c = pool.acquire().await.unwrap();
    genres::replace_artist_genres(&mut c, a, GenreSource::File, &s(names))
        .await
        .unwrap();
    a
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_genres_threshold_15_and_limit_20(pool: PgPool) {
    seeded(&pool, "A", 15, 0, &["edge15"]).await;
    seeded(&pool, "B", 14, 0, &["edge14"]).await;
    // 16 tracks, one missing: 15 present, counts.
    seeded(&pool, "C", 16, 1, &["present15"]).await;
    // 16 tracks, two missing: 14 present, does not count.
    seeded(&pool, "D", 16, 2, &["present14"]).await;
    let got = genres::library_genres(&pool).await.unwrap();
    assert_eq!(got, vec![("edge15".into(), 15), ("present15".into(), 15)]);

    // 21 qualifying genres: only 20 returned, biggest first.
    for i in 0..21 {
        let name = format!("g{i:02}");
        seeded(&pool, &format!("X{i}"), 20 + i, 0, &[name.as_str()]).await;
    }
    let got = genres::library_genres(&pool).await.unwrap();
    assert_eq!(got.len(), 20);
    assert_eq!(got[0], ("g20".into(), 40));
    assert_eq!(got[19], ("g01".into(), 21));
    assert!(got.iter().all(|(n, _)| n != "g00"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_genres_counts_distinct_tracks_across_artists(pool: PgPool) {
    seeded(&pool, "A", 8, 0, &["shared"]).await;
    seeded(&pool, "B", 7, 0, &["shared"]).await;
    assert_eq!(
        genres::library_genres(&pool).await.unwrap(),
        vec![("shared".into(), 15)]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_name_candidates_fold_case_accents_and_ampersand(pool: PgPool) {
    artist(&pool, "Beyoncé").await;
    artist(&pool, "R and B").await;
    artist(&pool, "Other").await;
    let got = genres::artist_name_candidates(&pool, &s(&["beyonce", "r&b", "rock"]))
        .await
        .unwrap();
    let mut got = got;
    got.sort();
    assert_eq!(
        got,
        vec![
            ("beyonce".into(), "Beyoncé".into()),
            ("r&b".into(), "R and B".into())
        ]
    );
}
