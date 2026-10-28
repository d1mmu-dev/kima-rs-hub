//! The candidate search (spec §19.1-§19.2): one job per album on the `autotag` queue collects
//! MusicBrainz releases (the album's tag, a manual MBID, a release search, the tracks'
//! recordings), fetches each with its release group, scores and orders them, and stores them
//! with everything apply needs (the snapshot, so apply makes no request).
//!
//! Outcomes: a rate limit from any request puts the same job back at the pause's end and
//! returns `Ok` (the album stays `queued`); a transient failure is retried by the queue and
//! marks the album `failed` only on the last attempt; a permanent one fails it at once.

use std::collections::{HashMap, HashSet};

use kima_db::PgPool;
use kima_db::autotag::{
    self, AlbumState, LocalAlbum, LocalTrack, NewCandidate, Origin, local_album, set_state,
    store_search,
};
use kima_jobs::queue;
use kima_jobs::worker::{self, Handler, JobError};
use kima_library::autotag::events::{AutotagEvents, AutotagJob};
use kima_net::{Failure, Net, Outcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use super::score::{self, Scored};
use crate::IdentCtx;
use crate::mbid::Mbid;
use crate::musicbrainz::{self, ArtistClause, Release, ReleaseGroup};

/// The autotagger's job queue (concurrency 1).
pub const QUEUE: &str = "autotag";
/// The search job's payload `kind`.
pub const SEARCH_KIND: &str = "search";
pub const MAX_ATTEMPTS: i32 = 3;
/// The most candidates one album keeps.
pub const MAX_CANDIDATES: usize = 10;
/// A release search with fewer hits than this is repeated without `AND tracks:<n>`.
pub const RETRY_BELOW: usize = 2;
/// The most recordings probed for the releases they appear on.
pub const MAX_PROBED: usize = 5;
/// The most releases the recording probe adds.
pub const MAX_RECORDING_RELEASES: usize = 5;
/// Recording sources probed first (the most trustworthy).
const PREFERRED_SOURCES: [&str; 3] = ["file_tag", "tracklist", "isrc"];
/// The album's error while identification is switched off (spec §15.3).
pub const OFF_MESSAGE: &str = "Outbound lookups are turned off";
/// How an unknown or blank local album artist is scored and searched (spec §19.3).
const UNKNOWN_ARTIST: &str = "Unknown Artist";

/// A manual search's text: replaces the album's title and artist in the release search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualQuery {
    pub artist: String,
    pub album: String,
}

/// The search job: `{"kind":"search","albumId":…,"query"?:{artist,album},"releaseMbid"?:…}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchJob {
    pub album_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<ManualQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_mbid: Option<Mbid>,
}

impl SearchJob {
    /// A search with neither a query nor an MBID.
    pub fn plain(album_id: Uuid) -> Self {
        Self {
            album_id,
            query: None,
            release_mbid: None,
        }
    }

    /// Neither a query nor an MBID: the only kind deduplicated per album.
    pub fn is_plain(&self) -> bool {
        self.query.is_none() && self.release_mbid.is_none()
    }

    /// The job payload, `kind` included.
    pub fn payload(&self) -> Value {
        let mut v = serde_json::to_value(self).expect("a search job serializes");
        v["kind"] = Value::from(SEARCH_KIND);
        v
    }

    /// The job a payload describes; `None` for another kind or a malformed one.
    pub fn from_payload(payload: &Value) -> Option<Self> {
        if payload.get("kind").and_then(Value::as_str) != Some(SEARCH_KIND) {
            return None;
        }
        serde_json::from_value(payload.clone()).ok()
    }
}

/// A release reference typed by a person: a bare MBID, or a URL whose path is
/// `/release/<mbid>` (trailing segments ignored) on `musicbrainz.org` or a subdomain of it,
/// over http or https. Anything else is `None`.
pub fn parse_release_ref(s: &str) -> Option<Mbid> {
    let s = s.trim();
    if let Some(id) = Mbid::parse(s) {
        return Some(id);
    }
    let lower = s.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &s[8..]
    } else if lower.starts_with("http://") {
        &s[7..]
    } else {
        return None;
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    // A user-info part (`musicbrainz.org@elsewhere`) names another host.
    if authority.contains('@') {
        return None;
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(host, _)| host)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host != "musicbrainz.org" && !host.ends_with(".musicbrainz.org") {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut segments = path.split('/').filter(|p| !p.is_empty());
    match (segments.next(), segments.next()) {
        (Some("release"), Some(id)) => Mbid::parse(id),
        _ => None,
    }
}

/// Queues a search for the album and marks it `queued`. A plain search is not queued again
/// while another search of the album waits (`queued`); a manual one always is. Returns the new
/// job's id, `None` for a duplicate.
pub async fn enqueue_search(pool: &PgPool, job: &SearchJob) -> sqlx::Result<Option<Uuid>> {
    set_state(pool, job.album_id, AlbumState::Queued, None).await?;
    if job.is_plain() {
        queue::enqueue_unless_queued_with_key(pool, QUEUE, job.payload(), MAX_ATTEMPTS, "albumId")
            .await
    } else {
        queue::enqueue(pool, QUEUE, job.payload(), MAX_ATTEMPTS)
            .await
            .map(Some)
    }
}

/// Why a search stopped before storing.
#[derive(Debug)]
enum Stop {
    RateLimited(OffsetDateTime),
    Transient(Failure),
    Permanent(Failure),
    Cancelled,
    Db(sqlx::Error),
}

impl Stop {
    /// A non-answer as a stop; `None` for `Answered` and `Absent`.
    fn of<T>(o: Outcome<T>) -> Option<Stop> {
        match o {
            Outcome::Answered(_) | Outcome::Absent(_) => None,
            Outcome::RateLimited { until } => Some(Stop::RateLimited(until)),
            Outcome::Transient(f) => Some(Stop::Transient(f)),
            Outcome::Permanent(f) => Some(Stop::Permanent(f)),
        }
    }

    /// A request's stop as an outcome ([`collect`] answers in outcomes).
    fn outcome<T>(self) -> Outcome<T> {
        match self {
            Stop::RateLimited(until) => Outcome::RateLimited { until },
            Stop::Transient(f) => Outcome::Transient(f),
            Stop::Permanent(f) => Outcome::Permanent(f),
            Stop::Cancelled | Stop::Db(_) => unreachable!("collect stops only on requests"),
        }
    }
}

/// An answer, `None` for an absence, or the stop.
fn settle<T>(o: Outcome<T>) -> Result<Option<T>, Stop> {
    match o {
        Outcome::Answered(t) => Ok(Some(t)),
        Outcome::Absent(_) => Ok(None),
        other => Err(Stop::of(other).expect("not an answer")),
    }
}

fn is_unknown_artist(name: &str) -> bool {
    let name = name.trim();
    name.is_empty() || name.eq_ignore_ascii_case(UNKNOWN_ARTIST)
}

/// Candidates in collection order, unique by release, at most [`MAX_CANDIDATES`].
#[derive(Default)]
struct Collected {
    list: Vec<(Mbid, Origin)>,
    seen: HashSet<Mbid>,
}

impl Collected {
    fn push(&mut self, id: Mbid, origin: Origin) {
        if !self.full() && self.seen.insert(id) {
            self.list.push((id, origin));
        }
    }

    fn full(&self) -> bool {
        self.list.len() >= MAX_CANDIDATES
    }
}

/// One release search; no hits, an absence and a refused query (400) are no releases.
async fn search_once(net: &Net, query: &str) -> Result<Vec<Mbid>, Stop> {
    match musicbrainz::search_releases(net, query).await {
        Outcome::Permanent(f) if f.code == "HTTP_400" => Ok(Vec::new()),
        o => Ok(settle(o)?.unwrap_or_default()),
    }
}

/// Step 3: the release search, repeated without the track count when it finds fewer than two.
async fn search_step(net: &Net, album: &LocalAlbum, job: &SearchJob) -> Result<Vec<Mbid>, Stop> {
    let (title, clause) = match &job.query {
        Some(q) => (q.album.as_str(), ArtistClause::Name(&q.artist)),
        None if album.is_compilation => (album.title.as_str(), ArtistClause::Various),
        None if is_unknown_artist(&album.artist_name) => (album.title.as_str(), ArtistClause::Any),
        None => (album.title.as_str(), ArtistClause::Name(&album.artist_name)),
    };
    if title.trim().is_empty() {
        return Ok(Vec::new());
    }
    let n = album.tracks.len();
    let mut ids = search_once(net, &musicbrainz::release_query(title, clause, Some(n))).await?;
    if ids.len() < RETRY_BELOW {
        for id in search_once(net, &musicbrainz::release_query(title, clause, None)).await? {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

/// The recordings probed: distinct, the preferred sources first, then album order.
fn probed(tracks: &[LocalTrack]) -> Vec<Mbid> {
    let mut with: Vec<&LocalTrack> = tracks
        .iter()
        .filter(|t| t.recording_mbid.is_some())
        .collect();
    with.sort_by_key(|t| {
        let preferred = t
            .recording_source
            .as_deref()
            .is_some_and(|s| PREFERRED_SOURCES.contains(&s));
        !preferred
    });
    let mut out: Vec<Mbid> = Vec::new();
    for t in with {
        let id = Mbid(t.recording_mbid.expect("filtered"));
        if !out.contains(&id) {
            out.push(id);
            if out.len() == MAX_PROBED {
                break;
            }
        }
    }
    out
}

/// Step 4: the releases at least half of the answered probes list, most-listed first.
async fn recording_step(net: &Net, album: &LocalAlbum) -> Result<Vec<Mbid>, Stop> {
    let mut counts: Vec<(Mbid, usize)> = Vec::new();
    let mut answered = 0usize;
    for rec in probed(&album.tracks) {
        let Some(releases) = settle(musicbrainz::recording_releases(net, rec).await)? else {
            continue; // a recording MusicBrainz no longer has tells nothing
        };
        answered += 1;
        let mut once = HashSet::new();
        for id in releases.into_iter().filter(|id| once.insert(*id)) {
            match counts.iter_mut().find(|(r, _)| *r == id) {
                Some((_, n)) => *n += 1,
                None => counts.push((id, 1)),
            }
        }
    }
    let need = answered.div_ceil(2);
    // Stable: equal counts keep the order MusicBrainz listed them in.
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    Ok(counts
        .into_iter()
        .filter(|(_, n)| answered > 0 && *n >= need)
        .take(MAX_RECORDING_RELEASES)
        .map(|(id, _)| id)
        .collect())
}

/// §19.2 steps 1-4 in order (tag, manual, search, recording), unique by release MBID, at most
/// [`MAX_CANDIDATES`]. Recordings are not probed once the list is full. Any request that
/// neither answers nor reports an absence stops the collection with its outcome.
pub async fn collect(
    ctx: &IdentCtx,
    album: &LocalAlbum,
    job: &SearchJob,
) -> Outcome<Vec<(Mbid, Origin)>> {
    match collect_inner(&ctx.net, album, job).await {
        Ok(c) => Outcome::Answered(c.list),
        Err(stop) => stop.outcome(),
    }
}

async fn collect_inner(net: &Net, album: &LocalAlbum, job: &SearchJob) -> Result<Collected, Stop> {
    let mut c = Collected::default();
    if let Some(id) = album.release_mbid {
        c.push(Mbid(id), Origin::Tag);
    }
    if let Some(id) = job.release_mbid {
        c.push(id, Origin::Manual);
    }
    for id in search_step(net, album, job).await? {
        c.push(id, Origin::Search);
    }
    if !c.full() {
        for id in recording_step(net, album).await? {
            c.push(id, Origin::Recording);
        }
    }
    Ok(c)
}

/// How one attempt of the job runs.
#[derive(Debug, Clone, Default)]
pub struct SearchAttempt {
    /// The job's final attempt: a transient failure now marks the album `failed`.
    pub last: bool,
    /// Shutdown or a lost claim: the search stops between releases and is retried.
    pub cancel: CancellationToken,
}

/// The album as it is scored and searched: a blank album artist reads as "Unknown Artist",
/// which the distance skips, rather than as a name that matches nothing.
fn for_scoring(mut album: LocalAlbum) -> LocalAlbum {
    if album.artist_name.trim().is_empty() {
        album.artist_name = UNKNOWN_ARTIST.to_string();
    }
    album
}

/// A fetched, scored candidate.
struct Fetched {
    origin: Origin,
    release: Release,
    group: ReleaseGroup,
    scored: Scored,
}

fn to_json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("a candidate's parts serialize")
}

impl Fetched {
    fn into_new(self) -> NewCandidate {
        NewCandidate {
            release_mbid: self.release.id.0,
            release_group_mbid: self.group.id.0,
            origin: self.origin,
            distance: self.scored.distance,
            recommendation: self.scored.recommendation,
            penalties: to_json(&self.scored.penalties),
            mapping: to_json(&self.scored.mapping.pairs),
            extra_tracks: to_json(&self.scored.mapping.extra),
            missing_tracks: to_json(&self.scored.mapping.missing),
            release: to_json(&self.release),
            release_group: to_json(&self.group),
        }
    }
}

/// Steps 1-6: collect, fetch each release and its group, score; best first.
async fn search_and_score(
    ctx: &IdentCtx,
    album: &LocalAlbum,
    job: &SearchJob,
    cancel: &CancellationToken,
    events: &mut AutotagEvents,
) -> Result<Vec<NewCandidate>, Stop> {
    let net = &ctx.net;
    let ids = collect_inner(net, album, job).await?.list;
    let total = ids.len();
    let this_year = OffsetDateTime::now_utc().year();
    let mut groups: HashMap<Mbid, ReleaseGroup> = HashMap::new();
    let mut fetched: Vec<Fetched> = Vec::new();
    let mut releases_seen = HashSet::new();
    for (done, (id, origin)) in ids.into_iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        let candidate = fetch(net, &mut groups, id).await?;
        // A merged release answers under its new id: keep the first candidate per release.
        if let Some((release, group)) = candidate.filter(|(r, _)| releases_seen.insert(r.id)) {
            let scored = score::score(album, &release, &group, this_year);
            if scored.distance.is_finite() {
                fetched.push(Fetched {
                    origin,
                    release,
                    group,
                    scored,
                });
            } else {
                warn!(release = %id, "candidate scored a non-finite distance; skipped");
            }
        }
        events.progress(
            AutotagJob::Search,
            Some(album.id),
            None,
            done + 1,
            total,
            Some(AlbumState::Searching.as_str()),
        );
    }
    fetched.sort_by_cached_key(|f| score::order_key(&f.release, &f.scored));
    Ok(fetched.into_iter().map(Fetched::into_new).collect())
}

/// Steps 5-6 for one release: the release, then its group (one request per group per job).
/// `None` when either is absent, or when the release names no readable group: a candidate
/// without its group's snapshot could not be applied, so it is skipped, never stored groupless.
async fn fetch(
    net: &Net,
    groups: &mut HashMap<Mbid, ReleaseGroup>,
    id: Mbid,
) -> Result<Option<(Release, ReleaseGroup)>, Stop> {
    let Some(release) = settle(musicbrainz::release(net, id).await)? else {
        return Ok(None);
    };
    let Some(rg) = release.release_group.as_ref().map(|g| g.id) else {
        warn!(release = %id, "release names no readable release group; skipped");
        return Ok(None);
    };
    let group = match groups.get(&rg) {
        Some(g) => g.clone(),
        None => match settle(musicbrainz::release_group(net, rg).await)? {
            Some(g) => {
                groups.insert(rg, g.clone());
                g
            }
            None => return Ok(None),
        },
    };
    Ok(Some((release, group)))
}

fn db(e: sqlx::Error) -> JobError {
    JobError::Retry(format!("autotag state: {e}"))
}

fn failure_text(f: &Failure) -> String {
    format!("MusicBrainz {}: {}", f.code, f.message)
}

/// One attempt of the search job (§19.2). The album must exist with a present track (else
/// `Permanent("album not found")`, nothing written); with identification off it is marked
/// `failed` without a request.
pub async fn run_search(
    ctx: &IdentCtx,
    job: &SearchJob,
    attempt: SearchAttempt,
) -> Result<(), JobError> {
    let pool = &ctx.pool;
    let album_id = job.album_id;
    let Some(album) = local_album(pool, album_id).await.map_err(db)? else {
        return Err(JobError::Permanent("album not found".into()));
    };
    let mut events = AutotagEvents::new(ctx.events.clone());
    let finish = |events: &mut AutotagEvents, done: usize, state: AlbumState| {
        events.finish(
            AutotagJob::Search,
            Some(album_id),
            None,
            done,
            done,
            Some(state.as_str()),
        );
    };
    if !ctx.net.enabled() {
        set_state(pool, album_id, AlbumState::Failed, Some(OFF_MESSAGE))
            .await
            .map_err(db)?;
        finish(&mut events, 0, AlbumState::Failed);
        return Err(JobError::Permanent(OFF_MESSAGE.into()));
    }
    let album = for_scoring(album);
    set_state(pool, album_id, AlbumState::Searching, None)
        .await
        .map_err(db)?;
    events.progress(
        AutotagJob::Search,
        Some(album_id),
        None,
        0,
        0,
        Some(AlbumState::Searching.as_str()),
    );

    let result = match search_and_score(ctx, &album, job, &attempt.cancel, &mut events).await {
        Ok(ordered) => store_search(pool, album_id, &ordered)
            .await
            .map(|_| ordered.len())
            .map_err(Stop::Db),
        Err(stop) => Err(stop),
    };
    match result {
        Ok(n) => {
            let state = if n == 0 {
                AlbumState::NoCandidates
            } else {
                AlbumState::Proposed
            };
            finish(&mut events, n, state);
            Ok(())
        }
        Err(Stop::RateLimited(until)) => {
            set_state(pool, album_id, AlbumState::Queued, None)
                .await
                .map_err(db)?;
            queue::enqueue_at(pool, QUEUE, job.payload(), MAX_ATTEMPTS, until)
                .await
                .map_err(db)?;
            finish(&mut events, 0, AlbumState::Queued);
            Ok(())
        }
        Err(Stop::Permanent(f)) => {
            let text = failure_text(&f);
            set_state(pool, album_id, AlbumState::Failed, Some(&text))
                .await
                .map_err(db)?;
            finish(&mut events, 0, AlbumState::Failed);
            Err(JobError::Permanent(text))
        }
        Err(stop) => {
            let text = match &stop {
                Stop::Transient(f) => failure_text(f),
                Stop::Db(e) => format!("autotag state: {e}"),
                _ => "autotag search cancelled".to_string(),
            };
            // Only the last attempt's failure is the album's; before it the queue retries.
            let failed = attempt.last && !matches!(stop, Stop::Cancelled);
            let (state, error) = if failed {
                (AlbumState::Failed, Some(text.as_str()))
            } else {
                (AlbumState::Queued, None)
            };
            if let Err(e) = set_state(pool, album_id, state, error).await {
                warn!(error = %e, album = %album_id, "could not record the search's state");
            }
            finish(&mut events, 0, state);
            Err(JobError::Retry(text))
        }
    }
}

/// The `autotag` queue handler: search jobs. The attempt that reaches the job's
/// `max_attempts` is its last.
pub fn handler(ctx: IdentCtx) -> Handler {
    worker::handler_ctx(move |job, job_ctx| {
        let ctx = ctx.clone();
        async move {
            let Some(search) = SearchJob::from_payload(&job.payload) else {
                return Err(JobError::Permanent(format!(
                    "invalid autotag job payload: {}",
                    job.payload
                )));
            };
            let attempt = SearchAttempt {
                last: job.attempts >= job.max_attempts,
                cancel: job_ctx.cancel,
            };
            run_search(&ctx, &search, attempt).await
        }
    })
}

/// Startup, before the workers start: searches a dead process left `running` are requeued
/// (or failed on their last attempt), and albums it left `searching` follow their job.
pub async fn prepare(pool: &PgPool) -> sqlx::Result<()> {
    let recovered = queue::requeue_running(pool, QUEUE).await?;
    if recovered.requeued + recovered.failed > 0 {
        tracing::info!(
            requeued = recovered.requeued,
            failed = recovered.failed,
            "recovered autotag searches left running by a previous process"
        );
    }
    autotag::reset_interrupted_searches(pool).await?;
    Ok(())
}
