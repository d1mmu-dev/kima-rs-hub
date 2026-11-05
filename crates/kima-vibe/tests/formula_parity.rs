//! The formula against `parity.json` (written by the measurement's own Python code), then the pieces the engine
//! adds on top: the caps, the weighted-set algebra and the arc weights. Ports of upstream `formula.test.ts`.
mod common;
use common::*;
use kima_vibe::formula::*;
use std::collections::HashMap;

/// `toBeCloseTo(x, 12)`.
const CLOSE: f64 = 5e-13;

struct Fixture {
    json: serde_json::Value,
    tracks: Vec<MatchTrack>,
    stats: FeelStats,
    model: GenreModel,
    genre_spread: f64,
    sound_spread: f64,
    wg_sq: f64,
}

fn f(v: &serde_json::Value) -> f64 {
    v.as_f64().expect("number")
}

fn load() -> Fixture {
    let json = fixture("parity.json");
    let raw: Vec<[f64; 2]> = json["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| [f(&t["valence"]), f(&t["arousal"])])
        .collect();
    let n = raw.len() as f64;
    let mean = |k: usize| raw.iter().fold(0.0, |s, r| s + r[k]) / n;
    let sd = |k: usize, m: f64| (raw.iter().fold(0.0, |s, r| s + (r[k] - m).powi(2)) / n).sqrt();
    let (m0, m1) = (mean(0), mean(1));
    let stats = FeelStats {
        mean: [m0, m1],
        sd: [sd(0, m0), sd(1, m1)],
    };
    let tracks: Vec<MatchTrack> = json["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(&raw)
        .map(|(t, r)| MatchTrack {
            track_id: t["id"].as_str().unwrap().into(),
            album_id: t["albumId"].as_str().unwrap().into(),
            artist_id: t["artistId"].as_str().unwrap().into(),
            sound: unit(
                &t["sound"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(f)
                    .collect::<Vec<_>>(),
            ),
            feel: feel_vector(*r, &stats),
        })
        .collect();
    let artists: Vec<LibraryArtist> = json["artists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            let id = a["artistId"].as_str().unwrap();
            let genres: Vec<&str> = a["genres"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g.as_str().unwrap())
                .collect();
            LibraryArtist {
                artist_id: id.into(),
                tracks: tracks.iter().filter(|t| t.artist_id == id).count() as u64,
                genres: MbGenres::bundled().matching_genres(&genres),
            }
        })
        .collect();
    let (model, genre_spread) = build_genre_model(&artists);
    let dim = tracks[0].sound.len();
    let mean_sound: Vec<f64> = (0..dim)
        .map(|d| tracks.iter().fold(0.0, |s, t| s + t.sound[d]) / tracks.len() as f64)
        .collect();
    let sound_spread = 1.0 - dot(&mean_sound, &mean_sound);
    let wg_sq = genre_weight(sound_spread, genre_spread).powi(2);
    Fixture {
        json,
        tracks,
        stats,
        model,
        genre_spread,
        sound_spread,
        wg_sq,
    }
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < CLOSE, "{what}: {a} vs {b}");
}

fn ranked<'a>(
    fx: &'a Fixture,
    score: impl Fn(&MatchTrack) -> f64,
    exclude: &[&str],
) -> Vec<(&'a MatchTrack, f64)> {
    let mut v: Vec<_> = fx
        .tracks
        .iter()
        .filter(|t| !exclude.contains(&t.track_id.as_str()))
        .map(|t| (t, score(t)))
        .collect();
    v.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap()
            .then_with(|| a.0.track_id.cmp(&b.0.track_id))
    });
    v
}

struct Row<'a>(&'a MatchTrack);
impl Placed for Row<'_> {
    fn album_id(&self) -> &str {
        &self.0.album_id
    }
    fn artist_id(&self) -> &str {
        &self.0.artist_id
    }
}

fn place_of(t: &MatchTrack) -> Place {
    Place {
        album_id: t.album_id.clone(),
        artist_id: t.artist_id.clone(),
    }
}

#[test]
fn standardises_feel_with_library_mean_and_population_sd() {
    let fx = load();
    for k in 0..2 {
        close(fx.stats.mean[k], f(&fx.json["feel"]["mean"][k]), "mean");
        close(fx.stats.sd[k], f(&fx.json["feel"]["sd"][k]), "sd");
    }
}

#[test]
fn matching_genres_keeps_mb_spelling_and_builds_same_genre_vectors() {
    let fx = load();
    let want: Vec<&str> = fx.json["vocabulary"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(fx.model.vocabulary, want);
    for (artist, expected) in fx.json["genreVectors"].as_object().unwrap() {
        let g =
            genre_vector(&fx.model, artist).unwrap_or_else(|| vec![0.0; fx.model.vocabulary.len()]);
        for (i, x) in expected.as_array().unwrap().iter().enumerate() {
            close(g[i], f(x), &format!("{artist}[{i}]"));
        }
    }
    let m = MbGenres::bundled();
    assert_eq!(
        m.matching_genres(&["Rock", "Synth-Pop", "darkwave", "not a genre", "rock"]),
        ["rock", "synth-pop", "dark wave"]
    );
}

#[test]
fn genre_weight_is_quarter_of_sound_spread_ratio() {
    let fx = load();
    close(fx.sound_spread, f(&fx.json["soundSpread"]), "sound spread");
    close(fx.genre_spread, f(&fx.json["genreSpread"]), "genre spread");
    close(
        genre_weight(fx.sound_spread, fx.genre_spread),
        f(&fx.json["genreWeight"]),
        "genre weight",
    );
}

#[test]
fn scores_every_pair_the_same() {
    let fx = load();
    for (i, a) in fx.tracks.iter().enumerate() {
        for (j, b) in fx.tracks.iter().enumerate() {
            close(
                pair_score(a, b, &fx.model, fx.wg_sq),
                f(&fx.json["scores"][i][j]),
                &format!("pair {i},{j}"),
            );
        }
    }
}

#[test]
fn gives_each_track_the_same_capped_neighbours() {
    let fx = load();
    for seed in &fx.tracks {
        let list = cap_list(
            ranked(
                &fx,
                |t| pair_score(seed, t, &fx.model, fx.wg_sq),
                &[&seed.track_id],
            )
            .into_iter()
            .map(|(t, _)| Row(t)),
            &[place_of(seed)],
            10,
        );
        let got: Vec<&str> = list.iter().map(|r| r.0.track_id.as_str()).collect();
        let want: Vec<&str> = fx.json["neighbours"][&seed.track_id]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(got, want, "seed {}", seed.track_id);
    }
}

#[test]
fn weighted_set_is_the_sum_of_pair_scores() {
    let fx = load();
    let by_id: HashMap<&str, &MatchTrack> =
        fx.tracks.iter().map(|t| (t.track_id.as_str(), t)).collect();
    for set in fx.json["sets"].as_array().unwrap() {
        let inputs: Vec<WeightedTrack> = set["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| WeightedTrack {
                track: by_id[i["trackId"].as_str().unwrap()],
                weight: f(&i["weight"]),
            })
            .collect();
        let (sound, feel) = blend_sound_feel(&inputs);
        let bonus = artist_bonuses(&inputs, &fx.model, fx.wg_sq);
        let score = |t: &MatchTrack| {
            dot(&t.sound, &sound)
                + FEEL_WEIGHT * FEEL_WEIGHT * (t.feel[0] * feel[0] + t.feel[1] * feel[1])
                + bonus.get(&t.artist_id).copied().unwrap_or(0.0)
        };
        for (k, t) in fx.tracks.iter().enumerate() {
            close(score(t), f(&set["scores"][k]), "set score");
        }
        close(
            blend_self_score(&inputs, &fx.model, fx.wg_sq),
            f(&set["selfScore"]),
            "self score",
        );
        let exclude: Vec<&str> = set["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["trackId"].as_str().unwrap())
            .collect();
        let counted: Vec<Place> = set["counted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| place_of(by_id[id.as_str().unwrap()]))
            .collect();
        let list = cap_list(
            ranked(&fx, score, &exclude)
                .into_iter()
                .map(|(t, _)| Row(t)),
            &counted,
            10,
        );
        let got: Vec<&str> = list.iter().map(|r| r.0.track_id.as_str()).collect();
        let want: Vec<&str> = set["capped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(got, want);
        // The SQL shortcut returns the same list.
        let two = cap_two_window(
            ranked(&fx, score, &exclude)
                .into_iter()
                .map(|(t, _)| Row(t)),
            &counted,
            10,
        );
        assert_eq!(
            two.iter()
                .map(|r| r.0.track_id.as_str())
                .collect::<Vec<_>>(),
            want
        );
    }
}

#[test]
fn scores_artists_as_formula_averaged_over_track_pairs() {
    let fx = load();
    let profile = |artist: &str| {
        let own: Vec<&MatchTrack> = fx.tracks.iter().filter(|t| t.artist_id == artist).collect();
        let n = own.len() as f64;
        let dim = own[0].sound.len();
        ArtistProfile {
            artist_id: artist.into(),
            sound: (0..dim)
                .map(|d| own.iter().fold(0.0, |s, t| s + t.sound[d]) / n)
                .collect(),
            feel: [
                own.iter().fold(0.0, |s, t| s + t.feel[0]) / n,
                own.iter().fold(0.0, |s, t| s + t.feel[1]) / n,
            ],
        }
    };
    for m in fx.json["artistMatch"].as_array().unwrap() {
        let got = artist_match(
            &profile(m["a"].as_str().unwrap()),
            &profile(m["b"].as_str().unwrap()),
            &fx.model,
            fx.wg_sq,
        )
        .unwrap();
        close(got, f(&m["value"]), "artist match");
    }
}

fn row(id: &str, album: &str, artist: &str) -> (String, Place) {
    (
        id.into(),
        Place {
            album_id: album.into(),
            artist_id: artist.into(),
        },
    )
}
struct R((String, Place));
impl Placed for R {
    fn album_id(&self) -> &str {
        &self.0.1.album_id
    }
    fn artist_id(&self) -> &str {
        &self.0.1.artist_id
    }
}
fn ids(v: &[R]) -> Vec<&str> {
    v.iter().map(|r| r.0.0.as_str()).collect()
}
fn seed_place() -> Place {
    Place {
        album_id: "seedAlbum".into(),
        artist_id: "seedArtist".into(),
    }
}

#[test]
fn cap_list_counts_the_seed_against_its_own_album_and_artist() {
    let rows: Vec<R> = (0..10)
        .map(|i| R(row(&format!("t{i}"), &format!("alb{i}"), "seedArtist")))
        .collect();
    assert_eq!(
        cap_list(rows, &[seed_place()], 20).len(),
        MAX_PER_ARTIST - 1
    );
    let rows: Vec<R> = (0..10)
        .map(|i| R(row(&format!("t{i}"), "seedAlbum", &format!("art{i}"))))
        .collect();
    assert_eq!(cap_list(rows, &[seed_place()], 20).len(), MAX_PER_ALBUM - 1);
}

#[test]
fn cap_list_counts_every_input() {
    let counted = [
        Place {
            album_id: "alb".into(),
            artist_id: "art".into(),
        },
        Place {
            album_id: "alb".into(),
            artist_id: "art".into(),
        },
    ];
    let rows = vec![
        R(row("x", "alb", "art")),
        R(row("y", "other", "art")),
        R(row("z", "other", "art")),
        R(row("w", "third", "art")),
    ];
    assert_eq!(ids(&cap_list(rows, &counted, 10)), ["y", "z"]);
}

#[test]
fn cap_list_stops_at_limit() {
    let rows: Vec<R> = (0..10)
        .map(|i| {
            R(row(
                &format!("t{i}"),
                &format!("alb{i}"),
                &format!("art{i}"),
            ))
        })
        .collect();
    assert_eq!(
        ids(&cap_list(rows, &[] as &[Place], 4)),
        ["t0", "t1", "t2", "t3"]
    );
}

#[test]
fn feel_vector_zero_on_library_mean() {
    assert_eq!(
        feel_vector(
            [0.4, 0.6],
            &FeelStats {
                mean: [0.4, 0.6],
                sd: [0.1, 0.2]
            }
        ),
        [0.0, 0.0]
    );
}

#[test]
fn feel_vector_drops_constant_axis() {
    assert_eq!(
        feel_vector(
            [0.9, 0.3],
            &FeelStats {
                mean: [0.5, 0.3],
                sd: [0.1, 0.0]
            }
        ),
        [1.0, 0.0]
    );
}

#[test]
fn slerp_starts_and_ends_at_the_tracks() {
    assert_eq!(slerp_weights(0.3, 0.0), [1.0, 0.0]);
    let [w0, w1] = slerp_weights(0.3, 1.0);
    close(w0, 0.0, "w0");
    close(w1, 1.0, "w1");
}

#[test]
fn slerp_keeps_arc_points_at_track_length() {
    for cos in [-0.5, 0.0, 0.3, 0.9] {
        for t in [0.2, 0.5, 0.8] {
            let [w0, w1] = slerp_weights(cos, t);
            close(w0 * w0 + w1 * w1 + 2.0 * w0 * w1 * cos, 1.0, "arc length");
        }
    }
    assert_eq!(
        slerp_weights(1.0, 0.25),
        [0.75, 0.25],
        "parallel tracks: straight line"
    );
}

#[test]
fn genre_weight_zero_without_genres() {
    assert_eq!(genre_weight(0.7, 0.0), 0.0);
}

#[test]
fn self_score_is_1_0625() {
    assert_eq!(SELF_SCORE, 1.0625);
    let fx = load();
    let t = &fx.tracks[0];
    close(
        pair_score(t, t, &fx.model, fx.wg_sq),
        SELF_SCORE,
        "self score",
    );
}

#[test]
fn genre_vector_of_artist_without_model_entry_is_none() {
    let fx = load();
    assert!(genre_vector(&fx.model, "no such artist").is_none());
}

/// Upstream's SQL two-window shortcut equals the walk when each album belongs to one artist (random lists, seeded).
#[test]
fn cap_list_equals_two_window_shortcut_on_random_ranked_lists() {
    let mut rng = Xorshift(2026_1105);
    for case in 0..1000 {
        let artists = 2 + (rng.next_u64() % 5) as usize;
        let albums_per = 1 + (rng.next_u64() % 3) as usize;
        let n = (rng.next_u64() % 60) as usize;
        let rows: Vec<R> = (0..n)
            .map(|i| {
                let a = (rng.next_u64() % artists as u64) as usize;
                let al = (rng.next_u64() % albums_per as u64) as usize;
                R(row(
                    &format!("t{i:03}"),
                    &format!("alb{a}-{al}"),
                    &format!("art{a}"),
                ))
            })
            .collect();
        let counted: Vec<Place> = (0..(rng.next_u64() % 4))
            .map(|_| {
                let a = (rng.next_u64() % artists as u64) as usize;
                Place {
                    album_id: format!("alb{a}-{}", rng.next_u64() % albums_per as u64),
                    artist_id: format!("art{a}"),
                }
            })
            .collect();
        let limit = 1 + (rng.next_u64() % 30) as usize;
        let clone = |rows: &[R]| rows.iter().map(|r| R(r.0.clone())).collect::<Vec<_>>();
        let walk = cap_list(clone(&rows), &counted, limit);
        let windows = cap_two_window(clone(&rows), &counted, limit);
        assert_eq!(ids(&walk), ids(&windows), "case {case}");
    }
}

#[test]
fn two_window_differs_for_shared_albums() {
    // Album "A" is shared by two artists; X is already full (4 kept), so x5 is dropped by the artist cap in the walk
    // and does not use a place of A, but the SQL windows rank it first. Documented limit of the shortcut.
    let mut rows: Vec<R> = (0..4)
        .map(|i| R(row(&format!("x{i}"), &format!("ax{i}"), "X")))
        .collect();
    rows.push(R(row("x5", "A", "X")));
    rows.push(R(row("y1", "A", "Y")));
    rows.push(R(row("y2", "A", "Y")));
    let clone = |rows: &[R]| rows.iter().map(|r| R(r.0.clone())).collect::<Vec<_>>();
    let walk = cap_list(clone(&rows), &[] as &[Place], 20);
    let windows = cap_two_window(clone(&rows), &[] as &[Place], 20);
    assert_eq!(ids(&walk), ["x0", "x1", "x2", "x3", "y1", "y2"]);
    assert_eq!(ids(&windows), ["x0", "x1", "x2", "x3", "y1"]);
}
