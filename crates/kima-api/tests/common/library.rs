//! Seeds library rows through `upsert_track`; no audio files are needed.

use kima_db::PgPool;
use kima_db::library::{self, Guessed, ResolvedTrack};
use time::OffsetDateTime;
use uuid::Uuid;

pub struct Seed<'a> {
    pub artist: &'a str,
    pub album: &'a str,
    pub year: Option<i32>,
    pub title: &'a str,
    pub disc: i32,
    pub track_no: i32,
}

pub struct Seeded {
    pub track: Uuid,
    pub album: Uuid,
    pub artist: Uuid,
}

impl<'a> Seed<'a> {
    pub fn new(artist: &'a str, album: &'a str, year: Option<i32>, title: &'a str) -> Self {
        Self {
            artist,
            album,
            year,
            title,
            disc: 1,
            track_no: 1,
        }
    }

    pub fn at(mut self, disc: i32, track_no: i32) -> Self {
        self.disc = disc;
        self.track_no = track_no;
        self
    }
}

/// Inserts one track (path `<artist>/<album>/<disc>-<no> <title>.mp3`).
pub async fn seed(pool: &PgPool, s: Seed<'_>) -> Seeded {
    let mut tx = pool.begin().await.unwrap();
    let track = library::upsert_track(
        &mut tx,
        &ResolvedTrack {
            album_artist: s.artist.into(),
            album_artist_key: s.artist.to_lowercase(),
            track_artist: s.artist.into(),
            album: s.album.into(),
            album_key: s.album.to_lowercase(),
            album_year: s.year,
            track_year: s.year,
            is_compilation: false,
            disc: s.disc,
            track_no: s.track_no,
            title: s.title.into(),
            duration_secs: 180,
            codec: "mp3".into(),
            bitrate: Some(320),
            path: format!(
                "{}/{}/{}-{} {}.mp3",
                s.artist, s.album, s.disc, s.track_no, s.title
            ),
            size: 1,
            mtime: OffsetDateTime::now_utc(),
            fingerprint: [1; 32],
            guessed: Guessed::default(),
            ids: Default::default(),
            isrc: None,
            disc_total: None,
            track_total: None,
            artist_mbid_candidate: None,
        },
    )
    .await
    .unwrap()
    .track;
    tx.commit().await.unwrap();
    let (album, artist): (Uuid, Uuid) = sqlx::query_as(
        "SELECT t.album_id, al.artist_id FROM tracks t JOIN albums al ON al.id = t.album_id
         WHERE t.id = $1",
    )
    .bind(track)
    .fetch_one(pool)
    .await
    .unwrap();
    Seeded {
        track,
        album,
        artist,
    }
}

pub async fn mark_missing(pool: &PgPool, track: Uuid) {
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(track)
        .execute(pool)
        .await
        .unwrap();
}
