//! The autotagger's library side (spec M3b, §19): what the search, apply and undo jobs share
//! without depending on `kima-ident` or `kima-net`.

pub mod apply;
pub mod capability;
pub mod events;
pub mod journal;

pub use apply::{enqueue_apply, enqueue_undo};

/// `files` is refused on a library kima-rs cannot write (spec §19.9).
pub const FILES_READ_ONLY: &str =
    "The library is mounted read-only. Mount it writable, or use copy or database-only.";
/// `copy` is refused until a copy-mode organizer run covers the album (spec §19.9).
pub const COPY_FIRST: &str =
    "Copy the library first (Settings > Organize, copy mode), then tag the copy.";
/// The album's present tracks are no longer the ones the candidate mapped (Review Focus 2).
pub const TRACKS_CHANGED: &str =
    "The album's tracks changed since the match was computed; search again";
/// A journalled file that no longer holds what was planned, nor the new tags (spec §19.9).
pub const CHANGED_OUTSIDE: &str = "changed outside kima-rs";
/// An album whose tracks hold another run's tag overrides: a new apply would replace them and
/// an undo of it would lose them, so a re-match requires undoing first.
pub const APPLIED_MATCH_EXISTS: &str = "This album already has an applied match; undo it first";
