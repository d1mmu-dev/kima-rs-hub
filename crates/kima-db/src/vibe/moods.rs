//! Learned moods in the database (spec 8.2; port of upstream `moods/state.ts`, `moods/inputsHash.ts` and the
//! snapshot half of `moodBucketService.ts`).
//!
//! Per eligible vector, [`update_moods`] stores the 22 scores (one dot product per head, in SQL); per library,
//! [`MoodBuilder::build`] stores each mood's and each reading's mean and population SD in `mood_state`. Both run in
//! one `REPEATABLE READ` transaction that locks the `mood_state` row first (the engine's lock order), so a mix never
//! reads scores of one set of weights with statistics of another and a concurrent re-run shows up as a write
//! conflict, which is swallowed. [`MoodSnapshots`] holds the dominant-mood decision of every track and rebuilds it
//! only when the statistics' version or fingerprint moved.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use kima_vibe::mood_heads::{MOODS, MoodHeads};
use kima_vibe::moods::{CLIENT_MOODS, MoodDistribution, MoodStats, MoodValues, MoodView, classify};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use super::ELIGIBLE;
use super::state::is_write_conflict;

/// Raise when the tracks the statistics cover or a reading's transform changes, so every library rebuilds them.
const STATE_FORMULA_VERSION: u32 = 2;

/// The readings the mixes blend with the moods; each is stored as a logit of the value clipped to 0.01..0.99, so a
/// stored 0 or 1 stays finite. Names are columns of `mood_state` (`<name>_mean`, `<name>_sd`).
pub const READINGS: [&str; 3] = ["arousal", "instrumental_logit", "acoustic_logit"];

/// SQL for the logit of a `track_clap_readings` column under alias `r`; NULL when the track has no reading row.
/// `GREATEST` and `LEAST` skip a NULL argument, so absence is kept explicitly rather than read as 0.01.
fn logit(column: &str) -> String {
    let p = format!("LEAST(0.99, GREATEST(0.01, r.{column}))");
    format!("CASE WHEN r.{column} IS NOT NULL THEN ln({p} / (1 - {p})) END")
}

/// `track_clap_readings` column behind each of [`READINGS`].
const READING_COLUMNS: [&str; 3] = ["arousal_raw", "instrumentalness", "acousticness"];

/// `"2/<heads version>"`: the re-run key of the `mood_state` row.
pub fn mood_state_version(heads: &MoodHeads) -> String {
    format!("{STATE_FORMULA_VERSION}/{}", heads.version)
}

/// The longest a change signal may stand in for the full hash. The signal watches what the vector and score steps
/// write, so a write that touches none of it (a hand edit of a reading) would otherwise go unseen for good.
pub const HASH_AT_LEAST_EVERY: Duration = Duration::from_secs(60 * 60);

fn quoted_moods() -> String {
    MOODS
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Scores for every eligible vector that has none, an older vector's, or scores of other weights: one statement,
/// 22 dot products. The heads were fitted on unit vectors and the writer lets a stored vector's length differ from
/// 1 slightly, hence the division by the norm. Returns the rows written.
///
/// The heads' own `model` label is not compared: which vectors the heads may score is decided by [`ELIGIBLE`]
/// alone (D-182), and the heads asset is bound to that checkpoint by its pinned hash.
pub async fn update_moods(conn: &mut PgConnection, heads: &MoodHeads) -> sqlx::Result<u64> {
    let eligible: &str = &ELIGIBLE;
    // $1 = weights version; then (weights, bias) per mood.
    let scores = (0..MOODS.len())
        .map(|k| {
            format!(
                "-(te.embedding <#> ${}::real[]::vector) / vector_norm(te.embedding) + ${}::float8",
                2 + 2 * k,
                3 + 2 * k
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let updates = MOODS
        .iter()
        .map(|m| format!("\"{m}\" = EXCLUDED.\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO track_moods (track_id, embedded_at, weights, {cols})
         SELECT te.track_id, te.created_at, $1, {scores}
         FROM {eligible}
           AND NOT EXISTS (SELECT 1 FROM track_moods m
                           WHERE m.track_id = te.track_id AND m.embedded_at = te.created_at AND m.weights = $1)
         ON CONFLICT (track_id) DO UPDATE
         SET embedded_at = EXCLUDED.embedded_at, weights = EXCLUDED.weights, {updates}",
        cols = quoted_moods()
    );
    let mut q = sqlx::query(&sql).bind(&heads.version);
    for head in &heads.heads {
        q = q
            .bind(head.weights.iter().map(|w| *w as f32).collect::<Vec<f32>>())
            .bind(head.bias);
    }
    Ok(q.execute(conn).await?.rows_affected())
}

/// Eligible tracks (a vector of the engine's checkpoint, file present) and how many have scores of these weights
/// for the vector they have now.
pub async fn mood_progress(pool: &PgPool, heads: &MoodHeads) -> sqlx::Result<(i64, i64)> {
    let eligible: &str = &ELIGIBLE;
    sqlx::query_as(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at FROM {eligible})
         SELECT count(*)::bigint,
                (count(*) FILTER (WHERE EXISTS (SELECT 1 FROM track_moods m
                      WHERE m.track_id = e.track_id AND m.embedded_at = e.created_at AND m.weights = $1)))::bigint
         FROM e"
    ))
    .bind(&heads.version)
    .fetch_one(pool)
    .await
}

/// What one [`MoodBuilder::build`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MoodBuild {
    /// Score rows written.
    pub scored: u64,
    /// The `mood_state` row was written or removed.
    pub state_changed: bool,
}

#[derive(Debug, Clone)]
struct LastHash {
    signal: String,
    fingerprint: String,
    at: Instant,
}

/// The mood step of one process: the heads, and what this process last learned about the statistics' inputs.
pub struct MoodBuilder {
    heads: Arc<MoodHeads>,
    /// The change signal when this process last hashed, and the fingerprint that hash gave. While the stored row
    /// still holds that fingerprint and the signal has not moved, the inputs are the ones already hashed and the
    /// full hash is skipped. A process that has not hashed yet has nothing here and hashes.
    last_hash: Mutex<Option<LastHash>>,
    hash_at_least_every: Duration,
    full_hashes: AtomicU64,
}

impl MoodBuilder {
    pub fn new(heads: Arc<MoodHeads>) -> Self {
        Self {
            heads,
            last_hash: Mutex::new(None),
            hash_at_least_every: HASH_AT_LEAST_EVERY,
            full_hashes: AtomicU64::new(0),
        }
    }

    /// Another bound on how long the change signal may stand in for the full hash (tests).
    pub fn with_hash_at_least_every(mut self, every: Duration) -> Self {
        self.hash_at_least_every = every;
        self
    }

    pub fn heads(&self) -> &Arc<MoodHeads> {
        &self.heads
    }

    /// How many times this builder ran the full inputs hash.
    pub fn full_hash_runs(&self) -> u64 {
        self.full_hashes.load(Ordering::Relaxed)
    }

    /// One run of the step: scores for new or changed vectors, then the statistics when their inputs changed, in one
    /// transaction. A write conflict with a concurrent re-run writes nothing (the next pass builds from the new data).
    pub async fn build(&self, pool: &PgPool) -> sqlx::Result<MoodBuild> {
        match self.try_build(pool).await {
            Err(e) if is_write_conflict(&e) => {
                tracing::info!(
                    "mood scores changed while the build waited; the next pass rebuilds"
                );
                Ok(MoodBuild::default())
            }
            other => other,
        }
    }

    async fn try_build(&self, pool: &PgPool) -> sqlx::Result<MoodBuild> {
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout = '300s'")
            .execute(&mut *tx)
            .await?;
        // The state row first, as a re-run takes it, before the score rows: one order, so neither deadlocks.
        sqlx::query("SELECT id FROM mood_state WHERE id = 'default' FOR UPDATE")
            .execute(&mut *tx)
            .await?;
        let scored = update_moods(&mut tx, &self.heads).await?;
        let state_changed = self.build_state(&mut tx, scored).await?;
        tx.commit().await?;
        Ok(MoodBuild {
            scored,
            state_changed,
        })
    }

    /// The statistics, rebuilt when their inputs changed; true when the stored row was written or removed. `scored`
    /// is how many scores the same transaction just wrote: the signal is only trusted when it is 0.
    async fn build_state(&self, conn: &mut PgConnection, scored: u64) -> sqlx::Result<bool> {
        let version = mood_state_version(&self.heads);
        let current: Option<(String, String)> =
            sqlx::query_as("SELECT version, fingerprint FROM mood_state WHERE id = 'default'")
                .fetch_optional(&mut *conn)
                .await?;
        let signal = change_signal(conn).await?;
        if scored == 0
            && let Some((v, f)) = &current
            && *v == version
            && self.last_hash.lock().unwrap().as_ref().is_some_and(|l| {
                l.fingerprint == *f
                    && l.signal == signal
                    && l.at.elapsed() < self.hash_at_least_every
            })
        {
            return Ok(false);
        }
        let (n, hash) = hash_inputs(conn, &self.heads).await?;
        self.full_hashes.fetch_add(1, Ordering::Relaxed);
        let fingerprint = hex::encode(Sha256::digest(
            serde_json::json!([version, n, hash]).to_string().as_bytes(),
        ));
        *self.last_hash.lock().unwrap() = Some(LastHash {
            signal,
            fingerprint: fingerprint.clone(),
            at: Instant::now(),
        });
        if current.is_some_and(|(v, f)| v == version && f == fingerprint) {
            return Ok(false);
        }
        let Some(state) = compute_state(conn, &self.heads).await? else {
            let gone = sqlx::query("DELETE FROM mood_state")
                .execute(&mut *conn)
                .await?;
            return Ok(gone.rows_affected() > 0);
        };
        write_state(conn, &version, &fingerprint, &state).await?;
        Ok(true)
    }
}

/// The count of covered tracks and the hash of everything about them the statistics read. Covered: eligible, with
/// scores of these weights for the vector they have now.
async fn hash_inputs(conn: &mut PgConnection, heads: &MoodHeads) -> sqlx::Result<(i64, String)> {
    let eligible: &str = &ELIGIBLE;
    sqlx::query_as(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at FROM {eligible})
         SELECT count(*)::bigint,
                coalesce(sum(hashtextextended(
                    m.track_id::text || ':' || extract(epoch FROM m.embedded_at)::text || ':' ||
                    coalesce(r.arousal_raw::text, '') || ':' || coalesce(r.instrumentalness::text, '') || ':' ||
                    coalesce(r.acousticness::text, ''), 0)::numeric), 0)::text
         FROM e
         JOIN track_moods m ON m.track_id = e.track_id AND m.embedded_at = e.created_at AND m.weights = $1
         LEFT JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at"
    ))
    .bind(&heads.version)
    .fetch_one(conn)
    .await
}

/// Moves when a track is added, removed, rewritten or goes missing, when an eligible vector is written, or when
/// scores or readings are written. Each part reads one table; none joins.
async fn change_signal(conn: &mut PgConnection) -> sqlx::Result<String> {
    let eligible: &str = &ELIGIBLE;
    sqlx::query_scalar(&format!(
        "SELECT concat_ws('|',
            (SELECT concat_ws(',', count(*), count(*) FILTER (WHERE missing_since IS NULL), max(updated_at)) FROM tracks),
            (SELECT concat_ws(',', count(*), max(te.created_at)) FROM {eligible}),
            (SELECT concat_ws(',', count(*), max(embedded_at)) FROM track_moods),
            (SELECT concat_ws(',', count(*), max(embedded_at)) FROM track_clap_readings))"
    ))
    .fetch_one(conn)
    .await
}

/// Mean and population SD of every mood and reading over the covered tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct MoodState {
    pub track_count: i32,
    /// In [`MOODS`] order.
    pub moods: Vec<MoodStats>,
    /// In [`READINGS`] order; `None` for a reading no covered track has.
    pub readings: Vec<Option<MoodStats>>,
}

fn state_columns() -> Vec<String> {
    MOODS
        .iter()
        .chain(READINGS.iter())
        .flat_map(|n| [format!("{n}_mean"), format!("{n}_sd")])
        .collect()
}

/// Each mood's and reading's mean and population SD over the covered tracks, or `None` when none is covered. A
/// reading a track lacks leaves that track out of that reading's statistics only; a reading no covered track has is
/// stored as absent (NULL), so the moods keep their statistics and only the recipes that blend that reading refuse.
/// `ORDER BY track_id` makes each aggregate order-stable.
async fn compute_state(
    conn: &mut PgConnection,
    heads: &MoodHeads,
) -> sqlx::Result<Option<MoodState>> {
    let eligible: &str = &ELIGIBLE;
    let values: Vec<(String, String)> = MOODS
        .iter()
        .map(|m| ((*m).to_owned(), format!("m.\"{m}\"::float8")))
        .chain(
            READINGS
                .iter()
                .zip(READING_COLUMNS)
                .map(|(n, c)| ((*n).to_owned(), logit(c))),
        )
        .collect();
    let aggregates = values
        .iter()
        .map(|(name, v)| {
            format!(
                "avg({v} ORDER BY m.track_id) AS \"{name}_mean\", stddev_pop({v} ORDER BY m.track_id) AS \"{name}_sd\""
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let row = sqlx::query(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at FROM {eligible})
         SELECT count(*)::int AS n, {aggregates}
         FROM e
         JOIN track_moods m ON m.track_id = e.track_id AND m.embedded_at = e.created_at AND m.weights = $1
         LEFT JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at"
    ))
    .bind(&heads.version)
    .fetch_one(conn)
    .await?;
    let n: i32 = row.try_get("n")?;
    if n == 0 {
        return Ok(None);
    }
    let finite = |column: String| -> sqlx::Result<f64> {
        match row.try_get::<Option<f64>, _>(column.as_str())? {
            Some(v) if v.is_finite() => Ok(v),
            _ => Err(sqlx::Error::Protocol(format!(
                "[Moods] {column} has no value over {n} eligible tracks"
            ))),
        }
    };
    let mut moods = Vec::with_capacity(MOODS.len());
    for m in MOODS {
        moods.push(MoodStats {
            mean: finite(format!("{m}_mean"))?,
            sd: finite(format!("{m}_sd"))?,
        });
    }
    let mut readings = Vec::with_capacity(READINGS.len());
    for r in READINGS {
        if row
            .try_get::<Option<f64>, _>(format!("{r}_mean").as_str())?
            .is_none()
        {
            tracing::warn!(
                "no eligible track has the {r} reading; recipes that blend it rank no songs"
            );
            readings.push(None);
        } else {
            readings.push(Some(MoodStats {
                mean: finite(format!("{r}_mean"))?,
                sd: finite(format!("{r}_sd"))?,
            }));
        }
    }
    Ok(Some(MoodState {
        track_count: n,
        moods,
        readings,
    }))
}

async fn write_state(
    conn: &mut PgConnection,
    version: &str,
    fingerprint: &str,
    state: &MoodState,
) -> sqlx::Result<()> {
    let cols = state_columns();
    let names = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let params = (0..cols.len())
        .map(|i| format!("${}", 4 + i))
        .collect::<Vec<_>>()
        .join(", ");
    let updates = cols
        .iter()
        .map(|c| format!("\"{c}\" = EXCLUDED.\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO mood_state (id, version, fingerprint, track_count, {names}) VALUES ('default', $1, $2, $3, {params})
         ON CONFLICT (id) DO UPDATE SET version = EXCLUDED.version, fingerprint = EXCLUDED.fingerprint,
                track_count = EXCLUDED.track_count, {updates}, computed_at = now()"
    );
    let mut q = sqlx::query(&sql)
        .bind(version)
        .bind(fingerprint)
        .bind(state.track_count);
    for s in &state.moods {
        q = q.bind(s.mean).bind(s.sd);
    }
    for s in &state.readings {
        q = q.bind(s.map(|s| s.mean)).bind(s.map(|s| s.sd));
    }
    q.execute(conn).await?;
    Ok(())
}

/// The library's statistics, or `None` until the mood step has built them for these weights (a row of other
/// weights is not read back).
pub async fn load_mood_state(
    conn: &mut PgConnection,
    heads: &MoodHeads,
) -> sqlx::Result<Option<MoodState>> {
    let cols = state_columns()
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let Some(row) = sqlx::query(&format!(
        "SELECT track_count, {cols} FROM mood_state WHERE id = 'default' AND version = $1"
    ))
    .bind(mood_state_version(heads))
    .fetch_optional(conn)
    .await?
    else {
        return Ok(None);
    };
    let moods = MOODS
        .iter()
        .map(|m| {
            Ok(MoodStats {
                mean: row.try_get(format!("{m}_mean").as_str())?,
                sd: row.try_get(format!("{m}_sd").as_str())?,
            })
        })
        .collect::<sqlx::Result<Vec<_>>>()?;
    let readings = READINGS
        .iter()
        .map(|r| {
            let mean: Option<f64> = row.try_get(format!("{r}_mean").as_str())?;
            let sd: Option<f64> = row.try_get(format!("{r}_sd").as_str())?;
            Ok(mean.zip(sd).map(|(mean, sd)| MoodStats { mean, sd }))
        })
        .collect::<sqlx::Result<Vec<_>>>()?;
    Ok(Some(MoodState {
        track_count: row.try_get("track_count")?,
        moods,
        readings,
    }))
}

/// The signature while the library has no current statistics (a rescore or a weights change before the next mood
/// pass finishes): nothing is judged then, and the last snapshot stands.
pub const NO_STATISTICS: &str = "none";

/// Every track's mood decision at one statistics signature.
#[derive(Debug, Clone)]
pub struct MoodSnapshot {
    /// `<state version>:<state fingerprint>`; changes whenever a track's mood scores could. [`NO_STATISTICS`] only
    /// for a snapshot built before this process ever saw statistics; it has no views.
    pub signature: String,
    pub views: Arc<HashMap<Uuid, MoodView>>,
    /// Eligible tracks still waiting for scores of these weights.
    pub pending: i64,
}

/// The process-wide mood snapshot: rebuilt only when the statistics' signature changes, refreshed by the mood phase
/// and a 30 s interval task, readable without touching the database.
#[derive(Default)]
pub struct MoodSnapshots {
    current: RwLock<Option<Arc<MoodSnapshot>>>,
    /// One rebuild at a time: concurrent callers wait for it and then see the new signature.
    refreshing: tokio::sync::Mutex<()>,
}

impl MoodSnapshots {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last built snapshot, if any.
    pub fn peek(&self) -> Option<Arc<MoodSnapshot>> {
        self.current.read().unwrap().clone()
    }

    /// The last built snapshot, or a fresh one if none exists yet. For read paths that must not query the library
    /// on every request.
    pub async fn current(
        &self,
        pool: &PgPool,
        heads: &MoodHeads,
    ) -> sqlx::Result<Arc<MoodSnapshot>> {
        match self.peek() {
            Some(s) => Ok(s),
            None => Ok(self.refresh(pool, heads).await?.0),
        }
    }

    /// Re-reads the library only when its signature changed; `changed` says whether this call rebuilt it. While the
    /// library has no current statistics the last snapshot stands, unchanged.
    pub async fn refresh(
        &self,
        pool: &PgPool,
        heads: &MoodHeads,
    ) -> sqlx::Result<(Arc<MoodSnapshot>, bool)> {
        let _one_at_a_time = self.refreshing.lock().await;
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let state: Option<(String, String)> =
            sqlx::query_as("SELECT version, fingerprint FROM mood_state WHERE id = 'default'")
                .fetch_optional(&mut *tx)
                .await?;
        let signature = match state {
            Some((v, f)) if v == mood_state_version(heads) => format!("{v}:{f}"),
            _ => NO_STATISTICS.to_owned(),
        };
        let pending = pending_tracks(&mut tx, heads).await?;
        let prior = self.peek();
        if let Some(p) = &prior
            && (p.signature == signature || signature == NO_STATISTICS)
        {
            return Ok((self.with_pending(p, pending), false));
        }
        let built = if signature == NO_STATISTICS {
            MoodSnapshot {
                signature,
                views: Arc::default(),
                pending,
            }
        } else {
            MoodSnapshot {
                views: Arc::new(build_views(&mut tx, heads).await?),
                signature,
                pending,
            }
        };
        tx.commit().await?;
        let built = Arc::new(built);
        *self.current.write().unwrap() = Some(built.clone());
        Ok((built, true))
    }

    fn with_pending(&self, prior: &Arc<MoodSnapshot>, pending: i64) -> Arc<MoodSnapshot> {
        if prior.pending == pending {
            return prior.clone();
        }
        let next = Arc::new(MoodSnapshot {
            pending,
            ..(**prior).clone()
        });
        *self.current.write().unwrap() = Some(next.clone());
        next
    }
}

/// Eligible tracks without scores of these weights for the vector they have.
async fn pending_tracks(conn: &mut PgConnection, heads: &MoodHeads) -> sqlx::Result<i64> {
    let eligible: &str = &ELIGIBLE;
    sqlx::query_scalar(&format!(
        "SELECT count(*)::bigint FROM {eligible}
           AND NOT EXISTS (SELECT 1 FROM track_moods m
                           WHERE m.track_id = te.track_id AND m.embedded_at = te.created_at AND m.weights = $1)"
    ))
    .bind(&heads.version)
    .fetch_one(conn)
    .await
}

/// Every track the statistics cover, classified against the library.
async fn build_views(
    conn: &mut PgConnection,
    heads: &MoodHeads,
) -> sqlx::Result<HashMap<Uuid, MoodView>> {
    let Some(state) = load_mood_state(&mut *conn, heads).await? else {
        return Ok(HashMap::new());
    };
    let eligible: &str = &ELIGIBLE;
    let learned: Vec<usize> = CLIENT_MOODS
        .iter()
        .map(|c| {
            MOODS
                .iter()
                .position(|m| *m == c.learned)
                .expect("client moods are learned moods")
        })
        .collect();
    let cols = CLIENT_MOODS
        .iter()
        .map(|c| format!("m.\"{}\"::float8", c.learned))
        .collect::<Vec<_>>()
        .join(", ");
    let rows: Vec<(Uuid, f64, f64, f64, f64, f64)> = sqlx::query_as(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at FROM {eligible})
         SELECT e.track_id, {cols}
         FROM e JOIN track_moods m ON m.track_id = e.track_id AND m.embedded_at = e.created_at AND m.weights = $1"
    ))
    .bind(&heads.version)
    .fetch_all(conn)
    .await?;
    let stats: [MoodStats; 5] = std::array::from_fn(|i| state.moods[learned[i]]);
    let values: Vec<(Uuid, MoodValues)> = rows
        .into_iter()
        .map(|(id, a, b, c, d, e)| (id, [Some(a), Some(b), Some(c), Some(d), Some(e)]))
        .collect();
    let all: Vec<MoodValues> = values.iter().map(|(_, v)| *v).collect();
    let dist = MoodDistribution::new(&all, stats);
    Ok(values
        .into_iter()
        .map(|(id, v)| (id, classify(&v, &dist)))
        .collect())
}
