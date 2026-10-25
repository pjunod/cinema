//! One retained node-capacity observation job, shared by all selected owners.
//!
//! The blocking task owns only the configured output root. A cancelled HTTP
//! waiter never detaches it, drops its result, or starts a replacement while
//! the original observation is still running. This is service infrastructure;
//! per-media native acquisition still begins only after its own admission.

use std::time::{Duration, Instant};

use super::{AdmissionRefusal, NodeResourceRuntime};
use crate::resource_limits::admission::AdmissionObservation;

const REUSABLE_FOR: Duration = Duration::from_millis(250);

pub(super) struct CapacityObserver {
    cached: Option<AdmissionObservation>,
    pending: Option<tokio::task::JoinHandle<AdmissionObservation>>,
    failed: bool,
}

impl CapacityObserver {
    pub(super) fn new() -> Self {
        Self {
            cached: None,
            pending: None,
            failed: false,
        }
    }

    fn fresh(&self) -> Option<AdmissionObservation> {
        self.cached.filter(|sample| {
            Instant::now()
                .checked_duration_since(sample.measured_at)
                .is_some_and(|age| age <= REUSABLE_FOR)
        })
    }

    async fn join_original(
        &mut self,
        deadline: Instant,
    ) -> Result<AdmissionObservation, AdmissionRefusal> {
        let pending = self.pending.as_mut().ok_or(AdmissionRefusal::Unavailable)?;
        let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), pending)
            .await
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        self.pending.take();
        let sample = result.map_err(|_| {
            self.failed = true;
            AdmissionRefusal::Unavailable
        })?;
        self.cached = Some(sample);
        Ok(sample)
    }
}

impl NodeResourceRuntime {
    /// Refresh the actual configured node's capacity without permitting an
    /// HTTP cancellation to detach native observation. A returned epoch is
    /// still advisory; reservation rechecks it and every live ledger claim.
    pub async fn refresh_admission_capacity(
        &self,
        deadline: Instant,
    ) -> Result<u64, AdmissionRefusal> {
        if Instant::now() >= deadline {
            return Err(AdmissionRefusal::Unavailable);
        }
        let mut owner = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.capacity_observer.lock(),
        )
        .await
        .map_err(|_| AdmissionRefusal::Unavailable)?;
        if owner.failed {
            return Err(AdmissionRefusal::Unavailable);
        }
        let sample = if let Some(sample) = owner.fresh() {
            sample
        } else {
            if owner.pending.is_none() {
                let root = self
                    .rendition_root
                    .clone()
                    .ok_or(AdmissionRefusal::Unavailable)?;
                // Store the original JoinHandle before the first await. The
                // blocking closure captures no runtime/ledger/native owner.
                owner.pending = Some(tokio::task::spawn_blocking(move || {
                    crate::resource_limits::admission::snapshot(&root)
                }));
            }
            owner.join_original(deadline).await?
        };
        drop(owner);
        self.ledger
            .observe_capacity(sample.capacity, sample.measured_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    /// Cancelling a waiter releases only its async mutex guard. The original
    /// blocking observation is still registered and the next waiter joins it.
    /// This test never publishes a sample to a ledger or mints native proof.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_waiter_joins_original_observer_without_double_spawn() {
        let owner = Arc::new(tokio::sync::Mutex::new(CapacityObserver::new()));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let starts = Arc::new(AtomicUsize::new(0));
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        {
            let gate = Arc::clone(&gate);
            let starts = Arc::clone(&starts);
            let mut guard = owner.lock().await;
            guard.pending = Some(tokio::task::spawn_blocking(move || {
                starts.fetch_add(1, Ordering::SeqCst);
                let _ = entered.send(());
                let (lock, signal) = &*gate;
                let mut released = lock.lock().expect("test gate");
                while !*released {
                    let (next, wait) = signal
                        .wait_timeout(released, Duration::from_secs(5))
                        .expect("test gate wait");
                    assert!(!wait.timed_out(), "observer gate did not release");
                    released = next;
                }
                crate::resource_limits::admission::snapshot(std::path::Path::new(env!(
                    "CARGO_MANIFEST_DIR"
                )))
            }));
        }
        entered_rx.await.expect("original observer entered");
        let (waiting, waiting_rx) = tokio::sync::oneshot::channel();
        let first = {
            let owner = Arc::clone(&owner);
            tokio::spawn(async move {
                let mut guard = owner.lock().await;
                let _ = waiting.send(());
                guard
                    .join_original(Instant::now() + Duration::from_secs(5))
                    .await
            })
        };
        waiting_rx
            .await
            .expect("first waiter acquired original handle");
        first.abort();
        let _ = first.await;
        assert!(
            owner.lock().await.pending.is_some(),
            "original handle was detached"
        );
        {
            let (lock, signal) = &*gate;
            *lock.lock().expect("test gate") = true;
            signal.notify_all();
        }
        let sample = owner
            .lock()
            .await
            .join_original(Instant::now() + Duration::from_secs(5))
            .await
            .expect("second waiter joins original sample");
        assert!(sample.measured_at <= Instant::now());
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(owner.lock().await.pending.is_none());
    }
}
