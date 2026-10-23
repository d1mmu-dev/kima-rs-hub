//! Playlists. Items are ordered by a dense integer `sort` (0..n-1). Every mutation takes a row
//! lock on the playlist (`SELECT ... FOR UPDATE`) inside a transaction, so concurrent adds,
//! removals and moves are serialised and the order stays dense and consistent.

use std::collections::HashSet;

use sqlx::{FromRow, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::browse::TrackRow;

#[derive(Debug, Clone, FromRow)]
pub struct Playlist {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub is_public: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct PlaylistSummary {
    #[sqlx(flatten)]
    pub playlist: Playlist,
    pub username: String,
    /// Present items only (tracks flagged missing are not counted).
    pub track_count: i64,
    pub hidden: bool,
}

/// A playlist item with its track. Items whose track is flagged missing are not listed: they stay
/// in the playlist (keeping their place) and return with the file.
#[derive(Debug, Clone, FromRow)]
pub struct ItemRow {
    pub playlist_id: Uuid,
    pub item_id: Uuid,
    pub sort: i32,
    #[sqlx(flatten)]
    pub track: TrackRow,
}

const PLAYLIST_COLS: &str = "p.id, p.user_id, p.name, p.is_public, p.created_at, p.updated_at";

const ITEM_COLS: &str = "pi.playlist_id, pi.id AS item_id, pi.sort,
        t.id, t.title, t.artist_name, t.duration_secs, t.disc, t.track_no,
        t.codec, t.bitrate, t.year, al.id AS album_id, al.title AS album_title, al.year AS album_year,
        ar.id AS album_artist_id, ar.name AS album_artist_name, t.recording_mbid, t.isrc";

const ITEM_FROM: &str = "FROM playlist_items pi
    JOIN tracks t ON t.id = pi.track_id AND t.missing_since IS NULL
    JOIN albums al ON al.id = t.album_id
    JOIN artists ar ON ar.id = al.artist_id";

pub async fn find(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<Playlist>> {
    sqlx::query_as(&format!(
        "SELECT {PLAYLIST_COLS} FROM playlists p WHERE p.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn create(
    pool: &PgPool,
    id: Uuid,
    user: Uuid,
    name: &str,
    is_public: bool,
) -> sqlx::Result<Playlist> {
    sqlx::query_as(
        "INSERT INTO playlists (id, user_id, name, is_public) VALUES ($1, $2, $3, $4)
         RETURNING id, user_id, name, is_public, created_at, updated_at",
    )
    .bind(id)
    .bind(user)
    .bind(name)
    .bind(is_public)
    .fetch_one(pool)
    .await
}

pub async fn update(
    pool: &PgPool,
    id: Uuid,
    name: Option<&str>,
    is_public: Option<bool>,
) -> sqlx::Result<Option<Playlist>> {
    sqlx::query_as(
        "UPDATE playlists SET name = COALESCE($2, name), is_public = COALESCE($3, is_public),
                updated_at = now()
         WHERE id = $1
         RETURNING id, user_id, name, is_public, created_at, updated_at",
    )
    .bind(id)
    .bind(name)
    .bind(is_public)
    .fetch_optional(pool)
    .await
}

pub async fn delete(pool: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    let done = sqlx::query("DELETE FROM playlists WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// A playlist with its owner's name and the viewer's hidden flag. `None` when it does not exist
/// or the viewer may not read it.
pub async fn summary(
    pool: &PgPool,
    id: Uuid,
    viewer: Uuid,
) -> sqlx::Result<Option<PlaylistSummary>> {
    Ok(summaries(pool, Some(id), viewer, 1, 0).await?.pop())
}

/// The viewer's own and public playlists, newest first.
pub async fn list(
    pool: &PgPool,
    viewer: Uuid,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<PlaylistSummary>> {
    summaries(pool, None, viewer, limit, offset).await
}

async fn summaries(
    pool: &PgPool,
    only: Option<Uuid>,
    viewer: Uuid,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<PlaylistSummary>> {
    sqlx::query_as(&format!(
        "SELECT {PLAYLIST_COLS}, u.username,
            (SELECT count(*) FROM playlist_items pi
               JOIN tracks t ON t.id = pi.track_id AND t.missing_since IS NULL
              WHERE pi.playlist_id = p.id) AS track_count,
            EXISTS (SELECT 1 FROM hidden_playlists h
                     WHERE h.playlist_id = p.id AND h.user_id = $1) AS hidden
         FROM playlists p
         JOIN users u ON u.id = p.user_id
         WHERE ($2::uuid IS NULL OR p.id = $2) AND (p.user_id = $1 OR p.is_public)
         ORDER BY p.created_at DESC, p.id DESC
         LIMIT $3 OFFSET $4"
    ))
    .bind(viewer)
    .bind(only)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// All present items of a playlist, in order.
pub async fn items(pool: &PgPool, playlist: Uuid) -> sqlx::Result<Vec<ItemRow>> {
    sqlx::query_as(&format!(
        "SELECT {ITEM_COLS} {ITEM_FROM} WHERE pi.playlist_id = $1 ORDER BY pi.sort, pi.id"
    ))
    .bind(playlist)
    .fetch_all(pool)
    .await
}

/// The first `per_playlist` present items of each listed playlist, in order (cover mosaics).
pub async fn first_items(
    pool: &PgPool,
    playlists: &[Uuid],
    per_playlist: i64,
) -> sqlx::Result<Vec<ItemRow>> {
    sqlx::query_as(&format!(
        "SELECT * FROM (
            SELECT {ITEM_COLS},
                   row_number() OVER (PARTITION BY pi.playlist_id ORDER BY pi.sort, pi.id) AS rn
            {ITEM_FROM}
            WHERE pi.playlist_id = ANY($1)
         ) q WHERE rn <= $2
         ORDER BY playlist_id, sort"
    ))
    .bind(playlists)
    .bind(per_playlist)
    .fetch_all(pool)
    .await
}

/// The present items for these tracks, in playlist order.
pub async fn items_for_tracks(
    pool: &PgPool,
    playlist: Uuid,
    tracks: &[Uuid],
) -> sqlx::Result<Vec<ItemRow>> {
    sqlx::query_as(&format!(
        "SELECT {ITEM_COLS} {ITEM_FROM}
         WHERE pi.playlist_id = $1 AND pi.track_id = ANY($2) ORDER BY pi.sort, pi.id"
    ))
    .bind(playlist)
    .bind(tracks)
    .fetch_all(pool)
    .await
}

/// Locks the playlist row for the rest of the transaction. `false` when it is gone.
async fn lock(tx: &mut Transaction<'_, Postgres>, playlist: Uuid) -> sqlx::Result<bool> {
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM playlists WHERE id = $1 FOR UPDATE")
            .bind(playlist)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(found.is_some())
}

async fn touch(tx: &mut Transaction<'_, Postgres>, playlist: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE playlists SET updated_at = now() WHERE id = $1")
        .bind(playlist)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AddResult {
    pub added: usize,
    /// Already in the playlist.
    pub duplicates: usize,
    /// Not a present library track.
    pub rejected: Vec<Uuid>,
}

/// Appends tracks in the given order. Repeated ids count once; tracks already in the playlist
/// are duplicates (not an error); unknown tracks and tracks flagged missing are rejected.
/// `None` when the playlist no longer exists.
pub async fn add_tracks(
    pool: &PgPool,
    playlist: Uuid,
    tracks: &[Uuid],
) -> sqlx::Result<Option<AddResult>> {
    let mut tx = pool.begin().await?;
    if !lock(&mut tx, playlist).await? {
        return Ok(None);
    }
    let result = add_locked(&mut tx, playlist, tracks).await?;
    tx.commit().await?;
    Ok(Some(result))
}

/// Replaces every item with `tracks`, under one row lock, by M1's add rules (repeats count once,
/// unknown and missing tracks are rejected). `None` when the playlist no longer exists.
pub async fn replace_items(
    pool: &PgPool,
    playlist: Uuid,
    tracks: &[Uuid],
) -> sqlx::Result<Option<AddResult>> {
    let mut tx = pool.begin().await?;
    if !lock(&mut tx, playlist).await? {
        return Ok(None);
    }
    let cleared = sqlx::query("DELETE FROM playlist_items WHERE playlist_id = $1")
        .bind(playlist)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    let result = add_locked(&mut tx, playlist, tracks).await?;
    if cleared > 0 && result.added == 0 {
        touch(&mut tx, playlist).await?;
    }
    tx.commit().await?;
    Ok(Some(result))
}

/// Removes the items at these positions, counted over the present items in `sort` order (the
/// ones a client was shown, D-065). Positions out of range are ignored, repeats count once. The
/// order is rewritten dense. Returns how many were removed.
pub async fn remove_at_indexes(
    pool: &PgPool,
    playlist: Uuid,
    indexes: &[usize],
) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    if !lock(&mut tx, playlist).await? {
        return Ok(0);
    }
    let removed = remove_locked(&mut tx, playlist, indexes).await?;
    tx.commit().await?;
    Ok(removed)
}

/// Removes the items at `remove_indexes` (as `remove_at_indexes`), then appends `add` (as
/// `add_tracks`), under one row lock in one transaction: when either step fails, neither lands.
/// Returns how many were removed and the add outcome; `None` when the playlist no longer exists.
pub async fn edit_items(
    pool: &PgPool,
    playlist: Uuid,
    remove_indexes: &[usize],
    add: &[Uuid],
) -> sqlx::Result<Option<(u64, AddResult)>> {
    let mut tx = pool.begin().await?;
    if !lock(&mut tx, playlist).await? {
        return Ok(None);
    }
    let removed = remove_locked(&mut tx, playlist, remove_indexes).await?;
    let added = add_locked(&mut tx, playlist, add).await?;
    tx.commit().await?;
    Ok(Some((removed, added)))
}

/// `remove_at_indexes` with the playlist row already locked.
async fn remove_locked(
    tx: &mut Transaction<'_, Postgres>,
    playlist: Uuid,
    indexes: &[usize],
) -> sqlx::Result<u64> {
    if indexes.is_empty() {
        return Ok(0);
    }
    let visible: Vec<Uuid> = sqlx::query_scalar(
        "SELECT pi.id FROM playlist_items pi
         JOIN tracks t ON t.id = pi.track_id AND t.missing_since IS NULL
         WHERE pi.playlist_id = $1 ORDER BY pi.sort, pi.id",
    )
    .bind(playlist)
    .fetch_all(&mut **tx)
    .await?;
    let doomed: Vec<Uuid> = indexes
        .iter()
        .filter_map(|i| visible.get(*i).copied())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if doomed.is_empty() {
        return Ok(0);
    }
    let removed = sqlx::query("DELETE FROM playlist_items WHERE playlist_id = $1 AND id = ANY($2)")
        .bind(playlist)
        .bind(&doomed)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    let ids = ordered_ids(tx, playlist).await?;
    write_order(tx, playlist, &ids).await?;
    touch(tx, playlist).await?;
    Ok(removed)
}

/// Total duration in seconds of the present items, per playlist (playlists without any are
/// absent from the map).
pub async fn durations(
    pool: &PgPool,
    playlists: &[Uuid],
) -> sqlx::Result<std::collections::HashMap<Uuid, i64>> {
    let rows: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT pi.playlist_id, COALESCE(sum(t.duration_secs), 0)::bigint
         FROM playlist_items pi
         JOIN tracks t ON t.id = pi.track_id AND t.missing_since IS NULL
         WHERE pi.playlist_id = ANY($1) GROUP BY pi.playlist_id",
    )
    .bind(playlists)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// The add rules, with the playlist row already locked.
async fn add_locked(
    tx: &mut Transaction<'_, Postgres>,
    playlist: Uuid,
    tracks: &[Uuid],
) -> sqlx::Result<AddResult> {
    let mut seen = HashSet::new();
    let requested: Vec<Uuid> = tracks.iter().copied().filter(|t| seen.insert(*t)).collect();

    let present: HashSet<Uuid> =
        sqlx::query_scalar("SELECT id FROM tracks WHERE id = ANY($1) AND missing_since IS NULL")
            .bind(&requested)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .collect();
    let rejected: Vec<Uuid> = requested
        .iter()
        .copied()
        .filter(|t| !present.contains(t))
        .collect();
    let existing: HashSet<Uuid> = sqlx::query_scalar(
        "SELECT track_id FROM playlist_items WHERE playlist_id = $1 AND track_id = ANY($2)",
    )
    .bind(playlist)
    .bind(&requested)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    let fresh: Vec<Uuid> = requested
        .iter()
        .copied()
        .filter(|t| present.contains(t) && !existing.contains(t))
        .collect();
    let duplicates = requested.iter().filter(|t| existing.contains(t)).count();

    if !fresh.is_empty() {
        let first: i32 = sqlx::query_scalar(
            "SELECT COALESCE(max(sort) + 1, 0) FROM playlist_items WHERE playlist_id = $1",
        )
        .bind(playlist)
        .fetch_one(&mut **tx)
        .await?;
        let ids: Vec<Uuid> = fresh.iter().map(|_| Uuid::now_v7()).collect();
        sqlx::query(
            "INSERT INTO playlist_items (id, playlist_id, track_id, sort)
             SELECT u.id, $1, u.track_id, $4 + (u.n - 1)::int
             FROM unnest($2::uuid[], $3::uuid[]) WITH ORDINALITY AS u(id, track_id, n)",
        )
        .bind(playlist)
        .bind(&ids)
        .bind(&fresh)
        .bind(first)
        .execute(&mut **tx)
        .await?;
        touch(tx, playlist).await?;
    }
    Ok(AddResult {
        added: fresh.len(),
        duplicates,
        rejected,
    })
}

/// Writes `sort = 0..n-1` for the given item ids, in order.
async fn write_order(
    tx: &mut Transaction<'_, Postgres>,
    playlist: Uuid,
    ordered: &[Uuid],
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE playlist_items pi SET sort = (u.n - 1)::int
         FROM unnest($2::uuid[]) WITH ORDINALITY AS u(id, n)
         WHERE pi.id = u.id AND pi.playlist_id = $1 AND pi.sort <> (u.n - 1)::int",
    )
    .bind(playlist)
    .bind(ordered)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn ordered_ids(
    tx: &mut Transaction<'_, Postgres>,
    playlist: Uuid,
) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM playlist_items WHERE playlist_id = $1 ORDER BY sort, id")
        .bind(playlist)
        .fetch_all(&mut **tx)
        .await
}

/// Removes tracks (present or missing) from a playlist and closes the gaps. Returns how many
/// were removed.
pub async fn remove_tracks(pool: &PgPool, playlist: Uuid, tracks: &[Uuid]) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    if !lock(&mut tx, playlist).await? {
        return Ok(0);
    }
    let removed =
        sqlx::query("DELETE FROM playlist_items WHERE playlist_id = $1 AND track_id = ANY($2)")
            .bind(playlist)
            .bind(tracks)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if removed > 0 {
        let ids = ordered_ids(&mut tx, playlist).await?;
        write_order(&mut tx, playlist, &ids).await?;
        touch(&mut tx, playlist).await?;
    }
    tx.commit().await?;
    Ok(removed)
}

#[derive(Debug, PartialEq, Eq)]
pub enum MoveError {
    ItemNotFound,
    AnchorNotFound,
}

/// Moves one item directly after `after` (first when `None`). Returns the item's new `sort`.
pub async fn move_item(
    pool: &PgPool,
    playlist: Uuid,
    item: Uuid,
    after: Option<Uuid>,
) -> sqlx::Result<Result<i32, MoveError>> {
    let mut tx = pool.begin().await?;
    if !lock(&mut tx, playlist).await? {
        return Ok(Err(MoveError::ItemNotFound));
    }
    let mut ids = ordered_ids(&mut tx, playlist).await?;
    let Some(from) = ids.iter().position(|i| *i == item) else {
        return Ok(Err(MoveError::ItemNotFound));
    };
    if let Some(anchor) = after
        && !ids.contains(&anchor)
    {
        return Ok(Err(MoveError::AnchorNotFound));
    }
    if after != Some(item) {
        ids.remove(from);
        let at = match after {
            Some(anchor) => ids.iter().position(|i| *i == anchor).map_or(0, |p| p + 1),
            None => 0,
        };
        ids.insert(at, item);
        write_order(&mut tx, playlist, &ids).await?;
        touch(&mut tx, playlist).await?;
    }
    let sort = ids.iter().position(|i| *i == item).unwrap_or(0) as i32;
    tx.commit().await?;
    Ok(Ok(sort))
}

pub async fn hide(pool: &PgPool, user: Uuid, playlist: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO hidden_playlists (user_id, playlist_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(user)
    .bind(playlist)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn unhide(pool: &PgPool, user: Uuid, playlist: Uuid) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM hidden_playlists WHERE user_id = $1 AND playlist_id = $2")
        .bind(user)
        .bind(playlist)
        .execute(pool)
        .await?;
    Ok(())
}
