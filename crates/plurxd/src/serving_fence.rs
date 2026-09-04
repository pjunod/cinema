//! Store-free serving authority derived from the passive Raft quorum proof.
//!
//! Readiness and media teardown cannot wait for a fresh Store call after a
//! partition: that call owns a multi-second timeout, while the quorum
//! watermark is already a short monotonic lease refreshed by the replication
//! monitor. This projection turns that lease into one process-wide watch.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use plurx_core::cluster::migration::status::PassiveRaftMetrics;

const SERVING_FENCE_POLL: Duration = Duration::from_millis(25);

pub(crate) const SERVING_FENCED_MESSAGE: &str = "this node has lost quorum serving authority";
pub(crate) const SERVING_FENCED_JSON: &str =
    r#"{"code":"serving_fenced","message":"this node has lost quorum serving authority"}"#;
pub(crate) const RESTART_DRAIN_JSON: &str = r#"{"code":"restart_drain_active","message":"this node is preparing for restart and is not accepting new mutable media work"}"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RestartDrainStatus {
    pub(crate) new_admissions_blocked: bool,
    pub(crate) admissions_in_flight: u64,
    pub(crate) expires_at_unix_ms: Option<u64>,
    pub(crate) drained: bool,
}

#[derive(Default)]
struct RestartDrainState {
    expires_at: Option<tokio::time::Instant>,
    expires_at_unix_ms: Option<u64>,
}

#[derive(Default)]
struct RestartDrain {
    state: tokio::sync::Mutex<RestartDrainState>,
    admissions: AtomicU64,
    changed: tokio::sync::Notify,
}

pub(crate) struct RestartAdmission {
    drain: Arc<RestartDrain>,
}

impl Drop for RestartAdmission {
    fn drop(&mut self) {
        self.drain.admissions.fetch_sub(1, Ordering::AcqRel);
        self.drain.changed.notify_waiters();
    }
}

/// Monotonic serving authority. `ready` may recover, but a loss generation
/// never does: a consumer admitted under generation N must retire when it
/// observes any generation greater than N, even if a fast loss/recovery was
/// coalesced into one watch notification before that consumer was scheduled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ServingState {
    pub(crate) ready: bool,
    pub(crate) loss_generation: u64,
}

/// Cloneable, read-only view of the synchronous authority projection.
/// Publication paths use this directly rather than waiting for the teardown
/// watch consumer to mirror a loss into subsystem-local state.
#[derive(Clone)]
pub(crate) struct ServingAuthority {
    ready: Arc<AtomicBool>,
    loss_generation: Arc<AtomicU64>,
    loss_transitions_pending: Arc<AtomicU64>,
    transition: Arc<tokio::sync::RwLock<()>>,
}

impl ServingAuthority {
    pub(crate) fn always_ready() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            loss_generation: Arc::new(AtomicU64::new(0)),
            loss_transitions_pending: Arc::new(AtomicU64::new(0)),
            transition: Arc::new(tokio::sync::RwLock::new(())),
        }
    }

    pub(crate) fn state(&self) -> ServingState {
        loop {
            let before = self.loss_generation.load(Ordering::Acquire);
            let pending_before = self.loss_transitions_pending.load(Ordering::Acquire);
            let ready = self.ready.load(Ordering::Acquire);
            let after = self.loss_generation.load(Ordering::Acquire);
            let pending_after = self.loss_transitions_pending.load(Ordering::Acquire);
            if before == after && pending_before == pending_after {
                return ServingState {
                    ready: ready && pending_after == 0,
                    loss_generation: after,
                };
            }
        }
    }

    pub(crate) fn admit(&self) -> Option<u64> {
        let state = self.state();
        state.ready.then_some(state.loss_generation)
    }

    pub(crate) fn is_current(&self, admitted_generation: u64) -> bool {
        !self.state().authority_lost_since(admitted_generation)
    }

    /// Serialize an EOF-side mutation before or after the synchronous quorum
    /// loss transition. The returned read guard must be held across the exact
    /// commit; a loss publisher takes the write side before changing either
    /// atomic, so no commit can straddle the authority boundary.
    pub(crate) async fn commit_guard_before(
        &self,
        admitted_generation: u64,
        deadline: std::time::Instant,
    ) -> Option<tokio::sync::OwnedRwLockReadGuard<()>> {
        let guard = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Arc::clone(&self.transition).read_owned(),
        )
        .await
        .ok()?;
        self.is_current(admitted_generation).then_some(guard)
    }
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
    quorum_managed: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    loss_generation: Arc<AtomicU64>,
    loss_transitions_pending: Arc<AtomicU64>,
    transition: Arc<tokio::sync::RwLock<()>>,
    state: tokio::sync::watch::Sender<ServingState>,
    restart_drain: Arc<RestartDrain>,
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
        let transition = Arc::new(tokio::sync::RwLock::new(()));
        Self {
            metrics,
            quorum_managed: Arc::new(AtomicBool::new(quorum_managed)),
            ready: Arc::new(AtomicBool::new(ready)),
            loss_generation: Arc::new(AtomicU64::new(0)),
            loss_transitions_pending: Arc::new(AtomicU64::new(0)),
            transition,
            state,
            restart_drain: Arc::new(RestartDrain::default()),
        }
    }

    pub(crate) fn is_quorum_managed(&self) -> bool {
        self.quorum_managed.load(Ordering::Acquire)
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
            && self.loss_transitions_pending.load(Ordering::Acquire) == 0
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<ServingState> {
        self.state.subscribe()
    }

    pub(crate) fn authority(&self) -> ServingAuthority {
        ServingAuthority {
            ready: Arc::clone(&self.ready),
            loss_generation: Arc::clone(&self.loss_generation),
            loss_transitions_pending: Arc::clone(&self.loss_transitions_pending),
            transition: Arc::clone(&self.transition),
        }
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
            || path.ends_with("/live-tv/channels")
            || path == "/_internal/v1/live-tv/snapshot"
    }

    /// Routes whose successful handler may admit new process-local media
    /// work. Existing segment/control requests intentionally remain outside
    /// this set so a preparation drains rather than interrupts them.
    pub(crate) fn starts_mutable_media(method: &str, path: &str) -> bool {
        (method == "POST"
            && (path.ends_with("/hls/sessions")
                || path.ends_with("/offline-packages")
                || path.ends_with("/publication")))
            || (method == "GET"
                && (path.ends_with("/hls/start")
                    || path.ends_with("/stream.mp4")
                    || path.ends_with("/direct")
                    || path.starts_with("/library/parts/")
                    || path == "/photo/:/transcode"))
    }

    /// Linearize one new admission against restart preparation.
    pub(crate) async fn try_restart_admission(&self) -> Option<RestartAdmission> {
        let mut state = self.restart_drain.state.lock().await;
        expire_restart_drain(&mut state);
        if state.expires_at.is_some() {
            return None;
        }
        self.restart_drain.admissions.fetch_add(1, Ordering::AcqRel);
        Some(RestartAdmission {
            drain: Arc::clone(&self.restart_drain),
        })
    }

    /// Fence new local work only until the already-committed replicated lease
    /// expires. Returning `false` means consensus consumed the whole lease and
    /// no local admission was fenced.
    pub(crate) async fn begin_restart_preparation_until(&self, expires_at_unix_ms: u64) -> bool {
        let mut state = self.restart_drain.state.lock().await;
        let now_unix_ms = unix_ms();
        let Some(remaining_ms) = expires_at_unix_ms.checked_sub(now_unix_ms) else {
            state.expires_at = None;
            state.expires_at_unix_ms = None;
            return false;
        };
        if remaining_ms == 0 {
            state.expires_at = None;
            state.expires_at_unix_ms = None;
            return false;
        }
        let now = tokio::time::Instant::now();
        state.expires_at = Some(now + Duration::from_millis(remaining_ms));
        state.expires_at_unix_ms = Some(expires_at_unix_ms);
        true
    }

    /// Wait until every admission that won before the drain flag has either
    /// failed or published its owned-work lifetime. Once this returns, the
    /// caller can sample the owned registries without a start slipping
    /// entirely between that sample and the drain linearization point.
    pub(crate) async fn wait_for_restart_admissions(&self) {
        loop {
            if self.restart_drain.admissions.load(Ordering::Acquire) == 0 {
                return;
            }
            let changed = self.restart_drain.changed.notified();
            if self.restart_drain.admissions.load(Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }

    pub(crate) async fn cancel_restart_preparation(
        &self,
        active_sessions: usize,
    ) -> RestartDrainStatus {
        let mut state = self.restart_drain.state.lock().await;
        state.expires_at = None;
        state.expires_at_unix_ms = None;
        self.restart_drain.changed.notify_waiters();
        restart_drain_status(&self.restart_drain, &state, active_sessions)
    }

    pub(crate) async fn restart_drain_status(&self, active_sessions: usize) -> RestartDrainStatus {
        let mut state = self.restart_drain.state.lock().await;
        expire_restart_drain(&mut state);
        restart_drain_status(&self.restart_drain, &state, active_sessions)
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

    async fn publish(&self, desired: bool) {
        if self.ready.load(Ordering::Acquire) == desired {
            return;
        }
        // Announce authority loss before waiting for an EOF commit that
        // already owns the transition read side. New admissions fail closed
        // immediately, while the write lock still serializes the generation
        // change after every commit admitted before the loss was observed.
        // Count rather than boolean so overlapping publishers cannot clear
        // another publisher's pending fence.
        let _pending_loss = (self.ready.load(Ordering::Acquire) && !desired)
            .then(|| PendingLossTransition::new(Arc::clone(&self.loss_transitions_pending)));
        let _transition = self.transition.write().await;
        let previous = self.ready.swap(desired, Ordering::AcqRel);
        if previous == desired {
            return;
        }
        let loss_generation = if previous && !desired {
            self.loss_generation.fetch_add(1, Ordering::AcqRel) + 1
        } else {
            self.loss_generation.load(Ordering::Acquire)
        };
        self.state.send_replace(ServingState {
            ready: desired,
            loss_generation,
        });
    }

    async fn refresh(&self) {
        let snapshot = self.metrics.snapshot();
        let authority_is_current = snapshot.watermark_valid
            && snapshot.watermark.is_some_and(|watermark| {
                if snapshot.watermark_requires_local_binding {
                    snapshot.local_source && watermark.apply_lag_entries == Some(0)
                } else {
                    !snapshot.local_source && watermark.apply_lag_entries.is_none()
                }
            });
        let desired = !self.is_quorum_managed() || authority_is_current;
        let previous = self.is_ready();
        self.publish(desired).await;
        if previous != desired {
            if desired {
                tracing::info!("serving authority recovered from a fresh quorum watermark");
            } else {
                tracing::warn!("serving authority expired; mutable media is self-fenced");
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn validation_set_ready(&self, ready: bool) {
        self.quorum_managed.store(true, Ordering::Release);
        self.publish(ready).await;
    }

    pub(crate) async fn monitor_loop(self, shutdown: tokio_util::sync::CancellationToken) {
        loop {
            self.refresh().await;
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(SERVING_FENCE_POLL) => {}
            }
        }
    }
}

fn expire_restart_drain(state: &mut RestartDrainState) {
    if state.expires_at.is_some_and(|expires_at| {
        expires_at <= tokio::time::Instant::now()
            || state
                .expires_at_unix_ms
                .is_some_and(|expires_at_unix_ms| expires_at_unix_ms <= unix_ms())
    }) {
        state.expires_at = None;
        state.expires_at_unix_ms = None;
    }
}

fn restart_drain_status(
    drain: &RestartDrain,
    state: &RestartDrainState,
    active_sessions: usize,
) -> RestartDrainStatus {
    let admissions_in_flight = drain.admissions.load(Ordering::Acquire);
    RestartDrainStatus {
        new_admissions_blocked: state.expires_at.is_some(),
        admissions_in_flight,
        expires_at_unix_ms: state.expires_at_unix_ms,
        drained: active_sessions == 0 && admissions_in_flight == 0,
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

struct PendingLossTransition {
    pending: Arc<AtomicU64>,
}

impl PendingLossTransition {
    fn new(pending: Arc<AtomicU64>) -> Self {
        pending.fetch_add(1, Ordering::AcqRel);
        Self { pending }
    }
}

impl Drop for PendingLossTransition {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::AcqRel);
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

    #[tokio::test]
    async fn loss_generation_survives_a_coalesced_recovery() {
        let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let mut state = fence.subscribe();
        let admitted = state.borrow_and_update().loss_generation;
        fence.validation_set_ready(false).await;
        fence.validation_set_ready(true).await;

        let recovered = *state.borrow_and_update();
        assert!(recovered.ready);
        assert!(recovered.authority_lost_since(admitted));
    }

    #[tokio::test]
    async fn authority_state_is_retained_before_the_first_subscriber() {
        let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        fence.validation_set_ready(false).await;
        fence.validation_set_ready(true).await;

        let state = *fence.subscribe().borrow();
        assert!(state.ready);
        assert_eq!(state.loss_generation, 1);
        assert!(state.authority_lost_since(0));
    }

    #[tokio::test]
    async fn restart_drain_linearizes_against_in_flight_admission() {
        let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let admission = fence
            .try_restart_admission()
            .await
            .expect("admit before preparation");
        assert!(
            fence
                .begin_restart_preparation_until(unix_ms().saturating_add(60_000))
                .await
        );
        let preparing = fence.restart_drain_status(0).await;
        assert!(preparing.new_admissions_blocked);
        assert_eq!(preparing.admissions_in_flight, 1);
        assert!(!preparing.drained);
        assert!(fence.try_restart_admission().await.is_none());

        let waiting = {
            let fence = fence.clone();
            tokio::spawn(async move { fence.wait_for_restart_admissions().await })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(admission);
        waiting.await.expect("admission settlement waiter");
        let drained = fence.restart_drain_status(0).await;
        assert!(drained.drained);
        assert_eq!(drained.admissions_in_flight, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn restart_drain_expires_and_accepts_new_work_again() {
        let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        assert!(
            fence
                .begin_restart_preparation_until(unix_ms().saturating_add(60_000))
                .await
        );
        assert!(fence.try_restart_admission().await.is_none());
        tokio::time::advance(Duration::from_secs(61)).await;
        assert!(fence.try_restart_admission().await.is_some());
        assert!(!fence.restart_drain_status(0).await.new_admissions_blocked);
    }

    #[tokio::test]
    async fn restart_drain_advertises_exactly_the_replicated_bound() {
        let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let replicated_expiry = unix_ms().saturating_add(60_000);
        assert!(
            fence
                .begin_restart_preparation_until(replicated_expiry)
                .await
        );
        let status = fence.restart_drain_status(0).await;
        assert_eq!(status.expires_at_unix_ms, Some(replicated_expiry));
        assert!(!fence.begin_restart_preparation_until(unix_ms()).await);
    }

    #[tokio::test]
    async fn pending_loss_blocks_new_admission_before_existing_commit_finishes() {
        let fence = ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let authority = fence.authority();
        let admitted = authority.admit().expect("initial authority");
        let commit = authority
            .commit_guard_before(admitted, std::time::Instant::now() + Duration::from_secs(1))
            .await
            .expect("commit guard");

        let publisher = fence.clone();
        let loss = tokio::spawn(async move { publisher.validation_set_ready(false).await });
        for _ in 0..100 {
            if !fence.is_ready() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(!fence.is_ready());
        assert!(authority.admit().is_none());
        assert!(!authority.is_current(admitted));

        drop(commit);
        loss.await.expect("loss publisher");
        assert_eq!(fence.authority().state().loss_generation, 1);
    }
}
