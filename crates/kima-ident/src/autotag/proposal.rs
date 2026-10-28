//! The applied values computed from a candidate's snapshot (spec §19.6) and the proposed-changes
//! diff the review page shows. Pure: no request, no database.

use super::score::Mapping;
use crate::musicbrainz::{Credit, Release, ReleaseGroup, VARIOUS_ARTISTS_MBID, top_genres};
use kima_db::autotag::{Applied, AppliedAlbum, AppliedTrack, LocalAlbum};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

fn line(credits: &[Credit], part: impl Fn(&Credit) -> &str) -> String {
    credits
        .iter()
        .map(|c| format!("{}{}", part(c), c.joinphrase))
        .collect()
}

fn ids(credits: &[Credit]) -> Vec<Uuid> {
    credits.iter().map(|c| c.artist.id.0).collect()
}

fn nonempty(s: &Option<String>) -> Option<String> {
    s.as_ref().filter(|s| !s.is_empty()).cloned()
}

fn year_of(date: &str) -> Option<i32> {
    date.get(..4)?.parse().ok()
}

/// The §19.6 values for `release` (`R`), its `group` (`RG`) and the track `mapping`.
pub fn applied(release: &Release, group: &ReleaseGroup, mapping: &Mapping) -> Applied {
    let date = nonempty(&release.date);
    let original_date = nonempty(&group.first_release_date);
    let year = original_date
        .as_deref()
        .and_then(year_of)
        .or_else(|| date.as_deref().and_then(year_of));
    // Spec §19.6: compilation iff the release credit is the VA MBID. `score::release_is_va` is
    // looser (it also accepts a "Various Artists" name), so a release scored as VA whose credit
    // has another MBID applies `compilation = false`.
    let va = Uuid::parse_str(VARIOUS_ARTISTS_MBID).expect("static VA MBID");
    let compilation = matches!(release.artist_credit.as_slice(), [c] if c.artist.id.0 == va);
    let genres = match top_genres(&group.genres, 3) {
        g if g.is_empty() => top_genres(&release.genres, 3),
        g => g,
    };
    let album = AppliedAlbum {
        title: release.title.clone(),
        artist: line(&release.artist_credit, |c| &c.name),
        artist_sort: line(&release.artist_credit, |c| &c.artist.sort_name),
        artist_mbids: ids(&release.artist_credit),
        release_mbid: release.id.0,
        release_group_mbid: group.id.0,
        date,
        original_date,
        year,
        compilation,
        genres,
        disc_total: release.media.len() as u32,
    };

    let tracks = mapping
        .pairs
        .iter()
        .map(|p| {
            let found = release
                .media
                .iter()
                .find(|m| m.position == p.medium)
                .and_then(|m| {
                    m.tracks
                        .iter()
                        .find(|t| t.position == p.position)
                        .map(|t| (m, t))
                });
            let Some((medium, t)) = found else {
                return unmatched(p.track_id);
            };
            let credits = if t.artist_credit.is_empty() {
                &release.artist_credit
            } else {
                &t.artist_credit
            };
            AppliedTrack {
                track_id: p.track_id,
                matched: true,
                disc: Some(medium.position),
                disc_total: Some(release.media.len() as u32),
                track_no: Some(t.position),
                track_total: Some(medium.track_count),
                title: Some(t.title.clone()),
                artist: Some(line(credits, |c| &c.name)),
                artist_sort: Some(line(credits, |c| &c.artist.sort_name)),
                artist_mbids: ids(credits),
                recording_mbid: Some(t.recording.id.0),
                release_track_mbid: Some(t.id.0),
            }
        })
        .chain(mapping.extra.iter().map(|id| unmatched(*id)))
        .collect();
    Applied {
        album,
        tracks,
        mbids_only: false,
    }
}

fn unmatched(track_id: Uuid) -> AppliedTrack {
    AppliedTrack {
        track_id,
        matched: false,
        disc: None,
        disc_total: None,
        track_no: None,
        track_total: None,
        title: None,
        artist: None,
        artist_sort: None,
        artist_mbids: Vec::new(),
        recording_mbid: None,
        release_track_mbid: None,
    }
}

/// The "write only the MBIDs" form: the MBIDs and the disc/track numbers and totals stay; every
/// name, date, genre and the year and compilation flag are blanked.
///
/// The result is NOT a complete value: it must pass through [`with_local_identity`] before it is
/// stored on a run or applied. Empty/`None` fields mean "leave the existing value alone".
pub fn mbids_only(mut a: Applied) -> Applied {
    a.mbids_only = true;
    a.album.title.clear();
    a.album.artist.clear();
    a.album.artist_sort.clear();
    a.album.date = None;
    a.album.original_date = None;
    a.album.year = None;
    a.album.compilation = false;
    a.album.genres.clear();
    for t in &mut a.tracks {
        t.title = None;
        t.artist = None;
        t.artist_sort = None;
    }
    a
}

/// Fill the blanked names of an [`mbids_only`] value with what the album already has (required
/// before it is stored or applied). Extra tracks stay all-`None`. There is no local sort name,
/// date or genre list, so those stay empty and are not written.
pub fn with_local_identity(a: &mut Applied, local: &LocalAlbum) {
    a.album.title = local.title.clone();
    a.album.artist = local.artist_name.clone();
    a.album.year = local.year;
    a.album.compilation = local.is_compilation;
    for t in &mut a.tracks {
        if !t.matched {
            continue;
        }
        if let Some(l) = local.tracks.iter().find(|l| l.id == t.track_id) {
            t.title = Some(l.title.clone());
            t.artist = Some(l.artist_name.clone());
        }
    }
}

/// One field of the proposed changes. `track_id` is `None` for an album field; `current` is
/// `null` for what the database does not hold.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub track_id: Option<Uuid>,
    pub field: &'static str,
    pub current: Value,
    pub proposed: Value,
}

/// An album field; an empty/`None` proposed value ("do not write") is shown as unchanged.
fn album_change(field: &'static str, current: Value, proposed: Value) -> Change {
    let proposed = match &proposed {
        Value::Null => current.clone(),
        Value::String(s) if s.is_empty() => current.clone(),
        Value::Array(a) if a.is_empty() => current.clone(),
        _ => proposed,
    };
    Change {
        track_id: None,
        field,
        current,
        proposed,
    }
}

/// Every field an apply would set, album first, then each local track in order. A value the
/// apply leaves alone (empty/`None`, an extra track's, or a missing one) is proposed as its
/// current value. `year` is a real change: apply writes it to the files and the album row.
pub fn changes(local: &LocalAlbum, a: &Applied) -> Vec<Change> {
    let al = &a.album;
    let mut out = vec![
        album_change("album", json!(local.title), json!(al.title)),
        album_change("albumArtist", json!(local.artist_name), json!(al.artist)),
        album_change("albumArtistSort", Value::Null, json!(al.artist_sort)),
        album_change("albumArtistMbids", Value::Null, json!(al.artist_mbids)),
        album_change(
            "releaseMbid",
            json!(local.release_mbid),
            json!(al.release_mbid),
        ),
        album_change(
            "releaseGroupMbid",
            json!(local.release_group_mbid),
            json!(al.release_group_mbid),
        ),
        album_change("date", Value::Null, json!(al.date)),
        album_change("originalDate", Value::Null, json!(al.original_date)),
        album_change("year", json!(local.year), json!(al.year)),
        album_change(
            "compilation",
            json!(local.is_compilation),
            json!(al.compilation),
        ),
        album_change("genres", Value::Null, json!(al.genres)),
    ];
    for l in &local.tracks {
        let t = a.tracks.iter().find(|t| t.track_id == l.id);
        let mut push = |field: &'static str, current: Value, proposed: Option<Value>| {
            let proposed = proposed.unwrap_or_else(|| current.clone());
            out.push(Change {
                track_id: Some(l.id),
                field,
                current,
                proposed,
            });
        };
        let opt = |v: Option<Value>| v.filter(|v| !v.is_null());
        push(
            "disc",
            json!(l.disc),
            opt(t.and_then(|t| t.disc).map(|v| json!(v))),
        );
        push(
            "discTotal",
            json!(l.disc_total),
            opt(t.and_then(|t| t.disc_total).map(|v| json!(v))),
        );
        push(
            "trackNo",
            json!(l.track_no),
            opt(t.and_then(|t| t.track_no).map(|v| json!(v))),
        );
        push(
            "trackTotal",
            json!(l.track_total),
            opt(t.and_then(|t| t.track_total).map(|v| json!(v))),
        );
        push(
            "title",
            json!(l.title),
            t.and_then(|t| t.title.as_ref()).map(|v| json!(v)),
        );
        push(
            "artist",
            json!(l.artist_name),
            t.and_then(|t| t.artist.as_ref()).map(|v| json!(v)),
        );
        push(
            "artistSort",
            Value::Null,
            t.and_then(|t| t.artist_sort.as_ref()).map(|v| json!(v)),
        );
        push(
            "artistMbids",
            json!(l.tag_artist_mbids),
            t.filter(|t| t.matched).map(|t| json!(t.artist_mbids)),
        );
        push(
            "recordingMbid",
            json!(l.recording_mbid),
            t.and_then(|t| t.recording_mbid).map(|v| json!(v)),
        );
        push(
            "releaseTrackMbid",
            Value::Null,
            t.and_then(|t| t.release_track_mbid).map(|v| json!(v)),
        );
    }
    out
}

/// The album's release or release group MBID was set by the user: apply keeps it (R2).
pub fn user_mbid_kept(local: &LocalAlbum) -> bool {
    local.release_mbid_source.as_deref() == Some("user")
        || local.release_group_mbid_source.as_deref() == Some("user")
}
