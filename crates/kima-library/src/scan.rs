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
use kima_db::genres::{self, GenreSource};
use kima_db::library::{self, IndexedTrack, ResolvedTrack, StoredCredit, TagIds};
use kima_db::{PgConnection, PgPool, retry_once_on_conflict};
use kima_db::{lyrics, stages};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::names::canonical_various;
use crate::resolve::{Resolved, resolve};
use crate::tags::{self, RawTags, TagOverrides};
use crate::vote::{self, Candidate, Credit};
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
    /// Links the walk could not follow (broken, unreadable, or pointing at or above the
    /// music folder), each as `<link>: <why>`. Logged once per scan, never fatal (D-162).
    pub skipped_links: Vec<String>,
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

/// Why a file is read. A file kima-rs itself just tagged (`TagWrite`, spec §19.7) is proven to
/// carry the same audio (the audio MD5 matched before the rename), so its re-read neither
/// reopens the track's stages nor drops its stored fingerprint; a `Scan` read of a changed file
/// reopens them as always.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadCause {
    Scan,
    TagWrite,
}

/// A file found on disk.
#[derive(Debug, Clone)]
struct DiskFile {
    rel: String,
    size: i64,
    mtime: OffsetDateTime,
    /// The file, or a folder above it under the root, is a symlink (`guessed.viaSymlink`).
    via_link: bool,
}

/// A changed file, read and resolved, not yet stored.
struct Prepared {
    disk: DiskFile,
    raw: RawTags,
    resolved: Resolved,
    fingerprint: [u8; 32],
    /// The identity key of the artist the file names itself (its vote), before the album's
    /// election: a track on an autotag-applied album that agrees with it stays there.
    own_key: String,
    /// The MusicBrainz id of the artist the file is filed under (D-164).
    artist_mbid: Option<Uuid>,
    /// The file's vote, stored with the row (`guessed.credit`).
    credit: StoredCredit,
    /// The missing track the merge picked for this file (a moved or renamed one), its stored
    /// override, if any, merged into `raw`: the file may only take over exactly that track.
    target: Option<Uuid>,
}

/// How [`store`] may take over a missing track for a path with no row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repoint {
    /// The path has a row; nothing is re-pointed.
    No,
    /// Any missing track with the same fingerprint (earliest missing first, locked rows skipped).
    /// Only for a file the merge found no missing track for.
    Any,
    /// Exactly the track the merge picked (its override, if any, merged into the file's tags);
    /// waits for its row lock. If it is no longer missing (or gone), nothing is stored.
    Exact(Uuid),
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
        via_link: false,
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
    skipped_links: Vec<String>,
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

/// The file at `rel`, or any folder above it under `root`, is a symlink. The
/// root itself may be a link; only the parts below it count. Blocking.
pub(crate) fn symlinked(root: &Path, rel: &str) -> bool {
    let mut cur = root.to_path_buf();
    for c in Path::new(rel).components() {
        if let Component::Normal(part) = c {
            cur.push(part);
            if std::fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
                return true;
            }
        }
    }
    false
}

/// Why a link is not followed.
const ABOVE_ROOT: &str = "it points above the music folder";

struct Walker<'a> {
    root: &'a Path,
    real_root: PathBuf,
    out: Walk,
    /// Folders already walked, by real path: loops and double visits end here.
    walked: HashSet<PathBuf>,
    /// Audio files already listed, by real path: one file is one song.
    taken: HashSet<PathBuf>,
    /// Links met, not yet followed: `(link as walked, its path relative to the root)`.
    links: Vec<(PathBuf, String)>,
    /// Every recorded path and each folder on the way to it.
    recorded: HashSet<String>,
}

impl Walker<'_> {
    /// `dir` is the folder as walked (under the root, links kept); `real` is the same folder
    /// with its links resolved. Below `real` nothing is a link until one is queued, so real
    /// paths are joined, not asked of the disk: a library with no links pays nothing.
    fn walk_dir(&mut self, dir: &Path, real: &Path, via_link: bool) {
        if !self.walked.insert(real.to_path_buf()) {
            return;
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => return self.unreadable_dir(dir, &e, via_link),
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name();
            if is_hidden(&name.to_string_lossy()) {
                continue;
            }
            let full = dir.join(&name);
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                self.walk_dir(&full, &real.join(&name), via_link);
            } else if ft.is_symlink() {
                match rel_of(self.root, &full) {
                    Some(rel) => self.links.push((full, rel)),
                    None => self.not_utf8(&full),
                }
            } else if ft.is_file() && tags::is_audio(&full) {
                if !self.taken.insert(real.join(&name)) {
                    continue;
                }
                match entry.metadata() {
                    Ok(meta) => self.push_file(&full, &meta, via_link),
                    Err(e) => self.file_error(&full, e.to_string()),
                }
            }
        }
    }

    fn unreadable_dir(&mut self, dir: &Path, e: &std::io::Error, via_link: bool) {
        let Some(rel) = rel_of(self.root, dir).filter(|r| !r.is_empty()) else {
            self.out
                .failed
                .push((dir.to_string_lossy().into_owned(), e.to_string()));
            return;
        };
        if via_link && e.kind() == std::io::ErrorKind::NotFound {
            // A folder that vanished under a link: gone like any other file.
            self.out.skipped_links.push(format!("{rel}: {e}"));
            return;
        }
        if via_link {
            self.out.skipped_links.push(format!("{rel}: {e}"));
        }
        // Shield what is under it: unreadable is not gone.
        self.out.failed.push((rel.clone(), e.to_string()));
        self.out.unreadable.push(rel);
    }

    fn not_utf8(&mut self, path: &Path) {
        self.out.failed.push((
            path.to_string_lossy().into_owned(),
            "path is not valid UTF-8".to_string(),
        ));
    }

    fn file_error(&mut self, full: &Path, why: String) {
        match rel_of(self.root, full) {
            Some(rel) => self.out.failed.push((rel, why)),
            None => self.not_utf8(full),
        }
    }

    fn push_file(&mut self, full: &Path, meta: &std::fs::Metadata, via_link: bool) {
        match rel_of(self.root, full) {
            Some(rel) => {
                let mut f = disk_file(rel, meta);
                f.via_link = via_link;
                self.out.files.push(f);
            }
            None => self.not_utf8(full),
        }
    }

    fn follow(&mut self, link: &Path, rel: &str) {
        let target = match std::fs::canonicalize(link) {
            Ok(t) => t,
            Err(e) => {
                self.out.skipped_links.push(format!("{rel}: {e}"));
                return;
            }
        };
        let meta = match std::fs::metadata(&target) {
            Ok(m) => m,
            Err(e) => {
                self.out.skipped_links.push(format!("{rel}: {e}"));
                return;
            }
        };
        if meta.is_dir() {
            // The folder holding the music folder is a loop by another name, and could be the
            // whole disk. (The music folder itself is just walked already.)
            if target != self.real_root && self.real_root.starts_with(&target) {
                self.out.skipped_links.push(format!("{rel}: {ABOVE_ROOT}"));
                return;
            }
            self.walk_dir(link, &target, true);
        } else if meta.is_file() && tags::is_audio(link) && self.taken.insert(target) {
            self.push_file(link, &meta, true);
        }
    }
}

/// Every audio file under `root`, following links (D-162, upstream `findAudioFiles`). A linked
/// song is listed where the link stands, so it moves with the link and not with its target.
/// One real file is one song however many paths reach it: real paths come first, then links,
/// those already holding a recorded song (`recorded`, relative paths) before the rest, so a new
/// link never takes a song from the path it is filed under. A link that cannot be followed is
/// skipped and reported, never fatal; the root and a real folder that cannot be read are
/// shielded as before. Blocking.
fn walk(root: &Path, recorded: &HashSet<String>) -> Result<Walk, ScanError> {
    let root_error = |source| ScanError::Root {
        path: root.to_path_buf(),
        source,
    };
    std::fs::read_dir(root).map_err(root_error)?;
    let real_root = std::fs::canonicalize(root).map_err(root_error)?;
    let mut w = Walker {
        root,
        real_root: real_root.clone(),
        out: Walk {
            files: Vec::new(),
            unreadable: Vec::new(),
            failed: Vec::new(),
            skipped_links: Vec::new(),
        },
        walked: HashSet::new(),
        taken: HashSet::new(),
        links: Vec::new(),
        recorded: HashSet::new(),
    };
    for path in recorded {
        let mut at = Some(Path::new(path));
        while let Some(p) = at.filter(|p| !p.as_os_str().is_empty()) {
            w.recorded.insert(p.to_string_lossy().into_owned());
            at = p.parent();
        }
    }
    w.walk_dir(root, &real_root, false);
    // A followed folder can hold more links, which are a round of their own.
    while !w.links.is_empty() {
        let mut round = std::mem::take(&mut w.links);
        round.sort_by(|a, b| {
            (!w.recorded.contains(&a.1), &a.1).cmp(&(!w.recorded.contains(&b.1), &b.1))
        });
        for (link, rel) in round {
            w.follow(&link, &rel);
        }
    }
    Ok(w.out)
}

fn under_any(rel: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| {
        !p.is_empty()
            && (rel == p || (rel.starts_with(p.as_str()) && rel[p.len()..].starts_with('/')))
    })
}

/// Reads one file and resolves it, with the path's stored override (if any) merged over the
/// file's tags first (spec §19.7: every read, so a database-only match survives rescans).
async fn prepare(
    root: &Path,
    disk: DiskFile,
    over: Option<&TagOverrides>,
) -> Result<Prepared, String> {
    let full = root.join(&disk.rel);
    let rel = disk.rel.clone();
    let read = tokio::task::spawn_blocking(move || {
        let raw = tags::read(&full).map_err(|e| e.to_string())?;
        let fp = fingerprint::of(&full).map_err(|e| e.to_string())?;
        Ok::<_, String>((raw, fp))
    })
    .await
    .map_err(|e| format!("reading tags panicked: {e}"))?;
    let (mut raw, fingerprint) = read?;
    if let Some(o) = over {
        raw.apply_overrides(o);
    }
    let resolved = resolve(&raw, Path::new(&rel));
    let mut p = Prepared {
        disk,
        raw,
        resolved,
        fingerprint,
        own_key: String::new(),
        artist_mbid: None,
        credit: StoredCredit {
            album_key: None,
            name: String::new(),
            mbid: None,
            from_album_artist: false,
            compilation: false,
        },
        target: None,
    };
    p.file_under(None);
    Ok(p)
}

impl Prepared {
    /// The file's vote for its album's artist.
    fn vote(&self) -> Credit {
        vote::credit_of(&self.raw, &self.resolved, &self.disk.rel)
    }

    /// Files the song under `chosen`, the artist its album elected, or under its own artist
    /// when none leads (`None`). Sets everything that follows from the artist: the album
    /// artist, the compilation flag (the album artist is Various Artists), the artist's MBID
    /// and the stored vote. Safe to repeat: the vote is built from the file's own tags.
    fn file_under(&mut self, chosen: Option<&Candidate>) {
        let credit = self.vote();
        let chosen = chosen.unwrap_or(&credit.candidate);
        let various = canonical_various(&chosen.name).is_some();
        self.own_key = names::key(&credit.candidate.name);
        self.resolved.album_artist = if various {
            names::VARIOUS_ARTISTS.to_string()
        } else {
            chosen.name.clone()
        };
        self.resolved.is_compilation = various;
        self.artist_mbid = if various { None } else { chosen.mbid };
        self.credit = StoredCredit {
            album_key: credit.album_key,
            name: credit.candidate.name,
            mbid: credit.candidate.mbid,
            from_album_artist: credit.from_album_artist,
            compilation: credit.compilation,
        };
    }
}

/// The elections of the albums a folder's files belong to: every changed file of an album and
/// its unchanged siblings (their stored votes) elect together (D-164).
struct Election {
    elected: HashMap<String, Option<Candidate>>,
}

/// A stored sibling's vote. A row written before the election has none: its album's stored
/// artist stands in, so an upgraded library neither splits nor merges albums by itself.
fn stored_vote(s: &IndexedTrack) -> Option<Credit> {
    let folder = vote::album_folder(&s.path);
    match &s.credit {
        Some(c) => {
            let key = c.album_key.as_ref()?;
            Some(Credit {
                group: vote::group_of(folder, key),
                album_key: Some(key.clone()),
                candidate: Candidate {
                    name: c.name.clone(),
                    mbid: c.mbid,
                },
                from_album_artist: c.from_album_artist,
                compilation: c.compilation,
            })
        }
        None => Some(Credit {
            group: vote::group_of(folder, &s.album_key),
            album_key: Some(s.album_key.clone()),
            candidate: Candidate {
                name: s.artist_name.clone(),
                mbid: None,
            },
            from_album_artist: false,
            compilation: false,
        }),
    }
}

impl Election {
    fn new(prepared: &[Prepared], siblings: &[&IndexedTrack]) -> Self {
        let mut groups: HashMap<String, Vec<(String, Credit)>> = HashMap::new();
        for p in prepared {
            let c = p.vote();
            groups
                .entry(c.group.clone())
                .or_default()
                .push((p.disk.rel.clone(), c));
        }
        for s in siblings {
            if prepared.iter().any(|p| p.disk.rel == s.path) {
                continue;
            }
            if let Some(c) = stored_vote(s) {
                groups
                    .entry(c.group.clone())
                    .or_default()
                    .push((s.path.clone(), c));
            }
        }
        let elected = groups
            .into_iter()
            .map(|(group, mut votes)| {
                // By path, byte for byte: a tie never depends on the locale.
                votes.sort_by(|a, b| a.0.cmp(&b.0));
                let credits: Vec<Credit> = votes.into_iter().map(|(_, c)| c).collect();
                (group, vote::elect(&credits))
            })
            .collect();
        Election { elected }
    }

    fn apply(&self, p: &mut Prepared) {
        let group = p.vote().group;
        let chosen = self.elected.get(&group).and_then(Option::as_ref);
        p.file_under(chosen);
    }
}

fn group_key(r: &Resolved) -> GroupKey {
    (names::key(&r.album_artist), names::key(&r.album))
}

fn to_track(p: &Prepared, album_year: Option<i32>) -> ResolvedTrack {
    let r = &p.resolved;
    let mut guessed = r.guessed.clone();
    guessed.year_conflict = conflicts(r.year, album_year);
    guessed.via_symlink = p.disk.via_link;
    guessed.credit = Some(p.credit.clone());
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
        ids: TagIds {
            recording: p.raw.recording_mbid,
            release_track: p.raw.release_track_mbid,
            release: p.raw.release_mbid,
            release_group: p.raw.release_group_mbid,
            artists: p.raw.artist_mbids.clone(),
            album_artists: p.raw.album_artist_mbids.clone(),
        },
        isrc: p.raw.isrc.clone(),
        disc_total: p.raw.disc_total.and_then(|n| i32::try_from(n).ok()),
        track_total: p.raw.track_total.and_then(|n| i32::try_from(n).ok()),
        artist_mbid_candidate: p.artist_mbid,
    }
}

/// What storing one file did.
struct StoreResult {
    stored: Stored,
    /// The row already existed (here or re-pointed from elsewhere) and is now on another album,
    /// so its old album may be empty.
    album_moved: bool,
    /// The album the row was on before, if it existed.
    old_album: Option<Uuid>,
    upserted: library::Upserted,
}

/// The track agrees with its album's applied identity (spec §19.7): the same artist and title
/// keys, and its own year equal to the applied one or missing.
///
/// The artist agrees when it is the one the album elected or the one the file names itself: a
/// guest song sitting on an album a person applied stays there (D-164).
fn agrees(id: &library::AppliedIdentity, t: &ResolvedTrack, own_key: &str) -> bool {
    (id.artist_key == t.album_artist_key || id.artist_key == own_key)
        && id.title_key == t.album_key
        && (t.track_year.is_none() || t.track_year == id.year)
}

/// Stores one file in its own transaction: the row with its tag ids, the file's genres, its
/// embedded lyrics (or the removal of an `embedded` row), and the stage reopen. For a path with
/// no row (`repoint`), first tries to re-point a missing track with the same fingerprint; the
/// re-point and the metadata upsert commit or roll back together, so a failed upsert never
/// leaves a half-moved track. A track on an autotagged album that still agrees with the
/// applied identity stays on that row by id, and the row's year is left as applied (spec
/// §19.7). With [`ReadCause::TagWrite`] no stage is reopened and the fingerprint row is kept.
async fn store(
    pool: &PgPool,
    t: &ResolvedTrack,
    raw: &RawTags,
    own_key: &str,
    repoint: Repoint,
    cause: ReadCause,
) -> sqlx::Result<Option<StoreResult>> {
    let mut tx = pool.begin().await?;
    let repointed = match repoint {
        Repoint::No => None,
        Repoint::Any => {
            library::repoint_by_fingerprint_in(&mut tx, &t.fingerprint, &t.path).await?
        }
        Repoint::Exact(id) => match library::repoint_exact_in(&mut tx, id, &t.path).await? {
            Some(id) => Some(id),
            // The file carries this track's override; under no other track may it be stored.
            None => return Ok(None),
        },
    };
    // Locks the track row (if any) before the album upsert locks the album: the recording
    // stage locks track then album, and the same order keeps the two from deadlocking.
    let old_album = library::album_of_path(&mut tx, &t.path).await?;
    let pin = match old_album {
        Some(album) => library::autotag_identity(&mut tx, album)
            .await?
            .filter(|id| agrees(id, t, own_key))
            .map(|id| (album, id)),
        None => None,
    };
    let up = match pin {
        Some((album, id)) => {
            // The applied year stands: the folder mode is not the album's year here.
            let mut t = t.clone();
            t.album_year = id.year;
            t.guessed.year_conflict = conflicts(t.track_year, id.year);
            library::upsert_track_pinned(&mut tx, &t, album).await?
        }
        None => library::upsert_track(&mut tx, t).await?,
    };
    genres::replace_track_genres(&mut tx, up.track, GenreSource::File, &raw.genres).await?;
    match &raw.lyrics {
        Some(l) => {
            lyrics::upsert_embedded(&mut tx, up.track, l.plain.as_deref(), l.synced.as_deref())
                .await?
        }
        None => lyrics::delete_embedded(&mut tx, up.track).await?,
    }
    if up.content_changed && cause == ReadCause::Scan {
        stages::reopen_for_changed_file(&mut tx, up.track).await?;
    } else {
        // A re-point, a missing row re-read at its own path, or kima-rs's own tag write (the
        // audio is proven unchanged): the file is back, nothing else is reopened.
        stages::reopen_file_missing(&mut tx, up.track).await?;
    }
    tx.commit().await?;
    let stored = match (repointed, old_album) {
        (Some(id), _) => Stored::Repointed(id),
        (None, Some(_)) => Stored::Updated,
        (None, None) => Stored::Added,
    };
    Ok(Some(StoreResult {
        stored,
        album_moved: old_album.is_some() && old_album != Some(up.album),
        old_album,
        upserted: up,
    }))
}

/// The most common value, ties to the smallest.
fn mode_uuid(values: &[Uuid]) -> Option<Uuid> {
    let mut counts: BTreeMap<Uuid, usize> = BTreeMap::new();
    for v in values {
        *counts.entry(*v).or_default() += 1;
    }
    let mut best: Option<(Uuid, usize)> = None;
    for (v, n) in counts {
        if best.is_none_or(|(_, b)| n > b) {
            best = Some((v, n));
        }
    }
    best.map(|(v, _)| v)
}

/// Albums and artists a folder's stores touched, for the per-folder refresh.
#[derive(Default)]
struct Touched {
    albums: HashSet<Uuid>,
    artists: HashSet<Uuid>,
    /// Artist MBID candidates read per artist.
    candidates: HashMap<Uuid, Vec<Uuid>>,
    /// Artists of the albums the folder's stores created.
    new_album_artists: HashSet<Uuid>,
}

impl Touched {
    fn add(&mut self, r: &StoreResult, t: &ResolvedTrack) {
        self.albums.insert(r.upserted.album);
        self.albums.extend(r.old_album);
        self.artists.insert(r.upserted.artist);
        if r.upserted.album_created {
            self.new_album_artists.insert(r.upserted.artist);
        }
        if let Some(c) = t.artist_mbid_candidate {
            self.candidates
                .entry(r.upserted.artist)
                .or_default()
                .push(c);
        }
    }
}

/// One artist's share of the folder refresh, in the order the artists are locked.
#[derive(Debug, PartialEq, Eq)]
struct ArtistStep {
    artist: Uuid,
    /// The tag MBID to set while the artist has none.
    mbid: Option<Uuid>,
    /// The artist gets a new album: reopen it if `completed`.
    reopen: bool,
}

/// The artists that need an MBID or a reopen, one step each, sorted by id: a single pass
/// takes their row locks in one order (final review M2), however the two lists overlap.
fn artist_steps(
    candidates: &HashMap<Uuid, Vec<Uuid>>,
    new_album_artists: &HashSet<Uuid>,
) -> Vec<ArtistStep> {
    let mut ids: Vec<Uuid> = candidates
        .keys()
        .chain(new_album_artists)
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    ids.sort_unstable();
    ids.into_iter()
        .map(|artist| ArtistStep {
            artist,
            mbid: candidates.get(&artist).and_then(|c| mode_uuid(c)),
            reopen: new_album_artists.contains(&artist),
        })
        .collect()
}

/// After a folder: album MBIDs (mode of the tags, R2), artist MBIDs from tags where unset, and
/// the file-derived album and artist genres, for everything the folder touched. One
/// transaction, run again once if the database picked it as a deadlock victim.
async fn refresh_touched(pool: &PgPool, touched: Touched) -> sqlx::Result<()> {
    if touched.albums.is_empty() && touched.candidates.is_empty() {
        return Ok(());
    }
    retry_once_on_conflict(|| refresh_touched_once(pool, &touched)).await
}

async fn refresh_touched_once(pool: &PgPool, touched: &Touched) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let mut albums: Vec<Uuid> = touched.albums.iter().copied().collect();
    albums.sort_unstable();
    for &album in &albums {
        library::refresh_album_mbids(&mut tx, album).await?;
    }
    // Rows already locked above; an autotagged album no present track is on any more goes
    // back to `scan` (spec §19.7).
    library::release_autotag_albums(&mut tx, &albums).await?;
    // The artists the folder gives an MBID or a new album, locked in one id-ordered pass. A
    // new album by a `completed` artist puts the artist back to `pending`, so the album gets
    // its cover fallback (final review I1; the cached answers make the re-run cheap).
    for step in artist_steps(&touched.candidates, &touched.new_album_artists) {
        if let Some(mbid) = step.mbid {
            library::set_artist_mbid_if_unset(&mut tx, step.artist, mbid).await?;
        }
        if step.reopen {
            library::reopen_completed_artists(&mut tx, &[step.artist]).await?;
        }
    }
    // An old album's artist loses the moved track's genres too.
    let mut artists = touched.artists.clone();
    artists.extend(library::artists_of_albums(&mut tx, &albums).await?);
    let mut artists: Vec<Uuid> = artists.into_iter().collect();
    artists.sort_unstable();
    genres::recompute_file_genres(&mut tx, &albums, &artists).await?;
    tx.commit().await
}

/// Re-derives what albums take from their present tracks: the tag MBIDs (R2) and the file
/// genres of the albums and their artists. Run after tracks go missing, come back or are purged
/// (an album with no present track keeps none). Locks in the folder refresh's order (albums,
/// then genres, ids sorted) and, like it, runs again once after a deadlock.
async fn refresh_albums(pool: &PgPool, albums: HashSet<Uuid>) -> sqlx::Result<()> {
    if albums.is_empty() {
        return Ok(());
    }
    let mut albums: Vec<Uuid> = albums.into_iter().collect();
    albums.sort_unstable();
    retry_once_on_conflict(|| async {
        let mut tx = pool.begin().await?;
        refresh_albums_in(&mut tx, &albums).await?;
        tx.commit().await
    })
    .await
}

/// The body of [`refresh_albums`] on the caller's transaction; `albums` sorted.
async fn refresh_albums_in(conn: &mut PgConnection, albums: &[Uuid]) -> sqlx::Result<()> {
    for &album in albums {
        library::refresh_album_mbids(conn, album).await?;
    }
    library::release_autotag_albums(conn, albums).await?;
    let mut artists = library::artists_of_albums(conn, albums).await?;
    artists.sort_unstable();
    genres::recompute_file_genres(conn, albums, &artists).await
}

/// Marks these paths missing (`restore` false) or brings them back (`restore` true) and
/// refreshes their albums, in one transaction, run again once after a deadlock: a victim never
/// leaves the flag changed and the album's MBIDs and genres stale. Returns the rows changed.
async fn flip_and_refresh(pool: &PgPool, paths: &[String], restore: bool) -> sqlx::Result<u64> {
    retry_once_on_conflict(|| async {
        let mut tx = pool.begin().await?;
        let changed = if restore {
            library::clear_missing_in(&mut tx, paths).await?
        } else {
            library::mark_missing_in(&mut tx, paths).await?
        };
        let mut albums = library::albums_of_paths_in(&mut tx, paths).await?;
        albums.sort_unstable();
        albums.dedup();
        refresh_albums_in(&mut tx, &albums).await?;
        tx.commit().await?;
        Ok(changed)
    })
    .await
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

/// The stored overrides of these paths, parsed. A value that does not parse is an error for
/// that path: reading the file without it could move a database-only match off its album.
async fn overrides_of(
    pool: &PgPool,
    paths: Vec<String>,
) -> sqlx::Result<HashMap<String, Result<TagOverrides, String>>> {
    Ok(kima_db::autotag::overrides_for_paths(pool, &paths)
        .await?
        .into_iter()
        .map(|(path, v)| {
            let o = serde_json::from_value::<TagOverrides>(v)
                .map_err(|e| format!("the stored tag override is unreadable: {e}"));
            (path, o)
        })
        .collect())
}

/// [`prepare`] with the path's override from `overrides`.
async fn prepare_with(
    root: &Path,
    disk: DiskFile,
    overrides: &HashMap<String, Result<TagOverrides, String>>,
) -> Result<Prepared, String> {
    match overrides.get(&disk.rel) {
        Some(Err(e)) => Err(e.clone()),
        Some(Ok(o)) => prepare(root, disk, Some(o)).await,
        None => prepare(root, disk, None).await,
    }
}

/// A file with no row at its path will take over a missing track with the same content
/// (`store`'s re-point). If that track has a stored override (a database-only match), the file
/// is resolved with it, so a renamed or moved file keeps the matched identity (spec §19.7). The
/// track is picked by the re-point's own order (earliest missing, then id), skipping tracks
/// earlier kept files of the folder were given; a file that was given a track, with or without
/// an override, is then stored only onto exactly that track ([`Repoint::Exact`]), never another
/// (so it can neither land on a duplicate that carries an override without merging it, nor skip
/// a locked one). A file dropped here (an unreadable override) claims nothing.
async fn merge_moved_overrides(
    pool: &PgPool,
    prepared: Vec<Prepared>,
    known: &HashMap<String, IndexedTrack>,
    overrides: &HashMap<String, Result<TagOverrides, String>>,
    out: &mut FolderResult,
) -> sqlx::Result<Vec<Prepared>> {
    let mut claimed: Vec<Uuid> = Vec::new();
    let mut kept = Vec::with_capacity(prepared.len());
    for mut p in prepared {
        if known.contains_key(&p.disk.rel) || overrides.contains_key(&p.disk.rel) {
            kept.push(p);
            continue;
        }
        let Some((track, fields)) =
            kima_db::autotag::override_by_missing_fingerprint(pool, &p.fingerprint, &claimed)
                .await?
        else {
            kept.push(p);
            continue;
        };
        if let Some(v) = fields {
            match serde_json::from_value::<TagOverrides>(v) {
                Ok(o) => {
                    p.raw.apply_overrides(&o);
                    p.resolved = resolve(&p.raw, Path::new(&p.disk.rel));
                }
                Err(e) => {
                    out.failed.push((
                        p.disk.rel.clone(),
                        format!("the stored tag override is unreadable: {e}"),
                    ));
                    continue;
                }
            }
        }
        p.target = Some(track);
        claimed.push(track);
        kept.push(p);
    }
    Ok(kept)
}

/// Reads and stores the changed files of one folder. `siblings` are the folder's other
/// present rows, unchanged on disk; they count towards the album year, and any whose album
/// year or conflict flag changes is re-read and stored too (unless it sits on an autotagged
/// album whose applied year it agrees with: that year stands, spec §19.7). `known` holds every
/// row at a path of `changed`. `cause` applies to `changed`; a sibling is always a `Scan` read.
async fn process_folder(
    ctx: &LibraryCtx,
    changed: Vec<DiskFile>,
    siblings: &[&IndexedTrack],
    known: &HashMap<String, IndexedTrack>,
    cause: ReadCause,
    refile: bool,
) -> Result<FolderResult, ScanError> {
    let mut out = FolderResult::default();
    let mut touched = Touched::default();
    let overrides = overrides_of(
        &ctx.pool,
        changed
            .iter()
            .map(|d| d.rel.clone())
            .chain(siblings.iter().map(|s| s.path.clone()))
            .collect(),
    )
    .await?;
    let paths: Vec<String> = changed.iter().map(|d| d.rel.clone()).collect();
    let mut prepared = Vec::with_capacity(changed.len());
    for disk in changed {
        out.tags_read += 1;
        let rel = disk.rel.clone();
        match prepare_with(&ctx.root.get(), disk, &overrides).await {
            Ok(p) => prepared.push(p),
            Err(e) => out.failed.push((rel, e)),
        }
    }
    // A re-file is a repair, not a normal incremental scan: never make an election from
    // incomplete votes, or remove a source album/artist carrying a person's edits. The
    // folder is the election boundary (including its disc subfolders), so protect it whole.
    if refile {
        let edited: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM tracks t
             JOIN albums al ON al.id = t.album_id
             JOIN artists ar ON ar.id = al.artist_id
             WHERE t.path = ANY($1) AND (
                 al.identity_source = 'autotag' OR al.display_title IS NOT NULL
                 OR al.display_year IS NOT NULL OR al.user_genres IS NOT NULL
                 OR al.image_source = 'user' OR al.release_mbid_source = 'user'
                 OR al.release_group_mbid_source = 'user'
                 OR ar.display_name IS NOT NULL OR ar.user_bio IS NOT NULL
                 OR ar.user_genres IS NOT NULL OR ar.image_source = 'user'
                 OR ar.mbid_source = 'user'))",
        )
        .bind(&paths)
        .fetch_one(&ctx.pool)
        .await?;
        if edited || !out.failed.is_empty() {
            return Ok(out);
        }
    }
    let mut prepared =
        merge_moved_overrides(&ctx.pool, prepared, known, &overrides, &mut out).await?;
    if refile && !out.failed.is_empty() {
        return Ok(out);
    }
    if refile {
        // A renamed last song has no row at its new path. Its fingerprint target still
        // belongs to the edited source album: restore its path without re-electing or
        // upserting its album identity (which would discard the person's edits).
        let targets: Vec<Uuid> = prepared.iter().filter_map(|p| p.target).collect();
        let edited_targets: Vec<Uuid> = sqlx::query_scalar(
            "SELECT t.id FROM tracks t JOIN albums al ON al.id = t.album_id
             JOIN artists ar ON ar.id = al.artist_id
             WHERE t.id = ANY($1) AND (
                 al.identity_source = 'autotag' OR al.display_title IS NOT NULL
                 OR al.display_year IS NOT NULL OR al.user_genres IS NOT NULL
                 OR al.image_source = 'user' OR al.release_mbid_source = 'user'
                 OR al.release_group_mbid_source = 'user'
                 OR ar.display_name IS NOT NULL OR ar.user_bio IS NOT NULL
                 OR ar.user_genres IS NOT NULL OR ar.image_source = 'user'
                 OR ar.mbid_source = 'user')",
        )
        .bind(&targets)
        .fetch_all(&ctx.pool)
        .await?;
        if !edited_targets.is_empty() {
            for p in &prepared {
                if let Some(id) = p.target.filter(|id| edited_targets.contains(id)) {
                    let mut tx = ctx.pool.begin().await?;
                    if library::repoint_exact_in(&mut tx, id, &p.disk.rel)
                        .await?
                        .is_some()
                    {
                        stages::reopen_file_missing(&mut tx, id).await?;
                        tx.commit().await?;
                        out.stored.push((p.disk.rel.clone(), Stored::Repointed(id)));
                    } else {
                        out.failed.push((
                            p.disk.rel.clone(),
                            "the moved edited track is no longer missing".into(),
                        ));
                    }
                }
            }
            return Ok(out);
        }
    }
    // One artist per album, elected from the changed files and the stored votes of their
    // unchanged siblings (D-164), before the album year groups form.
    let election = Election::new(&prepared, siblings);
    for p in &mut prepared {
        election.apply(p);
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
        let repoint = match p.target {
            Some(id) => Repoint::Exact(id),
            None if known.contains_key(&rel) => Repoint::No,
            None => Repoint::Any,
        };
        match retry_once_on_conflict(|| {
            store(&ctx.pool, &track, &p.raw, &p.own_key, repoint, cause)
        })
        .await
        {
            Ok(None) => out.failed.push((
                rel,
                "the moved track this file was matched to is no longer missing; \
                 it is read again on the next scan"
                    .to_string(),
            )),
            Ok(Some(r)) => {
                touched.add(&r, &track);
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
        // On an autotagged album whose applied identity is the row's own (`album_autotag`),
        // with its own year agreeing: the store would pin it there and keep the applied year,
        // so a re-read would change nothing.
        if s.album_autotag && !s.year_conflict && !conflicts(s.track_year, s.album_year) {
            continue;
        }
        let full = ctx.root.get().join(&s.path);
        // The scanner's own flag, not a tag: a re-read for the album year must keep it.
        let linked = {
            let (root, rel) = (ctx.root.get(), s.path.clone());
            tokio::task::spawn_blocking(move || symlinked(&root, &rel)).await?
        };
        let meta = match tokio::fs::metadata(&full).await {
            Ok(m) => m,
            Err(e) => {
                out.failed.push((s.path.clone(), e.to_string()));
                continue;
            }
        };
        out.tags_read += 1;
        let mut disk = disk_file(s.path.clone(), &meta);
        disk.via_link = linked;
        let mut p = match prepare_with(&ctx.root.get(), disk, &overrides).await {
            Ok(p) => p,
            Err(e) => {
                out.failed.push((s.path.clone(), e));
                continue;
            }
        };
        election.apply(&mut p);
        // Re-read tags may have moved it to another group; use that group's year if known.
        let year = years.get(&group_key(&p.resolved)).copied().unwrap_or(year);
        let track = to_track(&p, year);
        match retry_once_on_conflict(|| {
            store(
                &ctx.pool,
                &track,
                &p.raw,
                &p.own_key,
                Repoint::No,
                ReadCause::Scan,
            )
        })
        .await
        {
            Ok(None) => unreachable!("a store without a re-point always stores"),
            Ok(Some(r)) => {
                touched.add(&r, &track);
                out.album_moved |= r.album_moved;
                out.stored.push((s.path.clone(), Stored::Updated));
            }
            Err(e) if per_file_db_error(&e) => out.failed.push((s.path.clone(), e.to_string())),
            Err(e) => return Err(e.into()),
        }
    }
    refresh_touched(&ctx.pool, touched).await?;
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
    reconcile_with(ctx, cancel, false).await
}

/// Re-reads every present file through the same album-artist election as a normal scan.
/// The library queue serializes this with other library writers.
pub async fn refile_cancellable(
    ctx: &LibraryCtx,
    cancel: &CancellationToken,
) -> Result<ScanReport, ScanError> {
    reconcile_with(ctx, cancel, true).await
}

async fn reconcile_with(
    ctx: &LibraryCtx,
    cancel: &CancellationToken,
    force: bool,
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
    let index: HashMap<String, IndexedTrack> = library::scan_index(&ctx.pool, None)
        .await?
        .into_iter()
        .map(|t| (t.path.clone(), t))
        .collect();
    let recorded: HashSet<String> = index.keys().cloned().collect();
    let walked = tokio::task::spawn_blocking(move || walk(&root, &recorded)).await??;
    let mut report = ScanReport {
        failed: walked.failed,
        skipped_links: walked.skipped_links,
        ..Default::default()
    };
    if let Some(first) = report.skipped_links.first() {
        warn!(
            skipped = report.skipped_links.len(),
            first = %first,
            "some library links could not be followed"
        );
    }
    let mut progress = Progress::new(&ctx.events, walked.files.len());
    progress.send();

    let on_disk: HashSet<&str> = walked.files.iter().map(|f| f.rel.as_str()).collect();
    let mut restore = Vec::new();
    let mut changed: BTreeMap<String, Vec<DiskFile>> = BTreeMap::new();
    // Rows that are present, on disk and unchanged after this run, per folder.
    let mut settled: HashMap<&str, Vec<&IndexedTrack>> = HashMap::new();
    for f in &walked.files {
        match index.get(&f.rel) {
            Some(row) if !force && row.size == f.size && row.mtime == f.mtime => {
                if row.missing {
                    restore.push(f.rel.clone());
                } else {
                    report.unchanged += 1;
                }
                settled
                    .entry(vote::album_folder(&row.path))
                    .or_default()
                    .push(row);
            }
            _ => changed
                .entry(vote::album_folder(&f.rel).to_string())
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
    let candidates: Vec<String> = index
        .values()
        .filter(|t| !on_disk.contains(t.path.as_str()))
        .filter(|t| !parked.contains(&t.path))
        .filter(|t| !under_any(&t.path, &walked.unreadable))
        .map(|t| t.path.clone())
        .collect();
    // The walk follows links, so a track indexed through one shows up on disk like any other.
    // The flag follows what the walk saw: set where a file is now reached through a link,
    // cleared where a real file took its place.
    let mut link_flag = Vec::new();
    let mut link_clear = Vec::new();
    for f in &walked.files {
        match index.get(&f.rel) {
            Some(row) if f.via_link && !row.via_symlink => link_flag.push(f.rel.clone()),
            Some(row) if !f.via_link && row.via_symlink => link_clear.push(f.rel.clone()),
            _ => {}
        }
    }
    let vanished: Vec<String> = if report.root_empty_guard {
        Vec::new()
    } else {
        candidates
            .into_iter()
            .filter(|p| !index[p].missing)
            .collect()
    };
    // Each chunk's flag change commits with its albums' refresh (MBIDs and genres follow the
    // tracks that are present now).
    for chunk in vanished.chunks(MARK_CHUNK) {
        report.missing += flip_and_refresh(&ctx.pool, chunk, false).await? as usize;
    }
    library::set_via_symlink(&ctx.pool, &link_flag, true).await?;
    library::set_via_symlink(&ctx.pool, &link_clear, false).await?;
    let vanished_ids: HashSet<Uuid> = vanished.iter().map(|p| index[p].id).collect();
    for chunk in restore.chunks(MARK_CHUNK) {
        report.restored += flip_and_refresh(&ctx.pool, chunk, true).await? as usize;
    }
    progress.advance(report.unchanged + restore.len());

    for (folder, files) in changed {
        check()?;
        let n = files.len();
        if force
            && walked.unreadable.iter().any(|dir| {
                dir == &folder
                    || vote::album_folder(dir) == folder
                    || dir.starts_with(&format!("{folder}/"))
            })
        {
            // The walker did not see all the votes in this album (possibly a CD
            // subfolder); leave the whole group untouched until it can be read.
            progress.advance(n);
            continue;
        }
        let siblings = settled.get(folder.as_str()).map_or(&[][..], Vec::as_slice);
        let result = process_folder(ctx, files, siblings, &index, ReadCause::Scan, force).await?;
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
        let cache = ctx.data_dir.join("transcode");
        let retired = library::retire_missing(&ctx.pool, MISSING_GRACE, Some(&cache)).await?;
        if retired.stood_down {
            warn!(
                "a large share of the library is missing at once; this looks like a dropped \
                 mount, not a deletion: nothing retired"
            );
        }
        report.purged = retired.tracks;
        if retired.tracks > 0 {
            info!(
                tracks = retired.tracks,
                handed_over = retired.handed_over,
                "retired tracks missing past the grace period"
            );
            for path in &retired.artifacts {
                if let Err(e) = tokio::fs::remove_file(path).await
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    warn!(path = %path.display(), error = %e, "could not unlink a transcode of a retired track");
                }
            }
            kima_db::genres::prune_unused(&ctx.pool).await?;
        }
        // Upstream `playlistsHoldingTracks` (`bafecf23`): the owner of each playlist that lost a
        // place is told once, after the commit.
        for (owner, playlist) in &retired.playlists {
            ctx.events.publish(Event {
                user_id: Some(*owner),
                data: serde_json::json!({
                    "type": "playlist:updated", "playlistId": playlist, "deleted": false,
                }),
            });
        }
        refresh_albums(&ctx.pool, retired.albums.into_iter().collect()).await?;
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

/// `root.join(rel)` when `rel` is made only of `Component::Normal` parts (the M1 rule), so the
/// result cannot leave `root`; `None` for an empty, absolute or `..`-bearing path.
pub fn library_path(root: &Path, rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    let normal = !rel.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)));
    normal.then(|| root.join(p))
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

/// What `root/rel` is, following links.
enum Entry {
    File(DiskFile),
    /// Not there: nothing at the path, or a link that dangles or loops.
    Gone,
    NotAFile,
    Error(String),
}

/// The file at `rel`, with the size and time of what a link leads to and whether it is reached
/// through a link. Any error but "not there" is not "gone" (D-123, upstream `90a9c4a1`).
async fn disk_entry(root: &Path, rel: &str) -> Result<Entry, ScanError> {
    let (r, p) = (root.to_path_buf(), rel.to_string());
    let via_link = tokio::task::spawn_blocking(move || symlinked(&r, &p)).await?;
    Ok(match tokio::fs::metadata(root.join(rel)).await {
        Ok(m) if m.is_file() => {
            let mut d = disk_file(rel.to_string(), &m);
            d.via_link = via_link;
            Entry::File(d)
        }
        Ok(_) => Entry::NotAFile,
        // ELOOP: a link that leads back to itself.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(40) => {
            Entry::Gone
        }
        Err(e) => Entry::Error(e.to_string()),
    })
}

/// The file at `rel` is not there: marks its row missing (unless the root is unavailable).
async fn gone(ctx: &LibraryCtx, rel: &str) -> Result<PathOutcome, ScanError> {
    Ok(match library::find_by_path(&ctx.pool, rel).await? {
        None => PathOutcome::Ignored,
        Some(row) if row.missing_since.is_some() => PathOutcome::Unchanged,
        Some(_) if !root_available(&ctx.root.get()).await => PathOutcome::RootUnavailable,
        Some(_) => {
            flip_and_refresh(&ctx.pool, &[rel.to_string()], false).await?;
            PathOutcome::MarkedMissing
        }
    })
}

/// A new link (no row yet): another present row already stands for the file it leads to.
/// Candidates are the rows with the same size and time (a link reports its target's), compared
/// by real path.
async fn twin_indexed(ctx: &LibraryCtx, root: &Path, disk: &DiskFile) -> Result<bool, ScanError> {
    let candidates = library::present_paths_with_stat(&ctx.pool, disk.size, disk.mtime).await?;
    let (root, rel) = (root.to_path_buf(), disk.rel.clone());
    Ok(tokio::task::spawn_blocking(move || {
        let Ok(mine) = std::fs::canonicalize(root.join(&rel)) else {
            return false;
        };
        candidates
            .iter()
            .filter(|c| **c != rel)
            .any(|c| std::fs::canonicalize(root.join(c)).is_ok_and(|real| real == mine))
    })
    .await?)
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
    let root = ctx.root.get();
    let disk = match disk_entry(&root, &rel).await? {
        Entry::File(d) => d,
        Entry::Gone => return gone(ctx, &rel).await,
        Entry::NotAFile => return Ok(PathOutcome::Ignored),
        Entry::Error(e) => return Ok(PathOutcome::Failed(e)),
    };
    let album_dir = vote::album_folder(&rel);
    let folder: HashMap<String, IndexedTrack> =
        library::scan_index_album_folder(&ctx.pool, &dir_prefix(album_dir))
            .await?
            .into_iter()
            .filter(|t| vote::album_folder(&t.path) == album_dir)
            .map(|t| (t.path.clone(), t))
            .collect();
    let mut known = HashMap::new();
    if let Some(row) = folder.get(&rel) {
        if row.size == disk.size && row.mtime == disk.mtime {
            if row.via_symlink != disk.via_link {
                library::set_via_symlink(&ctx.pool, std::slice::from_ref(&rel), disk.via_link)
                    .await?;
            }
            if row.missing {
                flip_and_refresh(&ctx.pool, std::slice::from_ref(&rel), true).await?;
                return Ok(PathOutcome::Updated);
            }
            return Ok(PathOutcome::Unchanged);
        }
        known.insert(rel.clone(), row.clone());
    } else if disk.via_link && twin_indexed(ctx, &root, &disk).await? {
        // One real file is one song: a new link to a file already indexed under another path
        // does not take it (the walk makes the same choice).
        return Ok(PathOutcome::Ignored);
    }
    let siblings: Vec<&IndexedTrack> = folder
        .values()
        .filter(|t| t.path != rel && !t.missing)
        .collect();

    let result = process_folder(ctx, vec![disk], &siblings, &known, ReadCause::Scan, false).await?;
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

/// Re-reads exactly these files (paths relative to the library root), whatever their size and
/// mtime, grouped per folder with the folder's other present rows as siblings (as
/// [`update_path`]). A path that is gone is marked missing, one reached through a symlink is
/// handled as [`update_path`] does (never indexed anew), and an invalid one is reported in
/// `failed`. With [`ReadCause::TagWrite`] (the files kima-rs just tagged, audio proven
/// unchanged) no stage is reopened and no fingerprint row dropped (spec §19.7).
///
/// Must run on the concurrency-1 `library` queue, like [`update_path`] and every other library
/// writer: it may call `prune_empty`, which must not race upserts (an autotag apply or undo
/// enqueues its re-read there, or runs it inside its own `library` job).
pub async fn reread_paths(
    ctx: &LibraryCtx,
    paths: &[String],
    cause: ReadCause,
) -> Result<ScanReport, ScanError> {
    let mut report = ScanReport::default();
    let mut folders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in paths {
        match clean_rel(p).filter(|r| tags::is_audio(Path::new(r))) {
            Some(rel) => {
                let list = folders
                    .entry(vote::album_folder(&rel).to_string())
                    .or_default();
                if !list.contains(&rel) {
                    list.push(rel);
                }
            }
            None => report.failed.push((
                p.clone(),
                "not an audio file under the library root".to_string(),
            )),
        }
    }
    let root = ctx.root.get();
    let mut album_moved = false;
    for (folder, rels) in folders {
        let index: HashMap<String, IndexedTrack> =
            library::scan_index_album_folder(&ctx.pool, &dir_prefix(&folder))
                .await?
                .into_iter()
                .filter(|t| vote::album_folder(&t.path) == folder)
                .map(|t| (t.path.clone(), t))
                .collect();
        let mut changed = Vec::new();
        let mut known = HashMap::new();
        for rel in &rels {
            let outcome = match disk_entry(&root, rel).await? {
                Entry::File(disk) => {
                    changed.push(disk);
                    if let Some(row) = index.get(rel) {
                        known.insert(rel.clone(), row.clone());
                    }
                    None
                }
                Entry::NotAFile => Some(PathOutcome::Failed("not a regular file".to_string())),
                Entry::Gone => Some(gone(ctx, rel).await?),
                Entry::Error(e) => Some(PathOutcome::Failed(e)),
            };
            match outcome {
                None => {}
                Some(PathOutcome::MarkedMissing) => report.missing += 1,
                Some(PathOutcome::Updated) => report.restored += 1,
                Some(PathOutcome::Unchanged | PathOutcome::Ignored) => report.unchanged += 1,
                Some(PathOutcome::RootUnavailable) => report
                    .failed
                    .push((rel.clone(), "the library root is unavailable".to_string())),
                Some(PathOutcome::Failed(e)) => report.failed.push((rel.clone(), e)),
                Some(PathOutcome::Added | PathOutcome::Repointed) => report.updated += 1,
            }
        }
        if changed.is_empty() {
            continue;
        }
        let siblings: Vec<&IndexedTrack> = index
            .values()
            .filter(|t| !t.missing && !rels.contains(&t.path))
            .collect();
        let result = process_folder(ctx, changed, &siblings, &known, cause, false).await?;
        report.tags_read += result.tags_read;
        report.failed.extend(result.failed);
        album_moved |= result.album_moved;
        for (_, stored) in result.stored {
            match stored {
                Stored::Added => report.added += 1,
                Stored::Updated => report.updated += 1,
                Stored::Repointed(_) => report.repointed += 1,
            }
        }
    }
    if album_moved {
        prune_empty(ctx).await?;
    }
    Ok(report)
}

/// Prunes empty albums and artists, then (after the commit) deletes the cover cache of each
/// deleted album and the stored image of each deleted artist. A file that cannot be removed is
/// logged, not fatal: the rows are already gone.
async fn prune_empty(ctx: &LibraryCtx) -> Result<(), ScanError> {
    let pruned = library::prune_empty(&ctx.pool).await?;
    if pruned.albums.is_empty() && pruned.artists.is_empty() {
        return Ok(());
    }
    let data_dir = ctx.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        for id in pruned.albums {
            if let Err(e) = crate::artwork::remove_album(&data_dir, id) {
                warn!(album = %id, error = %e, "could not remove the cover cache of a deleted album");
            }
            if let Err(e) = crate::artwork::remove_album_image(&data_dir, id) {
                warn!(album = %id, error = %e, "could not remove the stored cover of a deleted album");
            }
        }
        for id in pruned.artists {
            if let Err(e) = crate::artwork::remove_artist_image(&data_dir, id) {
                warn!(artist = %id, error = %e, "could not remove the image of a deleted artist");
            }
            // The artist image's resized variants share the cover cache's layout.
            if let Err(e) = crate::artwork::remove_album(&data_dir, id) {
                warn!(artist = %id, error = %e, "could not remove the image cache of a deleted artist");
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
    fn artist_steps_are_one_sorted_pass_over_the_union() {
        let id = |n: u128| Uuid::from_u128(n);
        let candidates = HashMap::from([
            (id(9), vec![id(100), id(100), id(101)]),
            (id(2), vec![id(100)]),
        ]);
        // 5 only reopens, 9 does both, 2 only gets an MBID; the input order is scrambled.
        let new_album_artists = HashSet::from([id(9), id(5)]);
        assert_eq!(
            artist_steps(&candidates, &new_album_artists),
            vec![
                ArtistStep {
                    artist: id(2),
                    mbid: Some(id(100)),
                    reopen: false
                },
                ArtistStep {
                    artist: id(5),
                    mbid: None,
                    reopen: true
                },
                ArtistStep {
                    artist: id(9),
                    mbid: Some(id(100)),
                    reopen: true
                },
            ]
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
    fn library_path_stays_under_root() {
        let root = Path::new("/music");
        assert_eq!(
            library_path(root, "A/x.flac"),
            Some(PathBuf::from("/music/A/x.flac"))
        );
        assert_eq!(library_path(root, "../x.flac"), None);
        assert_eq!(library_path(root, "A/../../x.flac"), None);
        assert_eq!(library_path(root, "/etc/passwd"), None);
        assert_eq!(library_path(root, "./x.flac"), None);
        assert_eq!(library_path(root, ""), None);
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
        assert_eq!(dir_prefix(""), "");
        assert_eq!(dir_prefix("A/B"), "A/B/");
    }
}
