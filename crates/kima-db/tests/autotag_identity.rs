//! Album and artist identity on apply and undo (spec §19.7, §24.3 R1/R2), database only.

use std::time::Duration;

use kima_db::autotag::{
    Applied, AppliedAlbum, AppliedTrack, ConflictAlbum, IdentityError, IdentityKeys, NewCandidate,
    NewRun, Origin, Recommendation, RunRow, RunState, Target, active_run, apply_identity,
    create_run, delete_overrides, find_collision, finish_run, get_run, overrides_for_paths,
    put_overrides, store_search, transition_run, undo_collision, undo_identity,
};
use kima_db::{PgPool, library};
use serde_json::{Value, json};
use uuid::Uuid;

const REL: Uuid = uuid::uuid!("11111111-1111-4111-8111-111111111111");
const RG: Uuid = uuid::uuid!("22222222-2222-4222-8222-222222222222");
const ART_MBID: Uuid = uuid::uuid!("33333333-3333-4333-8333-333333333333");
const REC1: Uuid = uuid::uuid!("44444444-4444-4444-8444-444444444441");
const REC2: Uuid = uuid::uuid!("44444444-4444-4444-8444-444444444442");

fn key(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

async fn artist(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(name)
        .bind(key(name))
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn album(pool: &PgPool, artist: Uuid, title: &str, year: Option<i32>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, year) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(artist)
    .bind(title)
    .bind(key(title))
    .bind(year)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn track(pool: &PgPool, album: Uuid, path: &str, missing: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, \
         fingerprint, missing_since) \
         VALUES ($1, $2, 'a', $3, 'flac', $3, 1, now(), '\\x00', CASE WHEN $4 THEN now() END)",
    )
    .bind(id)
    .bind(album)
    .bind(path)
    .bind(missing)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn candidate(pool: &PgPool, album: Uuid) -> Uuid {
    store_search(
        pool,
        album,
        &[NewCandidate {
            release_mbid: REL,
            release_group_mbid: RG,
            origin: Origin::Search,
            distance: 0.01,
            recommendation: Recommendation::Strong,
            penalties: json!({}),
            mapping: json!([]),
            extra_tracks: json!([]),
            missing_tracks: json!([]),
            release: json!({"id": REL}),
            release_group: json!({"id": RG}),
        }],
    )
    .await
    .unwrap()
    .unwrap()
}

fn at(track: Uuid, rec: Option<Uuid>) -> AppliedTrack {
    AppliedTrack {
        track_id: track,
        matched: true,
        disc: Some(1),
        disc_total: Some(1),
        track_no: Some(1),
        track_total: Some(2),
        title: Some("T".into()),
        artist: None,
        artist_sort: None,
        artist_mbids: vec![],
        recording_mbid: rec,
        release_track_mbid: None,
    }
}

fn applied(title: &str, artist: &str, year: Option<i32>, tracks: Vec<AppliedTrack>) -> Applied {
    Applied {
        album: AppliedAlbum {
            title: title.into(),
            artist: artist.into(),
            artist_sort: String::new(),
            artist_mbids: vec![ART_MBID],
            release_mbid: REL,
            release_group_mbid: RG,
            date: None,
            original_date: None,
            year,
            compilation: false,
            genres: vec!["Rock".into(), "Shoegaze".into()],
            disc_total: 1,
        },
        tracks,
        mbids_only: false,
    }
}

fn keys(a: &Applied) -> IdentityKeys {
    IdentityKeys {
        artist_key: key(&a.album.artist),
        title_key: key(&a.album.title),
    }
}

async fn run(pool: &PgPool, album: Uuid, a: Applied) -> RunRow {
    let cand = candidate(pool, album).await;
    let mut conn = pool.acquire().await.unwrap();
    create_run(
        &mut conn,
        &NewRun {
            id: Uuid::now_v7(),
            album_id: album,
            candidate_id: cand,
            applied: a,
            target: Target::Db,
            reorganize: false,
            created_by: None,
        },
    )
    .await
    .unwrap()
}

async fn apply(pool: &PgPool, run: &RunRow) -> Result<kima_db::autotag::ArtistStep, IdentityError> {
    let a: Applied = serde_json::from_value(run.applied.clone().unwrap()).unwrap();
    let mut tx = pool.begin().await.unwrap();
    let r = apply_identity(&mut tx, run, &keys(&a)).await;
    if r.is_ok() {
        tx.commit().await.unwrap();
    }
    r
}

async fn undo(pool: &PgPool, run_id: Uuid) -> Result<(), IdentityError> {
    let run = get_run(pool, run_id).await.unwrap().unwrap();
    let mut tx = pool.begin().await.unwrap();
    let r = undo_identity(&mut tx, &run).await;
    if r.is_ok() {
        tx.commit().await.unwrap();
    }
    r
}

/// The album row's identity, MBIDs and sources, and its artist.
async fn row(pool: &PgPool, album: Uuid) -> Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object('title', al.title, 'titleKey', al.title_key, 'year', al.year,
                'comp', al.is_compilation, 'rel', al.release_mbid, 'relSrc', al.release_mbid_source,
                'rg', al.release_group_mbid, 'rgSrc', al.release_group_mbid_source,
                'idSrc', al.identity_source, 'applied', al.applied_identity,
                'artistId', al.artist_id, 'artist', ar.name)
         FROM albums al JOIN artists ar ON ar.id = al.artist_id WHERE al.id = $1",
    )
    .bind(album)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn mb_genres(pool: &PgPool, album: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT g.name FROM album_genres ag JOIN genres g ON g.id = ag.genre_id
         WHERE ag.album_id = $1 AND ag.source = 'musicbrainz' ORDER BY ag.rank",
    )
    .bind(album)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn artist_row(pool: &PgPool, id: Uuid) -> Option<Value> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object('name', name, 'key', name_key, 'sort', sort_name, 'bio', bio,
                'image', image_path, 'mbid', mbid, 'mbidSrc', mbid_source,
                'status', enrichment_status, 'search', search::text)
         FROM artists WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn album_state(pool: &PgPool, album: Uuid) -> (String, Option<Uuid>) {
    sqlx::query_as("SELECT state, chosen_candidate FROM autotag_albums WHERE album_id = $1")
        .bind(album)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, $2, 'x')")
        .bind(id)
        .bind(id.to_string())
        .execute(pool)
        .await
        .unwrap();
    id
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_id_stable_and_user_state_survives(pool: PgPool) {
    let x = artist(&pool, "Old Artist").await;
    let b = album(&pool, x, "Old Title", Some(1999)).await;
    let t1 = track(&pool, b, "a/1.flac", false).await;
    let _t2 = track(&pool, b, "a/2.flac", false).await;
    sqlx::query(
        "UPDATE albums SET display_title = 'Mine', release_mbid = $2, release_mbid_source = 'tag'
         WHERE id = $1",
    )
    .bind(b)
    .bind(REC2)
    .execute(&pool)
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    kima_db::genres::replace_album_genres(
        &mut conn,
        b,
        kima_db::genres::GenreSource::MusicBrainz,
        &["Jazz".into()],
    )
    .await
    .unwrap();
    drop(conn);
    let u = user(&pool).await;
    sqlx::query("INSERT INTO stars (id, user_id, album_id) VALUES ($1, $2, $3)")
        .bind(Uuid::now_v7())
        .bind(u)
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    let pl = Uuid::now_v7();
    sqlx::query("INSERT INTO playlists (id, user_id, name) VALUES ($1, $2, 'p')")
        .bind(pl)
        .bind(u)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO playlist_items (id, playlist_id, track_id, sort) VALUES ($1, $2, $3, 0)",
    )
    .bind(Uuid::now_v7())
    .bind(pl)
    .bind(t1)
    .execute(&pool)
    .await
    .unwrap();
    let before = row(&pool, b).await;

    let r = run(
        &pool,
        b,
        applied("New Title", "Old Artist", Some(2001), vec![]),
    )
    .await;
    assert_eq!(
        album_state(&pool, b).await,
        ("applying".into(), r.candidate_id)
    );
    let step = apply(&pool, &r).await.unwrap();
    assert_eq!(step.action, "none");

    let after = row(&pool, b).await;
    assert_eq!(after["title"], "New Title");
    assert_eq!(after["titleKey"], "new title");
    assert_eq!(after["year"], 2001);
    assert_eq!(after["artistId"], json!(x));
    assert_eq!(after["rel"], json!(REL));
    assert_eq!(after["relSrc"], "autotag");
    assert_eq!(after["rg"], json!(RG));
    assert_eq!(after["rgSrc"], "autotag");
    assert_eq!(after["idSrc"], "autotag");
    assert_eq!(
        after["applied"],
        json!({"artistKey": "old artist", "titleKey": "new title", "year": 2001})
    );
    assert_eq!(mb_genres(&pool, b).await, vec!["Rock", "Shoegaze"]);
    let stored = get_run(&pool, r.id).await.unwrap().unwrap();
    let ab = stored.album_before.unwrap();
    assert_eq!(ab["title"], "Old Title");
    assert_eq!(ab["year"], 1999);
    assert_eq!(ab["mbGenres"], json!(["Jazz"]));
    assert_eq!(stored.artist_action.as_deref(), Some("none"));

    let survives = |pool: PgPool| async move {
        let n: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM stars WHERE album_id = $1)
                  + (SELECT count(*) FROM playlist_items WHERE track_id = $2)
                  + (SELECT count(*) FROM autotag_candidates WHERE album_id = $1)
                  + (SELECT count(*) FROM albums WHERE id = $1 AND display_title = 'Mine')",
        )
        .bind(b)
        .bind(t1)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 4);
    };
    survives(pool.clone()).await;

    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await, before);
    assert_eq!(mb_genres(&pool, b).await, vec!["Jazz"]);
    assert_eq!(album_state(&pool, b).await.0, "proposed");
    survives(pool.clone()).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_reparented_to_existing(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let y = artist(&pool, "Yolanda").await;
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = 'tag' WHERE id = $1")
        .bind(y)
        .bind(REC1)
        .execute(&pool)
        .await
        .unwrap();
    let b = album(&pool, x, "Record", None).await;
    album(&pool, y, "Other", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let before = row(&pool, b).await;
    let x_before = artist_row(&pool, x).await;

    let r = run(&pool, b, applied("Record", "Yolanda", None, vec![])).await;
    let step = apply(&pool, &r).await.unwrap();
    assert_eq!(step.action, "reparented");
    assert_eq!(step.old_artist_id, Some(x));
    assert_eq!(step.new_artist_id, Some(y));
    assert_eq!(row(&pool, b).await["artistId"], json!(y));
    let yr = artist_row(&pool, y).await.unwrap();
    assert_eq!(yr["mbid"], json!(ART_MBID));
    assert_eq!(yr["mbidSrc"], "autotag");
    let stored = get_run(&pool, r.id).await.unwrap().unwrap();
    assert_eq!(stored.artist_action.as_deref(), Some("reparented"));
    assert_eq!(stored.old_artist_id, Some(x));
    assert_eq!(stored.new_artist_id, Some(y));

    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await, before);
    assert_eq!(artist_row(&pool, x).await, x_before);
    let yr = artist_row(&pool, y).await.unwrap();
    assert_eq!(yr["mbid"], json!(REC1));
    assert_eq!(yr["mbidSrc"], "tag");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_renamed_in_place_when_only_album(pool: PgPool) {
    let x = artist(&pool, "The Olds").await;
    sqlx::query("UPDATE artists SET bio = 'bio', image_path = 'x.jpg' WHERE id = $1")
        .bind(x)
        .execute(&pool)
        .await
        .unwrap();
    let b = album(&pool, x, "Record", Some(2000)).await;
    track(&pool, b, "o/1.flac", false).await;
    track(&pool, b, "o/2.flac", true).await;
    let before = row(&pool, b).await;
    let x_before = artist_row(&pool, x).await.unwrap();

    let r = run(
        &pool,
        b,
        applied("Record", "The Newcomers", Some(2000), vec![]),
    )
    .await;
    let step = apply(&pool, &r).await.unwrap();
    assert_eq!(step.action, "renamed");
    assert_eq!(step.old_artist_name.as_deref(), Some("The Olds"));
    assert_eq!(step.new_artist_id, Some(x));
    let xr = artist_row(&pool, x).await.unwrap();
    assert_eq!(xr["name"], "The Newcomers");
    assert_eq!(xr["key"], "the newcomers");
    assert_eq!(xr["sort"], "newcomers");
    assert_eq!(xr["bio"], "bio");
    assert_eq!(xr["image"], "x.jpg");
    assert_eq!(xr["mbid"], json!(ART_MBID));
    assert!(xr["search"].as_str().unwrap().contains("newcomers"));
    assert_eq!(row(&pool, b).await["artistId"], json!(x));
    let stored = get_run(&pool, r.id).await.unwrap().unwrap();
    assert_eq!(stored.old_artist_name.as_deref(), Some("The Olds"));

    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await, before);
    assert_eq!(artist_row(&pool, x).await.unwrap(), x_before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_created_when_old_has_other_albums(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    // The other album has only a missing track: it still counts.
    let c = album(&pool, x, "Other", None).await;
    track(&pool, c, "x/o.flac", true).await;
    let before = row(&pool, b).await;

    let r = run(&pool, b, applied("Record", "Zed", None, vec![])).await;
    let step = apply(&pool, &r).await.unwrap();
    assert_eq!(step.action, "created");
    let z = step.new_artist_id.unwrap();
    assert_ne!(z, x);
    let zr = artist_row(&pool, z).await.unwrap();
    assert_eq!(zr["name"], "Zed");
    assert_eq!(zr["status"], "pending");
    assert_eq!(row(&pool, b).await["artistId"], json!(z));
    assert_eq!(artist_row(&pool, x).await.unwrap()["name"], "Xavier");

    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await, before);
    let pruned = library::prune_empty(&pool).await.unwrap();
    assert_eq!(pruned.artists, vec![z]);
    assert!(artist_row(&pool, x).await.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_follows_the_rules_when_the_old_artist_is_gone(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let y = artist(&pool, "Yolanda").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let other = album(&pool, y, "Other", None).await;
    track(&pool, other, "y/1.flac", false).await;
    let r = run(&pool, b, applied("Record", "Yolanda", None, vec![])).await;
    assert_eq!(apply(&pool, &r).await.unwrap().action, "reparented");
    assert_eq!(library::prune_empty(&pool).await.unwrap().artists, vec![x]);
    assert_eq!(
        get_run(&pool, r.id).await.unwrap().unwrap().old_artist_id,
        None
    );

    // Yolanda keeps another album: a row for the old name is created.
    undo(&pool, r.id).await.unwrap();
    let after = row(&pool, b).await;
    assert_eq!(after["artist"], "Xavier");
    assert_ne!(after["artistId"], json!(y));
    assert_ne!(after["artistId"], json!(x));
    assert_eq!(after["title"], "Record");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_renames_back_or_reparents_to_the_name_holder(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let r = run(&pool, b, applied("Record", "Zed", None, vec![])).await;
    assert_eq!(apply(&pool, &r).await.unwrap().action, "renamed");
    // Meanwhile another row took the old name.
    let holder = artist(&pool, "Xavier").await;
    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await["artistId"], json!(holder));
    assert_eq!(artist_row(&pool, x).await.unwrap()["name"], "Zed");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn collision_refused_under_lock(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let y = artist(&pool, "Yolanda").await;
    let b = album(&pool, x, "Record", Some(1990)).await;
    let t = track(&pool, b, "x/1.flac", false).await;
    let r = run(
        &pool,
        b,
        applied("Taken", "Yolanda", Some(2001), vec![at(t, Some(REC1))]),
    )
    .await;
    let a: Applied = serde_json::from_value(r.applied.clone().unwrap()).unwrap();
    // The request-time check sees no collision; the album appears before the transaction.
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        find_collision(&mut conn, b, &keys(&a), Some(2001))
            .await
            .unwrap(),
        None
    );
    drop(conn);
    let other = album(&pool, y, "Taken", Some(2001)).await;
    let before = row(&pool, b).await;

    let err = apply(&pool, &r).await.unwrap_err();
    let IdentityError::Collision(c) = err else {
        panic!("expected a collision, got {err:?}");
    };
    assert_eq!(c.album_id, other);
    assert_eq!(
        c.message(),
        "Another album already has this identity: Yolanda \u{2013} Taken (2001)"
    );
    assert_eq!(row(&pool, b).await, before);
    let stored = get_run(&pool, r.id).await.unwrap().unwrap();
    assert!(stored.album_before.is_none());
    let rec: Option<Uuid> = sqlx::query_scalar("SELECT recording_mbid FROM tracks WHERE id = $1")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rec, None);

    // Without a year the " (<year>)" part is left out; the album's own year counts when the
    // applied year is unknown.
    let no_year = ConflictAlbum {
        album_id: other,
        artist: "A".into(),
        title: "T".into(),
        year: None,
    };
    assert_eq!(
        no_year.message(),
        "Another album already has this identity: A \u{2013} T"
    );
    let mut conn = pool.acquire().await.unwrap();
    let k = IdentityKeys {
        artist_key: "yolanda".into(),
        title_key: "taken".into(),
    };
    sqlx::query("UPDATE albums SET year = 2001 WHERE id = $1")
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        find_collision(&mut conn, b, &k, None)
            .await
            .unwrap()
            .map(|c| c.album_id),
        Some(other)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_refused_when_old_identity_taken(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let y = artist(&pool, "Yolanda").await;
    let b = album(&pool, x, "Record", Some(1990)).await;
    let t = track(&pool, b, "x/1.flac", false).await;
    album(&pool, y, "Keep", None).await;
    album(&pool, x, "Keep X", None).await;
    let r = run(
        &pool,
        b,
        applied("Fresh", "Yolanda", Some(2001), vec![at(t, Some(REC1))]),
    )
    .await;
    apply(&pool, &r).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let stored = get_run(&pool, r.id).await.unwrap().unwrap();
    assert_eq!(undo_collision(&mut conn, &stored).await.unwrap(), None);
    drop(conn);

    let taker = album(&pool, x, "Record", Some(1990)).await;
    let applied_row = row(&pool, b).await;
    let mut conn = pool.acquire().await.unwrap();
    let c = undo_collision(&mut conn, &stored).await.unwrap().unwrap();
    drop(conn);
    assert_eq!(c.album_id, taker);
    assert_eq!(
        c.message(),
        "Another album already has this identity: Xavier \u{2013} Record (1990)"
    );
    let err = undo(&pool, r.id).await.unwrap_err();
    assert!(
        matches!(err, IdentityError::Collision(ref c) if c.album_id == taker),
        "{err:?}"
    );
    assert_eq!(row(&pool, b).await, applied_row);
    let rec: Option<String> =
        sqlx::query_scalar("SELECT recording_source FROM tracks WHERE id = $1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rec.as_deref(), Some("autotag"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_mbid_kept(pool: PgPool) {
    let user_rel = Uuid::now_v7();
    let user_art = Uuid::now_v7();
    let tag_rg = Uuid::now_v7();
    let x = artist(&pool, "Xavier").await;
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = 'user' WHERE id = $1")
        .bind(x)
        .bind(user_art)
        .execute(&pool)
        .await
        .unwrap();
    let b = album(&pool, x, "Record", None).await;
    let t = track(&pool, b, "x/1.flac", false).await;
    sqlx::query(
        "UPDATE albums SET release_mbid = $2, release_mbid_source = 'user',
                release_group_mbid = $3, release_group_mbid_source = 'tag' WHERE id = $1",
    )
    .bind(b)
    .bind(user_rel)
    .bind(tag_rg)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE tracks SET tag_release_mbid = $2, tag_release_group_mbid = $3 WHERE id = $1",
    )
    .bind(t)
    .bind(Uuid::now_v7())
    .bind(tag_rg)
    .execute(&pool)
    .await
    .unwrap();
    let r = run(&pool, b, applied("Record", "Xavier", None, vec![])).await;
    apply(&pool, &r).await.unwrap();
    let after = row(&pool, b).await;
    assert_eq!(after["rel"], json!(user_rel));
    assert_eq!(after["relSrc"], "user");
    assert_eq!(after["rg"], json!(RG));
    assert_eq!(after["rgSrc"], "autotag");
    let xr = artist_row(&pool, x).await.unwrap();
    assert_eq!(xr["mbid"], json!(user_art));
    assert_eq!(xr["mbidSrc"], "user");

    // A later tag refresh keeps user > autotag > tag.
    let mut conn = pool.acquire().await.unwrap();
    library::refresh_album_mbids(&mut conn, b).await.unwrap();
    drop(conn);
    assert_eq!(row(&pool, b).await, after);

    undo(&pool, r.id).await.unwrap();
    let undone = row(&pool, b).await;
    assert_eq!(undone["rel"], json!(user_rel));
    assert_eq!(undone["relSrc"], "user");
    assert_eq!(undone["rg"], json!(tag_rg));
    assert_eq!(undone["rgSrc"], "tag");
    assert_eq!(artist_row(&pool, x).await.unwrap()["mbidSrc"], "user");
}

async fn stage(pool: &PgPool, t: Uuid) -> Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object('status', status, 'source', source, 'evidence', evidence,
                'error', last_error, 'finished', finished_at IS NOT NULL)
         FROM track_stages WHERE track_id = $1 AND stage = 'recording'",
    )
    .bind(t)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn recording(pool: &PgPool, t: Uuid) -> (Option<Uuid>, Option<String>) {
    sqlx::query_as("SELECT recording_mbid, recording_source FROM tracks WHERE id = $1")
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recording_stage_closed_and_restored(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    let t1 = track(&pool, b, "x/1.flac", false).await;
    let t2 = track(&pool, b, "x/2.flac", false).await;
    let old = Uuid::now_v7();
    sqlx::query(
        "UPDATE tracks SET recording_mbid = $2, recording_source = 'acoustid' WHERE id = $1",
    )
    .bind(t1)
    .bind(old)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, source, evidence, finished_at)
         VALUES ($1, 'recording', 'done', 'acoustid', '{\"score\":0.9}', now()),
                ($2, 'recording', 'failed', NULL, NULL, now())",
    )
    .bind(t1)
    .bind(t2)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE track_stages SET last_error = 'boom' WHERE track_id = $1")
        .bind(t2)
        .execute(&pool)
        .await
        .unwrap();
    let s1_before = stage(&pool, t1).await;

    let r = run(
        &pool,
        b,
        applied(
            "Record",
            "Xavier",
            None,
            vec![at(t1, Some(REC1)), at(t2, Some(REC2))],
        ),
    )
    .await;
    apply(&pool, &r).await.unwrap();
    assert_eq!(
        recording(&pool, t1).await,
        (Some(REC1), Some("autotag".into()))
    );
    assert_eq!(
        recording(&pool, t2).await,
        (Some(REC2), Some("autotag".into()))
    );
    for t in [t1, t2] {
        assert_eq!(
            stage(&pool, t).await,
            json!({"status": "done", "source": "autotag", "evidence": {"reason": "autotag"},
                   "error": null, "finished": true})
        );
    }

    undo(&pool, r.id).await.unwrap();
    assert_eq!(
        recording(&pool, t1).await,
        (Some(old), Some("acoustid".into()))
    );
    assert_eq!(stage(&pool, t1).await, s1_before);
    assert_eq!(recording(&pool, t2).await, (None, None));
    let s2 = stage(&pool, t2).await;
    assert_eq!(s2["status"], "pending");
    assert_eq!(s2["error"], Value::Null);
    assert_eq!(s2["finished"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mbids_only_leaves_identity(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", Some(1990)).await;
    let t = track(&pool, b, "x/1.flac", false).await;
    let before = row(&pool, b).await;
    let mut a = applied("Record", "Xavier", Some(2005), vec![at(t, Some(REC1))]);
    a.mbids_only = true;
    a.album.genres = vec![];
    let r = run(&pool, b, a).await;
    let step = apply(&pool, &r).await.unwrap();
    assert_eq!(step.action, "none");
    let after = row(&pool, b).await;
    for k in [
        "title", "titleKey", "year", "comp", "idSrc", "applied", "artistId", "artist",
    ] {
        assert_eq!(after[k], before[k], "{k}");
    }
    assert_eq!(after["rel"], json!(REL));
    assert_eq!(after["relSrc"], "autotag");
    assert_eq!(
        recording(&pool, t).await,
        (Some(REC1), Some("autotag".into()))
    );
    assert_eq!(
        get_run(&pool, r.id)
            .await
            .unwrap()
            .unwrap()
            .artist_action
            .as_deref(),
        Some("none")
    );
    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await, before);
    assert_eq!(recording(&pool, t).await, (None, None));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn previewed_organizer_run_superseded(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let org = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO organizer_runs (id, mode, state, source_root, target_root) \
         VALUES ($1, 'move', 'previewed', '/a', '/a')",
    )
    .bind(org)
    .execute(&pool)
    .await
    .unwrap();
    let r = run(&pool, b, applied("New", "Xavier", None, vec![])).await;
    apply(&pool, &r).await.unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM organizer_runs WHERE id = $1")
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "superseded");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn overrides_put_and_deleted(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    let t1 = track(&pool, b, "x/1.flac", false).await;
    let t2 = track(&pool, b, "x/2.flac", false).await;
    let r = run(&pool, b, applied("New", "Xavier", None, vec![])).await;
    let mut conn = pool.acquire().await.unwrap();
    put_overrides(
        &mut conn,
        r.id,
        &[(t1, json!({"album": "Old"})), (t2, json!({"album": "New"}))],
    )
    .await
    .unwrap();
    // A second put replaces the fields.
    put_overrides(&mut conn, r.id, &[(t1, json!({"album": "New"}))])
        .await
        .unwrap();
    drop(conn);
    let paths = vec!["x/1.flac".to_string(), "x/2.flac".to_string()];
    let o = overrides_for_paths(&pool, &paths).await.unwrap();
    assert_eq!(o["x/1.flac"], json!({"album": "New"}));
    assert_eq!(o["x/2.flac"], json!({"album": "New"}));

    apply(&pool, &r).await.unwrap();
    undo(&pool, r.id).await.unwrap();
    assert!(overrides_for_paths(&pool, &paths).await.unwrap().is_empty());

    let mut conn = pool.acquire().await.unwrap();
    put_overrides(&mut conn, r.id, &[(t1, json!({}))])
        .await
        .unwrap();
    assert_eq!(delete_overrides(&mut conn, r.id).await.unwrap(), 1);
    assert_eq!(delete_overrides(&mut conn, r.id).await.unwrap(), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn run_lifecycle(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let r = run(&pool, b, applied("New", "Xavier", None, vec![])).await;
    assert_eq!(r.state, "queued");
    assert_eq!(r.target, "db");
    assert_eq!(
        active_run(&pool, b).await.unwrap().map(|r| r.id),
        Some(r.id)
    );
    assert!(
        transition_run(&pool, r.id, &[RunState::Queued], RunState::Writing)
            .await
            .unwrap()
    );
    assert!(
        !transition_run(&pool, r.id, &[RunState::Queued], RunState::Writing)
            .await
            .unwrap()
    );
    finish_run(&pool, r.id, RunState::Done, &json!({"written": 0}), None)
        .await
        .unwrap();
    let done = get_run(&pool, r.id).await.unwrap().unwrap();
    assert_eq!(done.state, "done");
    assert_eq!(done.summary, Some(json!({"written": 0})));
    assert!(done.finished_at.is_some());
    assert_eq!(active_run(&pool, b).await.unwrap().map(|r| r.id), None);
    assert_eq!(serde_json::to_value(Target::Files).unwrap(), "files");
    assert_eq!(
        serde_json::to_value(RunState::UndoFailed).unwrap(),
        "undo_failed"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apply_again_keeps_the_first_before(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let before = row(&pool, b).await;
    let r = run(&pool, b, applied("New", "Zed", None, vec![])).await;
    let first = apply(&pool, &r).await.unwrap();
    let second = apply(&pool, &r).await.unwrap();
    assert_eq!(first, second);
    let stored = get_run(&pool, r.id).await.unwrap().unwrap();
    assert_eq!(stored.album_before.unwrap()["title"], "Record");
    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await, before);
}

/// Waits (bounded) until a backend is blocked on a lock `holder` holds, or the job finished.
async fn wait_until_blocked_on<T>(pool: &PgPool, holder: i32, job: &tokio::task::JoinHandle<T>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                            WHERE wait_event_type = 'Lock' AND $1 = ANY(pg_blocking_pids(pid)))",
        )
        .bind(holder)
        .fetch_one(pool)
        .await
        .unwrap();
        if blocked || job.is_finished() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after 30 s: apply never waited on the row lock held by backend {holder}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn spawn_apply(
    pool: &PgPool,
    r: RunRow,
) -> tokio::task::JoinHandle<Result<kima_db::autotag::ArtistStep, IdentityError>> {
    let pool = pool.clone();
    tokio::spawn(async move { apply(&pool, &r).await })
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apply_waits_for_a_locked_track(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    let t = track(&pool, b, "x/1.flac", true).await;
    let r = run(&pool, b, applied("New", "Zed", None, vec![])).await;

    let mut lock = pool.begin().await.unwrap();
    let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM tracks WHERE id = $1 FOR NO KEY UPDATE")
        .bind(t)
        .execute(&mut *lock)
        .await
        .unwrap();
    let job = spawn_apply(&pool, r.clone());
    wait_until_blocked_on(&pool, holder, &job).await;
    assert!(
        !job.is_finished(),
        "apply did not wait for the (missing) track's lock"
    );
    // It waits before it takes the album: the album row is still free.
    let mut probe = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM albums WHERE id = $1 FOR NO KEY UPDATE NOWAIT")
        .bind(b)
        .execute(&mut *probe)
        .await
        .expect("apply holds the album while it waits for a track");
    probe.rollback().await.unwrap();
    lock.commit().await.unwrap();
    assert_eq!(job.await.unwrap().unwrap().action, "renamed");
    assert_eq!(row(&pool, b).await["title"], "New");
}

/// A scan holds a track row, then takes the album and the artist (track -> album -> artist).
/// Apply, started while the scan holds the track, waits for it instead of taking the album
/// first: the scan's later locks go through, nothing deadlocks.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apply_queues_behind_a_scan_holding_the_track(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    let t = track(&pool, b, "x/1.flac", false).await;
    let r = run(
        &pool,
        b,
        applied("New", "Zed", None, vec![at(t, Some(REC1))]),
    )
    .await;

    let mut scan = pool.begin().await.unwrap();
    let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *scan)
        .await
        .unwrap();
    sqlx::query("SELECT album_id FROM tracks WHERE id = $1 FOR NO KEY UPDATE")
        .bind(t)
        .execute(&mut *scan)
        .await
        .unwrap();
    let job = spawn_apply(&pool, r.clone());
    wait_until_blocked_on(&pool, holder, &job).await;
    // Bounded: a deadlock is detected after deadlock_timeout (1 s by default).
    let album_lock = tokio::time::timeout(
        Duration::from_secs(20),
        sqlx::query("SELECT id FROM albums WHERE id = $1 FOR NO KEY UPDATE")
            .bind(b)
            .execute(&mut *scan),
    )
    .await
    .expect("the scan's album lock never came back");
    album_lock.expect("the scan's album lock failed (deadlock)");
    sqlx::query("SELECT id FROM artists WHERE id = $1 FOR NO KEY UPDATE")
        .bind(x)
        .execute(&mut *scan)
        .await
        .unwrap();
    scan.commit().await.unwrap();
    assert_eq!(job.await.unwrap().unwrap().action, "renamed");
    assert_eq!(
        recording(&pool, t).await,
        (Some(REC1), Some("autotag".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_applied_year_leaves_the_year(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", Some(1995)).await;
    track(&pool, b, "x/1.flac", false).await;
    let r = run(&pool, b, applied("New", "Xavier", None, vec![])).await;
    apply(&pool, &r).await.unwrap();
    let after = row(&pool, b).await;
    assert_eq!(after["year"], 1995);
    assert_eq!(after["applied"]["year"], 1995);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_waits_for_a_locked_track_before_the_album(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    let t = track(&pool, b, "x/1.flac", true).await;
    let r = run(&pool, b, applied("New", "Zed", None, vec![])).await;
    apply(&pool, &r).await.unwrap();

    let mut lock = pool.begin().await.unwrap();
    let holder: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM tracks WHERE id = $1 FOR NO KEY UPDATE")
        .bind(t)
        .execute(&mut *lock)
        .await
        .unwrap();
    let job = {
        let pool = pool.clone();
        let id = r.id;
        tokio::spawn(async move { undo(&pool, id).await })
    };
    wait_until_blocked_on(&pool, holder, &job).await;
    assert!(
        !job.is_finished(),
        "undo did not wait for the (missing) track's lock"
    );
    let mut probe = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM albums WHERE id = $1 FOR NO KEY UPDATE NOWAIT")
        .bind(b)
        .execute(&mut *probe)
        .await
        .expect("undo holds the album while it waits for a track");
    probe.rollback().await.unwrap();
    lock.commit().await.unwrap();
    job.await.unwrap().unwrap();
    assert_eq!(row(&pool, b).await["title"], "Record");
}

async fn artist_mbid(pool: &PgPool, id: Uuid) -> (Option<Uuid>, Option<String>) {
    sqlx::query_as("SELECT mbid, mbid_source FROM artists WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn created_undo_leaves_the_old_artists_later_mbid(pool: PgPool) {
    let m2 = Uuid::now_v7();
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let c = album(&pool, x, "Second", None).await;
    track(&pool, c, "x/2.flac", false).await;
    let r1 = run(&pool, b, applied("Record", "Zed", None, vec![])).await;
    assert_eq!(apply(&pool, &r1).await.unwrap().action, "created");
    let mut a2 = applied("Second", "Xavier", None, vec![]);
    a2.album.artist_mbids = vec![m2];
    let r2 = run(&pool, c, a2).await;
    apply(&pool, &r2).await.unwrap();
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(m2), Some("autotag".into()))
    );

    undo(&pool, r1.id).await.unwrap();
    assert_eq!(row(&pool, b).await["artistId"], json!(x));
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(m2), Some("autotag".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_leaves_another_runs_mbid_on_a_shared_artist(pool: PgPool) {
    let m2 = Uuid::now_v7();
    let x = artist(&pool, "Xavier").await;
    let a = album(&pool, x, "First", None).await;
    track(&pool, a, "x/1.flac", false).await;
    let b = album(&pool, x, "Second", None).await;
    track(&pool, b, "x/2.flac", false).await;
    let r1 = run(&pool, a, applied("First!", "Xavier", None, vec![])).await;
    apply(&pool, &r1).await.unwrap();
    assert_eq!(artist_mbid(&pool, x).await.0, Some(ART_MBID));
    let mut a2 = applied("Second!", "Xavier", None, vec![]);
    a2.album.artist_mbids = vec![m2];
    let r2 = run(&pool, b, a2).await;
    apply(&pool, &r2).await.unwrap();

    undo(&pool, r1.id).await.unwrap();
    assert_eq!(row(&pool, a).await["title"], "First");
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(m2), Some("autotag".into()))
    );
    // Run 2's own undo still gives back what it found (run 1's value).
    undo(&pool, r2.id).await.unwrap();
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(ART_MBID), Some("autotag".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_edits_after_apply_survive_undo(pool: PgPool) {
    let (urel, urg, uart, arec) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    let t = track(&pool, b, "x/1.flac", false).await;
    let r = run(
        &pool,
        b,
        applied("New", "Xavier", None, vec![at(t, Some(REC1))]),
    )
    .await;
    apply(&pool, &r).await.unwrap();
    sqlx::query(
        "UPDATE albums SET release_mbid = $2, release_mbid_source = 'user',
                release_group_mbid = $3, release_group_mbid_source = 'user' WHERE id = $1",
    )
    .bind(b)
    .bind(urel)
    .bind(urg)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = 'user' WHERE id = $1")
        .bind(x)
        .bind(uart)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE tracks SET recording_mbid = $2, recording_source = 'acoustid' WHERE id = $1",
    )
    .bind(t)
    .bind(arec)
    .execute(&pool)
    .await
    .unwrap();

    undo(&pool, r.id).await.unwrap();
    let after = row(&pool, b).await;
    assert_eq!(after["title"], "Record");
    assert_eq!(
        (&after["rel"], &after["relSrc"]),
        (&json!(urel), &json!("user"))
    );
    assert_eq!(
        (&after["rg"], &after["rgSrc"]),
        (&json!(urg), &json!("user"))
    );
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(uart), Some("user".into()))
    );
    assert_eq!(
        recording(&pool, t).await,
        (Some(arec), Some("acoustid".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_leaves_a_later_runs_album_and_recording_mbids(pool: PgPool) {
    let (rel2, rg2) = (Uuid::now_v7(), Uuid::now_v7());
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    let t = track(&pool, b, "x/1.flac", false).await;
    let r1 = run(
        &pool,
        b,
        applied("Record", "Xavier", None, vec![at(t, Some(REC1))]),
    )
    .await;
    apply(&pool, &r1).await.unwrap();
    let mut a2 = applied("Record", "Xavier", None, vec![at(t, Some(REC2))]);
    a2.album.release_mbid = rel2;
    a2.album.release_group_mbid = rg2;
    let r2 = run(&pool, b, a2).await;
    apply(&pool, &r2).await.unwrap();

    undo(&pool, r1.id).await.unwrap();
    let after = row(&pool, b).await;
    assert_eq!(
        (&after["rel"], &after["relSrc"]),
        (&json!(rel2), &json!("autotag"))
    );
    assert_eq!(
        (&after["rg"], &after["rgSrc"]),
        (&json!(rg2), &json!("autotag"))
    );
    assert_eq!(
        recording(&pool, t).await,
        (Some(REC2), Some("autotag".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_artist_is_renamed_into_various_artists(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Hits", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let mut a = applied("Hits", "Various Artists", None, vec![]);
    a.album.compilation = true;
    a.album.artist_mbids = vec![library::VARIOUS_ARTISTS_MBID];
    let r = run(&pool, b, a).await;
    let step = apply(&pool, &r).await.unwrap();
    assert_eq!(step.action, "created");
    let va = step.new_artist_id.unwrap();
    assert_ne!(va, x);
    let vr = artist_row(&pool, va).await.unwrap();
    assert_eq!(vr["key"], "various artists");
    assert_eq!(vr["mbid"], json!(library::VARIOUS_ARTISTS_MBID));
    assert_eq!(vr["status"], "completed");
    assert_eq!(artist_row(&pool, x).await.unwrap()["name"], "Xavier");
    assert_eq!(row(&pool, b).await["comp"], true);
    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await["artistId"], json!(x));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn various_artists_is_never_renamed(pool: PgPool) {
    let va = artist(&pool, "Various Artists").await;
    sqlx::query("UPDATE artists SET mbid = $2 WHERE id = $1")
        .bind(va)
        .bind(library::VARIOUS_ARTISTS_MBID)
        .execute(&pool)
        .await
        .unwrap();
    let b = album(&pool, va, "Hits", None).await;
    track(&pool, b, "va/1.flac", false).await;
    let r = run(&pool, b, applied("Hits", "Real Artist", None, vec![])).await;
    let step = apply(&pool, &r).await.unwrap();
    assert_eq!(step.action, "created");
    let vr = artist_row(&pool, va).await.unwrap();
    assert_eq!(vr["name"], "Various Artists");
    assert_eq!(vr["mbid"], json!(library::VARIOUS_ARTISTS_MBID));
    assert_eq!(row(&pool, b).await["artist"], "Real Artist");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn renamed_row_drops_the_various_artists_mbid(pool: PgPool) {
    let x = artist(&pool, "Comp Series").await;
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = 'tag' WHERE id = $1")
        .bind(x)
        .bind(library::VARIOUS_ARTISTS_MBID)
        .execute(&pool)
        .await
        .unwrap();
    let b = album(&pool, x, "Hits", None).await;
    track(&pool, b, "c/1.flac", false).await;
    let mut a = applied("Hits", "Someone", None, vec![]);
    a.album.artist_mbids = vec![];
    let r = run(&pool, b, a).await;
    assert_eq!(apply(&pool, &r).await.unwrap().action, "renamed");
    assert_eq!(artist_mbid(&pool, x).await, (None, None));
    undo(&pool, r.id).await.unwrap();
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(library::VARIOUS_ARTISTS_MBID), Some("tag".into()))
    );
    assert_eq!(artist_row(&pool, x).await.unwrap()["name"], "Comp Series");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_creates_the_old_name_when_the_renamed_row_gained_albums(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let r = run(&pool, b, applied("Record", "Zed", None, vec![])).await;
    assert_eq!(apply(&pool, &r).await.unwrap().action, "renamed");
    assert_eq!(artist_mbid(&pool, x).await.0, Some(ART_MBID));
    // A scan put another Zed album on the renamed row.
    let z2 = album(&pool, x, "Zed Live", None).await;
    track(&pool, z2, "z/1.flac", false).await;
    undo(&pool, r.id).await.unwrap();
    let after = row(&pool, b).await;
    assert_eq!(after["artist"], "Xavier");
    assert_ne!(after["artistId"], json!(x));
    assert_eq!(artist_row(&pool, x).await.unwrap()["name"], "Zed");
    assert_eq!(row(&pool, z2).await["artistId"], json!(x));
    // The renamed row keeps the MBID the apply gave its new identity; the old name had none.
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(ART_MBID), Some("autotag".into()))
    );
    let created: Uuid = serde_json::from_value(after["artistId"].clone()).unwrap();
    assert_eq!(artist_mbid(&pool, created).await, (None, None));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mbids_only_leaves_the_artist_row(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let x_before = artist_row(&pool, x).await;
    let mut a = applied("Record", "Xavier", None, vec![]);
    a.mbids_only = true;
    let r = run(&pool, b, a).await;
    apply(&pool, &r).await.unwrap();
    assert_eq!(artist_row(&pool, x).await, x_before);
    assert_eq!(row(&pool, b).await["relSrc"], "autotag");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_gives_the_old_mbid_to_the_recreated_old_name(pool: PgPool) {
    let old = Uuid::now_v7();
    let x = artist(&pool, "Xavier").await;
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = 'tag' WHERE id = $1")
        .bind(x)
        .bind(old)
        .execute(&pool)
        .await
        .unwrap();
    let b = album(&pool, x, "Record", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let r = run(&pool, b, applied("Record", "Zed", None, vec![])).await;
    assert_eq!(apply(&pool, &r).await.unwrap().action, "renamed");
    let z2 = album(&pool, x, "Zed Live", None).await;
    track(&pool, z2, "z/1.flac", false).await;

    undo(&pool, r.id).await.unwrap();
    let after = row(&pool, b).await;
    let created: Uuid = serde_json::from_value(after["artistId"].clone()).unwrap();
    assert_ne!(created, x);
    assert_eq!(after["artist"], "Xavier");
    assert_eq!(
        artist_mbid(&pool, created).await,
        (Some(old), Some("tag".into()))
    );
    assert_eq!(artist_row(&pool, x).await.unwrap()["name"], "Zed");
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(ART_MBID), Some("autotag".into()))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_never_moves_the_various_artists_mbid_to_the_renamed_row(pool: PgPool) {
    let m = Uuid::now_v7();
    let x = artist(&pool, "Comp Series").await;
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = 'tag' WHERE id = $1")
        .bind(x)
        .bind(library::VARIOUS_ARTISTS_MBID)
        .execute(&pool)
        .await
        .unwrap();
    let b = album(&pool, x, "Hits", None).await;
    track(&pool, b, "c/1.flac", false).await;
    let mut a = applied("Hits", "Someone", None, vec![]);
    a.album.artist_mbids = vec![m];
    let r = run(&pool, b, a).await;
    assert_eq!(apply(&pool, &r).await.unwrap().action, "renamed");
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(m), Some("autotag".into()))
    );
    let s2 = album(&pool, x, "Someone Else", None).await;
    track(&pool, s2, "s/1.flac", false).await;

    undo(&pool, r.id).await.unwrap();
    assert_eq!(artist_row(&pool, x).await.unwrap()["name"], "Someone");
    assert_eq!(
        artist_mbid(&pool, x).await,
        (Some(m), Some("autotag".into()))
    );
    let after = row(&pool, b).await;
    assert_eq!(after["artist"], "Comp Series");
    let created: Uuid = serde_json::from_value(after["artistId"].clone()).unwrap();
    assert_ne!(created, x);
    // Not a Various Artists name: the VA MBID is not put on it.
    assert_eq!(artist_mbid(&pool, created).await, (None, None));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undo_out_of_various_artists_after_the_old_row_was_pruned(pool: PgPool) {
    let x = artist(&pool, "Xavier").await;
    let b = album(&pool, x, "Hits", None).await;
    track(&pool, b, "x/1.flac", false).await;
    let mut a = applied("Hits", "Various Artists", None, vec![]);
    a.album.compilation = true;
    a.album.artist_mbids = vec![library::VARIOUS_ARTISTS_MBID];
    let r = run(&pool, b, a).await;
    let step = apply(&pool, &r).await.unwrap();
    assert_eq!(step.action, "created");
    let va = step.new_artist_id.unwrap();
    assert_eq!(library::prune_empty(&pool).await.unwrap().artists, vec![x]);

    // The VA row now holds only this album, yet it is never renamed: a row is created.
    undo(&pool, r.id).await.unwrap();
    let after = row(&pool, b).await;
    assert_eq!(after["artist"], "Xavier");
    assert_ne!(after["artistId"], json!(va));
    let vr = artist_row(&pool, va).await.unwrap();
    assert_eq!(vr["key"], "various artists");
    assert_eq!(vr["mbid"], json!(library::VARIOUS_ARTISTS_MBID));
    assert_eq!(after["comp"], false);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn various_artists_with_several_albums_keeps_them(pool: PgPool) {
    let va = artist(&pool, "Various Artists").await;
    sqlx::query("UPDATE artists SET mbid = $2 WHERE id = $1")
        .bind(va)
        .bind(library::VARIOUS_ARTISTS_MBID)
        .execute(&pool)
        .await
        .unwrap();
    let b = album(&pool, va, "Hits", None).await;
    track(&pool, b, "va/1.flac", false).await;
    let other = album(&pool, va, "More Hits", None).await;
    track(&pool, other, "va/2.flac", false).await;
    let before = row(&pool, b).await;
    let r = run(&pool, b, applied("Hits", "Real Artist", None, vec![])).await;
    assert_eq!(apply(&pool, &r).await.unwrap().action, "created");
    let vr = artist_row(&pool, va).await.unwrap();
    assert_eq!(vr["name"], "Various Artists");
    assert_eq!(vr["mbid"], json!(library::VARIOUS_ARTISTS_MBID));
    assert_eq!(row(&pool, other).await["artistId"], json!(va));
    undo(&pool, r.id).await.unwrap();
    assert_eq!(row(&pool, b).await, before);
    assert_eq!(
        artist_row(&pool, va).await.unwrap()["mbid"],
        json!(library::VARIOUS_ARTISTS_MBID)
    );
}
