//! Track mapping, album distance, candidate order and recommendation (spec §19.3, §19.4).

use super::distance::{
    Distance, ReleaseTrackRef, W_ALBUM, W_ALBUM_ID, W_ARTIST, W_MEDIUMS, W_MISSING_TRACKS,
    W_TRACKS, W_UNMATCHED_TRACKS, W_YEAR, string_dist, track_distance, year_penalty,
};
use crate::mbid::Mbid;
use crate::musicbrainz::{Credit, Release, ReleaseGroup, VARIOUS_ARTISTS_MBID, sortable_date};
use kima_db::autotag::{LocalAlbum, Recommendation};
use pathfinding::kuhn_munkres::kuhn_munkres_min;
use pathfinding::matrix::Matrix;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// A pair is broken into one extra and one missing track at or above this distance.
pub const BREAK_DISTANCE: f64 = 0.5;
pub const STRONG_BELOW: f64 = 0.04;
pub const MEDIUM_BELOW: f64 = 0.2;
/// The cost of an "unmatched" row or column, and the scale of the integer costs.
const PAD_COST: i64 = 1_000_000;
const COST_SCALE: f64 = 1_000_000.0;

/// One local track assigned to one release track (the §6.8 `mapping` entry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappedPair {
    pub track_id: Uuid,
    /// The release medium's position.
    pub medium: u32,
    /// The release track's position on its medium.
    pub position: u32,
    pub release_track_mbid: Mbid,
    pub distance: f64,
}

/// A release track no local track maps to (the §6.8 `missing_tracks` entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingTrack {
    pub medium: u32,
    pub position: u32,
    pub title: String,
    /// Milliseconds.
    pub length: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mapping {
    pub pairs: Vec<MappedPair>,
    /// Local tracks with no release track.
    pub extra: Vec<Uuid>,
    pub missing: Vec<MissingTrack>,
}

fn joined(credits: &[Credit]) -> String {
    credits
        .iter()
        .map(|c| format!("{}{}", c.name, c.joinphrase))
        .collect()
}

fn release_is_va(release: &Release) -> bool {
    release
        .artist_credit
        .iter()
        .any(|c| c.artist.id.0.to_string() == VARIOUS_ARTISTS_MBID)
        || kima_library::names::canonical_various(&joined(&release.artist_credit)).is_some()
}

/// Every release track with its medium position and its index across the release.
fn release_tracks(release: &Release) -> Vec<(u32, ReleaseTrackRef<'_>)> {
    let mut index = 0;
    let mut out = Vec::new();
    for medium in &release.media {
        for track in &medium.tracks {
            index += 1;
            out.push((
                medium.position,
                ReleaseTrackRef {
                    track,
                    absolute_index: index,
                },
            ));
        }
    }
    out
}

/// A pair's integer cost. A non-finite distance would cast to 0, a perfect pair: it costs as
/// much as no pair.
fn pair_cost(d: f64) -> i64 {
    if d.is_finite() {
        (d.clamp(0.0, 1.0) * COST_SCALE).round() as i64
    } else {
        PAD_COST
    }
}

/// Whether an assigned pair is broken into one extra and one missing track; a non-finite
/// distance always is.
fn broken(d: f64) -> bool {
    !d.is_finite() || d >= BREAK_DISTANCE
}

/// The assignment of local tracks to release tracks with the least summed distance (Hungarian,
/// on a square matrix padded with cost 1.0). A pair at distance `≥ 0.5` is broken.
pub fn map_tracks(local: &LocalAlbum, release: &Release) -> Mapping {
    let rts = release_tracks(release);
    let (nl, nr) = (local.tracks.len(), rts.len());
    let n = nl.max(nr);
    let mut mapping = Mapping::default();
    if n == 0 {
        return mapping;
    }
    let credit = joined(&release.artist_credit);
    let va = release_is_va(release);
    let dist: Vec<Vec<f64>> = local
        .tracks
        .iter()
        .map(|lt| {
            rts.iter()
                .map(|(_, rt)| track_distance(lt, rt, &credit, va))
                .collect()
        })
        .collect();
    let costs: Vec<i64> = (0..n)
        .flat_map(|i| (0..n).map(move |j| (i, j)))
        .map(|(i, j)| match dist.get(i).and_then(|row| row.get(j)) {
            Some(&d) => pair_cost(d),
            None => PAD_COST,
        })
        .collect();
    let matrix = Matrix::from_vec(n, n, costs).expect("n * n costs");
    let (_, assignment) = kuhn_munkres_min(&matrix);

    let mut taken = BTreeSet::new();
    let missing = |(medium, rt): &(u32, ReleaseTrackRef<'_>)| MissingTrack {
        medium: *medium,
        position: rt.track.position,
        title: rt.track.title.clone(),
        length: rt.track.length,
    };
    for (i, &j) in assignment.iter().enumerate().take(nl) {
        if j >= nr {
            mapping.extra.push(local.tracks[i].id);
        } else if broken(dist[i][j]) {
            mapping.extra.push(local.tracks[i].id);
            taken.insert(j);
            mapping.missing.push(missing(&rts[j]));
        } else {
            taken.insert(j);
            let (medium, rt) = &rts[j];
            mapping.pairs.push(MappedPair {
                track_id: local.tracks[i].id,
                medium: *medium,
                position: rt.track.position,
                release_track_mbid: rt.track.id,
                distance: dist[i][j],
            });
        }
    }
    // Release tracks given to no local track (the padding rows). Broken pairs were added above.
    let mut unmatched: Vec<MissingTrack> = (0..nr)
        .filter(|j| !taken.contains(j))
        .map(|j| missing(&rts[j]))
        .collect();
    mapping.missing.append(&mut unmatched);
    mapping
        .missing
        .sort_by_key(|m| (m.medium, m.position, m.title.clone()));
    mapping
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub distance: f64,
    pub recommendation: Recommendation,
    /// Each key's share of the distance; the shares sum to it.
    pub penalties: BTreeMap<String, f64>,
    pub mapping: Mapping,
}

fn year_of(date: Option<&str>) -> Option<i32> {
    let head = date?.get(..4)?;
    if !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    head.parse().ok().filter(|&y| y > 0)
}

fn is_unknown_artist(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("unknown artist")
}

/// strong below 0.04, medium below 0.2, else none; an incomplete mapping is at most medium.
pub fn recommend(distance: f64, incomplete: bool) -> Recommendation {
    if distance < STRONG_BELOW && !incomplete {
        Recommendation::Strong
    } else if distance < MEDIUM_BELOW {
        Recommendation::Medium
    } else {
        Recommendation::Weak
    }
}

/// The distance of a release from a local album (§19.3) and what follows from it.
pub fn score(
    local: &LocalAlbum,
    release: &Release,
    group: &ReleaseGroup,
    this_year: i32,
) -> Scored {
    let mapping = map_tracks(local, release);
    let mut d = Distance::default();
    d.add("album", W_ALBUM, string_dist(&local.title, &release.title));

    let credit = joined(&release.artist_credit);
    if !is_unknown_artist(&local.artist_name) {
        let both_va = release_is_va(release)
            && kima_library::names::canonical_various(&local.artist_name).is_some();
        let penalty = if both_va {
            0.0
        } else {
            string_dist(&local.artist_name, &credit)
        };
        d.add("artist", W_ARTIST, penalty);
    }
    if let Some(id) = local.release_mbid {
        d.add(
            "album_id",
            W_ALBUM_ID,
            if id == release.id.0 { 0.0 } else { 1.0 },
        );
    }
    if let (Some(y), Some(ry)) = (local.year, year_of(release.date.as_deref())) {
        let original = year_of(group.first_release_date.as_deref());
        d.add("year", W_YEAR, year_penalty(y, ry, original, this_year));
    }
    let discs = local.tracks.iter().map(|t| t.disc).collect::<BTreeSet<_>>();
    let disc_total = local
        .tracks
        .iter()
        .filter_map(|t| t.disc_total)
        .filter(|&t| t > 0)
        .max();
    if discs.len() > 1 || disc_total.is_some() {
        let local_count = disc_total.map_or(discs.len(), |t| t as usize);
        let same = local_count == release.media.len();
        d.add("mediums", W_MEDIUMS, if same { 0.0 } else { 1.0 });
    }
    for pair in &mapping.pairs {
        d.add("tracks", W_TRACKS, pair.distance);
    }
    for _ in &mapping.missing {
        d.add("missing_tracks", W_MISSING_TRACKS, 1.0);
    }
    for _ in &mapping.extra {
        d.add("unmatched_tracks", W_UNMATCHED_TRACKS, 1.0);
    }

    let distance = d.value();
    let incomplete = !mapping.missing.is_empty() || !mapping.extra.is_empty();
    Scored {
        distance,
        recommendation: recommend(distance, incomplete),
        penalties: d
            .shares()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        mapping,
    }
}

/// How candidates sort: derived `Ord` over the fields, in this order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrderKey {
    /// The distance in thousandths, rounded.
    pub millis: i64,
    /// `false` (sorts first) for an `Official` release.
    pub not_official: bool,
    pub media: u8,
    pub country: u8,
    pub date: String,
    pub id: Mbid,
}

fn media_rank(format: Option<&str>) -> u8 {
    match format {
        Some("Digital Media") => 0,
        Some("CD") => 1,
        Some(f) if f.ends_with(" CD") => 2,
        _ => 3,
    }
}

fn country_rank(country: Option<&str>) -> u8 {
    match country {
        Some("XW") => 0,
        Some("XE") => 1,
        _ => 2,
    }
}

pub fn order_key(release: &Release, scored: &Scored) -> OrderKey {
    OrderKey {
        millis: (scored.distance * 1000.0).round() as i64,
        not_official: release.status.as_deref() != Some("Official"),
        // The worst medium of the release.
        media: release
            .media
            .iter()
            .map(|m| media_rank(m.format.as_deref()))
            .max()
            .unwrap_or(3),
        country: country_rank(release.country.as_deref()),
        date: sortable_date(release.date.as_deref()),
        id: release.id,
    }
}

/// Best candidate first (§19.3 tie-breakers).
pub fn order(cands: &mut [(Release, Scored)]) {
    cands.sort_by_cached_key(|(r, s)| order_key(r, s));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_distance_is_never_a_perfect_pair() {
        for d in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(pair_cost(d), PAD_COST, "{d}");
            assert!(broken(d), "{d}");
        }
        assert_eq!(pair_cost(0.0), 0);
        assert_eq!(pair_cost(0.25), 250_000);
        assert!(!broken(0.499));
        assert!(broken(BREAK_DISTANCE));
    }
}
