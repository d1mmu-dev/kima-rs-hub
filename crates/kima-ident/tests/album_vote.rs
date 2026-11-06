//! The album vote (M-SYNC Task 13): a port of upstream's `albumIdentification.vote.test.ts` with
//! hand-built AcoustID answers (upstream's recorded album fixture holds a real library's file
//! names and fingerprints, so it is not copied), then the round against the database.

mod common;

use kima_ident::Mbid;
use kima_ident::acoustid::AcoustidGroup;
use kima_ident::album_vote::{AlbumSongAnswer, AlbumVerdict, NoVote, SongPick, vote};
use serde_json::json;

fn rg(n: u32) -> Mbid {
    Mbid::parse(&format!("00000000-0000-4000-8000-{n:012}")).unwrap()
}

fn group(n: u32, title: &str) -> AcoustidGroup {
    AcoustidGroup {
        id: rg(n),
        title: title.into(),
    }
}

fn answer(title_agrees: bool, groups: &[(u32, &str)]) -> AlbumSongAnswer {
    AlbumSongAnswer {
        pick: Some(SongPick {
            title_agrees,
            release_groups: groups.iter().map(|(n, t)| group(*n, t)).collect(),
        }),
    }
}

fn none() -> AlbumSongAnswer {
    AlbumSongAnswer { pick: None }
}

fn reason(v: &AlbumVerdict) -> NoVote {
    match v {
        AlbumVerdict::None { reason, .. } => *reason,
        other => panic!("expected no verdict, got {other:?}"),
    }
}

const ALBUM: u32 = 9;

/// 13 songs: AcoustID lists a compilation or soundtrack beside the album for most of them, a
/// different one each time; the album is on every one but two.
fn dangerous_days() -> Vec<AlbumSongAnswer> {
    (0..13)
        .map(|i| match i {
            11 => answer(true, &[(100 + i, "Synthwave Hits")]),
            12 => none(),
            _ => answer(
                true,
                &[
                    (100 + i, "Synthwave Hits"),
                    (ALBUM, "Dangerous Days"),
                    (200 + i, "OST"),
                ],
            ),
        })
        .collect()
}

#[test]
fn names_the_album_most_of_its_songs_agree_on_not_the_compilations_listed_first() {
    let v = vote("Dangerous Days", &dangerous_days());
    match v {
        AlbumVerdict::Assign {
            release_group,
            title,
            votes,
            songs,
        } => {
            assert_eq!(
                (release_group, title.as_str(), songs),
                (rg(ALBUM), "Dangerous Days", 13)
            );
            assert_eq!(votes, 11);
            assert!(votes * 2 > songs);
        }
        other => panic!("expected an album, got {other:?}"),
    }
}

#[test]
fn edition_words_aside_live_kept_apart() {
    // An edition of the album is the same album...
    let deluxe = vote("Dangerous Days (Deluxe Edition)", &dangerous_days());
    assert!(
        matches!(deluxe, AlbumVerdict::Assign { release_group, .. } if release_group == rg(ALBUM))
    );
    // ...a live album is not the studio album.
    let live = vote("Dangerous Days (Live)", &dangerous_days());
    assert_eq!(reason(&live), NoVote::NoAgreeingAlbum);
    // And the other way round: AcoustID lists a deluxe edition, the files are the plain album.
    let listed_deluxe = [answer(true, &[(1, "Bliss (Deluxe Edition)")])];
    assert!(matches!(
        vote("Bliss", &listed_deluxe),
        AlbumVerdict::Assign { .. }
    ));
}

#[test]
fn too_few_songs_known_no_verdict() {
    let known: Vec<_> = dangerous_days().into_iter().take(4).collect();
    let mut songs = known;
    songs.extend((0..9).map(|_| none()));
    let v = vote("Dangerous Days", &songs);
    assert_eq!(reason(&v), NoVote::ShortOfMajority);
    assert!(matches!(
        v,
        AlbumVerdict::None {
            votes: 4,
            songs: 13,
            ..
        }
    ));
}

#[test]
fn generic_title_is_never_decided() {
    let v = vote("Greatest Hits", &[answer(true, &[(1, "Greatest Hits")])]);
    assert_eq!(reason(&v), NoVote::GenericTitle);
}

#[test]
fn length_only_pick_does_not_vote() {
    let v = vote("Album", &[answer(false, &[(1, "Album")])]);
    assert_eq!(reason(&v), NoVote::NoAgreeingAlbum);
}

#[test]
fn a_one_song_album_needs_its_one_song() {
    let v = vote("Virga", &[answer(true, &[(1, "Virga")])]);
    assert!(matches!(
        v,
        AlbumVerdict::Assign { release_group, votes: 1, songs: 1, .. } if release_group == rg(1)
    ));
}

#[test]
fn winner_needs_more_than_half() {
    // Half the songs is not more than half.
    let yes = answer(true, &[(1, "Album")]);
    assert_eq!(
        reason(&vote("Album", &[yes.clone(), none()])),
        NoVote::ShortOfMajority
    );
    // Three of five is.
    let songs = [yes.clone(), yes.clone(), yes, none(), none()];
    assert!(matches!(
        vote("Album", &songs),
        AlbumVerdict::Assign {
            votes: 3,
            songs: 5,
            ..
        }
    ));
}

#[test]
fn tie_goes_only_to_the_exact_title() {
    // Two albums with the same songs: the one whose title is exactly this album's wins.
    let both = answer(true, &[(1, "Bliss"), (2, "Bliss (Deluxe Edition)")]);
    let v = vote("Bliss", &[both.clone(), both.clone()]);
    assert!(matches!(v, AlbumVerdict::Assign { release_group, .. } if release_group == rg(1)));
    let v = vote("Bliss (Deluxe Edition)", &[both.clone(), both]);
    assert!(matches!(v, AlbumVerdict::Assign { release_group, .. } if release_group == rg(2)));
    // Two the title cannot tell apart: no verdict, both named.
    let same = answer(true, &[(1, "Be Mine Tonight"), (2, "Be Mine Tonight")]);
    let v = vote("Be Mine Tonight", &[same.clone(), same]);
    assert_eq!(reason(&v), NoVote::Tie);
    assert!(matches!(&v, AlbumVerdict::None { leaders, .. } if leaders == &[rg(1), rg(2)]));
}

#[test]
fn a_song_votes_once_for_an_album_listed_twice() {
    // The same album listed on two recordings of the song is still one vote.
    let song = answer(true, &[(1, "Album"), (2, "Other"), (1, "Album")]);
    let v = vote("Album", &[song, none()]);
    assert_eq!(reason(&v), NoVote::ShortOfMajority);
    assert!(matches!(v, AlbumVerdict::None { votes: 1, .. }));
}

#[test]
fn the_verdict_is_kept_as_json() {
    let v = vote("Virga", &[answer(true, &[(1, "Virga")])]);
    assert_eq!(
        v.to_json(),
        json!({"kind": "assign", "releaseGroup": rg(1), "title": "Virga", "votes": 1, "songs": 1})
    );
    let v = vote("Greatest Hits", &[]);
    assert_eq!(
        v.to_json(),
        json!({"kind": "none", "reason": "generic-title", "votes": 0, "songs": 0, "leaders": []})
    );
}
