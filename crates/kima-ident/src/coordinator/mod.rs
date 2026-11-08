//! The enrichment coordinator (spec §10.6): one `cycle` job on the `enrich` queue drains
//! `track_stages`, then queues the next cycle. The control state lives in `enrichment_state`
//! ([`kima_db::enrichment`]), so it survives restarts.
//!
//! A cycle runs the fingerprint phase (local, whenever fingerprinting is available) alongside
//! the network phases (only while the identification switch is on): artists, then tracks (the
//! recording stage), each skipped while MusicBrainz is paused. A watcher re-reads the state at most every
//! [`STATE_REREAD`]; once it is no longer `running` (paused, stopping, halted) the phases
//! start nothing new, release what they had claimed but not started, and let the items in
//! flight finish. The job's cancel token (shutdown, lost claim) abandons in-flight items too.
//!
//! Every `enrichment:progress` event of a cycle is published here (spec §17): when a batch
//! changed counts, and when the status a cycle leaves differs from the one it found (a run
//! starting or finishing, `stopping` becoming `idle`). A cycle with nothing to do ends where it
//! began and is silent.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

pub mod control;

use kima_db::PgPool;
use kima_db::enrichment::{self, Phase, State, Status};
use kima_jobs::queue;
use kima_jobs::worker::{self, Handler, JobError};
use kima_library::jobs::OnChange;
use kima_net::Source;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::IdentCtx;
use crate::artist;
use crate::events::ProgressEvents;
use crate::stages::moods::MoodPass;
use crate::stages::similarity::SimilarityPass;
use crate::stages::{BatchReport, analysis, embedding, fingerprint, moods, recording, similarity};

/// The coordinator's job queue (concurrency 1).
pub const QUEUE: &str = "enrich";
/// The one job kind: `{"kind":"cycle"}`.
pub const CYCLE_KIND: &str = "cycle";
pub const MAX_ATTEMPTS: i32 = 3;
/// A cycle never starts sooner than this after the previous one (`MIN_INTERVAL_MS`).
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);
/// The next cycle after one that processed items.
pub const NEXT_AFTER_WORK: Duration = Duration::from_secs(5);
/// The next cycle after one that found nothing to do.
pub const NEXT_WHEN_IDLE: Duration = Duration::from_secs(60);
/// The state is re-read at most this often while a cycle runs.
pub const STATE_REREAD: Duration = Duration::from_secs(2);
/// While a library scan is running or queued a cycle starts nothing and comes back this soon
/// (upstream's "declined" cycle: an empty one backs off, a declined one returns promptly).
pub const WHILE_SCANNING: Duration = Duration::from_secs(10);
/// A running pipeline with no activity for this long is hung (upstream `detectHang`).
pub const HUNG_AFTER: time::Duration = time::Duration::minutes(15);

/// Whether the pipeline is hung at `now`: `running` and no activity for over [`HUNG_AFTER`].
pub fn hung(s: &State, now: OffsetDateTime) -> bool {
    s.status == Status::Running && s.last_activity < now - HUNG_AFTER
}

fn payload() -> serde_json::Value {
    serde_json::json!({ "kind": CYCLE_KIND })
}

/// Makes a cycle due now: a queued one is moved to now, else one is queued (spec §10.6
/// "Trigger"). One transaction on the database clock, so concurrent kicks leave one queued
/// cycle. A paused or halted pipeline is not lifted: the cycle it wakes does nothing.
pub async fn kick(pool: &PgPool) -> sqlx::Result<()> {
    queue::advance_or_enqueue_with_key(pool, QUEUE, payload(), MAX_ATTEMPTS, "kind").await?;
    Ok(())
}

/// [`kick`] as a library `on_change` hook: a reconcile or file update that changed tracks
/// kicks the coordinator. A failure is logged; the `enrich-kick` schedule is the backstop.
pub fn kick_on_change() -> OnChange {
    std::sync::Arc::new(|pool: PgPool| {
        Box::pin(async move {
            if let Err(error) = kick(&pool).await {
                warn!(%error, "could not kick the enrichment coordinator");
            }
        })
    })
}

/// Startup, before the workers start: cycles a dead process left `running` are requeued,
/// artists it left `enriching` go back to `pending`, and a cycle is kicked.
pub async fn prepare(pool: &PgPool) -> sqlx::Result<()> {
    let recovered = queue::requeue_running(pool, QUEUE).await?;
    if recovered.requeued + recovered.failed > 0 {
        info!(
            requeued = recovered.requeued,
            failed = recovered.failed,
            "recovered enrichment cycles left running by a previous process"
        );
    }
    let reset = enrichment::reset_enriching_artists(pool).await?;
    if reset > 0 {
        info!(reset, "artists left enriching are pending again");
    }
    kick(pool).await
}

/// The `enrich` queue handler.
pub fn handler(ctx: IdentCtx) -> Handler {
    worker::handler_ctx(move |job, job_ctx| {
        let ctx = ctx.clone();
        async move {
            match job.payload.get("kind").and_then(|k| k.as_str()) {
                Some(CYCLE_KIND) => run_cycle(&ctx, &job_ctx.cancel).await.map(|_| ()),
                _ => Err(JobError::Permanent(format!(
                    "invalid enrich job payload: {}",
                    job.payload
                ))),
            }
        }
    })
}

/// What one cycle did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CycleReport {
    /// The state let the cycle run (`running`, not halted).
    pub ran: bool,
    pub fingerprint: BatchReport,
    pub embedding: BatchReport,
    pub analysis: BatchReport,
    /// The mood step: `claimed = done =` scores written (it has no per-track rows to claim).
    pub moods: BatchReport,
    /// The similarity step: `claimed = done =` tracks that got feel or a map place.
    pub similarity: BatchReport,
    pub artists: BatchReport,
    pub tracks: BatchReport,
    /// Batches that ended in a database error (recorded as `last_error`, retried next cycle).
    pub errors: usize,
    /// When the next cycle was queued for; `None` when none was (paused, halted, stopped).
    pub next: Option<Duration>,
}

impl CycleReport {
    /// Whether any item was claimed.
    pub fn any_work(&self) -> bool {
        self.fingerprint.claimed
            + self.embedding.claimed
            + self.analysis.claimed
            + self.moods.claimed
            + self.similarity.claimed
            + self.artists.claimed
            + self.tracks.claimed
            > 0
    }
}

/// One cycle (spec §10.6 steps 1-7). `Err` only when the cycle was cancelled (the job is
/// retried; on shutdown the worker gives the attempt back) or the control state could not be
/// read or written. A database error inside a phase is recorded and the next cycle retries.
pub async fn run_cycle(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
) -> Result<CycleReport, JobError> {
    let pool = &ctx.pool;
    wait_min_interval(ctx, cancel).await?;

    // The status before the cycle touches it: what the cycle's announcements compare against.
    let found = enrichment::state(pool).await.map_err(db)?.status;
    let ann = Announcer::new(&ctx.progress_events, found);

    // The scan goes first: every step starts from the whole library, never the part scanned so
    // far (upstream `b78b1167`). Paused, halted and stopping pipelines go on to step 1-2 below
    // as they are; a declined cycle leaves the state alone and returns soon.
    let before = enrichment::state(pool).await.map_err(db)?;
    let may_start = !before.halted && matches!(before.status, Status::Idle | Status::Running);
    if may_start && enrichment::library_scan_running(pool).await.map_err(db)? {
        tracing::debug!("enrichment waits: a library scan is still adding songs");
        queue::enqueue_in_unless_queued_with_key(
            pool,
            QUEUE,
            payload(),
            MAX_ATTEMPTS,
            "kind",
            WHILE_SCANNING,
        )
        .await
        .map_err(db)?;
        return Ok(CycleReport {
            next: Some(WHILE_SCANNING),
            ..CycleReport::default()
        });
    }

    // Steps 1-2: paused, halted or stopping end here and queue nothing.
    let st = enrichment::begin_cycle(pool).await.map_err(db)?;
    let mut report = CycleReport::default();
    if !st.may_run() {
        ann.settle(&st);
        return Ok(report);
    }
    report.ran = true;

    let stop = CancellationToken::new();
    let phases = async {
        let (fp, emb, an, net) = tokio::join!(
            fingerprint_phase(ctx, cancel, &stop, &ann),
            embedding_phase(ctx, cancel, &stop, &ann),
            analysis_phase(ctx, cancel, &stop, &ann),
            network_phases(ctx, cancel, &stop, &ann)
        );
        // The library-wide steps read what the phases above made, so they follow them (spec 9.1).
        let vibe = vibe_phases(ctx, cancel, &stop, &ann).await;
        stop.cancel(); // ends the watcher
        (fp, emb, an, net, vibe)
    };
    let ((fp, emb, an, net, vibe), ()) = tokio::join!(phases, watch_state(pool, cancel, &stop));
    report.fingerprint = fp.report;
    report.embedding = emb.report;
    report.analysis = an.report;
    report.moods = vibe.moods.report;
    report.similarity = vibe.similarity.report;
    report.artists = net.artists.report;
    report.tracks = net.tracks.report;
    report.errors = fp.errors
        + emb.errors
        + an.errors
        + vibe.moods.errors
        + vibe.similarity.errors
        + net.artists.errors
        + net.tracks.errors;
    if cancel.is_cancelled() {
        return Err(JobError::Retry("enrichment cycle cancelled".into()));
    }

    // Step 7. A cycle without a batch error clears the last one.
    if report.errors == 0 {
        enrichment::clear_error(pool).await.map_err(db)?;
    }
    let idle = !report.any_work();
    enrichment::end_cycle(pool, idle).await.map_err(db)?;
    let st = enrichment::state(pool).await.map_err(db)?;
    ann.settle(&st);
    if st.halted || st.status == Status::Paused {
        return Ok(report);
    }
    let next = if idle {
        NEXT_WHEN_IDLE
    } else {
        NEXT_AFTER_WORK
    };
    // A cycle already queued (a kick) wins over this one.
    queue::enqueue_in_unless_queued_with_key(pool, QUEUE, payload(), MAX_ATTEMPTS, "kind", next)
        .await
        .map_err(db)?;
    report.next = Some(next);
    Ok(report)
}

fn db(e: sqlx::Error) -> JobError {
    JobError::Retry(format!("enrichment state: {e}"))
}

/// Sleeps out the rest of [`MIN_INTERVAL`] since the previous cycle started, then stamps this
/// one's start. Cancellable.
async fn wait_min_interval(ctx: &IdentCtx, cancel: &CancellationToken) -> Result<(), JobError> {
    let last = *ctx
        .last_cycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(wait) = last.and_then(|t| MIN_INTERVAL.checked_sub(t.elapsed())) {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(JobError::Retry("enrichment cycle cancelled".into()));
            }
            _ = tokio::time::sleep(wait) => {}
        }
    }
    *ctx.last_cycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    Ok(())
}

/// Re-reads the state every [`STATE_REREAD`] until `stop` or `cancel` fires; fires `stop` once
/// the state no longer lets the cycle run. A failed read is logged and retried.
async fn watch_state(pool: &PgPool, cancel: &CancellationToken, stop: &CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(STATE_REREAD) => {}
        }
        match enrichment::state(pool).await {
            Ok(st) if !st.may_run() => {
                stop.cancel();
                return;
            }
            Ok(_) => {}
            Err(error) => warn!(%error, "reading the enrichment state failed"),
        }
    }
}

#[derive(Default)]
struct PhaseOutcome {
    report: BatchReport,
    errors: usize,
}

/// Whether a phase goes on to another batch.
fn go_on(cancel: &CancellationToken, stop: &CancellationToken) -> bool {
    !cancel.is_cancelled() && !stop.is_cancelled()
}

/// Logs and records a batch's database error; the phase ends and the next cycle retries.
async fn batch_failed(ctx: &IdentCtx, phase: Phase, e: impl std::fmt::Display) {
    warn!(error = %e, phase = phase.as_str(), "enrichment batch failed; retrying next cycle");
    // A sqlx error names the statement's failure, never a bound value, so no secret reaches
    // `last_error`.
    let msg = format!("{} phase: {e}", phase.as_str());
    if let Err(error) = enrichment::record_error(&ctx.pool, &msg).await {
        warn!(%error, "recording the enrichment error failed");
    }
}

/// What one cycle has announced (spec §17). Counts changing are always announced; the status
/// only when it differs from the last one announced (or found, before the first).
struct Announcer<'a> {
    events: &'a ProgressEvents,
    status: Mutex<Status>,
    /// Serializes `mark`'s set-phase-then-read-state critical section: two local phases
    /// (fingerprint, embedding) run concurrently and must not interleave their writes and
    /// reads of `enrichment_state.current_phase`, or one can observe the other's phase.
    mark_lock: tokio::sync::Mutex<()>,
}

impl<'a> Announcer<'a> {
    fn new(events: &'a ProgressEvents, found: Status) -> Self {
        Self {
            events,
            status: Mutex::new(found),
            mark_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Counts changed: announced whatever the status.
    fn changed(&self, st: &State) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = st.status;
        self.events.notify(st.current_phase, st.status);
    }

    /// Announced only when the status is not the one last announced.
    fn settle(&self, st: &State) {
        let mut last = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        if *last != st.status {
            *last = st.status;
            drop(last);
            self.events.notify(st.current_phase, st.status);
        }
    }
}

/// Stamps the phase after a batch (`activity` when counts changed) and announces it: counts
/// changed, or the batch claimed work and the status moved (a run starting). A failure is
/// logged only.
async fn mark(ctx: &IdentCtx, ann: &Announcer<'_>, phase: Phase, b: BatchReport) {
    let changed = b.done + b.failed > 0;
    let _guard = ann.mark_lock.lock().await;
    if let Err(error) = enrichment::set_phase(&ctx.pool, Some(phase), changed).await {
        warn!(%error, "recording the enrichment phase failed");
        return;
    }
    if !changed && b.claimed == 0 {
        return;
    }
    match enrichment::state(&ctx.pool).await {
        Ok(st) if changed => ann.changed(&st),
        Ok(st) => ann.settle(&st),
        Err(error) => warn!(%error, "reading the enrichment state failed"),
    }
}

/// Step 3: fingerprint batches until none is left. Local work: runs whatever the switch says.
async fn fingerprint_phase(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    ann: &Announcer<'_>,
) -> PhaseOutcome {
    let mut out = PhaseOutcome::default();
    if !ctx.config.fingerprinting {
        return out;
    }
    let mut marked = false;
    while go_on(cancel, stop) {
        match fingerprint::run_batch_until(ctx, cancel, stop).await {
            Ok(b) => {
                let changed = b.done + b.failed > 0;
                if changed || !marked {
                    mark(ctx, ann, Phase::Fingerprint, b).await;
                    marked = true;
                }
                out.report += b;
                if b.claimed == 0 {
                    break;
                }
            }
            Err(e) => {
                out.errors += 1;
                batch_failed(ctx, Phase::Fingerprint, e).await;
                break;
            }
        }
    }
    out
}

/// Step 3b: embedding batches until none is left. Local work like fingerprint: runs
/// unconditionally, independent of `ctx.config.fingerprinting` and of
/// `network_may_run`/`ctx.net` (the MusicBrainz switch), per the owner's 2026-11-04
/// decision. Joined alongside `fingerprint_phase` and `network_phases` in `run_cycle`, so
/// it keeps claiming even while MusicBrainz network phases are paused or disabled.
async fn embedding_phase(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    ann: &Announcer<'_>,
) -> PhaseOutcome {
    let mut out = PhaseOutcome::default();
    let mut marked = false;
    while go_on(cancel, stop) {
        match embedding::run_batch_until(ctx, cancel, stop).await {
            Ok(b) => {
                let changed = b.done + b.failed > 0;
                if changed || !marked {
                    mark(ctx, ann, Phase::Embedding, b).await;
                    marked = true;
                }
                out.report += b;
                if b.claimed == 0 || b.halted {
                    break;
                }
            }
            Err(e) => {
                out.errors += 1;
                batch_failed(ctx, Phase::Embedding, e).await;
                break;
            }
        }
    }
    out
}

/// Analysis (M4) batches until none is left. Local DSP+MusiCNN work, independent of
/// the MusicBrainz switch. Runs in parallel with the network phases inside
/// `run_cycle`'s `tokio::join!`, so it never blocks the artists/tracks requests.
async fn analysis_phase(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    _ann: &Announcer<'_>,
) -> PhaseOutcome {
    let mut out = PhaseOutcome::default();
    let mut marked = false;
    while go_on(cancel, stop) {
        match analysis::run_batch_until(ctx, cancel, stop).await {
            Ok(b) => {
                let changed = b.done + b.failed > 0;
                if changed || !marked {
                    mark(ctx, _ann, Phase::Analysis, b).await;
                    marked = true;
                }
                out.report += b;
                if b.claimed == 0 || b.halted {
                    break;
                }
            }
            Err(e) => {
                out.errors += 1;
                batch_failed(ctx, Phase::Analysis, e).await;
                break;
            }
        }
    }
    out
}

/// What the mood and similarity phases did.
#[derive(Default)]
struct VibeOutcome {
    moods: PhaseOutcome,
    similarity: PhaseOutcome,
}

/// Steps 3c-3d (upstream `executeMoodPhase`, `executeSimilarityPhase`): the learned moods, then the matching state
/// and the vibe map. Library-wide passes with no per-track rows; each waits until nothing that feeds it is still
/// being made (`VibeInputs::settled`: embedding and analysis open, working or retrying). A failure is logged and
/// recorded and never stops the cycle: the moods failing does not keep similarity from running, as upstream.
/// An unavailable asset (no mood heads, no feel probe) turns only its own step off.
async fn vibe_phases(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    ann: &Announcer<'_>,
) -> VibeOutcome {
    let mut out = VibeOutcome::default();
    if !go_on(cancel, stop) {
        return out;
    }
    let inputs = match enrichment::vibe_inputs(&ctx.pool, true).await {
        Ok(i) => i,
        Err(e) => {
            out.moods.errors += 1;
            batch_failed(ctx, Phase::Mood, e).await;
            return out;
        }
    };
    if !inputs.settled {
        tracing::debug!(
            "moods and similarity wait: analysis or sound vectors are still being made"
        );
        return out;
    }
    out.moods = mood_phase(ctx, cancel, stop, ann).await;
    if go_on(cancel, stop) {
        // The one-time layout also waits for the artist genres the formula reads.
        out.similarity = similarity_phase(ctx, cancel, ann, inputs.artists_owed == 0).await;
    }
    out
}

async fn mood_phase(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    ann: &Announcer<'_>,
) -> PhaseOutcome {
    let mut out = PhaseOutcome::default();
    if !go_on(cancel, stop) {
        return out;
    }
    match moods::run_pass(ctx).await {
        Ok(MoodPass::Unavailable) => {
            tracing::debug!("mood heads unavailable; the mood step is off")
        }
        Ok(MoodPass::Ran { build, .. }) => {
            let n = usize::try_from(build.scored).unwrap_or(usize::MAX);
            let b = BatchReport {
                claimed: n,
                done: n,
                ..BatchReport::default()
            };
            mark(ctx, ann, Phase::Mood, b).await;
            out.report += b;
        }
        Err(e) => {
            out.errors += 1;
            batch_failed(ctx, Phase::Mood, e).await;
        }
    }
    out
}

async fn similarity_phase(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    ann: &Announcer<'_>,
    genres_settled: bool,
) -> PhaseOutcome {
    let mut out = PhaseOutcome::default();
    // A cancelled cycle (shutdown) leaves a layout to finish or stop on its own; the server stops it explicitly.
    let pass = tokio::select! {
        () = cancel.cancelled() => return out,
        pass = similarity::run_pass(ctx, genres_settled) => pass,
    };
    match pass {
        Ok(SimilarityPass::Unavailable) => {
            tracing::debug!("feel probe unavailable; the similarity step is off");
        }
        Ok(ran) => {
            let n = ran.worked();
            let b = BatchReport {
                claimed: n,
                done: n,
                ..BatchReport::default()
            };
            mark(ctx, ann, Phase::Similarity, b).await;
            out.report += b;
        }
        Err(e) => {
            out.errors += 1;
            batch_failed(ctx, Phase::Similarity, e).await;
        }
    }
    out
}

/// What the network phases did.
#[derive(Default)]
struct NetworkOutcome {
    artists: PhaseOutcome,
    tracks: PhaseOutcome,
}

/// Whether a network phase may run another batch: identification on and MusicBrainz, the
/// first source of both phases, not paused.
fn network_may_run(ctx: &IdentCtx, cancel: &CancellationToken, stop: &CancellationToken) -> bool {
    go_on(cancel, stop) && ctx.net.enabled() && ctx.net.paused_until(Source::MusicBrainz).is_none()
}

/// Step 4: the network phases, only while identification is on: one batch of 10 artists, then
/// tracks until none is left, each skipped while MusicBrainz is paused. The switch and the pause are checked
/// before every batch.
async fn network_phases(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    ann: &Announcer<'_>,
) -> NetworkOutcome {
    let artists = artists_phase(ctx, cancel, stop, ann).await;
    let tracks = tracks_phase(ctx, cancel, stop, ann).await;
    NetworkOutcome { artists, tracks }
}

/// Artist batches per cycle: one, as upstream (`enrichArtistsBatch` once per cycle), so the
/// tracks phase always runs and an artist that just ended (a paused source leaves it
/// `pending`) is not claimed again in the same cycle.
pub const ARTIST_BATCHES_PER_CYCLE: usize = 1;

async fn artists_phase(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    ann: &Announcer<'_>,
) -> PhaseOutcome {
    let mut out = PhaseOutcome::default();
    let mut marked = false;
    for _ in 0..ARTIST_BATCHES_PER_CYCLE {
        if !network_may_run(ctx, cancel, stop) {
            break;
        }
        match artist::run_batch_until(ctx, cancel, stop).await {
            Ok(b) => {
                let changed = b.done + b.failed > 0;
                if changed || !marked {
                    mark(ctx, ann, Phase::Artists, b).await;
                    marked = true;
                }
                out.report += b;
                if b.claimed == 0 {
                    break;
                }
            }
            Err(e) => {
                out.errors += 1;
                batch_failed(ctx, Phase::Artists, e).await;
                break;
            }
        }
    }
    out
}

async fn tracks_phase(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
    ann: &Announcer<'_>,
) -> PhaseOutcome {
    let mut out = PhaseOutcome::default();
    let mut marked = false;
    while network_may_run(ctx, cancel, stop) {
        match recording::run_batch_until(ctx, cancel, stop).await {
            Ok(b) => {
                let changed = b.done + b.failed > 0;
                if changed || !marked {
                    mark(ctx, ann, Phase::Tracks, b).await;
                    marked = true;
                }
                out.report += b;
                if b.claimed == 0 {
                    break;
                }
            }
            Err(e) => {
                out.errors += 1;
                batch_failed(ctx, Phase::Tracks, e).await;
                break;
            }
        }
    }
    out
}
