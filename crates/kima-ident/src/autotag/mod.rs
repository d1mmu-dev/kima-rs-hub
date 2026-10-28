//! The autotagger (spec M3b, §19): matching a local album against MusicBrainz releases.

pub mod distance;
pub mod proposal;
pub mod score;
pub mod search;
