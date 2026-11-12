//! Two lanes for Apple requests (`podcastCatalog.ts:72-82`): what a person is waiting on goes
//! first; work that can wait (category rows, the category fill-in) waits while any such request
//! is on its way. A background load a person joins is raised to the first lane.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Default)]
pub(crate) struct Lanes {
    /// Interactive loads on their way (raised ones included).
    interactive: Mutex<usize>,
    changed: Notify,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Lanes {
    /// One more interactive load.
    pub(crate) fn enter(&self) {
        *lock(&self.interactive) += 1;
        self.changed.notify_waiters();
    }

    /// One interactive load less.
    pub(crate) fn leave(&self) {
        let mut n = lock(&self.interactive);
        *n = n.saturating_sub(1);
        drop(n);
        self.changed.notify_waiters();
    }

    /// Raises a background load (`flag` false → true) into the first lane; `false` when it
    /// already was interactive.
    pub(crate) fn raise(&self, flag: &AtomicBool) -> bool {
        if flag.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.enter();
        true
    }

    /// Waits until no interactive load is on its way, or `raised` is set.
    pub(crate) async fn wait_turn(&self, raised: &AtomicBool) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if raised.load(Ordering::SeqCst) || *lock(&self.interactive) == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// An interactive turn held for as long as it lives.
pub(crate) struct Turn<'a>(&'a Lanes);

impl<'a> Turn<'a> {
    pub(crate) fn interactive(lanes: &'a Lanes) -> Turn<'a> {
        lanes.enter();
        Turn(lanes)
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.leave();
    }
}
