//! Similar artists (spec §12.5; upstream `artistSimilarity.ts`): ListenBrainz by MBID, Deezer by
//! name, merged into the list stored in `artists."similar"`.
//!
//! The library re-rank (`RERANK_MAX_DELTA`) needs M5's matching state; it is ported as delta 0,
//! which leaves the merged order and scores as they are.
//!
//! At read time [`resolve`] matches the stored list against the library (spec §12.8): no
//! request is made, and an artist outside the library has no image.

use std::collections::HashMap;

use kima_db::PgPool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Mbid;
use crate::listenbrainz::Similar;
use crate::text::normalize_artist_name;

/// The most similar artists stored (spec §12.5).
pub const MAX_SIMILAR: usize = 30;

/// One stored similar artist, serialised `{name, mbid, match}` (`mbid` null when unknown).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub mbid: Option<Mbid>,
    #[serde(rename = "match")]
    pub r#match: f64,
}

/// ListenBrainz's rows spread over 0.5-0.95 against the strongest:
/// `match = 0.5 + 0.45 × score / strongest` (`listenBrainz.ts:152-156`). The rows come
/// strongest first.
fn from_listenbrainz(lb: &[Similar]) -> Vec<Entry> {
    let strongest = lb
        .iter()
        .map(|s| s.score)
        .fold(0.0_f64, f64::max)
        .max(f64::MIN_POSITIVE);
    lb.iter()
        .map(|s| Entry {
            name: s.name.clone(),
            mbid: s.mbid,
            r#match: 0.5 + 0.45 * s.score / strongest,
        })
        .collect()
}

/// Deezer's related artists, in its order, falling linearly from 0.9 to 0.5 by position
/// (`deezer.ts:211-218`: `max(0.5, 0.9 − 0.4 × i / max(n − 1, 1))`).
fn from_deezer(deezer: &[String]) -> Vec<Entry> {
    let n = deezer.len().saturating_sub(1).max(1) as f64;
    deezer
        .iter()
        .enumerate()
        .map(|(i, name)| Entry {
            name: name.clone(),
            mbid: None,
            r#match: (0.9 - 0.4 * i as f64 / n).max(0.5),
        })
        .collect()
}

/// The stored list (spec §12.5): ListenBrainz alone when it has at least [`MAX_SIMILAR`];
/// otherwise ListenBrainz then Deezer, deduplicated by lower-cased trimmed name (the first,
/// ListenBrainz's, wins; an empty name is dropped), cut to [`MAX_SIMILAR`]. Both empty is empty.
pub fn merge(lb: &[Similar], deezer: &[String]) -> Vec<Entry> {
    let mut lb = from_listenbrainz(lb);
    if lb.len() >= MAX_SIMILAR {
        lb.truncate(MAX_SIMILAR);
        return lb;
    }
    let mut seen = std::collections::HashSet::new();
    lb.into_iter()
        .chain(from_deezer(deezer))
        .filter(|e| {
            let key = e.name.trim().to_lowercase();
            !key.is_empty() && seen.insert(key)
        })
        .take(MAX_SIMILAR)
        .collect()
}

/// The similar artists a page shows (spec §12.8).
pub const PAGE_SIMILAR: usize = 10;

/// One similar artist as the artist page sends it (upstream `resolveSimilarArtists`):
/// `{id, name, mbid, coverArt, albumCount, ownedAlbumCount, weight, inLibrary}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSimilar {
    /// The library artist's id, else the name.
    pub id: String,
    pub name: String,
    pub mbid: Option<String>,
    /// The library artist's id when it has a stored image (served by the cover route), else
    /// null. Artists outside the library have none: nothing is looked up on a page view.
    pub cover_art: Option<Uuid>,
    /// Always 0, as upstream.
    pub album_count: i64,
    /// Albums of the library artist with a present track.
    pub owned_album_count: i64,
    /// The stored `match`.
    pub weight: f64,
    pub in_library: bool,
    /// The library artist's own name (Subsonic shows it; the page shows the stored `name`).
    #[serde(skip)]
    pub library_name: Option<String>,
}

/// The entries of a stored `artists."similar"` value, in order. Anything that is not a list,
/// and any item without a name or a finite `match`, is skipped; an `mbid` that is not an MBID
/// reads as none.
pub fn stored_entries(v: &serde_json::Value) -> Vec<Entry> {
    #[derive(Deserialize)]
    struct Raw {
        name: String,
        #[serde(default)]
        mbid: Option<serde_json::Value>,
        #[serde(rename = "match")]
        r#match: f64,
    }
    let Some(items) = v.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| serde_json::from_value::<Raw>(item.clone()).ok())
        .filter(|r| !r.name.trim().is_empty() && r.r#match.is_finite())
        .map(|r| Entry {
            mbid: r
                .mbid
                .as_ref()
                .and_then(|m| m.as_str())
                .and_then(Mbid::parse),
            name: r.name,
            r#match: r.r#match,
        })
        .collect()
}

/// The most candidate rows one [`resolve`] reads. A short or common name ("The") matches far
/// more artists through the search vector than the list has entries; exact MBID and identity-key
/// matches come first, so the cut only drops fuzzy ones.
pub const MAX_CANDIDATES: i64 = 200;

/// A library artist that may be one of the similar artists.
#[derive(Debug)]
pub struct Candidate {
    pub id: Uuid,
    pub name: String,
    pub mbid: Option<Uuid>,
    pub has_image: bool,
}

/// Library artists (with a present track) that may be the first `limit` entries, at most
/// [`MAX_CANDIDATES`]: MBID and identity-key matches first, then the search-vector matches,
/// each by id.
///
/// Found through indexes, never a scan of every artist: the MBIDs (`artists_mbid_idx`), the
/// identity keys (`name_key`), and the unaccented search vector (`artists_search_idx`) holding
/// every word of a name's spellings (as given, normalised, and with "and" written `&`).
pub async fn candidates(pool: &PgPool, entries: &[Entry]) -> sqlx::Result<Vec<Candidate>> {
    let mbids: Vec<Uuid> = entries.iter().filter_map(|e| e.mbid.map(|m| m.0)).collect();
    let keys: Vec<String> = entries
        .iter()
        .map(|e| kima_library::names::key(&e.name))
        .collect();
    let mut spellings: Vec<String> = Vec::with_capacity(entries.len() * 3);
    for e in entries {
        let normalised = normalize_artist_name(&e.name);
        spellings.push(e.name.clone());
        spellings.push(normalised.replace(" and ", " & "));
        spellings.push(normalised);
    }
    let rows = sqlx::query!(
        r#"SELECT ar.id, ar.name, ar.mbid, (ar.image_path IS NOT NULL) AS "has_image!"
           FROM artists ar
           WHERE (ar.mbid = ANY($1::uuid[])
                  OR ar.name_key = ANY($2::text[])
                  OR ar.search @@ (
                      SELECT string_agg(
                                 '(' || plainto_tsquery('simple', unaccent(s.v))::text || ')',
                                 ' | ')::tsquery
                      FROM unnest($3::text[]) AS s(v)
                      WHERE length(to_tsvector('simple', unaccent(s.v))) > 0))
             AND EXISTS (SELECT 1 FROM albums al JOIN tracks t ON t.album_id = al.id
                          WHERE al.artist_id = ar.id AND t.missing_since IS NULL)
           ORDER BY (COALESCE(ar.mbid = ANY($1::uuid[]), false) OR ar.name_key = ANY($2::text[])) DESC,
                    ar.id
           LIMIT $4"#,
        &mbids,
        &keys,
        &spellings,
        MAX_CANDIDATES,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Candidate {
            id: r.id,
            name: r.name,
            mbid: r.mbid,
            has_image: r.has_image,
        })
        .collect())
}

/// The first `limit` entries, each matched to a library artist (one with a present track) by
/// MBID, else by [`normalize_artist_name`]; the rest are shown by name without an image. The
/// candidates are compared in Rust; only the matched artists get their album counts.
pub async fn resolve(
    pool: &PgPool,
    entries: &[Entry],
    limit: usize,
) -> sqlx::Result<Vec<ResolvedSimilar>> {
    let entries = &entries[..entries.len().min(limit)];
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let candidates = candidates(pool, entries).await?;

    // The first wins when two artists share an MBID or a normalised name.
    let mut by_mbid: HashMap<Uuid, &Candidate> = HashMap::new();
    let mut by_name: HashMap<String, &Candidate> = HashMap::new();
    for c in &candidates {
        if let Some(m) = c.mbid {
            by_mbid.entry(m).or_insert(c);
        }
        by_name.entry(normalize_artist_name(&c.name)).or_insert(c);
    }
    let found: Vec<Option<&Candidate>> = entries
        .iter()
        .map(|e| {
            e.mbid
                .and_then(|m| by_mbid.get(&m.0).copied())
                .or_else(|| by_name.get(&normalize_artist_name(&e.name)).copied())
        })
        .collect();

    let matched: Vec<Uuid> = found.iter().flatten().map(|c| c.id).collect();
    let owned: HashMap<Uuid, i64> = sqlx::query!(
        r#"SELECT ar.id,
                  (SELECT count(*) FROM albums al
                    WHERE al.artist_id = ar.id
                      AND EXISTS (SELECT 1 FROM tracks t
                                   WHERE t.album_id = al.id AND t.missing_since IS NULL))
                    AS "owned_album_count!"
           FROM artists ar WHERE ar.id = ANY($1)"#,
        &matched,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.id, r.owned_album_count))
    .collect();

    Ok(entries
        .iter()
        .zip(found)
        .map(|(e, found)| ResolvedSimilar {
            id: found.map_or_else(|| e.name.clone(), |c| c.id.to_string()),
            name: e.name.clone(),
            mbid: e.mbid.map(|m| m.to_string()),
            cover_art: found.filter(|c| c.has_image).map(|c| c.id),
            album_count: 0,
            owned_album_count: found.map_or(0, |c| owned.get(&c.id).copied().unwrap_or(0)),
            weight: e.r#match,
            in_library: found.is_some(),
            library_name: found.map(|c| c.name.clone()),
        })
        .collect())
}
