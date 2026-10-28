//! The safe write (spec §19.8): the original is never opened for writing. Tags go into a hidden
//! copy in the same folder, which is synced, re-read and checked (touched keys, every other
//! frame, the audio MD5) before it is renamed over the original.

use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{Container, Id3Version, NativeTags, TagWriteError, audio_md5};
use crate::organize::apply::{IGNORE_GRACE, sha256_file};
use crate::watch::IgnoreSet;

/// Test hooks; all off by default.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteHooks {
    /// The tag library fails on the copy (as a lofty error would).
    pub fail_lofty: bool,
    /// Returns an error right after the rename, as a crash there would: the file holds the new
    /// tags but the caller never hears of it.
    pub stop_after_rename: bool,
    /// Cuts the end off the copy's audio before its MD5 is taken, as a writer that damaged the
    /// audio would.
    pub corrupt_audio: bool,
    /// Panics once the copy exists, as a bug in the tag library would.
    pub panic_in_write: bool,
}

/// What the write left on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    /// SHA-256 of the whole written file.
    pub after_sha: [u8; 32],
    /// The audio MD5, equal before and after.
    pub audio_md5: [u8; 16],
}

/// `<folder>/.kima-tag-<journal id>.<ext>`: hidden, so neither the walker nor the watcher
/// indexes it, and in the same folder, so the rename is atomic.
pub fn copy_path(path: &Path, journal_id: i64) -> Result<PathBuf, TagWriteError> {
    let parent = path
        .parent()
        .ok_or_else(|| TagWriteError::Io("the file has no folder".to_string()))?;
    let mut name = OsString::from(format!(".kima-tag-{journal_id}"));
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    Ok(parent.join(name))
}

/// Size and modification time: what must not change between the copy and the rename.
fn stamp(path: &Path) -> Result<(u64, Option<SystemTime>), TagWriteError> {
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() {
        return Err(TagWriteError::Symlink);
    }
    if !m.is_file() {
        return Err(TagWriteError::Io("not a regular file".to_string()));
    }
    Ok((m.len(), m.modified().ok()))
}

/// Holds the original and the copy in the [`IgnoreSet`] and cleans up however the write ends
/// (error, early return or panic): the copy is deleted unless it was renamed over the original,
/// and both paths are released after [`IGNORE_GRACE`].
struct Hold<'a> {
    ignore: &'a IgnoreSet,
    path: PathBuf,
    copy: PathBuf,
    renamed: bool,
}

impl<'a> Hold<'a> {
    fn new(ignore: &'a IgnoreSet, path: &Path, copy: PathBuf) -> Self {
        ignore.add(path.to_path_buf());
        ignore.add(copy.clone());
        Hold {
            ignore,
            path: path.to_path_buf(),
            copy,
            renamed: false,
        }
    }
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        if !self.renamed && fs::symlink_metadata(&self.copy).is_ok() {
            if let Err(e) = fs::remove_file(&self.copy) {
                tracing::warn!(copy = %self.copy.display(), "cannot delete the tag writer's copy: {e}");
            }
        }
        self.ignore.release(&self.path, IGNORE_GRACE);
        self.ignore.release(&self.copy, IGNORE_GRACE);
    }
}

/// Writes `new` (the exact native keys to replace; `[]` removes a key) into the file at `path`:
///
/// 1. hold `path` in `ignore`; refuse a symbolic link (the caller checks the folders above it
///    with [`refuse_symlink`](super::refuse_symlink)), a container or ID3v2 version other than
///    the file's, and the writer's own copy name;
/// 2. take the original's audio MD5;
/// 3. copy it to [`copy_path`], write the tags into the copy, `fsync` it;
/// 4. re-read the copy: every key of `new` must read back its new value; every other frame must
///    be on disk as often as in the original (raw frame headers) and read back unchanged (and
///    the ID3v1/APE tags); a refusal names the frames;
/// 5. the copy's audio MD5 must equal the original's, else [`TagWriteError::AudioChanged`];
/// 6. the original must not have changed meanwhile; `rename` the copy over it, `fsync` the
///    folder (a failure there is logged: the new content is already in place); release `path`
///    from `ignore` after [`IGNORE_GRACE`].
///
/// On any failure before the rename, a panic included, the copy is deleted and the original is
/// untouched. A v2.3 MP3 is passed `Some(V3)` and stays v2.3; `None` (or `Some(V4)`) is v2.4.
/// Blocking.
pub fn safe_write(
    path: &Path,
    journal_id: i64,
    container: Container,
    id3: Option<Id3Version>,
    new: &NativeTags,
    ignore: &IgnoreSet,
    hooks: &WriteHooks,
) -> Result<Written, TagWriteError> {
    let copy = copy_path(path, journal_id)?;
    if path.file_name() == copy.file_name() {
        return Err(TagWriteError::Io(
            "refusing to write the tag writer's own copy".to_string(),
        ));
    }
    let mut hold = Hold::new(ignore, path, copy);
    run(&mut hold, container, id3, new, hooks)
}

fn run(
    hold: &mut Hold<'_>,
    container: Container,
    id3: Option<Id3Version>,
    new: &NativeTags,
    hooks: &WriteHooks,
) -> Result<Written, TagWriteError> {
    let (path, copy) = (hold.path.clone(), hold.copy.clone());
    let (path, copy) = (path.as_path(), copy.as_path());
    let before = stamp(path)?;
    if super::container_of(path)? != container {
        return Err(TagWriteError::Verify(format!(
            "the file is not a {container:?} file; nothing written"
        )));
    }
    if container == Container::Mp3 {
        let on_disk = super::id3_version(path)?.unwrap_or(Id3Version::V4);
        let asked = id3.unwrap_or(Id3Version::V4);
        if on_disk != asked {
            return Err(TagWriteError::Verify(format!(
                "the file has ID3v2 {} but {} was asked; nothing written",
                on_disk.as_str(),
                asked.as_str()
            )));
        }
    }
    let keys: Vec<String> = new.keys().cloned().collect();
    let kept = super::untouched(path, container, id3, &keys)?;
    let raw_kept = super::raw_untouched(path, container, id3, &keys)?;
    let md5 = audio_md5(path)?;

    // A leftover copy of this journal row (a crash) is ours to replace.
    if fs::symlink_metadata(copy).is_ok() {
        fs::remove_file(copy)?;
    }
    copy_file(path, copy)?;

    if hooks.panic_in_write {
        panic!("forced panic in the tag write (test hook)");
    }
    if hooks.fail_lofty {
        return Err(TagWriteError::Lofty(
            "forced failure (test hook)".to_string(),
        ));
    }
    super::write_native(copy, container, id3, new)?;
    File::open(copy)?.sync_all()?;

    let got = super::read_native(copy, container, &keys)?;
    if &got != new {
        let bad: Vec<&str> = new
            .iter()
            .filter(|(k, v)| got.get(*k) != Some(*v))
            .map(|(k, _)| k.as_str())
            .collect();
        return Err(TagWriteError::Verify(format!(
            "these keys did not read back: {}; nothing written",
            bad.join(", ")
        )));
    }
    let raw_now = super::raw_untouched(copy, container, id3, &keys)?;
    if raw_now != raw_kept {
        return Err(TagWriteError::Verify(super::loss_message(
            container, id3, &raw_kept, &raw_now,
        )));
    }
    let now = super::untouched(copy, container, id3, &keys)?;
    if now != kept {
        let mut changed: Vec<&str> = kept
            .iter()
            .filter(|x| !now.contains(x))
            .chain(now.iter().filter(|x| !kept.contains(x)))
            .map(|(label, _)| label.as_str())
            .collect();
        changed.sort_unstable();
        changed.dedup();
        return Err(TagWriteError::Verify(format!(
            "these {} would have changed: {}; nothing written",
            super::noun(container),
            changed.join(", ")
        )));
    }

    if hooks.corrupt_audio {
        let f = fs::OpenOptions::new().write(true).open(copy)?;
        let len = f.metadata()?.len();
        f.set_len(len.saturating_sub(len / 4))?;
        f.sync_all()?;
    }
    if audio_md5(copy)? != md5 {
        return Err(TagWriteError::AudioChanged);
    }

    let sha = sha256_file(copy)?;
    let after_sha: [u8; 32] = sha
        .try_into()
        .map_err(|_| TagWriteError::Io("bad SHA-256 length".to_string()))?;

    // The original's permission bits, set last: a read-only original must not stop the write
    // into the copy.
    fs::set_permissions(copy, fs::metadata(path)?.permissions())?;
    if stamp(path)? != before {
        return Err(TagWriteError::Verify(
            "the file changed during the tag write; nothing written".to_string(),
        ));
    }
    fs::rename(copy, path)?;
    hold.renamed = true;
    if hooks.stop_after_rename {
        return Err(TagWriteError::Io(
            "stopped after the rename (test hook)".to_string(),
        ));
    }
    // The new content is in place; a failed folder sync only weakens durability across a power
    // loss, so it is logged, not reported as a failed write (the organizer ignores it outright).
    if let Some(dir) = path.parent()
        && let Err(e) = File::open(dir).and_then(|d| d.sync_all())
    {
        tracing::warn!(folder = %dir.display(), "cannot sync the folder after a tag write: {e}");
    }
    Ok(Written {
        after_sha,
        audio_md5: md5,
    })
}

/// Copies the bytes into a new file (never over an existing one).
fn copy_file(from: &Path, to: &Path) -> Result<(), TagWriteError> {
    let mut src = File::open(from)?;
    let mut dst = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    std::io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    Ok(())
}
