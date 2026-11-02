//! Read models for the admin autotag review API (spec §16.5).
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::journal::JournalRow;

#[derive(Debug, Default)]
pub struct AlbumFilter {
    pub state: Option<String>,
    pub recommendation: Option<String>,
    pub q: Option<String>,
}

#[derive(Debug)]
pub struct AlbumListRow {
    pub album_id: Uuid,
    pub artist: String,
    pub album: String,
    pub year: Option<i32>,
    pub track_count: i64,
    pub state: String,
    pub recommendation: Option<String>,
    pub distance: Option<f64>,
    pub release_mbid: Option<Uuid>,
    pub searched_at: Option<OffsetDateTime>,
    pub applied_at: Option<OffsetDateTime>,
}

/// Present albums only, in a stable artist/title/year/id order. Search is a literal,
/// case- and accent-insensitive substring, not user-supplied LIKE syntax.
pub async fn list_albums(
    pool: &PgPool,
    f: &AlbumFilter,
    offset: i64,
    limit: i64,
) -> sqlx::Result<(Vec<AlbumListRow>, i64)> {
    let q = f.q.as_ref().map(|q| {
        format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM albums al JOIN artists ar ON ar.id=al.artist_id
        LEFT JOIN autotag_albums aa ON aa.album_id=al.id
        WHERE EXISTS (SELECT 1 FROM tracks t WHERE t.album_id=al.id AND t.missing_since IS NULL)
        AND ($1::text IS NULL OR COALESCE(aa.state,'new')=$1)
        AND ($2::text IS NULL OR aa.recommendation=$2)
        AND ($3::text IS NULL OR unaccent(ar.name) ILIKE unaccent($3) OR unaccent(al.title) ILIKE unaccent($3))"#,
        f.state, f.recommendation, q
    ).fetch_one(pool).await?;
    let rows = sqlx::query_as!(AlbumListRow,
        r#"SELECT al.id AS album_id, ar.name AS artist, al.title AS album, al.year,
        (SELECT count(*) FROM tracks t WHERE t.album_id=al.id AND t.missing_since IS NULL) AS "track_count!",
        COALESCE(aa.state,'new') AS "state!", aa.recommendation AS "recommendation?", c.distance AS "distance?", c.release_mbid AS "release_mbid?",
        aa.searched_at AS "searched_at?", aa.applied_at AS "applied_at?"
        FROM albums al JOIN artists ar ON ar.id=al.artist_id
        LEFT JOIN autotag_albums aa ON aa.album_id=al.id
        LEFT JOIN autotag_candidates c ON c.id=aa.best_candidate
        WHERE EXISTS (SELECT 1 FROM tracks t WHERE t.album_id=al.id AND t.missing_since IS NULL)
        AND ($1::text IS NULL OR COALESCE(aa.state,'new')=$1)
        AND ($2::text IS NULL OR aa.recommendation=$2)
        AND ($3::text IS NULL OR unaccent(ar.name) ILIKE unaccent($3) OR unaccent(al.title) ILIKE unaccent($3))
        ORDER BY lower(ar.name), lower(al.title), al.year, al.id LIMIT $4 OFFSET $5"#,
        f.state, f.recommendation, q, limit, offset
    ).fetch_all(pool).await?;
    Ok((rows, total))
}

pub struct ReviewState {
    pub state: String,
    pub recommendation: Option<String>,
    pub error: Option<String>,
    pub best_candidate: Option<Uuid>,
    pub chosen_candidate: Option<Uuid>,
}

pub async fn review_state(pool: &PgPool, album: Uuid) -> sqlx::Result<Option<ReviewState>> {
    sqlx::query_as!(ReviewState,"SELECT state,recommendation,error,best_candidate,chosen_candidate FROM autotag_albums WHERE album_id=$1",album).fetch_optional(pool).await
}

pub struct RunListRow {
    pub id: Uuid,
    pub album_id: Uuid,
    pub album: String,
    pub artist: String,
    pub target: String,
    pub reorganize: bool,
    pub state: String,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
    pub summary: Option<serde_json::Value>,
    pub error: Option<String>,
}

pub async fn list_runs(pool: &PgPool, offset: i64, limit: i64) -> sqlx::Result<Vec<RunListRow>> {
    sqlx::query_as!(RunListRow,
        "SELECT r.id,r.album_id,al.title AS album,ar.name AS artist,r.target,r.reorganize,r.state,r.created_at,r.finished_at,r.summary,r.error
        FROM autotag_runs r JOIN albums al ON al.id=r.album_id JOIN artists ar ON ar.id=al.artist_id
        ORDER BY r.created_at DESC,r.id DESC LIMIT $1 OFFSET $2",limit,offset).fetch_all(pool).await
}

pub async fn run_view(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<RunListRow>> {
    sqlx::query_as!(RunListRow,
        "SELECT r.id,r.album_id,al.title AS album,ar.name AS artist,r.target,r.reorganize,r.state,r.created_at,r.finished_at,r.summary,r.error
        FROM autotag_runs r JOIN albums al ON al.id=r.album_id JOIN artists ar ON ar.id=al.artist_id WHERE r.id=$1",id).fetch_optional(pool).await
}

pub async fn run_rows(
    pool: &PgPool,
    id: Uuid,
    offset: i64,
    limit: i64,
) -> sqlx::Result<(Vec<JournalRow>, i64)> {
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM autotag_journal WHERE run_id=$1"#,
        id
    )
    .fetch_one(pool)
    .await?;
    let rows=sqlx::query_as!(JournalRow,
        "SELECT id,run_id,track_id,root,path,before_sha,after_sha,audio_md5,old_tags,new_tags,id3_version,state,note,applied_seq
        FROM autotag_journal WHERE run_id=$1 ORDER BY id LIMIT $2 OFFSET $3",id,limit,offset).fetch_all(pool).await?;
    Ok((rows, total))
}
