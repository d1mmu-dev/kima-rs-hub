//! Pure path planning for the organizer (spec §5.1). No I/O and no scanner types: [`Fields`] is
//! filled by the caller, so the M3 autotagger can feed corrected values.
//!
//! Collisions are detected case-insensitively (and after NFC), because macOS and Windows
//! filesystems are case-insensitive: `Abba/x.flac` and `ABBA/x.flac` cannot coexist there.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use unicode_normalization::UnicodeNormalization;

use crate::names::{self, VARIOUS_ARTISTS};

/// Longest path component, in bytes.
const MAX_COMPONENT: usize = 120;

/// Everything the path pattern needs about one track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    pub album_artist: String,
    pub track_artist: String,
    pub album: String,
    pub year: Option<i32>,
    pub disc: u32,
    pub disc_total: u32,
    pub track_no: u32,
    pub title: String,
    /// File extension without the dot.
    pub ext: String,
    pub is_compilation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMove {
    pub track_id: Uuid,
    pub from: PathBuf,
    pub to: PathBuf,
    /// Another track wanted the same target, or something already sits there.
    pub collision: bool,
    pub unchanged: bool,
    /// `to` differs from `from` only in letter case: on a case-insensitive filesystem this
    /// needs a two-step move.
    pub case_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarMove {
    pub from: PathBuf,
    pub to: PathBuf,
    pub collision: bool,
    pub unchanged: bool,
}

fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end();
    let up = stem.to_ascii_uppercase();
    matches!(up.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((up.starts_with("COM") || up.starts_with("LPT"))
            && up.len() == 4
            && matches!(up.as_bytes()[3], b'1'..=b'9'))
}

fn truncate_bytes(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

fn trim_tail(s: &mut String) {
    let n = s.trim_end_matches(['.', ' ']).len();
    s.truncate(n);
}

/// NFC, forbidden characters and control characters replaced by `_`; a piece that is nothing
/// but dots and spaces becomes `_`. For parts that are joined into a longer component.
fn scrub(s: &str) -> String {
    let out: String = s
        .nfc()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if out.trim_end_matches(['.', ' ']).is_empty() {
        "_".into()
    } else {
        out
    }
}

/// Makes one path component safe for Linux, Windows and macOS. Never returns an empty string,
/// `.` or `..`, and never a string containing a separator.
pub fn sanitize_component(s: &str) -> String {
    let mut out = scrub(s);
    // A leading dot would make the name hidden, and the scanner and watcher skip hidden names.
    out = out.trim_start_matches(['.', ' ']).to_string();
    trim_tail(&mut out);
    truncate_bytes(&mut out, MAX_COMPONENT);
    trim_tail(&mut out);
    if out.is_empty() {
        return "_".into();
    }
    if is_reserved(&out) {
        out.insert(0, '_');
        truncate_bytes(&mut out, MAX_COMPONENT);
        trim_tail(&mut out);
    }
    out
}

/// A safe extension: ASCII alphanumerics only, at most 8 of them.
fn safe_ext(ext: &str) -> String {
    ext.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// A file name `<stem>.<ext>` whose stem is sanitised and short enough for the whole name to
/// stay within the component limit, with room left for a ` (nn)` collision suffix.
fn file_name(stem: &str, ext: &str) -> String {
    let ext = safe_ext(ext);
    let reserve = if ext.is_empty() { 0 } else { ext.len() + 1 };
    let mut s = sanitize_component(stem);
    truncate_bytes(&mut s, MAX_COMPONENT - reserve - 6);
    trim_tail(&mut s);
    if s.is_empty() {
        s.push('_');
    }
    if ext.is_empty() {
        s
    } else {
        format!("{s}.{ext}")
    }
}

/// The relative target path for a track. Always relative and free of `..`.
pub fn target(f: &Fields) -> PathBuf {
    let artist = if f.is_compilation {
        VARIOUS_ARTISTS
    } else {
        &f.album_artist
    };
    let album = match f.year {
        Some(y) if y > 0 => format!("{y} - {}", scrub(&f.album)),
        _ => f.album.clone(), // sanitised below
    };
    let num = if f.disc_total > 1 || f.disc > 1 {
        format!("{}-{:02}", f.disc, f.track_no)
    } else {
        format!("{:02}", f.track_no)
    };
    let stem = if f.is_compilation {
        format!("{num} - {} - {}", scrub(&f.track_artist), scrub(&f.title))
    } else {
        format!("{num} - {}", scrub(&f.title))
    };
    let mut p = PathBuf::new();
    p.push(sanitize_component(artist));
    p.push(sanitize_component(&album));
    p.push(file_name(&stem, &f.ext));
    p
}

/// Collision key: NFC, case-insensitive, per whole path.
fn path_key(p: &Path) -> String {
    names::key(&p.to_string_lossy())
}

/// `dir/name (n).ext`
fn with_suffix(p: &Path, n: u32) -> PathBuf {
    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("_");
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    p.with_file_name(format!("{stem} ({n}){ext}"))
}

/// Plans every track's target. Deterministic: tracks are ordered by (target, track id) and the
/// second and later claimants of a target get ` (2)`, ` (3)`. A track already at its target
/// keeps it. `existing` holds paths already occupied at the destination; the planned tracks'
/// own current paths are treated as vacated.
pub fn plan(tracks: &[(Uuid, Fields, PathBuf)], existing: &HashSet<PathBuf>) -> Vec<PlannedMove> {
    let own: HashSet<String> = tracks.iter().map(|(_, _, from)| path_key(from)).collect();
    let mut claimed: HashSet<String> = existing
        .iter()
        .map(|p| path_key(p))
        .filter(|k| !own.contains(k))
        .collect();
    let existing_hit = claimed.clone();

    // An album's disc count is the largest of the given totals and the discs actually present,
    // so a missing total still gives every disc the `D-NN` prefix.
    let album_key = |f: &Fields| (names::key(&f.album_artist), names::key(&f.album), f.year);
    let mut discs: HashMap<(String, String, Option<i32>), u32> = HashMap::new();
    for (_, f, _) in tracks {
        let e = discs.entry(album_key(f)).or_default();
        *e = (*e).max(f.disc_total).max(f.disc);
    }
    let mut items: Vec<(PathBuf, Uuid, &PathBuf)> = tracks
        .iter()
        .map(|(id, f, from)| {
            let mut f = f.clone();
            f.disc_total = discs[&album_key(&f)];
            (target(&f), *id, from)
        })
        .collect();
    items.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));

    let mut group: HashMap<String, usize> = HashMap::new();
    for (to, _, _) in &items {
        *group.entry(path_key(to)).or_default() += 1;
    }
    // Tracks already in place keep their path.
    for (to, _, from) in &items {
        if to == *from {
            claimed.insert(path_key(to));
        }
    }
    items
        .into_iter()
        .map(|(base, track_id, from)| {
            let key = path_key(&base);
            let shared = group[&key] > 1;
            if &base == from {
                return PlannedMove {
                    track_id,
                    from: from.clone(),
                    to: base,
                    collision: false,
                    unchanged: true,
                    case_only: false,
                };
            }
            let mut to = base.clone();
            let mut n = 2;
            while claimed.contains(&path_key(&to)) {
                to = with_suffix(&base, n);
                n += 1;
            }
            claimed.insert(path_key(&to));
            let unchanged = &to == from;
            PlannedMove {
                track_id,
                from: from.clone(),
                // A file that stays where it is has nothing to warn about, so a re-plan of an
                // organized library reports no collisions.
                collision: !unchanged && (shared || to != base || existing_hit.contains(&key)),
                case_only: !unchanged
                    && to.to_string_lossy().to_lowercase() == from.to_string_lossy().to_lowercase(),
                to,
                unchanged,
            }
        })
        .collect()
}

/// Whether a non-audio file travels with its album: cover images, `.cue` and `.log`.
pub fn is_sidecar(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "cue" | "log")
    )
}

/// Plans sidecar files into the target folder of the album their source folder holds. A source
/// folder whose tracks land in several folders sends sidecars to the most common one (ties:
/// the smallest path). Sidecars in folders with no planned track stay out of the plan.
pub fn plan_sidecars(
    moves: &[PlannedMove],
    sidecars: &[PathBuf],
    existing: &HashSet<PathBuf>,
) -> Vec<SidecarMove> {
    let mut votes: HashMap<PathBuf, BTreeMap<PathBuf, usize>> = HashMap::new();
    for m in moves {
        if let (Some(src), Some(dst)) = (m.from.parent(), m.to.parent()) {
            *votes
                .entry(src.to_path_buf())
                .or_default()
                .entry(dst.to_path_buf())
                .or_default() += 1;
        }
    }
    let mut dest: HashMap<PathBuf, PathBuf> = HashMap::new();
    for (src, v) in votes {
        // Ascending iteration; reversed so `max_by_key` (last maximum wins) picks the smallest.
        if let Some((d, _)) = v.into_iter().rev().max_by_key(|(_, c)| *c) {
            dest.insert(src, d);
        }
    }

    let own: HashSet<String> = sidecars.iter().map(|p| path_key(p)).collect();
    let mut claimed: HashSet<String> = existing
        .iter()
        .chain(moves.iter().map(|m| &m.to))
        .map(|p| path_key(p))
        .filter(|k| !own.contains(k))
        .collect();
    let mut sorted: Vec<&PathBuf> = sidecars.iter().collect();
    sorted.sort();
    let mut out = Vec::new();
    for from in sorted {
        let Some(dir) = from.parent().and_then(|p| dest.get(p)) else {
            continue;
        };
        let name = from.file_name().and_then(|s| s.to_str()).unwrap_or("_");
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
            _ => (name, ""),
        };
        let base = dir.join(file_name(stem, ext));
        let mut to = base.clone();
        let mut n = 2;
        while claimed.contains(&path_key(&to)) {
            to = with_suffix(&base, n);
            n += 1;
        }
        claimed.insert(path_key(&to));
        let unchanged = &to == from;
        out.push(SidecarMove {
            from: from.clone(),
            collision: !unchanged && to != base,
            unchanged,
            to,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(artist: &str, album: &str, year: Option<i32>, no: u32, title: &str) -> Fields {
        Fields {
            album_artist: artist.into(),
            track_artist: artist.into(),
            album: album.into(),
            year,
            disc: 1,
            disc_total: 1,
            track_no: no,
            title: title.into(),
            ext: "flac".into(),
            is_compilation: false,
        }
    }

    #[test]
    fn basic_pattern() {
        let p = target(&f("Radiohead", "OK Computer", Some(1997), 1, "Airbag"));
        assert_eq!(
            p,
            PathBuf::from("Radiohead/1997 - OK Computer/01 - Airbag.flac")
        );
    }

    #[test]
    fn multi_disc_prefix() {
        let mut x = f("Pink Floyd", "The Wall", Some(1979), 5, "Hey You");
        x.disc = 2;
        x.disc_total = 2;
        assert_eq!(
            target(&x),
            PathBuf::from("Pink Floyd/1979 - The Wall/2-05 - Hey You.flac")
        );
    }

    #[test]
    fn no_year() {
        let p = target(&f("Radiohead", "Demo", None, 3, "Song"));
        assert_eq!(p, PathBuf::from("Radiohead/Demo/03 - Song.flac"));
    }

    #[test]
    fn compilation_pattern() {
        let mut x = f("Some Label", "Now 5", Some(2000), 7, "Hit");
        x.track_artist = "Zed".into();
        x.is_compilation = true;
        assert_eq!(
            target(&x),
            PathBuf::from("Various Artists/2000 - Now 5/07 - Zed - Hit.flac")
        );
    }

    #[test]
    fn two_digit_padding() {
        let p = target(&f("A", "B", None, 12, "T"));
        assert_eq!(p, PathBuf::from("A/B/12 - T.flac"));
        let p = target(&f("A", "B", None, 7, "T"));
        assert_eq!(p, PathBuf::from("A/B/07 - T.flac"));
    }

    fn assert_safe(p: &Path) {
        assert!(p.is_relative(), "{p:?}");
        for c in p.components() {
            assert!(matches!(c, std::path::Component::Normal(_)), "{p:?}");
            assert!(c.as_os_str().len() <= 120, "{p:?}");
        }
        assert_eq!(p.components().count(), 3, "{p:?}");
    }

    #[test]
    fn hostile_names() {
        assert_eq!(sanitize_component("AC/DC"), "AC_DC");
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("."), "_");
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("con.txt"), "_con.txt");
        assert_eq!(sanitize_component("LPT9"), "_LPT9");
        assert_eq!(sanitize_component("COM0"), "COM0");
        assert_eq!(sanitize_component("Console"), "Console");
        assert_eq!(sanitize_component("a:b?c*"), "a_b_c_");
        assert_eq!(sanitize_component("name. . "), "name");
        assert_eq!(sanitize_component("a\u{0}b\nc"), "a_b_c");
        assert_eq!(sanitize_component(""), "_");
        assert_eq!(
            sanitize_component("...And You Will Know Us"),
            "And You Will Know Us"
        );
        assert_eq!(sanitize_component("..."), "_");
        assert_eq!(sanitize_component(". .x"), "x");
        assert_eq!(file_name(".hidden", "flac"), "hidden.flac");
        assert_eq!(file_name("...", "flac"), "_.flac");
        let t = target(&f(
            "...And You Will Know Us by the Trail of Dead",
            ".hidden",
            None,
            1,
            ".t",
        ));
        assert!(
            t.components()
                .all(|c| !c.as_os_str().to_string_lossy().starts_with('.')),
            "{t:?}"
        );
        assert_eq!(sanitize_component("e\u{301}"), sanitize_component("\u{e9}"));

        let long = "é".repeat(300);
        let s = sanitize_component(&long);
        assert!(s.len() <= 120);
        assert!(std::str::from_utf8(s.as_bytes()).is_ok());

        let cases = [
            f("AC/DC", "..", Some(1980), 1, "Back in Black"),
            f("..", "..", None, 1, ".."),
            f("CON", "NUL", Some(2000), 1, "a:b?c*"),
            f("A", "B", None, 1, &"x".repeat(300)),
            f("A", "B", None, 1, &"é".repeat(300)),
            f("../..", "/etc", None, 1, "\\..\\x"),
            f("A", "B", None, 1, "ends with dot."),
        ];
        for c in &cases {
            assert_safe(&target(c));
        }
        let p = target(&cases[0]);
        assert_eq!(p, PathBuf::from("AC_DC/1980 - _/01 - Back in Black.flac"));
        let nfd = target(&f("Beyonce\u{301}", "B", None, 1, "T"));
        let nfc = target(&f("Beyonc\u{e9}", "B", None, 1, "T"));
        assert_eq!(nfd, nfc);
        let name = target(&cases[3]);
        assert!(name.file_name().unwrap().len() <= 120);
        assert!(name.to_str().unwrap().ends_with(".flac"));
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn collisions_get_suffix_deterministically() {
        let a = f("Abba", "Gold", None, 1, "Same");
        let b = f("ABBA", "gold", None, 1, "same"); // case-insensitive collision
        let c = f("Abba", "Gold", None, 1, "Same");
        let input = vec![
            (id(3), c.clone(), PathBuf::from("z/3.flac")),
            (id(1), a.clone(), PathBuf::from("z/1.flac")),
            (id(2), b.clone(), PathBuf::from("z/2.flac")),
        ];
        let out = plan(&input, &HashSet::new());
        let mut reversed = input.clone();
        reversed.reverse();
        assert_eq!(out, plan(&reversed, &HashSet::new()));
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|m| m.collision));
        let by = |n: u128| out.iter().find(|m| m.track_id == id(n)).unwrap();
        // Sorted by (to, id): "ABBA/..." sorts before "Abba/..." (uppercase B first).
        assert_eq!(by(2).to, PathBuf::from("ABBA/gold/01 - same.flac"));
        assert_eq!(by(1).to, PathBuf::from("Abba/Gold/01 - Same (2).flac"));
        assert_eq!(by(3).to, PathBuf::from("Abba/Gold/01 - Same (3).flac"));
        let keys: HashSet<_> = out.iter().map(|m| path_key(&m.to)).collect();
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn existing_paths_force_a_suffix() {
        let x = f("A", "B", None, 1, "T");
        let existing = HashSet::from([PathBuf::from("a/b/01 - t.flac")]);
        let out = plan(&[(id(1), x, PathBuf::from("in/1.flac"))], &existing);
        assert_eq!(out[0].to, PathBuf::from("A/B/01 - T (2).flac"));
        assert!(out[0].collision);
    }

    #[test]
    fn unchanged_detected() {
        let x = f("A", "B", Some(2000), 1, "T");
        let at = target(&x);
        // The track's own path is in `existing`: it must not collide with itself.
        let existing = HashSet::from([at.clone()]);
        let out = plan(&[(id(1), x, at.clone())], &existing);
        assert!(out[0].unchanged);
        assert!(!out[0].collision);
        assert_eq!(out[0].to, at);
    }

    #[test]
    fn unchanged_track_keeps_its_path_over_a_newcomer() {
        let x = f("A", "B", None, 1, "T");
        let at = target(&x);
        let out = plan(
            &[
                (id(1), x.clone(), PathBuf::from("in/new.flac")),
                (id(2), x, at.clone()),
            ],
            &HashSet::new(),
        );
        let stay = out.iter().find(|m| m.track_id == id(2)).unwrap();
        assert!(stay.unchanged && stay.to == at);
        let other = out.iter().find(|m| m.track_id == id(1)).unwrap();
        assert_eq!(other.to, PathBuf::from("A/B/01 - T (2).flac"));
    }

    #[test]
    fn sidecars_follow_album() {
        let x = f("Radiohead", "OK Computer", Some(1997), 1, "Airbag");
        let y = f("Radiohead", "OK Computer", Some(1997), 2, "Lucky");
        let moves = plan(
            &[
                (id(1), x, PathBuf::from("messy/ok/a.flac")),
                (id(2), y, PathBuf::from("messy/ok/b.flac")),
            ],
            &HashSet::new(),
        );
        let sidecars = vec![
            PathBuf::from("messy/ok/folder.jpg"),
            PathBuf::from("messy/ok/rip.log"),
            PathBuf::from("messy/ok/ok.cue"),
            PathBuf::from("messy/other/cover.jpg"),
        ];
        assert!(is_sidecar(&sidecars[0]) && !is_sidecar(Path::new("a.flac")));
        let out = plan_sidecars(&moves, &sidecars, &HashSet::new());
        assert_eq!(out.len(), 3);
        for s in &out {
            assert_eq!(
                s.to.parent().unwrap(),
                Path::new("Radiohead/1997 - OK Computer")
            );
            assert!(!s.collision);
        }
        assert!(out.iter().any(|s| s.to.ends_with("folder.jpg")));
    }

    #[test]
    fn disc_prefix_without_total() {
        let mut x = f("A", "B", None, 5, "T");
        x.disc = 2;
        x.disc_total = 0;
        assert_eq!(target(&x), PathBuf::from("A/B/2-05 - T.flac"));

        // Discs 1 and 2 present, no totals: both get the prefix in `plan`.
        let mut d1 = f("A", "B", Some(2000), 1, "One");
        d1.disc = 1;
        d1.disc_total = 0;
        let mut d2 = f("A", "B", Some(2000), 1, "Two");
        d2.disc = 2;
        d2.disc_total = 0;
        let out = plan(
            &[
                (id(1), d1, PathBuf::from("in/1.flac")),
                (id(2), d2, PathBuf::from("in/2.flac")),
            ],
            &HashSet::new(),
        );
        assert_eq!(out[0].to, PathBuf::from("A/2000 - B/1-01 - One.flac"));
        assert_eq!(out[1].to, PathBuf::from("A/2000 - B/2-01 - Two.flac"));
    }

    #[test]
    fn replan_of_organized_library_is_all_unchanged() {
        let a = f("Abba", "Gold", Some(1992), 1, "Same");
        let b = f("ABBA", "gold", Some(1992), 1, "same");
        let c = f("Abba", "Gold", Some(1992), 2, "Other");
        let input = vec![
            (id(1), a, PathBuf::from("m/1.flac")),
            (id(2), b, PathBuf::from("m/2.flac")),
            (id(3), c, PathBuf::from("m/3.flac")),
        ];
        let first = plan(&input, &HashSet::new());
        assert!(first.iter().any(|m| m.collision));
        let moved: HashMap<Uuid, PathBuf> =
            first.iter().map(|m| (m.track_id, m.to.clone())).collect();
        let applied: Vec<_> = input
            .iter()
            .map(|(i, fl, _)| (*i, fl.clone(), moved[i].clone()))
            .collect();
        // The organized files all exist now.
        let existing: HashSet<PathBuf> = moved.values().cloned().collect();
        for ex in [HashSet::new(), existing] {
            let second = plan(&applied, &ex);
            assert!(
                second.iter().all(|m| m.unchanged && !m.collision),
                "{second:?}"
            );
            for m in &second {
                assert_eq!(m.to, moved[&m.track_id]);
            }
        }
    }

    #[test]
    fn case_only_rename_is_flagged() {
        let x = f("Abba", "Gold", None, 1, "T");
        let out = plan(
            &[(id(1), x, PathBuf::from("abba/gold/01 - T.flac"))],
            &HashSet::new(),
        );
        assert!(!out[0].unchanged && out[0].case_only);
        assert_eq!(out[0].to, PathBuf::from("Abba/Gold/01 - T.flac"));
    }

    #[test]
    fn long_title_with_suffix_and_odd_extension_stay_valid() {
        let mut x = f("A", "B", None, 1, &"é".repeat(300));
        x.ext = "MP3".into();
        let out = plan(
            &[
                (id(1), x.clone(), PathBuf::from("in/1.mp3")),
                (id(2), x, PathBuf::from("in/2.mp3")),
            ],
            &HashSet::new(),
        );
        let name = out[1].to.file_name().unwrap().to_str().unwrap();
        assert!(name.len() <= 120, "{}", name.len());
        assert!(name.ends_with(" (2).mp3"), "{name}");
        assert!(out[0].to.to_str().unwrap().ends_with(".mp3"));
    }

    #[test]
    fn nfd_and_nfc_collide_in_plan() {
        let nfd = f("Beyonce\u{301}", "B", None, 1, "T");
        let nfc = f("Beyonc\u{e9}", "B", None, 1, "T");
        let out = plan(
            &[
                (id(1), nfd, PathBuf::from("in/1.flac")),
                (id(2), nfc, PathBuf::from("in/2.flac")),
            ],
            &HashSet::new(),
        );
        assert_eq!(out[0].to.parent(), Some(Path::new("Beyonc\u{e9}/B")));
        assert_eq!(out[1].to.file_name().unwrap(), "01 - T (2).flac");
        assert!(out.iter().all(|m| m.collision));
    }

    #[test]
    fn nonpositive_year_is_no_year() {
        assert_eq!(
            target(&f("A", "B", Some(0), 1, "T")),
            PathBuf::from("A/B/01 - T.flac")
        );
        assert_eq!(
            target(&f("A", "B", Some(-5), 1, "T")),
            PathBuf::from("A/B/01 - T.flac")
        );
    }

    #[test]
    fn sidecar_collisions_get_suffix() {
        let x = f("A", "B", None, 1, "T");
        let moves = plan(&[(id(1), x, PathBuf::from("a/1.flac"))], &HashSet::new());
        let existing = HashSet::from([PathBuf::from("A/B/cover.jpg")]);
        let out = plan_sidecars(&moves, &[PathBuf::from("a/Cover.jpg")], &existing);
        assert_eq!(out[0].to, PathBuf::from("A/B/Cover (2).jpg"));
        assert!(out[0].collision);
    }
}
