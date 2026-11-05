//! One artist per album, elected from all its songs (upstream 1.12 `albumArtistVote.ts`,
//! D-164).
//!
//! Each file names a candidate: its album artist, else its track artist, else the folder. Songs
//! of one album folder (a disc subfolder counts as its parent) under one album-tag key are one
//! album. The most common candidate (by identity key; a tie goes to the one named first, so the
//! caller passes the songs in path order) wins when it covers every song, or more than one song
//! and at least a quarter of them (beets' rule). Otherwise each song keeps its own artist,
//! except a compilation (see [`elect`]). Pure code: no database, no files.

use crate::names::{self, VARIOUS_ARTISTS, canonical_various};
use crate::resolve::{ArtistFrom, Resolved, extract_artist_from_relative_path, parse_credit};
use crate::tags::RawTags;
use uuid::Uuid;

/// The share of an album's songs its most common artist must cover (beets'
/// `SINGLE_ARTIST_THRESH`).
pub const LEAD_SHARE: f64 = 0.25;

/// An artist a file (or an election) names, with the MusicBrainz id its tags pair with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub mbid: Option<Uuid>,
}

/// What one file says about its album's artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credit {
    /// The songs that are one album: album folder plus album-tag key. A song with no album tag
    /// is its own group.
    pub group: String,
    /// Identity key of the album tag (`None`: the song has no album tag and is its own album).
    pub album_key: Option<String>,
    pub candidate: Candidate,
    /// The candidate came from an album-artist tag.
    pub from_album_artist: bool,
    /// The file is tagged a compilation (never under `Singles/`, where the folder names the
    /// artist).
    pub compilation: bool,
}

/// The group of the songs that are one album: the album folder plus the album tag's identity key.
pub fn group_of(folder: &str, album_key: &str) -> String {
    format!("{folder}\n{album_key}")
}

/// A disc subfolder name: `CD 1`, `Disc 2`, `Disk03`, `Digital Media 02`, `Vinyl 1`, `Side 2`
/// (case-insensitive; a prefix, optional `[ ._-]*`, then digits ending at a word boundary).
fn is_disc_folder(name: &str) -> bool {
    let lower = name.to_lowercase();
    let Some(rest) = ["cd", "disc", "disk", "digital media", "vinyl", "side"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
    else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t', '.', '_', '-']);
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    // `\d+\b`: the run of digits must not be followed by a letter, digit or underscore.
    digits > 0
        && rest[digits..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
}

/// The album folder of a relative path: the file's own folder, or the one above it when its own
/// is a disc subfolder. `""` is the library root.
pub fn album_folder(rel: &str) -> &str {
    let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
    let (parent, name) = dir.rsplit_once('/').map_or(("", dir), |(p, n)| (p, n));
    if !dir.is_empty() && is_disc_folder(name) {
        parent
    } else {
        dir
    }
}

/// `A (feat. B)` is `A`: a parenthesised or bracketed featured clause at the end of a name does
/// not make a second artist.
pub fn without_featured_clause(name: &str) -> &str {
    let t = name.trim_end();
    let Some(open) = t
        .strip_suffix(')')
        .map(|s| (s, '('))
        .or_else(|| t.strip_suffix(']').map(|s| (s, '[')))
        .and_then(|(s, o)| s.rfind(o))
    else {
        return name;
    };
    let inner = t[open + 1..t.len() - 1].trim_start().to_lowercase();
    let is_feat = ["feat.", "feat", "ft.", "ft", "featuring"].iter().any(|m| {
        inner
            .strip_prefix(m)
            .is_some_and(|r| r.starts_with(char::is_whitespace))
    });
    if is_feat {
        name[..open].trim_end()
    } else {
        name
    }
}

/// A tag value without the characters that are not part of a name: byte-order and zero-width
/// marks.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\u{feff}' | '\u{200b}'..='\u{200d}' | '\u{2060}'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// The first name of a tag value with the MusicBrainz id paired with it. One ID3 frame can hold
/// several NUL-separated names; the first is filed. A tag's ids follow its credits' order, so
/// the first id is the first name's only when there is one id per credited artist, featured
/// ones (`A feat. B`) included. (Upstream also splits a joint name `A & B` when the ids prove
/// two artists; kima-rs files such a name whole, D-164.)
pub fn first_credited(tag: &str, ids: &[Uuid]) -> Option<Candidate> {
    let names: Vec<String> = tag
        .split('\0')
        .map(clean)
        .filter(|n| !n.is_empty())
        .collect();
    let first = names.first()?;
    let primary = parse_credit(first, None).primary;
    let name = if primary.is_empty() {
        first.clone()
    } else {
        primary
    };
    let credited: usize = names
        .iter()
        .map(|n| 1 + parse_credit(n, None).featured.len())
        .sum();
    let mbid = (ids.len() == credited).then(|| ids[0]);
    Some(Candidate { name, mbid })
}

/// What a file says about its album's artist. `r` is the file's resolved fields.
pub fn credit_of(raw: &RawTags, r: &Resolved, rel: &str) -> Credit {
    let tagged = match r.album_artist_from {
        ArtistFrom::AlbumArtistTag => raw
            .album_artist
            .as_deref()
            .and_then(|t| first_credited(t, &raw.album_artist_mbids)),
        ArtistFrom::ArtistTag => raw
            .artist
            .as_deref()
            .and_then(|t| first_credited(t, &raw.artist_mbids)),
        ArtistFrom::Path | ArtistFrom::Unknown => None,
    };
    let mut candidate = tagged.unwrap_or_else(|| Candidate {
        name: r.own_artist.clone(),
        mbid: None,
    });
    let singles = rel.starts_with("Singles/");
    // Downloads under Singles/ often carry compilation tags: the folder's artist beats
    // "Various Artists".
    if singles
        && canonical_various(&candidate.name).is_some()
        && let Some(folder) = extract_artist_from_relative_path(rel)
        && canonical_various(&folder).is_none()
    {
        candidate = Candidate {
            name: folder,
            mbid: None,
        };
    }
    let album_key = raw
        .album
        .as_deref()
        .map(clean)
        .filter(|a| !a.is_empty())
        .map(|a| names::key(&a));
    let group = match &album_key {
        Some(key) => group_of(album_folder(rel), key),
        // A song with no album tag shares no album with its neighbours.
        None => format!("file\n{rel}"),
    };
    Credit {
        group,
        album_key,
        candidate,
        from_album_artist: r.album_artist_from == ArtistFrom::AlbumArtistTag,
        compilation: raw.compilation && !singles,
    }
}

fn identity(name: &str) -> String {
    names::key(name)
}

/// The most common of `values`, a tie going to the one seen first.
fn most_common<T: Clone + PartialEq>(values: &[T]) -> Option<T> {
    let mut counts: Vec<(&T, usize)> = Vec::new();
    for v in values {
        match counts.iter_mut().find(|(c, _)| *c == v) {
            Some((_, n)) => *n += 1,
            None => counts.push((v, 1)),
        }
    }
    let mut best: Option<(&T, usize)> = None;
    for (v, n) in counts {
        if best.is_none_or(|(_, b)| n > b) {
            best = Some((v, n));
        }
    }
    best.map(|(v, _)| v.clone())
}

/// The artist one album's songs elect, or `None` when none leads and each song keeps its own.
/// `credits` are that album's files in a stable order (by relative path, never by collation).
///
/// A compilation is: tagged so by more than half the songs **and** its songs really name more
/// than one artist (featured clauses peeled); a lone artist's album with a stray flag is that
/// artist's. A compilation is `Various Artists` at once when no song has an album-artist tag,
/// and otherwise when no artist leads.
pub fn elect(credits: &[Credit]) -> Option<Candidate> {
    let total = credits.len();
    if total == 0 {
        return None;
    }
    let various = || Candidate {
        name: VARIOUS_ARTISTS.to_string(),
        mbid: None,
    };
    let artists: std::collections::HashSet<String> = credits
        .iter()
        .map(|c| identity(without_featured_clause(&c.candidate.name)))
        .collect();
    let compilation =
        credits.iter().filter(|c| c.compilation).count() * 2 > total && artists.len() > 1;
    if compilation && !credits.iter().any(|c| c.from_album_artist) {
        return Some(various());
    }

    let mut votes: Vec<(String, Vec<&Credit>)> = Vec::new();
    for c in credits {
        let key = identity(&c.candidate.name);
        match votes.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(c),
            None => votes.push((key, vec![c])),
        }
    }
    let mut winners = &votes[0].1;
    for (_, v) in &votes {
        if v.len() > winners.len() {
            winners = v;
        }
    }
    let share = winners.len();
    if share != total && !(share > 1 && share as f64 / total as f64 >= LEAD_SHARE) {
        return compilation.then(various);
    }
    let name = most_common(
        &winners
            .iter()
            .map(|c| c.candidate.name.clone())
            .collect::<Vec<_>>(),
    )?;
    let ids: Vec<Uuid> = winners.iter().filter_map(|c| c.candidate.mbid).collect();
    Some(Candidate {
        name,
        mbid: most_common(&ids),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Uuid = Uuid::from_u128(0xa);
    const B: Uuid = Uuid::from_u128(0xb);
    const G: Uuid = Uuid::from_u128(0xc);

    fn credit(name: &str, from_album_artist: bool, compilation: bool) -> Credit {
        Credit {
            group: "g".into(),
            album_key: None,
            candidate: Candidate {
                name: name.into(),
                mbid: None,
            },
            from_album_artist,
            compilation,
        }
    }
    fn tagged(name: &str) -> Credit {
        credit(name, true, false)
    }
    fn own(name: &str) -> Credit {
        credit(name, false, false)
    }
    fn won(c: Option<Candidate>) -> Option<String> {
        c.map(|c| c.name)
    }

    #[test]
    fn lone_guest_in_album_artist_tag_does_not_split_the_album() {
        let mut v: Vec<Credit> = (0..11).map(|_| tagged("A")).collect();
        v.push(tagged("B"));
        assert_eq!(won(elect(&v)), Some("A".into()));
    }

    #[test]
    fn most_common_covering_a_quarter_wins() {
        // 2 of 4 = 50 %.
        let v = [tagged("A"), tagged("A"), tagged("B"), tagged("C")];
        assert_eq!(won(elect(&v)), Some("A".into()));
        // Exactly a quarter, more than one song: 2 of 8.
        let mut v = vec![tagged("A"), tagged("A")];
        v.extend(["B", "C", "D", "E", "F", "G"].map(tagged));
        assert_eq!(won(elect(&v)), Some("A".into()));
        // One song covers everything.
        assert_eq!(won(elect(&[tagged("A")])), Some("A".into()));
    }

    #[test]
    fn no_leader_keeps_each_songs_own_artist() {
        let v = [own("One"), own("Two"), own("Three")];
        assert_eq!(elect(&v), None);
        // A quarter, but a single song is no lead: 1 of 4.
        let v = [own("One"), own("Two"), own("Three"), own("Four")];
        assert_eq!(elect(&v), None);
        // Less than a quarter: 2 of 9.
        let mut v = vec![own("A"), own("A")];
        v.extend(["B", "C", "D", "E", "F", "G", "H"].map(own));
        assert_eq!(elect(&v), None);
    }

    #[test]
    fn compilation_flag_with_several_artists_is_various_artists() {
        let v = [
            credit("Lead", false, true),
            credit("Lead", false, true),
            credit("Other", false, true),
        ];
        assert_eq!(won(elect(&v)), Some("Various Artists".into()));
        // Flagged by most, no artist leads, album artists present: still Various Artists.
        let v = [
            credit("One", true, true),
            credit("Two", true, true),
            credit("Three", true, true),
        ];
        assert_eq!(won(elect(&v)), Some("Various Artists".into()));
    }

    #[test]
    fn compilation_flagged_by_half_or_fewer_is_not_a_compilation() {
        let v = [credit("One", false, true), credit("Two", false, false)];
        assert_eq!(elect(&v), None);
    }

    #[test]
    fn stray_compilation_flag_on_a_one_artist_album_is_ignored() {
        let v = [
            credit("Solo", false, true),
            credit("Solo", false, true),
            credit("Solo", false, true),
        ];
        assert_eq!(won(elect(&v)), Some("Solo".into()));
    }

    #[test]
    fn featured_credit_is_the_same_artist() {
        let v = [
            credit("Singer", false, true),
            credit("Singer (feat. Guest)", false, true),
            credit("Singer", false, true),
        ];
        assert_ne!(won(elect(&v)), Some("Various Artists".into()));
        assert_eq!(without_featured_clause("Singer (feat. Guest)"), "Singer");
        assert_eq!(without_featured_clause("Singer [ft. Guest]"), "Singer");
        assert_eq!(without_featured_clause("Singer (Live)"), "Singer (Live)");
        assert_eq!(without_featured_clause("Singer"), "Singer");
    }

    #[test]
    fn vote_ties_follow_path_order_not_locale() {
        // Two votes each: the artist named first (earliest path) wins, however the names sort.
        let v = [own("Zed"), own("Åke"), own("Zed"), own("Åke")];
        assert_eq!(won(elect(&v)), Some("Zed".into()));
        let v = [own("Åke"), own("Zed"), own("Zed"), own("Åke")];
        assert_eq!(won(elect(&v)), Some("Åke".into()));
    }

    #[test]
    fn same_artist_is_one_vote_across_spellings() {
        let v = [own("Beyoncé"), own("BEYONCÉ"), own("Other")];
        assert_eq!(won(elect(&v)), Some("Beyoncé".into()));
    }

    #[test]
    fn winners_mbid_comes_from_the_songs_that_named_it() {
        let mut ided = tagged("Ided");
        ided.candidate.mbid = Some(A);
        let mut other = tagged("Other");
        other.candidate.mbid = Some(B);
        let v = [ided.clone(), ided, other];
        assert_eq!(
            elect(&v),
            Some(Candidate {
                name: "Ided".into(),
                mbid: Some(A)
            })
        );
    }

    #[test]
    fn disc_subfolders_vote_as_one_album() {
        assert_eq!(album_folder("Band/Double/CD 2/01.flac"), "Band/Double");
        assert_eq!(album_folder("Band/Double/CD 1/01.flac"), "Band/Double");
        assert_eq!(album_folder("Band/Double/Disc03/01.flac"), "Band/Double");
        assert_eq!(album_folder("Band/Double/disk_2/01.flac"), "Band/Double");
        assert_eq!(
            album_folder("Band/Double/Digital Media 02/01.flac"),
            "Band/Double"
        );
        assert_eq!(album_folder("Band/Double/Vinyl 1/01.flac"), "Band/Double");
        assert_eq!(album_folder("Band/Double/Side 2/01.flac"), "Band/Double");
        assert_eq!(album_folder("Band/Double/01.flac"), "Band/Double");
        assert_eq!(album_folder("Band/Extras/01.flac"), "Band/Extras");
        // `\d+\b`: a digit run glued to letters is not a disc.
        assert_eq!(album_folder("Band/Album/CD1B/01.flac"), "Band/Album/CD1B");
        // A bare "CD" or "Disc" has no number.
        assert_eq!(album_folder("Band/Album/Disc/01.flac"), "Band/Album/Disc");
        // A disc folder directly under the root belongs to the root.
        assert_eq!(album_folder("CD 1/01.flac"), "");
        assert_eq!(album_folder("01.flac"), "");
    }

    #[test]
    fn nul_separated_names_are_split_and_the_first_filed() {
        let c = first_credited("First\0\u{feff}Second", &[A, B]).unwrap();
        assert_eq!(c.name, "First");
        assert_eq!(c.mbid, Some(A));
        // A featured credit inside a name counts when pairing ids: three ids for three artists.
        let c = first_credited("First feat. Guest\0Second", &[A, G, B]).unwrap();
        assert_eq!(c.name, "First");
        assert_eq!(c.mbid, Some(A));
        // Ids that do not match the credited count prove nothing.
        assert_eq!(first_credited("First\0Second", &[A]).unwrap().mbid, None);
        // A joint name stays whole, ids or not (D-164).
        let c = first_credited("First & Second", &[A, B]).unwrap();
        assert_eq!((c.name.as_str(), c.mbid), ("First & Second", None));
        assert_eq!(first_credited("  \u{200b} ", &[]), None);
    }

    #[test]
    fn credit_groups_by_album_folder_and_album_key_and_loose_songs_stand_alone() {
        let raw = |album: Option<&str>| RawTags {
            album_artist: Some("A".into()),
            album: album.map(str::to_string),
            ..RawTags::default()
        };
        let credit = |album: Option<&str>, rel: &str| {
            let r = raw(album);
            let res = crate::resolve::resolve(&r, std::path::Path::new(rel));
            credit_of(&r, &res, rel)
        };
        let cd1 = credit(Some("Double"), "Band/Double/CD 1/01.flac");
        let cd2 = credit(Some("DOUBLE "), "Band/Double/CD 2/01.flac");
        assert_eq!(cd1.group, cd2.group);
        let other = credit(Some("Double"), "Band/Other/01.flac");
        assert_ne!(cd1.group, other.group);
        let loose_a = credit(None, "Loose/a.mp3");
        let loose_b = credit(None, "Loose/b.mp3");
        assert_ne!(loose_a.group, loose_b.group);
    }

    #[test]
    fn singles_folder_artist_beats_a_compilation_tag() {
        let raw = RawTags {
            artist: Some("Folder Artist".into()),
            album: Some("Single".into()),
            compilation: true,
            ..RawTags::default()
        };
        let rel = "Singles/Folder Artist/Single/01.flac";
        let res = crate::resolve::resolve(&raw, std::path::Path::new(rel));
        let c = credit_of(&raw, &res, rel);
        assert_eq!(c.candidate.name, "Folder Artist");
        assert!(!c.compilation);
    }
}
