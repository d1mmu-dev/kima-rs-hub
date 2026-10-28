//! M4A/MP4: the §19.8 atom column, through lofty's concrete `Ilst`.
//!
//! Keys are spelled as the atoms: a fourcc with `©` for byte `0xA9` (`©nam`, `trkn`), or
//! `----:<mean>:<name>` for a freeform atom. Every value is one `data` atom; `trkn`/`disk` hold
//! `n/t` (`n` when the total is 0) as the binary pair, `cpil` `1`/`0` as a one-byte integer,
//! `©gen` one value with the genres joined by `; `.
//!
//! What lofty does on save, and what is checked here so nothing else is lost: only
//! `moov.udta.meta.ilst` is rebuilt (with the `free` atoms next to it; a missing `udta`/`meta`
//! is created), every `data` atom is re-encoded with locale 0, and an item lofty cannot read
//! (an unknown type set, a `covr` of an unknown image type, a value that is not valid text) is
//! dropped while reading. When `moov` grows, lofty moves what follows it and corrects the
//! `stco`/`co64` chunk offsets, but only the first `moof`'s `tfhd`, so a fragmented file is
//! refused. The checks parse the file as stored: every top-level atom (the `mdat` byte for byte),
//! every leaf atom in `moov` with the chunk offsets taken relative to the atom they point into,
//! and every `ilst` item byte for byte.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::mp4::{Atom, AtomData, AtomIdent, DataType, Ilst, Mp4File};
use lofty::tag::TagExt;
use sha2::{Digest, Sha256};

use super::values::{NumberPair, TagValues};
use super::{NativeTags, TagWriteError};

const ITUNES: &str = "----:com.apple.iTunes:";
const ORIGINAL_DATE: &str = "----:com.apple.iTunes:ORIGINALDATE";
const MB_TRACK: &str = "----:com.apple.iTunes:MusicBrainz Track Id";
const MB_RELEASE_TRACK: &str = "----:com.apple.iTunes:MusicBrainz Release Track Id";
const MB_ALBUM: &str = "----:com.apple.iTunes:MusicBrainz Album Id";
const MB_RELEASE_GROUP: &str = "----:com.apple.iTunes:MusicBrainz Release Group Id";
const MB_ARTIST: &str = "----:com.apple.iTunes:MusicBrainz Artist Id";
const MB_ALBUM_ARTIST: &str = "----:com.apple.iTunes:MusicBrainz Album Artist Id";

/// Every atom this module may touch: the §19.8 MP4 column.
const KEYS: &[&str] = &[
    "©nam",
    "©ART",
    "soar",
    "©alb",
    "aART",
    "soaa",
    "trkn",
    "disk",
    "©day",
    ORIGINAL_DATE,
    "©gen",
    "cpil",
    MB_TRACK,
    MB_RELEASE_TRACK,
    MB_ALBUM,
    MB_RELEASE_GROUP,
    MB_ARTIST,
    MB_ALBUM_ARTIST,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    /// `trkn`, `disk`.
    Pair,
    /// `cpil`.
    Flag,
}

fn kind(key: &str) -> Result<Kind, TagWriteError> {
    if !KEYS.contains(&key) {
        return Err(TagWriteError::Verify(format!(
            "not a writable MP4 atom: {key}"
        )));
    }
    Ok(match key {
        "trkn" | "disk" => Kind::Pair,
        "cpil" => Kind::Flag,
        _ => Kind::Text,
    })
}

fn check_keys(keys: &[String]) -> Result<(), TagWriteError> {
    keys.iter().try_for_each(|k| kind(k).map(|_| ()))
}

/// A fourcc as a key: each byte as the Latin-1 character (`0xA9` is `©`).
fn fourcc_str(f: &[u8]) -> String {
    f.iter().map(|b| char::from(*b)).collect()
}

/// The lofty identifier of a key of [`KEYS`].
fn ident(key: &str) -> AtomIdent<'static> {
    match key.strip_prefix(ITUNES) {
        Some(name) => AtomIdent::Freeform {
            mean: "com.apple.iTunes".into(),
            name: name.to_string().into(),
        },
        None => {
            let mut f = [0u8; 4];
            for (o, c) in f.iter_mut().zip(key.chars()) {
                *o = u8::try_from(u32::from(c)).unwrap_or(b'?');
            }
            AtomIdent::Fourcc(f)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Mapping

fn pair(p: NumberPair) -> String {
    match p.total {
        Some(t) => format!("{}/{}", p.number, t),
        None => p.number.to_string(),
    }
}

pub(super) fn native(v: &TagValues) -> NativeTags {
    let mut out = NativeTags::new();
    let mut put = |key: &str, val: Option<Vec<String>>| {
        if let Some(val) = val {
            out.insert(key.to_string(), val);
        }
    };
    let one = |s: &Option<String>| s.clone().filter(|s| !s.is_empty()).map(|s| vec![s]);
    put("©nam", one(&v.title));
    put("©ART", one(&v.artist));
    put("soar", one(&v.artist_sort));
    put("©alb", one(&v.album));
    put("aART", one(&v.album_artist));
    put("soaa", one(&v.album_artist_sort));
    put("trkn", v.track.map(|p| vec![pair(p)]));
    put("disk", v.disc.map(|p| vec![pair(p)]));
    put("©day", one(&v.date));
    put(ORIGINAL_DATE, one(&v.original_date));
    put(
        "©gen",
        v.genres.as_ref().and_then(|g| {
            let g: Vec<&str> = g
                .iter()
                .map(String::as_str)
                .filter(|s| !s.is_empty())
                .collect();
            (!g.is_empty()).then(|| vec![g.join("; ")])
        }),
    );
    put(
        "cpil",
        v.compilation
            .map(|c| if c { vec!["1".to_string()] } else { Vec::new() }),
    );
    let id = |u: &Option<uuid::Uuid>| u.map(|u| vec![u.to_string()]);
    let ids = |u: &Option<Vec<uuid::Uuid>>| {
        u.as_ref()
            .filter(|u| !u.is_empty())
            .map(|u| u.iter().map(ToString::to_string).collect())
    };
    put(MB_TRACK, id(&v.recording_mbid));
    put(MB_RELEASE_TRACK, id(&v.release_track_mbid));
    put(MB_ALBUM, id(&v.release_mbid));
    put(MB_RELEASE_GROUP, id(&v.release_group_mbid));
    put(MB_ARTIST, ids(&v.artist_mbids));
    put(MB_ALBUM_ARTIST, ids(&v.album_artist_mbids));
    out
}

// ---------------------------------------------------------------------------------------------
// The file as stored

fn sha(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

fn refuse(what: &str) -> TagWriteError {
    TagWriteError::Verify(format!("{what}; nothing written"))
}

#[derive(Debug, Clone, Copy)]
struct Box_ {
    start: usize,
    /// Start of the content (after the header).
    body: usize,
    end: usize,
    fourcc: [u8; 4],
}

fn be32(b: &[u8]) -> u64 {
    u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// The atoms in `b[from..to]`.
fn boxes(b: &[u8], from: usize, to: usize) -> Result<Vec<Box_>, TagWriteError> {
    let bad = || refuse("the MP4 atom structure is damaged");
    let mut out = Vec::new();
    let mut pos = from;
    while pos < to {
        let h = b
            .get(pos..pos + 8)
            .filter(|_| pos + 8 <= to)
            .ok_or_else(bad)?;
        let (len, hdr) = match be32(h) {
            0 => ((to - pos) as u64, 8),
            1 => (be64(b.get(pos + 8..pos + 16).ok_or_else(bad)?), 16),
            n => (n, 8),
        };
        let len = usize::try_from(len).map_err(|_| bad())?;
        if len < hdr || pos.checked_add(len).is_none_or(|e| e > to) {
            return Err(bad());
        }
        out.push(Box_ {
            start: pos,
            body: pos + hdr,
            end: pos + len,
            fourcc: [h[4], h[5], h[6], h[7]],
        });
        pos += len;
    }
    Ok(out)
}

const CONTAINERS: &[&[u8; 4]] = &[
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"udta", b"meta", b"edts", b"dinf", b"mvex",
];

/// The `hdlr` lofty writes when it creates `udta.meta`: not a loss when it appears.
const LOFTY_HDLR: [u8; 25] = [
    0, 0, 0, 0, 0, 0, 0, 0, b'm', b'd', b'i', b'r', b'a', b'p', b'p', b'l', 0, 0, 0, 0, 0, 0, 0, 0,
    0,
];

/// One `ilst` item as stored.
struct Item {
    key: String,
    raw: (usize, usize),
    /// `(type, payload)` of each `data` atom.
    data: Vec<(u32, (usize, usize))>,
}

struct Raw {
    bytes: Vec<u8>,
    /// `(label, digest)` of every leaf atom outside `ilst`.
    parts: Vec<(String, [u8; 32])>,
    items: Vec<Item>,
}

fn full_atom_payload<'a>(b: &'a [u8], bx: &Box_) -> &'a [u8] {
    &b[(bx.body + 4).min(bx.end)..bx.end]
}

fn parse_item(b: &[u8], bx: &Box_) -> Result<Item, TagWriteError> {
    let mut key = fourcc_str(&bx.fourcc);
    let (mut mean, mut name) = (None, None);
    let mut data = Vec::new();
    for c in boxes(b, bx.body, bx.end)? {
        match &c.fourcc {
            b"mean" => mean = Some(String::from_utf8_lossy(full_atom_payload(b, &c)).into_owned()),
            b"name" => name = Some(String::from_utf8_lossy(full_atom_payload(b, &c)).into_owned()),
            b"data" if c.end >= c.body + 8 => {
                let ty = be32(&b[c.body..c.body + 4]) as u32;
                data.push((ty, (c.body + 8, c.end)));
            }
            _ => {}
        }
    }
    if &bx.fourcc == b"----" {
        key = format!(
            "----:{}:{}",
            mean.unwrap_or_default(),
            name.unwrap_or_default()
        );
    }
    Ok(Item {
        key,
        raw: (bx.start, bx.end),
        data,
    })
}

/// The chunk offsets of a `stco`/`co64`, each as (index of the top-level atom it points into,
/// offset inside it).
fn offsets(b: &[u8], bx: &Box_, top: &[Box_], wide: bool) -> Vec<u8> {
    let body = &b[bx.body..bx.end];
    let mut out = Vec::new();
    if body.len() < 8 {
        return body.to_vec();
    }
    let count = be32(&body[4..8]) as usize;
    let size = if wide { 8 } else { 4 };
    for i in 0..count {
        let at = 8 + i * size;
        let Some(e) = body.get(at..at + size) else {
            out.extend(b"<truncated>");
            break;
        };
        let off = if wide { be64(e) } else { be32(e) } as usize;
        let (idx, rel) = top
            .iter()
            .enumerate()
            .find(|(_, t)| t.start <= off && off < t.end)
            .map_or((usize::MAX, off), |(i, t)| (i, off - t.start));
        out.extend((idx as u64).to_be_bytes());
        out.extend((rel as u64).to_be_bytes());
    }
    out
}

fn walk(
    b: &[u8],
    parent: &Box_,
    path: &str,
    top: &[Box_],
    raw: &mut Vec<(String, [u8; 32])>,
    items: &mut Vec<Item>,
) -> Result<(), TagWriteError> {
    let mut from = parent.body;
    // A full `meta` (ISO) has version and flags before its children; a QuickTime one does not.
    if &parent.fourcc == b"meta" && b.get(from..from + 4) == Some(&[0, 0, 0, 0]) {
        from += 4;
    }
    for c in boxes(b, from, parent.end)? {
        let label = format!("{path}.{}", fourcc_str(&c.fourcc));
        match &c.fourcc {
            b"free" | b"skip" => {}
            b"ilst" => {
                for it in boxes(b, c.body, c.end)? {
                    if !matches!(&it.fourcc, b"free" | b"skip") {
                        items.push(parse_item(b, &it)?);
                    }
                }
            }
            b"stco" => raw.push((label, sha(&offsets(b, &c, top, false)))),
            b"co64" => raw.push((label, sha(&offsets(b, &c, top, true)))),
            b"hdlr" if path.ends_with("udta.meta") && b[c.body..c.end] == LOFTY_HDLR => {}
            f if CONTAINERS.contains(&f) => walk(b, &c, &label, top, raw, items)?,
            _ => raw.push((label, sha(&b[c.body..c.end]))),
        }
    }
    Ok(())
}

fn parse(path: &Path) -> Result<Raw, TagWriteError> {
    let bytes = std::fs::read(path)?;
    let all = boxes(&bytes, 0, bytes.len())?;
    if all.iter().any(|t| &t.fourcc == b"moof") {
        return Err(refuse("fragmented MP4 files are not written"));
    }
    // Top-level `free` atoms are padding; the others are indexed for the chunk offsets.
    let top: Vec<Box_> = all
        .into_iter()
        .filter(|t| !matches!(&t.fourcc, b"free" | b"skip"))
        .collect();
    if top.iter().filter(|t| &t.fourcc == b"moov").count() != 1 {
        return Err(refuse("the file does not have exactly one moov atom"));
    }
    let mut parts = Vec::new();
    let mut items = Vec::new();
    for t in &top {
        if &t.fourcc == b"moov" {
            walk(&bytes, t, "moov", &top, &mut parts, &mut items)?;
        } else {
            parts.push((fourcc_str(&t.fourcc), sha(&bytes[t.body..t.end])));
        }
    }
    Ok(Raw {
        bytes,
        parts,
        items,
    })
}

// ---------------------------------------------------------------------------------------------
// Reading

fn utf16be(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn value(k: Kind, ty: u32, payload: &[u8]) -> String {
    match k {
        Kind::Pair if payload.len() >= 6 => {
            let n = u16::from_be_bytes([payload[2], payload[3]]);
            let t = u16::from_be_bytes([payload[4], payload[5]]);
            if t == 0 {
                n.to_string()
            } else {
                format!("{n}/{t}")
            }
        }
        Kind::Flag => if payload.iter().any(|x| *x != 0) {
            "1"
        } else {
            "0"
        }
        .to_string(),
        _ if ty == 2 => utf16be(payload),
        _ => String::from_utf8_lossy(payload).into_owned(),
    }
}

pub(super) fn read(path: &Path, keys: &[String]) -> Result<NativeTags, TagWriteError> {
    let raw = parse(path)?;
    let mut out = NativeTags::new();
    for key in keys {
        let k = kind(key)?;
        let vals = raw
            .items
            .iter()
            .filter(|it| &it.key == key)
            .flat_map(|it| it.data.iter())
            .map(|(ty, (s, e))| value(k, *ty, &raw.bytes[*s..*e]))
            .collect();
        out.insert(key.clone(), vals);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Writing

fn options() -> ParseOptions {
    ParseOptions::new()
        .read_properties(false)
        .implicit_conversions(false)
}

fn data(k: Kind, key: &str, v: &str) -> Result<AtomData, TagWriteError> {
    Ok(match k {
        Kind::Text => AtomData::UTF8(v.to_string()),
        Kind::Flag => AtomData::Bool(v != "0"),
        Kind::Pair => {
            let bad = || TagWriteError::Verify(format!("{key}: not a number pair: {v}"));
            let (n, t) = v.split_once('/').unwrap_or((v, "0"));
            let n: u16 = n.trim().parse().map_err(|_| bad())?;
            let t: u16 = t.trim().parse().map_err(|_| bad())?;
            let (n, t) = (n.to_be_bytes(), t.to_be_bytes());
            // `trkn` is 8 bytes, `disk` 6 (as iTunes and Picard write them).
            let mut d = vec![0, 0, n[0], n[1], t[0], t[1]];
            if key == "trkn" {
                d.extend([0, 0]);
            }
            AtomData::Unknown {
                code: DataType::Reserved,
                data: d,
            }
        }
    })
}

/// Replaces the atoms of `new` in the file's `ilst` (creating one if there is none) and saves.
/// Every other atom keeps its place; the new ones go last, one `data` atom per value.
pub(super) fn write(path: &Path, new: &NativeTags) -> Result<(), TagWriteError> {
    let mut f = File::open(path)?;
    let file = Mp4File::read_from(&mut f, options())?;
    drop(f);
    let mut ilst = file.ilst().cloned().unwrap_or_else(Ilst::new);
    for (key, vals) in new {
        let k = kind(key)?;
        let id = ident(key);
        ilst.retain(|a| a.ident() != &id);
        let data = vals
            .iter()
            .map(|v| data(k, key, v))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(atom) = Atom::from_collection(id, data) {
            ilst.insert(atom);
        }
    }
    ilst.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Preservation check

/// One `(label, digest)` per atom of the file that is not one of `keys`: top-level atoms,
/// leaf atoms in `moov` (chunk offsets made relative), and `ilst` items (all items with one
/// key, in order, byte for byte); sorted.
pub(super) fn untouched(
    path: &Path,
    keys: &[String],
) -> Result<Vec<(String, [u8; 32])>, TagWriteError> {
    check_keys(keys)?;
    let raw = parse(path)?;
    let mut by_key: BTreeMap<&str, Sha256> = BTreeMap::new();
    for it in raw.items.iter().filter(|it| !keys.contains(&it.key)) {
        let bytes = &raw.bytes[it.raw.0..it.raw.1];
        let h = by_key.entry(&it.key).or_default();
        h.update((bytes.len() as u64).to_be_bytes());
        h.update(bytes);
    }
    let mut out = raw.parts;
    out.extend(
        by_key
            .into_iter()
            .map(|(k, h)| (format!("ilst {k}"), h.finalize().into())),
    );
    out.sort_unstable();
    Ok(out)
}

/// The atoms as stored, with counts, the `ilst` items `keys` own left out.
pub(super) fn raw_untouched(
    path: &Path,
    keys: &[String],
) -> Result<BTreeMap<String, usize>, TagWriteError> {
    check_keys(keys)?;
    let raw = parse(path)?;
    let mut out = BTreeMap::new();
    for label in raw
        .items
        .iter()
        .filter(|it| !keys.contains(&it.key))
        .map(|it| format!("ilst {}", it.key))
        .chain(raw.parts.into_iter().map(|(l, _)| l))
    {
        *out.entry(label).or_insert(0) += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping() {
        let v = TagValues {
            track: Some(NumberPair {
                number: 3,
                total: Some(12),
            }),
            disc: Some(NumberPair {
                number: 1,
                total: None,
            }),
            genres: Some(vec!["Rock".into(), String::new(), "Pop".into()]),
            compilation: Some(false),
            ..TagValues::default()
        };
        let n = native(&v);
        assert_eq!(n["trkn"], vec!["3/12"]);
        assert_eq!(n["disk"], vec!["1"]);
        assert_eq!(n["©gen"], vec!["Rock; Pop"]);
        assert!(n["cpil"].is_empty());
    }

    #[test]
    fn keys_and_idents() {
        assert!(kind("©nam").is_ok());
        assert!(kind("covr").is_err());
        assert!(kind("----:com.apple.iTunes:REPLAYGAIN_TRACK_GAIN").is_err());
        assert_eq!(ident("©nam"), AtomIdent::Fourcc(*b"\xa9nam"));
        assert_eq!(fourcc_str(b"\xa9nam"), "©nam");
        assert_eq!(
            ident(MB_ARTIST),
            AtomIdent::Freeform {
                mean: "com.apple.iTunes".into(),
                name: "MusicBrainz Artist Id".into()
            }
        );
    }

    #[test]
    fn pairs_round_trip() {
        for (key, v, len) in [("trkn", "3/12", 8), ("disk", "1", 6)] {
            let AtomData::Unknown { data, .. } = data(Kind::Pair, key, v).unwrap() else {
                panic!()
            };
            assert_eq!(data.len(), len);
            assert_eq!(value(Kind::Pair, 0, &data), v);
        }
        assert!(data(Kind::Pair, "trkn", "x/1").is_err());
        assert_eq!(value(Kind::Flag, 21, &[0]), "0");
        assert_eq!(value(Kind::Flag, 21, &[1]), "1");
    }
}
