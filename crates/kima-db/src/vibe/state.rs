//! Feel update and the library's matching state (spec 6.2-6.3).
//!
//! [`build_matching`] is upstream's `buildMatching`: one `REPEATABLE READ` transaction that locks the state row
//! first (the lock order of the engine: state row, then feel/score/map rows, then track rows), computes the feel of
//! every eligible vector whose stored feel is stale, fingerprints the inputs and rebuilds the state only when
//! `(version, fingerprint)` changed. A concurrent re-run or map write shows up as a serialization failure, which
//! is swallowed: nothing is written and the next pass rebuilds.

use super::{ELIGIBLE, MATCHING_SOUND};
use kima_vibe::feel_probe::FeelProbe;
use kima_vibe::formula::{self, LibraryArtist, MbGenres};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Independent of upstream's `FORMULA_VERSION` counter (D-181): bump when the formula's stored inputs change.
pub const FORMULA_VERSION: u32 = 1;

/// The re-run key of the state row: `1/<model>/<probe id>`.
pub fn state_version(probe: &FeelProbe) -> String {
    format!(
        "{FORMULA_VERSION}/{}/{}",
        MATCHING_SOUND.model_id, probe.probe_id
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildOutcome {
    /// Feel rows written (new vectors, changed vectors, another probe).
    pub feel_updated: u64,
    /// The state row was rewritten because its version or inputs changed.
    pub rebuilt: bool,
}

/// `total` = eligible tracks; `completed` = those whose feel and map row are both current [V `state.ts:251`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub total: i64,
    pub completed: i64,
}

pub(super) fn is_write_conflict(e: &sqlx::Error) -> bool {
    matches!(
        e.as_database_error().and_then(|d| d.code()).as_deref(),
        Some("40001" | "40P01")
    )
}

/// Brings feel and state up to date with the library. A write conflict with a concurrent re-run or map write is
/// not an error: the transaction is rolled back and `Ok(BuildOutcome::default())` returned.
pub async fn build_matching(pool: &PgPool, probe: &FeelProbe) -> sqlx::Result<BuildOutcome> {
    match try_build(pool, probe).await {
        Err(e) if is_write_conflict(&e) => {
            tracing::debug!("matching build lost a write conflict, the next pass rebuilds");
            Ok(BuildOutcome::default())
        }
        other => other,
    }
}

async fn try_build(pool: &PgPool, probe: &FeelProbe) -> sqlx::Result<BuildOutcome> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    // Build 5 min < re-run 7 min < map write 8 min: chosen limits, in this order (upstream `state.ts`).
    sqlx::query("SET LOCAL statement_timeout = '300s'")
        .execute(&mut *tx)
        .await?;

    // Lock the row if it exists; a missing row is not created as a placeholder (upstream does not either).
    let prior: Option<(String, String)> = sqlx::query_as(
        "SELECT version, fingerprint FROM matching_state WHERE id = 'default' FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?;

    let feel_updated = update_feel(&mut tx, probe).await?;
    let version = state_version(probe);
    let eligible: &str = &ELIGIBLE;

    let (count, hash_sum, mv, ma, sv, sa, sound_norm) = sqlx::query_as::<
        _,
        (i64, String, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>),
    >(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at, te.embedding, t.album_id FROM {eligible})
         SELECT count(*)::bigint,
                coalesce(sum(hashtextextended(e.track_id::text || ':' || extract(epoch FROM e.created_at)::text
                         || ':' || al.artist_id::text, 0)::numeric), 0)::text,
                avg(f.valence::float8 ORDER BY e.track_id),
                avg(f.arousal::float8 ORDER BY e.track_id),
                stddev_pop(f.valence::float8 ORDER BY e.track_id),
                stddev_pop(f.arousal::float8 ORDER BY e.track_id),
                vector_norm(avg(l2_normalize(e.embedding) ORDER BY e.track_id))
         FROM e
         JOIN matching_feel f ON f.track_id = e.track_id AND f.embedded_at = e.created_at AND f.probe = $1
         JOIN albums al ON al.id = e.album_id"
    ))
    .bind(&probe.probe_id)
    .fetch_one(&mut *tx)
    .await?;

    let (Some(mv), Some(ma), Some(sv), Some(sa), Some(sound_norm)) = (mv, ma, sv, sa, sound_norm)
    else {
        // No eligible track: there is nothing to match against, so the state goes.
        sqlx::query("DELETE FROM matching_state")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(BuildOutcome {
            feel_updated,
            rebuilt: false,
        });
    };

    let artists = artist_matching_genres(&mut tx).await?;
    let fingerprint = fingerprint(&version, count, &hash_sum, &artists);
    if prior.is_some_and(|(v, f)| v == version && f == fingerprint) {
        tx.commit().await?;
        return Ok(BuildOutcome {
            feel_updated,
            rebuilt: false,
        });
    }

    let sound_spread = 1.0 - sound_norm * sound_norm;
    let (model, genre_spread) = formula::build_genre_model(&artists);
    let genre_weight = formula::genre_weight(sound_spread, genre_spread);
    let genres = serde_json::to_value(&model).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        "INSERT INTO matching_state (id, version, fingerprint, track_count, feel_mean_valence, feel_mean_arousal,
                feel_sd_valence, feel_sd_arousal, sound_spread, genre_spread, genre_weight, genres)
         VALUES ('default', $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT (id) DO UPDATE SET version = EXCLUDED.version, fingerprint = EXCLUDED.fingerprint,
                track_count = EXCLUDED.track_count, feel_mean_valence = EXCLUDED.feel_mean_valence,
                feel_mean_arousal = EXCLUDED.feel_mean_arousal, feel_sd_valence = EXCLUDED.feel_sd_valence,
                feel_sd_arousal = EXCLUDED.feel_sd_arousal, sound_spread = EXCLUDED.sound_spread,
                genre_spread = EXCLUDED.genre_spread, genre_weight = EXCLUDED.genre_weight,
                genres = EXCLUDED.genres, built_at = now()",
    )
    .bind(&version)
    .bind(&fingerprint)
    .bind(i32::try_from(count).unwrap_or(i32::MAX))
    .bind(mv)
    .bind(ma)
    .bind(sv)
    .bind(sa)
    .bind(sound_spread)
    .bind(genre_spread)
    .bind(genre_weight)
    .bind(genres)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(BuildOutcome {
        feel_updated,
        rebuilt: true,
    })
}

/// Feel of every eligible vector that has none for this probe and this vector (`embedded_at` is the vector's
/// `created_at`, which every re-embed rewrites): one `INSERT … SELECT … ON CONFLICT`, computed in SQL from the
/// stored vector, `clamp(0, 1, e . w / |e| + bias)` per axis (`<#>` is the negated inner product).
async fn update_feel(conn: &mut PgConnection, probe: &FeelProbe) -> sqlx::Result<u64> {
    let eligible: &str = &ELIGIBLE;
    let weights = |w: &[f64]| w.iter().map(|x| *x as f32).collect::<Vec<f32>>();
    let done = sqlx::query(&format!(
        "INSERT INTO matching_feel (track_id, embedded_at, probe, valence, arousal)
         SELECT te.track_id, te.created_at, $1,
                LEAST(1, GREATEST(0, -(te.embedding <#> $2::real[]::vector) / vector_norm(te.embedding) + $3)),
                LEAST(1, GREATEST(0, -(te.embedding <#> $4::real[]::vector) / vector_norm(te.embedding) + $5))
         FROM {eligible}
           AND NOT EXISTS (SELECT 1 FROM matching_feel f
                           WHERE f.track_id = te.track_id AND f.embedded_at = te.created_at AND f.probe = $1)
         ON CONFLICT (track_id) DO UPDATE SET embedded_at = EXCLUDED.embedded_at, probe = EXCLUDED.probe,
                valence = EXCLUDED.valence, arousal = EXCLUDED.arousal"
    ))
    .bind(&probe.probe_id)
    .bind(weights(&probe.valence.weights))
    .bind(probe.valence.bias)
    .bind(weights(&probe.arousal.weights))
    .bind(probe.arousal.bias)
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

/// Every artist with an eligible track, with its eligible track count and the genres the formula sees: the
/// person's own list when it has any name, else the effective enriched list (MusicBrainz, else file tags), kept
/// only where MusicBrainz lists the genre (spec 6.4, D-192).
pub async fn artist_matching_genres(conn: &mut PgConnection) -> sqlx::Result<Vec<LibraryArtist>> {
    let eligible: &str = &ELIGIBLE;
    let rows: Vec<(Uuid, i64, Vec<String>, Vec<String>)> = sqlx::query_as(&format!(
        "WITH e AS (SELECT t.album_id FROM {eligible})
         SELECT al.artist_id, count(*)::bigint, coalesce(ar.user_genres, '{{}}'),
                coalesce((SELECT array_agg(g.name ORDER BY ag.rank, g.name)
                          FROM artist_genres_effective ag JOIN genres g ON g.id = ag.genre_id
                          WHERE ag.artist_id = al.artist_id), '{{}}')
         FROM e JOIN albums al ON al.id = e.album_id JOIN artists ar ON ar.id = al.artist_id
         GROUP BY al.artist_id, ar.user_genres
         ORDER BY al.artist_id"
    ))
    .fetch_all(conn)
    .await?;
    let mb = MbGenres::bundled();
    Ok(rows
        .into_iter()
        .map(|(id, tracks, user, enriched)| {
            let own = user.iter().any(|g| !formula::genre_key(g).is_empty());
            LibraryArtist {
                artist_id: id.to_string(),
                tracks: tracks as u64,
                genres: mb.matching_genres(if own { &user } else { &enriched }),
            }
        })
        .collect())
}

/// `sha256(JSON[version, count, sum of per-track hashes, [[artist, [genres]]…]])`: the per-track part moves with
/// any vector, file or artist change; the filtered genre lists move with anything that changes the genre model.
fn fingerprint(version: &str, count: i64, hash_sum: &str, artists: &[LibraryArtist]) -> String {
    let genres: Vec<(&str, &[String])> = artists
        .iter()
        .map(|a| (a.artist_id.as_str(), a.genres.as_slice()))
        .collect();
    let text = serde_json::json!([version, count, hash_sum, genres]).to_string();
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Progress of the matching pass for `probe` (see [`Progress`]).
pub async fn matching_progress(pool: &PgPool, probe: &FeelProbe) -> sqlx::Result<Progress> {
    let eligible: &str = &ELIGIBLE;
    let (total, completed): (i64, i64) = sqlx::query_as(&format!(
        "WITH e AS (SELECT te.track_id, te.created_at FROM {eligible})
         SELECT count(*)::bigint,
                (count(*) FILTER (WHERE
                    EXISTS (SELECT 1 FROM matching_feel f WHERE f.track_id = e.track_id
                            AND f.embedded_at = e.created_at AND f.probe = $1)
                    AND EXISTS (SELECT 1 FROM matching_map m WHERE m.track_id = e.track_id
                                AND m.embedded_at = e.created_at)))::bigint
         FROM e"
    ))
    .bind(&probe.probe_id)
    .fetch_one(pool)
    .await?;
    Ok(Progress { total, completed })
}
