//! The neighbour sweep against upstream's own output (`ts-golden-neighbours.json`, bit for bit) and its invariants.
mod common;
use common::*;
use kima_vibe::formula::*;
use kima_vibe::neighbours::*;
use std::collections::{BTreeMap, HashSet};

struct Lib {
    tracks: Vec<MatchTrack>,
    model: GenreModel,
    gw_sq: f64,
}

/// A golden library as `buildLibrary` in `tools/vibe-goldens/run.mjs` makes it (rows already in id order).
fn load(name: &str) -> Lib {
    let json = fixture("ts-golden-library.json");
    let lib = &json[name];
    let bits = |v: &serde_json::Value| from_bits_hex(v.as_str().unwrap());
    let stats = FeelStats {
        mean: [bits(&lib["feelMean"][0]), bits(&lib["feelMean"][1])],
        sd: [bits(&lib["feelSd"][0]), bits(&lib["feelSd"][1])],
    };
    let f32_of = |v: &serde_json::Value| f64::from(v.as_f64().unwrap() as f32);
    let tracks: Vec<MatchTrack> = lib["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| MatchTrack {
            track_id: t["id"].as_str().unwrap().into(),
            album_id: t["albumId"].as_str().unwrap().into(),
            artist_id: t["artistId"].as_str().unwrap().into(),
            sound: unit(
                &t["sound"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_f64().unwrap())
                    .collect::<Vec<_>>(),
            ),
            feel: feel_vector([f32_of(&t["valence"]), f32_of(&t["arousal"])], &stats),
        })
        .collect();
    let artists: Vec<LibraryArtist> = lib["artistGenres"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, genres)| LibraryArtist {
            artist_id: id.clone(),
            tracks: tracks.iter().filter(|t| &t.artist_id == id).count() as u64,
            genres: genres
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g.as_str().unwrap().to_owned())
                .collect(),
        })
        .collect();
    let (model, spread) = build_genre_model(&artists);
    assert!(same_bits(spread, bits(&lib["genreSpread"])), "genre spread");
    let wg = bits(&lib["genreWeight"]);
    Lib {
        tracks,
        model,
        gw_sq: wg * wg,
    }
}

const LIBS: [&str; 2] = ["fixture50", "synthetic300"];

#[test]
fn nearest_columns_matches_ts_golden() {
    let golden = fixture("ts-golden-neighbours.json");
    for name in LIBS {
        let lib = load(name);
        let g = &golden["libraries"][name];
        let k = g["k"].as_u64().unwrap() as usize;
        let near = nearest_columns(&lib.tracks, &lib.tracks, &lib.model, lib.gw_sq, 14, true);
        assert_eq!(near.k, k, "{name}");
        let idx: Vec<u32> = g["idx"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        assert_eq!(near.idx, idx, "{name}: neighbour indices");
        assert_eq!(near.score.len(), g["score"].as_array().unwrap().len());
        for (i, (got, want)) in near
            .score
            .iter()
            .zip(g["score"].as_array().unwrap())
            .enumerate()
        {
            let want = from_bits_hex(want.as_str().unwrap());
            assert!(same_bits(*got, want), "{name}: score {i}: {got} vs {want}");
        }
        assert!(
            same_bits(
                near.max_score,
                from_bits_hex(g["maxScore"].as_str().unwrap())
            ),
            "{name}: maxScore"
        );
    }
}

#[test]
fn neighbour_lists_capped_with_track_itself_counted() {
    let golden = fixture("ts-golden-neighbours.json");
    for name in LIBS {
        let lib = load(name);
        let lists = neighbour_lists(&lib.tracks, &lib.model, lib.gw_sq, 10);
        let want = golden["libraries"][name]["capped10"].as_object().unwrap();
        assert_eq!(lists.len(), want.len(), "{name}");
        for (id, entries) in want {
            let got = &lists[id];
            let entries = entries.as_array().unwrap();
            assert_eq!(got.len(), entries.len(), "{name} {id}");
            for (g, w) in got.iter().zip(entries) {
                assert_eq!(g.0, w[0].as_str().unwrap(), "{name} {id}");
                assert!(
                    same_bits(g.1, from_bits_hex(w[1].as_str().unwrap())),
                    "{name} {id}"
                );
            }
        }
        // The caps hold with the seed taking one place in its own album and artist.
        for t in &lib.tracks {
            let list = &lists[&t.track_id];
            let by_id = |id: &str| lib.tracks.iter().find(|x| x.track_id == id).unwrap();
            assert!(list.iter().all(|(id, _)| *id != t.track_id));
            let (mut albums, mut artists) = (BTreeMap::new(), BTreeMap::new());
            for (id, _) in list {
                let o = by_id(id);
                *albums.entry(o.album_id.clone()).or_insert(0) += 1;
                *artists.entry(o.artist_id.clone()).or_insert(0) += 1;
            }
            assert!(albums.get(&t.album_id).copied().unwrap_or(0) < MAX_PER_ALBUM);
            assert!(artists.get(&t.artist_id).copied().unwrap_or(0) < MAX_PER_ARTIST);
            assert!(albums.values().all(|n| *n <= MAX_PER_ALBUM));
            assert!(artists.values().all(|n| *n <= MAX_PER_ARTIST));
        }
    }
}

#[test]
fn equal_scores_in_id_order() {
    // Four identical tracks of one artist plus two others, fed in an order that is not the id order.
    let base = |id: &str, artist: &str, sound: [f64; 3]| MatchTrack {
        track_id: id.into(),
        album_id: format!("alb-{id}"),
        artist_id: artist.into(),
        sound: unit(&sound),
        feel: [0.0, 0.0],
    };
    let tracks = vec![
        base("d", "x", [1.0, 0.0, 0.0]),
        base("b", "x", [1.0, 0.0, 0.0]),
        base("a", "x", [1.0, 0.0, 0.0]),
        base("c", "x", [1.0, 0.0, 0.0]),
        base("z", "y", [0.0, 1.0, 0.0]),
        base("q", "y", [0.0, 0.0, 1.0]),
    ];
    let near = nearest_columns(&tracks, &tracks, &GenreModel::default(), 0.0, 3, true);
    assert_eq!(near.k, 3);
    // Row "d" (index 0): its three equals first, by id: a (2), b (1), c (3).
    assert_eq!(&near.idx[0..3], &[2, 1, 3]);
    // Row "z": nothing is similar; the zero scores of the other four come in id order.
    let row = 4;
    let ids: Vec<&str> = near.idx[row * 3..row * 3 + 3]
        .iter()
        .map(|i| tracks[*i as usize].track_id.as_str())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
}

#[test]
fn top_indices_heap_matches_full_sort() {
    let mut rng = Xorshift(0x9e3779b97f4a7c15);
    for case in 0..200 {
        let n = 1 + (rng.next_u64() % 150) as usize;
        // Few distinct values, so ties are common.
        let scores: Vec<f64> = (0..n).map(|_| (rng.next_u64() % 9) as f64 / 8.0).collect();
        let mut rank: Vec<u32> = (0..n as u32).collect();
        for i in (1..n).rev() {
            rank.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
        }
        let skip = if case % 2 == 0 {
            Some((rng.next_u64() % n as u64) as usize)
        } else {
            None
        };
        let size = (rng.next_u64() % (n as u64 + 3)) as usize;
        let got = top_indices(&scores, &rank, size, skip);
        let mut all: Vec<usize> = (0..n).filter(|j| Some(*j) != skip).collect();
        all.sort_by(|x, y| {
            scores[*y]
                .partial_cmp(&scores[*x])
                .unwrap()
                .then(rank[*x].cmp(&rank[*y]))
        });
        all.truncate(size);
        assert_eq!(got, all, "case {case} n={n} size={size}");
    }
}

#[test]
fn same_set_never_returns_self() {
    for name in LIBS {
        let lib = load(name);
        let near = nearest_columns(&lib.tracks, &lib.tracks, &lib.model, lib.gw_sq, 14, true);
        for row in 0..lib.tracks.len() {
            let mine = &near.idx[row * near.k..(row + 1) * near.k];
            assert!(!mine.contains(&(row as u32)), "{name} row {row}");
            assert_eq!(mine.iter().collect::<HashSet<_>>().len(), near.k);
        }
    }
    // k larger than the other tracks is clipped to them.
    let lib = load("fixture50");
    let few = &lib.tracks[..4];
    let near = nearest_columns(few, few, &lib.model, lib.gw_sq, 14, true);
    assert_eq!(near.k, 3);
    // A different set may return the same track: it is not "self" there.
    let other = nearest_columns(
        &lib.tracks[..3],
        &lib.tracks,
        &lib.model,
        lib.gw_sq,
        2,
        false,
    );
    assert_eq!(other.k, 2);
    assert_eq!(
        other.idx[0], 0,
        "a track is its own best match in another set"
    );
}

#[test]
fn rows_visited_grouped_by_artist_build_bonuses_once() {
    let lib = load("synthetic300");
    let artists: HashSet<&str> = lib.tracks.iter().map(|t| t.artist_id.as_str()).collect();
    let (_, stats) = nearest_columns_with(
        Sweep::default(),
        &lib.tracks,
        &lib.tracks,
        &lib.model,
        lib.gw_sq,
        14,
        true,
    );
    assert_eq!(stats.bonus_builds, artists.len());
}

#[test]
fn f32_accumulation_stays_close_to_f64() {
    let lib = load("synthetic300");
    let exact = nearest_columns(&lib.tracks, &lib.tracks, &lib.model, lib.gw_sq, 14, true);
    let (fast, _) = nearest_columns_with(
        Sweep {
            accumulation: Accumulation::F32,
            parallel: false,
        },
        &lib.tracks,
        &lib.tracks,
        &lib.model,
        lib.gw_sq,
        14,
        true,
    );
    assert!((exact.max_score - fast.max_score).abs() < 1e-5);
    let same_first = (0..lib.tracks.len())
        .filter(|r| exact.idx[r * 14] == fast.idx[r * 14])
        .count();
    assert!(same_first >= 295, "{same_first}");
}

#[cfg(feature = "parallel")]
#[test]
fn parallel_equals_serial() {
    for name in LIBS {
        let lib = load(name);
        for acc in [Accumulation::F64, Accumulation::F32] {
            let run = |parallel| {
                nearest_columns_with(
                    Sweep {
                        accumulation: acc,
                        parallel,
                    },
                    &lib.tracks,
                    &lib.tracks,
                    &lib.model,
                    lib.gw_sq,
                    14,
                    true,
                )
                .0
            };
            let (a, b) = (run(false), run(true));
            assert_eq!(a.idx, b.idx);
            assert!(
                a.score
                    .iter()
                    .zip(&b.score)
                    .all(|(x, y)| x.to_bits() == y.to_bits())
            );
            assert_eq!(a.max_score.to_bits(), b.max_score.to_bits());
        }
        let lists = |parallel| {
            neighbour_lists_with(
                Sweep {
                    accumulation: Accumulation::F64,
                    parallel,
                },
                &lib.tracks,
                &lib.model,
                lib.gw_sq,
                10,
            )
        };
        assert_eq!(lists(false), lists(true));
    }
}
