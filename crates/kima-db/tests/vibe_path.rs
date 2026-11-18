//! Song path service (M5 Task 7b, spec 6.7) against a real Postgres.
mod vibe_common;
use kima_db::PgPool;
use kima_db::vibe::path::{PathError, PathOptions, generate_song_path};
use kima_db::vibe::rank::{RankError, WeightedInput, matching_context};
use kima_db::vibe::state::build_matching;
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use kima_vibe::formula::{self, MatchTrack, SELF_SCORE};
use kima_vibe::song_path::{self, Mode, ScoredTrack};
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

fn probe() -> FeelProbe {
    let axis = |scale: f64, bias: f64| ProbeAxis {
        alpha: 1.0,
        bias,
        weights: (0..512)
            .map(|i| (((i * 7) % 13) as f64 / 13.0 - 0.5) * scale)
            .collect(),
    };
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

/// `artists` artists with 4 tracks each, vectors spread along a smooth curve in a random 2-D subspace so that
/// there is a real arc between the ends and plenty of mid-way tracks.
async fn library(pool: &PgPool, artists: usize) -> Vec<Row> {
    let mut rng = Rng(11);
    let basis: Vec<Vec<f64>> = (0..3)
        .map(|_| (0..512).map(|_| rng.next()).collect())
        .collect();
    let mut rows = vec![];
    for a in 0..artists {
        let artist = artist(pool, &format!("artist {a}")).await;
        let album = album(pool, artist).await;
        for _ in 0..4 {
            let angle = (a as f64 + 0.3 * rng.next()) / artists as f64 * std::f64::consts::PI;
            let v: Vec<f64> = (0..512)
                .map(|i| {
                    angle.cos() * basis[0][i]
                        + angle.sin() * basis[1][i]
                        + 0.35 * rng.next() * basis[2][i]
                })
                .collect();
            let vector = unit_vec(&v);
            let id = track_in(pool, album).await;
            embed(pool, id, &vector).await;
            rows.push(Row {
                id,
                album,
                artist,
                vector,
            });
        }
    }
    build_matching(pool, &probe()).await.unwrap();
    rows
}

fn match_track(r: &Row, stats: &formula::FeelStats) -> MatchTrack {
    let p = probe();
    MatchTrack {
        track_id: r.id.to_string(),
        album_id: r.album.to_string(),
        artist_id: r.artist.to_string(),
        sound: formula::unit(&r.vector.iter().map(|x| f64::from(*x)).collect::<Vec<_>>()),
        feel: formula::feel_vector(
            [
                feel_of(&r.vector, &p.valence),
                feel_of(&r.vector, &p.arousal),
            ],
            stats,
        ),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn walks_an_arc_between_two_tracks(pool: PgPool) {
    assert!(current_database(&pool).await.starts_with("_sqlx_test"));
    let rows = library(&pool, 30).await;
    let (start, end) = (&rows[0], &rows[rows.len() - 1]);
    let path = generate_song_path(
        &pool,
        &probe(),
        start.id,
        end.id,
        PathOptions {
            length: Some(12),
            mode: Mode::Smooth,
        },
        |_| 0,
    )
    .await
    .unwrap();

    // The same walk on the f64 reference of every track.
    let ctx = matching_context(&pool, &probe(), &[start.id, end.id])
        .await
        .unwrap()
        .unwrap();
    let tracks: Vec<MatchTrack> = rows
        .iter()
        .map(|r| match_track(r, &ctx.state.feel))
        .collect();
    let gw = ctx.state.genre_weight_sq();
    let (s, e) = (&tracks[0], &tracks[tracks.len() - 1]);
    let (cos_theta, distance) =
        song_path::cos_and_distance(formula::pair_score(s, e, &ctx.state.genres, gw));
    let scored: Vec<ScoredTrack> = tracks
        .iter()
        .skip(1)
        .take(tracks.len() - 2)
        .map(|t| ScoredTrack {
            id: t.track_id.clone(),
            artist_id: t.artist_id.clone(),
            s0: formula::pair_score(t, s, &ctx.state.genres, gw),
            s1: formula::pair_score(t, e, &ctx.state.genres, gw),
        })
        .collect();
    let want = song_path::walk(&scored, cos_theta, 12, Mode::Smooth, |_| 0);
    assert_eq!(want.len(), 12);
    assert_eq!(
        path.path
            .iter()
            .map(|t| t.id.to_string())
            .collect::<Vec<_>>(),
        want.iter().map(|p| p.id.clone()).collect::<Vec<_>>()
    );
    assert!((path.embedding_distance - distance).abs() < 1e-5);
    assert_eq!((path.start.id, path.end.id), (start.id, end.id));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn path_metadata_fields(pool: PgPool) {
    let rows = library(&pool, 30).await;
    let (start, end) = (&rows[0], &rows[rows.len() - 1]);
    let mut picks = vec![];
    let path = generate_song_path(
        &pool,
        &probe(),
        start.id,
        end.id,
        PathOptions {
            length: None,
            mode: Mode::Discovery,
        },
        |bound| {
            picks.push(bound);
            1.min(bound - 1)
        },
    )
    .await
    .unwrap();
    // Default length is max(8, min(50, round(distance * 15))).
    let want_len = song_path::waypoint_count(path.embedding_distance, None);
    assert_eq!(path.path.len(), want_len);
    assert!((8..=50).contains(&want_len));
    assert_eq!(path.total_tracks, path.path.len() + 2);
    assert_eq!(path.mode, Mode::Discovery);
    assert_eq!(path.mode.as_str(), "discovery");
    assert_eq!(
        picks.len(),
        want_len,
        "discovery asks the injected pick once per waypoint"
    );
    assert!(picks.iter().all(|b| *b <= 3));
    assert!(path.average_step_size > 0.0 && path.average_step_size < 1.5);
    // Every track is a full list row.
    for t in path.path.iter().chain([&path.start, &path.end]) {
        assert_eq!(t.title, "t");
        assert!(t.artist_name.starts_with("artist "));
    }
    // The picked tracks are distinct.
    let mut ids: Vec<Uuid> = path.path.iter().map(|t| t.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), path.path.len());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refuses_track_without_matching_data(pool: PgPool) {
    let rows = library(&pool, 6).await;
    let other = track(&pool).await;
    embed_as(&pool, other.track, "other", &[7; 32], "p", &axis(3))
        .await
        .unwrap();
    for (a, b) in [
        (rows[0].id, other.track),
        (other.track, rows[0].id),
        (rows[0].id, Uuid::now_v7()),
    ] {
        let err = generate_song_path(&pool, &probe(), a, b, PathOptions::default(), |_| 0)
            .await
            .unwrap_err();
        assert!(matches!(err, PathError::NotMatched), "{err}");
        assert_eq!(
            err.to_string(),
            "One or both tracks have not been through the similarity step yet"
        );
        assert_eq!(err.status(), 400);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refuses_a_path_before_any_state_exists(pool: PgPool) {
    let t = track(&pool).await;
    embed(&pool, t.track, &axis(1)).await;
    let u = track(&pool).await;
    embed(&pool, u.track, &axis(2)).await;
    let err = generate_song_path(
        &pool,
        &probe(),
        t.track,
        u.track,
        PathOptions::default(),
        |_| 0,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PathError::NotMatched));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refuses_too_similar_tracks(pool: PgPool) {
    let rows = library(&pool, 10).await;
    // A twin of the first track under another artist: the same direction, distance ~0.
    let twin_artist = artist(&pool, "twin").await;
    let twin_album = album(&pool, twin_artist).await;
    let twin = track_in(&pool, twin_album).await;
    embed(&pool, twin, &rows[0].vector).await;
    build_matching(&pool, &probe()).await.unwrap();
    let err = generate_song_path(
        &pool,
        &probe(),
        rows[0].id,
        twin,
        PathOptions::default(),
        |_| 0,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PathError::TooSimilar), "{err}");
    assert_eq!(
        err.to_string(),
        "These tracks are too similar for a meaningful journey. Try \"Similar Tracks\" instead."
    );
    assert_eq!(err.status(), 400);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_scoring_excludes_both_endpoints(pool: PgPool) {
    // A tiny library: asking for more waypoints than there are other tracks ends early, never repeating a track
    // and never offering an endpoint.
    let rows = library(&pool, 5).await; // 20 tracks
    let (start, end) = (&rows[0], &rows[rows.len() - 1]);
    let path = generate_song_path(
        &pool,
        &probe(),
        start.id,
        end.id,
        PathOptions {
            length: Some(50),
            mode: Mode::Smooth,
        },
        |_| 0,
    )
    .await
    .unwrap();
    assert_eq!(path.path.len(), 18);
    assert!(path.path.iter().all(|t| t.id != start.id && t.id != end.id));
    assert_eq!(path.total_tracks, 20);
    let _ = (
        SELF_SCORE,
        RankError::CancelsOut.to_string(),
        WeightedInput {
            track_id: start.id,
            weight: 1.0,
        },
    );
}
