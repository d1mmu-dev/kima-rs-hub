//! The keys of `/api/mixes` track rows (M5b Tasks 9, 10; owner Q6 = B, D-264, Appendix D of the M5b plan).

use std::collections::BTreeSet;

use serde_json::Value;

/// Upstream's Prisma `Track` scalar keys, in model order (Appendix D).
pub const TRACK_KEYS: [&str; 58] = [
    "id",
    "albumId",
    "title",
    "trackNo",
    "duration",
    "mime",
    "filePath",
    "fileModified",
    "fileSize",
    "bpm",
    "bpmConfidence",
    "beatsCount",
    "key",
    "keyScale",
    "keyStrength",
    "energy",
    "loudness",
    "dynamicRange",
    "danceability",
    "valence",
    "arousal",
    "deamValence",
    "deamArousal",
    "clapValenceRaw",
    "clapArousalRaw",
    "instrumentalness",
    "acousticness",
    "speechiness",
    "danceabilityMl",
    "moodTags",
    "essentiaGenres",
    "analysisStatus",
    "analysisVersion",
    "analysisMode",
    "analyzedAt",
    "analysisError",
    "analysisRetryCount",
    "updatedAt",
    "displayTitle",
    "displayTrackNo",
    "hasUserOverrides",
    "analysisStartedAt",
    "vibeAnalysisStatus",
    "vibeAnalysisStartedAt",
    "vibeAnalysisError",
    "vibeAnalysisRetryCount",
    "vibeAnalysisStatusUpdatedAt",
    "isrc",
    "isrcSource",
    "acoustidFingerprint",
    "recordingMbid",
    "corrupt",
    "missingSince",
    "scanStatus",
    "scanError",
    "scanStartedAt",
    "discNumber",
    "discSubtitle",
];

/// Upstream's Prisma `Album` scalar keys, in model order.
pub const ALBUM_KEYS: [&str; 20] = [
    "id",
    "rgMbid",
    "rgMbidSource",
    "artistId",
    "title",
    "identityKey",
    "sortName",
    "year",
    "coverUrl",
    "primaryType",
    "label",
    "genres",
    "lastSynced",
    "location",
    "displayTitle",
    "displayYear",
    "hasUserOverrides",
    "userCoverUrl",
    "userGenres",
    "originalYear",
];

pub fn key_set(v: &Value) -> BTreeSet<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

/// The keys appear in this order in the raw reply from `from` on (serde_json's `Value` sorts keys, so the order is
/// read from the text): each `"key":` after the one before it.
pub fn assert_in_order(text: &str, from: usize, keys: &[&str]) -> usize {
    let mut at = from;
    for k in keys {
        let needle = format!("\"{k}\":");
        let found = text[at..]
            .find(&needle)
            .unwrap_or_else(|| panic!("{k} not after offset {at}"));
        at += found + needle.len();
    }
    at
}

/// Every row of `rows` carries exactly the Appendix D keys, and the raw reply `text` lists them in upstream's model
/// order from `from` on (the track's keys, `album` with its keys, its `artist`).
pub fn assert_rows_shape(text: &str, from: usize, rows: &[Value]) {
    let mut track_keys: BTreeSet<&str> = TRACK_KEYS.into_iter().collect();
    track_keys.insert("album");
    let mut album_keys: BTreeSet<&str> = ALBUM_KEYS.into_iter().collect();
    album_keys.insert("artist");
    let mut cursor = from;
    for row in rows {
        assert_eq!(key_set(row), track_keys, "{row}");
        assert_eq!(key_set(&row["album"]), album_keys);
        assert_eq!(
            key_set(&row["album"]["artist"]),
            BTreeSet::from(["id", "name", "mbid"])
        );
        cursor = assert_in_order(text, cursor, &TRACK_KEYS);
        cursor = assert_in_order(text, cursor, &["album"]);
        cursor = assert_in_order(text, cursor, &ALBUM_KEYS);
        cursor = assert_in_order(text, cursor, &["artist", "id", "name", "mbid"]);
    }
}
