//! The "needs attention" report (spec §4.5): per album, what the scanner had to guess.
//! Tracks flagged missing are left out.

use std::collections::{BTreeSet, HashMap, HashSet};

use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::library::Guessed;

pub const ARTIST_FROM_PATH: &str = "artistFromPath";
pub const ALBUM_FROM_PATH: &str = "albumFromPath";
pub const UNKNOWN_ARTIST: &str = "unknownArtist";
pub const UNKNOWN_ALBUM: &str = "unknownAlbum";
pub const MISSING_YEAR: &str = "missingYear";
pub const CONFLICTING_YEARS: &str = "conflictingYears";
pub const MISSING_TRACK_NUMBERS: &str = "missingTrackNumbers";
pub const DUPLICATE_TRACK_NUMBERS: &str = "duplicateTrackNumbers";
pub const FOLDER_SPLIT: &str = "folderSplit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumAttention {
    pub album_id: Uuid,
    pub artist: String,
    pub album: String,
    pub year: Option<i32>,
    pub issues: Vec<&'static str>,
}

#[derive(FromRow)]
struct Row {
    album_id: Uuid,
    artist: String,
    album: String,
    album_year: Option<i32>,
    track_year: Option<i32>,
    disc: i32,
    track_no: i32,
    path: String,
    guessed: sqlx::types::Json<Guessed>,
}

/// Albums with at least one issue, ordered by artist, album, year, id. Issues within an album
/// keep the declaration order of the constants above.
pub async fn report(pool: &PgPool) -> sqlx::Result<Vec<AlbumAttention>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT t.album_id, ar.name AS artist, al.title AS album, al.year AS album_year,
                t.year AS track_year, t.disc, t.track_no, t.path, t.guessed
         FROM tracks t
         JOIN albums al ON al.id = t.album_id
         JOIN artists ar ON ar.id = al.artist_id
         WHERE t.missing_since IS NULL
         ORDER BY t.album_id, t.path",
    )
    .fetch_all(pool)
    .await?;
    Ok(analyse(rows))
}

fn analyse(rows: Vec<Row>) -> Vec<AlbumAttention> {
    // Albums per folder, to spot a folder whose tracks land in several albums.
    let mut folders: HashMap<&str, HashSet<Uuid>> = HashMap::new();
    for r in &rows {
        let dir = r.path.rfind('/').map_or("", |i| &r.path[..i]);
        folders.entry(dir).or_default().insert(r.album_id);
    }
    let split: HashSet<Uuid> = folders
        .values()
        .filter(|a| a.len() > 1)
        .flatten()
        .copied()
        .collect();

    let mut by_album: Vec<(Uuid, Vec<&Row>)> = Vec::new();
    for r in &rows {
        match by_album.last_mut() {
            Some((id, v)) if *id == r.album_id => v.push(r),
            _ => by_album.push((r.album_id, vec![r])),
        }
    }

    let mut out = Vec::new();
    for (album_id, tracks) in by_album {
        let any = |f: fn(&Guessed) -> bool| tracks.iter().any(|t| f(&t.guessed.0));
        let first = tracks[0];
        let years: BTreeSet<i32> = tracks.iter().filter_map(|t| t.track_year).collect();
        let mut seen = HashSet::new();
        let duplicate = tracks
            .iter()
            .filter(|t| t.track_no > 0)
            .any(|t| !seen.insert((t.disc, t.track_no)));

        let checks = [
            (ARTIST_FROM_PATH, any(|g| g.artist_from_path)),
            (ALBUM_FROM_PATH, any(|g| g.album_from_path)),
            (UNKNOWN_ARTIST, any(|g| g.unknown_artist)),
            (UNKNOWN_ALBUM, any(|g| g.unknown_album)),
            (MISSING_YEAR, first.album_year.is_none()),
            (
                CONFLICTING_YEARS,
                any(|g| g.year_conflict) || years.len() > 1,
            ),
            (
                MISSING_TRACK_NUMBERS,
                tracks.iter().any(|t| t.track_no <= 0),
            ),
            (DUPLICATE_TRACK_NUMBERS, duplicate),
            (FOLDER_SPLIT, split.contains(&album_id)),
        ];
        let issues: Vec<&'static str> = checks
            .into_iter()
            .filter_map(|(code, hit)| hit.then_some(code))
            .collect();
        if !issues.is_empty() {
            out.push(AlbumAttention {
                album_id,
                artist: first.artist.clone(),
                album: first.album.clone(),
                year: first.album_year,
                issues,
            });
        }
    }
    out.sort_by(|a, b| {
        (&a.artist, &a.album, a.year, a.album_id).cmp(&(&b.artist, &b.album, b.year, b.album_id))
    });
    out
}
