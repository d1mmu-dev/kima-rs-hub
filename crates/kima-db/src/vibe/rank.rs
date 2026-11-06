//! The ranking pass every vibe feature uses (spec 6.6): the library scored against a weighted set of input tracks
//! with the matching formula `sum_i w_i s(t, i)` in one exact scan, scoring in SQL (pgvector) as upstream does.
//!
//! Port of upstream `services/matching/rank.ts`. Dynamic SQL is built with bound parameters only: vectors travel
//! as `$n::real[]::vector`, artist bonuses as a `uuid[]` plus one `float8[]` per input set. Every read of vectors
//! goes through [`ELIGIBLE`].

use std::collections::HashMap;
use std::fmt;

use super::ELIGIBLE;
use super::state::state_version;
use kima_vibe::feel_probe::FeelProbe;
use kima_vibe::formula::{
    self, ArtistProfile, FEEL_WEIGHT, FeelStats, GenreModel, MAX_PER_ALBUM, MAX_PER_ARTIST,
    MatchTrack, WeightedTrack,
};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

#[derive(Debug)]
pub enum RankError {
    Db(sqlx::Error),
    /// These tracks have no current matching data (no eligible vector, no current feel).
    NoMatchingData(Vec<Uuid>),
    /// The weighted inputs add up to a vector of length 0 (or less): there is no direction to rank by.
    CancelsOut,
    /// The stored genre model names an artist id that is not a UUID.
    BadState(String),
}

impl fmt::Display for RankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Db(e) => write!(f, "{e}"),
            Self::NoMatchingData(ids) => {
                write!(
                    f,
                    "No matching data for track {}",
                    ids.first().map_or_else(String::new, |i| i.to_string())
                )
            }
            Self::CancelsOut => {
                f.write_str("The tracks to add and to subtract cancel each other out")
            }
            Self::BadState(m) => write!(f, "matching state is unusable: {m}"),
        }
    }
}

impl std::error::Error for RankError {}

impl From<sqlx::Error> for RankError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// The library statistics the formula needs, read from the `matching_state` row (`loadMatchingState`).
#[derive(Debug, Clone, PartialEq)]
pub struct MatchingState {
    pub track_count: i32,
    pub feel: FeelStats,
    pub sound_spread: f64,
    pub genre_spread: f64,
    pub genre_weight: f64,
    pub genres: GenreModel,
}

impl MatchingState {
    pub fn genre_weight_sq(&self) -> f64 {
        self.genre_weight * self.genre_weight
    }
}

/// The state row, or `None` until one is built for this probe and formula version.
pub async fn load_matching_state(
    pool: &PgPool,
    probe: &FeelProbe,
) -> sqlx::Result<Option<MatchingState>> {
    #[derive(FromRow)]
    struct Row {
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
    let row: Option<Row> = sqlx::query_as(
        "SELECT version, track_count, feel_mean_valence, feel_mean_arousal, feel_sd_valence, feel_sd_arousal,
                sound_spread, genre_spread, genre_weight, genres FROM matching_state WHERE id = 'default'",
    )
    .fetch_optional(pool)
    .await?;
    let Some(r) = row.filter(|r| r.version == state_version(probe)) else {
        return Ok(None);
    };
    let genres = serde_json::from_value(r.genres).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    Ok(Some(MatchingState {
        track_count: r.track_count,
        feel: FeelStats {
            mean: [r.feel_mean_valence, r.feel_mean_arousal],
            sd: [r.feel_sd_valence, r.feel_sd_arousal],
        },
        sound_spread: r.sound_spread,
        genre_spread: r.genre_spread,
        genre_weight: r.genre_weight,
        genres,
    }))
}

/// A track with the columns every list shows.
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct LibraryTrack {
    pub id: Uuid,
    pub title: String,
    /// Seconds.
    pub duration_secs: i32,
    pub album_id: Uuid,
    pub album_title: String,
    /// A stored cover exists (`image_path` set); the bytes are served by album id.
    pub album_has_cover: bool,
    pub artist_id: Uuid,
    pub artist_name: String,
}

#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct RankedTrack {
    #[sqlx(flatten)]
    pub track: LibraryTrack,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightedInput {
    pub track_id: Uuid,
    pub weight: f64,
}

/// A track that takes one of the places its album and artist are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counted {
    pub album_id: Uuid,
    pub artist_id: Uuid,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredTrack {
    pub id: Uuid,
    pub album_id: Uuid,
    pub artist_id: Uuid,
    /// One score per input set, in order.
    pub scores: Vec<f64>,
}

const FEEL_SQ: f64 = FEEL_WEIGHT * FEEL_WEIGHT;

/// The library's state and the named tracks' matching data.
#[derive(Debug, Clone)]
pub struct MatchingContext {
    pub state: MatchingState,
    probe_id: String,
    /// The requested tracks that have current matching data.
    pub tracks: HashMap<Uuid, MatchTrack>,
}

impl MatchingContext {
    /// The ids (in the given order) that have no matching data.
    pub fn missing<'a>(&self, ids: impl IntoIterator<Item = &'a Uuid>) -> Vec<Uuid> {
        ids.into_iter()
            .filter(|id| !self.tracks.contains_key(id))
            .copied()
            .collect()
    }

    fn counted(&self, ids: &[Uuid]) -> Result<Vec<Counted>, RankError> {
        let missing = self.missing(ids);
        if !missing.is_empty() {
            return Err(RankError::NoMatchingData(missing));
        }
        ids.iter()
            .map(|id| {
                let t = &self.tracks[id];
                Ok(Counted {
                    album_id: parse_id(&t.album_id)?,
                    artist_id: parse_id(&t.artist_id)?,
                })
            })
            .collect()
    }
}

fn parse_id(s: &str) -> Result<Uuid, RankError> {
    Uuid::parse_str(s).map_err(|e| RankError::BadState(format!("{s}: {e}")))
}

/// The state and the matching data of `track_ids`; `None` until a state is built (`matchingContext`).
pub async fn matching_context(
    pool: &PgPool,
    probe: &FeelProbe,
    track_ids: &[Uuid],
) -> sqlx::Result<Option<MatchingContext>> {
    let Some(state) = load_matching_state(pool, probe).await? else {
        return Ok(None);
    };
    let eligible: &str = &ELIGIBLE;
    let rows: Vec<(Uuid, Uuid, Uuid, Vec<f32>, f32, f32)> = sqlx::query_as(&format!(
        "SELECT te.track_id, t.album_id, al.artist_id, te.embedding::real[], f.valence, f.arousal
         FROM (SELECT te.track_id, te.embedding, te.created_at, t.album_id FROM {eligible}) te
         JOIN tracks t ON t.id = te.track_id
         JOIN albums al ON al.id = t.album_id
         JOIN matching_feel f ON f.track_id = te.track_id AND f.embedded_at = te.created_at AND f.probe = $2
         WHERE te.track_id = ANY($1)"
    ))
    .bind(track_ids)
    .bind(&probe.probe_id)
    .fetch_all(pool)
    .await?;
    let tracks = rows
        .into_iter()
        .map(|(id, album, artist, embedding, valence, arousal)| {
            let sound: Vec<f64> = embedding.iter().map(|x| f64::from(*x)).collect();
            (
                id,
                MatchTrack {
                    track_id: id.to_string(),
                    album_id: album.to_string(),
                    artist_id: artist.to_string(),
                    sound: formula::unit(&sound),
                    feel: formula::feel_vector(
                        [f64::from(valence), f64::from(arousal)],
                        &state.feel,
                    ),
                },
            )
        })
        .collect();
    Ok(Some(MatchingContext {
        state,
        probe_id: probe.probe_id.clone(),
        tracks,
    }))
}

/// Inputs as the formula's weighted tracks; a track without matching data is an error.
pub fn weighted_tracks<'a>(
    ctx: &'a MatchingContext,
    inputs: &[WeightedInput],
) -> Result<Vec<WeightedTrack<'a>>, RankError> {
    let missing = ctx.missing(inputs.iter().map(|i| &i.track_id));
    if !missing.is_empty() {
        return Err(RankError::NoMatchingData(missing));
    }
    Ok(inputs
        .iter()
        .map(|i| WeightedTrack {
            track: &ctx.tracks[&i.track_id],
            weight: i.weight,
        })
        .collect())
}

/// `blendSelfScore` of the inputs (the squared length of the blend); `CancelsOut` unless it is positive.
pub fn check_blend(ctx: &MatchingContext, inputs: &[WeightedInput]) -> Result<f64, RankError> {
    let blend_sq = formula::blend_self_score(
        &weighted_tracks(ctx, inputs)?,
        &ctx.state.genres,
        ctx.state.genre_weight_sq(),
    );
    if blend_sq > 0.0 {
        Ok(blend_sq)
    } else {
        Err(RankError::CancelsOut)
    }
}

/// `WITH e AS (<eligible rows except exclude>), bonus AS (...) SELECT <select>, <one score per set> FROM e ...`
/// (upstream `scoreQuery`). `select` may use `e.*` (track_id, title, duration_secs, album_id), `al` (album) and
/// `ar` (artist); the scores are named `names`.
fn score_query<'a>(
    qb: &mut QueryBuilder<'a, Postgres>,
    ctx: &'a MatchingContext,
    sets: &[Vec<WeightedTrack<'_>>],
    names: &[&str],
    select: &str,
    exclude: &'a [Uuid],
) -> Result<(), RankError> {
    let gw_sq = ctx.state.genre_weight_sq();
    let bonus_maps: Vec<_> = sets
        .iter()
        .map(|set| formula::artist_bonuses(set, &ctx.state.genres, gw_sq))
        .collect();
    let artist_ids: Vec<Uuid> = bonus_maps[0]
        .keys()
        .map(|k| parse_id(k))
        .collect::<Result<_, _>>()?;
    let eligible: &str = &ELIGIBLE;
    qb.push(format!(
        "WITH e AS (SELECT te.track_id, te.embedding, te.created_at, t.album_id, t.title, t.duration_secs
                    FROM {eligible} AND te.track_id <> ALL("
    ));
    qb.push_bind(exclude);
    qb.push(")), bonus AS (SELECT * FROM unnest(");
    qb.push_bind(artist_ids);
    qb.push("::uuid[]");
    for map in &bonus_maps {
        let column: Vec<f64> = map.values().copied().collect();
        qb.push(", ");
        qb.push_bind(column);
        qb.push("::float8[]");
    }
    qb.push(") AS b(artist_id");
    for k in 0..sets.len() {
        qb.push(format!(", v{k}"));
    }
    qb.push(")) SELECT ");
    qb.push(select);
    for (k, set) in sets.iter().enumerate() {
        let (sound, feel) = formula::blend_sound_feel(set);
        let sound_norm = formula::dot(&sound, &sound).sqrt();
        qb.push(", (");
        if sound_norm > 0.0 {
            qb.push_bind(sound_norm);
            qb.push("::float8 * (1 - (e.embedding <=> ");
            qb.push_bind(sound.iter().map(|x| *x as f32).collect::<Vec<f32>>());
            qb.push("::real[]::vector))");
        } else {
            qb.push("0::float8");
        }
        qb.push(format!(" + {FEEL_SQ:?}::float8 * COALESCE((z.v * "));
        qb.push_bind(feel[0]);
        qb.push("::float8 + z.a * ");
        qb.push_bind(feel[1]);
        qb.push(format!(
            "::float8) / NULLIF(sqrt(z.v * z.v + z.a * z.a), 0), 0) + COALESCE(b.v{k}, 0)) AS \"{}\"",
            names[k]
        ));
    }
    qb.push(" FROM e JOIN matching_feel f ON f.track_id = e.track_id AND f.embedded_at = e.created_at AND f.probe = ");
    qb.push_bind(&ctx.probe_id);
    qb.push(" CROSS JOIN LATERAL ");
    standardised_feel(qb, &ctx.state);
    qb.push(
        " z JOIN albums al ON al.id = e.album_id JOIN artists ar ON ar.id = al.artist_id
          LEFT JOIN bonus b ON b.artist_id = ar.id",
    );
    Ok(())
}

/// `(SELECT v, a)`: the stored feel standardised by the library's mean and SD (an axis with SD 0 is 0).
fn standardised_feel<'a>(qb: &mut QueryBuilder<'a, Postgres>, state: &MatchingState) {
    let FeelStats { mean, sd } = state.feel;
    qb.push("(SELECT ");
    for (k, (name, col)) in [("v", "valence"), ("a", "arousal")].into_iter().enumerate() {
        if k > 0 {
            qb.push(", ");
        }
        if sd[k] > 0.0 {
            qb.push(format!("(f.{col}::float8 - "));
            qb.push_bind(mean[k]);
            qb.push("::float8) / ");
            qb.push_bind(sd[k]);
            qb.push(format!("::float8 AS {name}"));
        } else {
            qb.push(format!("0::float8 AS {name}"));
        }
    }
    qb.push(")");
}

const TRACK_COLUMNS: &str =
    "e.track_id AS id, e.title, e.duration_secs, al.id AS album_id, al.title AS album_title,
    (al.image_path IS NOT NULL) AS album_has_cover, ar.id AS artist_id, ar.name AS artist_name";

fn tally(keys: impl IntoIterator<Item = Uuid>) -> (Vec<Uuid>, Vec<i32>) {
    let mut counts: Vec<(Uuid, i32)> = vec![];
    for k in keys {
        match counts.iter_mut().find(|(id, _)| *id == k) {
            Some((_, n)) => *n += 1,
            None => counts.push((k, 1)),
        }
    }
    counts.into_iter().unzip()
}

/// The top `limit` rows of the scored query with at most 2 per album and 4 per artist, each counted track taking
/// one place in its own album and artist. Keeping the best rows per album, then per artist among those, is the
/// same as walking the ranked list in order (upstream `capRanked`).
async fn cap_ranked(
    pool: &PgPool,
    ctx: &MatchingContext,
    set: &[WeightedTrack<'_>],
    exclude: &[Uuid],
    counted: &[Counted],
    limit: i64,
) -> Result<Vec<RankedTrack>, RankError> {
    let (album_keys, album_counts) = tally(counted.iter().map(|c| c.album_id));
    let (artist_keys, artist_counts) = tally(counted.iter().map(|c| c.artist_id));
    let mut qb = QueryBuilder::<Postgres>::new("WITH scored AS (");
    score_query(
        &mut qb,
        ctx,
        std::slice::from_ref(&set.to_vec()),
        &["score"],
        TRACK_COLUMNS,
        exclude,
    )?;
    qb.push("), album_used AS (SELECT * FROM unnest(");
    qb.push_bind(album_keys);
    qb.push("::uuid[], ");
    qb.push_bind(album_counts);
    qb.push("::int[]) AS u(album_id, used)), artist_used AS (SELECT * FROM unnest(");
    qb.push_bind(artist_keys);
    qb.push("::uuid[], ");
    qb.push_bind(artist_counts);
    qb.push(
        "::int[]) AS u(artist_id, used)),
         album_ranked AS (
            SELECT s.*, ROW_NUMBER() OVER (PARTITION BY s.album_id ORDER BY s.score DESC, s.id) AS album_rank
            FROM scored s),
         album_kept AS (
            SELECT r.* FROM album_ranked r LEFT JOIN album_used u ON u.album_id = r.album_id
            WHERE r.album_rank <= ",
    );
    qb.push_bind(MAX_PER_ALBUM as i32);
    qb.push(
        " - COALESCE(u.used, 0)),
         artist_ranked AS (
            SELECT k.*, ROW_NUMBER() OVER (PARTITION BY k.artist_id ORDER BY k.score DESC, k.id) AS artist_rank
            FROM album_kept k)
         SELECT r.id, r.title, r.duration_secs, r.album_id, r.album_title, r.album_has_cover,
                r.artist_id, r.artist_name, r.score
         FROM artist_ranked r LEFT JOIN artist_used u ON u.artist_id = r.artist_id
         WHERE r.artist_rank <= ",
    );
    qb.push_bind(MAX_PER_ARTIST as i32);
    qb.push(" - COALESCE(u.used, 0) ORDER BY r.score DESC, r.id LIMIT ");
    qb.push_bind(limit);
    Ok(qb.build_query_as::<RankedTrack>().fetch_all(pool).await?)
}

/// The top `limit` tracks for one input set, inputs excluded, capped with the `counted` inputs taking places.
pub async fn rank_matches(
    pool: &PgPool,
    ctx: &MatchingContext,
    inputs: &[WeightedInput],
    limit: i64,
    counted: &[Uuid],
) -> Result<Vec<RankedTrack>, RankError> {
    let set = weighted_tracks(ctx, inputs)?;
    let counted = ctx.counted(counted)?;
    let exclude: Vec<Uuid> = inputs.iter().map(|i| i.track_id).collect();
    cap_ranked(pool, ctx, &set, &exclude, &counted, limit).await
}

/// The `limit` eligible tracks closest to one input set, best first, no album or artist cap, the inputs included
/// (an input is among its own closest matches) unless `exclude` names them. Ordering and limit run in the database.
pub async fn top_scores(
    pool: &PgPool,
    ctx: &MatchingContext,
    inputs: &[WeightedInput],
    limit: i64,
    exclude: &[Uuid],
) -> Result<Vec<(Uuid, f64)>, RankError> {
    let set = weighted_tracks(ctx, inputs)?;
    let mut qb = QueryBuilder::<Postgres>::new("WITH scored AS (");
    score_query(
        &mut qb,
        ctx,
        std::slice::from_ref(&set),
        &["score"],
        "e.track_id AS id",
        exclude,
    )?;
    qb.push(") SELECT id, score FROM scored ORDER BY score DESC, id LIMIT ");
    qb.push_bind(limit);
    Ok(qb.build_query_as::<(Uuid, f64)>().fetch_all(pool).await?)
}

/// Every eligible track except `exclude`, with one score per input set, in input-set order.
pub async fn score_library(
    pool: &PgPool,
    ctx: &MatchingContext,
    sets: &[Vec<WeightedInput>],
    exclude: &[Uuid],
) -> Result<Vec<ScoredTrack>, RankError> {
    let weighted: Vec<Vec<WeightedTrack>> = sets
        .iter()
        .map(|s| weighted_tracks(ctx, s))
        .collect::<Result<_, _>>()?;
    let names: Vec<String> = (0..sets.len()).map(|k| format!("s{k}")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut qb = QueryBuilder::<Postgres>::new("");
    score_query(
        &mut qb,
        ctx,
        &weighted,
        &name_refs,
        "e.track_id AS id, al.id AS album_id, ar.id AS artist_id",
        exclude,
    )?;
    use sqlx::Row;
    let rows = qb.build().fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            Ok(ScoredTrack {
                id: r.try_get("id")?,
                album_id: r.try_get("album_id")?,
                artist_id: r.try_get("artist_id")?,
                scores: names
                    .iter()
                    .map(|n| r.try_get::<f64, _>(n.as_str()))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect::<sqlx::Result<_>>()
        .map_err(RankError::from)
}

/// The named tracks with their album and artist, in no particular order.
pub async fn library_tracks(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<Vec<LibraryTrack>> {
    sqlx::query_as(
        "SELECT t.id, t.title, t.duration_secs, al.id AS album_id, al.title AS album_title,
                (al.image_path IS NOT NULL) AS album_has_cover, ar.id AS artist_id, ar.name AS artist_name
         FROM tracks t JOIN albums al ON al.id = t.album_id JOIN artists ar ON ar.id = al.artist_id
         WHERE t.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
}

/// Each named artist's mean unit sound vector and mean feel vector over its eligible tracks.
pub async fn artist_profiles(
    pool: &PgPool,
    probe: &FeelProbe,
    state: &MatchingState,
    artist_ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, ArtistProfile>> {
    let eligible: &str = &ELIGIBLE;
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT ar.id, avg(l2_normalize(e.embedding))::real[],
                avg(COALESCE(z.v / NULLIF(sqrt(z.v * z.v + z.a * z.a), 0), 0))::float8,
                avg(COALESCE(z.a / NULLIF(sqrt(z.v * z.v + z.a * z.a), 0), 0))::float8
         FROM (SELECT te.track_id, te.embedding, te.created_at, t.album_id FROM {eligible}) e
         JOIN matching_feel f ON f.track_id = e.track_id AND f.embedded_at = e.created_at AND f.probe = "
    ));
    qb.push_bind(&probe.probe_id);
    qb.push(" CROSS JOIN LATERAL ");
    standardised_feel(&mut qb, state);
    qb.push(" z JOIN albums al ON al.id = e.album_id JOIN artists ar ON ar.id = al.artist_id WHERE ar.id = ANY(");
    qb.push_bind(artist_ids);
    qb.push(") GROUP BY ar.id");
    let rows = qb
        .build_query_as::<(Uuid, Vec<f32>, f64, f64)>()
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(id, sound, v, a)| {
            (
                id,
                ArtistProfile {
                    artist_id: id.to_string(),
                    sound: sound.iter().map(|x| f64::from(*x)).collect(),
                    feel: [v, a],
                },
            )
        })
        .collect())
}
