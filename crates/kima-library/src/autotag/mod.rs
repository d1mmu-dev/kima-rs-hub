//! The autotagger's library side (spec M3b, §19): what the search, apply and undo jobs share
//! without depending on `kima-ident` or `kima-net`.

pub mod apply;
pub mod capability;
pub mod events;
pub mod journal;
pub mod undo;

pub use apply::{enqueue_apply, enqueue_undo};
pub use undo::run_undo;

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
/// `reorganize` moves the library files (spec §19.9 step 5): only with the `files` target.
pub const REORGANIZE_FILES_ONLY: &str =
    "Reorganizing is only available when tagging the library files";
/// Undo goes newest first: an older run waits while a newer one of the album is undoable.
pub const UNDO_NEWER_FIRST: &str = "A newer match of this album must be undone first";
/// A refused run that wrote no file and committed no identity: nothing to undo.
pub const NOTHING_TO_UNDO: &str = "This match changed nothing; there is nothing to undo";
/// A written file that no longer hashes to what the apply wrote (spec §19.9 undo step 2).
pub const UNDO_CHANGED: &str = "changed since the tag write";
/// The run ends `undo_failed` because some files could not be restored.
pub const UNDO_INCOMPLETE: &str = "Some files could not be restored; see the journal";
/// The files to restore are on a mount kima-rs cannot write.
pub const UNDO_READ_ONLY: &str =
    "The tagged files are on a read-only mount; mount it writable to undo this match";
/// A `copy` run whose copy run is gone, or was switched to and is no longer the library.
pub const COPY_GONE: &str = "The tagged copy is no longer available; this match cannot be undone";
/// The reorganize's organizer run is still applying under its own job.
pub const REORGANIZE_IN_PROGRESS: &str =
    "The reorganize is still moving files; try again when it finishes";
