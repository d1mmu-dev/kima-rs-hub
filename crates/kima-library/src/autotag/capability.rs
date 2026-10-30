//! Where an apply may write (spec §19.9, C14): `files` only to a writable library, `copy` only
//! to the organized copies of a finished whole-library copy run, `db` always. The read-only
//! default stays read-only (D-120).

use std::collections::BTreeSet;
use std::path::Path;

use kima_db::autotag::{LocalAlbum, Target};
use kima_db::organizer::{self as odb, Run};
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

/// `files`: the library root passes the organizer's write probe. `copy`: the newest finished,
/// unswitched whole-library copy run ([`odb::latest_copy_run`]) exists and its target passes the
/// probe (`copy_run_id` is that run); else [`COPY_FIRST`]. Whether the copy covers a given album
/// is [`album_refusal`]'s.
pub async fn capabilities(ctx: &LibraryCtx) -> sqlx::Result<Capabilities> {
    let files_ok = probe(ctx.root.get()).await;
    let copy_run = usable_copy_run(ctx).await?;
    Ok(Capabilities {
        files: if files_ok {
            Availability::yes()
        } else {
            Availability::no(FILES_READ_ONLY)
        },
        copy: if copy_run.is_some() {
            Availability::yes()
        } else {
            Availability::no(COPY_FIRST)
        },
        copy_run_id: copy_run.map(|r| r.id),
    })
}

/// The copy run a `copy` apply would write into now: [`odb::latest_copy_run`] when its target
/// is writable.
pub async fn usable_copy_run(ctx: &LibraryCtx) -> sqlx::Result<Option<Run>> {
    let Some(run) = odb::latest_copy_run(&ctx.pool).await? else {
        return Ok(None);
    };
    Ok(probe(run.target_root.clone().into()).await.then_some(run))
}

async fn probe(dir: std::path::PathBuf) -> bool {
    tokio::task::spawn_blocking(move || writable(&dir))
        .await
        .unwrap_or(false)
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
/// (M1's `SYMLINK_NOTE`). A file that cannot be probed is named by its path relative to its
/// root. `copy` checks the copies, not the library files: no usable copy run, or one without a
/// copy of every present track of the album, is [`COPY_FIRST`]. `db` writes no file and works
/// for any album. Blocking work runs off the async thread.
pub async fn album_refusal(
    ctx: &LibraryCtx,
    album: &LocalAlbum,
    target: Target,
) -> sqlx::Result<Option<String>> {
    match target {
        Target::Db => Ok(None),
        Target::Files => {
            let rels: Vec<String> = album.tracks.iter().map(|t| t.path.clone()).collect();
            members_refusal(ctx, album, ctx.root.get(), rels).await
        }
        Target::Copy => match usable_copy_run(ctx).await? {
            Some(run) => Ok(copy_refusal(ctx, album, &run).await?.err()),
            None => Ok(Some(COPY_FIRST.to_string())),
        },
    }
}

/// The `copy` check against one copy run: every present track of the album has a copy there
/// ([`odb::copy_rows_for_tracks`]), else [`COPY_FIRST`]; then the format and symlink checks on
/// the copies (under the run's target). Answers each track's copy path (relative to the
/// target), or the refusal.
pub async fn copy_refusal(
    ctx: &LibraryCtx,
    album: &LocalAlbum,
    run: &Run,
) -> sqlx::Result<Result<std::collections::HashMap<Uuid, String>, String>> {
    let ids: Vec<Uuid> = album.tracks.iter().map(|t| t.id).collect();
    let copies = odb::copy_rows_for_tracks(&ctx.pool, run.id, &ids).await?;
    if ids.iter().any(|t| !copies.contains_key(t)) {
        return Ok(Err(COPY_FIRST.to_string()));
    }
    let rels: Vec<String> = ids.iter().map(|t| copies[t].clone()).collect();
    let root = std::path::PathBuf::from(&run.target_root);
    Ok(match members_refusal(ctx, album, root, rels).await? {
        Some(reason) => Err(reason),
        None => Ok(copies),
    })
}

/// The format and symlink checks of the files at `rels` under `root` that stand for the
/// album's tracks.
async fn members_refusal(
    ctx: &LibraryCtx,
    album: &LocalAlbum,
    root: std::path::PathBuf,
    rels: Vec<String>,
) -> sqlx::Result<Option<String>> {
    let ids: Vec<Uuid> = album.tracks.iter().map(|t| t.id).collect();
    let linked_rows = kima_db::autotag::any_via_symlink(&ctx.pool, &ids).await?;
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
