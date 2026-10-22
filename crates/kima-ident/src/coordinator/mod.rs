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
use crate::stages::{BatchReport, fingerprint, recording};

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
        self.fingerprint.claimed + self.artists.claimed + self.tracks.claimed > 0
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

    // Steps 1-2: paused, halted or stopping end here and queue nothing.
    let st = enrichment::begin_cycle(pool).await.map_err(db)?;
    let mut report = CycleReport::default();
    if !st.may_run() {
        return Ok(report);
    }
    report.ran = true;

    let stop = CancellationToken::new();
    let phases = async {
        let (fp, net) = tokio::join!(
            fingerprint_phase(ctx, cancel, &stop),
            network_phases(ctx, cancel, &stop)
        );
        stop.cancel(); // ends the watcher
        (fp, net)
    };
    let ((fp, net), ()) = tokio::join!(phases, watch_state(pool, cancel, &stop));
    report.fingerprint = fp.report;
    report.artists = net.artists.report;
    report.tracks = net.tracks.report;
    report.errors = fp.errors + net.artists.errors + net.tracks.errors;
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
async fn batch_failed(ctx: &IdentCtx, phase: Phase, e: sqlx::Error) {
    warn!(error = %e, phase = phase.as_str(), "enrichment batch failed; retrying next cycle");
    // A sqlx error names the statement's failure, never a bound value, so no secret reaches
    // `last_error`.
    let msg = format!("{} phase: {e}", phase.as_str());
    if let Err(error) = enrichment::record_error(&ctx.pool, &msg).await {
        warn!(%error, "recording the enrichment error failed");
    }
}

/// Stamps the phase; `activity` when counts changed. A failure is logged only.
async fn mark(ctx: &IdentCtx, phase: Phase, activity: bool) {
    if let Err(error) = enrichment::set_phase(&ctx.pool, Some(phase), activity).await {
        warn!(%error, "recording the enrichment phase failed");
    }
}

/// Step 3: fingerprint batches until none is left. Local work: runs whatever the switch says.
async fn fingerprint_phase(
    ctx: &IdentCtx,
    cancel: &CancellationToken,
    stop: &CancellationToken,
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
                    mark(ctx, Phase::Fingerprint, changed).await;
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
) -> NetworkOutcome {
    let artists = artists_phase(ctx, cancel, stop).await;
    let tracks = tracks_phase(ctx, cancel, stop).await;
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
                    mark(ctx, Phase::Artists, changed).await;
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
) -> PhaseOutcome {
    let mut out = PhaseOutcome::default();
    let mut marked = false;
    while network_may_run(ctx, cancel, stop) {
        match recording::run_batch_until(ctx, cancel, stop).await {
            Ok(b) => {
                let changed = b.done + b.failed > 0;
                if changed || !marked {
                    mark(ctx, Phase::Tracks, changed).await;
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
