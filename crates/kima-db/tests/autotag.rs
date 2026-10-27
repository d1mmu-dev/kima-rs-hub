use kima_db::PgPool;
use kima_db::autotag::{
    AlbumState, NewCandidate, Origin, Recommendation, candidate, candidates, local_album,
    set_state, store_search, unmatched_albums,
};
use serde_json::json;
use uuid::Uuid;

async fn artist(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'Art', $2)")
        .bind(id)
        .bind(id.to_string())
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn album(pool: &PgPool) -> Uuid {
    let artist_id = artist(pool).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 'Alb', $3)")
        .bind(id)
        .bind(artist_id)
        .bind(id.to_string())
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn track(pool: &PgPool, album: Uuid, disc: i32, no: i32, path: &str, missing: bool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, disc, track_no, codec, path, size, \
         mtime, fingerprint, missing_since) \
         VALUES ($1, $2, 'a', $6, $3, $4, 'flac', $5, 1, now(), '\\x00', \
         CASE WHEN $7 THEN now() END)",
    )
    .bind(id)
    .bind(album)
    .bind(disc)
    .bind(no)
    .bind(path)
    .bind(path)
    .bind(missing)
    .execute(pool)
    .await
    .unwrap();
    id
}

fn cand(release: Uuid, rec: Recommendation, distance: f64) -> NewCandidate {
    NewCandidate {
        release_mbid: release,
        release_group_mbid: Uuid::new_v4(),
        origin: Origin::Search,
        distance,
        recommendation: rec,
        penalties: json!({"album": 0.0}),
        mapping: json!([]),
        extra_tracks: json!([]),
        missing_tracks: json!([]),
        release: json!({"id": release}),
        release_group: json!({}),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn migration_constraints(pool: PgPool) {
    let a = album(&pool).await;
    let bogus = sqlx::query("INSERT INTO autotag_albums (album_id, state) VALUES ($1, 'bogus')")
        .bind(a)
        .execute(&pool)
        .await;
    assert!(bogus.is_err());

    let bad = sqlx::query("UPDATE albums SET identity_source = 'autotag' WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await;
    assert!(bad.is_err());
    sqlx::query(
        "UPDATE albums SET identity_source = 'autotag', applied_identity = '{}'::jsonb WHERE id = $1",
    )
    .bind(a)
    .execute(&pool)
    .await
    .unwrap();

    let run = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO organizer_runs (id, mode, state, source_root, target_root) \
         VALUES ($1, 'copy', 'superseded', '/a', '/b')",
    )
    .bind(run)
    .execute(&pool)
    .await
    .unwrap();
    let origin: String = sqlx::query_scalar("SELECT origin FROM organizer_runs WHERE id = $1")
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(origin, "user");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn candidate_cycle_set_null(pool: PgPool) {
    let a = album(&pool).await;
    let best = store_search(
        &pool,
        a,
        &[cand(Uuid::new_v4(), Recommendation::Strong, 0.01)],
    )
    .await
    .unwrap()
    .unwrap();
    sqlx::query("DELETE FROM autotag_candidates WHERE id = $1")
        .bind(best)
        .execute(&pool)
        .await
        .unwrap();
    let b: Option<Uuid> =
        sqlx::query_scalar("SELECT best_candidate FROM autotag_albums WHERE album_id = $1")
            .bind(a)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(b.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn store_search_replaces_and_sets_best(pool: PgPool) {
    let a = album(&pool).await;
    let (r1, r2, r3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    store_search(
        &pool,
        a,
        &[
            cand(r1, Recommendation::Strong, 0.01),
            cand(r2, Recommendation::Medium, 0.1),
        ],
    )
    .await
    .unwrap();
    let first = candidates(&pool, a).await.unwrap();
    let r2_id = first.iter().find(|c| c.release_mbid == r2).unwrap().id;

    let best = store_search(
        &pool,
        a,
        &[
            cand(r2, Recommendation::Medium, 0.15),
            cand(r3, Recommendation::Weak, 0.5),
        ],
    )
    .await
    .unwrap();
    assert_eq!(best, Some(r2_id));
    let rows = candidates(&pool, a).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|c| c.release_mbid != r1));
    let kept = candidate(&pool, r2_id).await.unwrap().unwrap();
    assert_eq!(kept.distance, 0.15);

    let (state, rec, searched): (String, Option<String>, bool) = sqlx::query_as(
        "SELECT state, recommendation, searched_at IS NOT NULL FROM autotag_albums WHERE album_id = $1",
    )
    .bind(a)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "proposed");
    assert_eq!(rec.as_deref(), Some("medium"));
    assert!(searched);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn store_search_empty_is_no_candidates(pool: PgPool) {
    let a = album(&pool).await;
    set_state(&pool, a, AlbumState::Failed, Some("boom"))
        .await
        .unwrap();
    store_search(
        &pool,
        a,
        &[cand(Uuid::new_v4(), Recommendation::Strong, 0.01)],
    )
    .await
    .unwrap();
    let best = store_search(&pool, a, &[]).await.unwrap();
    assert!(best.is_none());
    assert!(candidates(&pool, a).await.unwrap().is_empty());
    let (state, rec, best, err): (String, Option<String>, Option<Uuid>, Option<String>) =
        sqlx::query_as(
            "SELECT state, recommendation, best_candidate, error FROM autotag_albums WHERE album_id = $1",
        )
        .bind(a)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "no_candidates");
    assert!(rec.is_none() && best.is_none() && err.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn local_album_present_tracks_in_order(pool: PgPool) {
    let a = album(&pool).await;
    let t2 = track(&pool, a, 1, 2, "/m/b", false).await;
    let t1 = track(&pool, a, 1, 1, "/m/a", false).await;
    let d2 = track(&pool, a, 2, 1, "/m/c", false).await;
    track(&pool, a, 1, 3, "/m/gone", true).await;
    let l = local_album(&pool, a).await.unwrap().unwrap();
    assert_eq!(l.title, "Alb");
    assert_eq!(l.artist_name, "Art");
    assert_eq!(l.identity_source, "scan");
    let ids: Vec<Uuid> = l.tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![t1, t2, d2]);
    assert!(local_album(&pool, Uuid::new_v4()).await.unwrap().is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unmatched_albums_skip_tagged_and_empty(pool: PgPool) {
    let open = album(&pool).await;
    track(&pool, open, 1, 1, "/m/open", false).await;
    let tagged = album(&pool).await;
    track(&pool, tagged, 1, 1, "/m/tagged", false).await;
    sqlx::query("UPDATE albums SET release_mbid = gen_random_uuid() WHERE id = $1")
        .bind(tagged)
        .execute(&pool)
        .await
        .unwrap();
    let gone = album(&pool).await;
    track(&pool, gone, 1, 1, "/m/gone", true).await;
    album(&pool).await;
    assert_eq!(unmatched_albums(&pool).await.unwrap(), vec![open]);
}
