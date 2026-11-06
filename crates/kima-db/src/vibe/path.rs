//! The song path service (spec 6.7): `score_library` -> `song_path::walk` -> `library_tracks`.
//!
//! The arithmetic is `kima_vibe::song_path` (port of upstream `services/songPath.ts`); this module is the glue that
//! scores the library against both endpoints in one pass and loads the picked rows.

use std::collections::HashMap;
use std::fmt;

use super::rank::{
    LibraryTrack, RankError, WeightedInput, library_tracks, matching_context, score_library,
};
use kima_vibe::feel_probe::FeelProbe;
use kima_vibe::formula::pair_score;
use kima_vibe::song_path::{self, Mode, ScoredTrack};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct PathOptions {
    /// A non-zero explicit waypoint count; `None` derives it from the distance.
    pub length: Option<usize>,
    pub mode: Mode,
}

impl Default for PathOptions {
    fn default() -> Self {
        Self {
            length: None,
            mode: Mode::Smooth,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SongPath {
    pub start: LibraryTrack,
    pub end: LibraryTrack,
    pub path: Vec<LibraryTrack>,
    /// Waypoints picked plus the two endpoints.
    pub total_tracks: usize,
    /// `1 - pair_score(start, end) / SELF_SCORE`.
    pub embedding_distance: f64,
    /// Mean `distance` of the picked waypoints (0 without any).
    pub average_step_size: f64,
    pub mode: Mode,
}

#[derive(Debug)]
pub enum PathError {
    /// One of the tracks has no current matching data (or no state is built yet).
    NotMatched,
    TooSimilar,
    Db(sqlx::Error),
}

impl PathError {
    /// The HTTP status upstream answers (`UserFacingError(…, 400)` vs a failure).
    pub fn status(&self) -> u16 {
        match self {
            Self::NotMatched | Self::TooSimilar => 400,
            Self::Db(_) => 500,
        }
    }
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMatched => {
                f.write_str("One or both tracks have not been through the similarity step yet")
            }
            Self::TooSimilar => write!(f, "{}", song_path::PathError::TooSimilar),
            Self::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PathError {}

impl From<sqlx::Error> for PathError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

impl From<RankError> for PathError {
    fn from(e: RankError) -> Self {
        match e {
            RankError::Db(e) => Self::Db(e),
            RankError::NoMatchingData(_) => Self::NotMatched,
            other => Self::Db(sqlx::Error::Protocol(other.to_string())),
        }
    }
}

/// A path between two tracks on the matching formula. `pick_below(bound)` returns an index below `bound`
/// (discovery mode only; the caller supplies the randomness).
pub async fn generate_song_path(
    pool: &PgPool,
    probe: &FeelProbe,
    start: Uuid,
    end: Uuid,
    options: PathOptions,
    pick_below: impl FnMut(usize) -> usize,
) -> Result<SongPath, PathError> {
    let ctx = matching_context(pool, probe, &[start, end])
        .await?
        .ok_or(PathError::NotMatched)?;
    let (Some(s), Some(e)) = (ctx.tracks.get(&start), ctx.tracks.get(&end)) else {
        return Err(PathError::NotMatched);
    };
    let (cos_theta, distance) = song_path::cos_and_distance(pair_score(
        s,
        e,
        &ctx.state.genres,
        ctx.state.genre_weight_sq(),
    ));
    song_path::check_distance(distance).map_err(|_| PathError::TooSimilar)?;
    let waypoints = song_path::waypoint_count(distance, options.length);

    let sets = [
        vec![WeightedInput {
            track_id: start,
            weight: 1.0,
        }],
        vec![WeightedInput {
            track_id: end,
            weight: 1.0,
        }],
    ];
    let library: Vec<ScoredTrack> = score_library(pool, &ctx, &sets, &[start, end])
        .await?
        .into_iter()
        .map(|t| ScoredTrack {
            id: t.id.to_string(),
            artist_id: t.artist_id.to_string(),
            s0: t.scores[0],
            s1: t.scores[1],
        })
        .collect();
    let picked = song_path::walk(&library, cos_theta, waypoints, options.mode, pick_below);

    let ids: Vec<Uuid> = [start, end]
        .into_iter()
        .chain(picked.iter().filter_map(|p| Uuid::parse_str(&p.id).ok()))
        .collect();
    let mut info: HashMap<Uuid, LibraryTrack> = library_tracks(pool, &ids)
        .await?
        .into_iter()
        .map(|t| (t.id, t))
        .collect();
    let missing = || PathError::Db(sqlx::Error::RowNotFound);
    let path = picked
        .iter()
        .map(|p| {
            Uuid::parse_str(&p.id)
                .ok()
                .and_then(|id| info.get(&id).cloned())
                .ok_or_else(missing)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let total_step: f64 = picked.iter().map(|p| p.distance).sum();
    Ok(SongPath {
        start: info.remove(&start).ok_or_else(missing)?,
        end: info.remove(&end).ok_or_else(missing)?,
        total_tracks: path.len() + 2,
        average_step_size: if picked.is_empty() {
            0.0
        } else {
            total_step / picked.len() as f64
        },
        path,
        embedding_distance: distance,
        mode: options.mode,
    })
}
