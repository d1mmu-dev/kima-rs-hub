//! Process-wide admission policy, not an allocator or a hard RSS sandbox.
use super::OnnxError;
use std::sync::Mutex;

const MAX_SESSIONS: usize = 32;
const MAX_THREAD_SLOTS: usize = 64;
const MAX_ESTIMATED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const SESSION_OVERHEAD: u64 = 16 * 1024 * 1024;

#[derive(Debug, Default)]
struct Budget {
    sessions: usize,
    threads: usize,
    bytes: u64,
}

static BUDGET: Mutex<Budget> = Mutex::new(Budget {
    sessions: 0,
    threads: 0,
    bytes: 0,
});

#[derive(Debug)]
pub(super) struct ResourceReservation {
    sessions: usize,
    threads: usize,
    bytes: u64,
}

impl Budget {
    fn reserve(
        &mut self,
        sessions: usize,
        threads: usize,
        encoded_bytes: usize,
    ) -> Result<ResourceReservation, OnnxError> {
        let bytes = (encoded_bytes as u64)
            .checked_mul(4)
            .and_then(|n| n.checked_add(SESSION_OVERHEAD))
            .and_then(|n| n.checked_mul(sessions as u64));
        let threads = sessions.checked_mul(threads);
        let Some((bytes, threads)) = bytes.zip(threads) else {
            return Err(OnnxError::SessionBuilder(
                "resource estimate overflow".into(),
            ));
        };
        if sessions > MAX_SESSIONS.saturating_sub(self.sessions)
            || threads > MAX_THREAD_SLOTS.saturating_sub(self.threads)
            || bytes > MAX_ESTIMATED_BYTES.saturating_sub(self.bytes)
        {
            return Err(OnnxError::SessionBuilder(
                "aggregate ONNX session/thread/memory admission budget exceeded".into(),
            ));
        }
        self.sessions += sessions;
        self.threads += threads;
        self.bytes += bytes;
        Ok(ResourceReservation {
            sessions,
            threads,
            bytes,
        })
    }
}

impl ResourceReservation {
    pub(super) fn acquire(
        sessions: usize,
        threads: usize,
        bytes: usize,
    ) -> Result<Self, OnnxError> {
        BUDGET
            .lock()
            .map_err(|_| OnnxError::LockPoisoned)?
            .reserve(sessions, threads, bytes)
    }
}

impl Drop for ResourceReservation {
    fn drop(&mut self) {
        let mut budget = BUDGET.lock().unwrap_or_else(|e| e.into_inner());
        budget.sessions -= self.sessions;
        budget.threads -= self.threads;
        budget.bytes -= self.bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_admission_rejects_overflow_and_aggregate_excess() {
        // Use the actual process budget, without allocating multi-GB artifacts.
        assert!(ResourceReservation::acquire(1, 1, usize::MAX).is_err());
        assert!(ResourceReservation::acquire(1, 1, 1024 * 1024 * 1024).is_err());
        let reservation = ResourceReservation::acquire(1, 1, 600 * 1024 * 1024).unwrap();
        assert!(ResourceReservation::acquire(1, 1, 600 * 1024 * 1024).is_err());
        drop(reservation);
        assert!(ResourceReservation::acquire(1, 1, 600 * 1024 * 1024).is_ok());
    }
}
