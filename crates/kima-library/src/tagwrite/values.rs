//! The §19.8 fields of one file, container-independent. `None` means "leave this key alone";
//! each container's mapping (`id3::native`, and Task 13's Vorbis and MP4 ones) turns a `Some` into
//! the key's new values.

use kima_db::autotag::{AppliedAlbum, AppliedTrack};
use uuid::Uuid;

/// A number and its optional total (`TRCK`/`TPOS` = `n/t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumberPair {
    pub number: u32,
    pub total: Option<u32>,
}

/// What one file gets. Every field is touched only when `Some`; `compilation: Some(false)`
/// removes the flag, and the vectors are never empty when `Some`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagValues {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artist_sort: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub album_artist_sort: Option<String>,
    pub track: Option<NumberPair>,
    pub disc: Option<NumberPair>,
    /// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`.
    pub date: Option<String>,
    pub original_date: Option<String>,
    pub genres: Option<Vec<String>>,
    pub compilation: Option<bool>,
    pub recording_mbid: Option<Uuid>,
    pub release_track_mbid: Option<Uuid>,
    pub release_mbid: Option<Uuid>,
    pub release_group_mbid: Option<Uuid>,
    pub artist_mbids: Option<Vec<Uuid>>,
    pub album_artist_mbids: Option<Vec<Uuid>>,
}

/// A trimmed, non-empty string, else "not touched": a blank value never clears a field.
fn text(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn opt_text(s: Option<&String>) -> Option<String> {
    s.and_then(|s| text(s))
}

fn ids(v: &[Uuid]) -> Option<Vec<Uuid>> {
    (!v.is_empty()).then(|| v.to_vec())
}

impl TagValues {
    /// The §19.6 values for one track. A matched track gets the album- and track-scope fields;
    /// an extra (unmatched) local track gets the album-scope fields except the MBIDs and keeps its
    /// own title, artist and numbers, so it stays in the same album.
    pub fn from_applied(album: &AppliedAlbum, track: &AppliedTrack) -> TagValues {
        let genres: Vec<String> = album.genres.iter().filter_map(|g| text(g)).collect();
        let mut v = TagValues {
            album: text(&album.title),
            album_artist: text(&album.artist),
            album_artist_sort: text(&album.artist_sort),
            date: opt_text(album.date.as_ref()),
            original_date: opt_text(album.original_date.as_ref()),
            genres: (!genres.is_empty()).then_some(genres),
            compilation: Some(album.compilation),
            ..TagValues::default()
        };
        if !track.matched {
            return v;
        }
        v.release_mbid = Some(album.release_mbid);
        v.release_group_mbid = Some(album.release_group_mbid);
        v.album_artist_mbids = ids(&album.artist_mbids);
        v.title = opt_text(track.title.as_ref());
        v.artist = opt_text(track.artist.as_ref());
        v.artist_sort = opt_text(track.artist_sort.as_ref());
        v.artist_mbids = ids(&track.artist_mbids);
        v.recording_mbid = track.recording_mbid;
        v.release_track_mbid = track.release_track_mbid;
        v.track = track.track_no.map(|n| NumberPair {
            number: n,
            total: track.track_total,
        });
        let disc_total = track
            .disc_total
            .or((album.disc_total > 0).then_some(album.disc_total));
        v.disc = track.disc.map(|n| NumberPair {
            number: n,
            total: disc_total,
        });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album() -> AppliedAlbum {
        AppliedAlbum {
            title: "Album".into(),
            artist: "Artist".into(),
            artist_sort: String::new(),
            artist_mbids: vec![Uuid::from_u128(1)],
            release_mbid: Uuid::from_u128(2),
            release_group_mbid: Uuid::from_u128(3),
            date: Some("2001-02-03".into()),
            original_date: None,
            year: Some(2001),
            compilation: false,
            genres: vec!["rock".into(), " ".into()],
            disc_total: 2,
        }
    }

    fn track(matched: bool) -> AppliedTrack {
        AppliedTrack {
            track_id: Uuid::from_u128(9),
            matched,
            disc: Some(1),
            disc_total: None,
            track_no: Some(4),
            track_total: Some(10),
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            artist_sort: Some(" ".into()),
            artist_mbids: vec![Uuid::from_u128(1)],
            recording_mbid: Some(Uuid::from_u128(5)),
            release_track_mbid: Some(Uuid::from_u128(6)),
        }
    }

    #[test]
    fn matched_track_gets_everything_blank_means_untouched() {
        let v = TagValues::from_applied(&album(), &track(true));
        assert_eq!(v.title.as_deref(), Some("Song"));
        assert_eq!(v.artist_sort, None);
        assert_eq!(v.album_artist_sort, None);
        assert_eq!(v.original_date, None);
        assert_eq!(v.genres, Some(vec!["rock".to_string()]));
        assert_eq!(
            v.disc,
            Some(NumberPair {
                number: 1,
                total: Some(2)
            })
        );
        assert_eq!(v.release_mbid, Some(Uuid::from_u128(2)));
        assert_eq!(v.compilation, Some(false));
    }

    #[test]
    fn extra_track_gets_album_scope_without_mbids() {
        let v = TagValues::from_applied(&album(), &track(false));
        assert_eq!(v.album.as_deref(), Some("Album"));
        assert_eq!(v.date.as_deref(), Some("2001-02-03"));
        assert_eq!((v.title, v.track, v.disc), (None, None, None));
        assert_eq!((v.release_mbid, v.release_group_mbid), (None, None));
        assert_eq!((v.album_artist_mbids, v.artist_mbids), (None, None));
        assert_eq!((v.recording_mbid, v.release_track_mbid), (None, None));
    }
}
