//! Ranking queries (M5 Task 6, spec 6.6) against a real Postgres, compared with the pure formula crate.
mod vibe_common;
use kima_db::PgPool;
use kima_db::genres::{GenreSource, replace_artist_genres};
use kima_db::vibe::rank::{
    Counted, MatchingContext, RankError, WeightedInput, artist_profiles, check_blend,
    library_tracks, matching_context, rank_matches, score_library, top_scores, weighted_tracks,
};
use kima_db::vibe::state::build_matching;
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use kima_vibe::formula::{self, MatchTrack, Place, SELF_SCORE, WeightedTrack};
use uuid::Uuid;
use vibe_common::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as f64 / (1u64 << 31) as f64 * 2.0 - 1.0
    }
}

fn unit_vec(v: &[f64]) -> Vec<f32> {
    let n = v.iter().map(|a| a * a).sum::<f64>().sqrt();
    v.iter().map(|a| (a / n) as f32).collect()
}

/// A vector near its artist's centre, so one artist's tracks rank close together and the caps bite.
fn clustered(centre: &[f64], rng: &mut Rng) -> Vec<f32> {
    let v: Vec<f64> = centre.iter().map(|c| 2.0 * c + 0.7 * rng.next()).collect();
    unit_vec(&v)
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
        probe_id: "0123456789abcdef".into(),
    }
}

fn feel_of(e: &[f32], axis: &ProbeAxis) -> f64 {
    let norm = e.iter().map(|a| f64::from(*a).powi(2)).sum::<f64>().sqrt();
    let dot: f64 = e
        .iter()
        .zip(&axis.weights)
        .map(|(a, w)| f64::from(*a) * w)
        .sum();
    (dot / norm + axis.bias).clamp(0.0, 1.0)
}

struct Row {
    id: Uuid,
    album: Uuid,
    artist: Uuid,
    vector: Vec<f32>,
}

struct Lib {
    rows: Vec<Row>,
}

impl Lib {
    fn idx(&self, id: Uuid) -> usize {
        self.rows.iter().position(|r| r.id == id).unwrap()
    }
}

/// `artists` artists, each with two albums of 3 and 2 tracks (five per artist), clustered per artist; every
/// third artist is tagged with genres. Built through `build_matching`, so the state row exists.
async fn library(pool: &PgPool, artists: usize) -> Lib {
    let mut rng = Rng(7);
    let mut lib = Lib { rows: vec![] };
    for a in 0..artists {
        let artist = artist(pool, &format!("artist {a}")).await;
        let centre: Vec<f64> = (0..512).map(|_| rng.next()).collect();
        for tracks in [3, 2] {
            let album = album(pool, artist).await;
            for _ in 0..tracks {
                let id = track_in(pool, album).await;
                let vector = clustered(&centre, &mut rng);
                embed(pool, id, &vector).await;
                lib.rows.push(Row {
                    id,
                    album,
                    artist,
                    vector,
                });
            }
        }
        if a % 3 == 0 {
            let names: &[&str] = if a % 2 == 0 {
                &["Rock", "Pop"]
            } else {
                &["Jazz"]
            };
            let names: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
            let mut conn = pool.acquire().await.unwrap();
            replace_artist_genres(&mut conn, artist, GenreSource::MusicBrainz, &names)
                .await
                .unwrap();
        }
    }
    build_matching(pool, &probe()).await.unwrap();
    lib
}

async fn ctx_for(pool: &PgPool, ids: &[Uuid]) -> MatchingContext {
    matching_context(pool, &probe(), ids)
        .await
        .unwrap()
        .expect("state built")
}

fn input(id: Uuid, weight: f64) -> WeightedInput {
    WeightedInput {
        track_id: id,
        weight,
    }
}

/// The formula's view of every track, independent of the SQL under test: f64 feel from the vectors.
struct Reference {
    tracks: Vec<MatchTrack>,
}

fn reference(lib: &Lib, ctx: &MatchingContext) -> Reference {
    let p = probe();
    Reference {
        tracks: lib
            .rows
            .iter()
            .map(|r| MatchTrack {
                track_id: r.id.to_string(),
                album_id: r.album.to_string(),
                artist_id: r.artist.to_string(),
                sound: formula::unit(&r.vector.iter().map(|x| f64::from(*x)).collect::<Vec<_>>()),
                feel: formula::feel_vector(
                    [
                        feel_of(&r.vector, &p.valence),
                        feel_of(&r.vector, &p.arousal),
                    ],
                    &ctx.state.feel,
                ),
            })
            .collect(),
    }
}

impl Reference {
    fn get(&self, id: Uuid) -> &MatchTrack {
        self.tracks
            .iter()
            .find(|t| t.track_id == id.to_string())
            .unwrap()
    }

    /// `sum_i w_i s(t, i)` for every track not excluded, best first (score desc, then id).
    fn ranked(
        &self,
        ctx: &MatchingContext,
        inputs: &[WeightedInput],
        exclude: &[Uuid],
    ) -> Vec<(MatchTrack, f64)> {
        let weighted: Vec<WeightedTrack> = inputs
            .iter()
            .map(|i| WeightedTrack {
                track: self.get(i.track_id),
                weight: i.weight,
            })
            .collect();
        let mut out: Vec<(MatchTrack, f64)> = self
            .tracks
            .iter()
            .filter(|t| !exclude.iter().any(|e| e.to_string() == t.track_id))
            .map(|t| {
                let s: f64 = weighted
                    .iter()
                    .map(|w| {
                        w.weight
                            * formula::pair_score(
                                t,
                                w.track,
                                &ctx.state.genres,
                                ctx.state.genre_weight_sq(),
                            )
                    })
                    .sum();
                (t.clone(), s)
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.track_id.cmp(&b.0.track_id)));
        out
    }
}

fn ids(v: &[kima_db::vibe::rank::RankedTrack]) -> Vec<Uuid> {
    v.iter().map(|r| r.track.id).collect()
}

fn pick(lib: &Lib, n: &[usize]) -> Vec<Uuid> {
    n.iter().map(|i| lib.rows[*i].id).collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn scores_every_eligible_track_as_the_formula_does(pool: PgPool) {
    assert!(current_database(&pool).await.starts_with("_sqlx_test"));
    let lib = library(&pool, 40).await; // 200 tracks
    let seed = lib.rows[7].id;
    let ctx = ctx_for(&pool, &[seed]).await;
    assert!(ctx.state.genre_weight > 0.0, "the genre term is exercised");
    let reference = reference(&lib, &ctx);
    let inputs = [input(seed, 1.0)];

    let scored = score_library(&pool, &ctx, &[inputs.to_vec()], &[seed])
        .await
        .unwrap();
    assert_eq!(scored.len(), 199);
    let want = reference.ranked(&ctx, &inputs, &[seed]);
    for s in &scored {
        let r = want
            .iter()
            .find(|(t, _)| t.track_id == s.id.to_string())
            .unwrap();
        assert!(
            (s.scores[0] - r.1).abs() <= 1e-5,
            "{}: sql {} vs formula {}",
            s.id,
            s.scores[0],
            r.1
        );
        let row = &lib.rows[lib.idx(s.id)];
        assert_eq!((s.album_id, s.artist_id), (row.album, row.artist));
    }

    // The order is the formula's, except where two scores are closer than the tolerance.
    let top = top_scores(&pool, &ctx, &inputs, 60, &[seed]).await.unwrap();
    assert_eq!(top.len(), 60);
    for (k, (id, score)) in top.iter().enumerate() {
        assert!((score - want[k].1).abs() <= 1e-5);
        if k > 0 && (want[k - 1].1 - want[k].1).abs() > 2e-5 {
            assert_eq!(id.to_string(), want[k].0.track_id, "rank {k}");
        }
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn returns_the_capped_list_a_walk_down_the_ranking_gives(pool: PgPool) {
    let lib = library(&pool, 40).await;
    let seed = lib.rows[12].id;
    let ctx = ctx_for(&pool, &[seed]).await;
    let reference = reference(&lib, &ctx);
    let inputs = [input(seed, 1.0)];

    let ranked = reference.ranked(&ctx, &inputs, &[seed]);
    let seed_place = [Place {
        album_id: lib.rows[12].album.to_string(),
        artist_id: lib.rows[12].artist.to_string(),
    }];
    let walk = formula::cap_list(ranked.iter().map(|(t, _)| t.clone()), &seed_place, 20);
    let uncapped: Vec<&str> = ranked
        .iter()
        .take(20)
        .map(|(t, _)| t.track_id.as_str())
        .collect();
    assert_ne!(
        uncapped,
        walk.iter().map(|t| t.track_id.as_str()).collect::<Vec<_>>(),
        "the fixture must make the caps bite"
    );
    let got = rank_matches(&pool, &ctx, &inputs, 20, &[seed])
        .await
        .unwrap();
    assert_eq!(
        ids(&got).iter().map(Uuid::to_string).collect::<Vec<_>>(),
        walk.iter().map(|t| t.track_id.clone()).collect::<Vec<_>>()
    );
    // The caps really bit: the seed's own album/artist shows at most what is left.
    let seed_artist = lib.rows[12].artist;
    assert!(
        got.iter()
            .filter(|r| r.track.artist_id == seed_artist)
            .count()
            <= 3
    );
    assert!(got.windows(2).all(|w| w[0].score >= w[1].score));
    assert!(!ids(&got).contains(&seed));
    // The row carries the list columns.
    assert_eq!(got[0].track.title, "t");
    assert!(got[0].track.artist_name.starts_with("artist "));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn alchemy_blend_add_tracks_take_places(pool: PgPool) {
    let lib = library(&pool, 40).await;
    let add = pick(&lib, &[3, 4, 50]); // 3 and 4 share artist 0 (and album)
    let sub = pick(&lib, &[120, 121]);
    let all: Vec<Uuid> = add.iter().chain(&sub).copied().collect();
    let ctx = ctx_for(&pool, &all).await;
    let reference = reference(&lib, &ctx);
    let mut inputs: Vec<WeightedInput> = add.iter().map(|i| input(*i, 1.0 / 3.0)).collect();
    inputs.extend(sub.iter().map(|i| input(*i, -0.5)));

    let blend_sq = check_blend(&ctx, &inputs).unwrap();
    assert!(blend_sq > 0.0);
    let want_sq = formula::blend_self_score(
        &weighted_tracks(&ctx, &inputs).unwrap(),
        &ctx.state.genres,
        ctx.state.genre_weight_sq(),
    );
    assert_eq!(blend_sq, want_sq);
    assert!((blend_sq * SELF_SCORE).sqrt() > 0.0);

    let ranked = reference.ranked(&ctx, &inputs, &all);
    let counted: Vec<Place> = add
        .iter()
        .map(|i| {
            let r = &lib.rows[lib.idx(*i)];
            Place {
                album_id: r.album.to_string(),
                artist_id: r.artist.to_string(),
            }
        })
        .collect();
    let walk = formula::cap_list(ranked.iter().map(|(t, _)| t.clone()), &counted, 15);
    let got = rank_matches(&pool, &ctx, &inputs, 15, &add).await.unwrap();
    assert_eq!(
        ids(&got).iter().map(Uuid::to_string).collect::<Vec<_>>(),
        walk.iter().map(|t| t.track_id.clone()).collect::<Vec<_>>()
    );
    for r in &got {
        assert!(!all.contains(&r.track.id), "inputs are never offered");
        let want = ranked
            .iter()
            .find(|(t, _)| t.track_id == r.track.id.to_string())
            .unwrap();
        assert!((r.score - want.1).abs() <= 1e-5);
    }
    // The two added tracks of artist 0 took two places of its four: at most 2 more of that artist.
    let artist0 = lib.rows[3].artist;
    assert!(got.iter().filter(|r| r.track.artist_id == artist0).count() <= 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refuses_cancelling_blend_and_unmatched_tracks(pool: PgPool) {
    let lib = library(&pool, 6).await;
    let same = lib.rows[0].id;
    // A track of another checkpoint has no matching data.
    let other = track(&pool).await;
    embed_as(&pool, other.track, "other", &[7; 32], "p", &axis_vec())
        .await
        .unwrap();
    let ctx = ctx_for(&pool, &[same, other.track]).await;
    assert_eq!(ctx.missing(&[same, other.track]), vec![other.track]);

    let cancel = [input(same, 1.0), input(same, -1.0)];
    assert!(matches!(
        check_blend(&ctx, &cancel),
        Err(RankError::CancelsOut)
    ));
    let unmatched = [input(other.track, 1.0)];
    assert!(matches!(
        weighted_tracks(&ctx, &unmatched),
        Err(RankError::NoMatchingData(m)) if m == vec![other.track]
    ));
    assert!(matches!(
        rank_matches(&pool, &ctx, &unmatched, 5, &[other.track]).await,
        Err(RankError::NoMatchingData(_))
    ));
    // Counted tracks need matching data too.
    assert!(matches!(
        rank_matches(&pool, &ctx, &[input(same, 1.0)], 5, &[other.track]).await,
        Err(RankError::NoMatchingData(_))
    ));
}

fn axis_vec() -> Vec<f32> {
    vibe_common::axis(5)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn without_a_built_state_there_is_no_context(pool: PgPool) {
    let t = track(&pool).await;
    embed(&pool, t.track, &vibe_common::axis(1)).await;
    assert!(
        matching_context(&pool, &probe(), &[t.track])
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn never_offers_missing_file(pool: PgPool) {
    let lib = library(&pool, 10).await;
    let seed = lib.rows[0].id;
    let ctx = ctx_for(&pool, &[seed]).await;
    let inputs = [input(seed, 1.0)];
    let before = rank_matches(&pool, &ctx, &inputs, 10, &[seed])
        .await
        .unwrap();
    let gone = before[0].track.id;
    // No rebuild in between: the file going missing hides the track at once.
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    let after = rank_matches(&pool, &ctx, &inputs, 10, &[seed])
        .await
        .unwrap();
    assert!(!ids(&after).contains(&gone));
    assert!(
        !top_scores(&pool, &ctx, &inputs, 100, &[])
            .await
            .unwrap()
            .iter()
            .any(|(id, _)| *id == gone)
    );
    assert!(
        !score_library(&pool, &ctx, &[inputs.to_vec()], &[])
            .await
            .unwrap()
            .iter()
            .any(|s| s.id == gone)
    );
}

/// The file-change trigger deletes the vector of a track that goes missing, which hides it anyway; with the trigger
/// off, the `missing_since` condition of `ELIGIBLE` alone must keep the track out of every ranking.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_file_is_filtered_even_when_its_vector_survives(pool: PgPool) {
    let lib = library(&pool, 10).await;
    let seed = lib.rows[0].id;
    let ctx = ctx_for(&pool, &[seed]).await;
    let inputs = [input(seed, 1.0)];
    let gone = rank_matches(&pool, &ctx, &inputs, 1, &[seed])
        .await
        .unwrap()[0]
        .track
        .id;
    sqlx::query("ALTER TABLE tracks DISABLE TRIGGER tracks_ml_identity")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        count(&pool, "track_embeddings").await,
        lib.rows.len() as i64
    );
    assert!(
        !ids(&rank_matches(&pool, &ctx, &inputs, 50, &[seed])
            .await
            .unwrap())
        .contains(&gone)
    );
    assert!(ctx_for(&pool, &[gone]).await.missing(&[gone]).len() == 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ignores_vector_of_other_model(pool: PgPool) {
    let lib = library(&pool, 10).await;
    let seed = lib.rows[0].id;
    let ctx = ctx_for(&pool, &[seed]).await;
    let inputs = [input(seed, 1.0)];
    let best = rank_matches(&pool, &ctx, &inputs, 1, &[seed])
        .await
        .unwrap()[0]
        .track
        .id;
    // Re-embed the best match under another checkpoint (a new `created_at` too): it vanishes from every ranking.
    embed_as(
        &pool,
        best,
        "other",
        &[7; 32],
        "p",
        &lib.rows[lib.idx(best)].vector,
    )
    .await
    .unwrap();
    let again = rank_matches(&pool, &ctx, &inputs, 50, &[seed])
        .await
        .unwrap();
    assert!(!ids(&again).contains(&best));
    assert_eq!(
        score_library(&pool, &ctx, &[inputs.to_vec()], &[seed])
            .await
            .unwrap()
            .len(),
        lib.rows.len() - 2
    );
    assert!(ctx_for(&pool, &[best]).await.missing(&[best]).len() == 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_profiles_average_each_artists_tracks(pool: PgPool) {
    let lib = library(&pool, 12).await;
    let ctx = ctx_for(&pool, &[]).await;
    let reference = reference(&lib, &ctx);
    let wanted = [lib.rows[0].artist, lib.rows[20].artist];
    let got = artist_profiles(&pool, &probe(), &ctx.state, &wanted)
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    for a in wanted {
        let mine: Vec<&MatchTrack> = reference
            .tracks
            .iter()
            .filter(|t| t.artist_id == a.to_string())
            .collect();
        assert_eq!(mine.len(), 5);
        let n = mine.len() as f64;
        let mut sound = vec![0.0; 512];
        let (mut v, mut r) = (0.0, 0.0);
        for t in &mine {
            for (s, x) in sound.iter_mut().zip(&t.sound) {
                *s += x / n;
            }
            v += t.feel[0] / n;
            r += t.feel[1] / n;
        }
        let p = &got[&a];
        assert_eq!(p.artist_id, a.to_string());
        assert_eq!(p.sound.len(), 512);
        for (x, y) in p.sound.iter().zip(&sound) {
            assert!((x - y).abs() < 1e-5);
        }
        assert!((p.feel[0] - v).abs() < 1e-5 && (p.feel[1] - r).abs() < 1e-5);
    }
    // An artist without eligible tracks has no profile.
    let none = artist(&pool, "nobody").await;
    assert!(
        artist_profiles(&pool, &probe(), &ctx.state, &[none])
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn top_scores_includes_inputs_unless_excluded(pool: PgPool) {
    let lib = library(&pool, 10).await;
    let seed = lib.rows[3].id;
    let ctx = ctx_for(&pool, &[seed]).await;
    let inputs = [input(seed, 1.0)];
    let with = top_scores(&pool, &ctx, &inputs, 5, &[]).await.unwrap();
    assert_eq!(with.len(), 5);
    assert_eq!(with[0].0, seed, "an input is among its own closest matches");
    assert!(with[0].1 > 1.0 && with[0].1 <= SELF_SCORE + 1e-5);
    let without = top_scores(&pool, &ctx, &inputs, 5, &[seed]).await.unwrap();
    assert!(!without.iter().any(|(id, _)| *id == seed));
    assert_eq!(without.len(), 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn score_library_one_pass_two_sets(pool: PgPool) {
    let lib = library(&pool, 10).await;
    let (a, b) = (lib.rows[1].id, lib.rows[30].id);
    let ctx = ctx_for(&pool, &[a, b]).await;
    let set_a = vec![input(a, 1.0)];
    let set_b = vec![input(b, 1.0)];
    let both = score_library(&pool, &ctx, &[set_a.clone(), set_b.clone()], &[a, b])
        .await
        .unwrap();
    assert_eq!(both.len(), lib.rows.len() - 2);
    let only_a = score_library(&pool, &ctx, &[set_a], &[a, b]).await.unwrap();
    let only_b = score_library(&pool, &ctx, &[set_b], &[a, b]).await.unwrap();
    for s in &both {
        assert_eq!(s.scores.len(), 2);
        let x = only_a.iter().find(|o| o.id == s.id).unwrap();
        let y = only_b.iter().find(|o| o.id == s.id).unwrap();
        assert!((s.scores[0] - x.scores[0]).abs() < 1e-12);
        assert!((s.scores[1] - y.scores[0]).abs() < 1e-12);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ties_by_track_id(pool: PgPool) {
    // Three artists; the best match for the seed is two identical vectors, one on each of two albums of artist 1.
    let seed_a = artist(&pool, "seed").await;
    let near = artist(&pool, "near").await;
    let far = artist(&pool, "far").await;
    let (s_al, n_al1, n_al2, f_al) = (
        album(&pool, seed_a).await,
        album(&pool, near).await,
        album(&pool, near).await,
        album(&pool, far).await,
    );
    let mut rng = Rng(99);
    let centre: Vec<f64> = (0..512).map(|_| rng.next()).collect();
    let twin = unit_vec(&centre);
    let seed = track_in(&pool, s_al).await;
    embed(&pool, seed, &clustered(&centre, &mut rng)).await;
    let (t1, t2) = (track_in(&pool, n_al1).await, track_in(&pool, n_al2).await);
    for t in [t1, t2] {
        embed(&pool, t, &twin).await;
    }
    for _ in 0..4 {
        let t = track_in(&pool, f_al).await;
        let c: Vec<f64> = (0..512).map(|_| rng.next()).collect();
        embed(&pool, t, &unit_vec(&c)).await;
    }
    build_matching(&pool, &probe()).await.unwrap();
    let ctx = ctx_for(&pool, &[seed]).await;
    let inputs = [input(seed, 1.0)];
    let (lo, hi) = if t1 < t2 { (t1, t2) } else { (t2, t1) };

    let top = top_scores(&pool, &ctx, &inputs, 3, &[seed]).await.unwrap();
    assert_eq!(top[0].1, top[1].1, "identical vectors score identically");
    assert_eq!((top[0].0, top[1].0), (lo, hi));
    let ranked = rank_matches(&pool, &ctx, &inputs, 3, &[seed])
        .await
        .unwrap();
    assert_eq!((ranked[0].track.id, ranked[1].track.id), (lo, hi));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_tracks_names_album_and_artist(pool: PgPool) {
    let t = track(&pool).await;
    let got = library_tracks(&pool, &[t.track, Uuid::now_v7()])
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    let g = &got[0];
    assert_eq!(
        (g.id, g.album_id, g.artist_id, g.artist_name.as_str()),
        (t.track, t.album, t.artist, "a")
    );
    let _ = Counted {
        album_id: g.album_id,
        artist_id: g.artist_id,
    };
}
