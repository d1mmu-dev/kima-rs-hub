//! FLAC, Ogg Vorbis and Opus: the §19.8 Vorbis comment column, through lofty's concrete
//! `VorbisComments`.
//!
//! One field per value; field names are matched without regard to case (the Vorbis comment
//! rule) and written upper-case. `TRACKTOTAL`/`DISCTOTAL` are always written with the number;
//! `TOTALTRACKS`/`TOTALDISCS` only when the file already has them ([`super::native_for_file`]).
//!
//! What lofty does on save, and what is checked here so nothing else is lost:
//! - FLAC: the `VORBIS_COMMENT` and `PICTURE` blocks are rebuilt (comments first, then the
//!   pictures, after every other block); `STREAMINFO`, `SEEKTABLE`, `CUESHEET`, `APPLICATION` and
//!   `PADDING` blocks are kept as they are (no padding is added: `preferred_padding(0)`), and an
//!   ID3v2 tag before `fLaC` is skipped, not rewritten. Two `VORBIS_COMMENT` blocks are refused
//!   (lofty keeps only the last).
//! - Ogg: the header packets are re-paged and every later page renumbered; pages of a second
//!   logical stream (a multiplexed or chained file) would be renumbered as if they were the
//!   first stream's, so such a file is refused. Anything after the last page would be dropped.
//! - Comments lofty cannot read (no `=`, an invalid name, a value that is not UTF-8) are dropped
//!   while reading; `METADATA_BLOCK_PICTURE` fields are re-encoded (and in FLAC moved into
//!   `PICTURE` blocks), `COVERART` is converted.
//!
//! The checks therefore parse the file as stored: every comment field (by name, in order, byte
//! for byte), the vendor string, every FLAC metadata block and the audio frames, and the Ogg
//! identification/setup headers and audio pages (all but the page number and checksum).

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::flac::FlacFile;
use lofty::ogg::tag::VorbisComments;
use lofty::ogg::{OggPictureStorage, OpusFile, VorbisFile};
use lofty::tag::TagExt;
use sha2::{Digest, Sha256};

use super::values::{NumberPair, TagValues};
use super::{Container, NativeTags, TagWriteError};

/// Every field this module may touch: the §19.8 Vorbis column. A key outside this list (from a
/// corrupted journal, say) is refused, never written.
const KEYS: &[&str] = &[
    "TITLE",
    "ARTIST",
    "ARTISTSORT",
    "ALBUM",
    "ALBUMARTIST",
    "ALBUMARTISTSORT",
    "TRACKNUMBER",
    "TRACKTOTAL",
    "TOTALTRACKS",
    "DISCNUMBER",
    "DISCTOTAL",
    "TOTALDISCS",
    "DATE",
    "ORIGINALDATE",
    "GENRE",
    "COMPILATION",
    "MUSICBRAINZ_TRACKID",
    "MUSICBRAINZ_RELEASETRACKID",
    "MUSICBRAINZ_ALBUMID",
    "MUSICBRAINZ_RELEASEGROUPID",
    "MUSICBRAINZ_ARTISTID",
    "MUSICBRAINZ_ALBUMARTISTID",
];

fn check_key(key: &str) -> Result<(), TagWriteError> {
    if KEYS.contains(&key) {
        Ok(())
    } else {
        Err(TagWriteError::Verify(format!(
            "not a writable Vorbis comment: {key}"
        )))
    }
}

fn check_keys(keys: &[String]) -> Result<(), TagWriteError> {
    keys.iter().try_for_each(|k| check_key(k))
}

// ---------------------------------------------------------------------------------------------
// Mapping

pub(super) fn native(v: &TagValues) -> NativeTags {
    let mut out = NativeTags::new();
    let mut put = |key: &str, val: Option<Vec<String>>| {
        if let Some(val) = val {
            out.insert(key.to_string(), val);
        }
    };
    let one = |s: &Option<String>| s.clone().filter(|s| !s.is_empty()).map(|s| vec![s]);
    let many = |vs: Vec<String>| {
        let vs: Vec<String> = vs.into_iter().filter(|s| !s.is_empty()).collect();
        (!vs.is_empty()).then_some(vs)
    };
    put("TITLE", one(&v.title));
    put("ARTIST", one(&v.artist));
    put("ARTISTSORT", one(&v.artist_sort));
    put("ALBUM", one(&v.album));
    put("ALBUMARTIST", one(&v.album_artist));
    put("ALBUMARTISTSORT", one(&v.album_artist_sort));
    let number = |p: NumberPair| vec![p.number.to_string()];
    let total = |p: NumberPair| p.total.map(|t| t.to_string()).into_iter().collect();
    put("TRACKNUMBER", v.track.map(number));
    put("TRACKTOTAL", v.track.map(total));
    put("DISCNUMBER", v.disc.map(number));
    put("DISCTOTAL", v.disc.map(total));
    put("DATE", one(&v.date));
    put("ORIGINALDATE", one(&v.original_date));
    put("GENRE", v.genres.clone().and_then(many));
    put(
        "COMPILATION",
        v.compilation
            .map(|c| if c { vec!["1".to_string()] } else { Vec::new() }),
    );
    let id = |u: &Option<uuid::Uuid>| u.map(|u| vec![u.to_string()]);
    let ids = |u: &Option<Vec<uuid::Uuid>>| {
        u.as_ref()
            .and_then(|u| many(u.iter().map(ToString::to_string).collect()))
    };
    put("MUSICBRAINZ_TRACKID", id(&v.recording_mbid));
    put("MUSICBRAINZ_RELEASETRACKID", id(&v.release_track_mbid));
    put("MUSICBRAINZ_ALBUMID", id(&v.release_mbid));
    put("MUSICBRAINZ_RELEASEGROUPID", id(&v.release_group_mbid));
    put("MUSICBRAINZ_ARTISTID", ids(&v.artist_mbids));
    put("MUSICBRAINZ_ALBUMARTISTID", ids(&v.album_artist_mbids));
    out
}

/// Adds `TOTALTRACKS`/`TOTALDISCS` (the same values as `TRACKTOTAL`/`DISCTOTAL`) when the file
/// already has them.
pub(super) fn add_present_totals(
    path: &Path,
    container: Container,
    tags: &mut NativeTags,
) -> Result<(), TagWriteError> {
    let raw = parse(path, container)?;
    for (total, alias) in [("TRACKTOTAL", "TOTALTRACKS"), ("DISCTOTAL", "TOTALDISCS")] {
        if let Some(v) = tags.get(total).cloned()
            && raw.fields.iter().any(|f| f.label == alias)
        {
            tags.insert(alias.to_string(), v);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The file as stored

fn sha(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

fn damaged(what: &str) -> TagWriteError {
    TagWriteError::Verify(format!("{what}; nothing written"))
}

/// One comment field as stored.
struct Field {
    /// The upper-cased name; `<field without =>` or `<field with an invalid name>` for one lofty
    /// cannot read.
    label: String,
    value: Vec<u8>,
    raw: Vec<u8>,
}

/// What the checks compare: the comments and every other part of the file.
struct Raw {
    vendor: Vec<u8>,
    fields: Vec<Field>,
    /// `(label, digest)` of every non-comment part (FLAC blocks and frames, Ogg headers and
    /// pages).
    parts: Vec<(String, [u8; 32])>,
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn u32le(&mut self) -> Option<u32> {
        self.take(4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn rest(&self) -> &'a [u8] {
        &self.b[self.pos.min(self.b.len())..]
    }
}

fn valid_name(k: &[u8]) -> bool {
    !k.is_empty() && k.iter().all(|c| (0x20..=0x7D).contains(c) && *c != b'=')
}

/// Vendor, fields and what follows them, from a comment packet without its signature.
fn parse_comments(b: &[u8]) -> Option<(Vec<u8>, Vec<Field>, &[u8])> {
    let mut r = Reader { b, pos: 0 };
    let vlen = r.u32le()? as usize;
    let vendor = r.take(vlen)?.to_vec();
    let count = r.u32le()?;
    let mut fields = Vec::new();
    for _ in 0..count {
        let len = r.u32le()? as usize;
        let raw = r.take(len)?;
        let (label, value) = match raw.iter().position(|c| *c == b'=') {
            None => ("<field without =>".to_string(), Vec::new()),
            Some(i) if !valid_name(&raw[..i]) => (
                "<field with an invalid name>".to_string(),
                raw[i + 1..].to_vec(),
            ),
            Some(i) => (
                String::from_utf8_lossy(&raw[..i]).to_ascii_uppercase(),
                raw[i + 1..].to_vec(),
            ),
        };
        fields.push(Field {
            label,
            value,
            raw: raw.to_vec(),
        });
    }
    Some((vendor, fields, r.rest()))
}

fn parse(path: &Path, container: Container) -> Result<Raw, TagWriteError> {
    let bytes = std::fs::read(path)?;
    match container {
        Container::Flac => parse_flac(&bytes),
        Container::Ogg | Container::Opus => parse_ogg(&bytes, container),
        _ => Err(TagWriteError::Verify(
            "not a Vorbis comment container".into(),
        )),
    }
}

fn synchsafe(b: &[u8]) -> usize {
    b.iter()
        .fold(0usize, |acc, x| (acc << 7) | usize::from(x & 0x7F))
}

fn flac_block_name(ty: u8) -> String {
    match ty {
        0 => "STREAMINFO".into(),
        1 => "PADDING".into(),
        2 => "APPLICATION".into(),
        3 => "SEEKTABLE".into(),
        4 => "VORBIS_COMMENT".into(),
        5 => "CUESHEET".into(),
        6 => "PICTURE".into(),
        n => format!("block type {n}"),
    }
}

fn parse_flac(b: &[u8]) -> Result<Raw, TagWriteError> {
    let mut parts = Vec::new();
    let mut pos = 0;
    if b.len() >= 10 && &b[..3] == b"ID3" {
        let footer = if b[5] & 0x10 != 0 { 10 } else { 0 };
        pos = (10 + synchsafe(&b[6..10]) + footer).min(b.len());
        parts.push(("ID3v2 tag before fLaC".to_string(), sha(&b[..pos])));
    }
    if b.get(pos..pos + 4) != Some(b"fLaC") {
        return Err(damaged("the file has no FLAC stream marker"));
    }
    pos += 4;
    let mut comments = Vec::new();
    loop {
        let hdr = b
            .get(pos..pos + 4)
            .ok_or_else(|| damaged("the FLAC metadata is damaged"))?;
        let (last, ty) = (hdr[0] & 0x80 != 0, hdr[0] & 0x7F);
        let len = (usize::from(hdr[1]) << 16) | (usize::from(hdr[2]) << 8) | usize::from(hdr[3]);
        let content = b
            .get(pos + 4..pos + 4 + len)
            .ok_or_else(|| damaged("the FLAC metadata is damaged"))?;
        pos += 4 + len;
        if ty == 4 {
            comments.push(content);
        } else {
            parts.push((format!("FLAC {} block", flac_block_name(ty)), sha(content)));
        }
        if last {
            break;
        }
    }
    parts.push(("FLAC audio frames".to_string(), sha(&b[pos..])));
    if comments.len() > 1 {
        return Err(damaged(&format!(
            "the file has {} VORBIS_COMMENT blocks",
            comments.len()
        )));
    }
    let (vendor, fields) = match comments.first() {
        // An empty block holds nothing (lofty skips it).
        Some(c) if !c.is_empty() => {
            let (vendor, fields, _) =
                parse_comments(c).ok_or_else(|| damaged("the Vorbis comments are damaged"))?;
            (vendor, fields)
        }
        _ => (Vec::new(), Vec::new()),
    };
    Ok(Raw {
        vendor,
        fields,
        parts,
    })
}

struct Page<'a> {
    flags: u8,
    granule: &'a [u8],
    serial: &'a [u8],
    lacing: &'a [u8],
    payload: &'a [u8],
}

fn parse_ogg(b: &[u8], container: Container) -> Result<Raw, TagWriteError> {
    let mut pages = Vec::new();
    let mut pos = 0;
    while pos < b.len() {
        let Some(hdr) = b.get(pos..pos + 27).filter(|h| &h[..4] == b"OggS") else {
            break;
        };
        let nseg = usize::from(hdr[26]);
        let Some(lacing) = b.get(pos + 27..pos + 27 + nseg) else {
            break;
        };
        let size: usize = lacing.iter().map(|x| usize::from(*x)).sum();
        let start = pos + 27 + nseg;
        let Some(payload) = b.get(start..start + size) else {
            break;
        };
        pages.push(Page {
            flags: hdr[5],
            granule: &hdr[6..14],
            serial: &hdr[14..18],
            lacing,
            payload,
        });
        pos = start + size;
    }
    let first = pages
        .first()
        .ok_or_else(|| damaged("the file has no Ogg page"))?;
    if pages
        .iter()
        .enumerate()
        .any(|(i, p)| p.serial != first.serial || (i > 0 && p.flags & 0x02 != 0))
    {
        return Err(damaged(
            "multi-stream and chained Ogg files are not written",
        ));
    }

    // The header packets, and the first page after them.
    let wanted = if container == Container::Opus { 2 } else { 3 };
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut cur = Vec::new();
    let mut audio_from = None;
    'pages: for (i, p) in pages.iter().enumerate() {
        let mut off = 0;
        for (j, l) in p.lacing.iter().enumerate() {
            let l = usize::from(*l);
            cur.extend_from_slice(&p.payload[off..off + l]);
            off += l;
            if l < 255 {
                packets.push(std::mem::take(&mut cur));
                if packets.len() == wanted {
                    if j + 1 != p.lacing.len() {
                        return Err(damaged("an Ogg header page also carries audio"));
                    }
                    audio_from = Some(i + 1);
                    break 'pages;
                }
            }
        }
    }
    let audio_from = audio_from.ok_or_else(|| damaged("the Ogg headers are damaged"))?;

    let mut parts = vec![("Ogg identification header".to_string(), sha(&packets[0]))];
    let (sig, rest): (&[u8], &[u8]) = match container {
        Container::Opus => (b"OpusTags", &packets[1]),
        _ => {
            parts.push(("Ogg setup header".to_string(), sha(&packets[2])));
            (b"\x03vorbis", &packets[1])
        }
    };
    let body = rest
        .strip_prefix(sig)
        .ok_or_else(|| damaged("the Ogg comment header is damaged"))?;
    let (vendor, fields, tail) =
        parse_comments(body).ok_or_else(|| damaged("the Vorbis comments are damaged"))?;
    // RFC 7845: data after the OpusTags comments whose first bit is set must be kept (lofty
    // drops it); with the bit clear it is padding.
    if container == Container::Opus && tail.first().is_some_and(|x| x & 1 != 0) {
        parts.push(("OpusTags binary data".to_string(), sha(tail)));
    }
    let mut h = Sha256::new();
    for p in &pages[audio_from..] {
        h.update([p.flags]);
        h.update(p.granule);
        h.update(p.serial);
        h.update([p.lacing.len() as u8]);
        h.update(p.lacing);
        h.update(p.payload);
    }
    parts.push((
        format!("Ogg audio pages ({})", pages.len() - audio_from),
        h.finalize().into(),
    ));
    if pos < b.len() {
        parts.push(("data after the last Ogg page".to_string(), sha(&b[pos..])));
    }
    Ok(Raw {
        vendor,
        fields,
        parts,
    })
}

/// The Ogg page checksum: CRC-32, polynomial 0x04C11DB7, not reflected, initial value 0.
fn ogg_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for b in page {
        crc ^= u32::from(*b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// kima-rs's own check of a written Ogg file (lofty re-pages and renumbers it): the whole file
/// is pages, each logical stream's page numbers run 0, 1, 2, … without a gap, and every page
/// checksum is right.
pub(super) fn check_pages(b: &[u8]) -> Result<(), TagWriteError> {
    let bad = |what: String| damaged(&format!("the written Ogg file is damaged: {what}"));
    let mut next: BTreeMap<[u8; 4], u32> = BTreeMap::new();
    let (mut pos, mut n) = (0usize, 0usize);
    while pos < b.len() {
        let hdr = b
            .get(pos..pos + 27)
            .filter(|h| &h[..4] == b"OggS")
            .ok_or_else(|| bad(format!("no page at byte {pos}")))?;
        let nseg = usize::from(hdr[26]);
        let lacing = b
            .get(pos + 27..pos + 27 + nseg)
            .ok_or_else(|| bad(format!("page {n} is cut off")))?;
        let end = pos + 27 + nseg + lacing.iter().map(|x| usize::from(*x)).sum::<usize>();
        let page = b
            .get(pos..end)
            .ok_or_else(|| bad(format!("page {n} is cut off")))?;
        let serial = [hdr[14], hdr[15], hdr[16], hdr[17]];
        let seq = u32::from_le_bytes([hdr[18], hdr[19], hdr[20], hdr[21]]);
        let expected = next.entry(serial).or_insert(0);
        if seq != *expected {
            return Err(bad(format!(
                "page {n} is numbered {seq}, expected {expected}"
            )));
        }
        *expected = expected.wrapping_add(1);
        let stored = u32::from_le_bytes([hdr[22], hdr[23], hdr[24], hdr[25]]);
        let mut zeroed = page.to_vec();
        zeroed[22..26].fill(0);
        if ogg_crc(&zeroed) != stored {
            return Err(bad(format!("page {n} has a wrong checksum")));
        }
        pos = end;
        n += 1;
    }
    if n == 0 {
        return Err(bad("no page".to_string()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Reading

pub(super) fn read(
    path: &Path,
    container: Container,
    keys: &[String],
) -> Result<NativeTags, TagWriteError> {
    check_keys(keys)?;
    let raw = parse(path, container)?;
    Ok(keys
        .iter()
        .map(|k| {
            let vals = raw
                .fields
                .iter()
                .filter(|f| &f.label == k)
                .map(|f| String::from_utf8_lossy(&f.value).into_owned())
                .collect();
            (k.clone(), vals)
        })
        .collect())
}

// ---------------------------------------------------------------------------------------------
// Writing

fn options() -> ParseOptions {
    ParseOptions::new()
        .read_properties(false)
        .implicit_conversions(false)
}

/// The file's comments as lofty reads them, a FLAC file's `PICTURE` blocks included (lofty
/// keeps those outside the tag, and saving the tag alone would drop them).
fn read_comments(path: &Path, container: Container) -> Result<VorbisComments, TagWriteError> {
    let mut f = File::open(path)?;
    Ok(match container {
        Container::Flac => {
            let file = FlacFile::read_from(&mut f, options())?;
            let mut vc = file.vorbis_comments().cloned().unwrap_or_default();
            for (pic, info) in file.pictures() {
                vc.insert_picture(pic.clone(), Some(*info))
                    .map_err(|e| TagWriteError::Lofty(e.to_string()))?;
            }
            vc
        }
        Container::Ogg => VorbisFile::read_from(&mut f, options())?
            .vorbis_comments()
            .clone(),
        Container::Opus => OpusFile::read_from(&mut f, options())?
            .vorbis_comments()
            .clone(),
        _ => {
            return Err(TagWriteError::Verify(
                "not a Vorbis comment container".into(),
            ));
        }
    })
}

/// Replaces the fields of `new` (any case) and saves. Every other field keeps its place and
/// order; the new fields go last, one per value.
pub(super) fn write(
    path: &Path,
    container: Container,
    new: &NativeTags,
) -> Result<(), TagWriteError> {
    check_keys(&new.keys().cloned().collect::<Vec<_>>())?;
    let mut vc = read_comments(path, container)?;
    let kept: Vec<(String, String)> = vc
        .take_items()
        .filter(|(k, _)| !new.keys().any(|n| n.eq_ignore_ascii_case(k)))
        .collect();
    for (k, v) in kept {
        vc.push(k, v);
    }
    for (k, vals) in new {
        for v in vals {
            vc.push(k.clone(), v.clone());
        }
    }
    vc.save_to_path(path, WriteOptions::default().preferred_padding(0))?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Preservation check

/// One `(label, digest)` per field name `keys` do not own (all its fields, in order, byte for
/// byte), the vendor string and every other part of the file; sorted.
pub(super) fn untouched(
    path: &Path,
    container: Container,
    keys: &[String],
) -> Result<Vec<(String, [u8; 32])>, TagWriteError> {
    check_keys(keys)?;
    let raw = parse(path, container)?;
    let mut by_name: BTreeMap<&str, Sha256> = BTreeMap::new();
    for f in raw.fields.iter().filter(|f| !keys.contains(&f.label)) {
        let h = by_name.entry(&f.label).or_default();
        h.update((f.raw.len() as u64).to_le_bytes());
        h.update(&f.raw);
    }
    let mut out = raw.parts;
    out.push(("vendor string".to_string(), sha(&raw.vendor)));
    out.extend(
        by_name
            .into_iter()
            .map(|(k, h)| (k.to_string(), h.finalize().into())),
    );
    out.sort_unstable();
    Ok(out)
}

/// The comment field names as stored and every other part of the file, with counts, the fields
/// `keys` own left out.
pub(super) fn raw_untouched(
    path: &Path,
    container: Container,
    keys: &[String],
) -> Result<BTreeMap<String, usize>, TagWriteError> {
    check_keys(keys)?;
    let raw = parse(path, container)?;
    let mut out = BTreeMap::new();
    for label in raw
        .fields
        .iter()
        .map(|f| &f.label)
        .filter(|l| !keys.contains(l))
        .chain(raw.parts.iter().map(|(l, _)| l))
    {
        *out.entry(label.clone()).or_insert(0) += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_follow_the_numbers() {
        let v = TagValues {
            track: Some(NumberPair {
                number: 3,
                total: None,
            }),
            disc: Some(NumberPair {
                number: 1,
                total: Some(2),
            }),
            compilation: Some(false),
            genres: Some(vec![String::new()]),
            ..TagValues::default()
        };
        let n = native(&v);
        assert_eq!(n["TRACKNUMBER"], vec!["3"]);
        assert!(n["TRACKTOTAL"].is_empty(), "no total removes it");
        assert_eq!(n["DISCTOTAL"], vec!["2"]);
        assert!(n["COMPILATION"].is_empty());
        assert!(!n.contains_key("GENRE"));
        assert!(!n.contains_key("TOTALTRACKS"));
    }

    #[test]
    fn only_table_keys_are_writable() {
        assert!(check_key("TITLE").is_ok());
        assert!(check_key("TOTALDISCS").is_ok());
        assert!(check_key("title").is_err());
        assert!(check_key("REPLAYGAIN_TRACK_GAIN").is_err());
        assert!(check_key("METADATA_BLOCK_PICTURE").is_err());
    }

    /// One page with a right checksum.
    fn page(serial: u32, seq: u32, payload: &[u8]) -> Vec<u8> {
        let mut p = b"OggS\0\0".to_vec();
        p.extend([0u8; 8]);
        p.extend(serial.to_le_bytes());
        p.extend(seq.to_le_bytes());
        p.extend([0u8; 4]);
        p.push(1);
        p.push(payload.len() as u8);
        p.extend(payload);
        let crc = ogg_crc(&p);
        p[22..26].copy_from_slice(&crc.to_le_bytes());
        p
    }

    #[test]
    fn written_ogg_pages_are_checked() {
        let good = [page(7, 0, b"a"), page(7, 1, b"b"), page(9, 0, b"c")].concat();
        assert!(check_pages(&good).is_ok());

        let gap = [page(7, 0, b"a"), page(7, 2, b"b")].concat();
        let err = check_pages(&gap).unwrap_err().to_string();
        assert!(err.contains("page 1 is numbered 2, expected 1"), "{err}");
        let late_start = page(7, 1, b"a");
        assert!(check_pages(&late_start).is_err());

        let mut crc = good.clone();
        crc[28] ^= 1; // the first payload byte
        let err = check_pages(&crc).unwrap_err().to_string();
        assert!(err.contains("page 0 has a wrong checksum"), "{err}");
        assert!(err.ends_with("; nothing written"));

        let mut tail = good.clone();
        tail.extend(b"junk");
        assert!(check_pages(&tail).is_err());
        assert!(check_pages(&good[..good.len() - 1]).is_err());
        assert!(check_pages(&[]).is_err());
    }

    #[test]
    fn comment_fields_parse_as_stored() {
        let mut b = Vec::new();
        b.extend(3u32.to_le_bytes());
        b.extend(b"ven");
        b.extend(3u32.to_le_bytes());
        for f in [&b"title=A"[..], b"nosep", b"BAD\x7fNAME=x"] {
            b.extend((f.len() as u32).to_le_bytes());
            b.extend(f);
        }
        b.push(1);
        let (vendor, fields, tail) = parse_comments(&b).unwrap();
        assert_eq!(vendor, b"ven");
        let labels: Vec<&str> = fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            labels,
            ["TITLE", "<field without =>", "<field with an invalid name>"]
        );
        assert_eq!(tail, [1]);
        assert!(parse_comments(&b[..b.len() - 3]).is_none());
    }
}
