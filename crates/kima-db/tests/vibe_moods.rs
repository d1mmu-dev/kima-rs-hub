//! Learned moods (M5 Task 11, spec 8.2), ported from upstream `moods.integration.test.ts` and
//! `moodSnapshot.integration.test.ts`, against a real Postgres with synthetic heads (the real heads are a private
//! asset, owner decision Q3).
mod vibe_common;
use kima_db::PgPool;
use kima_db::vibe::moods::{
    MoodBuild, MoodBuilder, MoodSnapshots, NO_STATISTICS, READINGS, load_mood_state, mood_progress,
    mood_state_version, update_moods,
};
use kima_vibe::mood_heads::{MOODS, MoodHead, MoodHeads};
use kima_vibe::moods::score;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use vibe_common::*;

fn heads_v(version: &str, scale: f64) -> Arc<MoodHeads> {
    Arc::new(MoodHeads {
        version: version.into(),
        model: "synthetic".into(),
        dims: 512,
        heads: (0..MOODS.len())
            .map(|k| MoodHead {
                weights: (0..512)
                    .map(|i| (((i * 7 + k * 13) % 17) as f64 / 17.0 - 0.5) * scale)
                    .collect(),
                bias: k as f64 * 0.05 - 0.5,
            })
            .collect(),
    })
}

fn heads() -> Arc<MoodHeads> {
    heads_v("synthetic-v1", 0.3)
}

/// A deterministic vector per seed, scaled to length `len` (the writer lets a stored vector's length differ from 1).
fn vector(seed: u64, len: f32) -> Vec<f32> {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut v: Vec<f32> = (0..512)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((x >> 33) as f64 / (1u64 << 31) as f64 * 2.0 - 1.0) as f32
        })
        .collect();
    let n = v.iter().map(|a| f64::from(*a).powi(2)).sum::<f64>().sqrt() as f32;
    v.iter_mut().for_each(|a| *a = *a / n * len);
    v
}

/// `n` tracks (one artist each), embedded under the gate's identity.
async fn library(pool: &PgPool, n: usize) -> (Vec<Uuid>, Vec<Vec<f32>>) {
    let mut ids = vec![];
    let mut vs = vec![];
    for i in 0..n {
        let s = track(pool).await;
        let v = vector(i as u64 + 1, 1.0);
        embed(pool, s.track, &v).await;
        ids.push(s.track);
        vs.push(v);
    }
    (ids, vs)
}

async fn scores(pool: &PgPool, id: Uuid) -> Vec<f64> {
    let cols = MOODS.map(|m| format!("\"{m}\"::float8")).join(", ");
    let row: sqlx::postgres::PgRow = sqlx::query(&format!(
        "SELECT {cols} FROM track_moods WHERE track_id = $1"
    ))
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    use sqlx::Row;
    (0..22).map(|i| row.get::<f64, _>(i)).collect()
}

async fn update(pool: &PgPool, heads: &kima_vibe::mood_heads::MoodHeads) -> u64 {
    let mut c = pool.acquire().await.unwrap();
    update_moods(&mut c, heads).await.unwrap()
}

async fn stored_state(pool: &PgPool) -> Option<kima_db::vibe::moods::MoodState> {
    let mut c = pool.acquire().await.unwrap();
    load_mood_state(&mut c, &heads()).await.unwrap()
}

async fn put_readings(
    pool: &PgPool,
    id: Uuid,
    instrumentalness: f32,
    acousticness: f32,
    arousal: f32,
) {
    sqlx::query(
        "INSERT INTO track_clap_readings (track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw)
         SELECT track_id, created_at, $2, $3, 0.5, $4 FROM track_embeddings WHERE track_id = $1
         ON CONFLICT (track_id) DO UPDATE SET instrumentalness = $2, acousticness = $3, arousal_raw = $4",
    )
    .bind(id)
    .bind(instrumentalness)
    .bind(acousticness)
    .bind(arousal)
    .execute(pool)
    .await
    .unwrap();
}

fn out(scored: u64, state_changed: bool) -> MoodBuild {
    MoodBuild {
        scored,
        state_changed,
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn scores_every_vector_of_the_heads_model_to_1e_4_of_the_folded_dot_product(pool: PgPool) {
    assert!(current_database(&pool).await.starts_with("_sqlx_test"));
    let h = heads();
    let (ids, mut vs) = library(&pool, 4).await;
    // A stored vector's length may differ from 1 slightly; the score divides by it.
    let long = track(&pool).await;
    let lv = vector(77, 1.0008);
    embed(&pool, long.track, &lv).await;
    // A vector of another checkpoint is never scored.
    let other = track(&pool).await;
    embed_as(&pool, other.track, "other", &[7; 32], "p", &vector(5, 1.0))
        .await
        .unwrap();
    let mut all_ids = ids;
    all_ids.push(long.track);
    vs.push(lv);

    assert_eq!(update(&pool, &h).await, 5);
    assert_eq!(count(&pool, "track_moods").await, 5);
    for (id, v) in all_ids.iter().zip(&vs) {
        let got = scores(&pool, *id).await;
        let e: Vec<f64> = v.iter().map(|x| f64::from(*x)).collect();
        for (k, head) in h.heads.iter().enumerate() {
            let want = score(head, &e);
            assert!(
                (got[k] - want).abs() < 1e-4,
                "{} {} vs {want}",
                MOODS[k],
                got[k]
            );
        }
    }
    let version: String = sqlx::query_scalar("SELECT DISTINCT weights FROM track_moods")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, h.version);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn leaves_current_rows_alone(pool: PgPool) {
    let h = heads();
    library(&pool, 3).await;
    assert_eq!(update(&pool, &h).await, 3);
    let before: Vec<i64> =
        sqlx::query_scalar("SELECT xmin::text::bigint FROM track_moods ORDER BY track_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(update(&pool, &h).await, 0);
    let after: Vec<i64> =
        sqlx::query_scalar("SELECT xmin::text::bigint FROM track_moods ORDER BY track_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(before, after, "no row was rewritten");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rescored_only_for_vector_analysed_again(pool: PgPool) {
    let h = heads();
    let (ids, _) = library(&pool, 3).await;
    update(&pool, &h).await;
    let kept = scores(&pool, ids[0]).await;
    embed(&pool, ids[1], &vector(900, 1.0)).await; // re-embedded: new created_at
    assert_eq!(update(&pool, &h).await, 1);
    assert_eq!(scores(&pool, ids[0]).await, kept);
    let e: Vec<f64> = vector(900, 1.0).iter().map(|x| f64::from(*x)).collect();
    let now = scores(&pool, ids[1]).await;
    assert!((now[0] - score(&h.heads[0], &e)).abs() < 1e-4);
    let stamp_matches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM track_moods m JOIN track_embeddings te ON te.track_id = m.track_id AND te.created_at = m.embedded_at",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stamp_matches, 3);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rescores_rows_from_other_weights_only_those(pool: PgPool) {
    let h = heads();
    let (ids, _) = library(&pool, 4).await;
    update(&pool, &h).await;
    sqlx::query("UPDATE track_moods SET weights = 'old', happy = 99 WHERE track_id = ANY($1)")
        .bind(&ids[..2])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(update(&pool, &h).await, 2);
    let fixed: Vec<f64> = scores(&pool, ids[0]).await;
    assert!(fixed[0] < 50.0, "the stale score was replaced");
    // New weights rescore every row.
    let h2 = heads_v("synthetic-v2", 0.31);
    assert_eq!(update(&pool, &h2).await, 4);
    let versions: Vec<String> = sqlx::query_scalar("SELECT DISTINCT weights FROM track_moods")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(versions, vec!["synthetic-v2".to_string()]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn loses_scores_with_the_vector(pool: PgPool) {
    let h = heads();
    let (ids, _) = library(&pool, 2).await;
    update(&pool, &h).await;
    sqlx::query("DELETE FROM track_embeddings WHERE track_id = $1")
        .bind(ids[0])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count(&pool, "track_moods").await, 1);
    // A changed file drops its vector (file-change trigger) and with it the scores.
    sqlx::query("UPDATE tracks SET size = size + 1 WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count(&pool, "track_moods").await, 0);
}

fn logit(p: f64) -> f64 {
    let p = p.clamp(0.01, 0.99);
    (p / (1.0 - p)).ln()
}

fn mean_sd(v: &[f64]) -> (f64, f64) {
    let m = v.iter().sum::<f64>() / v.len() as f64;
    (
        m,
        (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt(),
    )
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn statistics_hold_mean_and_population_sd(pool: PgPool) {
    let h = heads();
    let (ids, _) = library(&pool, 5).await;
    // Readings, including 0 and 1, which the logit clips to 0.01 / 0.99 to stay finite.
    let rs = [
        (0.0, 0.2, 0.9),
        (1.0, 0.4, 0.5),
        (0.3, 0.6, 0.1),
        (0.5, 0.8, 0.7),
        (0.7, 1.0, 0.3),
    ];
    for (id, (i, a, ar)) in ids.iter().zip(rs) {
        put_readings(&pool, *id, i, a, ar).await;
    }
    let b = MoodBuilder::new(h.clone());
    assert_eq!(b.build(&pool).await.unwrap(), out(5, true));

    let s = stored_state(&pool).await.unwrap();
    assert_eq!(s.track_count, 5);
    let mut all = vec![];
    for id in &ids {
        all.push(scores(&pool, *id).await);
    }
    for k in 0..22 {
        let col: Vec<f64> = all.iter().map(|r| r[k]).collect();
        let (m, sd) = mean_sd(&col);
        assert!((s.moods[k].mean - m).abs() < 1e-5, "{} mean", MOODS[k]);
        assert!((s.moods[k].sd - sd).abs() < 1e-5, "{} sd", MOODS[k]);
        assert!(s.moods[k].sd > 0.0);
    }
    let want = [
        rs.iter().map(|r| logit(f64::from(r.2))).collect::<Vec<_>>(),
        rs.iter().map(|r| logit(f64::from(r.0))).collect(),
        rs.iter().map(|r| logit(f64::from(r.1))).collect(),
    ];
    for (i, w) in want.iter().enumerate() {
        let (m, sd) = mean_sd(w);
        let got = s.readings[i].unwrap();
        assert!(
            (got.mean - m).abs() < 1e-5 && (got.sd - sd).abs() < 1e-5,
            "{}",
            READINGS[i]
        );
    }
    let version: String = sqlx::query_scalar("SELECT version FROM mood_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, "2/synthetic-v1");
    assert_eq!(version, mood_state_version(&h));
    // Statistics of other weights are not read back.
    let mut c = pool.acquire().await.unwrap();
    assert!(
        load_mood_state(&mut c, &heads_v("other", 0.3))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn statistics_rebuild_when_a_reading_changes_without_rescoring(pool: PgPool) {
    let (ids, _) = library(&pool, 3).await;
    for id in &ids {
        put_readings(&pool, *id, 0.4, 0.4, 0.4).await;
    }
    // A hand edit moves none of the signal's parts; the hourly full hash is the backstop (zero here).
    let b = MoodBuilder::new(heads()).with_hash_at_least_every(Duration::ZERO);
    b.build(&pool).await.unwrap();
    let before = stored_state(&pool).await.unwrap().readings[0].unwrap();
    put_readings(&pool, ids[0], 0.4, 0.4, 0.8).await;
    assert_eq!(b.build(&pool).await.unwrap(), out(0, true));
    let after = stored_state(&pool).await.unwrap().readings[0].unwrap();
    assert_ne!(before.mean, after.mean);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn statistics_stay_when_a_track_outside_them_changes(pool: PgPool) {
    let (_, _) = library(&pool, 3).await;
    // Outside the statistics: a vector of another checkpoint, and a missing file.
    let other = track(&pool).await;
    embed_as(&pool, other.track, "other", &[7; 32], "p", &vector(5, 1.0))
        .await
        .unwrap();
    let gone = track(&pool).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(gone.track)
        .execute(&pool)
        .await
        .unwrap();
    embed(&pool, gone.track, &vector(6, 1.0)).await;
    let b = MoodBuilder::new(heads()).with_hash_at_least_every(Duration::ZERO);
    assert_eq!(b.build(&pool).await.unwrap(), out(3, true));
    let fp: String = sqlx::query_scalar("SELECT fingerprint FROM mood_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    put_readings(&pool, gone.track, 0.9, 0.9, 0.9).await;
    sqlx::query("UPDATE tracks SET title = 'changed' WHERE id = $1")
        .bind(other.track)
        .execute(&pool)
        .await
        .unwrap();
    // The full hash really runs (interval zero) and finds the same inputs.
    let runs = b.full_hash_runs();
    assert_eq!(b.build(&pool).await.unwrap(), out(0, false));
    assert_eq!(b.full_hash_runs(), runs + 1);
    let after: String = sqlx::query_scalar("SELECT fingerprint FROM mood_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(fp, after);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn statistics_rebuild_when_a_track_leaves(pool: PgPool) {
    let (ids, _) = library(&pool, 4).await;
    let b = MoodBuilder::new(heads());
    b.build(&pool).await.unwrap();
    assert_eq!(stored_state(&pool).await.unwrap().track_count, 4);
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(ids[0])
        .execute(&pool)
        .await
        .unwrap();
    // The default interval: the change signal itself must move so the hash runs.
    assert_eq!(b.build(&pool).await.unwrap(), out(0, true));
    assert_eq!(stored_state(&pool).await.unwrap().track_count, 3);
    // Everyone gone: the statistics go with them.
    sqlx::query("UPDATE tracks SET missing_since = now()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(b.build(&pool).await.unwrap(), out(0, true));
    assert!(stored_state(&pool).await.is_none());
    assert_eq!(count(&pool, "mood_state").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn absent_reading_is_stored_null_and_moods_keep_statistics(pool: PgPool) {
    let (ids, _) = library(&pool, 3).await;
    let b = MoodBuilder::new(heads());
    b.build(&pool).await.unwrap();
    let absent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mood_state
         WHERE arousal_mean IS NULL AND arousal_sd IS NULL AND instrumental_logit_mean IS NULL
           AND instrumental_logit_sd IS NULL AND acoustic_logit_mean IS NULL AND acoustic_logit_sd IS NULL
           AND happy_mean IS NOT NULL AND happy_sd IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        absent, 1,
        "absent readings are NULL and the moods keep their statistics"
    );
    assert_eq!(
        stored_state(&pool).await.unwrap().readings,
        vec![None, None, None]
    );

    // One track with readings: the statistics cover that track only (it moves the change signal by itself).
    put_readings(&pool, ids[1], 0.5, 0.25, 0.75).await;
    assert_eq!(b.build(&pool).await.unwrap(), out(0, true));
    let s = stored_state(&pool).await.unwrap();
    let a = s.readings[0].unwrap();
    assert!((a.mean - logit(0.75)).abs() < 1e-9 && a.sd == 0.0);
    assert_eq!(s.track_count, 3, "the moods still cover every track");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unchanged_library_skips_the_full_hash(pool: PgPool) {
    let (ids, _) = library(&pool, 3).await;
    let b = MoodBuilder::new(heads());
    assert_eq!(b.build(&pool).await.unwrap(), out(3, true));
    assert_eq!(
        b.full_hash_runs(),
        1,
        "scores were written, so the signal is not trusted"
    );
    for _ in 0..3 {
        assert_eq!(b.build(&pool).await.unwrap(), out(0, false));
    }
    assert_eq!(
        b.full_hash_runs(),
        1,
        "an unchanged library does not run the full hash"
    );
    // A changed library still hashes.
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(ids[2])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(b.build(&pool).await.unwrap(), out(0, true));
    assert_eq!(b.full_hash_runs(), 2);
    // A new vector is scored and hashed.
    let s = track(&pool).await;
    embed(&pool, s.track, &vector(500, 1.0)).await;
    assert_eq!(b.build(&pool).await.unwrap(), out(1, true));
    assert_eq!(b.full_hash_runs(), 3);
    // A process that has not hashed yet hashes, finds the stored fingerprint current and writes nothing.
    let fresh = MoodBuilder::new(heads());
    assert_eq!(fresh.build(&pool).await.unwrap(), out(0, false));
    assert_eq!(fresh.full_hash_runs(), 1);
    // The signal stands in for the hash for a bounded time only.
    let stale = MoodBuilder::new(heads()).with_hash_at_least_every(Duration::ZERO);
    stale.build(&pool).await.unwrap();
    stale.build(&pool).await.unwrap();
    assert_eq!(stale.full_hash_runs(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn progress_counts_eligible_and_scored(pool: PgPool) {
    let h = heads();
    let (ids, _) = library(&pool, 3).await;
    let other = track(&pool).await;
    embed_as(&pool, other.track, "other", &[7; 32], "p", &vector(5, 1.0))
        .await
        .unwrap();
    assert_eq!(mood_progress(&pool, &h).await.unwrap(), (3, 0));
    update(&pool, &h).await;
    assert_eq!(mood_progress(&pool, &h).await.unwrap(), (3, 3));
    embed(&pool, ids[0], &vector(321, 1.0)).await;
    assert_eq!(mood_progress(&pool, &h).await.unwrap(), (3, 2));
    assert_eq!(
        mood_progress(&pool, &heads_v("v2", 0.3)).await.unwrap(),
        (3, 0)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn snapshot_rebuilds_only_when_signature_changes(pool: PgPool) {
    let h = heads();
    let (ids, _) = library(&pool, 12).await;
    let b = MoodBuilder::new(h.clone());
    let snaps = MoodSnapshots::new();

    // No statistics yet: a first snapshot with no views (a process that starts here gives no moods).
    let (s, changed) = snaps.refresh(&pool, &h).await.unwrap();
    assert!(changed);
    assert_eq!(
        (s.signature.as_str(), s.views.len(), s.pending),
        (NO_STATISTICS, 0, 12)
    );
    assert!(!snaps.refresh(&pool, &h).await.unwrap().1);

    b.build(&pool).await.unwrap();
    let (s, changed) = snaps.refresh(&pool, &h).await.unwrap();
    assert!(changed);
    assert!(s.signature.starts_with("2/synthetic-v1:"));
    assert_eq!((s.views.len(), s.pending), (12, 0));
    // Every track has a decision, ranks for the five moods, and ranks are in 0..=1.
    for id in &ids {
        let v = &s.views[id];
        assert_ne!(v.dominant, kima_vibe::moods::UNANALYZED);
        assert_eq!(v.ranks.len(), 5);
        assert!(v.ranks.iter().all(|(_, r)| (0.0..=1.0).contains(r)));
    }
    let first_views = s.views.clone();

    // Nothing changed: no rebuild, same contents.
    let (again, changed) = snaps.refresh(&pool, &h).await.unwrap();
    assert!(!changed);
    assert!(Arc::ptr_eq(&again.views, &first_views));

    // A rescore moves the statistics' fingerprint, so the snapshot rebuilds.
    embed(&pool, ids[0], &vector(4242, 1.0)).await;
    let (s, changed) = snaps.refresh(&pool, &h).await.unwrap();
    assert!(
        !changed,
        "the statistics have not been rebuilt yet; the last snapshot stands"
    );
    assert_eq!(s.pending, 1, "one track waits for scores");
    b.build(&pool).await.unwrap();
    let (s, changed) = snaps.refresh(&pool, &h).await.unwrap();
    assert!(changed);
    assert_eq!((s.views.len(), s.pending), (12, 0));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn snapshot_decisions_follow_the_line_and_the_library_rank(pool: PgPool) {
    let h = heads();
    let (ids, _) = library(&pool, 20).await;
    let b = MoodBuilder::new(h.clone());
    b.build(&pool).await.unwrap();
    let snaps = MoodSnapshots::new();
    let (s, _) = snaps.refresh(&pool, &h).await.unwrap();
    // Recompute independently from the stored scores.
    let state = stored_state(&pool).await.unwrap();
    let mut rows = vec![];
    for id in &ids {
        rows.push(scores(&pool, *id).await);
    }
    let learned = [0usize, 3, 7, 10, 12]; // happy sad calm aggressive party
    let mut some_mood = false;
    for (id, row) in ids.iter().zip(&rows) {
        let v = &s.views[id];
        let mut want: Option<(usize, f64)> = None;
        for (i, k) in learned.iter().enumerate() {
            let z = (row[*k] - state.moods[*k].mean) / state.moods[*k].sd;
            if z >= 1.25 {
                let below = rows.iter().filter(|r| r[*k] < row[*k]).count() as f64;
                let eq = rows.iter().filter(|r| r[*k] <= row[*k]).count() as f64;
                let rank = (below + eq) / 2.0 / rows.len() as f64;
                if want.is_none_or(|(_, best)| rank > best) {
                    want = Some((i, rank));
                }
            }
        }
        match want {
            Some((i, rank)) => {
                some_mood = true;
                assert_eq!(v.dominant, kima_vibe::moods::CLIENT_MOODS[i].field);
                assert!((v.score - rank).abs() < 1e-6);
            }
            None => assert_eq!(v.dominant, kima_vibe::moods::NEUTRAL),
        }
    }
    assert!(
        some_mood,
        "the synthetic library must exercise a dominant mood"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn snapshot_keeps_last_contents_without_statistics(pool: PgPool) {
    let h = heads();
    let (ids, _) = library(&pool, 6).await;
    let b = MoodBuilder::new(h.clone());
    b.build(&pool).await.unwrap();
    let snaps = MoodSnapshots::new();
    let (before, _) = snaps.refresh(&pool, &h).await.unwrap();
    assert_eq!(before.views.len(), 6);

    // A re-run clears the statistics: the last snapshot stands, unchanged.
    sqlx::query("DELETE FROM mood_state")
        .execute(&pool)
        .await
        .unwrap();
    let (kept, changed) = snaps.refresh(&pool, &h).await.unwrap();
    assert!(!changed);
    assert!(Arc::ptr_eq(&kept.views, &before.views));
    assert_eq!(kept.signature, before.signature);
    // Statistics of other weights count as none.
    let (kept, changed) = snaps.refresh(&pool, &heads_v("v2", 0.3)).await.unwrap();
    assert!(!changed);
    assert_eq!(kept.views.len(), 6);

    // The inputs are the same after a re-run rebuilt the statistics, so the fingerprint and the signature are too:
    // nothing is rebuilt.
    sqlx::query("DELETE FROM track_moods WHERE track_id = $1")
        .bind(ids[0])
        .execute(&pool)
        .await
        .unwrap();
    b.build(&pool).await.unwrap(); // rescores it and rebuilds the statistics
    let (s, changed) = snaps.refresh(&pool, &h).await.unwrap();
    assert!(!changed);
    assert!(s.views.contains_key(&ids[0]));
    // Scores lost again: the track is pending, and the last snapshot stands until the statistics move.
    sqlx::query("DELETE FROM track_moods WHERE track_id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    let (s, _) = snaps.refresh(&pool, &h).await.unwrap();
    assert_eq!(s.pending, 1);
    assert!(s.views.contains_key(&ids[1]));
}
