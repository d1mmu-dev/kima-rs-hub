//! Artist enrichment (spec §10.5, §12.1-§12.5; upstream `artistEnrichment.ts:74-387`): the
//! MBID (the guard, then name resolution), the bio, the image, MusicBrainz genres and similar
//! artists, then one write of the outcome.
//!
//! Every answer is untrusted. A source that fails keeps what was stored (it never records
//! "none") and, for MusicBrainz, Wikidata and the similar-artist sources, fails the run: the
//! artist is `failed`, and `unresolvable` at the [`FAILURE_CAP`]-th failure in a row; a source
//! that only waits on its rate limit leaves the artist `pending` without counting a failure.
//! Image sources never fail the run. Images are decoded and re-encoded before they are stored
//! under `<data_dir>/artist-images/<id>.jpg`, and a `user` image is never touched.

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
use crate::{IdentCtx, Mbid, deezer, fanart, listenbrainz, similar, text, wikidata};

/// Artists claimed per batch (spec §10.6).
pub const BATCH: i64 = 10;
/// Namesakes whose catalogue is read before giving up (`MAX_CATALOGUE_CHECKS`).
pub const MAX_CATALOGUE_CHECKS: usize = 3;
/// Failures in a row after which an artist is `unresolvable` (`STAGE_RETRY_CAP`).
pub const FAILURE_CAP: i16 = RETRY_CAP;
/// MusicBrainz genres kept per artist (spec §12.4).
pub const MAX_GENRES: usize = 5;
/// The most one artist's enrichment may take, every request and download included; past it the
/// run fails with `TIMEOUT` and nothing it learned is written.
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

/// What a run learned, before it is written.
struct Gathered {
    write: ArtistWrite,
    steps: Steps,
    /// The run is over: MusicBrainz has no such artist.
    unresolvable: bool,
    /// A new image file was written.
    image_written: bool,
}

/// Normalises `bytes` and stores them as the artist's image, off the async threads.
///
/// CARRIED (Task 21 review I2, to the task that writes user images, spec §16.3): the file is
/// written to `<id>.jpg` before `save_artist_outcome` checks the row's `image_source`, so a
/// `user` image set while this artist is enriching could be overwritten on disk (the row keeps
/// `user`); likewise a guard-drop deletes the file after the commit. The user-image writer
/// must lock the artist row (or use a distinct file name) to close this.
async fn store_image(ctx: &IdentCtx, artist: Uuid, bytes: Vec<u8>) -> Result<(), String> {
    let data_dir = ctx.library.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        let jpeg = artwork::normalize_image(&bytes).map_err(|e| e.to_string())?;
        artwork::store_artist_image(&data_dir, artist, &jpeg).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Spec §12.3 step 2: the artist's image in the library, copied when found.
async fn store_local_image(ctx: &IdentCtx, a: &ArtistForEnrichment) -> bool {
    let root = ctx.library.root.get();
    let dirs: Vec<PathBuf> = a
        .album_dirs
        .iter()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect();
    let found = tokio::task::spawn_blocking(move || {
        let path = artwork::find_local_artist_image(&root, &dirs)?;
        artwork::read_local_image(&path).ok()
    })
    .await
    .ok()
    .flatten();
    let Some(bytes) = found else {
        return false;
    };
    match store_image(ctx, a.id, bytes).await {
        Ok(()) => true,
        Err(error) => {
            warn!(artist = %a.id, %error, "the library's artist image could not be used");
            false
        }
    }
}

/// Downloads `url` and stores it as the artist's image. A failure (a download refused or
/// failed, an image that does not decode) is logged and the next source is tried.
async fn download_image(ctx: &IdentCtx, artist: Uuid, source: &str, url: &str) -> bool {
    let bytes = match ctx.net.get_bytes(url).await {
        Outcome::Answered(b) => b,
        other => {
            // The message names the host only (kima-net), never the URL's path or query.
            if let Some(code) = other.code() {
                debug!(%artist, source, code, "artist image download failed");
            }
            return false;
        }
    };
    match store_image(ctx, artist, bytes).await {
        Ok(()) => true,
        Err(error) => {
            debug!(%artist, source, %error, "artist image did not decode");
            false
        }
    }
}

/// Spec §12.1-§12.5 in upstream's order. Writes nothing; files are written (the image).
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
        image_written: false,
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
    let mut image_written = false;
    let image_name = format!("{}.jpg", a.id.hyphenated());
    let current = image_rank(image_source.as_deref());
    if current < image_rank(Some("user")) {
        if store_local_image(ctx, a).await {
            image_written = true;
            write.image = Change::Set((image_name.clone(), "local".to_owned()));
        } else if current < image_rank(Some("local")) {
            let mut stored = false;
            if let Some(url) = wikidata_image.filter(|_| current < image_rank(Some("wikidata"))) {
                stored = download_image(ctx, a.id, "wikidata", &url).await;
                if stored {
                    write.image = Change::Set((image_name.clone(), "wikidata".to_owned()));
                }
            }
            if !stored && current < image_rank(Some("fanart")) {
                if let (Some(id), Some(key)) = (mbid, ctx.fanart_key().await) {
                    if let Outcome::Answered(Some(url)) = fanart::artist_image(net, &key, id).await
                    {
                        stored = download_image(ctx, a.id, "fanart", &url).await;
                        if stored {
                            write.image = Change::Set((image_name.clone(), "fanart".to_owned()));
                        }
                    }
                }
            }
            if !stored && current < image_rank(Some("deezer")) {
                if let Some(url) = deezer_hit.as_ref().and_then(|h| h.picture.as_deref()) {
                    stored = download_image(ctx, a.id, "deezer", url).await;
                    if stored {
                        write.image = Change::Set((image_name, "deezer".to_owned()));
                    }
                }
            }
            image_written = stored;
        }
    }

    Gathered {
        write,
        steps,
        unresolvable: false,
        image_written,
    }
}

/// Enriches one claimed artist and writes the outcome (spec §12.1-§12.5, §10.5). A database
/// error is returned; everything else ends in the [`ArtistResult`] written to the row.
pub async fn enrich_one(ctx: &IdentCtx, a: &ArtistForEnrichment) -> sqlx::Result<ArtistResult> {
    let g = match tokio::time::timeout(ARTIST_TIMEOUT, gather(ctx, a)).await {
        Ok(g) => g,
        Err(_) => Gathered {
            write: ArtistWrite::status(ArtistStatus::Completed, 0),
            steps: Steps {
                failure: Some(Failure::new(
                    "TIMEOUT",
                    format!("artist enrichment took over {ARTIST_TIMEOUT:?}"),
                )),
                waiting: false,
            },
            unresolvable: false,
            // A download may have finished before the deadline.
            image_written: true,
        },
    };
    let Gathered {
        mut write,
        steps,
        unresolvable,
        image_written,
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
    let saved = identity::save_artist_outcome(&ctx.pool, a.id, &write).await?;
    // The image was dropped with another artist's id, or the artist was pruned meanwhile: the
    // file goes too.
    let orphaned = if saved {
        write.image == Change::Clear
    } else {
        image_written && !identity::artist_exists(&ctx.pool, a.id).await?
    };
    if orphaned {
        let data_dir = ctx.library.data_dir.clone();
        let id = a.id;
        let removed =
            tokio::task::spawn_blocking(move || artwork::remove_artist_image(&data_dir, id)).await;
        if let Ok(Err(error)) = removed {
            warn!(artist = %id, %error, "could not remove an artist image no row refers to");
        }
    }
    Ok(result)
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
