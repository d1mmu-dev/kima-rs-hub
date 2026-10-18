//! Subsonic stars: one row per (user, target), the target a track, an album or an artist
//! (spec §7.3). A track flagged missing keeps its stars; lists hide them until the purge deletes
//! the track and the foreign keys cascade.

use sqlx::PgPool;
use uuid::Uuid;

/// What a star or a rating points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarKind {
    Track,
    Album,
    Artist,
}

impl StarKind {
    /// The target column of `stars` and `ratings`. Fixed strings, safe to put in SQL.
    pub(crate) fn column(self) -> &'static str {
        match self {
            StarKind::Track => "track_id",
            StarKind::Album => "album_id",
            StarKind::Artist => "artist_id",
        }
    }

    fn table(self) -> &'static str {
        match self {
            StarKind::Track => "tracks",
            StarKind::Album => "albums",
            StarKind::Artist => "artists",
        }
    }

    /// SQL that is true for a row `{alias}` of the target table that is visible: a track that
    /// is present, or an album or artist with at least one present track.
    fn present(self, alias: &str) -> String {
        match self {
            StarKind::Track => format!("{alias}.missing_since IS NULL"),
            StarKind::Album => format!(
                "EXISTS (SELECT 1 FROM tracks pt WHERE pt.album_id = {alias}.id AND pt.missing_since IS NULL)"
            ),
            StarKind::Artist => format!(
                "EXISTS (SELECT 1 FROM albums pal JOIN tracks pt ON pt.album_id = pal.id AND pt.missing_since IS NULL
                         WHERE pal.artist_id = {alias}.id)"
            ),
        }
    }
}

/// Whether the target row exists at all (a missing track still does).
pub(crate) async fn target_exists(pool: &PgPool, kind: StarKind, id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE id = $1)",
        kind.table()
    ))
    .bind(id)
    .fetch_one(pool)
    .await
}

/// Stars the target for the user. `false` when the target does not exist. Starring twice keeps
/// the first star (and its time).
pub async fn set(pool: &PgPool, user: Uuid, kind: StarKind, id: Uuid) -> sqlx::Result<bool> {
    let sql = format!(
        "INSERT INTO stars (id, user_id, {col})
         SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM {table} WHERE id = $3)
         ON CONFLICT DO NOTHING",
        col = kind.column(),
        table = kind.table()
    );
    let inserted = sqlx::query(&sql)
        .bind(Uuid::now_v7())
        .bind(user)
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
    if inserted > 0 {
        return Ok(true);
    }
    // Nothing inserted: either it was already starred or the target is unknown.
    target_exists(pool, kind, id).await
}

pub async fn clear(pool: &PgPool, user: Uuid, kind: StarKind, id: Uuid) -> sqlx::Result<()> {
    let sql = format!(
        "DELETE FROM stars WHERE user_id = $1 AND {} = $2",
        kind.column()
    );
    sqlx::query(&sql).bind(user).bind(id).execute(pool).await?;
    Ok(())
}

/// The user's starred targets of one kind, newest star first. Targets with no present track are
/// left out.
pub async fn list_for_user(pool: &PgPool, user: Uuid, kind: StarKind) -> sqlx::Result<Vec<Uuid>> {
    let sql = format!(
        "SELECT s.{col} FROM stars s JOIN {table} x ON x.id = s.{col}
         WHERE s.user_id = $1 AND s.{col} IS NOT NULL AND {present}
         ORDER BY s.starred_at DESC, s.id DESC",
        col = kind.column(),
        table = kind.table(),
        present = kind.present("x")
    );
    sqlx::query_scalar(&sql).bind(user).fetch_all(pool).await
}
