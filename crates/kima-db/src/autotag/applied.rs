//! The values an apply writes (spec §19.6), as plain data: computed once from the candidate's
//! MusicBrainz snapshot and stored on the run (`autotag_runs.applied`), so apply and undo never
//! parse MusicBrainz JSON and make no request (R3).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The album-scope values.
///
/// "Do not write" contract: an empty `artist_sort`, `None` `date`/`original_date`/`year` and empty
/// `genres` mean "leave the existing value alone". An empty `title` or `artist` is only the
/// placeholder of an [`Applied::mbids_only`] value that `with_local_identity` has not filled yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedAlbum {
    pub title: String,
    /// The credit line: each credit's `name` + `joinphrase`.
    pub artist: String,
    /// Each credit's `artist.sort-name` + `joinphrase`.
    pub artist_sort: String,
    pub artist_mbids: Vec<Uuid>,
    pub release_mbid: Uuid,
    pub release_group_mbid: Uuid,
    /// The release date as given (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`); written into the files.
    pub date: Option<String>,
    /// The release group's first release date; written into the files.
    pub original_date: Option<String>,
    /// The year of the original date, else of the release date. Apply writes it into the files
    /// and onto the `albums` row (and `applied_identity`); it stands and is not recomputed from
    /// the folder mode (spec §19.7). Only the enrichment pipeline never fills it (§8.5).
    pub year: Option<i32>,
    pub compilation: bool,
    /// Top three by count; empty means "leave the existing genres alone".
    pub genres: Vec<String>,
    pub disc_total: u32,
}

/// The track-scope values. A `None` field is left as it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedTrack {
    pub track_id: Uuid,
    /// False for an extra local track: it gets the album-scope values only.
    pub matched: bool,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub track_no: Option<u32>,
    pub track_total: Option<u32>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artist_sort: Option<String>,
    pub artist_mbids: Vec<Uuid>,
    pub recording_mbid: Option<Uuid>,
    pub release_track_mbid: Option<Uuid>,
}

/// Everything an apply writes for one album.
///
/// A value with `mbids_only == true` is NOT complete: its names, dates, year, compilation flag and
/// genres are blanked and must pass through `kima_ident::autotag::proposal::with_local_identity`
/// before it is stored on a run or applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub album: AppliedAlbum,
    pub tracks: Vec<AppliedTrack>,
    /// Only the MBIDs and the numbers are written; names, dates and genres stay local.
    pub mbids_only: bool,
}
