//! Artist enrichment (spec §10.5, §12.1-§12.5; upstream `artistEnrichment.ts:74-387`): the
//! MBID (the guard, then name resolution), the bio, the image, MusicBrainz genres and similar
//! artists, then one write of the outcome.
//!
//! Every answer is untrusted. A source that fails keeps what was stored (it never records
//! "none") and, for MusicBrainz, Wikidata and the similar-artist sources, fails the run: the
//! artist is `failed`, and `unresolvable` at the [`FAILURE_CAP`]-th failure in a row; a source
//! that only waits on its rate limit leaves the artist `pending` without counting a failure.
//! Image sources never fail the run. Images are decoded and re-encoded, staged in a temp file,
//! and put in place at `<data_dir>/artist-images/<id>.jpg` only inside the transaction that
//! records them, after the row's precedence check accepted them: a `user` image is never
//! touched. After the artist's own image, its albums' missing covers are filled
//! ([`crate::album_covers`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kima_db::identity::{
    self, ArtistForEnrichment, ArtistStatus, ArtistWrite, Change, release_artists,
};
use kima_db::stages::cut_chars;
use kima_library::artwork;
use kima_net::{Failure, Net, Outcome};
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::musicbrainz::{self, ArtistInc};
use crate::stages::{BatchReport, MAX_ERROR_CHARS, RETRY_CAP};
use crate::{IdentCtx, Mbid, album_covers, deezer, fanart, listenbrainz, similar, text, wikidata};

/// Artists claimed per batch (spec §10.6).
pub const BATCH: i64 = 10;
/// Namesakes whose catalogue is read before giving up (`MAX_CATALOGUE_CHECKS`).
pub const MAX_CATALOGUE_CHECKS: usize = 3;
/// Failures in a row after which an artist is `unresolvable` (`STAGE_RETRY_CAP`).
pub const FAILURE_CAP: i16 = RETRY_CAP;
/// MusicBrainz genres kept per artist (spec §12.4).
pub const MAX_GENRES: usize = 5;
/// The most one artist's enrichment may take, every request and download included (the
/// production value of `IdentConfig::artist_timeout`). Past it before the artist's own write,
/// the run fails with `TIMEOUT` and nothing it learned is written; the album covers that
/// follow the write only get what is left.
pub const ARTIST_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How one artist's enrichment ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtistResult {
    Completed,
    /// Only waiting on a rate limit: asked again without counting a failure.
    Pending,
    Failed {
        code: String,
        message: String,
    },
    /// MusicBrainz has no such artist, or the failure cap was reached.
    Unresolvable,
}

/// The status and failure count after one more failure (upstream `failedArtistStatus`).
pub fn failed_status(failures_so_far: i16) -> (ArtistStatus, i16) {
    let failures = failures_so_far.max(0).saturating_add(1);
    let status = if failures >= FAILURE_CAP {
        ArtistStatus::Unresolvable
    } else {
        ArtistStatus::Failed
    };
    (status, failures)
}

/// A non-answer as another type's outcome. Only called on a non-`Answered` outcome.
fn retype<T, U>(o: Outcome<T>) -> Outcome<U> {
    match o {
        Outcome::Answered(_) => unreachable!("retype is never given an answer"),
        Outcome::Absent(e) => Outcome::Absent(e),
        Outcome::RateLimited { until } => Outcome::RateLimited { until },
        Outcome::Transient(f) => Outcome::Transient(f),
        Outcome::Permanent(f) => Outcome::Permanent(f),
    }
}

fn absent<T>(evidence: serde_json::Value) -> Outcome<T> {
    Outcome::Absent(kima_net::Evidence(evidence))
}

/// An artist name as a MusicBrainz id (spec §12.1; `resolveArtistMbid`,
/// `artistCorrection.ts:152-203`). The name, without collaboration and edition noise, is
/// searched; a candidate counts when its name or an alias equals it under
/// [`text::normalize_artist_name`]. Without usable album titles (generic ones dropped) only a
/// single candidate is taken; with titles, the first [`MAX_CATALOGUE_CHECKS`] candidates'
/// catalogues are read and the first holding one of the titles wins. Failures pass through.
pub async fn resolve_mbid(net: &Net, name: &str, album_titles: &[String]) -> Outcome<Mbid> {
    let query = text::strip_artist_suffixes(name);
    let wanted = text::normalize_artist_name(&query);
    if wanted.is_empty() {
        return absent(json!({"artistName": name, "reason": "nothing to search by"}));
    }
    let hits = match musicbrainz::search_artists(net, &text::escape_lucene(&query)).await {
        Outcome::Answered(h) => h,
        other => return retype(other),
    };
    let named: Vec<&musicbrainz::Artist> = hits
        .iter()
        .filter(|c| {
            std::iter::once(&c.name)
                .chain(c.aliases.iter().map(|a| &a.name))
                .any(|n| text::normalize_artist_name(n) == wanted)
        })
        .collect();
    if named.is_empty() {
        return absent(json!({"artistName": name, "hits": hits.len()}));
    }
    let titles: Vec<&String> = album_titles
        .iter()
        .filter(|t| !text::is_generic_album_title(t))
        .collect();
    if titles.is_empty() {
        if named.len() > 1 {
            return absent(json!({"artistName": name, "ambiguous": named.len()}));
        }
        return Outcome::Answered(named[0].id);
    }
    for candidate in named.iter().take(MAX_CATALOGUE_CHECKS) {
        let groups = match musicbrainz::all_artist_release_groups(net, candidate.id).await {
            Outcome::Answered(g) => g,
            Outcome::Absent(_) => continue,
            other => return retype(other),
        };
        let holds = groups
            .iter()
            .any(|g| titles.iter().any(|t| text::titles_agree(&g.title, t)));
        if holds {
            return Outcome::Answered(candidate.id);
        }
    }
    absent(json!({"artistName": name, "candidates": named.len()}))
}

/// Whether `mbid` names this artist (the guard, `dropMbidOfAnotherArtist`): MusicBrainz's name,
/// sort name or an alias's name or sort name agrees with one of `names` under
/// [`text::artists_agree`]. An id MusicBrainz does not know is `Answered(false)`; failures pass
/// through.
pub async fn mbid_belongs(net: &Net, mbid: Mbid, names: &[String]) -> Outcome<bool> {
    let held = match musicbrainz::artist(net, mbid, ArtistInc::Aliases).await {
        Outcome::Answered(a) => a,
        Outcome::Absent(_) => return Outcome::Answered(false),
        other => return retype(other),
    };
    let offered: Vec<String> = [held.name.clone(), held.sort_name.clone()]
        .into_iter()
        .chain(
            held.aliases
                .iter()
                .flat_map(|a| [a.name.clone(), a.sort_name.clone()]),
        )
        .filter(|n| !n.is_empty())
        .collect();
    Outcome::Answered(text::artists_agree(names, &offered))
}

/// The sources' verdicts so far: the first failure, and whether any source waits on its rate
/// limit.
#[derive(Default)]
struct Steps {
    failure: Option<Failure>,
    waiting: bool,
}

impl Steps {
    /// Notes a non-answer that fails the run (or makes it wait). `Answered` and `Absent` note
    /// nothing.
    fn note<T>(&mut self, what: &str, o: Outcome<T>) {
        match o {
            Outcome::Answered(_) | Outcome::Absent(_) => {}
            Outcome::RateLimited { .. } => self.waiting = true,
            Outcome::Transient(f) | Outcome::Permanent(f) => {
                debug!(step = what, code = %f.code, "artist enrichment step failed");
                self.failure.get_or_insert(f);
            }
        }
    }
}

/// Image precedence (spec §12.3): `user` > `local` > `wikidata` > `fanart` > `deezer`.
fn image_rank(source: Option<&str>) -> u8 {
    match source {
        Some("user") => 5,
        Some("local") => 4,
        Some("wikidata") => 3,
        Some("fanart") => 2,
        Some("deezer") => 1,
        _ => 0,
    }
}

/// An image a run found, staged next to its place but not in it yet.
struct NewImage {
    file: artwork::StagedImage,
    /// For a `local` image: the library file it was copied from, as it was.
    local: Option<artwork::SourceStamp>,
}

/// What a run learned, before it is written.
struct Gathered {
    write: ArtistWrite,
    steps: Steps,
    /// The run is over: MusicBrainz has no such artist.
    unresolvable: bool,
    /// The image `write.image` sets, put in place only if the write accepts it.
    image: Option<NewImage>,
    /// The run went through every step: the album covers are filled after the write.
    fill_covers: bool,
}

/// Normalises `bytes` and stages them as the artist's image, off the async threads.
async fn stage_image(ctx: &IdentCtx, bytes: Vec<u8>) -> Result<artwork::StagedImage, String> {
    let data_dir = ctx.library.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        let jpeg = artwork::normalize_image(&bytes).map_err(|e| e.to_string())?;
        artwork::stage_artist_image(&data_dir, &jpeg).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What the library holds for the artist (spec §12.3 step 2).
enum Local {
    /// No usable image.
    None,
    /// The image already stored was copied from this very file, unchanged since.
    Current,
    Found(NewImage),
}

/// Spec §12.3 step 2: the artist's image in the library, staged when found. When the stored
/// `local` image was copied from the same file, unchanged since (its stamp), nothing is read
/// or written again.
async fn local_image(ctx: &IdentCtx, a: &ArtistForEnrichment) -> Local {
    let root = ctx.library.root.get();
    let data_dir = ctx.library.data_dir.clone();
    let stored_local = a.image_source.as_deref() == Some("local");
    let id = a.id;
    let dirs: Vec<PathBuf> = a
        .album_dirs
        .iter()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect();
    let found = tokio::task::spawn_blocking(move || {
        let path = artwork::find_local_artist_image(&root, &dirs)?;
        // `find_local_artist_image` answers under the canonical root.
        let root = std::fs::canonicalize(&root).ok()?;
        if stored_local && artwork::local_artist_image_is_current(&data_dir, &root, id, &path) {
            return Some(Err(()));
        }
        // Stamped before reading, so a change during the copy shows up next time.
        let stamp = artwork::local_stamp(&root, &path).ok()?;
        let bytes = artwork::read_local_image(&path).ok()?;
        Some(Ok((bytes, stamp)))
    })
    .await
    .ok()
    .flatten();
    let (bytes, stamp) = match found {
        None => return Local::None,
        Some(Err(())) => return Local::Current,
        Some(Ok(found)) => found,
    };
    match stage_image(ctx, bytes).await {
        Ok(file) => Local::Found(NewImage {
            file,
            local: Some(stamp),
        }),
        Err(error) => {
            warn!(artist = %a.id, %error, "the library's artist image could not be used");
            Local::None
        }
    }
}

/// Downloads `url` and stages it as the artist's image. A failure (a download refused or
/// failed, an image that does not decode) is logged and the next source is tried.
async fn download_image(ctx: &IdentCtx, artist: Uuid, source: &str, url: &str) -> Option<NewImage> {
    let bytes = match ctx.net.get_bytes(url).await {
        Outcome::Answered(b) => b,
        other => {
            // The message names the host only (kima-net), never the URL's path or query.
            if let Some(code) = other.code() {
                debug!(%artist, source, code, "artist image download failed");
            }
            return None;
        }
    };
    match stage_image(ctx, bytes).await {
        Ok(file) => Some(NewImage { file, local: None }),
        Err(error) => {
            debug!(%artist, source, %error, "artist image did not decode");
            None
        }
    }
}

/// Spec §12.1-§12.5 in upstream's order. Writes nothing (the image is only staged).
async fn gather(ctx: &IdentCtx, a: &ArtistForEnrichment) -> Gathered {
    let net = &ctx.net;
    let mut steps = Steps::default();
    let mut write = ArtistWrite::status(ArtistStatus::Completed, 0);
    let mut image_source = a.image_source.clone();
    let mut mbid = a.mbid.map(Mbid);
    let names = [a.name.clone()];
    let trusted = matches!(a.mbid_source.as_deref(), Some("user" | "autotag"));
    let done = |write, steps, unresolvable| Gathered {
        write,
        steps,
        unresolvable,
        image: None,
        fill_covers: false,
    };

    // §12.1 the guard: an id of another artist goes with everything learned through it.
    if let (Some(id), false) = (mbid, trusted) {
        match mbid_belongs(net, id, &names).await {
            Outcome::Answered(true) => {}
            Outcome::Answered(false) => {
                warn!(artist = %a.id, mbid = %id, "the stored MusicBrainz id names another artist: dropped");
                mbid = None;
                write.mbid = Change::Clear;
                write.bio = Change::Clear;
                write.similar = Change::Clear;
                write.mb_genres = Change::Clear;
                if !matches!(image_source.as_deref(), Some("local" | "user")) {
                    if image_source.is_some() {
                        write.image = Change::Clear;
                    }
                    image_source = None;
                }
            }
            other => {
                steps.note("guard", other);
                return done(write, steps, false);
            }
        }
    }

    // §12.1 resolve by name.
    if mbid.is_none() {
        match resolve_mbid(net, &a.name, &a.album_titles).await {
            Outcome::Answered(id) => {
                mbid = Some(id);
                write.mbid = Change::Set((id.0, "search".to_owned()));
            }
            Outcome::Absent(_) => return done(write, steps, true),
            other => steps.note("musicbrainz", other),
        }
    }

    // §12.2 bio and the Wikidata image.
    let mut wikidata_image = None;
    if let Some(id) = mbid {
        match wikidata::lookup(net, id).await {
            Outcome::Answered(info) => {
                write.bio = info.bio.map_or(Change::Clear, Change::Set);
                wikidata_image = info.image_url;
            }
            other => steps.note("wikidata", other),
        }
    }

    // §12.4 genres: the top five, only when MusicBrainz has some.
    if let Some(id) = mbid {
        match musicbrainz::artist(net, id, ArtistInc::Genres).await {
            Outcome::Answered(held) => {
                let genres = musicbrainz::top_genres(&held.genres, MAX_GENRES);
                if !genres.is_empty() {
                    write.mb_genres = Change::Set(genres);
                }
            }
            other => steps.note("musicbrainz", other),
        }
    }

    // §12.5 similar artists: ListenBrainz and Deezer at once. Deezer's verified hit also
    // carries its picture (§12.3 step 5).
    let lb = async {
        match mbid {
            Some(id) => listenbrainz::similar(net, id).await,
            None => Outcome::Answered(Vec::new()),
        }
    };
    let dz = async {
        match deezer::find_artist(net, &a.name).await {
            Outcome::Answered(Some(hit)) => {
                let related = deezer::related(net, hit.id).await;
                (Some(hit), related)
            }
            Outcome::Answered(None) => (None, Outcome::Answered(Vec::new())),
            other => (None, retype(other)),
        }
    };
    let (lb, (deezer_hit, related)) = tokio::join!(lb, dz);
    let mut similar_ok = true;
    let lb = match lb {
        Outcome::Answered(rows) => rows,
        Outcome::Absent(_) => Vec::new(),
        other => {
            similar_ok = false;
            steps.note("listenbrainz", other);
            Vec::new()
        }
    };
    let related = match related {
        Outcome::Answered(names) => names,
        Outcome::Absent(_) => Vec::new(),
        other => {
            similar_ok = false;
            steps.note("deezer", other);
            Vec::new()
        }
    };
    if similar_ok {
        let list = similar::merge(&lb, &related);
        write.similar = Change::Set(serde_json::to_value(list).unwrap_or_else(|_| json!([])));
    }

    // §12.3 the image: user, local, Wikidata, fanart, Deezer; only a higher source replaces.
    let mut image = None;
    let image_name = format!("{}.jpg", a.id.hyphenated());
    let current = image_rank(image_source.as_deref());
    if current < image_rank(Some("user")) {
        let mut found = None;
        match local_image(ctx, a).await {
            Local::Found(new) => found = Some((new, "local")),
            // Already stored: nothing lower may replace it.
            Local::Current => {}
            Local::None if current < image_rank(Some("local")) => {
                if let Some(url) = wikidata_image.filter(|_| current < image_rank(Some("wikidata")))
                {
                    found = download_image(ctx, a.id, "wikidata", &url)
                        .await
                        .map(|n| (n, "wikidata"));
                }
                if found.is_none() && current < image_rank(Some("fanart")) {
                    if let (Some(id), Some(key)) = (mbid, ctx.fanart_key().await) {
                        if let Outcome::Answered(Some(url)) =
                            fanart::artist_image(net, &key, id).await
                        {
                            found = download_image(ctx, a.id, "fanart", &url)
                                .await
                                .map(|n| (n, "fanart"));
                        }
                    }
                }
                if found.is_none() && current < image_rank(Some("deezer")) {
                    if let Some(url) = deezer_hit.as_ref().and_then(|h| h.picture.as_deref()) {
                        found = download_image(ctx, a.id, "deezer", url)
                            .await
                            .map(|n| (n, "deezer"));
                    }
                }
            }
            Local::None => {}
        }
        if let Some((new, source)) = found {
            write.image = Change::Set((image_name, source.to_owned()));
            image = Some(new);
        }
    }

    Gathered {
        write,
        steps,
        unresolvable: false,
        image,
        fill_covers: true,
    }
}

/// Enriches one claimed artist and writes the outcome (spec §12.1-§12.5, §10.5). A database
/// error is returned; everything else ends in the [`ArtistResult`] written to the row.
pub async fn enrich_one(ctx: &IdentCtx, a: &ArtistForEnrichment) -> sqlx::Result<ArtistResult> {
    let cap = ctx.config.artist_timeout;
    let deadline = tokio::time::Instant::now() + cap;
    let g = match tokio::time::timeout_at(deadline, gather(ctx, a)).await {
        Ok(g) => g,
        Err(_) => Gathered {
            write: ArtistWrite::status(ArtistStatus::Completed, 0),
            steps: Steps {
                failure: Some(Failure::new(
                    "TIMEOUT",
                    format!("artist enrichment took over {cap:?}"),
                )),
                waiting: false,
            },
            unresolvable: false,
            // Whatever was staged was dropped with the run.
            image: None,
            fill_covers: false,
        },
    };
    let Gathered {
        mut write,
        steps,
        unresolvable,
        image,
        fill_covers,
    } = g;
    let result = if unresolvable {
        write.status = ArtistStatus::Unresolvable;
        write.failures = a.enrichment_failures;
        ArtistResult::Unresolvable
    } else if let Some(f) = steps.failure {
        let (status, failures) = failed_status(a.enrichment_failures);
        let message = cut_chars(&f.message, MAX_ERROR_CHARS).to_owned();
        write.status = status;
        write.failures = failures;
        write.error = Change::Set((f.code.clone(), message.clone()));
        if status == ArtistStatus::Unresolvable {
            ArtistResult::Unresolvable
        } else {
            ArtistResult::Failed {
                code: f.code,
                message,
            }
        }
    } else if steps.waiting {
        write.status = ArtistStatus::Pending;
        write.failures = a.enrichment_failures;
        ArtistResult::Pending
    } else {
        write.status = ArtistStatus::Completed;
        write.failures = 0;
        write.error = Change::Clear;
        ArtistResult::Completed
    };
    save(ctx, a.id, &write, image).await?;
    if !fill_covers {
        return Ok(result);
    }
    // §12.7 the album covers, after the artist's own outcome is written, so nothing they do can
    // discard it or count a failure. They get what is left of the artist's time cap (the same
    // deadline as `gather`, so an artist never takes longer than before). A source waiting on
    // its rate limit puts a completed artist back to `pending` (bounded by the source's pause);
    // running out of time does not: the artist stays `completed` and the remaining albums wait
    // for its next enrichment, so a slow image host cannot make it be claimed every cycle.
    let again =
        match tokio::time::timeout_at(deadline, album_covers::fill_for_artist(ctx, a.id)).await {
            Ok(Outcome::Answered(_) | Outcome::Absent(_)) => false,
            Ok(Outcome::RateLimited { .. }) => true,
            Ok(Outcome::Transient(f) | Outcome::Permanent(f)) => {
                warn!(artist = %a.id, code = %f.code, "album covers could not be filled");
                false
            }
            Err(_) => {
                debug!(artist = %a.id, "album covers ran out of the artist's time; the rest waits");
                false
            }
        };
    if again
        && result == ArtistResult::Completed
        && identity::reopen_completed_artist(&ctx.pool, a.id).await?
    {
        return Ok(ArtistResult::Pending);
    }
    Ok(result)
}

/// Writes the outcome and, while the artist row is still locked by that write, puts the staged
/// image in place when the row accepted it (no `user` image in the way). An image the write
/// cleared (another artist's id) is deleted before the commit, unless a user image holds the
/// place. A user image writer that locks the row before replacing the file can therefore
/// never have its file overwritten or deleted by a run.
///
/// If the commit fails after the rename, the file already holds the new (higher-ranked) image
/// while the row keeps the old source label; the next run sees the lower rank and records it
/// again (review m2, accepted).
///
/// The file steps are plain blocking calls on this task, not `spawn_blocking`: a detached
/// closure would still run after a cancelled run's rollback released the lock. Each is one
/// rename or unlink (and a stamp of a few bytes), so they do not hold up the runtime. If the
/// image cannot be put in place, the transaction is rolled back and the outcome is written
/// again without the image.
async fn save(
    ctx: &IdentCtx,
    id: Uuid,
    write: &ArtistWrite,
    image: Option<NewImage>,
) -> sqlx::Result<()> {
    let mut tx = ctx.pool.begin().await?;
    let Some(saved) = identity::save_artist_outcome_in(&mut tx, id, write).await? else {
        // Not `enriching` any more (a wipe, a restart, a prune): nothing is written and the
        // staged file goes with `image`.
        tx.rollback().await?;
        return Ok(());
    };
    let data_dir = &ctx.library.data_dir;
    let accepted = match (&write.image, image) {
        (Change::Set((_, source)), Some(new)) if saved.image_source.as_deref() == Some(source) => {
            Some(new)
        }
        _ => None,
    };
    if let Some(new) = accepted {
        if let Err(error) = new.file.persist(&artwork::artist_image_path(data_dir, id)) {
            warn!(artist = %id, %error, "could not put an artist image in place");
            tx.rollback().await?;
            let without_image = ArtistWrite {
                image: Change::Keep,
                ..write.clone()
            };
            identity::save_artist_outcome(&ctx.pool, id, &without_image).await?;
            return Ok(());
        }
        let stamped = match &new.local {
            Some(stamp) => artwork::write_local_stamp(data_dir, id, stamp),
            None => artwork::remove_local_stamp(data_dir, id),
        };
        if let Err(error) = stamped {
            warn!(artist = %id, %error, "could not record where the artist image came from");
        }
    }
    if write.image == Change::Clear && saved.image_source.is_none() {
        // Still under the row lock, so a user image cannot land in between.
        if let Err(error) = artwork::remove_artist_image(data_dir, id) {
            warn!(artist = %id, %error, "could not remove an artist image no row refers to");
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Claims up to [`BATCH`] artists and enriches them, at most `enrichmentConcurrency` at once.
/// Nothing is claimed when `cancel` already fired.
pub async fn run_batch(ctx: &IdentCtx, cancel: &CancellationToken) -> sqlx::Result<BatchReport> {
    run_batch_until(ctx, cancel, &CancellationToken::new()).await
}

/// [`run_batch`]; once `stop` or `cancel` fires no further artist starts and the claimed ones
/// not started go back to `pending`. `stop` lets the ones in flight finish; `cancel` abandons
/// them too, and they go back to `pending` as well. `done` counts `completed` and
/// `unresolvable`, `failed` counts `failed` and `pending`. The first database error is returned
/// after every started artist ended.
pub async fn run_batch_until(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
) -> sqlx::Result<BatchReport> {
    if cancel.is_cancelled() || stop.is_cancelled() {
        return Ok(BatchReport::default());
    }
    let claimed = identity::claim_artists(&ctx.pool, BATCH).await?;
    if claimed.is_empty() {
        return Ok(BatchReport::default());
    }
    let in_flight = ctx
        .settings
        .get()
        .await
        .map_or(1, |kv| usize::from(kv.enrichment_concurrency))
        .max(1);
    let mut report = BatchReport {
        claimed: claimed.len(),
        ..BatchReport::default()
    };
    let permits = Arc::new(Semaphore::new(in_flight));
    let mut set = JoinSet::new();
    let mut pending = claimed.into_iter();
    let mut give_back = Vec::new();
    while let Some(a) = pending.next() {
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            _ = stop.cancelled() => None,
            p = permits.clone().acquire_owned() => Some(p.expect("the semaphore is never closed")),
        };
        let Some(permit) = permit else {
            give_back.push(a.id);
            give_back.extend(pending.map(|a| a.id));
            break;
        };
        let ctx = ctx.clone();
        let cancel = cancel.clone();
        set.spawn(async move {
            let _permit = permit;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => (a.id, None),
                r = enrich_one(&ctx, &a) => (a.id, Some(r)),
            }
        });
    }

    let mut first_err = None;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((_, Some(Ok(ArtistResult::Completed | ArtistResult::Unresolvable)))) => {
                report.done += 1
            }
            Ok((_, Some(Ok(ArtistResult::Failed { .. } | ArtistResult::Pending)))) => {
                report.failed += 1
            }
            Ok((_, Some(Err(e)))) => {
                first_err.get_or_insert(e);
            }
            Ok((id, None)) => give_back.push(id),
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => {}
        }
    }
    if let Err(e) = release_artists(&ctx.pool, &give_back).await {
        first_err.get_or_insert(e);
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(report),
    }
}
