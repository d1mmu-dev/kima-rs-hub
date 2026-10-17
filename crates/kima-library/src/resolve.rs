//! Pure field resolution: tags first, then folder and file-name fallbacks (spec §4.1, §4.2).

use crate::names::{VARIOUS_ARTISTS, canonical_various};
use crate::tags::RawTags;
use kima_db::library::Guessed;
use std::path::Path;

/// The resolved fields of one track. `year` is the track's own year; the album year is decided
/// per folder group by the scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub album_artist: String,
    pub track_artist: String,
    pub album: String,
    pub title: String,
    pub year: Option<i32>,
    pub disc: u32,
    pub track: u32,
    pub is_compilation: bool,
    pub guessed: Guessed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCredit {
    pub primary: String,
    pub featured: Vec<String>,
}

const FEAT_WORDS: &[&str] = &["feat.", "feat", "ft.", "ft", "featuring", "w/"];

/// Splits off featured credits (`feat.`, `ft.`, `featuring`, `w/` as whole words), like upstream's
/// `FEATURED_MARKER`. Returns the primary part followed by the text after each marker.
fn split_featured(s: &str) -> Vec<&str> {
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut word_start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(ws) = word_start.take() {
                words.push((ws, i));
            }
        } else if word_start.is_none() {
            word_start = Some(i);
        }
    }
    if let Some(ws) = word_start {
        words.push((ws, s.len()));
    }
    let mut pieces = Vec::new();
    let mut start = 0;
    for (k, &(ws, we)) in words.iter().enumerate() {
        // A marker needs whitespace before it and a word after it.
        if k > 0 && k + 1 < words.len() && FEAT_WORDS.contains(&s[ws..we].to_lowercase().as_str()) {
            pieces.push(&s[start..ws]);
            start = we;
        }
    }
    pieces.push(&s[start..]);
    pieces
}

/// Splits an artist tag into a primary artist and featured credits. Prefers multi-value frames;
/// a joined string splits only on NUL and `;`, never on "&" or "and". Port of `parseCredit`.
pub fn parse_credit(raw: &str, multi: Option<&[String]>) -> ParsedCredit {
    let values: Vec<String> = match multi {
        Some(m) if m.len() > 1 => m.iter().map(|v| v.trim().to_string()).collect(),
        _ => raw
            .split(['\0', ';'])
            .map(|v| v.trim().to_string())
            .collect(),
    };
    let values: Vec<String> = values.into_iter().filter(|v| !v.is_empty()).collect();
    let Some((head, tail)) = values.split_first() else {
        return ParsedCredit {
            primary: String::new(),
            featured: Vec::new(),
        };
    };
    let mut featured = Vec::new();
    let mut parts = split_featured(head).into_iter();
    let primary_raw = parts.next().unwrap_or("").trim().to_string();
    featured.extend(
        parts
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty()),
    );
    for v in tail {
        featured.extend(
            split_featured(v)
                .into_iter()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty()),
        );
    }
    let primary = canonical_various(&primary_raw)
        .map(str::to_string)
        .unwrap_or(primary_raw);
    ParsedCredit { primary, featured }
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Artist name from a folder name: `Artist - Album (Year)` gives `Artist`; the scene format
/// `Artist.Name-Album.Name-FLAC-2023` gives `Artist.Name` (as upstream: its dash rule wins over
/// the scene rule, which only applies when the first part is too short). Port of `parseArtistFromPath`.
pub fn parse_artist_from_path(folder: &str) -> Option<String> {
    let folder = folder.trim();
    let (first, rest) = folder.split_once('-')?;
    let artist = first.trim();
    if !rest.is_empty() && artist.chars().count() >= 2 && !all_digits(artist) {
        return Some(artist.to_string());
    }
    // Scene format. (Upstream would accept an all-digit part here, returning "1997" for
    // "1997-Album"; rejected, see DIVERGENCES.)
    let artist = artist.replace('.', " ");
    let artist = artist.trim();
    let meta = ["flac", "mp3", "wav", "24bit", "web", "cd"];
    if artist.chars().count() >= 2
        && !all_digits(artist)
        && !meta.contains(&artist.to_lowercase().as_str())
    {
        return Some(artist.to_string());
    }
    None
}

fn path_parts(rel: &str) -> Vec<&str> {
    rel.split(['/', '\\']).collect()
}

/// Artist from a relative path: the album folder name, then the grandparent folder, then an
/// `Artist - Album - Track - Title` file name. Port of `extractArtistFromRelativePath`.
pub fn extract_artist_from_relative_path(rel: &str) -> Option<String> {
    let parts = path_parts(rel);
    let n = parts.len();
    if n < 2 {
        return None;
    }
    if let Some(a) = parse_artist_from_path(parts[n - 2]) {
        return Some(a);
    }
    if n >= 3 {
        let gp = parts[n - 3];
        let generic = ["music", "songs", "audio", "media", "downloads", "library"];
        if !gp.is_empty() && !generic.contains(&gp.to_lowercase().as_str()) {
            return Some(gp.to_string());
        }
    }
    let file = parts[n - 1];
    let base = file.rsplit_once('.').map_or(file, |(b, _)| b);
    let segs: Vec<&str> = base.split(" - ").collect();
    if segs.len() >= 3 {
        let a = segs[0].trim();
        if a.chars().count() >= 2 && !all_digits(a) {
            return Some(a.to_string());
        }
    }
    None
}

/// Album folder (the parent folder), or None at the root. Port of `extractAlbumFromRelativePath`.
pub fn extract_album_from_relative_path(rel: &str) -> Option<String> {
    let parts = path_parts(rel);
    let n = parts.len();
    if n < 2 || parts[n - 2].is_empty() {
        return None;
    }
    Some(parts[n - 2].to_string())
}

fn four_digit_year(s: &str) -> Option<i32> {
    if s.len() == 4 && all_digits(s) {
        let y: i32 = s.parse().ok()?;
        (1000..3000).contains(&y).then_some(y)
    } else {
        None
    }
}

/// Splits an album folder into (album title, year): `1997 - OK Computer`, `OK Computer (1997)`
/// or `[1997]`. When the folder reads `Artist - Album`, the artist part is dropped.
fn album_from_folder(folder: &str) -> (String, Option<i32>) {
    let mut name = folder.trim();
    let mut year = None;
    let mut had_prefix = false;
    // Prefix: "1997 - Title", "1997. Title", "1997_Title"
    if name.len() > 4
        && name.is_char_boundary(4)
        && let Some(y) = four_digit_year(&name[..4])
    {
        let rest = name[4..].trim_start();
        let stripped = rest.trim_start_matches(['-', '\u{2013}', '.', '_']);
        if stripped.len() < rest.len() && !stripped.trim().is_empty() {
            year = Some(y);
            name = stripped.trim();
            had_prefix = true;
        }
    }
    // Suffix: "Title (1997)" or "Title [1997]"
    if year.is_none() {
        for (open, close) in [('(', ')'), ('[', ']')] {
            if let Some((head, inner)) = name.strip_suffix(close).and_then(|s| s.rsplit_once(open))
                && !head.trim().is_empty()
                && let Some(y) = four_digit_year(inner.trim())
            {
                year = Some(y);
                name = head.trim();
                break;
            }
        }
    }
    // "Artist - Album" folders: the artist part is not the album.
    if !had_prefix
        && let Some((artist, album)) = name.split_once(" - ")
        && parse_artist_from_path(name).as_deref() == Some(artist.trim())
        && !album.trim().is_empty()
    {
        name = album.trim();
    }
    (name.to_string(), year)
}

/// Length of the 1 to 3 digit run at the start of `s`, or 0 (four or more digits is a year).
fn number_len(s: &str) -> usize {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    if n <= 3 { n } else { 0 }
}

/// Disc and track numbers from a file stem, plus the title left after them.
/// `03 - Title` is (None, 3, "Title"); `1-03 Title` is (Some(1), 3, "Title").
fn number_from_stem(stem: &str) -> (Option<u32>, Option<u32>, String) {
    let seps: &[char] = &[' ', '.', '-', '_', ')'];
    let n1 = number_len(stem);
    if n1 == 0 {
        return (None, None, stem.to_string());
    }
    let first: u32 = stem[..n1].parse().unwrap_or(0);
    let after = &stem[n1..];
    let (disc, track, rest) = match after.strip_prefix('-') {
        Some(r) if number_len(r) > 0 => {
            let n2 = number_len(r);
            (Some(first), r[..n2].parse().unwrap_or(0), &r[n2..])
        }
        _ => (None, first, after),
    };
    if !rest.is_empty() && !rest.starts_with(seps) {
        // "3rd Stone": not a track number.
        return (None, None, stem.to_string());
    }
    let title = rest.trim_start_matches(seps).trim();
    let title = if title.is_empty() { stem } else { title };
    (disc, Some(track), title.to_string())
}

/// Resolves every field of a track from its tags, falling back to the folder and file name
/// (spec §4.1). Records each fallback in `guessed`.
pub fn resolve(raw: &RawTags, rel_path: &Path) -> Resolved {
    let rel = rel_path.to_string_lossy().replace('\\', "/");
    let mut g = Guessed::default();

    let tag_credit = |s: &Option<String>| {
        s.as_deref()
            .map(|s| parse_credit(s, None).primary)
            .filter(|p| !p.is_empty())
    };
    let mut album_artist = tag_credit(&raw.album_artist).or_else(|| tag_credit(&raw.artist));
    if album_artist.is_none()
        && let Some(a) = extract_artist_from_relative_path(&rel)
    {
        g.artist_from_path = true;
        album_artist = Some(a);
    }
    let album_artist = album_artist.unwrap_or_else(|| {
        g.unknown_artist = true;
        "Unknown Artist".to_string()
    });
    let track_artist = raw
        .artist
        .as_deref()
        .map(|a| canonical_various(a).unwrap_or(a).to_string())
        .unwrap_or_else(|| album_artist.clone());

    let is_compilation = raw.compilation || canonical_various(&album_artist).is_some();
    let album_artist = if is_compilation {
        VARIOUS_ARTISTS.to_string()
    } else {
        album_artist
    };

    let folder_parts = extract_album_from_relative_path(&rel).map(|f| album_from_folder(&f));
    let album = match raw.album.as_deref() {
        Some(a) => a.to_string(),
        None => match folder_parts.as_ref().filter(|(a, _)| !a.is_empty()) {
            Some((a, _)) => {
                g.album_from_path = true;
                a.clone()
            }
            None => {
                g.unknown_album = true;
                "Unknown Album".to_string()
            }
        },
    };
    let year = raw.year.or_else(|| {
        let y = folder_parts.as_ref().and_then(|(_, y)| *y)?;
        g.year_from_path = true;
        Some(y)
    });

    let file = rel.rsplit('/').next().unwrap_or("");
    let stem = file.rsplit_once('.').map_or(file, |(b, _)| b);
    let (name_disc, name_track, name_title) = number_from_stem(stem);
    let title = match raw.title.as_deref() {
        Some(t) => t.to_string(),
        None => {
            g.title_from_name = true;
            name_title
        }
    };
    let track = match raw.track.filter(|&t| t > 0) {
        Some(t) => t,
        None => match name_track {
            Some(t) => {
                g.track_from_name = true;
                t
            }
            None => 0,
        },
    };
    let disc = raw.disc.filter(|&d| d > 0).or(name_disc).unwrap_or(0);

    Resolved {
        album_artist,
        track_artist,
        album,
        title,
        year,
        disc,
        track,
        is_compilation,
        guessed: g,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn raw() -> RawTags {
        RawTags::default()
    }
    fn res(r: &RawTags, p: &str) -> Resolved {
        resolve(r, Path::new(p))
    }

    // --- ported from upstream musicScanner.test.ts / artistIdentity.test.ts ---

    #[test]
    fn upstream_parse_artist_from_path() {
        assert_eq!(
            parse_artist_from_path("Radiohead - OK Computer").as_deref(),
            Some("Radiohead")
        );
        assert_eq!(
            parse_artist_from_path("Tool - Lateralus (2001)").as_deref(),
            Some("Tool")
        );
        assert_eq!(parse_artist_from_path(""), None);
        assert_eq!(parse_artist_from_path("Unknown"), None);
        assert_eq!(
            parse_artist_from_path("Artist.Name-Album.Name-24BIT-FLAC-2023-GROUP").as_deref(),
            Some("Artist.Name")
        );
    }

    #[test]
    fn upstream_extract_artist_from_relative_path() {
        assert_eq!(
            extract_artist_from_relative_path("Radiohead/OK Computer/01 - Airbag.flac").as_deref(),
            Some("Radiohead")
        );
        assert_eq!(extract_artist_from_relative_path("track.mp3"), None);
        assert_eq!(
            extract_artist_from_relative_path("Tool - Lateralus/01.mp3").as_deref(),
            Some("Tool")
        );
        assert_eq!(
            extract_artist_from_relative_path("Radiohead\\OK Computer\\01.flac").as_deref(),
            Some("Radiohead")
        );
        // generic grandparent is skipped
        assert_eq!(
            extract_artist_from_relative_path("Music/Album/01.mp3"),
            None
        );
        // "Artist - Album - Track - Title" file name
        assert_eq!(
            extract_artist_from_relative_path("Album/Pixies - Doolittle - 01 - Debaser.mp3")
                .as_deref(),
            Some("Pixies")
        );
    }

    #[test]
    fn upstream_extract_album_from_relative_path() {
        assert_eq!(
            extract_album_from_relative_path("Radiohead/OK Computer/01.flac").as_deref(),
            Some("OK Computer")
        );
        assert_eq!(extract_album_from_relative_path("track.mp3"), None);
        assert_eq!(extract_album_from_relative_path(""), None);
    }

    #[test]
    fn upstream_parse_credit_keeps_band_names_whole() {
        for name in [
            "Dance With the Dead",
            "Nick Cave & The Bad Seeds",
            "Bob Marley & The Wailers",
            "Tom Petty and the Heartbreakers",
            "Echo & the Bunnymen",
            "Derek and the Dominos",
            "Florence and the Machine",
            "Earth, Wind & Fire",
            "Of Mice & Men",
            "Simon & Garfunkel",
            "Sleater-Kinney",
            "Emerson, Lake & Palmer",
            "Crosby, Stills, Nash & Young",
            "Cheat Codes x Kris Kross",
        ] {
            let c = parse_credit(name, None);
            assert_eq!(c.primary, name);
            assert!(c.featured.is_empty(), "{name}");
        }
    }

    #[test]
    fn upstream_parse_credit_splits() {
        let c = parse_credit("Justice\0Tame Impala", None);
        assert_eq!(
            (c.primary.as_str(), c.featured),
            ("Justice", vec!["Tame Impala".to_string()])
        );
        let c = parse_credit("Justice;Tame Impala", None);
        assert_eq!(
            (c.primary.as_str(), c.featured),
            ("Justice", vec!["Tame Impala".to_string()])
        );
        let multi = vec!["R\u{f6}yksopp".to_string(), "Robyn".to_string()];
        let c = parse_credit("ignored", Some(&multi));
        assert_eq!(c.primary, "R\u{f6}yksopp");
        assert_eq!(c.featured, vec!["Robyn".to_string()]);
        for marker in ["feat.", "ft.", "featuring", "feat", "ft", "FEAT."] {
            let c = parse_credit(&format!("Dance With the Dead {marker} Gunship"), None);
            assert_eq!(c.primary, "Dance With the Dead", "{marker}");
            assert_eq!(c.featured, vec!["Gunship".to_string()], "{marker}");
        }
        for va in ["VA", "V.A.", "Various", "Various Artists"] {
            assert_eq!(parse_credit(va, None).primary, "Various Artists");
        }
        assert_eq!(parse_credit("", None).primary, "");
        assert!(parse_credit("", None).featured.is_empty());
    }

    // --- spec cases ---

    #[test]
    fn albumartist_wins_over_artist() {
        let r = RawTags {
            album_artist: Some("Album Artist".into()),
            artist: Some("Track Artist".into()),
            album: Some("A".into()),
            title: Some("T".into()),
            ..raw()
        };
        let x = res(&r, "x/y/z.mp3");
        assert_eq!(x.album_artist, "Album Artist");
        assert_eq!(x.track_artist, "Track Artist");
        assert_eq!(x.guessed, Guessed::default());
    }

    #[test]
    fn feat_track_stays_on_album_artist() {
        let r = RawTags {
            album_artist: Some("Dance With the Dead".into()),
            artist: Some("Dance With the Dead feat. Gunship".into()),
            album: Some("A".into()),
            title: Some("T".into()),
            ..raw()
        };
        let x = res(&r, "x/y/z.mp3");
        assert_eq!(x.album_artist, "Dance With the Dead");
        assert_eq!(x.track_artist, "Dance With the Dead feat. Gunship");
        // With no album artist, the feat. credit is peeled off for grouping.
        let r = RawTags {
            album_artist: None,
            ..r
        };
        assert_eq!(res(&r, "x/y/z.mp3").album_artist, "Dance With the Dead");
    }

    #[test]
    fn band_with_ampersand_not_split() {
        let r = RawTags {
            artist: Some("Simon & Garfunkel".into()),
            album: Some("Bookends".into()),
            title: Some("America".into()),
            ..raw()
        };
        let x = res(&r, "a/b/c.mp3");
        assert_eq!(x.album_artist, "Simon & Garfunkel");
        assert_eq!(x.track_artist, "Simon & Garfunkel");
    }

    #[test]
    fn year_from_folder_prefix() {
        let x = res(&raw(), "Artist/1997 - OK Computer/01.mp3");
        assert_eq!(x.year, Some(1997));
        assert_eq!(x.album, "OK Computer");
        assert_eq!(x.album_artist, "Artist");
        assert!(x.guessed.year_from_path);
        assert!(x.guessed.album_from_path);
        assert!(x.guessed.artist_from_path);
    }

    #[test]
    fn year_from_folder_suffix() {
        let x = res(&raw(), "Radiohead/OK Computer (1997)/01.mp3");
        assert_eq!(x.year, Some(1997));
        assert_eq!(x.album, "OK Computer");
        let x = res(&raw(), "Radiohead/OK Computer [1997]/01.mp3");
        assert_eq!(x.year, Some(1997));
        assert_eq!(x.album, "OK Computer");
    }

    #[test]
    fn tag_year_beats_folder_year() {
        let r = RawTags {
            year: Some(2009),
            album: Some("OK Computer".into()),
            ..raw()
        };
        let x = res(&r, "Radiohead/OK Computer (1997)/01.mp3");
        assert_eq!(x.year, Some(2009));
        assert!(!x.guessed.year_from_path);
    }

    #[test]
    fn track_from_filename() {
        let x = res(&raw(), "A/B/03 - Title.mp3");
        assert_eq!((x.disc, x.track), (0, 3));
        assert!(x.guessed.track_from_name);
        let x = res(&raw(), "A/B/1-03 Title.flac");
        assert_eq!((x.disc, x.track), (1, 3));
        assert_eq!(x.title, "Title");
        let r = RawTags {
            disc: Some(2),
            track: Some(7),
            ..raw()
        };
        let x = res(&r, "A/B/03 - Title.mp3");
        assert_eq!((x.disc, x.track), (2, 7));
        assert!(!x.guessed.track_from_name);
    }

    #[test]
    fn title_from_filename_strips_number() {
        let x = res(&raw(), "A/B/03 - Some Song.mp3");
        assert_eq!(x.title, "Some Song");
        assert!(x.guessed.title_from_name);
        assert_eq!(res(&raw(), "A/B/03. Some Song.mp3").title, "Some Song");
        assert_eq!(res(&raw(), "A/B/Some Song.mp3").title, "Some Song");
        assert_eq!(res(&raw(), "A/B/01.mp3").title, "01");
        let r = RawTags {
            title: Some("Tagged".into()),
            ..raw()
        };
        let x = res(&r, "A/B/03 - Some Song.mp3");
        assert_eq!(x.title, "Tagged");
        assert!(!x.guessed.title_from_name);
    }

    #[test]
    fn unknown_when_nothing() {
        let x = res(&raw(), "song.mp3");
        assert_eq!(x.album_artist, "Unknown Artist");
        assert_eq!(x.track_artist, "Unknown Artist");
        assert_eq!(x.album, "Unknown Album");
        assert!(x.guessed.unknown_artist);
        assert!(x.guessed.unknown_album);
        assert!(!x.guessed.year_conflict);
        assert_eq!(x.year, None);
    }

    #[test]
    fn compilation_flag_and_various_names_group_as_various() {
        let r = RawTags {
            compilation: true,
            album_artist: Some("Some DJ".into()),
            artist: Some("Track Artist".into()),
            album: Some("Now 99".into()),
            ..raw()
        };
        let x = res(&r, "c/Now 99/01.mp3");
        assert_eq!(x.album_artist, "Various Artists");
        assert_eq!(x.track_artist, "Track Artist");
        assert!(x.is_compilation);
        for va in ["VA", "V.A.", "various", "Various Artist", "Varios Artistas"] {
            let r = RawTags {
                album_artist: Some(va.into()),
                album: Some("Now".into()),
                ..raw()
            };
            let x = res(&r, "c/Now/01.mp3");
            assert_eq!(x.album_artist, "Various Artists", "{va}");
            assert!(x.is_compilation, "{va}");
        }
    }
}
