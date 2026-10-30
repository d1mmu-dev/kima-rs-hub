//! The file side of the apply journal (spec §19.9): planning a row from a file, removing the tag
//! writer's leftover copies and deciding what a `planned` row found on resume needs.
//!
//! Everything here is blocking (file I/O, ffmpeg); call it from `spawn_blocking`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use kima_db::autotag::journal::{JournalRow, NewJournalRow};
use uuid::Uuid;

use crate::organize::apply::sha256_file;
use crate::tagwrite::{
    self, NativeTags, TagValues, TagWriteError, audio_md5, container_of, copy_path, id3_version,
    native_for_file, read_native,
};

/// `rel` under `root`, refusing anything but plain components (the M1 rule).
pub fn join_rel(root: &Path, rel: &str) -> Result<PathBuf, TagWriteError> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(TagWriteError::Io(format!("unsafe path: {rel}")));
    }
    Ok(root.join(p))
}

/// `msg` with the library root taken out of every path in it, so a message that reaches a user
/// (a row note, a run error) names files relative to the library (ffmpeg's errors quote the
/// absolute path it was given).
pub fn scrub(root: &Path, msg: &str) -> String {
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return msg.to_string();
    }
    msg.replace(&format!("{root}/"), "").replace(root, "")
}

/// A row's tags as stored (`{key: [values]}`).
pub fn tags_of(v: &serde_json::Value) -> Result<NativeTags, TagWriteError> {
    serde_json::from_value(v.clone())
        .map_err(|e| TagWriteError::Io(format!("unreadable journal tags: {e}")))
}

/// The `planned` row for the file at `rel`: its whole-file SHA-256, audio MD5, the current
/// values of every key the write touches (`[]` when absent), the new ones (by
/// [`native_for_file`], so Vorbis `TOTALTRACKS`/`TOTALDISCS` follow the file) and its ID3v2
/// version. Refuses a symlinked file or folder (`SYMLINK_NOTE`).
pub fn plan_file(
    root: &Path,
    rel: &str,
    track_id: Uuid,
    values: &TagValues,
) -> Result<NewJournalRow, TagWriteError> {
    tagwrite::refuse_symlink(root, rel)?;
    let abs = join_rel(root, rel)?;
    let container = container_of(&abs)?;
    let id3 = id3_version(&abs)?;
    let new = native_for_file(&abs, container, id3, values)?;
    let keys: Vec<String> = new.keys().cloned().collect();
    let old = read_native(&abs, container, &keys)?;
    let before_sha = sha256_file(&abs)?;
    let md5 = audio_md5(&abs)?;
    let json = |t: &NativeTags| {
        serde_json::to_value(t).map_err(|e| TagWriteError::Io(format!("cannot store tags: {e}")))
    };
    Ok(NewJournalRow {
        track_id: Some(track_id),
        root: "library".to_string(),
        path: rel.to_string(),
        before_sha: Some(before_sha),
        audio_md5: Some(md5.to_vec()),
        old_tags: json(&old)?,
        new_tags: json(&new)?,
        id3_version: id3.map(|v| v.as_str().to_string()),
    })
}

/// Deletes the tag writer's copies (`.kima-tag-<row id>.<ext>`) a crash left next to the run's
/// files; answers how many. Only those exact names are ever removed.
pub fn remove_leftovers(root: &Path, rows: &[JournalRow]) -> usize {
    let mut removed = 0;
    for row in rows {
        let Ok(abs) = join_rel(root, &row.path) else {
            continue;
        };
        let Ok(copy) = copy_path(&abs, row.id) else {
            continue;
        };
        if fs::symlink_metadata(&copy).is_ok() {
            match fs::remove_file(&copy) {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!(row = row.id, "cannot delete a leftover tag copy: {e}"),
            }
        }
    }
    removed
}

/// What a `planned` row's file needs now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowCheck {
    /// The file still holds what was planned (`before_sha`): write it.
    Write,
    /// A crash after the rename: the touched keys already read back the new values and the
    /// audio is the planned audio. The current whole-file hash.
    AlreadyWritten(Vec<u8>),
    /// Neither: it changed outside kima-rs (or is gone). Never written over.
    Changed,
}

/// The §19.9 rules for a `planned` row. `resumed`: the row was journalled by an earlier,
/// interrupted call, so the file may already carry the new tags; on a first pass a file that
/// differs from `before_sha` is always [`RowCheck::Changed`].
pub fn check_row(root: &Path, row: &JournalRow, resumed: bool) -> RowCheck {
    let Ok(abs) = join_rel(root, &row.path) else {
        return RowCheck::Changed;
    };
    let Ok(sha) = sha256_file(&abs) else {
        return RowCheck::Changed;
    };
    if row.before_sha.as_deref() == Some(sha.as_slice()) {
        return RowCheck::Write;
    }
    if !resumed {
        return RowCheck::Changed;
    }
    let carries_new = || -> Result<bool, TagWriteError> {
        let new = tags_of(&row.new_tags)?;
        let keys: Vec<String> = new.keys().cloned().collect();
        let container = container_of(&abs)?;
        if read_native(&abs, container, &keys)? != new {
            return Ok(false);
        }
        Ok(row.audio_md5.as_deref() == Some(audio_md5(&abs)?.as_slice()))
    };
    match carries_new() {
        Ok(true) => RowCheck::AlreadyWritten(sha),
        _ => RowCheck::Changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_leaves_library_relative_paths() {
        let root = Path::new("/music/lib");
        assert_eq!(
            scrub(
                root,
                "ffmpeg could not read the audio: file:/music/lib/A/b.flac: bad"
            ),
            "ffmpeg could not read the audio: file:A/b.flac: bad"
        );
        assert_eq!(scrub(root, "no path here"), "no path here");
        assert_eq!(scrub(Path::new("/"), "/x"), "/x");
    }
}
