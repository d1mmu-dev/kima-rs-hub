//! One mapper per Subsonic shape (spec §6.2). Every endpoint builds its songs, albums and artists
//! here, so a field changes in one place.

use std::path::Path;

use kima_db::browse::TrackRow;
use kima_db::playlists::PlaylistSummary;
use kima_db::subsonic::{AlbumItem, ArtistItem, SongItem};
use kima_db::users::{ROLE_ADMIN, User};
use time::OffsetDateTime;

use crate::response::{Node, Value};

/// What `getArtists` advertises: upstream's `LEADING_ARTICLES`, capitalised. The generated
/// `sort_name` columns drop the same words (D-081).
pub const IGNORED_ARTICLES: &str = "The A An Le La Les Los Las Die Der Das";

/// RFC 3339 in UTC with milliseconds: `2026-01-01T00:01:00.000Z`.
pub fn iso(at: OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    )
}

/// The bucket letter for an artist: the first character of the sort name when it is A-Z (either
/// case), else `#`.
pub fn index_letter(sort_name: &str) -> String {
    match sort_name.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase().to_string(),
        _ => "#".to_owned(),
    }
}

/// The file extension, lower-cased; `mp3` when the path has none (what `mime_for` assumes too).
pub fn suffix(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "mp3".to_owned())
}

/// `tracks.bitrate` (kbit/s) when the scanner found one, else upstream's
/// `estimateBitrateFromMime` by file type.
pub fn bitrate(track: &TrackRow, suffix: &str) -> i64 {
    if let Some(bitrate) = track.bitrate.filter(|&b| b > 0) {
        return i64::from(bitrate);
    }
    let by = |s: &str| match s {
        "flac" => Some(900),
        "wav" => Some(1400),
        "aac" | "m4a" | "mp4" => Some(256),
        "ogg" | "oga" => Some(192),
        "opus" => Some(128),
        _ => None,
    };
    by(suffix)
        .or_else(|| by(&track.codec.to_lowercase()))
        .unwrap_or(192)
}

fn with_state(node: Node, starred_at: Option<OffsetDateTime>, rating: Option<i16>) -> Node {
    node.attr_opt("starred", starred_at.map(iso))
        .attr_opt("userRating", rating.map(i32::from))
}

/// A song as a `Child`. Endpoints that need another element name call `.renamed(..)`.
pub fn song(item: &SongItem) -> Node {
    let t = &item.track;
    let suffix = suffix(&item.path);
    let year = t.year.or(t.album_year).filter(|&y| y > 0);
    let node = Node::new("song")
        .attr("id", t.id.to_string())
        .attr("parent", t.album_id.to_string())
        .attr("title", &t.title)
        .attr("album", &t.album_title)
        .attr("artist", &t.artist_name)
        .attr("isDir", false)
        .attr("coverArt", t.album_id.to_string())
        .attr("duration", t.duration_secs)
        .attr("bitRate", bitrate(t, &suffix))
        .attr_opt("track", Some(t.track_no).filter(|&n| n > 0))
        .attr("discNumber", t.disc)
        .attr_opt("year", year)
        .attr("size", item.size)
        .attr(
            "contentType",
            kima_api::media::mime_for(Path::new(&item.path)),
        )
        .attr("suffix", suffix)
        .attr("albumId", t.album_id.to_string())
        .attr("artistId", t.album_artist_id.to_string())
        .attr("type", "music")
        .attr("created", iso(item.created_at));
    with_state(node, item.starred_at, item.rating)
}

fn album_fields(node: Node, a: &AlbumItem) -> Node {
    let node = node
        .attr("id", a.id.to_string())
        .attr("name", &a.title)
        .attr("artist", &a.artist_name)
        .attr("artistId", a.artist_id.to_string())
        .attr("coverArt", a.id.to_string())
        .attr("songCount", a.song_count)
        .attr("duration", a.duration_secs)
        .attr_opt("year", a.year.filter(|&y| y > 0))
        .attr("created", iso(a.created_at));
    with_state(node, a.starred_at, a.rating)
}

/// An album as `AlbumID3`.
pub fn album_id3(a: &AlbumItem) -> Node {
    album_fields(Node::new("album"), a)
}

/// An album as a folder `Child`: `isDir`, `title` and `parent` (the artist) in front of the ID3
/// fields (D-083).
pub fn album_child(a: &AlbumItem) -> Node {
    album_fields(
        Node::new("album")
            .attr("isDir", true)
            .attr("title", &a.title)
            .attr("album", &a.title)
            .attr("parent", a.artist_id.to_string()),
        a,
    )
}

/// An artist as `ArtistID3`.
pub fn artist_id3(a: &ArtistItem) -> Node {
    let node = Node::new("artist")
        .attr("id", a.id.to_string())
        .attr("name", &a.name)
        .attr("albumCount", a.album_count)
        .attr("coverArt", format!("ar-{}", a.id));
    with_state(node, a.starred_at, a.rating)
}

/// An artist as a folder `Child` in the music-folder directory.
pub fn artist_dir(a: &ArtistItem) -> Node {
    let node = Node::new("child")
        .attr("id", a.id.to_string())
        .attr("parent", "1")
        .attr("isDir", true)
        .attr("title", &a.name)
        .attr("artist", &a.name)
        .attr("coverArt", format!("ar-{}", a.id));
    with_state(node, a.starred_at, a.rating)
}

/// A playlist header: `id name songCount duration public owner created changed`. The caller adds
/// the `entry` songs where the endpoint sends them.
pub fn playlist(p: &PlaylistSummary, duration_secs: i64) -> Node {
    Node::new("playlist")
        .attr("id", p.playlist.id.to_string())
        .attr("name", &p.playlist.name)
        .attr("songCount", p.track_count)
        .attr("duration", duration_secs)
        .attr("public", p.playlist.is_public)
        .attr("owner", &p.username)
        .attr("created", iso(p.playlist.created_at))
        .attr("changed", iso(p.playlist.updated_at))
}

/// A user as upstream's `mapSubsonicUser`: `adminRole` from the role, the stream, download,
/// playlist, settings and scrobbling flags on, the rest off, and the one music folder.
pub fn user(u: &User) -> Node {
    Node::new("user")
        .attr("username", &u.username)
        .attr("scrobblingEnabled", true)
        .attr("adminRole", u.role == ROLE_ADMIN)
        .attr("settingsRole", true)
        .attr("downloadRole", true)
        .attr("uploadRole", false)
        .attr("playlistRole", true)
        .attr("coverArtRole", false)
        .attr("commentRole", false)
        .attr("podcastRole", false)
        .attr("streamRole", true)
        .attr("jukeboxRole", false)
        .attr("shareRole", false)
        .scalars("folder", vec![Value::Int(1)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::{write_json, write_xml};
    use serde_json::json;
    use uuid::Uuid;

    fn at(secs: i64, ms: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(secs).unwrap() + time::Duration::milliseconds(ms)
    }

    fn track(bitrate: Option<i32>, codec: &str) -> TrackRow {
        TrackRow {
            id: Uuid::from_u128(3),
            title: "Come Together".into(),
            artist_name: "Track Artist".into(),
            duration_secs: 259,
            disc: 2,
            track_no: 7,
            codec: codec.into(),
            bitrate,
            year: None,
            album_id: Uuid::from_u128(2),
            album_title: "Abbey Road".into(),
            album_year: Some(1969),
            album_artist_id: Uuid::from_u128(1),
            album_artist_name: "The Beatles".into(),
        }
    }

    fn item() -> SongItem {
        SongItem {
            track: track(Some(320), "mp3"),
            path: "The Beatles/Abbey Road/07 Come Together.FLAC".into(),
            size: 12345,
            created_at: at(1_767_225_660, 500),
            starred_at: Some(at(1_767_240_240, 0)),
            rating: Some(4),
        }
    }

    #[test]
    fn song_fields() {
        let json = write_json(&song(&item()));
        assert_eq!(
            json,
            json!({
                "id": Uuid::from_u128(3).to_string(),
                "parent": Uuid::from_u128(2).to_string(),
                "title": "Come Together",
                "album": "Abbey Road",
                "artist": "Track Artist",
                "isDir": false,
                "coverArt": Uuid::from_u128(2).to_string(),
                "duration": 259,
                "bitRate": 320,
                "track": 7,
                "discNumber": 2,
                "year": 1969,
                "size": 12345,
                "contentType": "audio/flac",
                "suffix": "flac",
                "albumId": Uuid::from_u128(2).to_string(),
                // The album artist, since kima-rs has rows only for album artists.
                "artistId": Uuid::from_u128(1).to_string(),
                "type": "music",
                "created": "2026-01-01T00:01:00.500Z",
                "starred": "2026-01-01T04:04:00.000Z",
                "userRating": 4,
            })
        );
        // No path and no genre, ever.
        let xml = write_xml(&song(&item()));
        assert!(!xml.contains("path=") && !xml.contains("genre="));
        // The track's own year wins; unstarred and unrated songs carry neither attribute.
        let mut plain = item();
        plain.track.year = Some(1970);
        plain.starred_at = None;
        plain.rating = None;
        let json = write_json(&song(&plain));
        assert_eq!(json["year"], 1970);
        assert!(json.get("starred").is_none() && json.get("userRating").is_none());
        assert_eq!(write_json(&song(&plain).renamed("entry")), json);
    }

    #[test]
    fn bitrate_falls_back_by_codec() {
        assert_eq!(bitrate(&track(Some(320), "mp3"), "mp3"), 320);
        assert_eq!(bitrate(&track(None, "flac"), "flac"), 900);
        assert_eq!(bitrate(&track(None, "wav"), "wav"), 1400);
        assert_eq!(bitrate(&track(None, "aac"), "m4a"), 256);
        assert_eq!(bitrate(&track(None, "vorbis"), "ogg"), 192);
        assert_eq!(bitrate(&track(None, "opus"), "opus"), 128);
        assert_eq!(bitrate(&track(None, "mp3"), "mp3"), 192);
        assert_eq!(bitrate(&track(Some(0), "flac"), "flac"), 900);
        // An unusual suffix falls back to the codec name, then to 192.
        assert_eq!(bitrate(&track(None, "flac"), "bin"), 900);
        assert_eq!(bitrate(&track(None, "unknown"), "bin"), 192);
    }

    #[test]
    fn index_letter_cases() {
        assert_eq!(index_letter("beatles"), "B");
        assert_eq!(index_letter("Roux"), "R");
        assert_eq!(index_letter("1975"), "#");
        assert_eq!(index_letter("émile"), "#");
        assert_eq!(index_letter(""), "#");
    }

    #[test]
    fn iso_has_milliseconds_and_z() {
        assert_eq!(
            iso(at(1_772_593_567, 89).to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap())),
            "2026-03-04T03:06:07.089Z"
        );
    }
}
