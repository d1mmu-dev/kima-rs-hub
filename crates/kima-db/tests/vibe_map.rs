//! The map service (M5 Task 10, spec 7): first layout, placement, single flight and the rebuild races, against a real
//! Postgres. The re-run of Task 12 does not exist yet; `rerun` below is a stand-in that takes the locks in the order
//! the engine fixes (state row first, then map/feel rows, then track rows) and must be replaced by the real one then.
mod vibe_common;
use kima_db::PgPool;
use kima_db::ml::{self, Provenance};
use kima_db::vibe::MATCHING_SOUND;
use kima_db::vibe::map::{
    MapError, MapHooks, MapRead, MapService, MapUpdate, map_version, read_map,
};
use kima_db::vibe::rank::load_matching_state;
use kima_db::vibe::state::build_matching;
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use kima_vibe::formula::{self, MatchTrack};
use kima_vibe::map_compute::layout_library;
use sqlx::Executor;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use uuid::Uuid;
use vibe_common::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

fn axis(scale: f64, bias: f64) -> ProbeAxis {
    ProbeAxis {
        alpha: 1.0,
        bias,
        weights: (0..512)
            .map(|i| (((i * 7) % 13) as f64 / 13.0 - 0.5) * scale)
            .collect(),
    }
}

fn probe() -> FeelProbe {
    FeelProbe {
        model: "synthetic".into(),
        valence: axis(0.02, 0.5),
        arousal: axis(-0.03, 0.4),
        probe_id: "mapprobe00000001".into(),
    }
}

struct Lib {
    ids: Vec<Uuid>,
    vectors: Vec<Vec<f32>>,
    /// Per artist: a centre and two directions. Its tracks lie on that plane (plus a little noise), so neighbour
    /// order is a property of the data, not of noise, and a 3-D map can keep it.
    planes: Vec<[Vec<f64>; 3]>,
    rng: Rng,
}

impl Lib {
    /// A unit vector on artist `cluster`'s plane.
    fn vector(&mut self, cluster: usize, noise: f64) -> Vec<f32> {
        let (a, b) = (self.rng.next(), self.rng.next());
        let [c, u, v] = &self.planes[cluster];
        let x: Vec<f64> = (0..512)
            .map(|i| c[i] + 0.5 * (a * u[i] + b * v[i]) + noise * self.rng.next())
            .collect();
        let n = x.iter().map(|x| x * x).sum::<f64>().sqrt();
        x.iter().map(|x| (x / n) as f32).collect()
    }
}

/// `per_artist` tracks for each of `artists` artists.
async fn library(pool: &PgPool, artists: usize, per_artist: usize) -> Lib {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let direction = |rng: &mut Rng| -> Vec<f64> { (0..512).map(|_| rng.next()).collect() };
    let planes = (0..artists)
        .map(|_| {
            [
                direction(&mut rng),
                direction(&mut rng),
                direction(&mut rng),
            ]
        })
        .collect();
    let mut lib = Lib {
        ids: vec![],
        vectors: vec![],
        planes,
        rng,
    };
    for a in 0..artists {
        let artist = artist(pool, &format!("artist {a}")).await;
        let album = album(pool, artist).await;
        for _ in 0..per_artist {
            add_track(pool, &mut lib, a, album).await;
        }
    }
    lib
}

async fn add_track(pool: &PgPool, lib: &mut Lib, cluster: usize, album: Uuid) -> Uuid {
    let id = track_in(pool, album).await;
    let v = lib.vector(cluster, 0.02);
    embed(pool, id, &v).await;
    lib.ids.push(id);
    lib.vectors.push(v);
    id
}

/// A built state (feel + state row) for the library.
async fn built(pool: &PgPool, artists: usize, per_artist: usize) -> Lib {
    let lib = library(pool, artists, per_artist).await;
    build_matching(pool, &probe()).await.unwrap();
    lib
}

fn service(pool: &PgPool) -> Arc<MapService> {
    MapService::new(pool.clone(), probe())
}

fn done(laid_out: usize, placed: usize) -> MapUpdate {
    MapUpdate { laid_out, placed }
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
struct MapRow {
    track_id: Uuid,
    embedded_at: time::OffsetDateTime,
    x: f64,
    y: f64,
    z: f64,
    colour: f64,
    laid_out: bool,
}

async fn map_rows(pool: &PgPool) -> BTreeMap<Uuid, MapRow> {
    let rows: Vec<MapRow> = sqlx::query_as("SELECT * FROM matching_map")
        .fetch_all(pool)
        .await
        .unwrap();
    rows.into_iter().map(|r| (r.track_id, r)).collect()
}

async fn map_meta(pool: &PgPool) -> (Option<String>, Option<serde_json::Value>) {
    sqlx::query_as("SELECT map_version, map_frame FROM matching_state WHERE id = 'default'")
        .fetch_one(pool)
        .await
        .unwrap()
}

type Hook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

fn hook<F, Fut>(f: F) -> Hook
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move || Box::pin(f()))
}

fn service_with(pool: &PgPool, before_compute: Hook) -> Arc<MapService> {
    MapService::with_hooks(
        pool.clone(),
        probe(),
        MapHooks {
            before_compute: Some(before_compute),
        },
    )
}

/// Waits until `n` backends wait on a lock in this database.
async fn waiting_on_locks(pool: &PgPool, n: i64) {
    for _ in 0..200 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{n} backend(s) never waited on a lock");
}

/// Stand-in for the Task 12 re-run: the state row first (deleting it), then the map, then, for `vectors`, the sound
/// vectors (whose cascade reaches the map and feel rows).
async fn rerun(pool: &PgPool, vectors: bool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM matching_state")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM matching_map")
        .execute(&mut *tx)
        .await
        .unwrap();
    if vectors {
        sqlx::query("DELETE FROM track_embeddings")
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reports_computing_until_the_step_lays_it_out(pool: PgPool) {
    let lib = built(&pool, 3, 5).await;
    assert!(matches!(read_map(&pool).await.unwrap(), MapRead::Computing));
    let svc = service(&pool);
    assert_eq!(svc.update(true).await.unwrap(), done(lib.ids.len(), 0));
    let MapRead::Ready(map) = read_map(&pool).await.unwrap() else {
        panic!("the map is laid out")
    };
    assert_eq!(map.points.len(), lib.ids.len());
    assert!(map.updated_at.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn waits_to_lay_out_until_inputs_settled(pool: PgPool) {
    built(&pool, 3, 5).await;
    let computed = Arc::new(AtomicUsize::new(0));
    let c = computed.clone();
    let svc = service_with(
        &pool,
        hook(move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }),
    );
    assert_eq!(svc.update(false).await.unwrap(), done(0, 0));
    // Nothing reached the compute step, nothing was stored and the map is still pending.
    assert_eq!(computed.load(Ordering::SeqCst), 0);
    assert_eq!(count(&pool, "matching_map").await, 0);
    assert_eq!(map_meta(&pool).await, (None, None));
    assert!(matches!(read_map(&pool).await.unwrap(), MapRead::Computing));
    assert_eq!(svc.update(true).await.unwrap().laid_out, 15);
    assert_eq!(computed.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn without_a_built_state_there_is_nothing_to_lay_out(pool: PgPool) {
    library(&pool, 2, 3).await;
    assert_eq!(service(&pool).update(true).await.unwrap(), done(0, 0));
    assert_eq!(count(&pool, "matching_map").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lays_out_from_the_formulas_own_scores(pool: PgPool) {
    let lib = built(&pool, 6, 10).await;
    let n = lib.ids.len();
    assert_eq!(service(&pool).update(true).await.unwrap(), done(n, 0));
    let rows = map_rows(&pool).await;
    assert_eq!(rows.len(), n);
    assert!(rows.values().all(|r| r.laid_out));
    let (version, frame) = map_meta(&pool).await;
    assert_eq!(version.unwrap(), map_version(&probe()));
    assert!(frame.is_some());

    // The same arithmetic run directly on the same inputs, rows in id order.
    let state = load_matching_state(&pool, &probe()).await.unwrap().unwrap();
    let feel: BTreeMap<Uuid, (f32, f32)> = sqlx::query_as::<_, (Uuid, f32, f32)>(
        "SELECT track_id, valence, arousal FROM matching_feel",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(id, v, a)| (id, (v, a)))
    .collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|i| lib.ids[*i]);
    let mut tracks = vec![];
    let mut feel_z = vec![];
    for i in &order {
        let (v, a) = feel[&lib.ids[*i]];
        let raw = [f64::from(v), f64::from(a)];
        let z = |k: usize| {
            if state.feel.sd[k] > 0.0 {
                (raw[k] - state.feel.mean[k]) / state.feel.sd[k]
            } else {
                0.0
            }
        };
        feel_z.extend([z(0), z(1)]);
        let artist: Uuid = sqlx::query_scalar(
            "SELECT al.artist_id FROM tracks t JOIN albums al ON al.id = t.album_id WHERE t.id = $1",
        )
        .bind(lib.ids[*i])
        .fetch_one(&pool)
        .await
        .unwrap();
        let album: Uuid = sqlx::query_scalar("SELECT album_id FROM tracks WHERE id = $1")
            .bind(lib.ids[*i])
            .fetch_one(&pool)
            .await
            .unwrap();
        tracks.push(MatchTrack {
            track_id: lib.ids[*i].to_string(),
            album_id: album.to_string(),
            artist_id: artist.to_string(),
            sound: formula::unit(
                &lib.vectors[*i]
                    .iter()
                    .map(|x| f64::from(*x))
                    .collect::<Vec<_>>(),
            ),
            feel: formula::feel_vector(raw, &state.feel),
        });
    }
    let direct = layout_library(&tracks, &feel_z, &state.genres, state.genre_weight, None);
    for (k, i) in order.iter().enumerate() {
        let row = &rows[&lib.ids[*i]];
        for (d, v) in [row.x, row.y, row.z].into_iter().enumerate() {
            assert_eq!(
                v.to_bits(),
                direct.positions[k * 3 + d].to_bits(),
                "{k} axis {d}"
            );
        }
        assert_eq!(
            row.colour.to_bits(),
            direct.colour[k].to_bits(),
            "colour {k}"
        );
    }
    let colours: Vec<f64> = rows.values().map(|r| r.colour).collect();
    assert_eq!(colours.iter().cloned().fold(f64::INFINITY, f64::min), 0.0);
    assert_eq!(
        colours.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        1.0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn neighbouring_tracks_land_close(pool: PgPool) {
    // Clusters well above 7 tracks, so the formula's 7 nearest are a real subset of a cluster, not nearly all of it.
    let lib = built(&pool, 4, 30).await;
    service(&pool).update(true).await.unwrap();
    let rows = map_rows(&pool).await;
    let state = load_matching_state(&pool, &probe()).await.unwrap().unwrap();
    let ids = lib.ids.clone();
    let tracks: Vec<MatchTrack> = {
        let mut out = vec![];
        for (i, id) in ids.iter().enumerate() {
            let (v, a): (f32, f32) =
                sqlx::query_as("SELECT valence, arousal FROM matching_feel WHERE track_id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let (artist, album): (Uuid, Uuid) = sqlx::query_as(
                "SELECT al.artist_id, al.id FROM tracks t JOIN albums al ON al.id = t.album_id WHERE t.id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
            out.push(MatchTrack {
                track_id: id.to_string(),
                album_id: album.to_string(),
                artist_id: artist.to_string(),
                sound: formula::unit(
                    &lib.vectors[i]
                        .iter()
                        .map(|x| f64::from(*x))
                        .collect::<Vec<_>>(),
                ),
                feel: formula::feel_vector([f64::from(v), f64::from(a)], &state.feel),
            });
        }
        out
    };
    let k = 7;
    let seven = |dist: &dyn Fn(usize, usize) -> f64| -> Vec<HashSet<usize>> {
        (0..ids.len())
            .map(|a| {
                let mut others: Vec<usize> = (0..ids.len()).filter(|b| *b != a).collect();
                others.sort_by(|b, c| dist(a, *b).partial_cmp(&dist(a, *c)).unwrap());
                others.into_iter().take(k).collect()
            })
            .collect()
    };
    let on_map = seven(&|a, b| {
        let (p, q) = (&rows[&ids[a]], &rows[&ids[b]]);
        ((p.x - q.x).powi(2) + (p.y - q.y).powi(2) + (p.z - q.z).powi(2)).sqrt()
    });
    let by_formula = seven(&|a, b| {
        -formula::pair_score(
            &tracks[a],
            &tracks[b],
            &state.genres,
            state.genre_weight_sq(),
        )
    });
    let shared: usize = (0..ids.len())
        .map(|i| on_map[i].intersection(&by_formula[i]).count())
        .sum();
    let recall = shared as f64 / (ids.len() * k) as f64;
    // Upstream asserts 0.9 on its own fixture. Recall is a property of the data (a 3-D map of 512-d neighbourhoods
    // is lossy): upstream's TypeScript layout scores 0.78 (50 tracks) and 0.57 (300 tracks) on the goldens of this
    // repository, and this library measures 0.82; chance is 7/119. The floor guards "neighbours land close", the
    // bit-equality test above guards the arithmetic.
    assert!(recall >= 0.75, "map recall {recall}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn serves_stored_map_without_missing_or_other_model(pool: PgPool) {
    let mut lib = built(&pool, 3, 5).await;
    // A file that goes missing after the layout, and a vector of another model, never appear.
    let artist2 = artist(&pool, "other").await;
    let album2 = album(&pool, artist2).await;
    let other = track_in(&pool, album2).await;
    embed_as(&pool, other, "other_model", &[7; 32], "p1", &lib.vectors[0])
        .await
        .unwrap();
    build_matching(&pool, &probe()).await.unwrap();
    service(&pool).update(true).await.unwrap();
    let gone = lib.ids.pop().unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    let MapRead::Ready(map) = read_map(&pool).await.unwrap() else {
        panic!("laid out")
    };
    let served: HashSet<Uuid> = map.points.iter().map(|p| p.track_id).collect();
    assert_eq!(served, lib.ids.iter().cloned().collect::<HashSet<_>>());
    assert!(!served.contains(&other) && !served.contains(&gone));
    let rows = map_rows(&pool).await;
    for p in &map.points {
        assert_eq!(p.colour, rows[&p.track_id].colour);
        assert_eq!(
            (p.x, p.y, p.z),
            (
                rows[&p.track_id].x,
                rows[&p.track_id].y,
                rows[&p.track_id].z
            )
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn places_a_re_embedded_track_without_moving_others(pool: PgPool) {
    let mut lib = built(&pool, 4, 6).await;
    let svc = service(&pool);
    let n = lib.ids.len();
    svc.update(true).await.unwrap();
    let before = map_rows(&pool).await;
    let id = lib.ids[8];
    let v = lib.vector(1, 0.02);
    embed(&pool, id, &v).await;
    build_matching(&pool, &probe()).await.unwrap();
    assert_eq!(svc.update(true).await.unwrap(), done(0, 1));
    let after = map_rows(&pool).await;
    assert_eq!(after.len(), n);
    for (other, row) in &before {
        if *other != id {
            assert_eq!(&after[other], row, "a placed track moved");
        }
    }
    assert!(!after[&id].laid_out);
    assert_ne!(after[&id].embedded_at, before[&id].embedded_at);
    assert_eq!(svc.update(true).await.unwrap(), done(0, 0));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn keeps_placing_as_library_grows_and_never_moves_a_placed_track(pool: PgPool) {
    let mut lib = built(&pool, 4, 6).await;
    let svc = service(&pool);
    svc.update(true).await.unwrap();
    let before = map_rows(&pool).await;
    let album = album(&pool, artist(&pool, "newcomer").await).await;
    let mut added = vec![];
    for i in 0..20 {
        added.push(add_track(&pool, &mut lib, i % 4, album).await);
    }
    build_matching(&pool, &probe()).await.unwrap();
    assert_eq!(svc.update(true).await.unwrap(), done(0, 20));
    let after = map_rows(&pool).await;
    for (id, row) in &before {
        assert_eq!(&after[id], row);
    }
    assert!(added.iter().all(|id| !after[id].laid_out));
    // A further wave is coloured on the stored frame and still moves nothing.
    for i in 0..5 {
        add_track(&pool, &mut lib, i % 4, album).await;
    }
    build_matching(&pool, &probe()).await.unwrap();
    assert_eq!(svc.update(true).await.unwrap(), done(0, 5));
    let again = map_rows(&pool).await;
    for (id, row) in &after {
        assert_eq!(&again[id], row);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn single_track_gets_origin_and_colour_half(pool: PgPool) {
    let lib = built(&pool, 1, 1).await;
    let svc = service(&pool);
    assert_eq!(svc.update(false).await.unwrap(), done(0, 0));
    assert_eq!(svc.update(true).await.unwrap(), done(1, 0));
    let rows = map_rows(&pool).await;
    let row = &rows[&lib.ids[0]];
    assert_eq!(
        (row.x, row.y, row.z, row.colour, row.laid_out),
        (0.0, 0.0, 0.0, 0.5, true)
    );
    let (version, frame) = map_meta(&pool).await;
    assert_eq!(version.unwrap(), map_version(&probe()));
    assert!(frame.is_none());
    assert_eq!(svc.update(true).await.unwrap(), done(0, 0));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_callers_share_one_run(pool: PgPool) {
    let lib = built(&pool, 3, 5).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let svc = service_with(
        &pool,
        hook(move || {
            let r = r.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }),
    );
    let calls: Vec<_> = (0..6)
        .map(|_| {
            let svc = svc.clone();
            tokio::spawn(async move { svc.update(true).await })
        })
        .collect();
    for call in calls {
        assert_eq!(call.await.unwrap().unwrap(), done(lib.ids.len(), 0));
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1, "one layout for six callers");
    // The flight is over: the next call starts a new run, which finds the map complete.
    assert_eq!(svc.update(true).await.unwrap(), done(0, 0));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rerun_overtaking_a_layout_stores_nothing(pool: PgPool) {
    built(&pool, 3, 5).await;
    let overtook = Arc::new(AtomicUsize::new(0));
    let (o, p) = (overtook.clone(), pool.clone());
    let svc = service_with(
        &pool,
        hook(move || {
            let (o, p) = (o.clone(), p.clone());
            async move {
                // Only the first run is overtaken.
                if o.fetch_add(1, Ordering::SeqCst) == 0 {
                    rerun(&p, false).await;
                }
            }
        }),
    );
    // The hook runs after the vectors were read and before the compute: the layout finds no state to write under.
    assert_eq!(svc.update(true).await.unwrap(), done(0, 0));
    assert_eq!(overtook.load(Ordering::SeqCst), 1);
    assert_eq!(count(&pool, "matching_map").await, 0);
    assert_eq!(count(&pool, "matching_state").await, 0);
    // The next pass builds the state again and lays the library out afresh.
    build_matching(&pool, &probe()).await.unwrap();
    assert_eq!(svc.update(true).await.unwrap().laid_out, 15);
    assert_eq!(overtook.load(Ordering::SeqCst), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn layout_first_then_rerun_leaves_no_old_map(pool: PgPool) {
    built(&pool, 3, 5).await;
    // Hold the state row, as a map write under way does, so the layout and the re-run both queue on it and reach it
    // in the order they started: the layout first.
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM matching_state WHERE id = 'default' FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let layout = {
        let svc = service(&pool);
        tokio::spawn(async move { svc.update(true).await })
    };
    waiting_on_locks(&pool, 1).await;
    let rerunning = {
        let pool = pool.clone();
        tokio::spawn(async move { rerun(&pool, false).await })
    };
    waiting_on_locks(&pool, 2).await;
    holder.commit().await.unwrap();
    assert_eq!(layout.await.unwrap().unwrap(), done(15, 0));
    rerunning.await.unwrap();
    assert_eq!(
        count(&pool, "matching_map").await,
        0,
        "no old map survives the re-run"
    );
    assert_eq!(count(&pool, "matching_state").await, 0);
    build_matching(&pool, &probe()).await.unwrap();
    assert_eq!(service(&pool).update(true).await.unwrap().laid_out, 15);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_deadlock_with_rerun_that_deletes_vectors(pool: PgPool) {
    let lib = built(&pool, 3, 5).await;
    let svc = service(&pool);
    svc.update(true).await.unwrap();
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM matching_state WHERE id = 'default' FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    // A placement write queues on the state row (the map is laid out, one track waits for a place).
    sqlx::query("DELETE FROM matching_map WHERE track_id = $1")
        .bind(lib.ids[0])
        .execute(&pool)
        .await
        .unwrap();
    let placement = {
        let svc = svc.clone();
        tokio::spawn(async move { svc.update(true).await })
    };
    waiting_on_locks(&pool, 1).await;
    // Deleting a vector deletes its map row: this re-run touches the map before the tracks, and must still take the
    // state row first.
    let rerunning = {
        let pool = pool.clone();
        tokio::spawn(async move { rerun(&pool, true).await })
    };
    waiting_on_locks(&pool, 2).await;
    holder.commit().await.unwrap();
    let placed = tokio::time::timeout(Duration::from_secs(30), placement)
        .await
        .expect("no deadlock")
        .unwrap();
    assert!(placed.is_ok());
    tokio::time::timeout(Duration::from_secs(30), rerunning)
        .await
        .expect("no deadlock")
        .unwrap();
    assert_eq!(count(&pool, "matching_map").await, 0);
    assert_eq!(count(&pool, "track_embeddings").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn build_that_waited_behind_a_rerun_writes_nothing(pool: PgPool) {
    built(&pool, 3, 5).await;
    // A re-run in flight: it has taken the state row and deleted it, and has not committed yet.
    let mut rerunning = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM matching_state")
        .execute(&mut *rerunning)
        .await
        .unwrap();
    sqlx::query("UPDATE track_embeddings SET created_at = created_at + interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let build = {
        let pool = pool.clone();
        tokio::spawn(async move { build_matching(&pool, &probe()).await })
    };
    waiting_on_locks(&pool, 1).await;
    rerunning.commit().await.unwrap();
    let outcome = build.await.unwrap().unwrap();
    assert_eq!((outcome.feel_updated, outcome.rebuilt), (0, false));
    assert_eq!(count(&pool, "matching_state").await, 0);
}

fn music_provenance() -> Provenance {
    Provenance {
        model_id: MATCHING_SOUND.model_id.into(),
        model_sha256: MATCHING_SOUND.model_sha256,
        pipeline_version: MATCHING_SOUND.pipeline_version.into(),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_deadlock_with_embedding_publish_during_rerun(pool: PgPool) {
    let lib = built(&pool, 2, 4).await;
    let svc = service(&pool);
    svc.update(true).await.unwrap();
    let id = lib.ids[0];
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, started_at, next_attempt_at)
         VALUES ($1, 'embedding', 'working', clock_timestamp(), now() + interval '15 minutes')
         ON CONFLICT (track_id, stage) DO UPDATE SET status = 'working',
             started_at = clock_timestamp(), next_attempt_at = now() + interval '15 minutes'",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let claim = ml::embedding_claim(&pool, id).await.unwrap().unwrap();

    // The re-run holds the state row and the track row (state, then tracks, then stages: the engine's order); the
    // analyzer's publish queues on the track row.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM matching_state")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM tracks WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let publish = {
        let (pool, v) = (pool.clone(), lib.vectors[0].clone());
        tokio::spawn(
            async move { ml::complete_embedding(&pool, &claim, &music_provenance(), &v).await },
        )
    };
    waiting_on_locks(&pool, 1).await;
    // The re-run goes on: map and vectors cleared, the stage reset, then it commits.
    tx.execute("DELETE FROM matching_map").await.unwrap();
    tx.execute(
        "UPDATE track_stages SET status = 'pending', started_at = NULL WHERE stage = 'embedding'",
    )
    .await
    .unwrap();
    tx.execute("DELETE FROM track_embeddings").await.unwrap();
    tx.commit().await.unwrap();
    let published = tokio::time::timeout(Duration::from_secs(30), publish)
        .await
        .expect("no deadlock")
        .unwrap()
        .unwrap();
    assert!(
        !published,
        "the publish found its stage reset and stored nothing"
    );
    assert_eq!(count(&pool, "track_embeddings").await, 0);
    assert_eq!(count(&pool, "matching_map").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn shutdown_cancels_a_running_layout(pool: PgPool) {
    built(&pool, 3, 5).await;
    let slot: Arc<std::sync::Mutex<Option<Arc<MapService>>>> = Arc::default();
    let s = slot.clone();
    let svc = service_with(
        &pool,
        hook(move || {
            let s = s.clone();
            async move {
                // Shutdown arrives while the layout is about to compute.
                s.lock().unwrap().as_ref().unwrap().cancel();
            }
        }),
    );
    *slot.lock().unwrap() = Some(svc.clone());
    let err = svc.update(true).await.unwrap_err();
    assert!(matches!(&*err, MapError::Cancelled), "{err}");
    assert_eq!(count(&pool, "matching_map").await, 0);
    assert_eq!(map_meta(&pool).await, (None, None));
    // The service stays stopped.
    assert!(matches!(
        &*svc.update(true).await.unwrap_err(),
        MapError::Cancelled
    ));
}
