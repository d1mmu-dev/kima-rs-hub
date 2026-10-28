//! The applied values and the proposed changes (spec §19.6), from the hand-written fixtures.

use kima_db::autotag::{Applied, LocalAlbum, LocalTrack};
use kima_ident::autotag::proposal::{
    applied, changes, mbids_only, user_mbid_kept, with_local_identity,
};
use kima_ident::autotag::score::{Mapping, map_tracks};
use kima_ident::musicbrainz::{Release, ReleaseGroup};
use serde_json::{Value, json};
use std::path::PathBuf;
use uuid::Uuid;

const R1: &str = "c0000001-0000-4000-8000-000000000001";
const R2: &str = "c0000002-0000-4000-8000-000000000001";
const R3: &str = "c0000003-0000-4000-8000-000000000001";
const BAND: &str = "b1a9c0e0-0000-4000-8000-00000000f001";
const VA: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

fn entries() -> Vec<Value> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/musicbrainz/autotag.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn body(prefix: &str) -> Value {
    entries()
        .into_iter()
        .find(|e| e["url"].as_str().unwrap().starts_with(prefix))
        .unwrap()["body"]
        .clone()
}

fn release(id: &str) -> Release {
    serde_json::from_value(body(&format!("/release/{id}?"))).unwrap()
}

/// The release group with its genres, as the snapshot stores it.
fn group(r: &Release) -> ReleaseGroup {
    let id = r.release_group.as_ref().unwrap().id;
    serde_json::from_value(body(&format!("/release-group/{id}?"))).unwrap()
}

fn local_track(disc: i32, no: i32, title: &str, artist: &str) -> LocalTrack {
    LocalTrack {
        id: Uuid::from_u128(disc as u128 * 1000 + no as u128),
        path: String::new(),
        disc,
        track_no: no,
        title: title.into(),
        artist_name: artist.into(),
        duration_secs: 2,
        recording_mbid: None,
        recording_source: None,
        tag_release_mbid: None,
        tag_artist_mbids: vec![],
        disc_total: None,
        track_total: None,
    }
}

fn local_album(title: &str, artist: &str, tracks: Vec<LocalTrack>) -> LocalAlbum {
    LocalAlbum {
        id: Uuid::nil(),
        title: title.into(),
        artist_id: Uuid::nil(),
        artist_name: artist.into(),
        year: None,
        is_compilation: false,
        release_mbid: None,
        release_mbid_source: None,
        release_group_mbid: None,
        release_group_mbid_source: None,
        identity_source: "tag".into(),
        tracks,
    }
}

fn twin(n: i32) -> LocalAlbum {
    let tracks = (1..=n)
        .map(|i| local_track(1, i, &format!("Fixture Song {i}"), "Fixture Band"))
        .collect();
    local_album("Fixture Album", "Fixture Band", tracks)
}

fn u(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

fn fixture_applied() -> (LocalAlbum, Applied) {
    let r = release(R1);
    let l = twin(10);
    let a = applied(&r, &group(&r), &map_tracks(&l, &r));
    (l, a)
}

#[test]
fn applied_from_fixture_release() {
    let (l, a) = fixture_applied();
    let al = &a.album;
    assert_eq!(al.title, "Fixture Album");
    assert_eq!(al.artist, "Fixture Band");
    assert_eq!(al.artist_sort, "Band, Fixture");
    assert_eq!(al.artist_mbids, vec![u(BAND)]);
    assert_eq!(al.release_mbid, u(R1));
    assert_eq!(
        al.release_group_mbid,
        u("d0000001-0000-4000-8000-000000000001")
    );
    assert_eq!(al.date.as_deref(), Some("2001-05-01"));
    assert_eq!(al.original_date.as_deref(), Some("2001-05-01"));
    assert_eq!(al.year, Some(2001));
    assert!(!al.compilation);
    assert_eq!(al.genres, vec!["rock", "indie rock", "pop"]);
    assert_eq!(al.disc_total, 1);
    assert!(!a.mbids_only);
    assert_eq!(a.tracks.len(), 10);
    let t = a
        .tracks
        .iter()
        .find(|t| t.track_id == l.tracks[0].id)
        .unwrap();
    assert!(t.matched);
    assert_eq!((t.disc, t.disc_total), (Some(1), Some(1)));
    assert_eq!((t.track_no, t.track_total), (Some(1), Some(10)));
    assert_eq!(t.title.as_deref(), Some("Fixture Song 1"));
    assert_eq!(t.artist.as_deref(), Some("Fixture Band"));
    assert_eq!(t.artist_sort.as_deref(), Some("Band, Fixture"));
    assert_eq!(t.artist_mbids, vec![u(BAND)]);
    assert_eq!(
        t.recording_mbid,
        Some(u("8e010100-0000-4000-8000-000000000001"))
    );
    assert_eq!(
        t.release_track_mbid,
        Some(u("7a010100-0000-4000-8000-000000000001"))
    );
}

#[test]
fn year_follows_original_date_then_release_date() {
    let r = release(R1);
    let mut g = group(&r);
    g.first_release_date = Some("1999-03".into());
    assert_eq!(applied(&r, &g, &Mapping::default()).album.year, Some(1999));
    g.first_release_date = None;
    assert_eq!(applied(&r, &g, &Mapping::default()).album.year, Some(2001));
    assert_eq!(
        applied(&r, &g, &Mapping::default()).album.original_date,
        None
    );
}

#[test]
fn genres_fall_back_to_release_then_empty() {
    let r = release(R1);
    let mut g = group(&r);
    g.genres.clear();
    assert!(applied(&r, &g, &Mapping::default()).album.genres.is_empty());
    let mut r2 = r.clone();
    r2.genres = serde_json::from_value(json!([
        {"name": "a", "count": 1}, {"name": "b", "count": 4},
        {"name": "c", "count": 3}, {"name": "d", "count": 2}
    ]))
    .unwrap();
    assert_eq!(
        applied(&r2, &g, &Mapping::default()).album.genres,
        vec!["b", "c", "d"]
    );
}

#[test]
fn va_release_is_compilation() {
    let r = release(R3);
    let a = applied(&r, &group(&r), &Mapping::default());
    assert!(a.album.compilation);
    assert_eq!(a.album.artist_mbids, vec![u(VA)]);
    assert!(!fixture_applied().1.album.compilation);
}

#[test]
fn track_credit_overrides_release_credit() {
    let r = release(R3);
    // The first tracks of R3 name Alpha Act while the release is Various Artists.
    let mut l = twin(3);
    l.tracks = (1..=3)
        .map(|i| {
            let t = &r.tracks().nth(i - 1).unwrap();
            local_track(1, i as i32, &t.title, "Alpha Act")
        })
        .collect();
    let a = applied(&r, &group(&r), &map_tracks(&l, &r));
    let t = a
        .tracks
        .iter()
        .find(|t| t.matched)
        .expect("a matched track");
    assert_ne!(t.artist, Some(a.album.artist.clone()));
    assert_eq!(t.artist.as_deref(), Some("Alpha Act"));
}

#[test]
fn track_without_credit_uses_release_credit() {
    let mut r = release(R1);
    r.media[0].tracks[0].artist_credit.clear();
    let l = twin(10);
    let a = applied(&r, &group(&r), &map_tracks(&l, &r));
    let t = a
        .tracks
        .iter()
        .find(|t| t.track_id == l.tracks[0].id)
        .unwrap();
    assert_eq!(t.artist.as_deref(), Some("Fixture Band"));
    assert_eq!(t.artist_mbids, vec![u(BAND)]);
}

#[test]
fn joinphrases_are_kept_in_credit_lines() {
    let mut r = release(R1);
    let mut second = r.artist_credit[0].clone();
    second.name = "Guest".into();
    second.artist.sort_name = "Guest, A".into();
    r.artist_credit[0].joinphrase = " feat. ".into();
    r.artist_credit.push(second);
    let a = applied(&r, &group(&r), &Mapping::default());
    assert_eq!(a.album.artist, "Fixture Band feat. Guest");
    assert_eq!(a.album.artist_sort, "Band, Fixture feat. Guest, A");
    assert_eq!(a.album.artist_mbids.len(), 2);
    assert!(!a.album.compilation);
}

#[test]
fn two_media_numbers() {
    let r = release(R2);
    let l = {
        let mut tracks = Vec::new();
        for (d, m) in r.media.iter().enumerate() {
            for t in &m.tracks {
                tracks.push(local_track(
                    d as i32 + 1,
                    t.position as i32,
                    &t.title,
                    "Fixture Band",
                ));
            }
        }
        local_album("Double Fixture", "Fixture Band", tracks)
    };
    let a = applied(&r, &group(&r), &map_tracks(&l, &r));
    assert_eq!(a.album.disc_total, 2);
    let t = a
        .tracks
        .iter()
        .find(|t| t.disc == Some(2))
        .expect("a disc 2 track");
    assert_eq!(t.disc_total, Some(2));
    assert_eq!(t.track_total, Some(r.media[1].track_count));
}

#[test]
fn extra_track_gets_album_fields_only() {
    let r = release(R1);
    let mut l = twin(10);
    l.tracks
        .push(local_track(1, 11, "Bonus Thing", "Fixture Band"));
    let extra = l.tracks[10].id;
    let a = applied(&r, &group(&r), &map_tracks(&l, &r));
    let t = a.tracks.iter().find(|t| t.track_id == extra).unwrap();
    assert!(!t.matched);
    assert!(t.disc.is_none() && t.disc_total.is_none() && t.track_no.is_none());
    assert!(t.track_total.is_none() && t.title.is_none() && t.artist.is_none());
    assert!(t.artist_sort.is_none() && t.artist_mbids.is_empty());
    assert!(t.recording_mbid.is_none() && t.release_track_mbid.is_none());
    // The album-scope values are still the album's.
    assert_eq!(a.album.release_mbid, u(R1));
    // The diff shows it unchanged.
    for c in changes(&l, &a).iter().filter(|c| c.track_id == Some(extra)) {
        assert_eq!(c.current, c.proposed, "{}", c.field);
    }
}

#[test]
fn mbids_only_keeps_local_identity() {
    let (mut l, a) = fixture_applied();
    l.year = Some(1990);
    l.is_compilation = true;
    l.title = "Local Album".into();
    l.artist_name = "Local Band".into();
    l.tracks[0].title = "Local Song".into();
    l.tracks[0].artist_name = "Local Artist".into();
    let full = a.clone();
    let mut m = mbids_only(a);
    assert!(m.mbids_only);
    with_local_identity(&mut m, &l);
    assert_eq!(m.album.title, "Local Album");
    assert_eq!(m.album.artist, "Local Band");
    assert_eq!(m.album.year, Some(1990));
    assert!(m.album.compilation);
    assert!(m.album.date.is_none() && m.album.original_date.is_none());
    assert!(m.album.genres.is_empty());
    assert_eq!(m.album.release_mbid, full.album.release_mbid);
    assert_eq!(m.album.release_group_mbid, full.album.release_group_mbid);
    assert_eq!(m.album.artist_mbids, full.album.artist_mbids);
    assert_eq!(m.album.disc_total, 1);
    let t = &m.tracks[0];
    assert_eq!(t.title.as_deref(), Some("Local Song"));
    assert_eq!(t.artist.as_deref(), Some("Local Artist"));
    assert!(t.artist_sort.is_none());
    assert_eq!(t.track_no, full.tracks[0].track_no);
    assert_eq!(t.track_total, full.tracks[0].track_total);
    assert_eq!(t.recording_mbid, full.tracks[0].recording_mbid);
    assert_eq!(t.release_track_mbid, full.tracks[0].release_track_mbid);
}

#[test]
fn changes_lists_every_field_with_nulls_for_unstored() {
    let (mut l, a) = fixture_applied();
    l.title = "Old".into();
    l.year = Some(2000);
    let cs = changes(&l, &a);
    let album: Vec<_> = cs
        .iter()
        .filter(|c| c.track_id.is_none())
        .map(|c| c.field)
        .collect();
    assert_eq!(
        album,
        [
            "album",
            "albumArtist",
            "albumArtistSort",
            "albumArtistMbids",
            "releaseMbid",
            "releaseGroupMbid",
            "date",
            "originalDate",
            "year",
            "compilation",
            "genres"
        ]
    );
    let first: Vec<_> = cs
        .iter()
        .filter(|c| c.track_id == Some(l.tracks[0].id))
        .map(|c| c.field)
        .collect();
    assert_eq!(
        first,
        [
            "disc",
            "discTotal",
            "trackNo",
            "trackTotal",
            "title",
            "artist",
            "artistSort",
            "artistMbids",
            "recordingMbid",
            "releaseTrackMbid"
        ]
    );
    assert_eq!(cs.len(), 11 + 10 * 10);
    // Track order is the local order.
    let order: Vec<_> = cs
        .iter()
        .filter(|c| c.field == "disc")
        .map(|c| c.track_id.unwrap())
        .collect();
    assert_eq!(order, l.tracks.iter().map(|t| t.id).collect::<Vec<_>>());

    let get = |f: &str| {
        cs.iter()
            .find(|c| c.track_id.is_none() && c.field == f)
            .unwrap()
    };
    assert_eq!(get("album").current, json!("Old"));
    assert_eq!(get("album").proposed, json!("Fixture Album"));
    assert_eq!(get("year").current, json!(2000));
    assert_eq!(get("year").proposed, json!(2001));
    for f in ["albumArtistSort", "date", "originalDate"] {
        assert_eq!(get(f).current, Value::Null, "{f}");
    }
    assert_eq!(get("date").proposed, json!("2001-05-01"));
    assert_eq!(get("releaseMbid").current, Value::Null);
    assert_eq!(get("releaseMbid").proposed, json!(R1));
    let t = |f: &str| {
        cs.iter()
            .find(|c| c.track_id == Some(l.tracks[0].id) && c.field == f)
            .unwrap()
    };
    assert_eq!(t("releaseTrackMbid").current, Value::Null);
    assert_eq!(t("artistSort").current, Value::Null);
    assert_eq!(t("artistSort").proposed, json!("Band, Fixture"));
    assert_eq!(t("trackTotal").current, Value::Null);
    assert_eq!(t("trackTotal").proposed, json!(10));
    assert_eq!(t("trackNo").current, json!(1));
    assert_eq!(t("title").current, json!("Fixture Song 1"));
    let wire = serde_json::to_value(t("title")).unwrap();
    assert_eq!(wire["trackId"], json!(l.tracks[0].id));
    assert_eq!(wire["field"], "title");
}

#[test]
fn user_mbid_is_kept() {
    let mut l = twin(1);
    assert!(!user_mbid_kept(&l));
    l.release_mbid_source = Some("autotag".into());
    assert!(!user_mbid_kept(&l));
    l.release_group_mbid_source = Some("user".into());
    assert!(user_mbid_kept(&l));
    l.release_group_mbid_source = None;
    l.release_mbid_source = Some("user".into());
    assert!(user_mbid_kept(&l));
}

#[test]
fn applied_round_trips_through_json() {
    let (_, a) = fixture_applied();
    let v = serde_json::to_value(&a).unwrap();
    assert_eq!(v["mbidsOnly"], json!(false));
    assert_eq!(v["album"]["artistSort"], json!("Band, Fixture"));
    assert_eq!(v["album"]["discTotal"], json!(1));
    assert_eq!(
        v["tracks"][0]["releaseTrackMbid"].as_str().unwrap().len(),
        36
    );
    let back: Applied = serde_json::from_value(v).unwrap();
    assert_eq!(back, a);
    let s = serde_json::to_string(&mbids_only(a.clone())).unwrap();
    let back: Applied = serde_json::from_str(&s).unwrap();
    assert!(back.mbids_only);
}

#[test]
fn with_local_identity_leaves_extra_tracks_alone() {
    let r = release(R1);
    let mut l = twin(10);
    l.tracks.push(local_track(1, 11, "Bonus", "Fixture Band"));
    let extra = l.tracks[10].id;
    let mut m = mbids_only(applied(&r, &group(&r), &map_tracks(&l, &r)));
    with_local_identity(&mut m, &l);
    let t = m.tracks.iter().find(|t| t.track_id == extra).unwrap();
    assert!(!t.matched && t.title.is_none() && t.artist.is_none());
}

#[test]
fn blank_values_are_unchanged_in_changes() {
    let (mut l, a) = fixture_applied();
    l.year = Some(1990);
    l.title = "Local".into();
    let mut m = mbids_only(a.clone());
    with_local_identity(&mut m, &l);
    let cs = changes(&l, &m);
    let get = |f: &str| {
        cs.iter()
            .find(|c| c.track_id.is_none() && c.field == f)
            .unwrap()
    };
    for f in [
        "albumArtistSort",
        "date",
        "originalDate",
        "genres",
        "year",
        "album",
    ] {
        assert_eq!(get(f).current, get(f).proposed, "{f}");
    }
    assert_eq!(get("releaseMbid").proposed, json!(R1));
    // Before with_local_identity the blank names are unchanged too.
    let blank = changes(&l, &mbids_only(a.clone()));
    let b = |f: &str| {
        blank
            .iter()
            .find(|c| c.track_id.is_none() && c.field == f)
            .unwrap()
    };
    assert_eq!(b("album").proposed, json!("Local"));
    // A normal apply without a year leaves the year alone.
    let mut a2 = a;
    a2.album.year = None;
    let y = changes(&l, &a2)
        .into_iter()
        .find(|c| c.field == "year")
        .unwrap();
    assert_eq!(y.proposed, json!(1990));
}
