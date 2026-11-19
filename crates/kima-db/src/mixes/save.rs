//! Saving a Home mix as a playlist (M5b Task 9; upstream `routes/mixes.ts` `POST /:id/save`): one transaction, the
//! playlist exists with all its songs or not at all, at most one per profile and mix id (`playlists_profile_mix_idx`,
//! migration 0063).

use std::collections::HashSet;

use kima_core::profile::ProfileId;
use sqlx::PgPool;
use uuid::Uuid;

/// What a save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveMix {
    Saved {
        id: Uuid,
        name: String,
        track_count: usize,
    },
    /// The profile already saved this mix (upstream's 409); also the answer to a save that lost a race.
    Exists { id: Uuid, name: String },
    /// None of the mix's songs is playable any more (upstream's 422); nothing was written.
    NothingLeft,
}

/// The profile's playlist saved from `mix_id`, if any.
pub async fn saved_playlist(
    pool: &PgPool,
    profile: ProfileId,
    mix_id: &str,
) -> sqlx::Result<Option<(Uuid, String)>> {
    sqlx::query_as("SELECT id, name FROM playlists WHERE profile_id = $1 AND mix_id = $2")
        .bind(profile)
        .bind(mix_id)
        .fetch_optional(pool)
        .await
}

/// Saves the mix's playable songs (duplicates dropped, the mix's order kept) as a private playlist named `name`.
/// The songs are locked in id order `FOR SHARE` through the commit, as playlist adds lock them, so a scan cannot mark
/// one missing between the check and the insert.
pub async fn save_mix_playlist(
    pool: &PgPool,
    profile: ProfileId,
    mix_id: &str,
    name: &str,
    track_ids: &[Uuid],
) -> sqlx::Result<SaveMix> {
    if let Some((id, name)) = saved_playlist(pool, profile, mix_id).await? {
        return Ok(SaveMix::Exists { id, name });
    }
    let mut seen = HashSet::new();
    let wanted: Vec<Uuid> = track_ids
        .iter()
        .copied()
        .filter(|t| seen.insert(*t))
        .collect();
    let mut tx = pool.begin().await?;
    let present: HashSet<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tracks WHERE id = ANY($1) AND missing_since IS NULL ORDER BY id FOR SHARE",
    )
    .bind(&wanted)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let kept: Vec<Uuid> = wanted.into_iter().filter(|t| present.contains(t)).collect();
    if kept.is_empty() {
        return Ok(SaveMix::NothingLeft);
    }
    let created: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO playlists (id, profile_id, name, is_public, mix_id) VALUES ($1, $2, $3, false, $4)
         ON CONFLICT (profile_id, mix_id) WHERE mix_id IS NOT NULL DO NOTHING
         RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(profile)
    .bind(name)
    .bind(mix_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = created else {
        // Another save of this mix committed first.
        tx.rollback().await?;
        return Ok(match saved_playlist(pool, profile, mix_id).await? {
            Some((id, name)) => SaveMix::Exists { id, name },
            None => SaveMix::NothingLeft,
        });
    };
    let item_ids: Vec<Uuid> = kept.iter().map(|_| Uuid::now_v7()).collect();
    sqlx::query(
        "INSERT INTO playlist_items (id, playlist_id, track_id, sort)
         SELECT i.id, $1, i.track_id, (i.ord - 1)::int
         FROM unnest($2::uuid[], $3::uuid[]) WITH ORDINALITY i(id, track_id, ord)",
    )
    .bind(id)
    .bind(&item_ids)
    .bind(&kept)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(SaveMix::Saved {
        id,
        name: name.to_string(),
        track_count: kept.len(),
    })
}
