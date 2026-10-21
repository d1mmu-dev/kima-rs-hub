//! The scanner: a full incremental reconcile of the library root, and single-file updates.
//!
//! Both run on the `library` job queue (concurrency 1), so they never overlap with each other
//! or with organizer runs. A file whose size and mtime match its row is never read. Changed
//! files are grouped by folder so the album year (the mode of the tracks' own years within one
//! folder, album artist and album title) is decided before anything is written. Every file is
//! written in its own transaction.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use kima_core::events::{Event, EventBus};
use kima_db::PgPool;
use kima_db::library::{self, IndexedTrack, ResolvedTrack};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use crate::resolve::{Resolved, resolve};
use crate::tags::{self, RawTags};
use crate::{fingerprint, names};

/// Tracks missing for longer than this are deleted (spec §4.3).
pub const MISSING_GRACE: Duration = Duration::from_secs(7 * 24 * 3600);
/// At most four progress events per second.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const MARK_CHUNK: usize = 1000;

/// The library root that track paths are relative to. Shared by the HTTP side, the job
/// worker and the watcher; switching the library to an organized copy changes it for all of
/// them at once. Cheap to clone; all clones share one value.
#[derive(Clone)]
pub struct LibraryRoot(Arc<tokio::sync::watch::Sender<PathBuf>>);

impl LibraryRoot {
    pub fn new(root: PathBuf) -> Self {
        Self(Arc::new(tokio::sync::watch::Sender::new(root)))
    }

    pub fn get(&self) -> PathBuf {
        self.0.borrow().clone()
    }

    pub fn set(&self, root: PathBuf) {
        self.0.send_replace(root);
    }

    /// Notified on every [`set`](Self::set).
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<PathBuf> {
        self.0.subscribe()
    }
}

impl From<PathBuf> for LibraryRoot {
    fn from(root: PathBuf) -> Self {
        Self::new(root)
    }
}

impl std::fmt::Debug for LibraryRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LibraryRoot").field(&self.get()).finish()
    }
}

#[derive(Clone)]
pub struct LibraryCtx {
    pub pool: PgPool,
    /// The library root that track paths are relative to (see [`effective_root`]).
    pub root: LibraryRoot,
    pub events: EventBus,
    pub data_dir: PathBuf,
    /// Paths the watcher must not react to; the organizer adds the files it moves.
    pub ignore: crate::watch::IgnoreSet,
}

/// `system_settings.library_root` when set, else `MUSIC_PATH`.
pub async fn effective_root(pool: &PgPool, music_path: &Path) -> sqlx::Result<PathBuf> {
    let settings = kima_db::settings::get(pool).await?;
    Ok(settings
        .library_root
        .filter(|r| !r.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| music_path.to_path_buf()))
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub added: usize,
    /// Changed files re-read, plus unchanged files moved to a different album year.
    pub updated: usize,
    pub repointed: usize,
    /// Tracks newly marked missing by this run (and not re-pointed in it).
    pub missing: usize,
    pub purged: usize,
    /// Missing tracks whose file is back, unchanged, at the same path.
    pub restored: usize,
    /// Files skipped because size and mtime matched.
    pub unchanged: usize,
    /// How many files had their tags read.
    pub tags_read: usize,
    /// (relative path, reason) for files or folders that could not be read.
    pub failed: Vec<(String, String)>,
    /// The walk found no audio files although the library has present tracks: most likely
    /// an unmounted disk (an empty mount point). Nothing was marked missing or purged.
    pub root_empty_guard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOutcome {
    Added,
    Updated,
    Unchanged,
    Repointed,
    MarkedMissing,
    /// Not an audio file, hidden, not under the root, or a vanished path kima never knew.
    Ignored,
    /// The file is gone, but so is the library (root missing, unreadable or without any audio
    /// file, as when a disk is unmounted); nothing was changed.
    RootUnavailable,
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("library root {path} is not readable: {source}")]
    Root {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("scan task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    /// The job's cancel token fired (shutdown or a lost claim); the run stopped between
    /// folders. What was stored so far stays; the next reconcile finishes the rest.
    #[error("scan cancelled")]
    Cancelled,
}

/// A file found on disk.
#[derive(Debug, Clone)]
struct DiskFile {
    rel: String,
    size: i64,
    mtime: OffsetDateTime,
}

/// A changed file, read and resolved, not yet stored.
struct Prepared {
    disk: DiskFile,
    raw: RawTags,
    resolved: Resolved,
    fingerprint: [u8; 32],
}

type GroupKey = (String, String);

/// The album year of a group: the most common of its tracks' years, ties to the earliest.
fn mode_year(years: impl IntoIterator<Item = Option<i32>>) -> Option<i32> {
    let mut counts: BTreeMap<i32, usize> = BTreeMap::new();
    for y in years.into_iter().flatten() {
        *counts.entry(y).or_default() += 1;
    }
    // BTreeMap iterates ascending; keep the first year that reaches the top count.
    let mut best: Option<(i32, usize)> = None;
    for (y, n) in counts {
        if best.is_none_or(|(_, b)| n > b) {
            best = Some((y, n));
        }
    }
    best.map(|(y, _)| y)
}

fn conflicts(own: Option<i32>, album: Option<i32>) -> bool {
    own.is_some() && own != album
}

fn folder_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(d, _)| d)
}

fn dir_prefix(folder: &str) -> String {
    if folder.is_empty() {
        String::new()
    } else {
        format!("{folder}/")
    }
}

/// Postgres keeps microseconds; compare at that precision.
fn to_db_time(t: SystemTime) -> OffsetDateTime {
    let t = OffsetDateTime::from(t);
    t.replace_nanosecond(t.nanosecond() / 1000 * 1000)
        .unwrap_or(t)
}

fn disk_file(rel: String, meta: &std::fs::Metadata) -> DiskFile {
    DiskFile {
        rel,
        size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
        mtime: to_db_time(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)),
    }
}

/// Only constraint and data errors are a problem with one file; anything else (connection,
/// pool, protocol) means the database is unwell and the job should stop and retry.
fn per_file_db_error(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(_))
}

struct Progress<'a> {
    events: &'a EventBus,
    total: usize,
    done: usize,
    last: Option<Instant>,
}

impl<'a> Progress<'a> {
    fn new(events: &'a EventBus, total: usize) -> Self {
        Self {
            events,
            total,
            done: 0,
            last: None,
        }
    }

    fn send(&mut self) {
        self.last = Some(Instant::now());
        self.events.publish(Event {
            user_id: None,
            data: serde_json::json!({
                "type": "library:progress",
                "job": "scan",
                "done": self.done,
                "total": self.total,
            }),
        });
    }

    fn advance(&mut self, n: usize) {
        self.done = (self.done + n).min(self.total);
        if self.last.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL) {
            self.send();
        }
    }

    fn finish(&mut self) {
        self.done = self.total;
        self.send();
    }
}

struct Walk {
    files: Vec<DiskFile>,
    /// Folders that could not be read; tracks under them are not marked missing.
    unreadable: Vec<String>,
    failed: Vec<(String, String)>,
}

fn rel_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let s = rel.to_str()?;
    Some(s.replace('\\', "/"))
}

/// Dot-prefixed names (hidden files and folders, `._` AppleDouble files) are never indexed.
fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn walk(root: &Path) -> Result<Walk, ScanError> {
    std::fs::read_dir(root).map_err(|source| ScanError::Root {
        path: root.to_path_buf(),
        source,
    })?;
    let mut out = Walk {
        files: Vec::new(),
        unreadable: Vec::new(),
        failed: Vec::new(),
    };
    // Symlinks are not followed, as upstream.
    let entries = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(&e.file_name().to_string_lossy()));
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) if e.depth() == 0 => {
                return Err(ScanError::Root {
                    path: root.to_path_buf(),
                    source: e
                        .into_io_error()
                        .unwrap_or_else(|| std::io::Error::other("cannot walk the library root")),
                });
            }
            Err(e) => {
                // Shield the unreadable sub-path from missing-marking. Never push "" (that
                // would shield everything); a path that cannot be made relative is only
                // reported.
                let path = e.path().map(Path::to_path_buf);
                let rel = path.as_deref().and_then(|p| {
                    p.strip_prefix(root)
                        .ok()
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                });
                match rel.filter(|r| !r.is_empty()) {
                    Some(rel) => {
                        out.failed.push((rel.clone(), e.to_string()));
                        out.unreadable.push(rel);
                    }
                    None => out.failed.push((
                        path.map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        e.to_string(),
                    )),
                }
                continue;
            }
        };
        if !entry.file_type().is_file() || !tags::is_audio(entry.path()) {
            continue;
        }
        let Some(rel) = rel_of(root, entry.path()) else {
            out.failed.push((
                entry.path().to_string_lossy().into_owned(),
                "path is not valid UTF-8".to_string(),
            ));
            continue;
        };
        match entry.metadata() {
            Ok(meta) => out.files.push(disk_file(rel, &meta)),
            Err(e) => out.failed.push((rel, e.to_string())),
        }
    }
    Ok(out)
}

fn under_any(rel: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| {
        !p.is_empty()
            && (rel == p || (rel.starts_with(p.as_str()) && rel[p.len()..].starts_with('/')))
    })
}

async fn prepare(root: &Path, disk: DiskFile) -> Result<Prepared, String> {
    let full = root.join(&disk.rel);
    let rel = disk.rel.clone();
    let read = tokio::task::spawn_blocking(move || {
        let raw = tags::read(&full).map_err(|e| e.to_string())?;
        let fp = fingerprint::of(&full).map_err(|e| e.to_string())?;
        Ok::<_, String>((raw, fp))
    })
    .await
    .map_err(|e| format!("reading tags panicked: {e}"))?;
    let (raw, fingerprint) = read?;
    let resolved = resolve(&raw, Path::new(&rel));
    Ok(Prepared {
        disk,
        raw,
        resolved,
        fingerprint,
    })
}

fn group_key(r: &Resolved) -> GroupKey {
    (names::key(&r.album_artist), names::key(&r.album))
}

fn to_track(p: &Prepared, album_year: Option<i32>) -> ResolvedTrack {
    let r = &p.resolved;
    let mut guessed = r.guessed.clone();
    guessed.year_conflict = conflicts(r.year, album_year);
    ResolvedTrack {
        album_artist: r.album_artist.clone(),
        album_artist_key: names::key(&r.album_artist),
        track_artist: r.track_artist.clone(),
        album: r.album.clone(),
        album_key: names::key(&r.album),
        album_year,
        track_year: r.year,
        is_compilation: r.is_compilation,
        disc: i32::try_from(r.disc).unwrap_or(0),
        track_no: i32::try_from(r.track).unwrap_or(0),
        title: r.title.clone(),
        duration_secs: p.raw.duration_secs.round() as i32,
        codec: p.raw.codec.clone().unwrap_or_else(|| "unknown".to_string()),
        bitrate: p.raw.bitrate.and_then(|b| i32::try_from(b).ok()),
        path: p.disk.rel.clone(),
        size: p.disk.size,
        mtime: p.disk.mtime,
        fingerprint: p.fingerprint,
        guessed,
    }
}

/// What storing one file did.
struct StoreResult {
    stored: Stored,
    /// The row already existed (here or re-pointed from elsewhere) and is now on another album,
    /// so its old album may be empty.
    album_moved: bool,
}

/// Stores one file in its own transaction. For a path with no row (`repoint`), first tries to
/// re-point a missing track with the same fingerprint; the re-point and the metadata upsert
/// commit or roll back together, so a failed upsert never leaves a half-moved track.
async fn store(pool: &PgPool, t: &ResolvedTrack, repoint: bool) -> sqlx::Result<StoreResult> {
    let mut tx = pool.begin().await?;
    let repointed = if repoint {
        library::repoint_by_fingerprint_in(&mut tx, &t.fingerprint, &t.path).await?
    } else {
        None
    };
    let old_album = library::album_of_path(&mut tx, &t.path).await?;
    library::upsert_track(&mut tx, t).await?;
    let new_album = library::album_of_path(&mut tx, &t.path).await?;
    tx.commit().await?;
    let stored = match (repointed, old_album) {
        (Some(id), _) => Stored::Repointed(id),
        (None, Some(_)) => Stored::Updated,
        (None, None) => Stored::Added,
    };
    Ok(StoreResult {
        stored,
        album_moved: old_album.is_some() && old_album != new_album,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored {
    Added,
    Updated,
    /// Took over a missing track's row; carries that track's id.
    Repointed(Uuid),
}

#[derive(Default)]
struct FolderResult {
    stored: Vec<(String, Stored)>,
    failed: Vec<(String, String)>,
    tags_read: usize,
    /// Some existing track changed album.
    album_moved: bool,
}

/// Reads and stores the changed files of one folder. `siblings` are the folder's other
/// present rows, unchanged on disk; they count towards the album year, and any whose album
/// year or conflict flag changes is re-read and stored too. `known` holds every row at a path
/// of `changed`.
async fn process_folder(
    ctx: &LibraryCtx,
    changed: Vec<DiskFile>,
    siblings: &[&IndexedTrack],
    known: &HashMap<String, IndexedTrack>,
) -> Result<FolderResult, ScanError> {
    let mut out = FolderResult::default();
    let mut prepared = Vec::with_capacity(changed.len());
    for disk in changed {
        out.tags_read += 1;
        let rel = disk.rel.clone();
        match prepare(&ctx.root.get(), disk).await {
            Ok(p) => prepared.push(p),
            Err(e) => out.failed.push((rel, e)),
        }
    }

    let mut groups: HashMap<GroupKey, Vec<Option<i32>>> = HashMap::new();
    for p in &prepared {
        groups
            .entry(group_key(&p.resolved))
            .or_default()
            .push(p.resolved.year);
    }
    for s in siblings {
        groups
            .entry((s.artist_key.clone(), s.album_key.clone()))
            .or_default()
            .push(s.track_year);
    }
    let years: HashMap<GroupKey, Option<i32>> = groups
        .into_iter()
        .map(|(k, ys)| (k, mode_year(ys)))
        .collect();

    for p in &prepared {
        let year = years.get(&group_key(&p.resolved)).copied().flatten();
        let track = to_track(p, year);
        let rel = p.disk.rel.clone();
        match store(&ctx.pool, &track, !known.contains_key(&rel)).await {
            Ok(r) => {
                out.album_moved |= r.album_moved;
                out.stored.push((rel, r.stored));
            }
            Err(e) if per_file_db_error(&e) => out.failed.push((rel, e.to_string())),
            Err(e) => return Err(e.into()),
        }
    }

    // Unchanged siblings whose album year or conflict flag moved with the group.
    for s in siblings {
        let year = years
            .get(&(s.artist_key.clone(), s.album_key.clone()))
            .copied()
            .flatten();
        if s.album_year == year && s.year_conflict == conflicts(s.track_year, year) {
            continue;
        }
        let full = ctx.root.get().join(&s.path);
        let meta = match tokio::fs::metadata(&full).await {
            Ok(m) => m,
            Err(e) => {
                out.failed.push((s.path.clone(), e.to_string()));
                continue;
            }
        };
        out.tags_read += 1;
        let p = match prepare(&ctx.root.get(), disk_file(s.path.clone(), &meta)).await {
            Ok(p) => p,
            Err(e) => {
                out.failed.push((s.path.clone(), e));
                continue;
            }
        };
        // Re-read tags may have moved it to another group; use that group's year if known.
        let year = years.get(&group_key(&p.resolved)).copied().unwrap_or(year);
        match store(&ctx.pool, &to_track(&p, year), false).await {
            Ok(r) => {
                out.album_moved |= r.album_moved;
                out.stored.push((s.path.clone(), Stored::Updated));
            }
            Err(e) if per_file_db_error(&e) => out.failed.push((s.path.clone(), e.to_string())),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

/// Walks the whole library: indexes new and changed files, re-points moved ones, marks
/// vanished ones missing, purges tracks missing past the grace period and prunes empty
/// albums and artists. A missing or unreadable root is an error and changes nothing.
pub async fn reconcile(ctx: &LibraryCtx) -> Result<ScanReport, ScanError> {
    reconcile_cancellable(ctx, &CancellationToken::new()).await
}

/// [`reconcile`], stopping with [`ScanError::Cancelled`] once `cancel` fires: checked after the
/// walk (before anything is marked missing), between folders, and before the purge.
pub async fn reconcile_cancellable(
    ctx: &LibraryCtx,
    cancel: &CancellationToken,
) -> Result<ScanReport, ScanError> {
    let check = || {
        if cancel.is_cancelled() {
            Err(ScanError::Cancelled)
        } else {
            Ok(())
        }
    };
    check()?;
    let root = ctx.root.get();
    let walked = tokio::task::spawn_blocking(move || walk(&root)).await??;
    let mut report = ScanReport {
        failed: walked.failed,
        ..Default::default()
    };
    let mut progress = Progress::new(&ctx.events, walked.files.len());
    progress.send();

    let index: HashMap<String, IndexedTrack> = library::scan_index(&ctx.pool, None)
        .await?
        .into_iter()
        .map(|t| (t.path.clone(), t))
        .collect();

    let on_disk: HashSet<&str> = walked.files.iter().map(|f| f.rel.as_str()).collect();
    let mut restore = Vec::new();
    let mut changed: BTreeMap<String, Vec<DiskFile>> = BTreeMap::new();
    // Rows that are present, on disk and unchanged after this run, per folder.
    let mut settled: HashMap<&str, Vec<&IndexedTrack>> = HashMap::new();
    for f in &walked.files {
        match index.get(&f.rel) {
            Some(row) if row.size == f.size && row.mtime == f.mtime => {
                if row.missing {
                    restore.push(f.rel.clone());
                } else {
                    report.unchanged += 1;
                }
                settled.entry(folder_of(&row.path)).or_default().push(row);
            }
            _ => changed
                .entry(folder_of(&f.rel).to_string())
                .or_default()
                .push(f.clone()),
        }
    }

    // Review Focus 2: an unmounted disk usually leaves an empty, readable mount point. If
    // the walk found no audio while the library has tracks, assume that rather than "the whole
    // library was deleted": mark nothing missing and purge nothing. Missing rows count too, so
    // single-file updates marking tracks missing one by one cannot defeat the guard.
    report.root_empty_guard = walked.files.is_empty() && !index.is_empty();
    if report.root_empty_guard {
        warn!(
            root = %ctx.root.get().display(),
            "library root has no audio files but the library has tracks; is the disk mounted? \
             Not marking anything missing"
        );
    }

    check()?;
    // Mark vanished files missing first, so a moved file's old row can be re-pointed below.
    // Files an unfinished organizer run parked under a hidden staging name are not walked,
    // but they are there: their tracks are not missing.
    let root = ctx.root.get();
    let parked: HashSet<String> = kima_db::organizer::parked_paths(&ctx.pool)
        .await?
        .into_iter()
        .filter(|p| root.join(p).is_file())
        .collect();
    let vanished: Vec<String> = if report.root_empty_guard {
        Vec::new()
    } else {
        index
            .values()
            .filter(|t| !t.missing && !on_disk.contains(t.path.as_str()))
            .filter(|t| !parked.contains(&t.path))
            .filter(|t| !under_any(&t.path, &walked.unreadable))
            .map(|t| t.path.clone())
            .collect()
    };
    for chunk in vanished.chunks(MARK_CHUNK) {
        report.missing += library::mark_missing(&ctx.pool, chunk).await? as usize;
    }
    let vanished_ids: HashSet<Uuid> = vanished.iter().map(|p| index[p].id).collect();
    for chunk in restore.chunks(MARK_CHUNK) {
        report.restored += library::clear_missing(&ctx.pool, chunk).await? as usize;
    }
    progress.advance(report.unchanged + restore.len());

    for (folder, files) in changed {
        check()?;
        let n = files.len();
        let siblings = settled.get(folder.as_str()).map_or(&[][..], Vec::as_slice);
        let result = process_folder(ctx, files, siblings, &index).await?;
        report.tags_read += result.tags_read;
        report.failed.extend(result.failed);
        for (_, stored) in result.stored {
            match stored {
                Stored::Added => report.added += 1,
                Stored::Updated => report.updated += 1,
                Stored::Repointed(id) => {
                    report.repointed += 1;
                    if vanished_ids.contains(&id) {
                        report.missing = report.missing.saturating_sub(1);
                    }
                }
            }
        }
        progress.advance(n);
    }

    check()?;
    if !report.root_empty_guard {
        report.purged = library::purge_missing(&ctx.pool, MISSING_GRACE).await? as usize;
    }
    prune_empty(ctx).await?;
    progress.finish();
    if !report.failed.is_empty() {
        warn!(
            failed = report.failed.len(),
            "some library files could not be read"
        );
    }
    Ok(report)
}

/// A relative path made only of normal, non-hidden components, or `None`.
fn clean_rel(rel: &str) -> Option<String> {
    let mut parts = Vec::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str()?;
                if is_hidden(s) {
                    return None;
                }
                parts.push(s);
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Brings one file's row up to date: adds, re-reads, re-points or marks it missing. Used for
/// watcher events. Directories are not expanded here.
pub async fn update_path(ctx: &LibraryCtx, rel: &str) -> Result<PathOutcome, ScanError> {
    let Some(rel) = clean_rel(rel) else {
        return Ok(PathOutcome::Ignored);
    };
    if !tags::is_audio(Path::new(&rel)) {
        return Ok(PathOutcome::Ignored);
    }
    let meta = match tokio::fs::metadata(ctx.root.get().join(&rel)).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(match library::find_by_path(&ctx.pool, &rel).await? {
                None => PathOutcome::Ignored,
                Some(row) if row.missing_since.is_some() => PathOutcome::Unchanged,
                Some(_) if !root_available(&ctx.root.get()).await => PathOutcome::RootUnavailable,
                Some(_) => {
                    library::mark_missing(&ctx.pool, std::slice::from_ref(&rel)).await?;
                    PathOutcome::MarkedMissing
                }
            });
        }
        Err(e) => return Ok(PathOutcome::Failed(e.to_string())),
    };
    if !meta.is_file() {
        return Ok(PathOutcome::Ignored);
    }
    let disk = disk_file(rel.clone(), &meta);

    let folder: HashMap<String, IndexedTrack> =
        library::scan_index(&ctx.pool, Some(&dir_prefix(folder_of(&rel))))
            .await?
            .into_iter()
            .map(|t| (t.path.clone(), t))
            .collect();
    let mut known = HashMap::new();
    if let Some(row) = folder.get(&rel) {
        if row.size == disk.size && row.mtime == disk.mtime {
            if row.missing {
                library::clear_missing(&ctx.pool, std::slice::from_ref(&rel)).await?;
                return Ok(PathOutcome::Updated);
            }
            return Ok(PathOutcome::Unchanged);
        }
        known.insert(rel.clone(), row.clone());
    }
    let siblings: Vec<&IndexedTrack> = folder
        .values()
        .filter(|t| t.path != rel && !t.missing)
        .collect();

    let result = process_folder(ctx, vec![disk], &siblings, &known).await?;
    let mine = result
        .stored
        .iter()
        .find(|(p, _)| *p == rel)
        .map(|(_, s)| *s);
    let outcome = match mine {
        Some(Stored::Added) => PathOutcome::Added,
        Some(Stored::Updated) => PathOutcome::Updated,
        Some(Stored::Repointed(_)) => PathOutcome::Repointed,
        None => {
            let reason = result
                .failed
                .iter()
                .find(|(p, _)| *p == rel)
                .map_or_else(|| "not stored".to_string(), |(_, e)| e.clone());
            PathOutcome::Failed(reason)
        }
    };
    if result.album_moved {
        // An existing track changed album (new tags, or the folder's album year moved), so
        // its old album may now be empty. Safe here: prune_empty must not race upserts, and
        // update_path only runs on the concurrency-1 `library` queue, like every other writer.
        prune_empty(ctx).await?;
    }
    Ok(outcome)
}

/// Prunes empty albums and artists, then deletes the cover cache of each deleted album.
/// A cache folder that cannot be removed is logged, not fatal: the rows are already gone.
async fn prune_empty(ctx: &LibraryCtx) -> Result<(), ScanError> {
    let albums = library::prune_empty(&ctx.pool).await?;
    if albums.is_empty() {
        return Ok(());
    }
    let data_dir = ctx.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        for id in albums {
            if let Err(e) = crate::artwork::remove_album(&data_dir, id) {
                warn!(album = %id, error = %e, "could not remove the cover cache of a deleted album");
            }
        }
    })
    .await?;
    Ok(())
}

/// The root is readable and holds at least one audio file. An unmounted disk usually
/// leaves an empty mount point behind.
async fn root_available(root: &Path) -> bool {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || has_audio(&root))
        .await
        .unwrap_or(false)
}

/// The root holds at least one non-hidden audio file, by the same rules as the reconcile walk.
/// Stops at the first hit. `.DS_Store`, `lost+found/` and other placeholders do not count.
fn has_audio(root: &Path) -> bool {
    if std::fs::read_dir(root).is_err() {
        return false;
    }
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(&e.file_name().to_string_lossy()))
        .filter_map(Result::ok)
        .any(|e| e.file_type().is_file() && tags::is_audio(e.path()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_year_prefers_most_common_then_earliest() {
        assert_eq!(mode_year([Some(1999), Some(1999), Some(2001)]), Some(1999));
        assert_eq!(mode_year([Some(2005), Some(2003)]), Some(2003));
        assert_eq!(mode_year([None, Some(2001), None]), Some(2001));
        assert_eq!(mode_year([None, None]), None);
        assert_eq!(mode_year([]), None);
        assert_eq!(
            mode_year([Some(2001), Some(1990), Some(2001), Some(1990)]),
            Some(1990)
        );
    }

    #[test]
    fn conflict_needs_an_own_year() {
        assert!(conflicts(Some(2001), Some(1999)));
        assert!(conflicts(Some(2001), None));
        assert!(!conflicts(None, Some(1999)));
        assert!(!conflicts(Some(1999), Some(1999)));
    }

    #[test]
    fn rel_paths_must_stay_under_root() {
        assert_eq!(clean_rel("A/B/x.mp3").as_deref(), Some("A/B/x.mp3"));
        assert_eq!(clean_rel("../x.mp3"), None);
        assert_eq!(clean_rel("/etc/x.mp3"), None);
        assert_eq!(clean_rel("A/./x.mp3").as_deref(), Some("A/x.mp3"));
        assert_eq!(clean_rel("A//x.mp3/").as_deref(), Some("A/x.mp3"));
        assert_eq!(clean_rel("A/../x.mp3"), None);
        assert_eq!(clean_rel(""), None);
        assert_eq!(clean_rel(".hidden/x.mp3"), None);
        assert_eq!(clean_rel("A/._x.mp3"), None);
    }

    #[test]
    fn prefixes_match_whole_components() {
        let p = vec!["A/B".to_string()];
        assert!(under_any("A/B/x.mp3", &p));
        assert!(!under_any("A/BB/x.mp3", &p));
        assert!(!under_any("anything", &[String::new()]));
    }

    #[test]
    fn folders() {
        assert_eq!(folder_of("A/B/x.mp3"), "A/B");
        assert_eq!(folder_of("x.mp3"), "");
        assert_eq!(dir_prefix(""), "");
        assert_eq!(dir_prefix("A/B"), "A/B/");
    }
}
