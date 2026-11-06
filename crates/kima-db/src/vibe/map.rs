//! The vibe map in the similarity step (spec 7; port of upstream `services/matching/map.ts`).
//!
//! The whole library is laid out once, when there is no layout for this formula yet; after that each new or
//! re-embedded track is placed among the laid-out ones and no track that has a place moves again. Positions and
//! colours are stored per track in `matching_map`; the page only draws them.
//!
//! Writes go through [`write_while_state_stands`]: the state row is locked first (the engine's lock order: state row,
//! then feel/score/map rows, then track rows) and a write whose state row is gone (a re-run deleted it) is dropped.
//! A re-run deletes the state row before the stored map, so a layout that overtook the re-run stores nothing and one
//! that reached the row first finishes and is then cleared by the re-run.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use kima_vibe::cancel::Cancel;
use kima_vibe::colour::ColourFrame;
use kima_vibe::feel_probe::FeelProbe;
use kima_vibe::formula::{self, MatchTrack};
use kima_vibe::map_compute::{
    LaidOutMap, PlacedTracks, layout_library_cancellable, place_tracks_cancellable,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::ELIGIBLE;
use super::rank::{MatchingState, load_matching_state};
use super::state::state_version;

/// Raise when the layout arithmetic changes, so every library is laid out again.
pub const LAYOUT_VERSION: u32 = 1;
/// Rows per `unnest` statement.
const WRITE_CHUNK: usize = 5000;
/// A map write may wait out a re-run that holds the state row: build 5 min < re-run 7 min < map write 8 min.
const MAP_WRITE_TIMEOUT: Duration = Duration::from_secs(8 * 60);

/// `<state version>/<layout version>`: a state rebuild for another probe or formula version lays the map out again.
pub fn map_version(probe: &FeelProbe) -> String {
    format!("{}/{LAYOUT_VERSION}", state_version(probe))
}

/// What one run did: tracks laid out (a first or new layout) and tracks placed around an existing layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapUpdate {
    pub laid_out: usize,
    pub placed: usize,
}

#[derive(Debug)]
pub enum MapError {
    Db(sqlx::Error),
    /// The service was stopped (shutdown) before the run finished; nothing was stored.
    Cancelled,
    /// The blocking pass or the run's task died.
    Failed(String),
    /// The stored state or frame is unusable.
    BadState(String),
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "{e}"),
            Self::Cancelled => f.write_str("the map run was cancelled"),
            Self::Failed(m) => write!(f, "the map run failed: {m}"),
            Self::BadState(m) => write!(f, "matching state is unusable: {m}"),
        }
    }
}

impl std::error::Error for MapError {}

impl From<sqlx::Error> for MapError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

pub type Hook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Test seams. `before_compute` runs after the vectors were read and before the layout or placement is computed,
/// so a test can run a re-run exactly where it overtakes the computation.
#[derive(Clone, Default)]
pub struct MapHooks {
    pub before_compute: Option<Hook>,
}

/// The frame stored in `matching_state.map_frame`: the colour field's turn and the highest pair score, which turns a
/// score into the distance placement uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredFrame {
    colour: ColourFrame,
    max_score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct TrackRow {
    id: Uuid,
    album_id: Uuid,
    artist_id: Uuid,
    embedding: Vec<f32>,
    valence: f32,
    arousal: f32,
    embedded_at: OffsetDateTime,
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    laid_out: Option<bool>,
}

type Flight = Shared<BoxFuture<'static, Result<MapUpdate, Arc<MapError>>>>;

/// The map part of the similarity step. Concurrent callers of [`MapService::update`] share one run.
pub struct MapService {
    pool: PgPool,
    probe: FeelProbe,
    flight: Mutex<Option<Flight>>,
    cancel: Cancel,
    hooks: MapHooks,
}

impl MapService {
    pub fn new(pool: PgPool, probe: FeelProbe) -> Arc<Self> {
        Self::with_hooks(pool, probe, MapHooks::default())
    }

    pub fn with_hooks(pool: PgPool, probe: FeelProbe, hooks: MapHooks) -> Arc<Self> {
        Arc::new(Self {
            pool,
            probe,
            flight: Mutex::new(None),
            cancel: Cancel::new(),
            hooks,
        })
    }

    /// Stops the service: a running sweep, start or layout notices at its next checkpoint and the run ends with
    /// [`MapError::Cancelled`] without storing anything; later calls fail the same way.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// One run of the map part of the similarity step; callers that arrive while it runs share it.
    /// `inputs_settled` says the library's artist genres, analysis and sound vectors are all settled: a library is
    /// laid out only then, since its layout is permanent and every later track is placed around it.
    pub async fn update(
        self: &Arc<Self>,
        inputs_settled: bool,
    ) -> Result<MapUpdate, Arc<MapError>> {
        let flight = {
            let mut slot = self.flight.lock().expect("flight slot");
            if let Some(running) = slot.as_ref() {
                running.clone()
            } else {
                let me = self.clone();
                // The run is its own task: it goes on (and ends, and clears the slot) even if every caller leaves.
                let task = tokio::spawn(async move {
                    let outcome = me.run(inputs_settled).await.map_err(Arc::new);
                    *me.flight.lock().expect("flight slot") = None;
                    outcome
                });
                let shared: Flight = async move {
                    task.await
                        .unwrap_or_else(|e| Err(Arc::new(MapError::Failed(e.to_string()))))
                }
                .boxed()
                .shared();
                *slot = Some(shared.clone());
                shared
            }
        };
        flight.await
    }

    async fn run(&self, inputs_settled: bool) -> Result<MapUpdate, MapError> {
        if self.cancel.is_cancelled() {
            return Err(MapError::Cancelled);
        }
        let none = MapUpdate {
            laid_out: 0,
            placed: 0,
        };
        let Some(state) = load_matching_state(&self.pool, &self.probe).await? else {
            return Ok(none);
        };
        // This runs every enrichment cycle: count first, and load the vectors only when there is a track to place or
        // a layout that can be made now. The first layout waits for the library to settle.
        let version = map_version(&self.probe);
        let status = map_status(&self.pool, &self.probe, &version).await?;
        if status.complete || (!status.laid_out && !inputs_settled) {
            return Ok(none);
        }
        let rows = load_rows(&self.pool, &self.probe).await?;
        if rows.is_empty() {
            return Ok(none);
        }
        let meta: Option<(Option<String>, Option<serde_json::Value>)> = sqlx::query_as(
            "SELECT map_version, map_frame FROM matching_state WHERE id = 'default'",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some((stored_version, stored_frame)) = meta else {
            return Ok(none);
        };
        let current = stored_version.as_deref() == Some(version.as_str());
        if rows.len() == 1 {
            if !inputs_settled || (rows[0].x.is_some() && current) {
                return Ok(none);
            }
            let written = replace_map(&self.pool, &rows, &[0.0; 3], &[0.5], None, &version).await?;
            return Ok(MapUpdate {
                laid_out: usize::from(written),
                placed: 0,
            });
        }
        let laid_out_count = rows.iter().filter(|r| r.laid_out == Some(true)).count();
        let relayout = !current || stored_frame.is_none() || laid_out_count < 2;
        if relayout {
            if !inputs_settled {
                return Ok(none);
            }
            self.before_compute().await;
            let rows = Arc::new(rows);
            let state = Arc::new(state);
            let map = self.compute_layout(rows.clone(), state).await?;
            let frame = StoredFrame {
                colour: map.frame,
                max_score: map.max_score,
            };
            let written = replace_map(
                &self.pool,
                &rows,
                &map.positions,
                &map.colour,
                Some(&frame),
                &version,
            )
            .await?;
            return Ok(MapUpdate {
                laid_out: if written { rows.len() } else { 0 },
                placed: 0,
            });
        }
        let unplaced: Vec<&TrackRow> = rows.iter().filter(|r| r.x.is_none()).collect();
        if unplaced.is_empty() {
            return Ok(none);
        }
        let frame: StoredFrame = serde_json::from_value(stored_frame.unwrap_or_default())
            .map_err(|e| MapError::BadState(format!("map frame: {e}")))?;
        self.before_compute().await;
        let placed = self
            .compute_placement(Arc::new(rows.clone()), Arc::new(state), frame)
            .await?;
        let unplaced: Vec<TrackRow> = rows.into_iter().filter(|r| r.x.is_none()).collect();
        let n = unplaced.len();
        let written = write_while_state_stands(
            &self.pool,
            Write::Place {
                rows: &unplaced,
                positions: &placed.positions,
                colour: &placed.colour,
            },
        )
        .await?;
        Ok(MapUpdate {
            laid_out: 0,
            placed: if written { n } else { 0 },
        })
    }

    async fn before_compute(&self) {
        if let Some(hook) = &self.hooks.before_compute {
            hook().await;
        }
    }

    async fn compute_layout(
        &self,
        rows: Arc<Vec<TrackRow>>,
        state: Arc<MatchingState>,
    ) -> Result<LaidOutMap, MapError> {
        let cancel = self.cancel.clone();
        let done = tokio::task::spawn_blocking(move || {
            let tracks: Vec<MatchTrack> = rows.iter().map(|r| to_track(r, &state)).collect();
            let feel_z = standardised(&rows, &state);
            layout_library_cancellable(
                &tracks,
                &feel_z,
                &state.genres,
                state.genre_weight,
                None,
                &cancel,
            )
        })
        .await
        .map_err(|e| MapError::Failed(e.to_string()))?;
        done.ok_or(MapError::Cancelled)
    }

    async fn compute_placement(
        &self,
        rows: Arc<Vec<TrackRow>>,
        state: Arc<MatchingState>,
        frame: StoredFrame,
    ) -> Result<PlacedTracks, MapError> {
        let cancel = self.cancel.clone();
        let done = tokio::task::spawn_blocking(move || {
            let laid_out: Vec<&TrackRow> =
                rows.iter().filter(|r| r.laid_out == Some(true)).collect();
            let positioned: Vec<&TrackRow> = rows.iter().filter(|r| r.x.is_some()).collect();
            let new: Vec<&TrackRow> = rows.iter().filter(|r| r.x.is_none()).collect();
            let tracks = |rs: &[&TrackRow]| -> Vec<MatchTrack> {
                rs.iter().map(|r| to_track(r, &state)).collect()
            };
            place_tracks_cancellable(
                &tracks(&laid_out),
                &positions_of(&laid_out),
                &positions_of(&positioned),
                &tracks(&new),
                &state.genres,
                state.genre_weight,
                &frame.colour,
                frame.max_score,
                &cancel,
            )
        })
        .await
        .map_err(|e| MapError::Failed(e.to_string()))?;
        done.ok_or(MapError::Cancelled)
    }
}

/// Feel standardised per axis (valence, arousal); an axis every track shares counts as zero.
fn standardised(rows: &[TrackRow], state: &MatchingState) -> Vec<f64> {
    let (mean, sd) = (state.feel.mean, state.feel.sd);
    let mut z = Vec::with_capacity(rows.len() * 2);
    for r in rows {
        let raw = [f64::from(r.valence), f64::from(r.arousal)];
        for k in 0..2 {
            z.push(if sd[k] > 0.0 {
                (raw[k] - mean[k]) / sd[k]
            } else {
                0.0
            });
        }
    }
    z
}

fn to_track(r: &TrackRow, state: &MatchingState) -> MatchTrack {
    MatchTrack {
        track_id: r.id.to_string(),
        album_id: r.album_id.to_string(),
        artist_id: r.artist_id.to_string(),
        sound: formula::unit(
            &r.embedding
                .iter()
                .map(|x| f64::from(*x))
                .collect::<Vec<_>>(),
        ),
        feel: formula::feel_vector([f64::from(r.valence), f64::from(r.arousal)], &state.feel),
    }
}

fn positions_of(rows: &[&TrackRow]) -> Vec<f64> {
    rows.iter()
        .flat_map(|r| [r.x.unwrap_or(0.0), r.y.unwrap_or(0.0), r.z.unwrap_or(0.0)])
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct MapStatus {
    laid_out: bool,
    complete: bool,
}

/// Whether the map is laid out at this version, and whether every eligible track with a current feel has a place.
async fn map_status(pool: &PgPool, probe: &FeelProbe, version: &str) -> sqlx::Result<MapStatus> {
    let meta: Option<(Option<String>, Option<serde_json::Value>)> =
        sqlx::query_as("SELECT map_version, map_frame FROM matching_state WHERE id = 'default'")
            .fetch_optional(pool)
            .await?;
    let ready = meta.is_some_and(|(v, frame)| v.as_deref() == Some(version) && frame.is_some());
    if !ready {
        return Ok(MapStatus {
            laid_out: false,
            complete: false,
        });
    }
    let eligible: &str = &ELIGIBLE;
    let (unplaced, laid_out): (i64, i64) = sqlx::query_as(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at FROM {eligible})
         SELECT count(*) FILTER (WHERE m.track_id IS NULL)::bigint, count(*) FILTER (WHERE m.laid_out)::bigint
         FROM e
         JOIN matching_feel f ON f.track_id = e.track_id AND f.embedded_at = e.created_at AND f.probe = $1
         LEFT JOIN matching_map m ON m.track_id = e.track_id AND m.embedded_at = e.created_at"
    ))
    .bind(&probe.probe_id)
    .fetch_one(pool)
    .await?;
    let laid_out = laid_out >= 2;
    Ok(MapStatus {
        laid_out,
        complete: laid_out && unplaced == 0,
    })
}

/// Every eligible track with a current feel, its stored place if it has one for this vector, in id order.
async fn load_rows(pool: &PgPool, probe: &FeelProbe) -> sqlx::Result<Vec<TrackRow>> {
    let eligible: &str = &ELIGIBLE;
    sqlx::query_as(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at, te.embedding::real[] AS embedding, t.album_id FROM {eligible})
         SELECT e.track_id AS id, e.album_id, al.artist_id, e.embedding, f.valence, f.arousal,
                e.created_at AS embedded_at, m.x, m.y, m.z, m.laid_out
         FROM e
         JOIN matching_feel f ON f.track_id = e.track_id AND f.embedded_at = e.created_at AND f.probe = $1
         JOIN albums al ON al.id = e.album_id
         LEFT JOIN matching_map m ON m.track_id = e.track_id AND m.embedded_at = e.created_at
         ORDER BY e.track_id"
    ))
    .bind(&probe.probe_id)
    .fetch_all(pool)
    .await
}

fn is_conflict(e: &sqlx::Error) -> bool {
    // 23503: a vector the rows point at went away (a file change or re-run) after they were read; 40001/40P01: the
    // usual write conflicts. The inputs moved: the write is dropped and the next pass redoes it.
    matches!(
        e.as_database_error().and_then(|d| d.code()).as_deref(),
        Some("23503" | "40001" | "40P01")
    )
}

/// What a map write stores.
enum Write<'a> {
    /// A new layout replaces the stored map whole: every row laid out, and the version and frame it was made with.
    Replace {
        rows: &'a [TrackRow],
        positions: &'a [f64],
        colour: &'a [f64],
        frame: Option<serde_json::Value>,
        version: &'a str,
    },
    /// New or re-embedded tracks get their place; nothing else changes.
    Place {
        rows: &'a [TrackRow],
        positions: &'a [f64],
        colour: &'a [f64],
    },
}

/// Writes map rows only while the state they were computed from still stands. The state row is locked first, and a
/// re-run deletes that row before the stored map, so the two queue on the one row: a re-run that came first leaves
/// nothing to lock and the write is dropped; a write that came first finishes, and the re-run then clears it.
async fn write_while_state_stands(pool: &PgPool, write: Write<'_>) -> sqlx::Result<bool> {
    let attempt = async {
        let mut tx = pool.begin().await?;
        sqlx::query(&format!(
            "SET LOCAL statement_timeout = '{}s'",
            MAP_WRITE_TIMEOUT.as_secs()
        ))
        .execute(&mut *tx)
        .await?;
        let state: Option<String> =
            sqlx::query_scalar("SELECT id FROM matching_state WHERE id = 'default' FOR UPDATE")
                .fetch_optional(&mut *tx)
                .await?;
        if state.is_none() {
            return Ok(false);
        }
        match &write {
            Write::Replace {
                rows,
                positions,
                colour,
                frame,
                version,
            } => {
                sqlx::query("DELETE FROM matching_map")
                    .execute(&mut *tx)
                    .await?;
                write_rows(&mut tx, rows, positions, colour, true).await?;
                sqlx::query(
                    "UPDATE matching_state SET map_version = $1, map_frame = $2, map_updated_at = now()
                     WHERE id = 'default'",
                )
                .bind(version)
                .bind(frame)
                .execute(&mut *tx)
                .await?;
            }
            Write::Place {
                rows,
                positions,
                colour,
            } => {
                write_rows(&mut tx, rows, positions, colour, false).await?;
                sqlx::query(
                    "UPDATE matching_state SET map_updated_at = now() WHERE id = 'default'",
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(true)
    };
    match attempt.await {
        Err(e) if is_conflict(&e) => {
            tracing::debug!("map write lost a conflict, the next pass redoes it: {e}");
            Ok(false)
        }
        other => other,
    }
}

async fn replace_map(
    pool: &PgPool,
    rows: &[TrackRow],
    positions: &[f64],
    colour: &[f64],
    frame: Option<&StoredFrame>,
    version: &str,
) -> sqlx::Result<bool> {
    let frame = frame
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    write_while_state_stands(
        pool,
        Write::Replace {
            rows,
            positions,
            colour,
            frame,
            version,
        },
    )
    .await
}

/// Upserts `rows` with their positions (`rows x 3`) and colours, in chunks of 5,000.
async fn write_rows(
    tx: &mut PgConnection,
    rows: &[TrackRow],
    positions: &[f64],
    colour: &[f64],
    laid_out: bool,
) -> sqlx::Result<()> {
    for (c, part) in rows.chunks(WRITE_CHUNK).enumerate() {
        let start = c * WRITE_CHUNK;
        let at = |axis: usize| -> Vec<f64> {
            (0..part.len())
                .map(|i| positions[(start + i) * 3 + axis])
                .collect()
        };
        sqlx::query(
            "INSERT INTO matching_map (track_id, embedded_at, x, y, z, colour, laid_out)
             SELECT u.id, u.embedded_at, u.x, u.y, u.z, u.colour, $7
             FROM unnest($1::uuid[], $2::timestamptz[], $3::float8[], $4::float8[], $5::float8[], $6::float8[])
                  AS u(id, embedded_at, x, y, z, colour)
             ON CONFLICT (track_id) DO UPDATE
             SET embedded_at = EXCLUDED.embedded_at, x = EXCLUDED.x, y = EXCLUDED.y, z = EXCLUDED.z,
                 colour = EXCLUDED.colour, laid_out = EXCLUDED.laid_out",
        )
        .bind(part.iter().map(|r| r.id).collect::<Vec<Uuid>>())
        .bind(part.iter().map(|r| r.embedded_at).collect::<Vec<_>>())
        .bind(at(0))
        .bind(at(1))
        .bind(at(2))
        .bind(&colour[start..start + part.len()])
        .bind(laid_out)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

/// One track on the stored map.
#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct MapPoint {
    pub track_id: Uuid,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Place on the logo's gradient, 0 the azure end to 1 the amber end.
    pub colour: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredMap {
    pub points: Vec<MapPoint>,
    pub updated_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MapRead {
    /// No track is laid out yet but the library has eligible tracks: the first layout is pending.
    Computing,
    Ready(StoredMap),
}

/// The stored map as the page draws it: every eligible track whose stored place belongs to its current vector, so a
/// missing file or a vector of another model never appears.
pub async fn read_map(pool: &PgPool) -> sqlx::Result<MapRead> {
    let eligible: &str = &ELIGIBLE;
    let points: Vec<MapPoint> = sqlx::query_as(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at FROM {eligible})
         SELECT m.track_id, m.x, m.y, m.z, m.colour
         FROM e JOIN matching_map m ON m.track_id = e.track_id AND m.embedded_at = e.created_at
         ORDER BY m.track_id"
    ))
    .fetch_all(pool)
    .await?;
    if points.is_empty() {
        let pending: bool =
            sqlx::query_scalar(&format!("SELECT EXISTS (SELECT 1 FROM {eligible})"))
                .fetch_one(pool)
                .await?;
        if pending {
            return Ok(MapRead::Computing);
        }
    }
    let updated_at: Option<Option<OffsetDateTime>> =
        sqlx::query_scalar("SELECT map_updated_at FROM matching_state WHERE id = 'default'")
            .fetch_optional(pool)
            .await?;
    Ok(MapRead::Ready(StoredMap {
        points,
        updated_at: updated_at.flatten(),
    }))
}
