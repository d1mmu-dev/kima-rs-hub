//! The recipe engine's ranking in SQL (M5b Task 4; port of upstream `services/mixes/recipes.ts` `recipeRankingSql`,
//! `recipeMembers`, `pickMixTracks`, `pickPageTracks`, `pageMemberCount` and the definitions' SQL half).
//!
//! A recipe blends moods and readings into one score per song (weighted library z-scores against the stored mood
//! statistics, a `low:` component negative), standardised again within the songs it can score; a member sits at least
//! [`RECIPE_LINE`] SD above that average and passes the literal filters. One copy per title (any case) and artist.
//!
//! On kima's schema (spec M5 §8, D-186): the scored songs are the tracks the mood statistics cover (an eligible vector
//! of [`crate::vibe::MATCHING_SOUND`], file present, scores of the heads' weights for that vector); readings come from
//! the current `track_clap_readings` row; tempo, its confidence (0 to 1, the steady beat is [`STEADY_BEAT_KIMA`],
//! D-262) and the key's scale from `track_analysis`. Without heads (M5 Q3: blocked) or before the mood step has built
//! statistics for them, every recipe has no members.
//!
//! The statement text is built at run time like the other vibe queries (`kima_db::vibe::moods`): the component list
//! decides its shape. Every number spliced into it is a finite `f64` from a parsed recipe or the stored statistics;
//! the heads' version and the familiarity boost are bind parameters.

use std::fmt;

use kima_mixes::artist::{MIN_MIX_TRACKS, NO_ONE_KEYS};
use kima_mixes::definitions::{
    Definition, DefinitionReading, NEUTRAL_PACE, PACE_BAND, Part, PartKind, definition_of,
};
use kima_mixes::recipes::{
    RECIPE_LINE, Recipe, STEADY_BEAT_KIMA, Signal, draw_members, get_recipe,
};
use kima_vibe::mood_heads::{MOODS, MoodHeads};
use kima_vibe::moods::MoodStats;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::vibe::ELIGIBLE;
use crate::vibe::moods::{MoodState, READINGS, load_mood_state, reading_sql};

/// Why a recipe cannot answer.
#[derive(Debug)]
pub enum RecipeError {
    /// Upstream `UnrankableLibraryError`: a component, the key's scale or the recipe score has no spread in this
    /// library. A property of the library, not a fault; callers show no mix or an empty station.
    Unrankable {
        recipe_id: String,
        reason: String,
    },
    /// A request the engine refuses (upstream's thrown `Error`/`RangeError`): an unknown recipe, a draw without a
    /// listener or a day, a limit that cannot make a mix.
    Invalid(String),
    Db(sqlx::Error),
}

impl fmt::Display for RecipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecipeError::Unrankable { recipe_id, reason } => {
                write!(f, "[Mixes] Recipe {recipe_id}: {reason}")
            }
            RecipeError::Invalid(message) => f.write_str(message),
            RecipeError::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RecipeError {}

impl From<sqlx::Error> for RecipeError {
    fn from(e: sqlx::Error) -> Self {
        RecipeError::Db(e)
    }
}

fn unrankable(recipe_id: &str, reason: impl Into<String>) -> RecipeError {
    RecipeError::Unrankable {
        recipe_id: recipe_id.to_string(),
        reason: reason.into(),
    }
}

/// Upstream `FamiliarBoost`: per track, how much it sounds like the listener's most-heard songs (familiarity.ts,
/// M5b Task 8); the type lives with the listener store that keeps it.
pub use kima_mixes::familiar::FamiliarBoost;

/// What a ranking reads besides the recipe: the mood heads (`None` while they are blocked: no recipe ranks) and the
/// listener's familiarity boost, used only by a recipe that carries a familiarity share.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecipeContext<'a> {
    pub heads: Option<&'a MoodHeads>,
    pub boost: Option<&'a FamiliarBoost>,
}

/// A member: the track and the artist it counts toward for the cap (upstream `artistSql`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: Uuid,
    pub artist_id: String,
}

/// A mix's songs (upstream `RecipeMix`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeMix {
    /// The drawn songs, in draw order.
    pub track_ids: Vec<Uuid>,
    /// How many songs the draw was taken from: the members through the filters, one per title and artist.
    pub members: usize,
}

/// Who and when a mix is drawn for: the same draw for one recipe, day and listener (the profile id).
#[derive(Debug, Clone, Copy)]
pub struct MixDraw<'a> {
    pub listener: &'a str,
    /// The day the mix holds for, or anything that stands for a reroll.
    pub date: &'a str,
    pub limit: usize,
}

/// A spread to score against: above 0 (a NaN has none).
pub(crate) fn has_spread(sd: f64) -> bool {
    sd > 0.0
}

/// A finite number as an SQL literal; anything else (a NaN or infinite weight, bound or statistic) is refused
/// rather than written into the SQL.
fn lit(x: f64) -> Result<String, RecipeError> {
    if !x.is_finite() {
        return Err(RecipeError::Invalid(format!(
            "Recipe engine: {x} is not a finite number"
        )));
    }
    Ok(format!("({x})::float8"))
}

/// One of the engine's own constants as an SQL literal (finite by construction).
fn const_lit(x: f64) -> String {
    lit(x).expect("the engine's constants are finite")
}

// A scale the key extractor did not name is no reading, not major.
const MINOR_SQL: &str =
    "CASE ta.key_mode WHEN 'minor' THEN 1.0::float8 WHEN 'major' THEN 0.0::float8 END";

/// Upstream `artistSql`: the album artist, except on the rows that name no one, where each song is its own artist.
/// Aliases t, a, ar.
pub fn artist_sql() -> String {
    let keys = NO_ONE_KEYS
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CASE WHEN ar.name_key IN ({keys}) THEN 'song:' || t.id::text ELSE a.artist_id::text END"
    )
}

/// Upstream `SCORED_SONGS`: the songs the mood statistics cover, the ones every recipe and the Wildcard station draw
/// from (`FROM` through the joins; `$1` = the heads' version). The same tracks `kima_db::vibe::moods` builds the
/// statistics over. Aliases m (scores), t (track), a (album), ar (album artist), r (current CLAP readings, may be
/// absent), ta (current analysis, may be absent).
pub fn scored_songs() -> String {
    let eligible: &str = &ELIGIBLE;
    format!(
        "FROM (SELECT te.track_id, te.created_at FROM {eligible}) e
         JOIN track_moods m ON m.track_id = e.track_id AND m.embedded_at = e.created_at AND m.weights = $1
         JOIN tracks t ON t.id = e.track_id
         JOIN albums a ON a.id = t.album_id
         JOIN artists ar ON ar.id = a.artist_id
         LEFT JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at
         LEFT JOIN track_analysis ta ON ta.track_id = t.id AND ta.file_generation = t.ml_generation"
    )
}

/// Upstream `PACE_SQL`: 1 inside the band (double or half counts) on a steady beat, 0 outside, else neutral.
fn pace_sql() -> String {
    let (lo, hi) = PACE_BAND;
    let steady = const_lit(STEADY_BEAT_KIMA);
    format!(
        "CASE WHEN ta.tempo_bpm IS NULL OR ta.tempo_confidence IS NULL OR ta.tempo_confidence < {steady} THEN {neutral}
         WHEN ta.tempo_bpm BETWEEN {lo} AND {hi} OR ta.tempo_bpm * 2 BETWEEN {lo} AND {hi}
              OR ta.tempo_bpm / 2 BETWEEN {lo} AND {hi}
         THEN 1::float8 ELSE 0::float8 END",
        neutral = const_lit(NEUTRAL_PACE),
        lo = const_lit(lo),
        hi = const_lit(hi),
    )
}

fn reading_index(signal: &Signal) -> Option<usize> {
    match signal {
        Signal::Arousal => Some(0),
        Signal::Instrumental => Some(1),
        Signal::Acoustic => Some(2),
        _ => None,
    }
}

fn definition_reading_index(reading: DefinitionReading) -> usize {
    match reading {
        DefinitionReading::Arousal => 0,
        DefinitionReading::Instrumental => 1,
        DefinitionReading::Acoustic => 2,
    }
}

fn mood_index(mood: &str) -> usize {
    MOODS
        .iter()
        .position(|m| *m == mood)
        .expect("recipe moods are parsed against MOODS")
}

fn mood_sql(mood: &str) -> String {
    format!("m.\"{}\"::float8", MOODS[mood_index(mood)])
}

fn reading_value_sql(k: usize) -> String {
    reading_sql(READINGS[k]).expect("READINGS names a reading")
}

fn is_defined(signal: &Signal) -> bool {
    matches!(signal, Signal::Mood(m) if definition_of(m).is_some())
}

/// The part's input column in the library CTE: one per mood, reading or the pace.
fn part_column(part: &Part) -> String {
    match &part.kind {
        PartKind::Mood(m) | PartKind::Head(m) => format!("dm_{m}"),
        PartKind::Reading(r) => format!("dr_{}", r.name()),
        PartKind::Pace => "dpace".to_string(),
    }
}

fn score_column(mood: &str) -> String {
    format!("dscore_{mood}")
}

/// The stored statistics of a mood or reading component, or why the recipe cannot use it.
fn scale_of(recipe_id: &str, state: &MoodState, signal: &Signal) -> Result<MoodStats, RecipeError> {
    let stats = match signal {
        Signal::Mood(m) => Some(state.moods[mood_index(m)]),
        other => reading_index(other).and_then(|k| state.readings[k]),
    };
    let name = signal.name();
    let Some(stats) = stats else {
        return Err(unrankable(
            recipe_id,
            format!("no song the statistics cover has the {name} reading"),
        ));
    };
    if !has_spread(stats.sd) {
        return Err(unrankable(
            recipe_id,
            format!("the library's {name} has no spread to score against"),
        ));
    }
    Ok(stats)
}

/// Upstream `definedCtes` over `library` (alias l): `pace_scale` when a definition reads pace, then `defined` with a
/// `dscore_<mood>` per row (weighted parts' z-scores over the weights' sizes, NULL when a reading is missing).
/// `extra_columns`/`extra_joins` extend `defined`.
pub(crate) fn defined_ctes(
    recipe_id: &str,
    state: &MoodState,
    definitions: &[&Definition],
    extra_columns: &str,
    extra_joins: &str,
) -> Result<String, RecipeError> {
    let uses_pace = definitions
        .iter()
        .any(|d| d.parts.iter().any(|p| p.kind == PartKind::Pace));
    let mut scores = Vec::new();
    for definition in definitions {
        let mut terms = Vec::new();
        for part in &definition.parts {
            let column = format!("l.{}", part_column(part));
            let weight = lit(part.weight)?;
            if part.kind == PartKind::Pace {
                terms.push(format!(
                    "{weight} * COALESCE(({column} - ps.mean) / NULLIF(ps.sd, 0), 0)"
                ));
                continue;
            }
            let (stats, name) = match &part.kind {
                PartKind::Reading(r) => (
                    state.readings[definition_reading_index(*r)],
                    r.name().to_string(),
                ),
                PartKind::Mood(m) | PartKind::Head(m) => {
                    (Some(state.moods[mood_index(m)]), m.clone())
                }
                PartKind::Pace => unreachable!(),
            };
            let Some(stats) = stats else {
                return Err(unrankable(
                    recipe_id,
                    format!("no song the statistics cover has the {name} reading"),
                ));
            };
            if !has_spread(stats.sd) {
                return Err(unrankable(
                    recipe_id,
                    format!("the library's {name} has no spread to score against"),
                ));
            }
            terms.push(format!(
                "{weight} * ({column} - {}) / {}",
                lit(stats.mean)?,
                lit(stats.sd)?
            ));
        }
        let total: f64 = definition.parts.iter().map(|p| p.weight.abs()).sum();
        scores.push(format!(
            "({}) / {} AS {}",
            terms.join(" + "),
            lit(total)?,
            score_column(&definition.mood)
        ));
    }
    let pace_scale = if uses_pace {
        "pace_scale AS MATERIALIZED (SELECT avg(dpace) AS mean, stddev_pop(dpace) AS sd FROM library),"
    } else {
        ""
    };
    let scores = if scores.is_empty() {
        String::new()
    } else {
        format!(", {}", scores.join(", "))
    };
    let pace_join = if uses_pace {
        "CROSS JOIN pace_scale ps"
    } else {
        ""
    };
    Ok(format!(
        "{pace_scale}
        defined AS (
            SELECT l.*{scores}{extra_columns}
            FROM library l {pace_join} {extra_joins}
        )"
    ))
}

/// The input columns the definitions read (upstream `definitionInputs`).
pub(crate) fn definition_inputs(definitions: &[&Definition]) -> Vec<String> {
    let mut columns: Vec<(String, String)> = Vec::new();
    for definition in definitions {
        for part in &definition.parts {
            let name = part_column(part);
            if columns.iter().any(|(n, _)| *n == name) {
                continue;
            }
            let sql = match &part.kind {
                PartKind::Pace => pace_sql(),
                PartKind::Reading(r) => reading_value_sql(definition_reading_index(*r)),
                PartKind::Mood(m) | PartKind::Head(m) => mood_sql(m),
            };
            columns.push((name, sql));
        }
    }
    columns
        .into_iter()
        .map(|(name, sql)| format!("{sql} AS {name}"))
        .collect()
}

/// Upstream `definedStatsCte`: `def_stats` over `defined`, each defined mood's library mean (`<mood>_mean`) and
/// population SD (`<mood>_sd`).
pub(crate) fn defined_stats_cte(definitions: &[&Definition]) -> String {
    let columns = definitions
        .iter()
        .flat_map(|d| {
            let s = score_column(&d.mood);
            [
                format!("avg({s}) AS {}_mean", d.mood),
                format!("stddev_pop({s}) AS {}_sd", d.mood),
            ]
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("def_stats AS MATERIALIZED (SELECT {columns} FROM defined)")
}

/// Upstream `definedZSql`: the standardised defined score of `mood` for a row of `defined` under alias `row`, against
/// `def_stats` (alias ds); NULL if none.
pub(crate) fn defined_z_sql(mood: &str, row: &str) -> String {
    let column = score_column(mood);
    format!("(({row}.{column} - ds.{mood}_mean) / NULLIF(ds.{mood}_sd, 0))")
}

/// Upstream `recipeRankingSql`: the recipe's score for every song it ranks, as CTEs of a WITH clause: `library`,
/// `defined` (plus the familiarity boost), `def_stats`, `key_scale`, `eligible`, `spread`, `ranked` (`recipe_z`) and
/// `placed` (arousal percentile, `in_filters`). `$1` is the heads' version; with an active boost `$2`/`$3` are its
/// ids and boosts. A component missing from `state` or without spread is [`RecipeError::Unrankable`].
fn ranking_sql(
    recipe: &Recipe,
    state: &MoodState,
    boost_active: bool,
) -> Result<String, RecipeError> {
    let components = &recipe.components;
    let filters = &recipe.filters;
    let mut definitions: Vec<&Definition> = Vec::new();
    for c in components {
        if let Signal::Mood(m) = &c.signal
            && let Some(d) = definition_of(m)
            && !definitions.iter().any(|x| x.mood == d.mood)
        {
            definitions.push(d);
        }
    }
    let mut scales: Vec<Option<MoodStats>> = Vec::with_capacity(components.len());
    for c in components {
        if c.signal == Signal::Minor || is_defined(&c.signal) {
            scales.push(None);
            continue;
        }
        scales.push(Some(scale_of(&recipe.id, state, &c.signal)?));
    }

    let (fam_column, fam_join) = if boost_active {
        (
            ", COALESCE(fb.boost, 0::float8) AS fam",
            "LEFT JOIN (SELECT * FROM unnest($2::uuid[], $3::float8[]) AS u(id, boost)) fb ON fb.id = l.id",
        )
    } else {
        ("", "")
    };
    let defined = defined_ctes(&recipe.id, state, &definitions, fam_column, fam_join)?;

    let mut values: Vec<String> = components
        .iter()
        .enumerate()
        .filter(|(_, c)| !is_defined(&c.signal))
        .map(|(i, c)| {
            let sql = match &c.signal {
                Signal::Mood(m) => mood_sql(m),
                Signal::Minor => MINOR_SQL.to_string(),
                other => reading_value_sql(reading_index(other).expect("a reading")),
            };
            format!("{sql} AS c{i}")
        })
        .collect();
    values.extend(definition_inputs(&definitions));
    let values = if values.is_empty() {
        "NULL::float8 AS unused".to_string()
    } else {
        values.join(", ")
    };

    let terms = components
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let weight = lit(c.weight)?;
            Ok(match (&c.signal, &scales[i]) {
                (Signal::Minor, _) => format!("{weight} * (l.c{i} - k.mean) / NULLIF(k.sd, 0)"),
                (Signal::Mood(m), _) if is_defined(&c.signal) => {
                    format!("{weight} * {}", defined_z_sql(m, "l"))
                }
                (_, Some(stats)) => format!(
                    "{weight} * (l.c{i} - {}) / {}",
                    lit(stats.mean)?,
                    lit(stats.sd)?
                ),
                (_, None) => unreachable!("every plain component has a scale"),
            })
        })
        .collect::<Result<Vec<_>, RecipeError>>()?
        .join(" + ");
    let total_weight: f64 = components.iter().map(|c| c.weight.abs()).sum();

    let mut present: Vec<String> = Vec::new();
    for (i, c) in components.iter().enumerate() {
        match &c.signal {
            Signal::Mood(m) if is_defined(&c.signal) => {
                present.push(format!("l.{} IS NOT NULL", score_column(m)));
            }
            Signal::Mood(_) => {}
            _ => present.push(format!("l.c{i} IS NOT NULL")),
        }
    }
    if filters.bpm.is_some() {
        present.push("l.bpm IS NOT NULL".into());
        present.push("l.bpm_confidence IS NOT NULL".into());
    }
    if filters.instrumental_max.is_some() || filters.instrumental_min.is_some() {
        present.push("l.instrumentalness IS NOT NULL".into());
    }
    if filters.arousal_pct.is_some() {
        present.push("l.arousal IS NOT NULL".into());
    }
    if filters.minor {
        present.push("l.minor IS NOT NULL".into());
    }
    let present = if present.is_empty() {
        "true".to_string()
    } else {
        present.join(" AND ")
    };

    // The arousal percentile is taken over every eligible song, before any filter.
    let percentile = if filters.arousal_pct.is_some() {
        "percent_rank() OVER (ORDER BY r.arousal)"
    } else {
        "NULL::float8"
    };
    let mut passes: Vec<String> = Vec::new();
    if let Some((lo, hi)) = filters.bpm {
        let (lo, hi) = (lit(lo)?, lit(hi)?);
        passes.push(format!(
            "p.bpm_confidence >= {} AND (p.bpm BETWEEN {lo} AND {hi} OR p.bpm * 2 BETWEEN {lo} AND {hi} \
             OR p.bpm / 2 BETWEEN {lo} AND {hi})",
            lit(STEADY_BEAT_KIMA)?
        ));
    }
    if let Some(max) = filters.instrumental_max {
        passes.push(format!("p.instrumentalness <= {}", lit(max)?));
    }
    if let Some(min) = filters.instrumental_min {
        passes.push(format!("p.instrumentalness >= {}", lit(min)?));
    }
    if let Some((lo, hi)) = filters.arousal_pct {
        passes.push(format!(
            "p.arousal_pct BETWEEN {} AND {}",
            lit(lo)?,
            lit(hi)?
        ));
    }
    if filters.minor {
        passes.push("p.minor = 1".into());
    }
    let passes = if passes.is_empty() {
        "true".to_string()
    } else {
        passes.join(" AND ")
    };

    let def_stats = if definitions.is_empty() {
        String::new()
    } else {
        format!("{},", defined_stats_cte(&definitions))
    };
    let def_join = if definitions.is_empty() {
        ""
    } else {
        "CROSS JOIN def_stats ds"
    };
    let fam = match (boost_active, recipe.familiarity) {
        (true, Some(share)) => format!("+ {} * e.fam", lit(share)?),
        _ => String::new(),
    };
    Ok(format!(
        "library AS (
            SELECT t.id, lower(t.title) AS title_key, {artist} AS artist_id,
                   ta.tempo_bpm AS bpm, ta.tempo_confidence AS bpm_confidence,
                   r.arousal_raw::float8 AS arousal, r.instrumentalness::float8 AS instrumentalness,
                   {MINOR_SQL} AS minor, {values}
            {scored}
        ),
        {defined},
        {def_stats}
        key_scale AS MATERIALIZED (SELECT avg(minor) AS mean, stddev_pop(minor) AS sd FROM defined),
        eligible AS (
            SELECT l.*, ({terms}) / {total} AS score
            FROM defined l CROSS JOIN key_scale k {def_join}
            WHERE {present}
        ),
        spread AS MATERIALIZED (
            SELECT count(*)::int AS eligible, avg(score) AS mean, stddev_pop(score) AS sd FROM eligible
        ),
        ranked AS (
            SELECT e.*, (e.score - s.mean {fam}) / NULLIF(s.sd, 0) AS recipe_z
            FROM eligible e CROSS JOIN spread s
        ),
        placed AS (
            SELECT p.*, {passes} AS in_filters
            FROM (SELECT r.*, {percentile} AS arousal_pct FROM ranked r) p
        )",
        artist = artist_sql(),
        scored = scored_songs(),
        total = lit(total_weight)?,
    ))
}

/// Upstream `MEMBERS_SQL` after the ranking: the songs at the line (at least [`RECIPE_LINE`] above the average, through
/// the filters, in a ranking of at least [`MIN_MIX_TRACKS`] songs), one per title (any case) and artist, the copy
/// furthest above the line (ties: the lower id).
fn members_sql() -> String {
    format!(
        "members AS (
            SELECT DISTINCT ON (r.title_key, r.artist_id) r.id, r.artist_id
            FROM placed r
            WHERE r.recipe_z >= {line} AND r.in_filters AND (SELECT eligible FROM spread) >= {min}
            ORDER BY r.title_key, r.artist_id, r.recipe_z DESC, r.id
        )",
        line = const_lit(RECIPE_LINE),
        min = MIN_MIX_TRACKS,
    )
}

struct Summary {
    eligible: i32,
    score_sd: Option<f64>,
    minor_sd: Option<f64>,
}

/// False when the recipe ranks fewer than [`MIN_MIX_TRACKS`] songs; an error when the ranking cannot set any apart.
fn ranks_enough(recipe: &Recipe, summary: &Summary) -> Result<bool, RecipeError> {
    if summary.eligible < MIN_MIX_TRACKS as i32 {
        let defined: Vec<&str> = recipe
            .components
            .iter()
            .filter(|c| is_defined(&c.signal))
            .map(|c| c.signal.name())
            .collect();
        if !defined.is_empty() {
            tracing::debug!(
                "[Mixes] {}: {} eligible songs; a defined mood ({}) may have no readings to score",
                recipe.id,
                summary.eligible,
                defined.join(", ")
            );
        }
        return Ok(false);
    }
    if recipe.components.iter().any(|c| c.signal == Signal::Minor)
        && !summary.minor_sd.is_some_and(has_spread)
    {
        return Err(unrankable(
            &recipe.id,
            "the library's key scale has no spread to score against",
        ));
    }
    if !summary.score_sd.is_some_and(has_spread) {
        return Err(unrankable(
            &recipe.id,
            "every eligible song has the same score, so none stands out",
        ));
    }
    Ok(true)
}

/// The statistics a ranking stands on, or `None` (no heads, no statistics for them, or fewer covered songs than a mix
/// holds).
async fn state_for<'a>(
    pool: &PgPool,
    ctx: RecipeContext<'a>,
) -> sqlx::Result<Option<(MoodState, &'a MoodHeads)>> {
    let Some(heads) = ctx.heads else {
        return Ok(None);
    };
    let mut conn = pool.acquire().await?;
    let Some(state) = load_mood_state(&mut conn, heads).await? else {
        return Ok(None);
    };
    if (state.track_count as usize) < MIN_MIX_TRACKS {
        return Ok(None);
    }
    Ok(Some((state, heads)))
}

/// The boost to apply: only for a recipe with a familiarity share, and only when it boosts some song.
fn active_boost<'a>(recipe: &Recipe, ctx: RecipeContext<'a>) -> Option<&'a FamiliarBoost> {
    ctx.boost
        .filter(|b| recipe.familiarity.is_some() && !b.ids.is_empty())
}

fn bind_query<'q>(
    sql: &'q str,
    heads: &'q MoodHeads,
    boost: Option<&'q FamiliarBoost>,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    let q = sqlx::query(sql).bind(&heads.version);
    match boost {
        Some(b) => q.bind(&b.ids).bind(&b.boosts),
        None => q,
    }
}

fn summary_of(row: &sqlx::postgres::PgRow) -> sqlx::Result<Summary> {
    Ok(Summary {
        eligible: row.try_get("eligible")?,
        score_sd: row.try_get("score_sd")?,
        minor_sd: row.try_get("minor_sd")?,
    })
}

/// Upstream `recipeMembers`: the recipe's members through its filters, one per title (any case) and artist. `None`
/// when the library has no mood statistics yet (or no heads) or fewer than [`MIN_MIX_TRACKS`] songs it could score.
/// A recipe that carries a familiarity share lifts the songs the boost names.
pub async fn recipe_members(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    recipe: &Recipe,
) -> Result<Option<Vec<Member>>, RecipeError> {
    let Some((state, heads)) = state_for(pool, ctx).await? else {
        return Ok(None);
    };
    let boost = active_boost(recipe, ctx);
    let sql = format!(
        "WITH {}, {}
         SELECT s.eligible, s.sd AS score_sd, k.sd AS minor_sd, mb.id, mb.artist_id
         FROM spread s CROSS JOIN key_scale k LEFT JOIN members mb ON true
         ORDER BY mb.id",
        ranking_sql(recipe, &state, boost.is_some())?,
        members_sql()
    );
    let rows = bind_query(&sql, heads, boost).fetch_all(pool).await?;
    let first = rows.first().expect("one summary row");
    if !ranks_enough(recipe, &summary_of(first)?)? {
        return Ok(None);
    }
    let mut members = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: Option<Uuid> = row.try_get("id")?;
        let artist_id: Option<String> = row.try_get("artist_id")?;
        if let (Some(id), Some(artist_id)) = (id, artist_id) {
            members.push(Member { id, artist_id });
        }
    }
    Ok(Some(members))
}

/// Upstream `recipeMemberCount`: how many members [`recipe_members`] would return, counted in the database; `None`
/// where it returns `None`.
pub async fn recipe_member_count(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    recipe: &Recipe,
) -> Result<Option<usize>, RecipeError> {
    let Some((state, heads)) = state_for(pool, ctx).await? else {
        return Ok(None);
    };
    let boost = active_boost(recipe, ctx);
    let sql = format!(
        "WITH {}, {}
         SELECT s.eligible, s.sd AS score_sd, k.sd AS minor_sd, (SELECT count(*)::int FROM members) AS members
         FROM spread s CROSS JOIN key_scale k",
        ranking_sql(recipe, &state, boost.is_some())?,
        members_sql()
    );
    let row = bind_query(&sql, heads, boost).fetch_one(pool).await?;
    if !ranks_enough(recipe, &summary_of(&row)?)? {
        return Ok(None);
    }
    let members: i32 = row.try_get("members")?;
    Ok(Some(members as usize))
}

/// Every song the recipe ranks with its `recipe_z` (upstream's tests read the `ranked` CTE): `None` where
/// [`recipe_members`] has no ranking. For checks of the familiarity boost and of the line.
pub async fn ranked_scores(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    recipe: &Recipe,
) -> Result<Option<Vec<(Uuid, f64)>>, RecipeError> {
    let Some((state, heads)) = state_for(pool, ctx).await? else {
        return Ok(None);
    };
    let boost = active_boost(recipe, ctx);
    let sql = format!(
        "WITH {} SELECT id, recipe_z FROM ranked WHERE recipe_z IS NOT NULL ORDER BY id",
        ranking_sql(recipe, &state, boost.is_some())?
    );
    let rows = bind_query(&sql, heads, boost).fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((row.try_get("id")?, row.try_get("recipe_z")?));
    }
    Ok(Some(out))
}

/// Upstream `drawMembers`: up to `limit` members in the order of their seeded key, at most 2 per artist (relaxed as
/// the cap relaxes).
pub fn draw(members: &[Member], seed: &str, limit: usize) -> Vec<Uuid> {
    fn id_of(k: &(String, String)) -> &str {
        &k.0
    }
    fn artist_of(k: &(String, String)) -> &str {
        &k.1
    }
    let keyed: Vec<(String, String)> = members
        .iter()
        .map(|m| (m.id.to_string(), m.artist_id.clone()))
        .collect();
    draw_members(&keyed, id_of, artist_of, seed, limit)
        .iter()
        .map(|id| Uuid::parse_str(id).expect("drawn ids are member ids"))
        .collect()
}

/// Upstream `pickMixTracks`: up to `limit` drawn evenly from the recipe's members, the same draw for one recipe, day
/// and listener (seed `JSON.stringify([recipe id, date, listener])`). `None` when fewer than [`MIN_MIX_TRACKS`] songs
/// are members; with fewer than `limit` the mix ships short.
pub async fn pick_mix_tracks(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    recipe: &Recipe,
    draw_for: MixDraw<'_>,
) -> Result<Option<RecipeMix>, RecipeError> {
    if draw_for.listener.is_empty() {
        return Err(RecipeError::Invalid(format!(
            "Recipe {}: a mix is drawn for a user",
            recipe.id
        )));
    }
    if draw_for.date.is_empty() {
        return Err(RecipeError::Invalid(format!(
            "Recipe {}: a mix is drawn for a day",
            recipe.id
        )));
    }
    if draw_for.limit < MIN_MIX_TRACKS {
        return Err(RecipeError::Invalid(format!(
            "A mix holds at least {MIN_MIX_TRACKS} songs; limit {} cannot make one",
            draw_for.limit
        )));
    }
    let Some(members) = recipe_members(pool, ctx, recipe).await? else {
        return Ok(None);
    };
    if members.len() < MIN_MIX_TRACKS {
        return Ok(None);
    }
    let seed = serde_json::to_string(&[recipe.id.as_str(), draw_for.date, draw_for.listener])
        .expect("strings serialise");
    Ok(Some(RecipeMix {
        track_ids: draw(&members, &seed, draw_for.limit),
        members: members.len(),
    }))
}

/// Upstream `pickRecipeTracks`: [`pick_mix_tracks`] for the shipped recipe with this id; an unknown id is refused.
pub async fn pick_recipe_tracks(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    recipe_id: &str,
    draw_for: MixDraw<'_>,
) -> Result<Option<RecipeMix>, RecipeError> {
    let recipe = get_recipe(recipe_id).map_err(RecipeError::Invalid)?;
    pick_mix_tracks(pool, ctx, recipe, draw_for).await
}

/// Upstream `pickPageTracks`: a mood page's mix, up to `limit` drawn evenly from the recipe's members in the order of
/// `seed` (upstream draws a fresh `randomUUID` per call; the caller passes one), skipping `exclude` (the songs this
/// listener was already served) while at least [`MIN_MIX_TRACKS`] others remain. `None` when fewer than
/// [`MIN_MIX_TRACKS`] songs are members.
pub async fn pick_page_tracks(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    recipe: &Recipe,
    limit: usize,
    exclude: &[Uuid],
    seed: &str,
) -> Result<Option<Vec<Uuid>>, RecipeError> {
    if limit < 1 {
        return Err(RecipeError::Invalid(format!(
            "limit must be a positive integer, got {limit}"
        )));
    }
    let Some(members) = recipe_members(pool, ctx, recipe).await? else {
        return Ok(None);
    };
    if members.len() < MIN_MIX_TRACKS {
        return Ok(None);
    }
    let fresh: Vec<Member> = members
        .iter()
        .filter(|m| !exclude.contains(&m.id))
        .cloned()
        .collect();
    let pool_of = if fresh.len() >= MIN_MIX_TRACKS {
        &fresh
    } else {
        &members
    };
    Ok(Some(draw(pool_of, seed, limit)))
}

/// Upstream `pageMemberCount`: how many songs a mood page draws from, or 0 when the page has no mix.
pub async fn page_member_count(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    recipe: &Recipe,
) -> Result<usize, RecipeError> {
    Ok(match recipe_member_count(pool, ctx, recipe).await? {
        Some(n) if n >= MIN_MIX_TRACKS => n,
        _ => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_that_is_not_finite_is_refused_not_written() {
        assert_eq!(lit(1.5).unwrap(), "(1.5)::float8");
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                matches!(lit(x), Err(RecipeError::Invalid(_))),
                "{x} was written into the SQL"
            );
        }
    }
}
