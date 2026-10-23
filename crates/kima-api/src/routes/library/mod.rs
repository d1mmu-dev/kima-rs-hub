//! The library browse API: artists, albums, tracks, lyrics, shuffle, recently added, scan.
//!
//! Responses carry the keys the forked frontend reads. Since M3 they carry what identification
//! and the MetadataEditor stored (MusicBrainz ids, bios, images, genres, overrides; spec §16.1),
//! read from the database only: a page view never makes a request (spec §12.8). Fields with no
//! data yet (play counts, top tracks) are present as null, empty or 0.

use axum::Router;
use kima_db::browse::{AlbumRow, TrackRow};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;

pub(super) mod albums;
pub(super) mod artists;
mod attention;
mod genres;
mod lyrics;
mod organizer;
mod scan;
mod stream;
mod tracks;

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(artists::routes())
        .merge(albums::routes())
        .merge(tracks::routes())
        .merge(scan::routes())
        .merge(attention::routes())
        .merge(genres::routes())
        .merge(lyrics::routes())
        .merge(organizer::routes())
        .merge(stream::routes())
}

/// Paging as upstream's `library/paging.ts`: a bad or absent number takes the default, a page
/// is never larger than 10000.
const MAX_LIMIT: i64 = 10_000;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Paging {
    limit: Option<String>,
    offset: Option<String>,
}

impl Paging {
    pub(super) fn limit(&self, fallback: i64) -> i64 {
        let parsed = self.limit.as_deref().and_then(parse_int).unwrap_or(0);
        (if parsed > 0 { parsed } else { fallback }).min(MAX_LIMIT)
    }

    pub(super) fn offset(&self) -> i64 {
        self.offset
            .as_deref()
            .and_then(parse_int)
            .unwrap_or(0)
            .max(0)
    }
}

/// JavaScript `parseInt`: leading integer, ignoring trailing junk.
pub(super) fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let (sign, digits) = match s.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end]
        .parse::<i64>()
        .ok()
        .map(|n| sign * n)
        .or((end > 0).then_some(i64::MAX))
}

/// The value the frontend hands to `api.getCoverArtUrl(id)`, which builds
/// `/api/library/cover-art/<id>` from a bare id. The album id is that id.
pub(super) fn cover(album: Uuid) -> String {
    album.to_string()
}

pub(super) fn track_json(t: &TrackRow) -> Value {
    let cover = cover(t.album_id);
    json!({
        "id": t.id,
        "title": t.title,
        "duration": t.duration_secs,
        "trackNo": t.track_no,
        "trackNumber": t.track_no,
        "discNumber": t.disc,
        "discSubtitle": null,
        "displayTitle": null,
        "displayTrackNo": null,
        "hasUserOverrides": false,
        "isMissing": false,
        "previewUrl": null,
        "playCount": 0,
        "userPlayCount": 0,
        "listeners": 0,
        "year": t.year,
        "codec": t.codec,
        "bitrate": t.bitrate,
        "audioFeatures": null,
        "recordingMbid": t.recording_mbid,
        "isrc": t.isrc,
        "albumId": t.album_id,
        "artist": { "id": t.album_artist_id, "name": t.artist_name },
        "album": {
            "id": t.album_id,
            "title": t.album_title,
            "year": t.album_year,
            "coverUrl": cover,
            "coverArt": cover,
            "artistId": t.album_artist_id,
            "artist": { "id": t.album_artist_id, "name": t.album_artist_name, "mbid": null },
        },
    })
}

pub(super) fn album_json(a: &AlbumRow) -> Value {
    let cover = cover(a.id);
    json!({
        "id": a.id,
        "title": a.title,
        "year": a.year,
        "coverUrl": cover,
        "coverArt": cover,
        "artistId": a.artist_id,
        "artist": { "id": a.artist_id, "mbid": null, "name": a.artist_name },
        "isCompilation": a.is_compilation,
        "trackCount": a.track_count,
        "songCount": a.track_count,
        "duration": a.duration_secs,
        "playCount": 0,
        "owned": true,
        "type": null,
        "mbid": a.release_mbid,
        "rgMbid": a.release_group_mbid,
        "label": a.label,
        // Effective genres are read for the album page only (`albums::detail`).
        "genres": [],
        "location": "LIBRARY",
        "lastSynced": a.created_at.format(&time::format_description::well_known::Rfc3339).ok(),
        "displayTitle": a.display_title,
        "displayYear": a.display_year,
        "userCoverUrl": null,
        "userGenres": a.user_genres,
        "hasUserOverrides": a.has_user_overrides(),
        "missingTracks": [],
    })
}

/// Groups `tracks` under their album and returns the album JSON with `tracks` filled in.
pub(super) fn album_with_tracks(a: &AlbumRow, tracks: &[TrackRow]) -> Value {
    let mut v = album_json(a);
    let own: Vec<Value> = tracks
        .iter()
        .filter(|t| t.album_id == a.id)
        .map(track_json)
        .collect();
    v["tracks"] = Value::Array(own);
    v
}

/// A path id that is not a UUID can match nothing.
pub(super) fn parse_id(id: &str) -> Option<Uuid> {
    id.parse().ok()
}
