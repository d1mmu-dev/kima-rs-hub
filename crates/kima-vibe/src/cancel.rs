//! A cooperative stop flag for the long blocking passes (neighbour sweep, PCA start, layout epochs). The caller keeps
//! a clone and calls [`Cancel::cancel`]; the pass notices at its next checkpoint (a block of four sweep rows, a
//! subspace iteration, an optimisation epoch) and returns without a result. Upstream stops its layout by terminating
//! the worker thread; a Rust thread cannot be killed, so the passes poll.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
