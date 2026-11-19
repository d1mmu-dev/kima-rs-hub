//! Song mood tags (M5b Task 5; port of upstream `services/moods/tags.ts`), written by the mood step after the scores
//! and statistics. A song is tagged with each live mood it sits at least [`TAG_LINE_Z`] SD above the library's mean
//! in; a defined mood (focus, driving) uses its defined score, the one the recipes read. The retired mood (night) has
//! no tag. Every copy of a title is tagged, or an untagged copy would play on the Wildcard station.

use std::sync::Mutex;

use kima_mixes::artist::MIN_MIX_TRACKS;
use kima_mixes::definitions::{DEFINITIONS, Definition, definition_of};
use kima_mixes::recipes::{RECIPE_LINE, is_live_mood};
use kima_vibe::mood_heads::{MOODS, MoodHeads};
use sqlx::PgPool;

use super::recipe::{
    defined_ctes, defined_stats_cte, defined_z_sql, definition_inputs, scored_songs,
};
use crate::vibe::ELIGIBLE;
use crate::vibe::moods::load_mood_state;
use crate::vibe::state::is_write_conflict;

/// A song is tagged at this many library SDs above the mean of its mood's score: the recipes' own line.
pub const TAG_LINE_Z: f64 = RECIPE_LINE;

/// The statistics row's signature (version, inputs fingerprint, written at): none while there is no row.
const SIGNATURE: &str = "SELECT version || ':' || fingerprint || ':' || computed_at::text FROM mood_state WHERE id = 'default'";

/// The tag writer of one process. It remembers the statistics row its last complete pass read: the row is rewritten
/// whenever its inputs fingerprint changes (`kima_db::vibe::moods`), so a pass over the same row would change nothing.
/// That holds only while every definition part reads inputs the fingerprint covers (moods, the CLAP readings, tempo
/// and its confidence; the key's scale too), which `tags_follow_what_the_fingerprint_covers` checks.
#[derive(Debug, Default)]
pub struct MoodTagger {
    tagged_from: Mutex<Option<String>>,
}

impl MoodTagger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Upstream `writeMoodTags`: every track's tags, in the moods' order. A song missing a reading has no tag of a
    /// mood whose definition reads it, an unscorable definition tags no song, and no defined mood is tagged while the
    /// library holds fewer than [`MIN_MIX_TRACKS`] songs. Only changed tracks are written, and a pass over the
    /// statistics the last one used is skipped. With no statistics (or statistics of other weights) tags stay. A
    /// write conflict (another transaction held tracks this one needed) writes nothing and returns 0; the next pass
    /// writes the tags. Returns how many tracks' tags changed.
    pub async fn write(&self, pool: &PgPool, heads: &MoodHeads) -> sqlx::Result<u64> {
        let current: Option<String> = sqlx::query_scalar(SIGNATURE).fetch_optional(pool).await?;
        if current.is_some() && current == *self.tagged_from.lock().unwrap() {
            return Ok(0);
        }
        match self.try_write(pool, heads).await {
            Ok((written, signature)) => {
                if let Some(signature) = signature {
                    *self.tagged_from.lock().unwrap() = Some(signature);
                }
                Ok(written)
            }
            Err(e) if is_write_conflict(&e) => {
                tracing::info!(
                    "[Moods] Tracks changed while the tags were written; the next pass writes them"
                );
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }

    async fn try_write(
        &self,
        pool: &PgPool,
        heads: &MoodHeads,
    ) -> sqlx::Result<(u64, Option<String>)> {
        // Read committed, so a track another writer changes meanwhile is waited for instead of failing the pass.
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout = '300s'")
            .execute(&mut *tx)
            .await?;
        // The state row first, as the build and a re-run take it, before the track rows: one order, so neither
        // deadlocks. Held to the commit, so the statistics cannot change under the update.
        sqlx::query("SELECT id FROM mood_state WHERE id = 'default' FOR UPDATE")
            .execute(&mut *tx)
            .await?;
        let Some(state) = load_mood_state(&mut tx, heads).await? else {
            return Ok((0, None));
        };
        let mut scorable: Vec<&Definition> = Vec::new();
        if state.track_count as usize >= MIN_MIX_TRACKS {
            for definition in DEFINITIONS.iter() {
                if defined_ctes("tags", &state, &[definition], "", "").is_ok() {
                    scorable.push(definition);
                }
            }
        }
        let defined = if scorable.is_empty() {
            String::new()
        } else {
            let ctes = defined_ctes("tags", &state, &scorable, "", "").expect("checked above");
            format!(
                "WITH library AS (SELECT t.id, {} {}), {ctes}, {}",
                definition_inputs(&scorable).join(", "),
                scored_songs(),
                defined_stats_cte(&scorable)
            )
        };
        let line = format!("({TAG_LINE_Z})::float8");
        let tags = MOODS
            .iter()
            .filter(|m| is_live_mood(m))
            .map(|mood| match definition_of(mood) {
                Some(_) if scorable.iter().any(|d| d.mood == *mood) => format!(
                    "CASE WHEN d.dscore_{mood} IS NOT NULL AND {} >= {line} THEN '{mood}' END",
                    defined_z_sql(mood, "d")
                ),
                Some(_) => "NULL".to_string(),
                None => format!(
                    "CASE WHEN (m.\"{mood}\" - s.\"{mood}_mean\") / NULLIF(s.\"{mood}_sd\", 0) >= {line} THEN '{mood}' END"
                ),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let defined_join = if scorable.is_empty() {
            ""
        } else {
            "LEFT JOIN defined d ON d.id = tr.id CROSS JOIN def_stats ds"
        };
        let eligible: &str = &ELIGIBLE;
        let sql = format!(
            "{defined}
            UPDATE tracks t
            SET mood_tags = n.tags
            FROM (
                SELECT tr.id,
                       CASE WHEN m.track_id IS NOT NULL
                            THEN array_remove(ARRAY[{tags}]::text[], NULL) ELSE '{{}}'::text[] END AS tags
                FROM tracks tr
                JOIN mood_state s ON s.id = 'default'
                LEFT JOIN (
                    SELECT m.* FROM (SELECT te.track_id, te.created_at FROM {eligible}) e
                    JOIN track_moods m ON m.track_id = e.track_id AND m.embedded_at = e.created_at AND m.weights = $1
                ) m ON m.track_id = tr.id
                {defined_join}
            ) n
            WHERE t.id = n.id AND t.mood_tags IS DISTINCT FROM n.tags"
        );
        let written = sqlx::query(&sql)
            .bind(&heads.version)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let signature: Option<String> = sqlx::query_scalar(SIGNATURE)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((written, signature))
    }
}
