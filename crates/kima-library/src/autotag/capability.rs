//! Where an apply may write (spec §19.9, C14): `files` only to a writable library, `copy` only
//! to an organized copy (Task 17), `db` always. The read-only default stays read-only (D-120).

use std::collections::BTreeSet;
use std::path::Path;

use kima_db::autotag::{LocalAlbum, Target};
use serde::Serialize;
use uuid::Uuid;

use super::journal::scrub;
use super::{COPY_FIRST, FILES_READ_ONLY};
use crate::organize::apply::{SYMLINK_NOTE, writable};
use crate::scan::LibraryCtx;
use crate::tagwrite::{self, TagWriteError};

/// Whether a target can be chosen, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    pub available: bool,
    pub reason: Option<String>,
}

impl Availability {
    fn yes() -> Self {
        Self {
            available: true,
            reason: None,
        }
    }

    fn no(reason: &str) -> Self {
        Self {
            available: false,
            reason: Some(reason.to_string()),
        }
    }
}

/// The targets other than `db` (always available).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub files: Availability,
    pub copy: Availability,
    /// The copy-mode organizer run a `copy` apply writes into.
    pub copy_run_id: Option<Uuid>,
}

/// `files`: the library root passes the organizer's write probe. `copy`: not offered until
/// Task 17 wires the organized copy.
pub async fn capabilities(ctx: &LibraryCtx) -> Capabilities {
    let root = ctx.root.get();
    let files_ok = tokio::task::spawn_blocking(move || writable(&root))
        .await
        .unwrap_or(false);
    Capabilities {
        files: if files_ok {
            Availability::yes()
        } else {
            Availability::no(FILES_READ_ONLY)
        },
        // Task 17: a done, unswitched copy-mode run covering the album, with a writable target.
        copy: Availability::no(COPY_FIRST),
        copy_run_id: None,
    }
}

/// `Tag writing is not supported for .wav files`, or for several:
/// `Tag writing is not supported for .wav, .wma files` (sorted, each once).
pub fn unsupported_message(exts: &BTreeSet<String>) -> String {
    let list: Vec<String> = exts.iter().map(|e| format!(".{e}")).collect();
    format!("Tag writing is not supported for {} files", list.join(", "))
}

/// Why this album cannot be written for `target`, if it cannot: for `files` (and `copy`), any
/// member in a format kima-rs does not write (the joined list), else any member that is a
/// symbolic link, lies under a symlinked folder, or whose row says it was reached through one
/// (M1's `SYMLINK_NOTE`). A file that cannot be probed is named by its library-relative path.
/// `db` writes no file and works for any album. Blocking work runs off the async thread.
pub async fn album_refusal(
    ctx: &LibraryCtx,
    album: &LocalAlbum,
    target: Target,
) -> sqlx::Result<Option<String>> {
    if target == Target::Db {
        return Ok(None);
    }
    // Task 17: a copy target checks the copies at the copy run's `to_path`s.
    let ids: Vec<Uuid> = album.tracks.iter().map(|t| t.id).collect();
    let linked_rows = kima_db::autotag::any_via_symlink(&ctx.pool, &ids).await?;
    let root = ctx.root.get();
    let rels: Vec<String> = album.tracks.iter().map(|t| t.path.clone()).collect();
    let verdict = tokio::task::spawn_blocking(move || files_refusal(&root, &rels))
        .await
        .unwrap_or_else(|e| Some(format!("cannot check the album's files: {e}")));
    Ok(verdict.or_else(|| linked_rows.then(|| SYMLINK_NOTE.to_string())))
}

/// The format and symlink checks on disk.
fn files_refusal(root: &Path, rels: &[String]) -> Option<String> {
    let mut unsupported = BTreeSet::new();
    let mut linked = false;
    for rel in rels {
        match tagwrite::refuse_symlink(root, rel) {
            Ok(()) => {}
            Err(TagWriteError::Symlink) => {
                linked = true;
                continue;
            }
            Err(e) => return Some(scrub(root, &format!("{rel}: {e}"))),
        }
        match tagwrite::container_of(&root.join(rel)) {
            Ok(_) => {}
            Err(TagWriteError::Unsupported(ext)) => {
                unsupported.insert(ext);
            }
            Err(e) => return Some(scrub(root, &format!("{rel}: {e}"))),
        }
    }
    if !unsupported.is_empty() {
        return Some(unsupported_message(&unsupported));
    }
    linked.then(|| SYMLINK_NOTE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_list_joined_sorted_once() {
        let one = BTreeSet::from(["wma".to_string()]);
        assert_eq!(
            unsupported_message(&one),
            "Tag writing is not supported for .wma files"
        );
        let two = BTreeSet::from(["wma".to_string(), "wav".to_string()]);
        assert_eq!(
            unsupported_message(&two),
            "Tag writing is not supported for .wav, .wma files"
        );
    }
}
