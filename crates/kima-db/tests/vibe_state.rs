//! Feel update and matching state (M5 Task 5, spec 6.2-6.3), against a real Postgres.
mod vibe_common;
use kima_db::PgPool;
use kima_db::genres::{GenreSource, replace_artist_genres};
use kima_db::vibe::MATCHING_SOUND;
use kima_db::vibe::state::{
    BuildOutcome, Progress, artist_matching_genres, build_matching, matching_progress,
    state_version,
};
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use kima_vibe::formula::{self, GenreModel, LibraryArtist};
use std::time::Duration;
use uuid::Uuid;
use vibe_common::*;

/// A deterministic unit vector per seed (varied components, so feel and spread are not degenerate).
fn vector(seed: u64) -> Vec<f32> {
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
    v.iter_mut().for_each(|a| *a /= n);
    v
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

fn probe(id: &str, bias: f64) -> FeelProbe {
    FeelProbe {
        model: "synthetic".into(),
        valence: axis(0.02, bias),
        arousal: axis(-0.03, 0.4),
        probe_id: id.into(),
    }
}

/// What the SQL must compute: clamp(0, 1, e . w / |e| + bias).
fn feel_of(e: &[f32], axis: &ProbeAxis) -> f64 {
    let norm = e.iter().map(|a| f64::from(*a).powi(2)).sum::<f64>().sqrt();
    let dot: f64 = e
        .iter()
        .zip(&axis.weights)
        .map(|(a, w)| f64::from(*a) * w)
        .sum();
    (dot / norm + axis.bias).clamp(0.0, 1.0)
}

struct Lib {
    artists: Vec<Uuid>,
    tracks: Vec<Uuid>,
    vectors: Vec<Vec<f32>>,
}

/// `per_artist` tracks for each of `artists` artists, each track embedded under the gate's identity.
async fn library(pool: &PgPool, artists: usize, per_artist: usize) -> Lib {
    let mut lib = Lib {
        artists: vec![],
        tracks: vec![],
        vectors: vec![],
    };
    for a in 0..artists {
        let artist = artist(pool, &format!("artist {a}")).await;
        let album = album(pool, artist).await;
        lib.artists.push(artist);
        for t in 0..per_artist {
            let track = track_in(pool, album).await;
            let v = vector((a * 100 + t) as u64 + 1);
            embed(pool, track, &v).await;
            lib.tracks.push(track);
            lib.vectors.push(v);
        }
    }
    lib
}

async fn set_genres(pool: &PgPool, artist: Uuid, source: GenreSource, names: &[&str]) {
    let names: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    let mut conn = pool.acquire().await.unwrap();
    replace_artist_genres(&mut conn, artist, source, &names)
        .await
        .unwrap();
}

async fn set_user_genres(pool: &PgPool, artist: Uuid, names: &[&str]) {
    let names: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    sqlx::query("UPDATE artists SET user_genres = $2 WHERE id = $1")
        .bind(artist)
        .bind(names)
        .execute(pool)
        .await
        .unwrap();
}

async fn feel_rows(pool: &PgPool) -> Vec<(Uuid, f32, f32, String)> {
    sqlx::query_as("SELECT track_id, valence, arousal, probe FROM matching_feel ORDER BY track_id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[derive(sqlx::FromRow)]
struct State {
    version: String,
    track_count: i32,
    feel_mean_valence: f64,
    feel_mean_arousal: f64,
    feel_sd_valence: f64,
    feel_sd_arousal: f64,
    sound_spread: f64,
    genre_spread: f64,
    genre_weight: f64,
    genres: serde_json::Value,
}

async fn state(pool: &PgPool) -> Option<State> {
    sqlx::query_as(
        "SELECT version, track_count, feel_mean_valence, feel_mean_arousal, feel_sd_valence, feel_sd_arousal,
                sound_spread, genre_spread, genre_weight, genres FROM matching_state",
    )
    .fetch_optional(pool)
    .await
    .unwrap()
}

fn outcome(feel_updated: u64, rebuilt: bool) -> BuildOutcome {
    BuildOutcome {
        feel_updated,
        rebuilt,
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn computes_feel_for_every_eligible_vector_and_builds_state(pool: PgPool) {
    assert!(current_database(&pool).await.starts_with("_sqlx_test"));
    let lib = library(&pool, 3, 2).await;
    // A vector of another checkpoint is invisible to the engine.
    let other = track(&pool).await;
    embed_as(&pool, other.track, "other", &[7; 32], "p", &vector(999))
        .await
        .unwrap();
    let p = probe("0123456789abcdef", 0.5);

    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(6, true));

    let rows = feel_rows(&pool).await;
    assert_eq!(
        rows.len(),
        6,
        "one feel row per eligible vector, none for the other model"
    );
    for (i, id) in lib.tracks.iter().enumerate() {
        let row = rows.iter().find(|r| r.0 == *id).unwrap();
        assert!((f64::from(row.1) - feel_of(&lib.vectors[i], &p.valence)).abs() < 1e-5);
        assert!((f64::from(row.2) - feel_of(&lib.vectors[i], &p.arousal)).abs() < 1e-5);
        assert_eq!(row.3, p.probe_id);
    }
    let s = state(&pool).await.unwrap();
    assert_eq!(
        s.version,
        format!("1/{}/{}", MATCHING_SOUND.model_id, p.probe_id)
    );
    assert_eq!(s.version, state_version(&p));
    assert_eq!(s.track_count, 6);
    assert_eq!(count(&pool, "matching_state").await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stores_the_library_statistics_the_formula_defines(pool: PgPool) {
    let lib = library(&pool, 4, 3).await;
    set_genres(
        &pool,
        lib.artists[0],
        GenreSource::MusicBrainz,
        &["Rock", "Pop"],
    )
    .await;
    set_genres(&pool, lib.artists[1], GenreSource::MusicBrainz, &["Jazz"]).await;
    set_genres(&pool, lib.artists[2], GenreSource::File, &["rock"]).await;
    let p = probe("aaaaaaaaaaaaaaaa", 0.5);
    build_matching(&pool, &p).await.unwrap();
    let s = state(&pool).await.unwrap();

    let feel = feel_rows(&pool).await;
    let n = feel.len() as f64;
    let stats = |pick: fn(&(Uuid, f32, f32, String)) -> f64| {
        let mean = feel.iter().map(pick).sum::<f64>() / n;
        let sd = (feel.iter().map(|r| (pick(r) - mean).powi(2)).sum::<f64>() / n).sqrt();
        (mean, sd)
    };
    let (mv, sv) = stats(|r| f64::from(r.1));
    let (ma, sa) = stats(|r| f64::from(r.2));
    for (got, want) in [
        (s.feel_mean_valence, mv),
        (s.feel_sd_valence, sv),
        (s.feel_mean_arousal, ma),
        (s.feel_sd_arousal, sa),
    ] {
        assert!((got - want).abs() < 1e-9, "{got} vs {want}");
    }

    // soundSpread = 1 - |mean of unit vectors|^2, from the vectors themselves.
    let mut mean = vec![0.0_f64; 512];
    for v in &lib.vectors {
        let norm = v.iter().map(|a| f64::from(*a).powi(2)).sum::<f64>().sqrt();
        for (m, a) in mean.iter_mut().zip(v) {
            *m += f64::from(*a) / norm / n;
        }
    }
    let spread = 1.0 - mean.iter().map(|m| m * m).sum::<f64>();
    assert!((s.sound_spread - spread).abs() < 1e-5);

    // The genre part is the pure crate's, on the same inputs: three tracks per artist.
    let mut ids = lib.artists.clone();
    ids.sort();
    let inputs: Vec<LibraryArtist> = lib
        .artists
        .iter()
        .enumerate()
        .map(|(i, id)| LibraryArtist {
            artist_id: id.to_string(),
            tracks: 3,
            genres: match i {
                0 => vec!["rock".into(), "pop".into()],
                1 => vec!["jazz".into()],
                2 => vec!["rock".into()],
                _ => vec![],
            },
        })
        .collect();
    let (model, genre_spread) = formula::build_genre_model(&inputs);
    assert_eq!(s.genre_spread, genre_spread);
    assert_eq!(
        s.genre_weight,
        formula::genre_weight(s.sound_spread, genre_spread)
    );
    assert!(s.genre_weight > 0.0);
    let stored: GenreModel = serde_json::from_value(s.genres).unwrap();
    assert_eq!(stored.vocabulary, model.vocabulary);
    assert_eq!(
        stored.artists.keys().collect::<Vec<_>>(),
        model.artists.keys().collect::<Vec<_>>()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn does_nothing_when_nothing_changed(pool: PgPool) {
    library(&pool, 2, 2).await;
    let p = probe("bbbbbbbbbbbbbbbb", 0.5);
    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(4, true));
    let before: time::OffsetDateTime = sqlx::query_scalar("SELECT built_at FROM matching_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(0, false));
    let after: time::OffsetDateTime = sqlx::query_scalar("SELECT built_at FROM matching_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "an unchanged library does not rewrite the state"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rebuilds_when_an_artists_genres_change(pool: PgPool) {
    let lib = library(&pool, 2, 2).await;
    set_genres(&pool, lib.artists[0], GenreSource::MusicBrainz, &["rock"]).await;
    let p = probe("cccccccccccccccc", 0.5);
    build_matching(&pool, &p).await.unwrap();

    set_genres(&pool, lib.artists[0], GenreSource::MusicBrainz, &["jazz"]).await;
    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(0, true));

    // A genre MusicBrainz does not list never reaches the model, so it moves nothing.
    set_genres(
        &pool,
        lib.artists[0],
        GenreSource::MusicBrainz,
        &["jazz", "zzz not a musicbrainz genre"],
    )
    .await;
    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(0, false));

    // The person's own list replaces the enriched one.
    set_user_genres(&pool, lib.artists[0], &["pop"]).await;
    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(0, true));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rebuilds_when_a_file_goes_missing_and_drops_the_track_from_results_at_once(pool: PgPool) {
    let lib = library(&pool, 2, 2).await;
    let p = probe("dddddddddddddddd", 0.5);
    build_matching(&pool, &p).await.unwrap();
    assert_eq!(matching_progress(&pool, &p).await.unwrap().total, 4);

    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(lib.tracks[0])
        .execute(&pool)
        .await
        .unwrap();
    // Before any rebuild: the track is already out of every eligible read.
    assert_eq!(matching_progress(&pool, &p).await.unwrap().total, 3);
    let mut conn = pool.acquire().await.unwrap();
    let counted: u64 = artist_matching_genres(&mut conn)
        .await
        .unwrap()
        .iter()
        .map(|a| a.tracks)
        .sum();
    assert_eq!(counted, 3);
    drop(conn);

    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(0, true));
    assert_eq!(state(&pool).await.unwrap().track_count, 3);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recomputes_feel_for_a_changed_vector(pool: PgPool) {
    let lib = library(&pool, 2, 2).await;
    let p = probe("eeeeeeeeeeeeeeee", 0.5);
    build_matching(&pool, &p).await.unwrap();

    let fresh = vector(4242);
    embed(&pool, lib.tracks[1], &fresh).await;
    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(1, true));
    let row = feel_rows(&pool)
        .await
        .into_iter()
        .find(|r| r.0 == lib.tracks[1])
        .unwrap();
    assert!((f64::from(row.1) - feel_of(&fresh, &p.valence)).abs() < 1e-5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn upsert_always_rewrites_created_at(pool: PgPool) {
    // The staleness key of feel, map and mood rows is track_embeddings.created_at: a re-embed with the very same
    // vector must still move it, or the dependent rows would never be recomputed.
    let lib = library(&pool, 1, 1).await;
    let created = |pool: PgPool, id: Uuid| async move {
        sqlx::query_scalar::<_, time::OffsetDateTime>(
            "SELECT created_at FROM track_embeddings WHERE track_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let p = probe("1111111111111111", 0.5);
    build_matching(&pool, &p).await.unwrap();
    let first = created(pool.clone(), lib.tracks[0]).await;
    embed(&pool, lib.tracks[0], &lib.vectors[0]).await;
    assert!(created(pool.clone(), lib.tracks[0]).await > first);
    assert_eq!(build_matching(&pool, &p).await.unwrap().feel_updated, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn feel_for_other_probe_is_stale(pool: PgPool) {
    library(&pool, 2, 2).await;
    build_matching(&pool, &probe("aaaa000000000000", 0.5))
        .await
        .unwrap();
    let b = probe("bbbb000000000000", 0.3);
    assert_eq!(build_matching(&pool, &b).await.unwrap(), outcome(4, true));
    assert!(feel_rows(&pool).await.iter().all(|r| r.3 == b.probe_id));
    assert_eq!(state(&pool).await.unwrap().version, state_version(&b));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_eligible_track_deletes_the_state_row(pool: PgPool) {
    library(&pool, 2, 2).await;
    let p = probe("2222222222222222", 0.5);
    build_matching(&pool, &p).await.unwrap();
    assert_eq!(count(&pool, "matching_state").await, 1);

    sqlx::query("DELETE FROM track_embeddings")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(0, false));
    assert_eq!(count(&pool, "matching_state").await, 0);
    assert_eq!(
        count(&pool, "matching_feel").await,
        0,
        "feel went with the vectors"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn write_conflict_40001_is_swallowed(pool: PgPool) {
    library(&pool, 2, 2).await;
    let p = probe("3333333333333333", 0.5);
    build_matching(&pool, &p).await.unwrap();

    // A new vector the build would otherwise pick up: a swallowed conflict must leave it untouched.
    let fresh = track(&pool).await;
    embed(&pool, fresh.track, &vector(31337)).await;

    // A concurrent writer (a re-run or a map write) holds the state row and changes it. The build waits on the row
    // lock and, under REPEATABLE READ, then fails with 40001: nothing is written and no error escapes.
    let mut other = pool.begin().await.unwrap();
    sqlx::query("UPDATE matching_state SET track_count = track_count")
        .execute(&mut *other)
        .await
        .unwrap();
    let racing = {
        let (pool, p) = (pool.clone(), p.clone());
        tokio::spawn(async move { build_matching(&pool, &p).await })
    };
    let mut waiting = false;
    for _ in 0..100 {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if n > 0 {
            waiting = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(waiting, "the build must be blocked on the state row lock");
    other.commit().await.unwrap();
    assert_eq!(racing.await.unwrap().unwrap(), outcome(0, false));
    assert_eq!(
        count(&pool, "matching_feel").await,
        4,
        "the rolled-back build wrote no feel"
    );
    assert_eq!(state(&pool).await.unwrap().track_count, 4);
    // The next pass picks the new vector up.
    assert_eq!(build_matching(&pool, &p).await.unwrap(), outcome(1, true));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn genre_source_prefers_user_list_and_filters_to_mb_list(pool: PgPool) {
    let lib = library(&pool, 5, 2).await;
    // 0: MusicBrainz list with one name MusicBrainz does not know; the person's list is empty -> enriched wins.
    set_genres(
        &pool,
        lib.artists[0],
        GenreSource::MusicBrainz,
        &["Rock", "Not A Real Genre Xyz"],
    )
    .await;
    set_user_genres(&pool, lib.artists[0], &[]).await;
    // 1: the person's list replaces the enriched one.
    set_genres(&pool, lib.artists[1], GenreSource::MusicBrainz, &["rock"]).await;
    set_user_genres(&pool, lib.artists[1], &["Jazz"]).await;
    // 2: file tags only, used when there is no MusicBrainz list.
    set_genres(&pool, lib.artists[2], GenreSource::File, &["pop", "rock"]).await;
    // 3: no genres at all, still counted for its tracks.
    // 4: an artist without any eligible track is not part of the library.
    let lonely = artist(&pool, "lonely").await;
    let album = album(&pool, lonely).await;
    track_in(&pool, album).await;
    set_genres(&pool, lonely, GenreSource::MusicBrainz, &["rock"]).await;

    let mut conn = pool.acquire().await.unwrap();
    let got = artist_matching_genres(&mut conn).await.unwrap();
    let of = |id: Uuid| {
        got.iter()
            .find(|a| a.artist_id == id.to_string())
            .map(|a| (a.tracks, a.genres.clone()))
    };
    assert_eq!(of(lib.artists[0]), Some((2, vec!["rock".to_owned()])));
    assert_eq!(of(lib.artists[1]), Some((2, vec!["jazz".to_owned()])));
    assert_eq!(
        of(lib.artists[2]),
        Some((2, vec!["pop".to_owned(), "rock".to_owned()]))
    );
    assert_eq!(of(lib.artists[3]), Some((2, vec![])));
    assert_eq!(of(lib.artists[4]), Some((2, vec![])));
    assert_eq!(of(lonely), None);
    assert_eq!(got.len(), 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn progress_counts_current_feel_and_current_map(pool: PgPool) {
    let lib = library(&pool, 3, 1).await;
    let p = probe("4444444444444444", 0.5);
    assert_eq!(
        matching_progress(&pool, &p).await.unwrap(),
        Progress {
            total: 3,
            completed: 0
        }
    );
    build_matching(&pool, &p).await.unwrap();
    assert_eq!(
        matching_progress(&pool, &p).await.unwrap().completed,
        0,
        "no map yet"
    );

    for (i, id) in lib.tracks.iter().enumerate() {
        // The third map row is stale: it was laid out for an older embedding.
        let stamp = if i < 2 {
            "(SELECT created_at FROM track_embeddings WHERE track_id = $1)"
        } else {
            "(SELECT created_at - interval '1 hour' FROM track_embeddings WHERE track_id = $1)"
        };
        sqlx::query(&format!(
            "INSERT INTO matching_map (track_id, embedded_at, x, y, z, colour, laid_out) VALUES ($1, {stamp}, 0, 0, 0, 0, true)"
        ))
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }
    assert_eq!(
        matching_progress(&pool, &p).await.unwrap(),
        Progress {
            total: 3,
            completed: 2
        }
    );
    // Feel from another probe is not current.
    assert_eq!(
        matching_progress(&pool, &probe("5555555555555555", 0.5))
            .await
            .unwrap()
            .completed,
        0
    );
}

/// Task 3 step 3, deferred to here: the plans of the feel join and the map count on 10k rows. Prints the plans
/// (`cargo test --test vibe_state explain -- --ignored --nocapture`); asserts only that they run.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
#[ignore = "prints query plans for the commit body"]
async fn explain_feel_join_and_map_count_on_10k_rows(pool: PgPool) {
    for sql in [
        "INSERT INTO artists (id, name, name_key) SELECT gen_random_uuid(), 'a' || s, 'a' || s FROM generate_series(1, 1000) s",
        "INSERT INTO albums (id, artist_id, title, title_key)
           SELECT gen_random_uuid(), a.id, 't', gen_random_uuid()::text FROM artists a",
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
           SELECT gen_random_uuid(), al.id, 'a', 't', 'flac', gen_random_uuid()::text, 1, now(), '\\x00'
           FROM albums al, generate_series(1, 10) s",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    let (model, sha, pipeline) = identity();
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding)
         SELECT t.id, 0, $1, $2, $3,
                l2_normalize(x.v) FROM tracks t,
                LATERAL (SELECT array_agg(random())::real[]::vector AS v FROM generate_series(1, 511 + t.size)) x",
    )
    .bind(model)
    .bind(sha)
    .bind(pipeline)
    .execute(&pool)
    .await
    .unwrap();
    let p = probe("6666666666666666", 0.5);
    build_matching(&pool, &p).await.unwrap();
    sqlx::query(
        "INSERT INTO matching_map (track_id, embedded_at, x, y, z, colour, laid_out)
         SELECT track_id, created_at, 0, 0, 0, 0, true FROM track_embeddings",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE").execute(&pool).await.unwrap();

    let eligible: &str = &kima_db::vibe::ELIGIBLE;
    let plans = [
        ("feel join (stale-feel selection)", format!(
            "SELECT te.track_id FROM {eligible} AND NOT EXISTS (SELECT 1 FROM matching_feel f
               WHERE f.track_id = te.track_id AND f.embedded_at = te.created_at AND f.probe = '6666666666666666')")),
        ("map count (progress)", format!(
            "WITH e AS (SELECT te.track_id, te.created_at FROM {eligible})
             SELECT count(*) FILTER (WHERE EXISTS (SELECT 1 FROM matching_map m WHERE m.track_id = e.track_id
               AND m.embedded_at = e.created_at)) FROM e")),
    ];
    for (name, sql) in plans {
        let rows: Vec<(String,)> =
            sqlx::query_as(&format!("EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) {sql}"))
                .fetch_all(&pool)
                .await
                .unwrap();
        println!("== {name}");
        rows.iter().for_each(|r| println!("{}", r.0));
    }
}
