//! Process-wide admission policy, not an allocator or a hard RSS sandbox.
use super::OnnxError;
use std::sync::Mutex;

const MAX_SESSIONS: usize = 32;
/// Intra-op thread slots all ONNX sessions of the process may reserve together.
pub const MAX_THREAD_SLOTS: usize = 64;
const MAX_ESTIMATED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Largest host-memory admission budget accepted (64 GiB).
pub const MAX_HOST_BUDGET_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
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

/// Host-memory admission limit in bytes; 4 GiB unless changed at startup.
static LIMIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(MAX_ESTIMATED_BYTES);

/// Sets the process-wide host-memory admission limit (estimated bytes, see
/// [`super::ProviderConfig`] for how CPU and CUDA sessions are counted). Call before
/// loading sessions; it never evicts admitted sessions. Values below the default 4 GiB are
/// accepted (stricter), zero is ignored and values above [`MAX_HOST_BUDGET_BYTES`] are capped.
pub fn set_host_budget_bytes(bytes: u64) {
    if bytes > 0 {
        LIMIT.store(
            bytes.min(MAX_HOST_BUDGET_BYTES),
            std::sync::atomic::Ordering::SeqCst,
        );
    }
}

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
        self.reserve_scaled(sessions, threads, encoded_bytes, 4)
    }

    fn reserve_scaled(
        &mut self,
        sessions: usize,
        threads: usize,
        encoded_bytes: usize,
        host_copies: u64,
    ) -> Result<ResourceReservation, OnnxError> {
        let limit = LIMIT.load(std::sync::atomic::Ordering::SeqCst);
        let bytes = (encoded_bytes as u64)
            .checked_mul(host_copies)
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
            || bytes > limit.saturating_sub(self.bytes)
        {
            return Err(OnnxError::SessionBuilder(format!(
                "aggregate ONNX session/thread/memory admission budget exceeded: this pool needs \
                 {sessions} sessions, {threads} threads and ~{} MiB; in use {}/{MAX_SESSIONS} \
                 sessions, {}/{MAX_THREAD_SLOTS} threads, ~{}/{} MiB host budget (raise \
                 KIMA_ONNX_HOST_BUDGET_MIB or lower KIMA_CLAP_POOL_SIZE/KIMA_CLAP_INTRA_THREADS)",
                bytes.div_ceil(MIB),
                self.sessions,
                self.threads,
                self.bytes.div_ceil(MIB),
                limit / MIB,
            )));
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

    /// CPU sessions count four host copies of the encoded model; CUDA sessions one (weights
    /// and activations are in device memory, bounded by the CUDA EP's own limit).
    pub(super) fn acquire_for(
        sessions: usize,
        threads: usize,
        bytes: usize,
        cuda: bool,
    ) -> Result<Self, OnnxError> {
        BUDGET
            .lock()
            .map_err(|_| OnnxError::LockPoisoned)?
            .reserve_scaled(sessions, threads, bytes, if cuda { 1 } else { 4 })
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
