//! Store-free serving authority derived from the passive Raft quorum proof.
//!
//! Readiness and media teardown cannot wait for a fresh Store call after a
//! partition: that call owns a multi-second timeout, while the quorum
//! watermark is already a short monotonic lease refreshed by the replication
//! monitor. This projection turns that lease into one process-wide watch.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use plurx_core::cluster::migration::status::PassiveRaftMetrics;

const SERVING_FENCE_POLL: Duration = Duration::from_millis(25);

pub(crate) const SERVING_FENCED_MESSAGE: &str = "this node has lost quorum serving authority";
pub(crate) const SERVING_FENCED_JSON: &str =
    r#"{"code":"serving_fenced","message":"this node has lost quorum serving authority"}"#;

/// Monotonic serving authority. `ready` may recover, but a loss generation
/// never does: a consumer admitted under generation N must retire when it
/// observes any generation greater than N, even if a fast loss/recovery was
/// coalesced into one watch notification before that consumer was scheduled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServingState {
    pub(crate) ready: bool,
    pub(crate) loss_generation: u64,
}

impl ServingState {
    pub(crate) fn authority_lost_since(self, admitted_generation: u64) -> bool {
        !self.ready || self.loss_generation != admitted_generation
    }
}

/// Storage-independent HTTP behavior shared by the daemon router and the
/// separate-process partition proof. A response here is final; `None` means
/// the request may continue to its ordinary handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServingHttpPolicy {
    pub(crate) status: u16,
    pub(crate) content_type: &'static str,
    pub(crate) body: &'static str,
    pub(crate) retry_after: bool,
}

#[derive(Clone)]
pub(crate) struct ServingFence {
    metrics: PassiveRaftMetrics,
    quorum_managed: bool,
    ready: Arc<AtomicBool>,
    loss_generation: Arc<AtomicU64>,
    state: tokio::sync::watch::Sender<ServingState>,
}

impl ServingFence {
    pub(crate) fn new(metrics: PassiveRaftMetrics) -> Self {
        let quorum_managed = metrics.snapshot().watermark_source;
        let ready = !quorum_managed;
        let initial = ServingState {
            ready,
            loss_generation: 0,
        };
        let (state, _) = tokio::sync::watch::channel(initial);
        Self {
            metrics,
            quorum_managed,
            ready: Arc::new(AtomicBool::new(ready)),
            loss_generation: Arc::new(AtomicU64::new(0)),
            state,
        }
    }

    pub(crate) fn is_quorum_managed(&self) -> bool {
        self.quorum_managed
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<ServingState> {
        self.state.subscribe()
    }

    pub(crate) fn requires_authority(path: &str) -> bool {
        path.contains("/files/")
            || path.contains("/hls/")
            || path.ends_with("/stream.mp4")
            || path.ends_with("/offline-packages")
            || path.contains("/offline/packages/")
            || path.contains("/offline/media/")
            || path.ends_with("/direct")
            || path.starts_with("/library/parts/")
            || path.ends_with("/content")
            || path.ends_with("/publication")
            || path.contains("/publication/")
            || path.contains("/subs/")
            || path.contains("/overlay")
            || path.contains("/images/")
            || path.ends_with("/photo")
            || path.starts_with("/library/metadata/")
            || path == "/photo/:/transcode"
    }

    pub(crate) fn http_policy(&self, path: &str) -> Option<ServingHttpPolicy> {
        match path {
            "/healthz" => Some(ServingHttpPolicy {
                status: 200,
                content_type: "text/plain",
                body: "ok\n",
                retry_after: false,
            }),
            "/readyz" if self.is_ready() => Some(ServingHttpPolicy {
                status: 200,
                content_type: "text/plain",
                body: "ready\n",
                retry_after: false,
            }),
            "/readyz" => Some(ServingHttpPolicy {
                status: 503,
                content_type: "text/plain",
                body: "quorum unavailable\n",
                retry_after: false,
            }),
            path if Self::requires_authority(path) && !self.is_ready() => Some(ServingHttpPolicy {
                status: 503,
                content_type: "application/json",
                body: SERVING_FENCED_JSON,
                retry_after: true,
            }),
            _ => None,
        }
    }

    fn publish(&self, desired: bool) {
        let previous = self.ready.swap(desired, Ordering::AcqRel);
        if previous == desired {
            return;
        }
        let loss_generation = if previous && !desired {
            self.loss_generation.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            self.loss_generation.load(Ordering::Acquire)
        };
        let _ = self.state.send(ServingState {
            ready: desired,
            loss_generation,
        });
    }

    fn refresh(&self) {
        let snapshot = self.metrics.snapshot();
        let desired = !self.quorum_managed
            || (snapshot.watermark_valid
                && snapshot
                    .watermark
                    .is_some_and(|watermark| watermark.apply_lag_entries == Some(0)));
        let previous = self.is_ready();
        self.publish(desired);
        if previous != desired {
            if desired {
                tracing::info!("serving authority recovered from a fresh quorum watermark");
            } else {
                tracing::warn!("serving authority expired; mutable media is self-fenced");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn validation_set_ready(&self, ready: bool) {
        self.publish(ready);
    }

    pub(crate) async fn monitor_loop(self, shutdown: tokio_util::sync::CancellationToken) {
        loop {
            self.refresh();
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(SERVING_FENCE_POLL) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::cluster::migration::status::ReplicationMonitor;

    #[test]
    fn sqlite_is_ready_without_a_quorum_sampler() {
        let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        assert!(fence.is_ready());
        assert!(!fence.is_quorum_managed());
    }

    #[test]
    fn loss_generation_survives_a_coalesced_recovery() {
        let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let mut state = fence.subscribe();
        let admitted = state.borrow_and_update().loss_generation;
        fence.validation_set_ready(false);
        fence.validation_set_ready(true);

        let recovered = *state.borrow_and_update();
        assert!(recovered.ready);
        assert!(recovered.authority_lost_since(admitted));
    }
}
