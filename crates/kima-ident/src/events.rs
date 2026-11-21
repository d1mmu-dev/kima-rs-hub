//! `enrichment:progress` events (spec §17, D-112): published on the M0 bus to every user, at
//! most once a second. A call inside the window is not lost: the window's end publishes one
//! trailing event with the latest values.
//!
//! [`ProgressEvents::notify`] never awaits and never blocks on the bus (a broadcast send); it
//! holds a plain mutex only for a few field updates. At most one trailing task is pending at a
//! time; it holds only a weak reference, so it neither keeps the throttle alive nor, being a
//! plain sleeping task, the runtime past shutdown.
//!
//! The publisher also carries upstream's `clearMixCache` points (D-260): [`ProgressEvents::completion_reached`]
//! sends one "drop the cached mixes" signal the first time core enrichment is complete and the first time the whole
//! pipeline is, per process; [`ProgressEvents::subscribe_mix_clears`] receives them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use kima_core::events::{Audience, Event, EventBus};
use kima_db::enrichment::{Phase, Status};
use tokio::sync::broadcast;
use tokio::time::Instant;

/// The shortest gap between two events.
pub const MIN_GAP: Duration = Duration::from_secs(1);

/// The event's `type`.
pub const TYPE: &str = "enrichment:progress";

/// The throttled publisher. Cheap to clone; clones share the window.
#[derive(Clone)]
pub struct ProgressEvents {
    shared: Arc<Shared>,
}

struct Shared {
    bus: EventBus,
    window: Mutex<Window>,
    /// The "drop the cached mixes" signal (upstream `clearMixCache`).
    mix_clears: broadcast::Sender<()>,
    /// Upstream `coreCacheCleared` / `fullCacheCleared`: each point clears once per process.
    core_cleared: AtomicBool,
    full_cleared: AtomicBool,
}

#[derive(Default)]
struct Window {
    /// When the last event was published.
    last: Option<Instant>,
    /// The values the trailing event will carry; `Some` exactly while a trailing task is pending.
    pending: Option<(Option<Phase>, Status)>,
}

impl ProgressEvents {
    pub fn new(bus: EventBus) -> Self {
        Self {
            shared: Arc::new(Shared {
                bus,
                window: Mutex::new(Window::default()),
                mix_clears: broadcast::channel(4).0,
                core_cleared: AtomicBool::new(false),
                full_cleared: AtomicBool::new(false),
            }),
        }
    }

    /// Announces the pipeline's phase and status: now when the last event is at least
    /// [`MIN_GAP`] old, else as the window's trailing event (the latest call's values win).
    pub fn notify(&self, phase: Option<Phase>, status: Status) {
        let now = Instant::now();
        let mut w = self.shared.lock();
        if w.pending.is_some() {
            w.pending = Some((phase, status));
            return;
        }
        let due = w.last.map(|last| last + MIN_GAP).filter(|due| *due > now);
        let Some(due) = due else {
            w.last = Some(now);
            self.shared.publish(phase, status);
            return;
        };
        // Outside a runtime nothing can wait for the window: publish now rather than drop it.
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            w.last = Some(now);
            self.shared.publish(phase, status);
            return;
        };
        w.pending = Some((phase, status));
        drop(w);
        let weak = Arc::downgrade(&self.shared);
        rt.spawn(trailing(weak, due));
    }
}

impl ProgressEvents {
    /// A receiver of the "drop the cached mixes" signal: one `()` per [`Self::completion_reached`] point.
    pub fn subscribe_mix_clears(&self) -> broadcast::Receiver<()> {
        self.shared.mix_clears.subscribe()
    }

    /// Upstream `clearMixCache("core")` when `core` (`progress.coreComplete`) and `clearMixCache("full")` when
    /// `full` (`isFullyComplete`), each the first time it holds in this process (`coreCacheCleared`,
    /// `fullCacheCleared`, reset only when the enrichment worker starts).
    pub fn completion_reached(&self, core: bool, full: bool) {
        let s = &self.shared;
        for (reached, cleared, point) in [
            (core, &s.core_cleared, "core"),
            (full, &s.full_cleared, "full"),
        ] {
            if reached && !cleared.swap(true, Ordering::AcqRel) {
                tracing::info!("dropping the cached mixes after {point} enrichment complete");
                // No receiver is not an error.
                let _ = s.mix_clears.send(());
            }
        }
    }

    /// Both points have cleared: a cycle need not work out the progress any more.
    pub fn mix_clears_done(&self) -> bool {
        self.shared.core_cleared.load(Ordering::Acquire)
            && self.shared.full_cleared.load(Ordering::Acquire)
    }
}

/// The window's trailing event.
async fn trailing(weak: Weak<Shared>, due: Instant) {
    tokio::time::sleep_until(due).await;
    let Some(shared) = weak.upgrade() else {
        return;
    };
    let mut w = shared.lock();
    if let Some((phase, status)) = w.pending.take() {
        w.last = Some(Instant::now());
        shared.publish(phase, status);
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Window> {
        self.window.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `{type, phase, status}` to every user. Only the two enums' names: no error text, no
    /// path, no secret.
    fn publish(&self, phase: Option<Phase>, status: Status) {
        self.bus.publish(Event {
            audience: Audience::All,
            data: serde_json::json!({
                "type": TYPE,
                "phase": phase.map(Phase::as_str),
                "status": status.as_str(),
            }),
        });
    }
}
