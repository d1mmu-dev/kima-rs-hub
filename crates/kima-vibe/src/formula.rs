//! The matching formula as pure arithmetic: sound, feel at 0.25 and artist genre at 0.25 (counted only between
//! different artists), then at most 2 tracks per album and 4 per artist. Port of upstream kima-hub
//! `backend/src/services/matching/formula.ts` (v1.12.3, c7527577).
//!
//! Everything is `f64`; sound vectors are stored `f32` and widened by the caller. Float operations keep upstream's
//! order (sums run in index order, no fused multiply-add), because the map goldens are bit-compared. Ids are
//! strings compared bytewise; lowercase hyphenated UUID text sorts like the uuid bytes, which is what Postgres
//! sorts by, so `ORDER BY id` in SQL and the id order here agree.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::exact_math::log;

pub const FEEL_WEIGHT: f64 = 0.25;
pub const GENRE_WEIGHT: f64 = 0.25;
pub const MAX_PER_ALBUM: usize = 2;
pub const MAX_PER_ARTIST: usize = 4;

/// What any track scores against itself: sound 1 plus feel 0.25^2, and no genre, being its own artist.
pub const SELF_SCORE: f64 = 1.0 + FEEL_WEIGHT * FEEL_WEIGHT;

/// (valence, arousal).
pub type Feel = [f64; 2];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FeelStats {
    pub mean: Feel,
    pub sd: Feel,
}

/// A track as the formula sees it. `sound` is unit length; `feel` comes from [`feel_vector`].
#[derive(Debug, Clone, PartialEq)]
pub struct MatchTrack {
    pub track_id: String,
    pub album_id: String,
    pub artist_id: String,
    pub sound: Vec<f64>,
    pub feel: Feel,
}

#[derive(Debug, Clone, Copy)]
pub struct WeightedTrack<'a> {
    pub track: &'a MatchTrack,
    pub weight: f64,
}

/// One artist's entry of the genre model: sparse TF-IDF weights over the vocabulary and `norm = |tfidf - mean|`,
/// so its genre vector is `(tfidf - mean) / norm`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistGenres {
    pub index: Vec<usize>,
    pub weight: Vec<f64>,
    pub norm: f64,
}

/// Artist genres as TF-IDF over the library, centred and made unit length. `artists` is keyed and iterated by
/// artist id, which is the order upstream inserts them in (tagged artists sorted by id).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GenreModel {
    pub vocabulary: Vec<String>,
    pub mean: Vec<f64>,
    pub artists: BTreeMap<String, ArtistGenres>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LibraryArtist {
    pub artist_id: String,
    pub tracks: u64,
    pub genres: Vec<String>,
}

pub fn dot(a: &[f64], b: &[f64]) -> f64 {
    let mut s = 0.0;
    for i in 0..a.len() {
        s += a[i] * b[i];
    }
    s
}

/// `values` scaled to unit length (unchanged when its length is 0).
pub fn unit(values: &[f64]) -> Vec<f64> {
    let mut out = values.to_vec();
    let n = dot(&out, &out).sqrt();
    if n > 0.0 {
        for x in &mut out {
            *x /= n;
        }
    }
    out
}

/// Feel in the library's own terms: standardised per axis, then unit length; zero on the library mean. An axis on
/// which every track is the same carries nothing.
pub fn feel_vector(raw: Feel, stats: &FeelStats) -> Feel {
    let v = if stats.sd[0] > 0.0 {
        (raw[0] - stats.mean[0]) / stats.sd[0]
    } else {
        0.0
    };
    let a = if stats.sd[1] > 0.0 {
        (raw[1] - stats.mean[1]) / stats.sd[1]
    } else {
        0.0
    };
    let n = (v * v + a * a).sqrt();
    if n > 0.0 { [v / n, a / n] } else { [0.0, 0.0] }
}

/// Track-weighted TF-IDF over the artists' genres, centred on the mean of the tracks that have genres. The second
/// value is the spread: the mean squared distance of every track's genre vector from the library's mean genre
/// vector, tracks without genres counting as zero.
pub fn build_genre_model(artists: &[LibraryArtist]) -> (GenreModel, f64) {
    let total_tracks: u64 = artists.iter().map(|a| a.tracks).sum();
    let mut tagged: Vec<&LibraryArtist> = artists
        .iter()
        .filter(|a| a.tracks > 0 && !a.genres.is_empty())
        .collect();
    tagged.sort_by(|a, b| a.artist_id.cmp(&b.artist_id));
    let mut vocabulary: Vec<String> = tagged
        .iter()
        .flat_map(|a| a.genres.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    vocabulary.sort();
    let position: HashMap<&str, usize> = vocabulary
        .iter()
        .enumerate()
        .map(|(i, g)| (g.as_str(), i))
        .collect();
    let tagged_tracks: u64 = tagged.iter().map(|a| a.tracks).sum();

    let mut df = vec![0.0_f64; vocabulary.len()];
    for a in &tagged {
        for g in &a.genres {
            df[position[g.as_str()]] += a.tracks as f64;
        }
    }
    let idf: Vec<f64> = df
        .iter()
        .map(|&d| log((tagged_tracks.max(1) as f64) / d.max(1.0)))
        .collect();

    struct Tfidf<'a> {
        artist: &'a LibraryArtist,
        index: Vec<usize>,
        weight: Vec<f64>,
    }
    let tfidf: Vec<Tfidf> = tagged
        .iter()
        .map(|&a| {
            let index: Vec<usize> = a.genres.iter().map(|g| position[g.as_str()]).collect();
            let raw: Vec<f64> = index.iter().map(|&i| idf[i]).collect();
            let n = dot(&raw, &raw).sqrt();
            let weight = raw
                .iter()
                .map(|&w| if n > 0.0 { w / n } else { 0.0 })
                .collect();
            Tfidf {
                artist: a,
                index,
                weight,
            }
        })
        .collect();

    let mut mean = vec![0.0_f64; vocabulary.len()];
    for x in &tfidf {
        for (k, &i) in x.index.iter().enumerate() {
            mean[i] += (x.artist.tracks as f64 * x.weight[k]) / tagged_tracks as f64;
        }
    }

    let mut model = GenreModel {
        vocabulary,
        mean,
        artists: BTreeMap::new(),
    };
    let mut sum = vec![0.0_f64; model.vocabulary.len()];
    let mut squares = 0.0_f64;
    for x in tfidf {
        let mut centred: Vec<f64> = model.mean.iter().map(|m| -m).collect();
        for (k, &i) in x.index.iter().enumerate() {
            centred[i] += x.weight[k];
        }
        let norm = dot(&centred, &centred).sqrt();
        if norm == 0.0 {
            continue;
        }
        for i in 0..centred.len() {
            sum[i] += (x.artist.tracks as f64 * centred[i]) / norm;
        }
        squares += x.artist.tracks as f64;
        model.artists.insert(
            x.artist.artist_id.clone(),
            ArtistGenres {
                index: x.index,
                weight: x.weight,
                norm,
            },
        );
    }
    let total = total_tracks as f64;
    let spread = if total_tracks > 0 {
        squares / total - dot(&sum, &sum) / (total * total)
    } else {
        0.0
    };
    (model, spread)
}

/// `w_g = 0.25 * sqrt(spread_sound / spread_genre)`: genre gets 0.25^2 of sound's pull.
pub fn genre_weight(sound_spread: f64, genre_spread: f64) -> f64 {
    if genre_spread > 0.0 {
        GENRE_WEIGHT * (sound_spread / genre_spread).sqrt()
    } else {
        0.0
    }
}

/// The artist's centred, unit-length genre vector over the model's vocabulary; `None` without genres.
pub fn genre_vector(model: &GenreModel, artist_id: &str) -> Option<Vec<f64>> {
    let entry = model.artists.get(artist_id)?;
    let mut g: Vec<f64> = model.mean.iter().map(|m| -m).collect();
    for (k, &i) in entry.index.iter().enumerate() {
        g[i] += entry.weight[k];
    }
    for x in &mut g {
        *x /= entry.norm;
    }
    Some(g)
}

/// The genre term of `sum_i w_i * s(t, i)` for a track by each artist B with genres:
/// `w_g^2 * (g_B . sum_i w_i g_A(i) - W_B)`, `W_B` the summed weight of the inputs by B, which removes the term
/// between an artist and itself. Artists absent from the model have no genres, so their term is zero. Empty when
/// `genre_weight_sq` is 0. Keyed (and iterated) by artist id.
pub fn artist_bonuses(
    inputs: &[WeightedTrack<'_>],
    model: &GenreModel,
    genre_weight_sq: f64,
) -> BTreeMap<String, f64> {
    let mut bonuses = BTreeMap::new();
    if genre_weight_sq == 0.0 {
        return bonuses;
    }
    let mut blended = vec![0.0_f64; model.vocabulary.len()];
    let mut weight_by_artist: HashMap<&str, f64> = HashMap::new();
    for input in inputs {
        *weight_by_artist
            .entry(input.track.artist_id.as_str())
            .or_insert(0.0) += input.weight;
        if let Some(g) = genre_vector(model, &input.track.artist_id) {
            for i in 0..g.len() {
                blended[i] += input.weight * g[i];
            }
        }
    }
    let mean_dot = dot(&model.mean, &blended);
    for (artist_id, x) in &model.artists {
        let mut x_dot = 0.0;
        for (k, &i) in x.index.iter().enumerate() {
            x_dot += x.weight[k] * blended[i];
        }
        let genre_dot = (x_dot - mean_dot) / x.norm;
        let own = weight_by_artist
            .get(artist_id.as_str())
            .copied()
            .unwrap_or(0.0);
        bonuses.insert(artist_id.clone(), genre_weight_sq * (genre_dot - own));
    }
    bonuses
}

/// `sum_i w_i c_i` and `sum_i w_i v_i`, the sound and feel halves of a weighted input set (zero-length sound for
/// an empty set).
pub fn blend_sound_feel(inputs: &[WeightedTrack<'_>]) -> (Vec<f64>, Feel) {
    let mut sound = vec![0.0_f64; inputs.first().map_or(0, |i| i.track.sound.len())];
    let mut feel = [0.0, 0.0];
    for input in inputs {
        for (acc, x) in sound.iter_mut().zip(&input.track.sound) {
            *acc += input.weight * x;
        }
        feel[0] += input.weight * input.track.feel[0];
        feel[1] += input.weight * input.track.feel[1];
    }
    (sound, feel)
}

/// s(a, b): sound + 0.25^2 * feel + w_g^2 * genre when the artists differ.
pub fn pair_score(a: &MatchTrack, b: &MatchTrack, model: &GenreModel, genre_weight_sq: f64) -> f64 {
    let mut s = dot(&a.sound, &b.sound)
        + FEEL_WEIGHT * FEEL_WEIGHT * (a.feel[0] * b.feel[0] + a.feel[1] * b.feel[1]);
    if a.artist_id != b.artist_id && genre_weight_sq > 0.0 {
        if let (Some(ga), Some(gb)) = (
            genre_vector(model, &a.artist_id),
            genre_vector(model, &b.artist_id),
        ) {
            s += genre_weight_sq * dot(&ga, &gb);
        }
    }
    s
}

/// What a weighted input set scores against itself, `sum_i sum_j w_i w_j s(i, j)`: the squared length of the blend.
pub fn blend_self_score(
    inputs: &[WeightedTrack<'_>],
    model: &GenreModel,
    genre_weight_sq: f64,
) -> f64 {
    let mut total = 0.0;
    for a in inputs {
        for b in inputs {
            total += a.weight * b.weight * pair_score(a.track, b.track, model, genre_weight_sq);
        }
    }
    total
}

/// An artist as the mean of its tracks: mean unit sound vector and mean feel vector.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistProfile {
    pub artist_id: String,
    pub sound: Vec<f64>,
    pub feel: Feel,
}

/// The formula averaged over every track pair of two artists, normalised like a cosine. For sound alone it is the
/// cosine between the artists' centroids. `None` when an artist has no spread to normalise by.
pub fn artist_match(
    a: &ArtistProfile,
    b: &ArtistProfile,
    model: &GenreModel,
    genre_weight_sq: f64,
) -> Option<f64> {
    let feel_sq = FEEL_WEIGHT * FEEL_WEIGHT;
    let self_score = |p: &ArtistProfile| {
        dot(&p.sound, &p.sound) + feel_sq * (p.feel[0].powi(2) + p.feel[1].powi(2))
    };
    let mut cross =
        dot(&a.sound, &b.sound) + feel_sq * (a.feel[0] * b.feel[0] + a.feel[1] * b.feel[1]);
    if a.artist_id != b.artist_id && genre_weight_sq > 0.0 {
        if let (Some(ga), Some(gb)) = (
            genre_vector(model, &a.artist_id),
            genre_vector(model, &b.artist_id),
        ) {
            cross += genre_weight_sq * dot(&ga, &gb);
        }
    }
    let norm = (self_score(a) * self_score(b)).sqrt();
    if norm > 0.0 { Some(cross / norm) } else { None }
}

/// Anything with an album and an artist, for the caps.
pub trait Placed {
    fn album_id(&self) -> &str;
    fn artist_id(&self) -> &str;
}

impl Placed for MatchTrack {
    fn album_id(&self) -> &str {
        &self.album_id
    }
    fn artist_id(&self) -> &str {
        &self.artist_id
    }
}

/// A bare (album, artist) pair: an input track that takes a place in the caps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub album_id: String,
    pub artist_id: String,
}

impl Placed for Place {
    fn album_id(&self) -> &str {
        &self.album_id
    }
    fn artist_id(&self) -> &str {
        &self.artist_id
    }
}

/// Walk a ranked list keeping at most [`MAX_PER_ALBUM`] per album and [`MAX_PER_ARTIST`] per artist, each counted
/// track (a seed, an added track) using one place in its own album and artist, up to `limit`.
pub fn cap_list<T: Placed, C: Placed>(
    ranked: impl IntoIterator<Item = T>,
    counted: &[C],
    limit: usize,
) -> Vec<T> {
    let mut per_album: HashMap<String, usize> = HashMap::new();
    let mut per_artist: HashMap<String, usize> = HashMap::new();
    for c in counted {
        *per_album.entry(c.album_id().to_owned()).or_insert(0) += 1;
        *per_artist.entry(c.artist_id().to_owned()).or_insert(0) += 1;
    }
    let mut out = Vec::new();
    for row in ranked {
        if out.len() >= limit {
            break;
        }
        let album_count = per_album.get(row.album_id()).copied().unwrap_or(0);
        let artist_count = per_artist.get(row.artist_id()).copied().unwrap_or(0);
        if album_count < MAX_PER_ALBUM && artist_count < MAX_PER_ARTIST {
            per_album.insert(row.album_id().to_owned(), album_count + 1);
            per_artist.insert(row.artist_id().to_owned(), artist_count + 1);
            out.push(row);
        }
    }
    out
}

/// Upstream's SQL shortcut for [`cap_list`] (`rank.ts capRanked`): keep the best `2 - used` rows per album, then
/// among those the best `4 - used` per artist, in ranked order, up to `limit`. It equals the walk whenever every
/// album belongs to one artist, which holds upstream and here because a track's artist is its album's artist. For an
/// album shared by several artists the two can differ (see the test `two_window_differs_for_shared_albums`).
pub fn cap_two_window<T: Placed, C: Placed>(
    ranked: impl IntoIterator<Item = T>,
    counted: &[C],
    limit: usize,
) -> Vec<T> {
    let mut album_used: HashMap<&str, usize> = HashMap::new();
    let mut artist_used: HashMap<&str, usize> = HashMap::new();
    for c in counted {
        *album_used.entry(c.album_id()).or_insert(0) += 1;
        *artist_used.entry(c.artist_id()).or_insert(0) += 1;
    }
    let ranked: Vec<T> = ranked.into_iter().collect();
    let mut album_rank: HashMap<&str, usize> = HashMap::new();
    let mut kept: Vec<usize> = Vec::new();
    for (n, row) in ranked.iter().enumerate() {
        let r = album_rank.entry(row.album_id()).or_insert(0);
        *r += 1;
        if *r <= MAX_PER_ALBUM.saturating_sub(album_used.get(row.album_id()).copied().unwrap_or(0))
        {
            kept.push(n);
        }
    }
    let mut artist_rank: HashMap<&str, usize> = HashMap::new();
    let mut wanted: HashSet<usize> = HashSet::new();
    for &n in &kept {
        let row = &ranked[n];
        let r = artist_rank.entry(row.artist_id()).or_insert(0);
        *r += 1;
        if *r
            <= MAX_PER_ARTIST.saturating_sub(artist_used.get(row.artist_id()).copied().unwrap_or(0))
            && wanted.len() < limit
        {
            wanted.insert(n);
        }
    }
    ranked
        .into_iter()
        .enumerate()
        .filter(|(n, _)| wanted.contains(n))
        .map(|(_, r)| r)
        .collect()
}

/// Weights (w0, w1) for the point a fraction `t` of the way along the arc between two tracks whose normalised
/// similarity is `cos_theta`; straight-line weights when they point the same way.
pub fn slerp_weights(cos_theta: f64, t: f64) -> [f64; 2] {
    if cos_theta.is_nan() {
        return [f64::NAN, f64::NAN];
    }
    let theta = cos_theta.clamp(-1.0, 1.0).acos();
    let sin_theta = theta.sin();
    if theta < 1e-6 || sin_theta == 0.0 {
        return [1.0 - t, t];
    }
    [
        ((1.0 - t) * theta).sin() / sin_theta,
        (t * theta).sin() / sin_theta,
    ]
}

/// The lower-case letters and digits of a genre name: the comparison the formula was measured with.
pub fn genre_key(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| matches!(c, 'a'..='z' | '0'..='9'))
        .collect()
}

/// The MusicBrainz genre list as a lookup from [`genre_key`] to MusicBrainz's spelling, lower-cased.
#[derive(Debug, Clone)]
pub struct MbGenres(HashMap<String, String>);

impl MbGenres {
    pub fn new<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut map = HashMap::new();
        for name in names {
            map.entry(genre_key(name))
                .or_insert_with(|| name.to_lowercase());
        }
        Self(map)
    }

    /// The list shipped in `assets/mbGenres.json` (CC0, MusicBrainz core data).
    pub fn bundled() -> &'static MbGenres {
        static BUNDLED: OnceLock<MbGenres> = OnceLock::new();
        BUNDLED.get_or_init(|| {
            #[derive(Deserialize)]
            struct File {
                genres: Vec<String>,
            }
            let file: File = serde_json::from_str(include_str!("../assets/mbGenres.json"))
                .expect("bundled mbGenres.json parses");
            MbGenres::new(file.genres.iter().map(String::as_str))
        })
    }

    /// An artist's genres that are MusicBrainz genres, each in MusicBrainz's spelling, first occurrence order.
    /// `merged` is already the person's list when they set one, else the enriched list.
    pub fn matching_genres<S: AsRef<str>>(&self, merged: &[S]) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for tag in merged {
            if let Some(name) = self.0.get(&genre_key(tag.as_ref())) {
                if seen.insert(name.clone()) {
                    out.push(name.clone());
                }
            }
        }
        out
    }
}
