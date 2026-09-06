//! Cluster placement transport and liveness for capability-authenticated HLS.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderName, Response, StatusCode};
use futures_util::{future::BoxFuture, stream, StreamExt};
use plurx_core::cluster::membership::MembershipManager;
use plurx_core::domain::{
    MediaSessionActivation, MediaSessionEnd, MediaSessionProjectionCompletion, MediaSessionRenewal,
    MediaSessionRoute, MediaSessionTakeover, MediaSessionTakeoverCursor, OwnedMediaSessionLease,
    MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS, MEDIA_SESSION_PUBLICATION_BLOCKED,
};
use plurx_core::error::StoreError;
use plurx_core::store::Store;
use plurx_core::transcode::OutputGrade;
use serde::{Deserialize, Serialize};

use crate::http::peer_transport::{
    deadline_after, PeerAuthMode, PeerResponse, PeerTransport, PeerTransportError,
};
use crate::media_pool::MediaOfferRequest;
use crate::state::AppState;
use crate::transcode::{
    ClusterReplacementGuard, SessionAdoptionToken, SessionKind, SessionRequest,
    SessionTakeoverStart, StartInfo, TranscodeManager,
};

pub(crate) const START_PATH: &str = "/internal/cluster/media/sessions/start";
pub(crate) const ACTIVATE_PATH: &str = "/internal/cluster/media/sessions/activate";
pub(crate) const ABORT_PATH: &str = "/internal/cluster/media/sessions/abort";
pub(crate) const RELAY_PATH: &str = "/internal/cluster/media/sessions/relay";
pub(crate) const CONTROL_PATH: &str = "/internal/cluster/media/sessions/control";
pub(crate) const MAX_CONTROL_REQUEST_BYTES: usize = 96 * 1024;
pub(crate) const MAX_ACTIVATION_REQUEST_BYTES: usize = 128 * 1024;
pub(crate) const REMOTE_START_OWNERSHIP_HEADER: &str = "x-plurx-start-ownership";
pub(crate) const REMOTE_START_OWNERSHIP_V1: &str = "created-v1";

const MAX_START_RESPONSE_BYTES: usize = 128 * 1024;
pub(crate) const START_DEADLINE: Duration = Duration::from_secs(50);
pub(crate) const OWNER_ASSIGNMENT_DEADLINE: Duration = Duration::from_secs(3);
pub(crate) const ACTIVATION_STORE_DEADLINE: Duration = Duration::from_secs(3);
const ABORT_DEADLINE: Duration = Duration::from_secs(5);
/// Post-header streaming is deliberately disjoint from the request's
/// preparation budget. A media lookup may spend almost its whole request
/// deadline before headers exist; every admitted local or relayed body then
/// gets this one bounded lifetime.
pub(crate) const MAX_ADMITTED_MEDIA_BODY_LIFETIME: Duration = Duration::from_secs(300);
/// Longest authenticated public/relay resource envelope before response
/// headers, including accepted inter-node clock disagreement. This is the
/// playlist ceiling; segment and short-control envelopes are smaller.
pub(crate) const MAX_ADMITTED_MEDIA_PREHEADER_LIFETIME: Duration = Duration::from_secs(
    RELAY_PLAYLIST_MAX_LIFETIME.as_secs() + RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE.as_secs(),
);
/// Once authoritative Store state is terminal, this outlives both the full
/// pre-header envelope and body lifetime of every response admitted under the
/// predecessor generation. Exact control projection normally settles
/// immediately; this is the crash/partition proof boundary.
pub(crate) const TERMINAL_PROJECTION_SAFETY_WINDOW: Duration =
    Duration::from_millis(MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS as u64);
const _: () = assert!(
    TERMINAL_PROJECTION_SAFETY_WINDOW.as_secs()
        == MAX_ADMITTED_MEDIA_PREHEADER_LIFETIME.as_secs()
            + MAX_ADMITTED_MEDIA_BODY_LIFETIME.as_secs()
            + 10
);
/// Once the downstream is actively polling, a local reader or remote owner
/// that produces no next body item for this long is failed. This is an
/// upstream no-progress bound, not a refresh of the total body lifetime.
pub(crate) const MEDIA_BODY_NO_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);
/// A relay pump may read only this many chunks ahead of its downstream. The
/// independently driven task owns the peer response and both body timers;
/// dropping the HTTP body drops the receiver and cancels that sole owner.
const RELAY_BODY_CHANNEL_CAPACITY: usize = 2;
const LEASE_INTERVAL: Duration = Duration::from_secs(3);
pub(crate) const LEASE_TTL_MS: i64 = 12_000;
/// A takeover claim must survive provisional publication plus the worst-case
/// first ordinary renewal path. After that first renewal it returns to the
/// normal 12-second lease. The longer one-shot claim is an ownership fence,
/// not an inactivity or playback-progress watchdog.
const TAKEOVER_CLAIM_LEASE_TTL_MS: i64 = 24_000;
/// Begins on the worker before the start response leaves it, so it must
/// strictly outlive ingress placement, owner assignment, the fixed activation
/// lease, and a scheduling margin. There is no independent reconciliation or
/// confirmation watchdog: the activation transaction owns one lease deadline.
pub(crate) const REMOTE_ACTIVATION_CONFIRMATION_WINDOW: Duration = Duration::from_secs(
    START_DEADLINE.as_secs()
        + OWNER_ASSIGNMENT_DEADLINE.as_secs()
        + (LEASE_TTL_MS as u64 / 1_000)
        + 10,
);
const MAX_MEDIA_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const ROUTE_CACHE_TTL: Duration = Duration::from_secs(1);
const MAX_ROUTE_CACHE_ENTRIES: usize = 4_096;
const MAX_CONTROL_RATE_ENTRIES: usize = 4_096;
const CONTROL_RATE_WINDOW: Duration = Duration::from_secs(1);
const CONTROL_RATE_PER_SESSION: u32 = 8;
const CONTROL_RATE_GLOBAL: u32 = 512;
/// Maximum authenticated clock disagreement accepted on a relayed resource
/// deadline. This is deliberately small: it is only tolerance for wall-clock
/// conversion between workers, never extra request work minted at the owner.
const RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE: Duration = Duration::from_secs(2);
/// The public playlist path reserves 55 seconds for rolling startup and five
/// seconds for publication. Keep this wire-envelope ceiling synchronized with
/// that externally visible request lifetime rather than trusting a peer to
/// choose an arbitrary absolute deadline.
const RELAY_PLAYLIST_MAX_LIFETIME: Duration = Duration::from_secs(60);
/// Public status, subtitle-segment, and release requests only perform bounded
/// response publication/control work.
const RELAY_SHORT_MAX_LIFETIME: Duration = Duration::from_secs(5);
/// A media segment can spend 30 seconds in a VOD blocked GET plus the five
/// second publication fence.
const RELAY_SEGMENT_MAX_LIFETIME: Duration = Duration::from_secs(35);
const ROUTE_QUERY_SHARDS: usize = 32;
const ROUTE_GENERATION_SHARDS: usize = 4_096;
const ROUTE_QUERY_DEADLINE: Duration = Duration::from_secs(3);
const LEASE_RENEWAL_BATCH: usize = 256;
const LEASE_RENEWAL_FANOUT: usize = 16;
const LEASE_RENEWAL_DEADLINE: Duration = Duration::from_secs(4);
const LEASE_RENEWAL_MIN_REMAINING_MS: i64 = 4_000;
const MAX_STALE_SETTLEMENTS_PER_TICK: usize = 64;
const STALE_SETTLEMENT_DEADLINE: Duration = Duration::from_secs(4);
/// A candidate must still own the complete exact-read window. Routes closer
/// to expiry are left active so a survivor can claim them instead of a stale
/// local cleanup racing that handoff.
const STALE_SETTLEMENT_MIN_RUNWAY_MS: i64 = 4_000;
const STALE_SETTLEMENT_RETRY_BACKOFF: Duration = Duration::from_secs(30);
const STALE_SETTLEMENT_MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// Public DELETE admits at most this many distinct commit-unknown capabilities
/// at once. Reconciliation itself uses a much smaller fan-out below, so a
/// failed Store cannot turn the fail-closed registry into request-amplified
/// background load.
const MAX_PENDING_RELEASE_RECONCILIATIONS: usize = 128;
const RELEASE_RECONCILIATION_SHARDS: usize = 32;
const RELEASE_RECONCILIATION_FANOUT: usize = 4;
const RELEASE_RECONCILIATION_RETRY_BACKOFF: Duration = Duration::from_secs(1);
const RELEASE_RECONCILIATION_MAX_BACKOFF: Duration = Duration::from_secs(30);
const RELEASE_CLEANUP_CAPACITY: usize = 128;
const TAKEOVER_INTERVAL: Duration = Duration::from_secs(2);
const TAKEOVER_DEADLINE: Duration = Duration::from_secs(8);
const TAKEOVER_BATCH: usize = 16;
const TAKEOVER_FANOUT: usize = 4;
const TAKEOVER_OVERLAP_MARGIN_MS: i64 = 2_000;
const TAKEOVER_CLEANUP_DEADLINE: Duration = Duration::from_secs(2);
/// Commit-unknown takeover settlement owns only the fixed lease proposed by
/// the CAS. Store attempts are kept short enough to make several exact reads
/// inside that lease; immediate failures back off so an unavailable Store
/// cannot become a busy loop.
const TAKEOVER_RECONCILIATION_STORE_DEADLINE: Duration = Duration::from_secs(1);
const TAKEOVER_RECONCILIATION_RETRY_BACKOFF: Duration = Duration::from_millis(250);
/// Enough authority to finish one bounded publication mutation and still
/// reach the next ordinary lease tick, complete its owner inventory, and
/// enter the renewal Store with the full fail-closed admission window.
const TAKEOVER_PUBLICATION_MIN_RUNWAY: Duration = Duration::from_secs(
    TAKEOVER_RECONCILIATION_STORE_DEADLINE.as_secs()
        + LEASE_INTERVAL.as_secs()
        + LEASE_RENEWAL_DEADLINE.as_secs()
        + (LEASE_RENEWAL_MIN_REMAINING_MS as u64 / 1_000)
        + 1,
);

fn try_admit_takeover_settlement() -> Option<tokio::sync::OwnedSemaphorePermit> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(TAKEOVER_BATCH))))
        .try_acquire_owned()
        .ok()
}

/// Physical cleanup is independent from the durable release verdict but must
/// still have finite ownership. Saturation deliberately falls back to lease
/// loss/stale settlement instead of delaying or relabelling a committed End.
pub(crate) fn try_admit_release_cleanup() -> Option<tokio::sync::OwnedSemaphorePermit> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(RELEASE_CLEANUP_CAPACITY))),
    )
    .try_acquire_owned()
    .ok()
}
/// Width of the HLS sequence range each ownership epoch gets to itself.
///
/// ffmpeg's HLS muxer carries the segment number through a C `int`, so any
/// floor above `i32::MAX` is silently truncated and the muxer writes a
/// negative filename that no allowlist accepts — every segment 404s. A
/// million-wide range keeps roughly two thousand epochs inside that ceiling,
/// which is far more failovers than one playback session can survive.
pub(crate) const TAKEOVER_SEQUENCE_STRIDE: i64 = 1_000_000;

/// Session ids whose durable route must survive a process-local transition,
/// counted rather than set-valued.
///
/// Takeover and same-player replacement both have a bounded interval where a
/// durable owner exists but the old worker is absent: takeover has not yet
/// published its adopted worker, while replacement has retired the predecessor
/// before the successor activation CAS. Overlapping attempts are ordinary, so
/// a plain set would let one guard drop un-protect another mid-settlement.
static SESSION_SETTLEMENT_PROTECTED: LazyLock<StdMutex<HashMap<String, usize>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// Keeps one durable session route out of stale settlement while its local
/// worker lifecycle is deliberately between generations. The lease loop must
/// neither end it for having no worker nor renew it without a frontier.
pub(crate) struct SessionSettlementGuard {
    session_id: String,
}

impl SessionSettlementGuard {
    /// Infallible by construction. A poisoned registry must not be able to
    /// turn this protection off — failing open here silently re-enables the
    /// races the guard exists to close, for the rest of the process.
    pub(crate) fn begin(session_id: &str) -> Self {
        *SESSION_SETTLEMENT_PROTECTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session_id.to_owned())
            .or_insert(0) += 1;
        Self {
            session_id: session_id.to_owned(),
        }
    }
}

const TAKEOVER_BUCKETS_MS: [u64; 7] = [100, 250, 500, 1_000, 2_500, 5_000, 10_000];
/// Index into the metric arrays. Named because "2" appearing on four
/// unrelated return paths is how an operator ends up reading a histogram that
/// counts the wrong thing.
const TAKEOVER_METHOD_COPY: usize = 0;
const TAKEOVER_METHOD_TRANSCODE: usize = 1;
const TAKEOVER_METHOD_UNKNOWN: usize = 2;
const TAKEOVER_WON: usize = 0;
const TAKEOVER_LOST: usize = 1;
const TAKEOVER_SKIPPED: usize = 2;
const TAKEOVER_FAILED: usize = 3;

struct TakeoverMetrics {
    outcomes: [[AtomicU64; 4]; 3],
    buckets: [[AtomicU64; 8]; 3],
    duration_micros: [AtomicU64; 3],
}

static TAKEOVER_METRICS: LazyLock<TakeoverMetrics> = LazyLock::new(|| TakeoverMetrics {
    outcomes: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
    buckets: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
    duration_micros: std::array::from_fn(|_| AtomicU64::new(0)),
});

struct TakeoverMetricGuard {
    method: usize,
    outcome: usize,
    started: Instant,
}

/// Every process-local capability that must move together after a takeover
/// claim becomes commit-unknown. Keeping this as one move-only value makes it
/// impossible for a request deadline to drop the worker while leaving either
/// the release generation or replacement serialization behind.
struct PendingTakeoverSettlement<W = TakeoverWorkerGuard, A = SessionAdoptionToken> {
    original: MediaSessionRoute,
    claim: MediaSessionTakeover,
    provisional_id: String,
    worker: W,
    adoption: A,
    monotonic_expiry: tokio::time::Instant,
    claim_cache_generation: u64,
    metric: TakeoverMetricGuard,
}

trait TakeoverWorkerLifecycle: Send + Sized + 'static {
    fn retain_until(&mut self, expires_at_ms: i64, monotonic_expiry: tokio::time::Instant);
    fn stop(self, reason: &'static str) -> BoxFuture<'static, ()>;
    fn stop_and_retain_until(
        self,
        reason: &'static str,
        expires_at_ms: i64,
        monotonic_expiry: tokio::time::Instant,
    ) -> BoxFuture<'static, ()>;
    fn publish(self);
}

trait TakeoverSettlementIo<A: Send + 'static, W: Send + 'static>: Sync {
    fn route_generation(&self, session_id: &str) -> u64;
    fn claim<'a>(
        &'a self,
        claim: &'a MediaSessionTakeover,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>>;
    fn replay<'a>(
        &'a self,
        claim: &'a MediaSessionTakeover,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>>;
    fn read<'a>(
        &'a self,
        incarnation_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>>;
    fn pin<'a>(
        &'a self,
        provisional_id: &'a str,
        route: &'a MediaSessionRoute,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    fn adopt<'a>(
        &'a self,
        provisional_id: &'a str,
        durable_session_id: &'a str,
        worker: W,
        adoption: A,
    ) -> BoxFuture<'a, Result<W, W>>;
    fn renew_first<'a>(
        &'a self,
        route: &'a MediaSessionRoute,
        local_session_id: &'a str,
        lease_expires_at_ms: i64,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>>;
    fn abort_inherited_preparation<'a>(
        &'a self,
        route: &'a MediaSessionRoute,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
    fn seed<'a>(&'a self, route: &'a MediaSessionRoute) -> BoxFuture<'a, ()>;
    fn cache<'a>(
        &'a self,
        route: MediaSessionRoute,
        observed_generation: u64,
    ) -> BoxFuture<'a, bool>;
}

impl TakeoverSettlementIo<SessionAdoptionToken, TakeoverWorkerGuard> for AppState {
    fn route_generation(&self, session_id: &str) -> u64 {
        self.media_sessions.route_generation(session_id)
    }

    fn claim<'a>(
        &'a self,
        claim: &'a MediaSessionTakeover,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        Box::pin(self.store.claim_media_session_takeover(claim))
    }

    fn replay<'a>(
        &'a self,
        claim: &'a MediaSessionTakeover,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        Box::pin(self.store.claim_media_session_takeover(claim))
    }

    fn read<'a>(
        &'a self,
        incarnation_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        Box::pin(
            self.store
                .media_session_route_by_incarnation(incarnation_id),
        )
    }

    fn pin<'a>(
        &'a self,
        provisional_id: &'a str,
        route: &'a MediaSessionRoute,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(self.transcode.pin_shared_session(
            provisional_id,
            &route.incarnation_id,
            route.owner_epoch,
            route.lease_expires_at_ms,
        ))
    }

    fn adopt<'a>(
        &'a self,
        provisional_id: &'a str,
        durable_session_id: &'a str,
        worker: TakeoverWorkerGuard,
        adoption: SessionAdoptionToken,
    ) -> BoxFuture<'a, Result<TakeoverWorkerGuard, TakeoverWorkerGuard>> {
        Box::pin(self.transcode.adopt_session_id_with_owner(
            provisional_id,
            durable_session_id,
            adoption,
            worker,
        ))
    }

    fn renew_first<'a>(
        &'a self,
        route: &'a MediaSessionRoute,
        local_session_id: &'a str,
        lease_expires_at_ms: i64,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        Box::pin(async move {
            let local_session_id = local_session_id.to_owned();
            let frontiers = self
                .transcode
                .session_frontiers(std::slice::from_ref(&local_session_id))
                .await;
            let Some(frontier) = frontiers.get(&local_session_id) else {
                return Ok(None);
            };
            let now_ms = unix_ms();
            let renewal = MediaSessionRenewal {
                incarnation_id: route.incarnation_id.clone(),
                owner_epoch: route.owner_epoch,
                produced_playable_through_ms: frontier.produced_playable_through_ms,
                fetched_through_ms: frontier.fetched_through_ms,
                media_sequence: frontier.media_sequence,
            };
            let renewed = self
                .store
                .renew_media_sessions(
                    &self.node_id,
                    std::slice::from_ref(&renewal),
                    now_ms,
                    lease_expires_at_ms,
                )
                .await?;
            if renewed.len() != 1 || renewed[0] != route.incarnation_id {
                return Ok(None);
            }
            let mut route = route.clone();
            route.lease_expires_at_ms = lease_expires_at_ms;
            route.produced_playable_through_ms = renewal.produced_playable_through_ms;
            route.fetched_through_ms = renewal.fetched_through_ms;
            route.media_sequence = renewal.media_sequence;
            route.updated_at_ms = now_ms;
            Ok(Some(route))
        })
    }

    fn abort_inherited_preparation<'a>(
        &'a self,
        route: &'a MediaSessionRoute,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some(staged) = self
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await?
            else {
                return Ok(());
            };
            if staged.expected_predecessor_incarnation_id != route.incarnation_id {
                return Err(StoreError::Task(
                    "takeover found a preparation for the wrong predecessor".to_owned(),
                ));
            }
            let aborted = self
                .store
                .abort_media_session_preparation(
                    route.user_id,
                    &route.playback_id,
                    &plurx_core::domain::MediaSessionPreparationAbortRequest {
                        staged_incarnation_id: staged.staged_incarnation_id.clone(),
                        expected_predecessor_owner_node_id: route.owner_node_id.clone(),
                        expected_predecessor_owner_epoch: route.owner_epoch,
                        now_ms: unix_ms(),
                    },
                )
                .await?;
            if aborted.is_some() {
                return Ok(());
            }
            let retained = self
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await?;
            if retained.as_ref().is_some_and(|retained| {
                retained.staged_incarnation_id == staged.staged_incarnation_id
            }) {
                return Err(StoreError::Task(
                    "takeover could not settle the inherited preparation".to_owned(),
                ));
            }
            Ok(())
        })
    }

    fn seed<'a>(&'a self, route: &'a MediaSessionRoute) -> BoxFuture<'a, ()> {
        Box::pin(self.media_sessions.seed_owned_lease(route))
    }

    fn cache<'a>(
        &'a self,
        route: MediaSessionRoute,
        observed_generation: u64,
    ) -> BoxFuture<'a, bool> {
        Box::pin(
            self.media_sessions
                .cache_route_if_generation(route, observed_generation),
        )
    }
}

/// Fail-closed cleanup owner for the provisional or adopted local worker.
/// Every normal verdict consumes this guard; cancellation or panic transfers
/// the exact worker and replacement serialization into detached teardown.
struct TakeoverWorkerGuard {
    manager: Arc<TranscodeManager>,
    local_session_id: String,
    replacement: Option<ClusterReplacementGuard>,
    settlement: Option<SessionSettlementGuard>,
    slot: Option<tokio::sync::OwnedSemaphorePermit>,
    retain_until: Option<(i64, tokio::time::Instant)>,
}

impl TakeoverWorkerGuard {
    fn new(
        manager: Arc<TranscodeManager>,
        local_session_id: String,
        replacement: ClusterReplacementGuard,
        settlement: SessionSettlementGuard,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Self {
        Self {
            manager,
            local_session_id,
            replacement: Some(replacement),
            settlement: Some(settlement),
            slot: Some(slot),
            retain_until: None,
        }
    }

    fn adopted(&mut self, durable_session_id: &str) {
        self.local_session_id = durable_session_id.to_owned();
    }

    fn retain_settlement_until(
        &mut self,
        expires_at_ms: i64,
        monotonic_expiry: tokio::time::Instant,
    ) {
        self.retain_until = Some((expires_at_ms, monotonic_expiry));
    }

    fn spawn_teardown(&mut self, reason: &'static str) -> Option<tokio::task::JoinHandle<()>> {
        let replacement = self.replacement.take()?;
        let manager = Arc::clone(&self.manager);
        let session_id = self.local_session_id.clone();
        let settlement = self.settlement.take();
        let slot = self.slot.take();
        let retain_until = self.retain_until.take();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                session = %crate::transcode::session_log_id(&session_id),
                "runtime ended before takeover worker teardown could be scheduled"
            );
            return None;
        };
        Some(runtime.spawn(async move {
            manager
                .stop_session_until(
                    &session_id,
                    reason,
                    tokio::time::Instant::now() + TAKEOVER_CLEANUP_DEADLINE,
                    replacement,
                )
                .await;
            if let Some((expires_at_ms, monotonic_expiry)) = retain_until {
                retain_takeover_settlement_until_expiry(expires_at_ms, monotonic_expiry).await;
            }
            drop(settlement);
            drop(slot);
        }))
    }

    async fn stop(mut self, reason: &'static str) {
        // A normal caller reaches this only with a definitive loss or before
        // claim submission. Unexpected Drop retains the armed fixed lease;
        // an explicit verdict can release it immediately.
        self.retain_until = None;
        if let Some(teardown) = self.spawn_teardown(reason) {
            let _ = teardown.await;
        }
    }

    async fn stop_and_retain_until(
        mut self,
        reason: &'static str,
        expires_at_ms: i64,
        monotonic_expiry: tokio::time::Instant,
    ) {
        self.retain_settlement_until(expires_at_ms, monotonic_expiry);
        if let Some(teardown) = self.spawn_teardown(reason) {
            // The spawned owner survives cancellation of this waiter and
            // retains both guards through the winner's fixed lease.
            let _ = teardown.await;
        }
    }

    fn publish(mut self) {
        drop(self.replacement.take());
        drop(self.settlement.take());
        drop(self.slot.take());
    }
}

impl TakeoverWorkerLifecycle for TakeoverWorkerGuard {
    fn retain_until(&mut self, expires_at_ms: i64, monotonic_expiry: tokio::time::Instant) {
        self.retain_settlement_until(expires_at_ms, monotonic_expiry);
    }

    fn stop(self, reason: &'static str) -> BoxFuture<'static, ()> {
        Box::pin(TakeoverWorkerGuard::stop(self, reason))
    }

    fn stop_and_retain_until(
        self,
        reason: &'static str,
        expires_at_ms: i64,
        monotonic_expiry: tokio::time::Instant,
    ) -> BoxFuture<'static, ()> {
        Box::pin(TakeoverWorkerGuard::stop_and_retain_until(
            self,
            reason,
            expires_at_ms,
            monotonic_expiry,
        ))
    }

    fn publish(self) {
        TakeoverWorkerGuard::publish(self);
    }
}

impl crate::transcode::SessionAdoptionOwner for TakeoverWorkerGuard {
    fn adopted_session_id(&mut self, durable_session_id: &str) {
        self.adopted(durable_session_id);
    }
}

impl Drop for TakeoverWorkerGuard {
    fn drop(&mut self) {
        drop(self.spawn_teardown("media-session takeover settlement owner dropped"));
    }
}

/// Owns the creation child and its exact cleanup capability as one value.
/// Dropping a JoinHandle detaches its task, so supervisor cancellation must
/// instead abort and await that child before exact worker teardown can begin.
struct TakeoverCreationOwner<W: TakeoverWorkerLifecycle = TakeoverWorkerGuard> {
    handle: Option<tokio::task::JoinHandle<Result<StartInfo, String>>>,
    worker: Option<W>,
}

impl<W: TakeoverWorkerLifecycle> TakeoverCreationOwner<W> {
    fn new(handle: tokio::task::JoinHandle<Result<StartInfo, String>>, worker: W) -> Self {
        Self {
            handle: Some(handle),
            worker: Some(worker),
        }
    }

    async fn finish(mut self, deadline: tokio::time::Instant) -> Result<(StartInfo, W), String> {
        let outcome = tokio::time::timeout_at(
            deadline,
            self.handle
                .as_mut()
                .expect("takeover creation owner always starts armed"),
        )
        .await;
        match outcome {
            Ok(Ok(Ok(info))) => {
                drop(self.handle.take());
                let worker = self
                    .worker
                    .take()
                    .expect("successful creation retains its worker owner");
                Ok((info, worker))
            }
            Ok(Ok(Err(error))) => {
                drop(self.handle.take());
                if let Some(worker) = self.worker.take() {
                    worker.stop("media-session takeover creation failed").await;
                }
                Err(error)
            }
            Ok(Err(error)) => {
                drop(self.handle.take());
                if let Some(worker) = self.worker.take() {
                    worker
                        .stop("media-session takeover creation task failed")
                        .await;
                }
                Err(format!(
                    "media-session takeover creation task failed: {error}"
                ))
            }
            Err(_) => {
                let handle = self
                    .handle
                    .as_mut()
                    .expect("timed-out takeover creation remains owned");
                handle.abort();
                let _ = handle.await;
                drop(self.handle.take());
                if let Some(worker) = self.worker.take() {
                    worker
                        .stop("media-session takeover creation timed out")
                        .await;
                }
                Err("media-session takeover creation timed out".to_owned())
            }
        }
    }
}

impl<W: TakeoverWorkerLifecycle> Drop for TakeoverCreationOwner<W> {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let Some(worker) = self.worker.take() else {
            handle.abort();
            return;
        };
        handle.abort();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!("runtime ended before takeover creation could be cancelled");
            return;
        };
        runtime.spawn(async move {
            let _ = handle.await;
            worker
                .stop("media-session takeover creation owner dropped")
                .await;
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TakeoverClaimVerdict {
    Won(Box<MediaSessionRoute>),
    Pending,
    Lost,
}

impl TakeoverMetricGuard {
    fn new() -> Self {
        Self {
            method: TAKEOVER_METHOD_UNKNOWN,
            outcome: TAKEOVER_FAILED,
            started: Instant::now(),
        }
    }
}

impl Drop for SessionSettlementGuard {
    fn drop(&mut self) {
        let mut settling = SESSION_SETTLEMENT_PROTECTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(holders) = settling.get_mut(&self.session_id) {
            *holders = holders.saturating_sub(1);
            if *holders == 0 {
                settling.remove(&self.session_id);
            }
        }
    }
}

pub(crate) fn settlement_protected_ids() -> HashSet<String> {
    SESSION_SETTLEMENT_PROTECTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .cloned()
        .collect()
}

impl Drop for TakeoverMetricGuard {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        TAKEOVER_METRICS.outcomes[self.method][self.outcome].fetch_add(1, Ordering::Relaxed);
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let bucket = TAKEOVER_BUCKETS_MS
            .iter()
            .position(|bound| elapsed_ms <= *bound)
            .unwrap_or(TAKEOVER_BUCKETS_MS.len());
        TAKEOVER_METRICS.buckets[self.method][bucket].fetch_add(1, Ordering::Relaxed);
        TAKEOVER_METRICS.duration_micros[self.method].fetch_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

pub(crate) fn prometheus() -> String {
    let mut out = String::from(
        "# HELP plurx_media_session_takeovers_total Expired media-session takeover decisions.\n\
         # TYPE plurx_media_session_takeovers_total counter\n",
    );
    for (method_index, method) in ["copy", "transcode", "unknown"].iter().enumerate() {
        for (outcome_index, outcome) in ["won", "lost", "skipped", "failed"].iter().enumerate() {
            let count =
                TAKEOVER_METRICS.outcomes[method_index][outcome_index].load(Ordering::Relaxed);
            out.push_str(&format!(
                "plurx_media_session_takeovers_total{{method=\"{method}\",outcome=\"{outcome}\"}} {count}\n"
            ));
        }
    }
    out.push_str(
        "# HELP plurx_media_session_takeover_seconds Time spent deciding and settling a takeover.\n\
         # TYPE plurx_media_session_takeover_seconds histogram\n",
    );
    for (method_index, method) in ["copy", "transcode", "unknown"].iter().enumerate() {
        let mut cumulative = 0_u64;
        for (bucket_index, bound_ms) in TAKEOVER_BUCKETS_MS.iter().enumerate() {
            cumulative = cumulative.saturating_add(
                TAKEOVER_METRICS.buckets[method_index][bucket_index].load(Ordering::Relaxed),
            );
            out.push_str(&format!(
                "plurx_media_session_takeover_seconds_bucket{{method=\"{method}\",le=\"{}\"}} {cumulative}\n",
                *bound_ms as f64 / 1_000.0
            ));
        }
        cumulative = cumulative.saturating_add(
            TAKEOVER_METRICS.buckets[method_index][TAKEOVER_BUCKETS_MS.len()]
                .load(Ordering::Relaxed),
        );
        let sum = TAKEOVER_METRICS.duration_micros[method_index].load(Ordering::Relaxed) as f64
            / 1_000_000.0;
        out.push_str(&format!(
            "plurx_media_session_takeover_seconds_bucket{{method=\"{method}\",le=\"+Inf\"}} {cumulative}\n\
             plurx_media_session_takeover_seconds_sum{{method=\"{method}\"}} {sum}\n\
             plurx_media_session_takeover_seconds_count{{method=\"{method}\"}} {cumulative}\n"
        ));
    }
    out
}

#[derive(Clone, Copy, Debug)]
struct StaleSettlementBackoff {
    failures: u32,
    next_attempt: tokio::time::Instant,
}

#[derive(Debug)]
struct StaleSettlementResult {
    session_id: String,
    retry: Option<StaleSettlementBackoff>,
}

struct PendingReleaseReconciliation {
    failures: u32,
    next_attempt: tokio::time::Instant,
    in_flight: bool,
    route_pending: bool,
    owner_projection: Option<MediaSessionRoute>,
    /// Starts only when a definitive durable terminal route is known. Starting
    /// this at HTTP election would let a long commit-unknown interval consume
    /// the proof window while the old owner was still legitimately active.
    projection_deadline: Option<tokio::time::Instant>,
    intent: ReleaseIntent,
    settlement: Arc<ReleaseSettlement>,
}

#[derive(Clone, Copy)]
pub(crate) struct ReleaseIntent {
    pub(crate) terminal: crate::vodserve::Terminal,
    pub(crate) reason: &'static str,
}

impl ReleaseIntent {
    #[cfg(test)]
    pub(crate) const CLIENT: Self = Self {
        terminal: crate::vodserve::Terminal::Deleted,
        reason: "released by client",
    };
}

struct PendingReleaseResult {
    session_id: String,
    settlement: Arc<ReleaseSettlement>,
    intent: ReleaseIntent,
    outcome: Result<Option<MediaSessionRoute>, StoreError>,
}

pub(crate) enum ReleaseAdmission {
    Won(Arc<ReleaseSettlement>),
    Joined(Arc<ReleaseSettlement>),
    Full,
}

pub(crate) struct ReleaseSettlement {
    result: StdMutex<Option<StatusCode>>,
    settled: tokio::sync::Notify,
}

/// Move-only evidence that authoritative Store terminal state or absence was
/// published into the route cache before a process-local release gate is
/// forgotten. Only coordinator completion methods can construct it.
pub(crate) struct DurableReleaseProof {
    session_id: String,
    terminal_projection_complete: bool,
}

impl DurableReleaseProof {
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn terminal_projection_complete(&self) -> bool {
        self.terminal_projection_complete
    }
}

impl ReleaseSettlement {
    fn new() -> Self {
        Self {
            result: StdMutex::new(None),
            settled: tokio::sync::Notify::new(),
        }
    }

    fn complete(&self, status: StatusCode) {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.is_none() {
            *result = Some(status);
            drop(result);
            self.settled.notify_waiters();
        }
    }

    pub(crate) async fn wait(&self) -> StatusCode {
        loop {
            if let Some(status) = *self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
            {
                return status;
            }
            let notified = self.settled.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
            {
                continue;
            }
            notified.await;
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteStartRequest {
    pub protocol_version: i64,
    pub incarnation_id: String,
    pub user_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    /// Whether this session serves the typeless sliding playlist shape.
    ///
    /// A successor renumbers from its epoch floor and advertises none of the
    /// predecessor's segments, which RFC 8216 §6.2.1 forbids an EVENT
    /// playlist from doing. A session that was serving EVENT therefore cannot
    /// be taken over at all — and a recipe written before this field existed
    /// defaults to `false`, so it is refused rather than guessed at.
    #[serde(default)]
    pub typeless_playlist: bool,
    pub request: SessionRequest,
}

impl RemoteStartRequest {
    pub(crate) fn is_valid(&self) -> bool {
        remote_start_envelope_is_valid(self) && worker_session_request_is_valid(&self.request)
    }
}

fn remote_start_envelope_is_valid(request: &RemoteStartRequest) -> bool {
    request.protocol_version == crate::media_pool::PROTOCOL_VERSION
        && uuid::Uuid::parse_str(&request.incarnation_id).is_ok()
        && request.user_id > 0
        && request.source_size >= 0
        && request.source_mtime >= 0
        && request.request.request_id.as_deref() == Some(request.incarnation_id.as_str())
}

/// A durable rolling-session recipe uses the same bounded field vocabulary as
/// private worker ingress, but has the legacy live presentation by design.
/// Keeping this validator separate means failover cannot accidentally widen
/// the network-facing remote-start contract beyond immutable VOD.
fn takeover_recipe_is_valid(request: &RemoteStartRequest) -> bool {
    remote_start_envelope_is_valid(request)
        && worker_session_request_fields_are_valid(&request.request)
        && request.request.presentation == crate::transcode::Presentation::Live
}

/// The request contract shared by public ingress and private worker ingress.
///
/// The cluster envelope separately binds the internal request id to its
/// incarnation. Public idempotency keys deliberately have a wider syntax, but
/// every field that reaches a local worker must obey the same media bounds as
/// a request sent to a peer.
pub(crate) fn worker_session_request_is_valid(request: &SessionRequest) -> bool {
    worker_session_request_fields_are_valid(request)
        && request.presentation == crate::transcode::Presentation::Vod
}

fn worker_session_request_fields_are_valid(request: &SessionRequest) -> bool {
    request.file_id > 0
        && !request.playback_id.trim().is_empty()
        && request.playback_id.len() <= 128
        && !request
            .playback_id
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
        && request.start_seconds.is_finite()
        && request.start_seconds >= 0.0
        && request.start_seconds * 1_000.0 <= MAX_MEDIA_MILLIS as f64
        && request
            .audio_index
            .is_none_or(|index| (0..=1_024).contains(&index))
        && request
            .subtitle_burn
            .is_none_or(|index| (0..=1_024).contains(&index))
        && (-15_000..=15_000).contains(&request.audio_offset_ms)
        && match &request.kind {
            SessionKind::Transcode { height } => {
                (crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT).contains(height)
            }
            SessionKind::Copy { .. } => true,
        }
        && matches!(
            (&request.previous_session_id, request.reopen_reason),
            (None, None) | (Some(_), Some(_))
        )
        && request
            .previous_session_id
            .as_deref()
            .is_none_or(|value| uuid::Uuid::parse_str(value).is_ok())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteStartResponse {
    pub session_id: String,
    pub playlist_url: String,
    pub duration_ms: Option<i64>,
    pub start_seconds: f64,
    pub media_origin_seconds: f64,
    pub target_height: i64,
    pub kind: SessionKind,
    pub encoder: String,
    pub grade: OutputGrade,
    pub vod: bool,
    pub control_lease_timeout_ms: u32,
    /// Serving generation admitted by a negotiated target START. It is
    /// omitted for legacy callers so their strict response decoder keeps the
    /// original wire shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_generation: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteActivateRequest {
    pub target_generation: u64,
    pub activation: MediaSessionActivation,
}

impl RemoteActivateRequest {
    pub(crate) fn is_valid_for(&self, target_node_id: &str) -> bool {
        self.activation.owner_node_id == target_node_id
            && self.activation.request_id.is_some()
            && self.activation.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED
            && self.activation.lease_expires_at_ms > self.activation.now_ms
    }
}

pub(crate) struct RemoteSessionStart {
    pub(crate) info: RemoteStartResponse,
    pub(crate) ownership: RemoteSessionStartOwnership,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteSessionStartOwnership {
    Created,
    Recovered,
    /// A pre-negotiation peer returned the legacy 200 response, which does
    /// not reveal whether this request created the worker or replayed one.
    LegacyAmbiguous,
}

fn decode_remote_start_response(
    response: crate::http::peer_transport::PeerResponse,
) -> Result<RemoteSessionStart, PeerTransportError> {
    if !matches!(
        response.status,
        reqwest::StatusCode::OK
            | reqwest::StatusCode::CREATED
            | reqwest::StatusCode::ALREADY_REPORTED
    ) {
        return Err(if response.status == reqwest::StatusCode::REQUEST_TIMEOUT {
            PeerTransportError::TimedOut
        } else {
            PeerTransportError::InvalidResponse
        });
    }
    let ownership = match response.status {
        reqwest::StatusCode::CREATED => RemoteSessionStartOwnership::Created,
        reqwest::StatusCode::ALREADY_REPORTED => RemoteSessionStartOwnership::Recovered,
        // A legacy 200 is deliberately not guessed. It may be a replay whose
        // worker belongs to an earlier request, so arming abort ownership here
        // could kill that live session. The ingress rejects this placement;
        // the target's activation-confirmation window cleans up a truly new
        // worker when no durable activation follows.
        reqwest::StatusCode::OK => RemoteSessionStartOwnership::LegacyAmbiguous,
        _ => unreachable!("successful status was checked above"),
    };
    serde_json::from_slice::<RemoteStartResponse>(&response.body)
        .ok()
        .filter(RemoteStartResponse::is_valid)
        .filter(|info| {
            ownership == RemoteSessionStartOwnership::LegacyAmbiguous
                || info.activation_generation.is_some()
        })
        .map(|info| RemoteSessionStart { info, ownership })
        .ok_or(PeerTransportError::InvalidResponse)
}

impl From<StartInfo> for RemoteStartResponse {
    fn from(info: StartInfo) -> Self {
        Self {
            session_id: info.session_id,
            playlist_url: info.playlist_url,
            duration_ms: info.duration_ms,
            start_seconds: info.start_seconds,
            media_origin_seconds: info.media_origin_seconds,
            target_height: info.target_height,
            kind: info.kind,
            encoder: info.encoder.to_owned(),
            grade: info.grade,
            vod: info.vod,
            control_lease_timeout_ms: info.control_lease_timeout_ms,
            activation_generation: None,
        }
    }
}

impl RemoteStartResponse {
    pub(crate) fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.playlist_url == format!("/api/v1/hls/{}/index.m3u8", self.session_id)
            && self
                .duration_ms
                .is_none_or(|duration| (0..=MAX_MEDIA_MILLIS).contains(&duration))
            && self.start_seconds.is_finite()
            && (0.0..=MAX_MEDIA_MILLIS as f64 / 1_000.0).contains(&self.start_seconds)
            && self.media_origin_seconds.is_finite()
            && (0.0..=MAX_MEDIA_MILLIS as f64 / 1_000.0).contains(&self.media_origin_seconds)
            && match &self.kind {
                SessionKind::Transcode { height } => {
                    *height == self.target_height
                        && (crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT)
                            .contains(height)
                }
                // Copy reports source metadata, not a selected ladder rung.
                // Unknown/audio sources use zero and existing sources may be
                // below or above the transcode ladder's supported range.
                SessionKind::Copy { .. } => self.target_height >= 0,
            }
            && !self.encoder.is_empty()
            && self.encoder.len() <= 256
            && !self
                .encoder
                .bytes()
                .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
            && matches!(
                self.control_lease_timeout_ms,
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS
                    | crate::playback_control::VOD_LEASE_TIMEOUT_MS
            )
        // `vod` is presentation telemetry, not structural validity. A
        // recovery-enabled worker may honestly answer `false` after accepting
        // a VOD request whose immutable prerequisites are still pending.
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteAbortRequest {
    pub incarnation_id: String,
    pub session_id: String,
    /// Exact durable owner generation. Zero is accepted only for the legacy
    /// pre-activation start-abort envelope.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub expected_owner_epoch: i64,
    /// Absent is the rolling-upgrade-compatible legacy start-abort reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl RemoteAbortRequest {
    pub(crate) fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.incarnation_id).is_ok()
            && uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.expected_owner_epoch >= 0
            && (self.reason.is_none() || self.expected_owner_epoch > 0)
            && self.reason.as_deref().is_none_or(|reason| {
                reason == "cluster start aborted"
                    || crate::vodserve::Terminal::from_control_reason(reason).is_some()
            })
    }

    fn without_reason(&self) -> Self {
        Self {
            incarnation_id: self.incarnation_id.clone(),
            session_id: self.session_id.clone(),
            // The pre-epoch wire shape knows only these two ids. Such a peer
            // predates takeover and can therefore own only epoch 1; omitting
            // the field is both compatible and generation-safe.
            expected_owner_epoch: 0,
            reason: None,
        }
    }
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "resource", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RelayResource {
    Status,
    Playlist {
        native: Option<u8>,
        subtitle: Option<i64>,
    },
    Master {
        subtitle: Option<i64>,
        diagnostic: Option<String>,
    },
    VideoPlaylist,
    SubtitlePlaylist {
        index: i64,
    },
    SubtitleSegment {
        index: i64,
        segment: String,
    },
    Segment {
        segment: String,
    },
    Delete,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayHeaders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_match: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_modified_since: Option<String>,
}

impl RelayHeaders {
    pub(crate) fn from_http(headers: &axum::http::HeaderMap) -> Self {
        let value = |name: axum::http::HeaderName| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .filter(|value| value.len() <= 1_024)
                .map(str::to_owned)
        };
        Self {
            range: value(axum::http::header::RANGE),
            // Preserve the presence of an invalid/oversized If-Range as an
            // explicit non-match. Treating it as absent would incorrectly
            // authorize a partial response that public ingress did not prove.
            if_range: headers.get(axum::http::header::IF_RANGE).map(|header| {
                header
                    .to_str()
                    .ok()
                    .filter(|value| value.len() <= 1_024)
                    .unwrap_or_default()
                    .to_owned()
            }),
            if_none_match: value(axum::http::header::IF_NONE_MATCH),
            if_modified_since: value(axum::http::header::IF_MODIFIED_SINCE),
        }
    }

    fn is_valid(&self) -> bool {
        [
            &self.range,
            &self.if_range,
            &self.if_none_match,
            &self.if_modified_since,
        ]
        .into_iter()
        .flatten()
        .all(|value| {
            value.len() <= 1_024
                && !value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
                && value.parse::<axum::http::HeaderValue>().is_ok()
        })
    }

    /// The original relay envelope is strict (`deny_unknown_fields`) and has
    /// no peer-version negotiation. Until a peer explicitly advertises
    /// If-Range support, never send the new field to it. Suppress Range too:
    /// an old owner must return the complete representation rather than a 206
    /// it could not prove against the validator.
    pub(crate) fn for_unversioned_peer(&self) -> Self {
        Self {
            range: None,
            if_range: None,
            if_none_match: self.if_none_match.clone(),
            if_modified_since: self.if_modified_since.clone(),
        }
    }
}

impl RelayResource {
    pub(crate) fn is_valid(&self) -> bool {
        match self {
            Self::Status | Self::VideoPlaylist | Self::Delete => true,
            Self::Playlist { native, subtitle } => {
                native.is_none_or(|value| value <= 1)
                    && subtitle.is_none_or(|value| (0..=1_024).contains(&value))
            }
            Self::Master {
                subtitle,
                diagnostic,
            } => {
                subtitle.is_none_or(|value| (0..=1_024).contains(&value))
                    && diagnostic.as_ref().is_none_or(|value| {
                        value.len() <= 64 && !value.contains('\r') && !value.contains('\n')
                    })
            }
            Self::SubtitlePlaylist { index } => (0..=1_024).contains(index),
            Self::SubtitleSegment { index, segment } => {
                (0..=1_024).contains(index) && valid_resource_name(segment)
            }
            Self::Segment { segment } => valid_resource_name(segment),
        }
    }

    fn max_lifetime(&self) -> Duration {
        match self {
            Self::Playlist { .. }
            | Self::Master { .. }
            | Self::VideoPlaylist
            | Self::SubtitlePlaylist { .. } => RELAY_PLAYLIST_MAX_LIFETIME,
            Self::Segment { .. } => RELAY_SEGMENT_MAX_LIFETIME,
            Self::Status | Self::SubtitleSegment { .. } | Self::Delete => RELAY_SHORT_MAX_LIFETIME,
        }
    }

    fn max_inherited_lifetime(&self) -> Duration {
        self.max_lifetime()
            .saturating_add(RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayRequest {
    pub session_id: String,
    pub resource: RelayResource,
    /// Absolute end-to-end resource deadline inherited from public ingress.
    /// A relay must consume this budget, never mint a fresh one at each hop.
    pub deadline_unix_ms: i64,
    #[serde(default)]
    pub headers: RelayHeaders,
}

impl RelayRequest {
    pub(crate) fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.resource.is_valid()
            && self.deadline_unix_ms > 0
            && self.deadline_is_plausible_at(unix_ms())
            && self.headers.is_valid()
    }

    fn remaining_at(&self, now_unix_ms: i64) -> Option<Duration> {
        let remaining_ms = u64::try_from(self.deadline_unix_ms.saturating_sub(now_unix_ms))
            .ok()
            .filter(|remaining| *remaining > 0)?;
        Some(Duration::from_millis(remaining_ms))
    }

    /// Convert the origin's own wall-clock envelope into its transport
    /// budget. No skew is spendable here because the sender has the same
    /// clock that produced the wire deadline.
    pub(crate) fn transport_budget_at(&self, now_unix_ms: i64) -> Option<Duration> {
        Some(
            self.remaining_at(now_unix_ms)?
                .min(self.resource.max_lifetime()),
        )
    }

    /// Convert a peer's wall-clock envelope into an owner-side monotonic
    /// budget. Validation tolerates bounded clock disagreement, but execution
    /// subtracts the entire allowance: an owner whose clock is behind cannot
    /// publish after the ingress transport has already abandoned the request.
    /// The origin's earlier absolute deadline otherwise remains authoritative;
    /// reconstructing this later consumes time rather than refreshing it.
    pub(crate) fn owner_budget_at(&self, now_unix_ms: i64) -> Option<Duration> {
        let budget = self
            .remaining_at(now_unix_ms)?
            .checked_sub(RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE)?
            .min(self.resource.max_lifetime());
        (!budget.is_zero()).then_some(budget)
    }

    fn deadline_is_plausible_at(&self, now_unix_ms: i64) -> bool {
        let max_ms =
            i64::try_from(self.resource.max_inherited_lifetime().as_millis()).unwrap_or(i64::MAX);
        self.deadline_unix_ms <= now_unix_ms.saturating_add(max_ms)
    }
}

fn valid_resource_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b'\r' | b'\n' | b'\0'))
        && value != "."
        && value != ".."
}

#[derive(Clone)]
pub(crate) struct MediaSessionCoordinator {
    membership: MembershipManager,
    transport: PeerTransport,
    store: Arc<dyn Store>,
    routes: Arc<tokio::sync::Mutex<HashMap<String, CachedRoute>>>,
    /// Capability releases which have been admitted but whose durable end is
    /// still being reconciled.  A Store error is commit-unknown, so the HTTP
    /// owner must neither discard the mutation nor allow its prior positive
    /// route cache to reopen publication while it retries idempotently.
    ///
    /// Entries are bounded by the HTTP release-settlement semaphore. They are
    /// removed only after a definitive terminal route or durable absence has
    /// replaced the cached answer.
    release_fences: Arc<Vec<StdMutex<HashMap<String, PendingReleaseReconciliation>>>>,
    release_fence_count: Arc<std::sync::atomic::AtomicUsize>,
    route_queries: Arc<Vec<tokio::sync::Mutex<()>>>,
    route_generations: Arc<Vec<std::sync::atomic::AtomicU64>>,
    lease_seeds: Arc<tokio::sync::Mutex<HashMap<String, LeaseSeed>>>,
    control_admission: Arc<StdMutex<ControlAdmission>>,
    #[cfg(test)]
    route_store_queries: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
struct CachedRoute {
    route: Option<MediaSessionRoute>,
    expires_at: tokio::time::Instant,
}

/// Durable resolution used when HTTP status depends on more than whether a
/// route can currently authorize bytes. `route` remains the compatibility
/// active-only facade; publication reclassification must use this raw view so
/// terminal state and a live remote handoff cannot collapse into a false 404.
#[derive(Clone, Debug)]
pub(crate) enum DurableRouteResolution {
    Absent,
    ActiveLocal(MediaSessionRoute),
    ActiveRemote(MediaSessionRoute),
    /// The durable row is still active, but its exact owner lease no longer
    /// authorizes bytes. A takeover or stale-owner settlement may still be in
    /// flight, so this is retryable state rather than a terminal tombstone.
    OwnerTransition(MediaSessionRoute),
    Terminal(MediaSessionRoute),
}

struct ControlRateEntry {
    window_started: Instant,
    admitted: u32,
}

struct ControlAdmission {
    window_started: Instant,
    admitted: u32,
    sessions: HashMap<String, ControlRateEntry>,
}

impl Default for ControlAdmission {
    fn default() -> Self {
        Self {
            window_started: Instant::now(),
            admitted: 0,
            sessions: HashMap::new(),
        }
    }
}

impl ControlAdmission {
    fn admit(&mut self, now: Instant, session_id: &str) -> Result<(), u32> {
        if now.duration_since(self.window_started) >= CONTROL_RATE_WINDOW {
            self.window_started = now;
            self.admitted = 0;
        }
        let global_remaining =
            CONTROL_RATE_WINDOW.saturating_sub(now.duration_since(self.window_started));
        if self.admitted >= CONTROL_RATE_GLOBAL {
            return Err(u32::try_from(global_remaining.as_millis())
                .unwrap_or(u32::MAX)
                .max(1));
        }
        self.sessions
            .retain(|_, entry| now.duration_since(entry.window_started) < CONTROL_RATE_WINDOW);
        if self.sessions.len() >= MAX_CONTROL_RATE_ENTRIES
            && !self.sessions.contains_key(session_id)
        {
            if let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, entry)| entry.window_started)
                .map(|(session_id, _)| session_id.clone())
            {
                self.sessions.remove(&oldest);
            }
        }
        let entry = self
            .sessions
            .entry(session_id.to_owned())
            .or_insert(ControlRateEntry {
                window_started: now,
                admitted: 0,
            });
        if now.duration_since(entry.window_started) >= CONTROL_RATE_WINDOW {
            entry.window_started = now;
            entry.admitted = 0;
        }
        if entry.admitted >= CONTROL_RATE_PER_SESSION {
            let remaining =
                CONTROL_RATE_WINDOW.saturating_sub(now.duration_since(entry.window_started));
            return Err(u32::try_from(remaining.as_millis())
                .unwrap_or(u32::MAX)
                .max(1));
        }
        entry.admitted = entry.admitted.saturating_add(1);
        self.admitted = self.admitted.saturating_add(1);
        Ok(())
    }
}

impl MediaSessionCoordinator {
    pub(crate) fn new(membership: MembershipManager, store: Arc<dyn Store>) -> Arc<Self> {
        Arc::new(Self {
            transport: PeerTransport::new(membership.clone()),
            membership,
            store,
            routes: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            release_fences: Arc::new(
                (0..RELEASE_RECONCILIATION_SHARDS)
                    .map(|_| StdMutex::new(HashMap::new()))
                    .collect(),
            ),
            release_fence_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            route_queries: Arc::new(
                (0..ROUTE_QUERY_SHARDS)
                    .map(|_| tokio::sync::Mutex::new(()))
                    .collect(),
            ),
            route_generations: Arc::new(
                (0..ROUTE_GENERATION_SHARDS)
                    .map(|_| std::sync::atomic::AtomicU64::new(0))
                    .collect(),
            ),
            lease_seeds: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            control_admission: Arc::new(StdMutex::new(ControlAdmission::default())),
            #[cfg(test)]
            route_store_queries: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Fixed-window admission before any durable lookup. The session budget
    /// leaves room for idempotent transport retries; the global budget bounds
    /// random UUID probes, and stale entries are capped and evicted.
    pub(crate) fn admit_control(&self, session_id: &str) -> Result<(), u32> {
        let now = Instant::now();
        let mut admission = self
            .control_admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        admission.admit(now, session_id)
    }

    /// Cache active routes and short negative answers. Deterministic query
    /// shards single-flight repeated capabilities and hard-bound concurrent
    /// consensus reads even when an unauthenticated caller sprays random UUIDs.
    /// Activation overwrites a prior miss immediately, and no cache entry can
    /// extend the exact durable lease boundary.
    #[cfg(test)]
    pub(crate) async fn route(
        &self,
        session_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        self.raw_route_before(
            session_id,
            tokio::time::Instant::now() + ROUTE_QUERY_DEADLINE,
        )
        .await
        .map(|route| route.filter(authorizing_route))
    }

    pub(crate) async fn route_resolution_before(
        &self,
        session_id: &str,
        local_node_id: &str,
        request_deadline: Instant,
    ) -> Result<DurableRouteResolution, StoreError> {
        let now = tokio::time::Instant::now();
        let remaining = request_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(StoreError::Database(
                "media-session route admission timed out".to_owned(),
            ));
        }
        let deadline = now + remaining.min(ROUTE_QUERY_DEADLINE);
        let route = self.raw_route_before(session_id, deadline).await?;
        validate_route_resolution(
            session_id,
            classify_durable_route(route, local_node_id, unix_ms()),
        )
    }

    /// Cache-bypassing durable resolution for a final HTTP status verdict.
    /// A one-second negative/old-owner cache is safe for ordinary routing but
    /// cannot prove 404 after exact publication ownership was rejected.
    pub(crate) async fn authoritative_route_resolution_before(
        &self,
        session_id: &str,
        local_node_id: &str,
        request_deadline: Instant,
    ) -> Result<DurableRouteResolution, StoreError> {
        self.reject_pending_release(session_id).await?;
        let now = tokio::time::Instant::now();
        let remaining = request_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(StoreError::Database(
                "media-session route reclassification timed out".to_owned(),
            ));
        }
        let deadline = now + remaining.min(ROUTE_QUERY_DEADLINE);
        let shard = route_hash(session_id) % self.route_queries.len();
        let _query = tokio::time::timeout_at(deadline, self.route_queries[shard].lock())
            .await
            .map_err(|_| {
                StoreError::Database("media-session route reclassification timed out".to_owned())
            })?;
        let route = tokio::time::timeout_at(deadline, self.store.media_session_route(session_id))
            .await
            .map_err(|_| {
                StoreError::Database("media-session route reclassification timed out".to_owned())
            })??;
        self.reject_pending_release(session_id).await?;
        validate_route_resolution(
            session_id,
            classify_durable_route(route, local_node_id, unix_ms()),
        )
    }

    async fn raw_route_before(
        &self,
        session_id: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        self.reject_pending_release(session_id).await?;
        if let Some(cached) = tokio::time::timeout_at(deadline, self.cached_route(session_id))
            .await
            .map_err(|_| {
                StoreError::Database("media-session route admission timed out".to_owned())
            })?
        {
            self.reject_pending_release(session_id).await?;
            return Ok(cached);
        }
        let hash = route_hash(session_id);
        let shard = hash % self.route_queries.len();
        let generation_shard = hash % self.route_generations.len();
        let _query = tokio::time::timeout_at(deadline, self.route_queries[shard].lock())
            .await
            .map_err(|_| {
                StoreError::Database("media-session route admission timed out".to_owned())
            })?;
        if let Some(cached) = tokio::time::timeout_at(deadline, self.cached_route(session_id))
            .await
            .map_err(|_| {
                StoreError::Database("media-session route admission timed out".to_owned())
            })?
        {
            self.reject_pending_release(session_id).await?;
            return Ok(cached);
        }
        let observed_generation =
            self.route_generations[generation_shard].load(std::sync::atomic::Ordering::Acquire);
        #[cfg(test)]
        self.route_store_queries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let route = tokio::time::timeout_at(deadline, self.store.media_session_route(session_id))
            .await
            .map_err(|_| {
                StoreError::Database("media-session route lookup timed out".to_owned())
            })??;
        let route = tokio::time::timeout_at(
            deadline,
            self.cache_queried_route_result(session_id, route, observed_generation),
        )
        .await
        .map_err(|_| StoreError::Database("media-session route lookup timed out".to_owned()))?;
        self.reject_pending_release(session_id).await?;
        Ok(route)
    }

    /// Authoritative, admission-bounded route read for a control mutation.
    /// Unlike media GET routing this deliberately bypasses the positive cache:
    /// an owner epoch may advance during its one-second TTL. The same query
    /// shards still prevent random capability probes from creating unbounded
    /// concurrent consensus reads.
    pub(crate) async fn control_route(
        &self,
        session_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        self.reject_pending_release(session_id).await?;
        let deadline = tokio::time::Instant::now() + ROUTE_QUERY_DEADLINE;
        let shard = route_hash(session_id) % self.route_queries.len();
        let _query = tokio::time::timeout_at(deadline, self.route_queries[shard].lock())
            .await
            .map_err(|_| {
                StoreError::Database("media-session control route admission timed out".to_owned())
            })?;
        let route = tokio::time::timeout_at(deadline, self.store.media_session_route(session_id))
            .await
            .map_err(|_| {
                StoreError::Database("media-session control route lookup timed out".to_owned())
            })??;
        self.reject_pending_release(session_id).await?;
        Ok(route)
    }

    /// Install a fail-closed publication fence before beginning the durable
    /// release mutation, or join the exact settlement already elected for
    /// this capability. A duplicate DELETE never consumes another worker
    /// permit and observes the elected transaction's eventual HTTP result.
    #[cfg(test)]
    pub(crate) async fn begin_release_reconciliation(&self, session_id: &str) -> ReleaseAdmission {
        self.begin_release_reconciliation_with_intent(session_id, ReleaseIntent::CLIENT)
            .await
    }

    /// First writer wins the terminal cause as well as the settlement. A
    /// concurrent DELETE/admin stop joins the exact transaction and cannot
    /// relabel its later reconciliation or peer acknowledgement.
    pub(crate) async fn begin_release_reconciliation_with_intent(
        &self,
        session_id: &str,
        intent: ReleaseIntent,
    ) -> ReleaseAdmission {
        let shard = route_hash(session_id) % self.release_fences.len();
        let mut releases = self.release_fences[shard]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(release) = releases.get(session_id) {
            return ReleaseAdmission::Joined(Arc::clone(&release.settlement));
        }
        if self
            .release_fence_count
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |count| {
                    (count < MAX_PENDING_RELEASE_RECONCILIATIONS).then_some(count.saturating_add(1))
                },
            )
            .is_err()
        {
            return ReleaseAdmission::Full;
        }
        let settlement = Arc::new(ReleaseSettlement::new());
        releases.insert(
            session_id.to_owned(),
            PendingReleaseReconciliation {
                failures: 0,
                next_attempt: tokio::time::Instant::now(),
                // The admitted HTTP owner performs the first exact mutation.
                in_flight: true,
                route_pending: true,
                owner_projection: None,
                projection_deadline: None,
                intent,
                settlement: Arc::clone(&settlement),
            },
        );
        drop(releases);
        let shard = route_hash(session_id) % self.route_generations.len();
        self.route_generations[shard].fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        ReleaseAdmission::Won(settlement)
    }

    /// Publish the exact durable tombstone before lifting the temporary
    /// release fence. Subsequent requests therefore see terminal state rather
    /// than falling back to a process-local actor in the cleanup gap.
    pub(crate) async fn complete_release_with_route(
        &self,
        route: MediaSessionRoute,
    ) -> Option<DurableReleaseProof> {
        // A durable proof must describe actual authoritative state, not a
        // process-local normalization. Treat a backend contract violation as
        // commit-unknown and retain the release generation fail closed.
        if route.state != "ended" {
            tracing::error!(
                session = %crate::transcode::session_log_id(&route.session_id),
                incarnation = %route.incarnation_id,
                state = %route.state,
                "Store returned a nonterminal route for media-session End"
            );
            return None;
        }
        // Ended rows reuse the publication field as durable terminal-owner
        // projection state: sentinel = not yet observed, finite = fallback
        // armed from this exact post-End observation, zero = acknowledged or
        // exact boundary proof complete. A process restart therefore resumes
        // one persisted interval instead of minting another forever.
        let route = if route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED {
            let observed_at_ms = unix_ms();
            let projection_safe_at_ms = observed_at_ms.saturating_add(
                i64::try_from(TERMINAL_PROJECTION_SAFETY_WINDOW.as_millis()).unwrap_or(i64::MAX),
            );
            match self
                .store
                .arm_media_session_terminal_projection(
                    &route.incarnation_id,
                    &route.owner_node_id,
                    route.owner_epoch,
                    projection_safe_at_ms,
                    observed_at_ms,
                )
                .await
            {
                Ok(Some(armed)) => armed,
                Ok(None) | Err(_) => return None,
            }
        } else {
            route
        };
        let session_id = route.session_id.clone();
        let terminal_projection_complete = route.publication_ready_at_ms == 0;
        let owner_projection = route.clone();
        self.cache_terminal_route(route).await;
        let shard = route_hash(&session_id) % self.release_fences.len();
        if let Some(release) = self.release_fences[shard]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&session_id)
        {
            release.route_pending = false;
            release.projection_deadline =
                (owner_projection.publication_ready_at_ms != 0).then(|| {
                    let remaining_ms = owner_projection
                        .publication_ready_at_ms
                        .saturating_sub(unix_ms())
                        .max(0);
                    tokio::time::Instant::now()
                        + Duration::from_millis(u64::try_from(remaining_ms).unwrap_or(u64::MAX))
                });
            release.owner_projection = Some(owner_projection);
            if let Some(terminal) = crate::vodserve::Terminal::from_durable_reason(
                release
                    .owner_projection
                    .as_ref()
                    .and_then(|route| route.terminal_reason.as_deref()),
            ) {
                release.intent = ReleaseIntent {
                    terminal,
                    reason: terminal.control_reason(),
                };
            }
        }
        Some(DurableReleaseProof {
            session_id,
            terminal_projection_complete,
        })
    }

    pub(crate) async fn complete_terminal_projection(
        &self,
        route: &MediaSessionRoute,
        proof: MediaSessionProjectionCompletion,
    ) -> Option<DurableReleaseProof> {
        let settled = self
            .store
            .complete_media_session_terminal_projection(
                &route.incarnation_id,
                &route.owner_node_id,
                route.owner_epoch,
                proof,
                unix_ms(),
            )
            .await
            .ok()??;
        self.complete_release_with_route(settled).await
    }

    /// Publish confirmed durable absence before lifting the temporary fence.
    /// This is the idempotent DELETE case; local capability cleanup is still
    /// performed by the owner before calling this method.
    pub(crate) async fn complete_release_absent(&self, session_id: &str) -> DurableReleaseProof {
        self.cache_route_result(session_id, None).await;
        let shard = route_hash(session_id) % self.release_fences.len();
        if let Some(release) = self.release_fences[shard]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(session_id)
        {
            release.route_pending = false;
        }
        DurableReleaseProof {
            session_id: session_id.to_owned(),
            terminal_projection_complete: true,
        }
    }

    /// Resolve every request joined to one release transaction and retire its
    /// bounded registry entry. Publication must already have a definitive
    /// terminal/absence cache before success is reported.
    pub(crate) async fn complete_release_settlement(
        &self,
        session_id: &str,
        expected: &Arc<ReleaseSettlement>,
        status: StatusCode,
    ) {
        let shard = route_hash(session_id) % self.release_fences.len();
        let mut releases = self.release_fences[shard]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if releases
            .get(session_id)
            .is_some_and(|release| Arc::ptr_eq(&release.settlement, expected))
        {
            // Publish the result while the exact election is still installed.
            // A new same-id request can only elect after every follower of
            // this transaction has an observable terminal value.
            expected.complete(status);
            releases.remove(session_id);
            self.release_fence_count
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }

    /// Transfer a commit-unknown first attempt from its HTTP task to the
    /// process lifecycle reconciler. The entry remains the publication fence;
    /// no additional task is created by a retrying DELETE for the same
    /// capability.
    pub(crate) async fn defer_release_reconciliation(
        &self,
        session_id: &str,
        expected: &Arc<ReleaseSettlement>,
    ) {
        let shard = route_hash(session_id) % self.release_fences.len();
        let mut releases = self.release_fences[shard]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(release) = releases.get_mut(session_id) else {
            return;
        };
        if !Arc::ptr_eq(&release.settlement, expected) {
            return;
        }
        release.failures = release.failures.saturating_add(1);
        release.next_attempt =
            tokio::time::Instant::now() + release_reconciliation_retry_delay(release.failures);
        release.in_flight = false;
    }

    async fn queue_release_owner_projection(
        &self,
        session_id: &str,
        expected: &Arc<ReleaseSettlement>,
    ) {
        let shard = route_hash(session_id) % self.release_fences.len();
        let mut releases = self.release_fences[shard]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(release) = releases.get_mut(session_id) else {
            return;
        };
        if Arc::ptr_eq(&release.settlement, expected) {
            release.next_attempt = tokio::time::Instant::now();
            release.in_flight = false;
        }
    }

    async fn take_release_reconciliation_candidates(
        &self,
    ) -> Vec<(
        String,
        Arc<ReleaseSettlement>,
        Option<MediaSessionRoute>,
        Option<tokio::time::Instant>,
        ReleaseIntent,
    )> {
        let now = tokio::time::Instant::now();
        let mut candidates = Vec::with_capacity(RELEASE_RECONCILIATION_FANOUT);
        for shard in self.release_fences.iter() {
            if candidates.len() >= RELEASE_RECONCILIATION_FANOUT {
                break;
            }
            let mut releases = shard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let selected = releases
                .iter()
                .filter(|(_, release)| !release.in_flight && release.next_attempt <= now)
                .take(RELEASE_RECONCILIATION_FANOUT - candidates.len())
                .map(|(session_id, release)| {
                    (
                        session_id.clone(),
                        Arc::clone(&release.settlement),
                        release.owner_projection.clone(),
                        release.projection_deadline,
                        release.intent,
                    )
                })
                .collect::<Vec<_>>();
            for (session_id, _, _, _, _) in &selected {
                if let Some(release) = releases.get_mut(session_id) {
                    release.in_flight = true;
                }
            }
            candidates.extend(selected);
        }
        candidates
    }

    async fn reject_pending_release(&self, session_id: &str) -> Result<(), StoreError> {
        let shard = route_hash(session_id) % self.release_fences.len();
        if self.release_fences[shard]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .is_some_and(|release| release.route_pending)
        {
            return Err(StoreError::Database(
                "media-session release reconciliation in progress".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn cache_route(&self, route: MediaSessionRoute) {
        let session_id = route.session_id.clone();
        self.cache_route_result(&session_id, Some(route)).await;
    }

    fn route_generation(&self, session_id: &str) -> u64 {
        let shard = route_hash(session_id) % self.route_generations.len();
        self.route_generations[shard].load(std::sync::atomic::Ordering::Acquire)
    }

    /// Publish an active route only if no activation or terminal owner changed
    /// this cache shard after the authoritative operation began. Release
    /// admission advances the generation before its Store End, so a delayed
    /// takeover cannot overwrite the tombstone it eventually publishes.
    async fn cache_route_if_generation(
        &self,
        route: MediaSessionRoute,
        observed_generation: u64,
    ) -> bool {
        let session_id = route.session_id.clone();
        let now = tokio::time::Instant::now();
        let mut routes = self.routes.lock().await;
        routes.retain(|_, cached| cached.expires_at > now);
        let shard = route_hash(&session_id) % self.route_generations.len();
        if self.route_generations[shard].load(std::sync::atomic::Ordering::Acquire)
            != observed_generation
        {
            return false;
        }
        self.route_generations[shard].fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        insert_cached_route(&mut routes, &session_id, Some(route), now);
        true
    }

    /// Publish a durable end as a typed terminal cache entry before process-
    /// local cleanup begins. This suppresses both a prior active owner and an
    /// in-flight stale query without turning the tombstone into an absence
    /// that could fall through to a still-live local actor.
    pub(crate) async fn cache_terminal_route(&self, mut route: MediaSessionRoute) {
        if route.state == "active" {
            // Fail closed even if a backend violates `end_media_session`'s
            // return contract. The exact owner/incarnation facts remain
            // intact; only publication authority is normalized terminal.
            route.state = "ended".to_owned();
        }
        route.lease_expires_at_ms = route.lease_expires_at_ms.min(unix_ms());
        self.cache_route(route).await;
    }

    async fn cached_route(&self, session_id: &str) -> Option<Option<MediaSessionRoute>> {
        let now = tokio::time::Instant::now();
        let mut routes = self.routes.lock().await;
        let cached = routes.get(session_id)?;
        if cached.expires_at > now {
            return Some(cached.route.clone());
        }
        routes.remove(session_id);
        None
    }

    async fn cache_route_result(&self, session_id: &str, route: Option<MediaSessionRoute>) {
        let now = tokio::time::Instant::now();
        let mut routes = self.routes.lock().await;
        routes.retain(|_, cached| cached.expires_at > now);
        let generation_shard = route_hash(session_id) % self.route_generations.len();
        self.route_generations[generation_shard].fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        insert_cached_route(&mut routes, session_id, route, now);
    }

    /// Publish a Store lookup only if no activation or terminal transition
    /// produced a newer authoritative cache entry while the read was in
    /// flight. This closes the miss-before-activation race without making
    /// activation wait behind a potentially slow consensus lookup.
    async fn cache_queried_route_result(
        &self,
        session_id: &str,
        route: Option<MediaSessionRoute>,
        observed_generation: u64,
    ) -> Option<MediaSessionRoute> {
        let now = tokio::time::Instant::now();
        let mut routes = self.routes.lock().await;
        routes.retain(|_, cached| cached.expires_at > now);
        let generation_shard = route_hash(session_id) % self.route_generations.len();
        if self.route_generations[generation_shard].load(std::sync::atomic::Ordering::Acquire)
            != observed_generation
        {
            return routes
                .get(session_id)
                .and_then(|cached| cached.route.clone());
        }
        if let Some(cached) = routes.get(session_id) {
            return cached.route.clone();
        }
        insert_cached_route(&mut routes, session_id, route.clone(), now);
        route
    }

    /// Hand a freshly activated local worker to the renewal loop before its
    /// next store inventory. If the store disappears in that exact window,
    /// the worker still knows the committed expiry and self-fences on time.
    pub(crate) async fn seed_owned_lease(&self, route: &MediaSessionRoute) {
        if route.state != "active"
            || route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED
        {
            debug_assert!(
                false,
                "only confirmed active media-session routes may enter lease accounting"
            );
            return;
        }
        self.lease_seeds.lock().await.insert(
            route.incarnation_id.clone(),
            (
                route.session_id.clone(),
                route.owner_epoch,
                route.lease_expires_at_ms,
                durable_route_is_vod(route),
            ),
        );
    }

    async fn take_lease_seeds(&self) -> HashMap<String, LeaseSeed> {
        std::mem::take(&mut *self.lease_seeds.lock().await)
    }

    async fn peer_base(
        &self,
        node_id: &str,
        deadline: tokio::time::Instant,
    ) -> Result<String, PeerTransportError> {
        tokio::time::timeout_at(deadline, self.membership.media_peers())
            .await
            .map_err(|_| PeerTransportError::TimedOut)?
            .map_err(|_| PeerTransportError::Unreachable)?
            .into_iter()
            .find(|peer| peer.node_id == node_id && peer.reachable)
            .and_then(|peer| peer.http_base)
            .ok_or(PeerTransportError::Unreachable)
    }

    pub(crate) async fn start_remote(
        &self,
        owner_node_id: &str,
        request: &RemoteStartRequest,
        deadline: tokio::time::Instant,
    ) -> Result<RemoteSessionStart, PeerTransportError> {
        let body = serde_json::to_vec(request).map_err(|_| PeerTransportError::InvalidResponse)?;
        if body.len() > MAX_CONTROL_REQUEST_BYTES {
            return Err(PeerTransportError::InvalidResponse);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PeerTransportError::TimedOut);
        }
        let base = self.peer_base(owner_node_id, deadline).await?;
        let response = self
            .transport
            .request_with_static_header(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                START_PATH,
                body,
                deadline,
                MAX_START_RESPONSE_BYTES,
                PeerAuthMode::ExactRequest,
                (REMOTE_START_OWNERSHIP_HEADER, REMOTE_START_OWNERSHIP_V1),
            )
            .await?;
        decode_remote_start_response(response)
    }

    pub(crate) async fn activate_remote(
        &self,
        owner_node_id: &str,
        request: &RemoteActivateRequest,
        deadline: tokio::time::Instant,
    ) -> Result<(), PeerTransportError> {
        let body = serde_json::to_vec(request).map_err(|_| PeerTransportError::InvalidResponse)?;
        if body.len() > MAX_ACTIVATION_REQUEST_BYTES || tokio::time::Instant::now() >= deadline {
            return Err(PeerTransportError::InvalidResponse);
        }
        let base = self.peer_base(owner_node_id, deadline).await?;
        let response = self
            .transport
            .request(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                ACTIVATE_PATH,
                body,
                deadline,
                4 * 1024,
                PeerAuthMode::ExactRequest,
            )
            .await?;
        if matches!(
            response.status,
            reqwest::StatusCode::NO_CONTENT | reqwest::StatusCode::ACCEPTED
        ) {
            Ok(())
        } else if response.status == reqwest::StatusCode::REQUEST_TIMEOUT {
            Err(PeerTransportError::TimedOut)
        } else {
            Err(PeerTransportError::InvalidResponse)
        }
    }

    pub(crate) async fn abort_remote(
        &self,
        owner_node_id: &str,
        request: &RemoteAbortRequest,
    ) -> Result<(), PeerTransportError> {
        let deadline = deadline_after(ABORT_DEADLINE);
        let base = self.peer_base(owner_node_id, deadline).await?;
        let body = serde_json::to_vec(request).map_err(|_| PeerTransportError::InvalidResponse)?;
        let mut response = self
            .transport
            .request(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                ABORT_PATH,
                body,
                deadline,
                1_024,
                PeerAuthMode::ExactRequest,
            )
            .await?;
        // The original strict envelope had no `reason`. An old
        // deny_unknown_fields endpoint rejects the new request before
        // settlement, so retry the same exact/idempotent abort once with the
        // legacy body. New peers consume the first request and preserve the
        // honest release/supersession reason.
        if let Some(body) = remote_abort_legacy_retry_body(request, response.status)? {
            response = self
                .transport
                .request(
                    owner_node_id,
                    &base,
                    reqwest::Method::POST,
                    ABORT_PATH,
                    body,
                    deadline,
                    1_024,
                    PeerAuthMode::ExactRequest,
                )
                .await?;
        }
        if response.status.is_success() {
            Ok(())
        } else {
            Err(PeerTransportError::InvalidResponse)
        }
    }

    pub(crate) async fn relay(
        &self,
        owner_node_id: &str,
        request: &RelayRequest,
    ) -> Result<Response<Body>, PeerTransportError> {
        let body = serde_json::to_vec(request).map_err(|_| PeerTransportError::InvalidResponse)?;
        if body.len() > MAX_CONTROL_REQUEST_BYTES {
            return Err(PeerTransportError::InvalidResponse);
        }
        let now = tokio::time::Instant::now();
        let budget = request
            .transport_budget_at(unix_ms())
            .ok_or(PeerTransportError::TimedOut)?;
        let deadline = now + budget;
        let base = self.peer_base(owner_node_id, deadline).await?;
        let response = self
            .transport
            .request_stream(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                RELAY_PATH,
                body,
                deadline,
                PeerAuthMode::ExactRequest,
            )
            .await?;
        relay_response(response)
    }

    /// Mutating playback control uses its own exact-auth endpoint. It must not
    /// inherit the generic relay's read authorization merely because the M1
    /// action happens to be `none`.
    pub(crate) async fn control(
        &self,
        owner_node_id: &str,
        request: &crate::playback_control::ControlRelayRequest,
    ) -> Result<Response<Body>, PeerTransportError> {
        let mut relay_metric = crate::playback_control::RelayMetricGuard::new();
        let body = serde_json::to_vec(request).map_err(|_| {
            relay_metric.invalid_response();
            PeerTransportError::InvalidResponse
        })?;
        if body.len() > crate::playback_control::MAX_RELAY_BYTES {
            relay_metric.invalid_response();
            return Err(PeerTransportError::InvalidResponse);
        }
        let budget =
            crate::playback_control::inherited_exchange_budget(request.deadline_unix_ms, unix_ms())
                .ok_or(PeerTransportError::TimedOut)?;
        let deadline = deadline_after(budget);
        let base = self.peer_base(owner_node_id, deadline).await?;
        let response = self
            .transport
            .request(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                CONTROL_PATH,
                body,
                deadline,
                crate::playback_control::MAX_RESPONSE_BYTES,
                PeerAuthMode::ExactRequest,
            )
            .await?;
        let response = validated_control_relay_response(response, request).inspect_err(|_| {
            relay_metric.invalid_response();
        })?;
        relay_metric.valid_response();
        Ok(response)
    }
}

fn remote_abort_legacy_retry_body(
    request: &RemoteAbortRequest,
    status: reqwest::StatusCode,
) -> Result<Option<Vec<u8>>, PeerTransportError> {
    if request.reason.is_none()
        || !matches!(
            status,
            reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY
        )
    {
        return Ok(None);
    }
    serde_json::to_vec(&request.without_reason())
        .map(Some)
        .map_err(|_| PeerTransportError::InvalidResponse)
}

fn validated_control_relay_response(
    response: PeerResponse,
    request: &crate::playback_control::ControlRelayRequest,
) -> Result<Response<Body>, PeerTransportError> {
    let status = StatusCode::from_u16(response.status.as_u16())
        .map_err(|_| PeerTransportError::InvalidResponse)?;
    let body = if status.is_success() {
        let parsed =
            serde_json::from_slice::<crate::playback_control::ControlResponseV1>(&response.body)
                .ok()
                .filter(|parsed| parsed.is_valid_for(request))
                .ok_or(PeerTransportError::InvalidResponse)?;
        serde_json::to_vec(&parsed).map_err(|_| PeerTransportError::InvalidResponse)?
    } else {
        let parsed =
            serde_json::from_slice::<crate::playback_control::ControlErrorBody>(&response.body)
                .ok()
                .filter(|parsed| parsed.is_valid_for_status(status.as_u16()))
                .ok_or(PeerTransportError::InvalidResponse)?;
        serde_json::to_vec(&parsed).map_err(|_| PeerTransportError::InvalidResponse)?
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .map_err(|_| PeerTransportError::InvalidResponse)
}

fn insert_cached_route(
    routes: &mut HashMap<String, CachedRoute>,
    session_id: &str,
    route: Option<MediaSessionRoute>,
    now: tokio::time::Instant,
) {
    if routes.len() >= MAX_ROUTE_CACHE_ENTRIES && !routes.contains_key(session_id) {
        if let Some(oldest) = routes
            .iter()
            .min_by_key(|(_, cached)| cached.expires_at)
            .map(|(session_id, _)| session_id.clone())
        {
            routes.remove(&oldest);
        }
    }
    routes.insert(
        session_id.to_owned(),
        CachedRoute {
            route,
            expires_at: now + ROUTE_CACHE_TTL,
        },
    );
}

fn route_hash(session_id: &str) -> usize {
    let mut hasher = DefaultHasher::new();
    session_id.hash(&mut hasher);
    hasher.finish() as usize
}

#[cfg(test)]
fn authorizing_route(route: &MediaSessionRoute) -> bool {
    let now_ms = unix_ms();
    route.state == "active"
        && route.lease_expires_at_ms > now_ms
        && route.publication_ready_at_ms == 0
}

fn classify_durable_route(
    route: Option<MediaSessionRoute>,
    local_node_id: &str,
    now_unix_ms: i64,
) -> DurableRouteResolution {
    match route {
        None => DurableRouteResolution::Absent,
        Some(route) if route.state != "active" => DurableRouteResolution::Terminal(route),
        // The successor owns and renews its lease, but cannot publish until
        // the old generation acknowledges supersession or the hard lifetime
        // of every previously admitted response has elapsed. Reuse the
        // retryable transition classification so every public/internal media
        // path fails closed without inventing a second routing destination.
        Some(route) if route.publication_ready_at_ms != 0 => {
            DurableRouteResolution::OwnerTransition(route)
        }
        Some(route) if route.lease_expires_at_ms <= now_unix_ms => {
            DurableRouteResolution::OwnerTransition(route)
        }
        Some(route) if route.owner_node_id == local_node_id => {
            DurableRouteResolution::ActiveLocal(route)
        }
        Some(route) => DurableRouteResolution::ActiveRemote(route),
    }
}

/// What a surviving node can honestly say about a route classified as
/// [`DurableRouteResolution::OwnerTransition`] (plan §10.3, hard owner loss).
///
/// **This is deliberately not a stopwatch.** The obvious design — answer
/// "retry" for some grace period after the lease expires, then declare the
/// owner lost — cannot be made correct here. `takeover_loop` is gated on
/// `remote_rollout_ready`, which requires every voter reachable with a fresh
/// snapshot, so the moment a node dies the gate that would replace its
/// sessions *shuts*, and it stays shut until the dead node is removed from
/// membership or comes back. A rebooting node's routes are adopted minutes
/// later, long after any plausible grace, and any deadline chosen in advance
/// would have told a viewer their session was over while a successor was on
/// its way. The scan is paged (`TAKEOVER_BATCH` per `TAKEOVER_INTERVAL`,
/// behind a semaphore of the same size), so a node dying with more sessions
/// than one page also outlives any fixed bound.
///
/// What *is* decidable is whether this route can ever be taken over at all.
/// The recipe is durable and immutable, and the refusals in
/// [`takeover_recipe_matches_route`] and `attempt_takeover` read only it. A
/// route those refuse is not waiting for anything, and that verdict does not
/// change however long the client waits — which is the common case, not the
/// exotic one: VOD and EVENT sessions are refused *by design* (§4 forbids
/// expanding the compatibility path to them), and a session created while
/// `cluster.session_takeover_enabled` was off recorded
/// `typeless_playlist: false` and is refused too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerLoss {
    /// The route is eligible for takeover, so a successor may still arrive.
    /// Retryable — for as long as the client is willing to wait, which is the
    /// client's decision to make and not the server's to guess.
    Transitioning(OwnerLossResume),
    /// The route's own persisted recipe can never be taken over. No successor
    /// is coming, and waiting cannot change that.
    Unrecoverable(OwnerLossResume),
}

impl OwnerLoss {
    /// Test convenience. Production always matches both arms, because the
    /// answer differs by more than the resume it carries.
    #[cfg(test)]
    pub(crate) fn resume(self) -> OwnerLossResume {
        match self {
            OwnerLoss::Transitioning(resume) | OwnerLoss::Unrecoverable(resume) => resume,
        }
    }
}

/// Where a reopen of a lost session should land on the source timeline.
///
/// Deliberately *not* an offer of continuity. §10.3 requires the no-snapshot
/// fallback to hand back the position and the fetched frontier and then
/// require a normal reopen — "do not guess transparency". These numbers make
/// the reopen land in the right place; nothing here claims the seam is
/// invisible, and a recovered session carries a discontinuity either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnerLossResume {
    /// Absolute source-timeline position to reopen at — the fetched frontier
    /// pulled back by a whole segment of this session's shape, exactly as
    /// [`takeover_resume`] pulls a successor back, and for the same reason:
    /// `fetched_through_ms` advances to the *end* of a segment the moment the
    /// client asks for it, so it is ahead of what the viewer actually saw.
    /// Landing early repeats seen media; landing late skips media nobody ever
    /// showed them.
    pub film_position_ms: i64,
    /// Absolute source-timeline position production had reached. Bounded
    /// below by `film_position_ms`, which is where the resume is clamped.
    pub film_frontier_ms: i64,
}

/// Decide whether an owner transition can still be answered by a successor.
///
/// `now_unix_ms` reads the lease, and only the lease — this is not a deadline
/// and nothing is timed from it. [`classify_durable_route`] raises
/// `OwnerTransition` for two different situations, and **a live lease is a
/// live owner**: a committed replacement holds one for the whole of its
/// publication window while it owns and renews but may not yet publish. Only
/// a lease that has actually stopped being renewed can be loss, and no
/// recipe makes a renewing owner dead.
pub(crate) fn classify_owner_loss(route: &MediaSessionRoute, now_unix_ms: i64) -> OwnerLoss {
    let envelope = serde_json::from_str::<RemoteStartRequest>(&route.recipe_json)
        .ok()
        .filter(|envelope| takeover_recipe_matches_route(envelope, route));
    let resume = owner_loss_resume(route, envelope.as_ref().map(|e| &e.request.kind));
    if route.lease_expires_at_ms > now_unix_ms {
        return OwnerLoss::Transitioning(resume);
    }
    let eligible = envelope.is_some_and(|envelope| {
        // The same three row-readable refusals `attempt_takeover` applies.
        // The source-revision check is left out on purpose: it needs a Store
        // read, and every fact this function is allowed to be wrong about
        // must fall toward "a successor may still arrive".
        envelope.typeless_playlist
            && route
                .owner_epoch
                .checked_add(1)
                .and_then(|next_epoch| takeover_start_number(route.media_sequence, next_epoch))
                .is_some()
    });
    if eligible {
        OwnerLoss::Transitioning(resume)
    } else {
        OwnerLoss::Unrecoverable(resume)
    }
}

/// How far behind the fetched frontier a resume lands, for a session of this
/// shape. Shared with [`takeover_resume`] so the position offered to a client
/// and the position a successor restarts from cannot drift apart.
fn resume_overlap_ms(kind: Option<&SessionKind>) -> i64 {
    let segment_ms = match kind {
        Some(SessionKind::Transcode { .. }) => i64::from(plurx_core::transcode::SEGMENT_SECONDS),
        // A copy session, or a recipe that would not parse: the wider of the
        // two shapes, because being early is the recoverable mistake.
        Some(SessionKind::Copy { .. }) | None => {
            i64::from(plurx_core::transcode::COPY_SEGMENT_MAX_SECS)
        }
    };
    segment_ms
        .saturating_mul(1_000)
        .saturating_add(TAKEOVER_OVERLAP_MARGIN_MS)
}

fn owner_loss_resume(route: &MediaSessionRoute, kind: Option<&SessionKind>) -> OwnerLossResume {
    let frontier_offset_ms = route
        .fetched_through_ms
        .saturating_sub(resume_overlap_ms(kind))
        .clamp(0, route.produced_playable_through_ms.max(0));
    OwnerLossResume {
        film_position_ms: route.media_origin_ms.saturating_add(frontier_offset_ms),
        film_frontier_ms: route
            .media_origin_ms
            .saturating_add(route.produced_playable_through_ms.max(frontier_offset_ms)),
    }
}

type LeaseSeed = (String, i64, i64, bool);

fn validate_route_resolution(
    session_id: &str,
    resolution: DurableRouteResolution,
) -> Result<DurableRouteResolution, StoreError> {
    let route = match &resolution {
        DurableRouteResolution::Absent => None,
        DurableRouteResolution::ActiveLocal(route)
        | DurableRouteResolution::ActiveRemote(route)
        | DurableRouteResolution::OwnerTransition(route)
        | DurableRouteResolution::Terminal(route) => Some(route),
    };
    if route.is_some_and(|route| route.session_id.as_str() != session_id) {
        return Err(StoreError::Database(
            "media-session route identity mismatch".to_owned(),
        ));
    }
    Ok(resolution)
}

fn relay_response(response: reqwest::Response) -> Result<Response<Body>, PeerTransportError> {
    relay_response_with_limits(
        response,
        MAX_ADMITTED_MEDIA_BODY_LIFETIME,
        MEDIA_BODY_NO_PROGRESS_TIMEOUT,
    )
}

fn relay_response_with_limits(
    response: reqwest::Response,
    max_lifetime: Duration,
    no_progress_timeout: Duration,
) -> Result<Response<Body>, PeerTransportError> {
    relay_response_with_limits_observed(response, max_lifetime, no_progress_timeout, None, None)
}

struct RelayPumpCompletion(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for RelayPumpCompletion {
    fn drop(&mut self) {
        if let Some(completion) = self.0.take() {
            let _ = completion.send(());
        }
    }
}

#[derive(Clone)]
struct RelayBodyTerminal {
    failure: Arc<StdMutex<Option<(io::ErrorKind, String)>>>,
    signal: tokio_util::sync::CancellationToken,
}

impl RelayBodyTerminal {
    fn new() -> Self {
        Self {
            failure: Arc::new(StdMutex::new(None)),
            signal: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn fail(&self, kind: io::ErrorKind, message: String) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failure.is_none() {
            *failure = Some((kind, message));
        }
        drop(failure);
        self.signal.cancel();
    }

    fn take_error(&self) -> Option<io::Error> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(|(kind, message)| io::Error::new(kind, message))
    }
}

fn relay_response_with_limits_observed(
    response: reqwest::Response,
    max_lifetime: Duration,
    no_progress_timeout: Duration,
    completion: Option<tokio::sync::oneshot::Sender<()>>,
    upstream_pulls: Option<Arc<std::sync::atomic::AtomicUsize>>,
) -> Result<Response<Body>, PeerTransportError> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .map_err(|_| PeerTransportError::InvalidResponse)?;
    let mut builder = Response::builder().status(status);
    for name in [
        header::CONTENT_TYPE,
        header::CACHE_CONTROL,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
        header::ETAG,
        header::LAST_MODIFIED,
        header::CONTENT_DISPOSITION,
    ] {
        if let Some(value) = response.headers().get(name.as_str()) {
            let name = HeaderName::from_bytes(name.as_str().as_bytes())
                .map_err(|_| PeerTransportError::InvalidResponse)?;
            builder = builder.header(name, value.as_bytes());
        }
    }
    let mut upstream = Box::pin(response.bytes_stream());
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<Result<Bytes, io::Error>>(RELAY_BODY_CHANNEL_CAPACITY);
    let terminal = RelayBodyTerminal::new();
    // Header admission consumed the inherited request/preparation deadline.
    // This independently driven task is the sole post-header network owner:
    // it advances both timers even when downstream never polls, reads at most
    // the bounded channel capacity ahead, and observes receiver Drop through
    // `closed()`/a failed send so abandoning the public body cancels the peer.
    let body_deadline = tokio::time::Instant::now() + max_lifetime;
    let pump_terminal = terminal.clone();
    tokio::spawn(async move {
        let _completion = RelayPumpCompletion(completion);
        let fail = |kind, message: String| pump_terminal.fail(kind, message);
        loop {
            // Reserve bounded downstream capacity before asking reqwest for
            // another chunk. This keeps the complete relay allocation at the
            // channel limit: there is never an extra Bytes value suspended in
            // a blocked send future beyond the advertised queue capacity.
            let reserve_deadline =
                (tokio::time::Instant::now() + no_progress_timeout).min(body_deadline);
            let permit = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    fail(
                        io::ErrorKind::TimedOut,
                        "relayed media response exceeded its post-header total lifetime".to_owned(),
                    );
                    return;
                }
                _ = tokio::time::sleep_until(reserve_deadline) => {
                    fail(
                        io::ErrorKind::TimedOut,
                        "relayed media response exceeded its post-header downstream no-progress deadline".to_owned(),
                    );
                    return;
                }
                permit = sender.reserve() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return,
                },
            };
            let progress_deadline =
                (tokio::time::Instant::now() + no_progress_timeout).min(body_deadline);
            if let Some(pulls) = upstream_pulls.as_ref() {
                pulls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            }
            let next = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    fail(
                        io::ErrorKind::TimedOut,
                        "relayed media response exceeded its post-header total lifetime".to_owned(),
                    );
                    return;
                }
                _ = sender.closed() => return,
                _ = tokio::time::sleep_until(progress_deadline) => {
                    fail(
                        io::ErrorKind::TimedOut,
                        "relayed media response exceeded its post-header no-progress deadline".to_owned(),
                    );
                    return;
                }
                next = upstream.next() => next,
            };
            let bytes = match next {
                Some(Ok(bytes)) => bytes,
                Some(Err(error)) => {
                    fail(io::ErrorKind::Other, error.to_string());
                    return;
                }
                None => return,
            };
            permit.send(Ok(bytes));
        }
    });

    let stream = stream::unfold(
        (receiver, terminal, body_deadline, false),
        |(mut receiver, terminal, body_deadline, finished)| async move {
            if finished {
                return None;
            }
            if let Some(error) = terminal.take_error() {
                return Some((Err(error), (receiver, terminal, body_deadline, true)));
            }
            if tokio::time::Instant::now() >= body_deadline {
                return Some((
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "relayed media response exceeded its post-header total lifetime",
                    )),
                    (receiver, terminal, body_deadline, true),
                ));
            }
            let item = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    return Some((
                        Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "relayed media response exceeded its post-header total lifetime",
                        )),
                        (receiver, terminal, body_deadline, true),
                    ));
                }
                () = terminal.signal.cancelled() => {
                    let error = terminal
                        .take_error()
                        .unwrap_or_else(|| io::Error::other("relayed media response producer failed"));
                    return Some((Err(error), (receiver, terminal, body_deadline, true)));
                }
                item = receiver.recv() => item,
            };
            let Some(item) = item else {
                if let Some(error) = terminal.take_error() {
                    return Some((Err(error), (receiver, terminal, body_deadline, true)));
                }
                return None;
            };
            if tokio::time::Instant::now() >= body_deadline || terminal.signal.is_cancelled() {
                let error = terminal.take_error().unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "relayed media response exceeded its post-header total lifetime",
                    )
                });
                return Some((Err(error), (receiver, terminal, body_deadline, true)));
            }
            Some((item, (receiver, terminal, body_deadline, false)))
        },
    );
    builder
        .body(Body::from_stream(stream))
        .map_err(|_| PeerTransportError::InvalidResponse)
}

pub(crate) fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn durable_route_is_vod(route: &MediaSessionRoute) -> bool {
    serde_json::from_str::<RemoteStartRequest>(&route.recipe_json).map_or(true, |request| {
        request.request.presentation == crate::transcode::Presentation::Vod
    })
}

fn durable_terminal_or_replaced(route: &MediaSessionRoute) -> crate::vodserve::Terminal {
    crate::vodserve::Terminal::from_durable_reason(route.terminal_reason.as_deref())
        .unwrap_or(crate::vodserve::Terminal::Replaced)
}

async fn fence_and_reap_sessions(
    state: &AppState,
    sessions: Vec<(String, String, i64, bool, &'static str)>,
) {
    if sessions.is_empty() {
        return;
    }
    futures_util::future::join_all(sessions.into_iter().map(
        |(incarnation_id, session_id, owner_epoch, known_vod, reason)| async move {
            if known_vod || state.transcode.vod_owns_or_preparing(&session_id).await {
                // A lease-loss observation closes publication immediately,
                // but it cannot choose a typed VOD tombstone ahead of the
                // Store's first-writer terminal decision. Exact ended state
                // supplies that cause; definitive absence has no competing
                // durable winner. Active/unknown state remains cause-neutral
                // for the bounded stale-settlement owner below to resolve.
                state
                    .transcode
                    .begin_session_publication_fence(&session_id)
                    .await;
                match tokio::time::timeout(
                    LEASE_RENEWAL_DEADLINE,
                    state
                        .store
                        .media_session_route_by_incarnation(&incarnation_id),
                )
                .await
                {
                    Ok(Ok(Some(route)))
                        if route.session_id == session_id
                            && route.owner_epoch == owner_epoch
                            && route.state == "ended" =>
                    {
                        let terminal = durable_terminal_or_replaced(&route);
                        state
                            .transcode
                            .begin_session_terminal(
                                &session_id,
                                terminal,
                                terminal.control_reason(),
                            )
                            .await;
                        if let Some(initial) = state
                            .media_sessions
                            .complete_release_with_route(route.clone())
                            .await
                        {
                            let proof = if initial.terminal_projection_complete() {
                                initial
                            } else {
                                state
                                    .media_sessions
                                    .complete_terminal_projection(
                                        &route,
                                        MediaSessionProjectionCompletion::PredecessorAcknowledged,
                                    )
                                    .await
                                    .unwrap_or(initial)
                            };
                            state.transcode.complete_session_release_durable(&proof);
                        }
                    }
                    Ok(Ok(None)) => {
                        state
                            .transcode
                            .begin_session_terminal(
                                &session_id,
                                crate::vodserve::Terminal::Replaced,
                                reason,
                            )
                            .await;
                        state.transcode.complete_session_release(&session_id);
                    }
                    Ok(Ok(Some(_))) | Ok(Err(_)) | Err(_) => {}
                }
                return;
            }
            // Rolling takeover deliberately reuses the public id. Fence only
            // the exact lost incarnation; a delayed cleanup must not poison or
            // remove a legitimate successor epoch.
            if state
                .transcode
                .fence_session_for_owner(&incarnation_id, &session_id, owner_epoch)
                .await
            {
                let cleanup_state = state.clone();
                tokio::spawn(async move {
                    cleanup_state
                        .transcode
                        .stop_session_for_owner(&incarnation_id, &session_id, owner_epoch, reason)
                        .await;
                });
            }
        },
    ))
    .await;
}

async fn persist_terminal_projection(
    state: &AppState,
    route: &MediaSessionRoute,
    proof: MediaSessionProjectionCompletion,
) -> bool {
    let Some(durable_release) = state
        .media_sessions
        .complete_terminal_projection(route, proof)
        .await
        .filter(DurableReleaseProof::terminal_projection_complete)
    else {
        tracing::warn!(
            session = %crate::transcode::session_log_id(&route.session_id),
            incarnation = %route.incarnation_id,
            owner = %route.owner_node_id,
            owner_epoch = route.owner_epoch,
            "terminal-owner projection proof remains commit-unknown"
        );
        return false;
    };
    state
        .transcode
        .complete_session_release_durable(&durable_release);
    true
}

async fn cleanup_reconciled_release(
    state: &AppState,
    route: &MediaSessionRoute,
    projection_deadline: tokio::time::Instant,
    intent: ReleaseIntent,
) -> bool {
    if route.publication_ready_at_ms == 0 {
        return true;
    }
    if route.owner_node_id == state.node_id {
        state
            .transcode
            .begin_session_terminal(&route.session_id, intent.terminal, intent.reason)
            .await;
        return persist_terminal_projection(
            state,
            route,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
        )
        .await;
    }
    if let Err(error) = state
        .media_sessions
        .abort_remote(
            &route.owner_node_id,
            &RemoteAbortRequest {
                incarnation_id: route.incarnation_id.clone(),
                session_id: route.session_id.clone(),
                expected_owner_epoch: route.owner_epoch,
                reason: Some(intent.reason.to_owned()),
            },
        )
        .await
    {
        // The durable tombstone has already been published locally, so bytes
        // remain fenced. Lease expiry/reconciliation is the crash/partition
        // backstop for a peer that cannot receive this exact abort.
        tracing::warn!(
            error = ?error,
            session = %crate::transcode::session_log_id(&route.session_id),
            owner = %route.owner_node_id,
            "reconciled media-session release could not reach its exact owner"
        );
        if tokio::time::Instant::now() >= projection_deadline {
            tracing::warn!(
                session = %crate::transcode::session_log_id(&route.session_id),
                owner = %route.owner_node_id,
                safety_window_seconds = TERMINAL_PROJECTION_SAFETY_WINDOW.as_secs(),
                "settling durable release after the response-lifetime safety window without owner acknowledgement"
            );
            return persist_terminal_projection(
                state,
                route,
                MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                    expected_not_before_ms: route.publication_ready_at_ms,
                },
            )
            .await;
        }
        return false;
    }
    persist_terminal_projection(
        state,
        route,
        MediaSessionProjectionCompletion::PredecessorAcknowledged,
    )
    .await
}

/// Reconcile only exact locally owned generations. This is part of the
/// existing owner lease lifecycle, so crash recovery and takeover naturally
/// resume a blocked handoff without a second watchdog registry.
async fn reconcile_owned_publication_fences(
    state: &AppState,
    leases: &[OwnedMediaSessionLease],
    live: &HashSet<String>,
) {
    let candidates = leases
        .iter()
        .filter(|lease| live.contains(&lease.session_id))
        .cloned()
        .collect::<Vec<_>>();
    stream::iter(candidates)
        .for_each_concurrent(LEASE_RENEWAL_FANOUT, |lease| {
            let state = state.clone();
            async move {
                let deadline = tokio::time::Instant::now() + LEASE_RENEWAL_DEADLINE;
                let route = match tokio::time::timeout_at(
                    deadline,
                    state
                        .store
                        .media_session_route_by_incarnation(&lease.incarnation_id),
                )
                .await
                {
                    Ok(Ok(Some(route)))
                        if route.incarnation_id == lease.incarnation_id
                            && route.session_id == lease.session_id
                            && route.owner_node_id == state.node_id
                            && route.owner_epoch == lease.owner_epoch
                            && route.state == "active" =>
                    {
                        route
                    }
                    Ok(Ok(_)) => return,
                    Ok(Err(error)) => {
                        tracing::debug!(
                            %error,
                            incarnation = %lease.incarnation_id,
                            "owned publication-fence route read remains unavailable"
                        );
                        return;
                    }
                    Err(_) => return,
                };
                if route.publication_ready_at_ms == 0 {
                    return;
                }
                let now_ms = unix_ms();
                let outcome = if route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED
                {
                    let not_before_ms = now_ms.saturating_add(
                        i64::try_from(TERMINAL_PROJECTION_SAFETY_WINDOW.as_millis())
                            .unwrap_or(i64::MAX),
                    );
                    tokio::time::timeout_at(
                        deadline,
                        state.store.arm_media_session_handoff(
                            &route.incarnation_id,
                            &route.owner_node_id,
                            route.owner_epoch,
                            not_before_ms,
                            now_ms,
                        ),
                    )
                    .await
                } else if route.publication_ready_at_ms <= now_ms {
                    tokio::time::timeout_at(
                        deadline,
                        state.store.complete_media_session_handoff(
                            &route.incarnation_id,
                            &route.owner_node_id,
                            route.owner_epoch,
                            MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                                expected_not_before_ms: route.publication_ready_at_ms,
                            },
                            now_ms,
                        ),
                    )
                    .await
                } else {
                    return;
                };
                match outcome {
                    Ok(Ok(Some(reconciled))) => {
                        state.media_sessions.cache_route(reconciled).await;
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => tracing::debug!(
                        %error,
                        incarnation = %route.incarnation_id,
                        "owned media-session publication fence remains pending"
                    ),
                    Err(_) => tracing::debug!(
                        incarnation = %route.incarnation_id,
                        "owned media-session publication-fence reconciliation timed out"
                    ),
                }
            }
        })
        .await;
}

/// Renew only locally live workers. Lost ownership self-fences immediately;
/// a transient store outage is allowed to use the already-committed lease but
/// never past its exact expiry.
pub(crate) async fn lease_loop(state: AppState) {
    let mut interval = tokio::time::interval(LEASE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut known = HashMap::<String, (String, i64, i64, bool)>::new();
    let mut settling = HashSet::<String>::new();
    let mut settlement_backoff = HashMap::<String, StaleSettlementBackoff>::new();
    let (settled_tx, mut settled_rx) =
        tokio::sync::mpsc::channel::<StaleSettlementResult>(MAX_STALE_SETTLEMENTS_PER_TICK);
    let (release_tx, mut release_rx) =
        tokio::sync::mpsc::channel::<PendingReleaseResult>(RELEASE_RECONCILIATION_FANOUT);
    loop {
        interval.tick().await;
        while let Ok(result) = release_rx.try_recv() {
            match result.outcome {
                Ok(Some(route)) => {
                    // Cache exact terminal authority before lifting the
                    // temporary release fence and before peer cleanup.
                    let terminal = crate::vodserve::Terminal::from_durable_reason(
                        route.terminal_reason.as_deref(),
                    )
                    .unwrap_or(result.intent.terminal);
                    state
                        .transcode
                        .begin_session_terminal(
                            &result.session_id,
                            terminal,
                            terminal.control_reason(),
                        )
                        .await;
                    let Some(durable_release) = state
                        .media_sessions
                        .complete_release_with_route(route.clone())
                        .await
                    else {
                        state
                            .media_sessions
                            .defer_release_reconciliation(&result.session_id, &result.settlement)
                            .await;
                        continue;
                    };
                    state
                        .transcode
                        .complete_session_release_durable(&durable_release);
                    let terminal_projection_complete = durable_release
                        .terminal_projection_complete()
                        || (route.owner_node_id == state.node_id
                            && persist_terminal_projection(
                                &state,
                                &route,
                                MediaSessionProjectionCompletion::PredecessorAcknowledged,
                            )
                            .await);
                    if terminal_projection_complete {
                        state
                            .media_sessions
                            .complete_release_settlement(
                                &result.session_id,
                                &result.settlement,
                                StatusCode::NO_CONTENT,
                            )
                            .await;
                    } else {
                        state
                            .media_sessions
                            .queue_release_owner_projection(&result.session_id, &result.settlement)
                            .await;
                    }
                    tracing::info!(
                        session = %crate::transcode::session_log_id(&result.session_id),
                        owner = %route.owner_node_id,
                        terminal_projection_complete,
                        "commit-unknown media-session End reconciled"
                    );
                }
                Ok(None) => {
                    // The elected HTTP owner projected the local authority
                    // fence before its first Store attempt. Definitive durable
                    // absence may therefore replace the temporary fence now;
                    // physical cleanup can continue without blocking lease
                    // renewal or reopening actor publication.
                    state
                        .transcode
                        .begin_session_terminal(
                            &result.session_id,
                            result.intent.terminal,
                            result.intent.reason,
                        )
                        .await;
                    let durable_release = state
                        .media_sessions
                        .complete_release_absent(&result.session_id)
                        .await;
                    state
                        .transcode
                        .complete_session_release_durable(&durable_release);
                    state
                        .media_sessions
                        .complete_release_settlement(
                            &result.session_id,
                            &result.settlement,
                            StatusCode::NO_CONTENT,
                        )
                        .await;
                    tracing::info!(
                        session = %crate::transcode::session_log_id(&result.session_id),
                        "commit-unknown media-session release reconciled as absent"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        session = %crate::transcode::session_log_id(&result.session_id),
                        "commit-unknown media-session release remains unavailable"
                    );
                    state
                        .media_sessions
                        .defer_release_reconciliation(&result.session_id, &result.settlement)
                        .await;
                }
            }
        }
        for (session_id, settlement, owner_projection, projection_deadline, intent) in state
            .media_sessions
            .take_release_reconciliation_candidates()
            .await
        {
            let release_state = state.clone();
            if let Some(route) = owner_projection {
                let Some(projection_deadline) = projection_deadline else {
                    if route.publication_ready_at_ms == 0 {
                        release_state
                            .media_sessions
                            .complete_release_settlement(
                                &session_id,
                                &settlement,
                                StatusCode::NO_CONTENT,
                            )
                            .await;
                    } else {
                        tracing::error!(
                            session = %crate::transcode::session_log_id(&session_id),
                            publication_ready_at_ms = route.publication_ready_at_ms,
                            "terminal owner projection was queued without a safety boundary"
                        );
                        release_state
                            .media_sessions
                            .defer_release_reconciliation(&session_id, &settlement)
                            .await;
                    }
                    continue;
                };
                let Some(cleanup_permit) = try_admit_release_cleanup() else {
                    release_state
                        .media_sessions
                        .defer_release_reconciliation(&session_id, &settlement)
                        .await;
                    continue;
                };
                tokio::spawn(async move {
                    let _cleanup_permit = cleanup_permit;
                    let projection_state = release_state.clone();
                    let projection_route = route.clone();
                    let attempt = tokio::spawn(async move {
                        cleanup_reconciled_release(
                            &projection_state,
                            &projection_route,
                            projection_deadline,
                            intent,
                        )
                        .await
                    });
                    match attempt.await {
                        Ok(true) => {
                            release_state
                                .media_sessions
                                .complete_release_settlement(
                                    &session_id,
                                    &settlement,
                                    StatusCode::NO_CONTENT,
                                )
                                .await;
                        }
                        Ok(false) => {
                            release_state
                                .media_sessions
                                .defer_release_reconciliation(&session_id, &settlement)
                                .await;
                        }
                        Err(error) => {
                            tracing::error!(
                                %error,
                                session = %crate::transcode::session_log_id(&session_id),
                                "media-session owner terminal projection panicked"
                            );
                            release_state
                                .media_sessions
                                .defer_release_reconciliation(&session_id, &settlement)
                                .await;
                        }
                    }
                });
                continue;
            }
            let release_tx = release_tx.clone();
            tokio::spawn(async move {
                let attempt_state = release_state.clone();
                let attempt_session = session_id.clone();
                let attempt = tokio::spawn(async move {
                    // Panic/cancellation recovery may be the first owner that
                    // reaches this point. Re-establish the process-local
                    // publication fence idempotently before retrying durable
                    // End.
                    attempt_state
                        .transcode
                        .begin_session_publication_fence(&attempt_session)
                        .await;
                    // This one-shot owner has no request deadline. Awaiting
                    // the mutation to a definitive result is required because
                    // both Stores can commit after caller cancellation.
                    attempt_state
                        .store
                        .end_media_session(
                            &attempt_session,
                            intent.terminal.durable_reason(),
                            unix_ms(),
                        )
                        .await
                });
                let outcome = match attempt.await {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        tracing::error!(
                            %error,
                            session = %crate::transcode::session_log_id(&session_id),
                            "media-session release reconciliation attempt panicked"
                        );
                        release_state
                            .media_sessions
                            .defer_release_reconciliation(&session_id, &settlement)
                            .await;
                        return;
                    }
                };
                if release_tx
                    .send(PendingReleaseResult {
                        session_id: session_id.clone(),
                        settlement: Arc::clone(&settlement),
                        intent,
                        outcome,
                    })
                    .await
                    .is_err()
                {
                    release_state
                        .media_sessions
                        .defer_release_reconciliation(&session_id, &settlement)
                        .await;
                }
            });
        }
        while let Ok(result) = settled_rx.try_recv() {
            settling.remove(&result.session_id);
            if let Some(retry) = result.retry {
                settlement_backoff.insert(result.session_id, retry);
            } else {
                settlement_backoff.remove(&result.session_id);
            }
        }
        let now_ms = unix_ms();
        let vod_capabilities = Arc::new(
            state
                .transcode
                .vod_live_or_preparing_session_ids()
                .await
                .into_iter()
                .collect::<HashSet<_>>(),
        );
        let routes = match tokio::time::timeout(
            LEASE_RENEWAL_DEADLINE,
            state.store.owned_media_sessions(&state.node_id, now_ms),
        )
        .await
        {
            Ok(Ok(routes)) => Some(routes),
            Ok(Err(error)) => {
                tracing::debug!(%error, "media-session lease inventory unavailable");
                None
            }
            Err(_) => {
                tracing::debug!("media-session lease inventory timed out");
                None
            }
        };
        // Sample process-local liveness *after* the owner inventory. A
        // takeover that commits while the Store read is in flight is then
        // observed either through its settlement guard or through the adopted
        // worker/lease seed. Freezing this set before the read let the same
        // tick misclassify a just-claimed successor as stale and End it.
        let mut live = lease_tick_live(state.transcode.renewable_session_ids().await);
        let fresh_seeds = state.media_sessions.take_lease_seeds().await;
        let fresh_seed_incarnations = fresh_seeds.keys().cloned().collect::<HashSet<_>>();
        // Publication inserts the seed before dropping its settlement guard.
        // Unioning both closes the boundary where the worker snapshot ran
        // before adoption but the guard snapshot ran after its drop.
        live.extend(
            fresh_seeds
                .values()
                .map(|(session_id, _, _, _)| session_id.clone()),
        );
        known.extend(fresh_seeds);
        known.retain(|_, (session_id, _, _, _)| live.contains(session_id));
        let Some(routes) = routes else {
            let failure_now_ms = unix_ms();
            let expired = known
                .iter()
                .filter(|(_, (session_id, _, expires_at_ms, _))| {
                    live.contains(session_id) && *expires_at_ms <= failure_now_ms
                })
                .map(|(incarnation_id, (session_id, owner_epoch, _, vod))| {
                    (
                        incarnation_id.clone(),
                        session_id.clone(),
                        *owner_epoch,
                        *vod,
                    )
                })
                .collect::<Vec<_>>();
            let cleanup = expired
                .iter()
                .map(|(incarnation_id, session_id, owner_epoch, vod)| {
                    (
                        incarnation_id.clone(),
                        session_id.clone(),
                        *owner_epoch,
                        *vod,
                        "cluster lease expired",
                    )
                })
                .collect::<Vec<_>>();
            for (incarnation_id, _, _, _) in expired {
                known.remove(&incarnation_id);
            }
            fence_and_reap_sessions(&state, cleanup).await;
            continue;
        };
        reconcile_owned_publication_fences(&state, &routes, &live).await;
        let routed_session_ids = routes
            .iter()
            .map(|route| route.session_id.as_str())
            .collect::<HashSet<_>>();
        settlement_backoff.retain(|session_id, _| {
            routed_session_ids.contains(session_id.as_str()) && !live.contains(session_id)
        });
        let current = routes
            .iter()
            .map(|route| route.incarnation_id.as_str())
            .collect::<HashSet<_>>();
        let lost = known
            .iter()
            .filter(|(incarnation_id, (session_id, _, _, _))| {
                known_generation_missing_from_inventory(
                    incarnation_id,
                    session_id,
                    &current,
                    &live,
                    &fresh_seed_incarnations,
                )
            })
            .map(|(incarnation_id, (session_id, owner_epoch, _, vod))| {
                (
                    incarnation_id.clone(),
                    session_id.clone(),
                    *owner_epoch,
                    *vod,
                )
            })
            .collect::<Vec<_>>();
        for route in &routes {
            let known_vod = vod_capabilities.contains(&route.session_id)
                || known
                    .get(&route.incarnation_id)
                    .is_some_and(|(_, _, _, vod)| *vod);
            known.insert(
                route.incarnation_id.clone(),
                (
                    route.session_id.clone(),
                    route.owner_epoch,
                    route.lease_expires_at_ms,
                    known_vod,
                ),
            );
        }
        let active = lease_tick_active(&routes, &live);
        let active_session_ids = active
            .iter()
            .map(|route| route.session_id.clone())
            .collect::<Vec<_>>();
        let frontiers = Arc::new(state.transcode.session_frontiers(&active_session_ids).await);
        let renewal_deadline = tokio::time::Instant::now() + LEASE_RENEWAL_DEADLINE;
        let renewal_batches = renewal_chunks(&active)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|route| (*route).clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let renewal_outcomes = stream::iter(renewal_batches.into_iter().map(|chunk| {
            let store = Arc::clone(&state.store);
            let owner_node_id = state.node_id.clone();
            let frontiers = Arc::clone(&frontiers);
            let vod_capabilities = Arc::clone(&vod_capabilities);
            async move {
                // Inventory and fan-out time consume the existing lease. A
                // renewal must compare against wall time at the store call,
                // never the earlier tick snapshot, or a delayed batch could
                // revive a lease that expired while it waited.
                let renewal_now_ms = unix_ms();
                let renewals = chunk
                    .iter()
                    // The replicated store has a three-second hard boundary
                    // inside this four-second caller deadline. Refuse to send
                    // work without the complete window remaining, so even a
                    // commit-unknown reply cannot revive an already-expired
                    // lease. Omitted routes fail closed in the outcome pass.
                    .filter(|route| {
                        route.lease_expires_at_ms
                            > renewal_now_ms.saturating_add(LEASE_RENEWAL_MIN_REMAINING_MS)
                    })
                    .filter_map(|route| {
                        frontiers
                            .get(&route.session_id)
                            .map(|frontier| MediaSessionRenewal {
                                incarnation_id: route.incarnation_id.clone(),
                                owner_epoch: route.owner_epoch,
                                produced_playable_through_ms: frontier.produced_playable_through_ms,
                                fetched_through_ms: frontier.fetched_through_ms,
                                media_sequence: frontier.media_sequence,
                            })
                    })
                    .collect::<Vec<_>>();
                let outcome = tokio::time::timeout_at(
                    renewal_deadline,
                    store.renew_media_sessions(
                        &owner_node_id,
                        &renewals,
                        renewal_now_ms,
                        renewal_now_ms.saturating_add(LEASE_TTL_MS),
                    ),
                )
                .await;
                (chunk, renewal_now_ms, outcome, vod_capabilities)
            }
        }))
        .buffer_unordered(LEASE_RENEWAL_FANOUT)
        .collect::<Vec<_>>()
        .await;
        let mut cleanup = Vec::new();
        for (chunk, renewal_now_ms, outcome, vod_capabilities) in renewal_outcomes {
            match outcome {
                Ok(Ok(renewed)) => {
                    let renewed = renewed.into_iter().collect::<HashSet<_>>();
                    for route in &chunk {
                        if renewed.contains(&route.incarnation_id) {
                            known.insert(
                                route.incarnation_id.clone(),
                                (
                                    route.session_id.clone(),
                                    route.owner_epoch,
                                    renewal_now_ms.saturating_add(LEASE_TTL_MS),
                                    vod_capabilities.contains(&route.session_id),
                                ),
                            );
                        } else {
                            cleanup.push((
                                route.incarnation_id.clone(),
                                route.session_id.clone(),
                                route.owner_epoch,
                                vod_capabilities.contains(&route.session_id),
                                "cluster lease lost",
                            ));
                            known.remove(&route.incarnation_id);
                        }
                    }
                }
                Ok(Err(error)) => {
                    tracing::debug!(%error, "media-session lease renewal chunk unavailable");
                    for route in &chunk {
                        // Store timeouts and transport errors are commit-
                        // unknown: SQLite blocking work and a submitted Raft
                        // proposal may still complete after their future is
                        // dropped. Fence the worker regardless of its prior
                        // expiry so a late durable renewal cannot resurrect
                        // serving authority.
                        cleanup.push((
                            route.incarnation_id.clone(),
                            route.session_id.clone(),
                            route.owner_epoch,
                            vod_capabilities.contains(&route.session_id),
                            "cluster lease renewal ambiguous",
                        ));
                        known.remove(&route.incarnation_id);
                    }
                }
                Err(_) => {
                    tracing::debug!("media-session lease renewal fan-out exceeded its deadline");
                    for route in &chunk {
                        cleanup.push((
                            route.incarnation_id.clone(),
                            route.session_id.clone(),
                            route.owner_epoch,
                            vod_capabilities.contains(&route.session_id),
                            "cluster lease renewal ambiguous",
                        ));
                        known.remove(&route.incarnation_id);
                    }
                }
            }
        }
        for (incarnation_id, session_id, owner_epoch, vod) in lost {
            cleanup.push((
                incarnation_id.clone(),
                session_id,
                owner_epoch,
                vod,
                "cluster lease lost",
            ));
            known.remove(&incarnation_id);
        }
        cleanup.sort_unstable_by(|left, right| left.1.cmp(&right.1));
        cleanup.dedup_by(|left, right| left.1 == right.1);
        fence_and_reap_sessions(&state, cleanup).await;
        let settlement_now = tokio::time::Instant::now();
        let settlement_now_ms = unix_ms();
        let unsettled = take_stale_settlement_candidates(
            &routes,
            &live,
            &mut settling,
            &mut settlement_backoff,
            settlement_now,
            settlement_now_ms,
        );
        for (route, previous_failures) in unsettled {
            let cleanup_state = state.clone();
            let session_id = route.session_id.clone();
            let known_incarnation_id = route.incarnation_id.clone();
            let settled_tx = settled_tx.clone();
            tokio::spawn(async move {
                // Re-read the full row before acting. The lean owner
                // inventory deliberately omits the presentation recipe, and
                // a takeover may have advanced the owner epoch since that
                // inventory snapshot. Every local fence and Store mutation
                // below is bound to the exact old generation.
                let current = match tokio::time::timeout(
                    STALE_SETTLEMENT_DEADLINE,
                    cleanup_state
                        .store
                        .media_session_route_by_incarnation(&route.incarnation_id),
                )
                .await
                {
                    Ok(Ok(current)) => current,
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "stale media-session generation read unavailable");
                        let failures = previous_failures.saturating_add(1);
                        let _ = settled_tx
                            .send(StaleSettlementResult {
                                session_id,
                                retry: Some(StaleSettlementBackoff {
                                    failures,
                                    next_attempt: tokio::time::Instant::now()
                                        + stale_settlement_retry_delay(failures),
                                }),
                            })
                            .await;
                        return;
                    }
                    Err(_) => {
                        tracing::debug!("stale media-session generation read timed out");
                        let failures = previous_failures.saturating_add(1);
                        let _ = settled_tx
                            .send(StaleSettlementResult {
                                session_id,
                                retry: Some(StaleSettlementBackoff {
                                    failures,
                                    next_attempt: tokio::time::Instant::now()
                                        + stale_settlement_retry_delay(failures),
                                }),
                            })
                            .await;
                        return;
                    }
                };
                let exact = current.filter(|current| {
                    current.session_id == route.session_id
                        && current.owner_node_id == cleanup_state.node_id
                        && current.owner_epoch == route.owner_epoch
                        && current.state == "active"
                        && current.lease_expires_at_ms == route.lease_expires_at_ms
                        && current.lease_expires_at_ms
                            > unix_ms().saturating_add(STALE_SETTLEMENT_MIN_RUNWAY_MS)
                });
                let Some(exact) = exact else {
                    // Absence, terminal state, or a newer owner epoch is a
                    // definitive loss for this cleanup generation.
                    let _ = settled_tx
                        .send(StaleSettlementResult {
                            session_id,
                            retry: None,
                        })
                        .await;
                    return;
                };
                let takeover_eligible = serde_json::from_str::<RemoteStartRequest>(
                    &exact.recipe_json,
                )
                .is_ok_and(|request| {
                    takeover_recipe_matches_route(&request, &exact) && request.typeless_playlist
                });
                if takeover_eligible
                    && cleanup_state
                        .transcode
                        .fence_session_for_owner(
                            &exact.incarnation_id,
                            &exact.session_id,
                            exact.owner_epoch,
                        )
                        .await
                {
                    let reap_state = cleanup_state.clone();
                    let reap = exact.clone();
                    tokio::spawn(async move {
                        reap_state
                            .transcode
                            .stop_session_for_owner(
                                &reap.incarnation_id,
                                &reap.session_id,
                                reap.owner_epoch,
                                "stale durable session",
                            )
                            .await;
                    });
                }
                if takeover_eligible {
                    // The absent local worker is exactly what the owner lease
                    // models. Leave this route active until fixed expiry so a
                    // survivor can claim it; terminalizing it here destroys
                    // the only durable failover recipe.
                    let _ = settled_tx
                        .send(StaleSettlementResult {
                            session_id,
                            retry: None,
                        })
                        .await;
                    return;
                }
                cleanup_state
                    .transcode
                    .begin_session_publication_fence(&session_id)
                    .await;
                let mut failures = previous_failures;
                let terminal_route = loop {
                    // This detached task is one of the fixed stale-settlement
                    // slots; it, rather than a request timeout, owns the
                    // commit-unknown mutation to a definitive result. In
                    // particular a VOD release gate cannot be abandoned when
                    // the End committed but its reply was lost and the row
                    // consequently disappears from the next active inventory.
                    match cleanup_state
                        .store
                        .end_media_session_if_owner(&MediaSessionEnd {
                            incarnation_id: exact.incarnation_id.clone(),
                            session_id: exact.session_id.clone(),
                            expected_owner_node_id: exact.owner_node_id.clone(),
                            expected_owner_epoch: exact.owner_epoch,
                            expected_lease_expires_at_ms: exact.lease_expires_at_ms,
                            terminal_reason: "replaced".to_owned(),
                            now_ms: unix_ms(),
                        })
                        .await
                    {
                        Ok(route) => break route,
                        Err(error) => {
                            failures = failures.saturating_add(1);
                            let delay = stale_settlement_retry_delay(failures);
                            tracing::debug!(
                                %error,
                                ?delay,
                                "exact stale media-session End remains commit-unknown"
                            );
                            tokio::time::sleep(delay).await;
                        }
                    }
                };
                {
                    let mut terminal_owner = terminal_route.clone();
                    let durable_release = match terminal_route {
                        Some(route) => {
                            cleanup_state
                                .media_sessions
                                .complete_release_with_route(route)
                                .await
                        }
                        None => loop {
                            match cleanup_state
                                .store
                                .media_session_route_by_incarnation(&exact.incarnation_id)
                                .await
                            {
                                Ok(Some(route)) if route.state == "ended" => {
                                    terminal_owner = Some(route.clone());
                                    break cleanup_state
                                        .media_sessions
                                        .complete_release_with_route(route)
                                        .await;
                                }
                                Ok(None) => {
                                    break Some(
                                        cleanup_state
                                            .media_sessions
                                            .complete_release_absent(&session_id)
                                            .await,
                                    );
                                }
                                Ok(Some(_)) => break None,
                                Err(error) => {
                                    failures = failures.saturating_add(1);
                                    tracing::debug!(
                                        %error,
                                        "stale VOD terminal proof remains unavailable"
                                    );
                                    tokio::time::sleep(stale_settlement_retry_delay(failures))
                                        .await;
                                }
                            }
                        },
                    };
                    let terminal = terminal_owner
                        .as_ref()
                        .map(durable_terminal_or_replaced)
                        .unwrap_or(crate::vodserve::Terminal::Replaced);
                    cleanup_state
                        .transcode
                        .begin_session_terminal(&session_id, terminal, terminal.control_reason())
                        .await;
                    let durable_release = match (durable_release, terminal_owner.as_ref()) {
                        (Some(proof), _) if proof.terminal_projection_complete() => Some(proof),
                        (Some(initial), Some(route)) => cleanup_state
                            .media_sessions
                            .complete_terminal_projection(
                                route,
                                MediaSessionProjectionCompletion::PredecessorAcknowledged,
                            )
                            .await
                            .filter(DurableReleaseProof::terminal_projection_complete)
                            .or(Some(initial)),
                        (proof, _) => proof,
                    };
                    if let Some(proof) = durable_release {
                        cleanup_state
                            .transcode
                            .complete_session_release_durable(&proof);
                    } else {
                        // An impossible active owner change is handled
                        // fail-closed: retain the released generation rather
                        // than treating an exact-CAS miss as absence.
                        cleanup_state
                            .transcode
                            .complete_session_release(&session_id);
                    }
                }
                let _ = settled_tx
                    .send(StaleSettlementResult {
                        session_id,
                        retry: None,
                    })
                    .await;
            });
            known.remove(&known_incarnation_id);
        }
    }
}

/// Retention and stale-row cleanup is intentionally independent from the
/// three-second owner heartbeat. Session leases self-fence at their exact
/// expiry, so cleanup can run coarsely without weakening correctness; the
/// replicated backend also preflights and avoids an idle Raft proposal.
pub(crate) async fn maintenance_loop(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5 * 60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if let Err(error) = state.store.maintain_media_sessions(unix_ms()).await {
            tracing::debug!(%error, "media-session lifecycle maintenance unavailable");
        }
    }
}

/// Contest expired session routes only after the separate replicated rollout
/// switch is enabled. Every candidate independently proves source/pipeline
/// eligibility; the Store CAS still admits exactly one successor epoch.
pub(crate) async fn takeover_loop(state: AppState) {
    let mut interval = tokio::time::interval(TAKEOVER_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Keep the keyset position across ticks. Permanently ineligible legacy or
    // malformed recipes remain authoritative until expiry/maintenance, but
    // cannot pin this bounded scanner to the oldest page in the meantime.
    let mut scan_cursor: Option<MediaSessionTakeoverCursor> = None;
    loop {
        interval.tick().await;
        let media_pool_enabled = state
            .store
            .get_setting(plurx_core::store::keys::CLUSTER_MEDIA_POOL_ENABLED)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1");
        let takeover_enabled = state
            .store
            .get_setting(plurx_core::store::keys::CLUSTER_SESSION_TAKEOVER_ENABLED)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1");
        if !media_pool_enabled
            || !takeover_enabled
            || !state.media_pool.remote_rollout_ready().await
        {
            scan_cursor = None;
            continue;
        }
        let now_ms = unix_ms();
        let routes = match tokio::time::timeout(
            Duration::from_secs(3),
            state
                .store
                .expired_media_sessions(now_ms, scan_cursor.clone(), TAKEOVER_BATCH),
        )
        .await
        {
            Ok(Ok(routes)) => routes,
            Ok(Err(error)) => {
                tracing::debug!(%error, "media-session takeover inventory unavailable");
                continue;
            }
            Err(_) => continue,
        };
        if routes.is_empty() {
            // Reaching the end starts a fresh oldest-first pass on the next
            // tick. This also revisits transient refusals without sacrificing
            // bounded progress through the current inventory.
            scan_cursor = None;
            continue;
        }
        scan_cursor = routes.last().map(MediaSessionTakeoverCursor::from);
        // A route stays expired-and-claimable until somebody's CAS lands, so
        // it reappears on every tick until then. Contesting it again while
        // this node's own attempt is still in flight buys nothing and costs an
        // offers round trip, an ffmpeg spawn and an admission slot each time.
        let in_flight = settlement_protected_ids();
        stream::iter(
            routes
                .into_iter()
                .filter(|route| !in_flight.contains(&route.session_id))
                .map(|route| {
                    let state = state.clone();
                    async move {
                        if let Err(error) = attempt_takeover(&state, route).await {
                            tracing::debug!(%error, "media-session takeover candidate refused");
                        }
                    }
                }),
        )
        .buffer_unordered(TAKEOVER_FANOUT)
        .collect::<Vec<_>>()
        .await;
    }
}

fn takeover_claim_verdict(
    original: &MediaSessionRoute,
    claim: &MediaSessionTakeover,
    current: Option<MediaSessionRoute>,
    now_ms: i64,
) -> TakeoverClaimVerdict {
    let Some(current) = current else {
        // A takeover only updates an existing incarnation. Once an exact
        // authoritative read observes absence, neither this proposal nor a
        // replay can recreate it.
        return TakeoverClaimVerdict::Lost;
    };
    let next_epoch = claim.expected_owner_epoch.saturating_add(1);
    let exact_winner = current.incarnation_id == original.incarnation_id
        && current.session_id == original.session_id
        && current.owner_node_id == claim.next_owner_node_id
        && current.owner_epoch == next_epoch
        && current.state == "active"
        && current.discontinuity_sequence >= original.discontinuity_sequence.saturating_add(1);
    if exact_winner {
        return if current.lease_expires_at_ms > now_ms {
            TakeoverClaimVerdict::Won(Box::new(current))
        } else {
            TakeoverClaimVerdict::Lost
        };
    }

    // A cancelled Store future may have submitted its mutation without
    // receiving the reply. Seeing the unchanged source generation does not
    // prove loss: that earlier proposal may still commit after this read.
    // Retain every local capability until a later exact state makes the CAS
    // impossible, or until its fixed proposed lease has no serving authority.
    let exact_source = current.incarnation_id == original.incarnation_id
        && current.session_id == original.session_id
        && current.owner_node_id == claim.expected_owner_node_id
        && current.owner_epoch == claim.expected_owner_epoch
        && current.state == "active"
        && current.lease_expires_at_ms == original.lease_expires_at_ms;
    if exact_source && now_ms < claim.lease_expires_at_ms {
        TakeoverClaimVerdict::Pending
    } else {
        TakeoverClaimVerdict::Lost
    }
}

fn takeover_reconciliation_remaining(
    expires_at_ms: i64,
    monotonic_expiry: tokio::time::Instant,
) -> Option<Duration> {
    let wall_ms = expires_at_ms.checked_sub(unix_ms())?;
    let wall_ms = u64::try_from(wall_ms).ok()?;
    let monotonic = monotonic_expiry.saturating_duration_since(tokio::time::Instant::now());
    let remaining = Duration::from_millis(wall_ms).min(monotonic);
    (!remaining.is_zero()).then_some(remaining)
}

fn takeover_reconciliation_deadline(
    expires_at_ms: i64,
    monotonic_expiry: tokio::time::Instant,
) -> Option<tokio::time::Instant> {
    takeover_reconciliation_remaining(expires_at_ms, monotonic_expiry).map(|remaining| {
        tokio::time::Instant::now() + remaining.min(TAKEOVER_RECONCILIATION_STORE_DEADLINE)
    })
}

async fn pause_takeover_reconciliation(
    expires_at_ms: i64,
    monotonic_expiry: tokio::time::Instant,
) -> bool {
    let Some(remaining) = takeover_reconciliation_remaining(expires_at_ms, monotonic_expiry) else {
        return false;
    };
    tokio::time::sleep(remaining.min(TAKEOVER_RECONCILIATION_RETRY_BACKOFF)).await;
    takeover_reconciliation_remaining(expires_at_ms, monotonic_expiry).is_some()
}

async fn retain_takeover_settlement_until_expiry(
    expires_at_ms: i64,
    monotonic_expiry: tokio::time::Instant,
) {
    if let Some(remaining) = takeover_reconciliation_remaining(expires_at_ms, monotonic_expiry) {
        tokio::time::sleep(remaining).await;
    }
}

async fn stop_pending_takeover<W, A>(
    mut pending: PendingTakeoverSettlement<W, A>,
    reason: &'static str,
    outcome: usize,
) where
    W: TakeoverWorkerLifecycle,
    A: Send + 'static,
{
    pending.metric.outcome = outcome;
    pending.worker.stop(reason).await;
}

/// Submit the immutable claim once, then hand every ambiguous outcome to the
/// exact replay/read reconciler. Keeping this boundary generic lets the
/// cancellation contract be exercised without a real Store or worker.
async fn settle_initial_takeover_claim<I, W, A>(
    io: &I,
    pending: PendingTakeoverSettlement<W, A>,
) -> Result<(), String>
where
    I: TakeoverSettlementIo<A, W>,
    W: TakeoverWorkerLifecycle,
    A: Send + 'static,
{
    let Some(claim_deadline) = takeover_reconciliation_deadline(
        pending.claim.lease_expires_at_ms,
        pending.monotonic_expiry,
    ) else {
        stop_pending_takeover(
            pending,
            "media-session takeover claim expired",
            TAKEOVER_FAILED,
        )
        .await;
        return Err("media-session takeover claim expired before submission".to_owned());
    };
    match tokio::time::timeout_at(claim_deadline, io.claim(&pending.claim)).await {
        Ok(Ok(Some(claimed))) => reconcile_pending_takeover(io, pending, Some(claimed)).await,
        Ok(Ok(None)) => {
            stop_pending_takeover(pending, "media-session takeover lost", TAKEOVER_LOST).await;
            Ok(())
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "media-session takeover claim reply is ambiguous");
            reconcile_pending_takeover(io, pending, None).await
        }
        Err(_) => {
            tracing::debug!("media-session takeover claim deadline became ambiguous");
            reconcile_pending_takeover(io, pending, None).await
        }
    }
}

/// Resolve one takeover CAS independently of the inventory tick that started
/// it. The loop replays the exact fixed proposal and then reads by incarnation:
/// replay is safe before or after a lost reply, while the read distinguishes
/// our successor from a terminal, renewed, or competing generation. The
/// proposed lease owns reconciliation; after proving the exact winner, this
/// task renews from the still-provisional worker before adopting its public id.
async fn reconcile_pending_takeover<I, W, A>(
    io: &I,
    pending: PendingTakeoverSettlement<W, A>,
    initial_winner: Option<MediaSessionRoute>,
) -> Result<(), String>
where
    I: TakeoverSettlementIo<A, W>,
    W: TakeoverWorkerLifecycle,
    A: Send + 'static,
{
    let mut observed_winner = initial_winner.map(|route| (route, pending.claim_cache_generation));
    let (mut claimed, winning_cache_generation) = loop {
        if let Some((candidate, cache_generation)) = observed_winner.take() {
            match takeover_claim_verdict(
                &pending.original,
                &pending.claim,
                Some(candidate),
                unix_ms(),
            ) {
                TakeoverClaimVerdict::Won(route) => break (*route, cache_generation),
                TakeoverClaimVerdict::Lost => {
                    stop_pending_takeover(pending, "media-session takeover lost", TAKEOVER_LOST)
                        .await;
                    return Ok(());
                }
                TakeoverClaimVerdict::Pending => {}
            }
        }

        let Some(replay_deadline) = takeover_reconciliation_deadline(
            pending.claim.lease_expires_at_ms,
            pending.monotonic_expiry,
        ) else {
            stop_pending_takeover(
                pending,
                "media-session takeover claim expired",
                TAKEOVER_FAILED,
            )
            .await;
            return Err("media-session takeover claim expired before settlement".to_owned());
        };
        let replay_cache_generation = io.route_generation(&pending.original.session_id);
        match tokio::time::timeout_at(replay_deadline, io.replay(&pending.claim)).await {
            Ok(Ok(Some(route))) => {
                observed_winner = Some((route, replay_cache_generation));
                continue;
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                tracing::debug!(%error, "takeover claim replay remains commit-unknown");
            }
            Err(_) => {
                tracing::debug!("takeover claim replay timed out");
            }
        }

        let Some(read_deadline) = takeover_reconciliation_deadline(
            pending.claim.lease_expires_at_ms,
            pending.monotonic_expiry,
        ) else {
            continue;
        };
        let read_cache_generation = io.route_generation(&pending.original.session_id);
        let current =
            match tokio::time::timeout_at(read_deadline, io.read(&pending.original.incarnation_id))
                .await
            {
                Ok(Ok(current)) => current,
                Ok(Err(error)) => {
                    tracing::debug!(%error, "takeover exact reconciliation read unavailable");
                    if pause_takeover_reconciliation(
                        pending.claim.lease_expires_at_ms,
                        pending.monotonic_expiry,
                    )
                    .await
                    {
                        continue;
                    }
                    continue;
                }
                Err(_) => {
                    tracing::debug!("takeover exact reconciliation read timed out");
                    if pause_takeover_reconciliation(
                        pending.claim.lease_expires_at_ms,
                        pending.monotonic_expiry,
                    )
                    .await
                    {
                        continue;
                    }
                    continue;
                }
            };
        match takeover_claim_verdict(&pending.original, &pending.claim, current, unix_ms()) {
            TakeoverClaimVerdict::Won(route) => break (*route, read_cache_generation),
            TakeoverClaimVerdict::Lost => {
                stop_pending_takeover(pending, "media-session takeover lost", TAKEOVER_LOST).await;
                return Ok(());
            }
            TakeoverClaimVerdict::Pending => {
                if pause_takeover_reconciliation(
                    pending.claim.lease_expires_at_ms,
                    pending.monotonic_expiry,
                )
                .await
                {
                    continue;
                }
            }
        }
    };

    let PendingTakeoverSettlement {
        provisional_id,
        mut worker,
        adoption,
        mut metric,
        claim,
        monotonic_expiry,
        ..
    } = pending;
    if !takeover_reconciliation_remaining(claim.lease_expires_at_ms, monotonic_expiry)
        .is_some_and(|remaining| remaining >= TAKEOVER_PUBLICATION_MIN_RUNWAY)
    {
        metric.outcome = TAKEOVER_FAILED;
        worker
            .stop_and_retain_until(
                "media-session takeover claim expired",
                claim.lease_expires_at_ms,
                monotonic_expiry,
            )
            .await;
        return Err("media-session takeover winner had no safe publication runway".to_owned());
    }

    // A staged successor belongs to the predecessor owner that admitted it.
    // The new process starts with a fresh ControlState and cannot safely
    // rehydrate that old node's engine-local slot. Resolve the durable row
    // before publishing the adopted worker; staging itself is owner-fenced,
    // so no old detached task can recreate it after the takeover CAS.
    let Some(preparation_deadline) =
        takeover_reconciliation_deadline(claim.lease_expires_at_ms, monotonic_expiry)
    else {
        metric.outcome = TAKEOVER_FAILED;
        worker
            .stop_and_retain_until(
                "media-session takeover preparation cleanup expired",
                claim.lease_expires_at_ms,
                monotonic_expiry,
            )
            .await;
        return Err("takeover preparation cleanup expired".to_owned());
    };
    match tokio::time::timeout_at(
        preparation_deadline,
        io.abort_inherited_preparation(&claimed),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::debug!(%error, "takeover inherited preparation cleanup failed");
            metric.outcome = TAKEOVER_FAILED;
            worker
                .stop_and_retain_until(
                    "media-session takeover preparation cleanup failed",
                    claim.lease_expires_at_ms,
                    monotonic_expiry,
                )
                .await;
            return Err("takeover could not settle inherited preparation".to_owned());
        }
        Err(_) => {
            metric.outcome = TAKEOVER_FAILED;
            worker
                .stop_and_retain_until(
                    "media-session takeover preparation cleanup timed out",
                    claim.lease_expires_at_ms,
                    monotonic_expiry,
                )
                .await;
            return Err("takeover inherited preparation cleanup timed out".to_owned());
        }
    }

    // A shared-cache pin is a second Store mutation and therefore has the
    // same lost-reply shape. Prove it against the provisional worker before
    // the stable public capability is installed; a pin failure can therefore
    // never expose unretained shared bytes.
    let pinned = loop {
        if !takeover_reconciliation_remaining(claim.lease_expires_at_ms, monotonic_expiry)
            .is_some_and(|remaining| remaining >= TAKEOVER_PUBLICATION_MIN_RUNWAY)
        {
            break false;
        }
        let Some(pin_deadline) =
            takeover_reconciliation_deadline(claim.lease_expires_at_ms, monotonic_expiry)
        else {
            break false;
        };
        match tokio::time::timeout_at(pin_deadline, io.pin(&provisional_id, &claimed)).await {
            Ok(Ok(true)) => break true,
            Ok(Ok(false)) => break false,
            Ok(Err(error)) => {
                tracing::debug!(%error, "takeover shared-cache pin remains commit-unknown");
            }
            Err(_) => {
                tracing::debug!("takeover shared-cache pin timed out");
            }
        }
        if !pause_takeover_reconciliation(claim.lease_expires_at_ms, monotonic_expiry).await {
            break false;
        }
    };

    if !pinned {
        metric.outcome = TAKEOVER_FAILED;
        worker
            .stop_and_retain_until(
                "media-session takeover pin failed",
                claim.lease_expires_at_ms,
                monotonic_expiry,
            )
            .await;
        return Err("takeover winner could not publish its local worker".to_owned());
    }

    if !takeover_reconciliation_remaining(claim.lease_expires_at_ms, monotonic_expiry)
        .is_some_and(|remaining| remaining >= TAKEOVER_PUBLICATION_MIN_RUNWAY)
    {
        metric.outcome = TAKEOVER_FAILED;
        worker
            .stop_and_retain_until(
                "media-session takeover publication runway spent",
                claim.lease_expires_at_ms,
                monotonic_expiry,
            )
            .await;
        return Err("media-session takeover pin consumed its publication runway".to_owned());
    }

    // Renew the durable route from the provisional worker's frontier before
    // installing its stable public capability. The ordinary interval may be
    // midway through an inventory/publication/cleanup tick, but no request can
    // reach the successor by durable id until this exact bootstrap succeeds.
    let bootstrap_now = tokio::time::Instant::now();
    let bootstrap_monotonic_expiry = bootstrap_now
        + Duration::from_millis(u64::try_from(TAKEOVER_CLAIM_LEASE_TTL_MS).unwrap_or_default());
    let bootstrap_expires_at_ms = unix_ms().saturating_add(TAKEOVER_CLAIM_LEASE_TTL_MS);
    worker.retain_until(bootstrap_expires_at_ms, bootstrap_monotonic_expiry);
    let bootstrap_deadline = bootstrap_now + LEASE_RENEWAL_DEADLINE;
    claimed = match tokio::time::timeout_at(
        bootstrap_deadline,
        io.renew_first(&claimed, &provisional_id, bootstrap_expires_at_ms),
    )
    .await
    {
        Ok(Ok(Some(route))) => route,
        Ok(Ok(None)) => {
            metric.outcome = TAKEOVER_LOST;
            worker
                .stop("media-session takeover bootstrap renewal lost")
                .await;
            return Ok(());
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "takeover bootstrap renewal remains commit-unknown");
            metric.outcome = TAKEOVER_FAILED;
            worker
                .stop_and_retain_until(
                    "media-session takeover bootstrap renewal failed",
                    bootstrap_expires_at_ms,
                    bootstrap_monotonic_expiry,
                )
                .await;
            return Err("takeover bootstrap renewal remained commit-unknown".to_owned());
        }
        Err(_) => {
            tracing::debug!("takeover bootstrap renewal timed out");
            metric.outcome = TAKEOVER_FAILED;
            worker
                .stop_and_retain_until(
                    "media-session takeover bootstrap renewal timed out",
                    bootstrap_expires_at_ms,
                    bootstrap_monotonic_expiry,
                )
                .await;
            return Err("takeover bootstrap renewal timed out".to_owned());
        }
    };

    let Some(adoption_deadline) =
        takeover_reconciliation_deadline(bootstrap_expires_at_ms, bootstrap_monotonic_expiry)
    else {
        metric.outcome = TAKEOVER_FAILED;
        worker
            .stop_and_retain_until(
                "media-session takeover adoption expired",
                bootstrap_expires_at_ms,
                bootstrap_monotonic_expiry,
            )
            .await;
        return Err("media-session takeover bootstrap lease expired before adoption".to_owned());
    };
    worker = match tokio::time::timeout_at(
        adoption_deadline,
        io.adopt(&provisional_id, &claimed.session_id, worker, adoption),
    )
    .await
    {
        Ok(Ok(worker)) => worker,
        Ok(Err(worker)) => {
            metric.outcome = TAKEOVER_FAILED;
            worker
                .stop_and_retain_until(
                    "media-session takeover adoption failed",
                    bootstrap_expires_at_ms,
                    bootstrap_monotonic_expiry,
                )
                .await;
            return Err("takeover winner could not adopt its local worker".to_owned());
        }
        Err(_) => {
            // The timed-out future owned the worker. Dropping it invokes exact
            // teardown using whichever identity the synchronous registry move
            // had reached, and retains settlement through the bootstrap lease.
            metric.outcome = TAKEOVER_FAILED;
            return Err("media-session takeover adoption timed out".to_owned());
        }
    };
    io.seed(&claimed).await;
    worker.publish();
    metric.outcome = TAKEOVER_WON;
    let cache_deadline = tokio::time::Instant::now() + TAKEOVER_RECONCILIATION_STORE_DEADLINE;
    let _ =
        tokio::time::timeout_at(cache_deadline, io.cache(claimed, winning_cache_generation)).await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn supervise_takeover_settlement(
    state: AppState,
    original: MediaSessionRoute,
    request: SessionRequest,
    user_name: String,
    start: SessionTakeoverStart,
    creation_deadline: tokio::time::Instant,
    adoption: SessionAdoptionToken,
    settlement: SessionSettlementGuard,
    slot: tokio::sync::OwnedSemaphorePermit,
    metric: TakeoverMetricGuard,
) -> Result<(), String> {
    let provisional_id = start.provisional_session_id.clone();
    let replacement = state
        .transcode
        .acquire_cluster_takeover_replacement(&request, original.user_id, creation_deadline)
        .await?;
    let worker = TakeoverWorkerGuard::new(
        Arc::clone(&state.transcode),
        provisional_id.clone(),
        replacement,
        settlement,
        slot,
    );

    // Creation runs under a child join so this supervisor, not the inventory
    // future, owns timeout and panic cleanup. Aborting a deadline-spent child
    // first prevents a later registration; the predetermined id and retained
    // replacement guard then make exact teardown possible in every phase.
    let creation_manager = Arc::clone(&state.transcode);
    let creation_request = request.clone();
    let creation_user = user_name.clone();
    let creation_start = start.clone();
    let creation_user_id = original.user_id;
    let creation = tokio::spawn(async move {
        creation_manager
            .create_cluster_takeover_session_under_guard(
                &creation_request,
                creation_user_id,
                &creation_user,
                creation_deadline,
                creation_start,
            )
            .await
    });
    let (created, mut worker) = TakeoverCreationOwner::new(creation, worker)
        .finish(creation_deadline)
        .await?;
    if created.session_id != provisional_id {
        worker.adopted(&created.session_id);
        worker
            .stop("media-session takeover returned an unexpected local id")
            .await;
        return Err("media-session takeover creation changed its provisional id".to_owned());
    }

    let claim_now_ms = unix_ms();
    let claim_monotonic_expiry = tokio::time::Instant::now()
        + Duration::from_millis(u64::try_from(TAKEOVER_CLAIM_LEASE_TTL_MS).unwrap_or_default());
    let claim_cache_generation = state.media_sessions.route_generation(&original.session_id);
    let takeover = MediaSessionTakeover {
        incarnation_id: original.incarnation_id.clone(),
        expected_owner_node_id: original.owner_node_id.clone(),
        expected_owner_epoch: original.owner_epoch,
        next_owner_node_id: state.node_id.clone(),
        now_ms: claim_now_ms,
        lease_expires_at_ms: claim_now_ms.saturating_add(TAKEOVER_CLAIM_LEASE_TTL_MS),
    };
    worker.retain_settlement_until(takeover.lease_expires_at_ms, claim_monotonic_expiry);
    let pending = PendingTakeoverSettlement {
        original,
        claim: takeover,
        provisional_id,
        worker,
        adoption,
        monotonic_expiry: claim_monotonic_expiry,
        claim_cache_generation,
        metric,
    };
    settle_initial_takeover_claim(&state, pending).await
}

/// A route whose durable recipe the takeover path accepts, for the tests in
/// this module and the HTTP ones. Defined once so the two suites cannot
/// disagree about what "eligible" means.
#[cfg(test)]
pub(crate) fn takeover_eligible_route(session_id: &str, incarnation_id: &str) -> MediaSessionRoute {
    let mut route = tests::media_route(session_id);
    route.incarnation_id = incarnation_id.to_owned();
    let base = tests::valid_start_request();
    let recipe = RemoteStartRequest {
        incarnation_id: incarnation_id.to_owned(),
        user_id: route.user_id,
        typeless_playlist: true,
        request: SessionRequest {
            request_id: Some(incarnation_id.to_owned()),
            presentation: crate::transcode::Presentation::Live,
            ..base.request
        },
        ..base
    };
    debug_assert!(takeover_recipe_matches_route(&recipe, &route));
    route.recipe_json = serde_json::to_string(&recipe).expect("serialize eligible recipe");
    route
}

/// Drive the takeover attempt for the EVENT-refusal test, which needs an
/// `AppState` fixture and so lives with the HTTP tests.
#[cfg(test)]
pub(crate) async fn attempt_takeover_for_test(
    state: &AppState,
    route: MediaSessionRoute,
) -> Result<(), String> {
    attempt_takeover(state, route).await
}

#[cfg(test)]
pub(crate) async fn abort_inherited_preparation_for_test(
    state: &AppState,
    route: &MediaSessionRoute,
) -> Result<(), StoreError> {
    state.abort_inherited_preparation(route).await
}

async fn attempt_takeover(state: &AppState, route: MediaSessionRoute) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + TAKEOVER_DEADLINE;
    let mut metric = TakeoverMetricGuard::new();
    if route.owner_node_id == state.node_id {
        metric.outcome = TAKEOVER_SKIPPED;
        return Ok(());
    }
    let Some(takeover_slot) = try_admit_takeover_settlement() else {
        metric.outcome = TAKEOVER_SKIPPED;
        return Err("media-session takeover settlement capacity is full".to_owned());
    };
    // Acquired before the first Store/offer await. The next inventory tick
    // therefore suppresses this exact capability during preparation as well
    // as during commit-unknown reconciliation.
    let settlement = SessionSettlementGuard::begin(&route.session_id);
    // Retain the durable capability's exact release generation across every
    // slow offer/start/CAS step. A DELETE that wins meanwhile flips this
    // token monotonically, so the late provisional worker cannot be renamed
    // into an already-ended public id.
    let Some(adoption) = state.transcode.session_adoption_token(&route.session_id) else {
        metric.outcome = TAKEOVER_SKIPPED;
        return Err("media-session adoption admission is full".to_owned());
    };
    // Settle the successor's numbering before spending any store read on it.
    // An epoch whose floor cannot be represented has no legal playlist, so
    // there is nothing to gain by discovering that after the work.
    let next_epoch = route
        .owner_epoch
        .checked_add(1)
        .ok_or_else(|| "media-session takeover epoch space exhausted".to_owned())?;
    let start_number = takeover_start_number(route.media_sequence, next_epoch)
        .ok_or_else(|| "media-session takeover sequence space exhausted".to_owned())?;
    let mut envelope = serde_json::from_str::<RemoteStartRequest>(&route.recipe_json)
        .map_err(|error| format!("invalid persisted takeover recipe: {error}"))?;
    if !takeover_recipe_matches_route(&envelope, &route) {
        // A route whose recipe no longer describes it is stale, not broken —
        // this node declines it. Counting it as a failure pages an operator
        // for ordinary rollout skew.
        metric.outcome = TAKEOVER_SKIPPED;
        return Err("persisted takeover recipe no longer matches its route".to_owned());
    }
    metric.method = match envelope.request.kind {
        SessionKind::Copy { .. } => TAKEOVER_METHOD_COPY,
        SessionKind::Transcode { .. } => TAKEOVER_METHOD_TRANSCODE,
    };
    // The rollout OPERATIONS.md prescribes turns this gate on while sessions
    // are already playing. Those sessions were created before the gate and
    // are serving `EXT-X-PLAYLIST-TYPE:EVENT`; a replacement generation
    // cannot satisfy EVENT semantics, and changing one URL's shape mid-film
    // is the failure this whole mechanism is supposed to avoid. Refusing is
    // the graceful degradation — the viewer restarts, which they would have
    // had to do before P7 anyway.
    if !envelope.typeless_playlist {
        metric.outcome = TAKEOVER_SKIPPED;
        return Err("takeover cannot replace a session serving an EVENT playlist".to_owned());
    }
    let file = tokio::time::timeout_at(deadline, state.store.get_file(envelope.request.file_id))
        .await
        .map_err(|_| "media-session takeover timed out".to_owned())?
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "takeover source is missing".to_owned())?;
    if !takeover_source_matches(&envelope, file.size, file.mtime) {
        // The library was rescanned under the session. Refusing is the
        // correct behaviour, so it is a skip and not a failure.
        metric.outcome = TAKEOVER_SKIPPED;
        return Err("takeover source revision changed".to_owned());
    }
    let (frontier_offset_ms, restart_ms) = takeover_resume(&route, &envelope.request.kind);
    envelope.request.start_seconds = restart_ms as f64 / 1_000.0;
    envelope.request.request_id = None;
    envelope.request.previous_session_id = None;
    envelope.request.reopen_reason = None;
    let target_height = match envelope.request.kind {
        SessionKind::Transcode { height } => height,
        SessionKind::Copy { .. } => file.height.unwrap_or(crate::transcode::MIN_HEIGHT),
    }
    .clamp(crate::transcode::MIN_HEIGHT, crate::transcode::MAX_HEIGHT);
    let offer_request = MediaOfferRequest::new(
        &file,
        target_height,
        restart_ms,
        envelope.request.audio_index,
        envelope.request.subtitle_burn,
        envelope.request.hdr10,
    )
    .map_err(str::to_owned)?;
    let offers = tokio::time::timeout_at(deadline, state.media_pool.offers(state, offer_request))
        .await
        .map_err(|_| "media-session takeover timed out".to_owned())?;
    if offers.selected_node_id.as_deref() != Some(state.node_id.as_str()) {
        metric.outcome = TAKEOVER_SKIPPED;
        return Ok(());
    }
    let user = tokio::time::timeout_at(deadline, state.store.get_user(route.user_id))
        .await
        .map_err(|_| "media-session takeover timed out".to_owned())?
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "takeover user is missing".to_owned())?;
    let provisional_id = uuid::Uuid::new_v4().to_string();
    let start = SessionTakeoverStart {
        provisional_session_id: provisional_id,
        incarnation_id: route.incarnation_id.clone(),
        origin_base_ms: route.media_origin_ms,
        frontier_offset_ms,
        media_sequence: start_number,
        discontinuity_sequence: route.discontinuity_sequence.saturating_add(1),
        owner_epoch: next_epoch,
    };
    let settlement_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = supervise_takeover_settlement(
            settlement_state,
            route,
            envelope.request,
            user.username,
            start,
            deadline,
            adoption,
            settlement,
            takeover_slot,
            metric,
        )
        .await
        {
            tracing::debug!(%error, "detached media-session takeover did not publish");
        }
    });
    Ok(())
}

/// The first HLS media sequence an ownership epoch may publish.
///
/// Each epoch owns a disjoint range, so a successor can never name a URI its
/// predecessor used — including segments the predecessor produced after its
/// last successful heartbeat, which the replicated `media_sequence` cannot
/// have seen. That is why the epoch floor and not the replicated sequence is
/// what the successor starts from.
///
/// `None` once the number would leave the range ffmpeg can represent: the HLS
/// muxer carries the segment number through a C `int`, so anything past
/// `i32::MAX` is truncated into a negative filename that no allowlist
/// accepts. Refusing the takeover beats publishing a playlist whose every
/// segment 404s. The check is on the number actually handed to the muxer, not
/// on the floor alone — a replicated sequence above the floor is what gets
/// published.
fn takeover_start_number(media_sequence: i64, next_epoch: i64) -> Option<i64> {
    next_epoch
        .checked_mul(TAKEOVER_SEQUENCE_STRIDE)
        .map(|floor| floor.max(media_sequence))
        .filter(|start| *start <= i64::from(i32::MAX))
}

/// How far behind the client's fetched frontier a successor restarts, and
/// where that lands on the source timeline.
///
/// The overlap is one whole segment of this session's own shape plus a
/// margin, never a fixed two seconds: `fetched_through_ms` advances to the
/// *end* of a segment as soon as the client requests it, so an owner that
/// dies during a fifteen-second copy segment has published a frontier well
/// past what the client holds. Anything less than a full segment of overlap
/// leaves media that no generation ever produces.
fn takeover_resume(route: &MediaSessionRoute, kind: &SessionKind) -> (i64, i64) {
    let overlap_ms = resume_overlap_ms(Some(kind));
    let frontier_offset_ms = route
        .fetched_through_ms
        .saturating_sub(overlap_ms)
        .clamp(0, route.produced_playable_through_ms);
    (
        frontier_offset_ms,
        route.media_origin_ms.saturating_add(frontier_offset_ms),
    )
}

/// Whether the persisted recipe still describes the file on this node's disk.
///
/// §7.3 requires an eligible candidate to prove the source snapshot, not to
/// assume a replicated row implies a mount. A row that could not be read when
/// the session started records an impossible `0/0` snapshot, so this is also
/// what refuses a takeover whose original placement never saw the file.
fn takeover_source_matches(envelope: &RemoteStartRequest, size: i64, mtime: i64) -> bool {
    size == envelope.source_size && mtime == envelope.source_mtime
}

fn takeover_recipe_matches_route(envelope: &RemoteStartRequest, route: &MediaSessionRoute) -> bool {
    takeover_recipe_is_valid(envelope)
        && envelope.incarnation_id == route.incarnation_id
        && envelope.user_id == route.user_id
}

/// What one lease tick may touch.
///
/// `live` is every session id this node is answerable for: the workers it can
/// produce for, plus routes protected during takeover or replacement.
/// `active` is the subset it can actually renew.
///
/// A protected route belongs to the first and not the second. It cannot report
/// a frontier while its old worker is gone and its successor is not durable:
/// leaving it out of `live` lets stale settlement delete the activation CAS
/// predecessor, while leaving it in `active` turns the missing frontier into a
/// false lease-loss verdict. Both halves are derived from the guard registry.
fn lease_tick_live(renewable_session_ids: Vec<String>) -> HashSet<String> {
    let mut live = renewable_session_ids.into_iter().collect::<HashSet<_>>();
    live.extend(settlement_protected_ids());
    live
}

fn lease_tick_active<'a>(
    routes: &'a [OwnedMediaSessionLease],
    live: &HashSet<String>,
) -> Vec<&'a OwnedMediaSessionLease> {
    let settling = settlement_protected_ids();
    routes
        .iter()
        .filter(|route| live.contains(&route.session_id) && !settling.contains(&route.session_id))
        .collect()
}

fn known_generation_missing_from_inventory(
    incarnation_id: &str,
    session_id: &str,
    inventory_incarnations: &HashSet<&str>,
    live_session_ids: &HashSet<String>,
    fresh_seed_incarnations: &HashSet<String>,
) -> bool {
    live_session_ids.contains(session_id)
        && !inventory_incarnations.contains(incarnation_id)
        && !fresh_seed_incarnations.contains(incarnation_id)
}

fn renewal_chunks<T>(items: &[T]) -> impl Iterator<Item = &[T]> {
    items.chunks(LEASE_RENEWAL_BATCH)
}

fn stale_settlement_retry_delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(4);
    STALE_SETTLEMENT_RETRY_BACKOFF
        .saturating_mul(1_u32 << exponent)
        .min(STALE_SETTLEMENT_MAX_BACKOFF)
}

fn release_reconciliation_retry_delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(5);
    RELEASE_RECONCILIATION_RETRY_BACKOFF
        .saturating_mul(1_u32 << exponent)
        .min(RELEASE_RECONCILIATION_MAX_BACKOFF)
}

fn stale_settlement_capacity(
    settling: usize,
    backoff: &HashMap<String, StaleSettlementBackoff>,
) -> usize {
    MAX_STALE_SETTLEMENTS_PER_TICK.saturating_sub(settling.saturating_add(backoff.len()))
}

fn take_stale_settlement_candidates(
    routes: &[OwnedMediaSessionLease],
    live: &HashSet<String>,
    settling: &mut HashSet<String>,
    backoff: &mut HashMap<String, StaleSettlementBackoff>,
    now: tokio::time::Instant,
    now_ms: i64,
) -> Vec<(OwnedMediaSessionLease, u32)> {
    let mut candidates = Vec::new();

    // A due retry already owns one of the fixed slots. Convert that exact slot
    // from retained backoff to in-flight before fresh rows are considered, so
    // route ordering cannot let unrelated work steal it.
    for route in routes {
        if live.contains(&route.session_id)
            || settling.contains(&route.session_id)
            || route.lease_expires_at_ms <= now_ms.saturating_add(STALE_SETTLEMENT_MIN_RUNWAY_MS)
        {
            continue;
        }
        let Some(retry) = backoff.get(&route.session_id).copied() else {
            continue;
        };
        if retry.next_attempt > now {
            continue;
        }
        backoff.remove(&route.session_id);
        settling.insert(route.session_id.clone());
        candidates.push((route.clone(), retry.failures));
    }

    let mut fresh_capacity = stale_settlement_capacity(settling.len(), backoff);
    for route in routes {
        if fresh_capacity == 0 {
            break;
        }
        if live.contains(&route.session_id)
            || settling.contains(&route.session_id)
            || backoff.contains_key(&route.session_id)
            || route.lease_expires_at_ms <= now_ms.saturating_add(STALE_SETTLEMENT_MIN_RUNWAY_MS)
        {
            continue;
        }
        settling.insert(route.session_id.clone());
        candidates.push((route.clone(), 0));
        fresh_capacity -= 1;
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    use axum::routing::get;
    use axum::Router;
    use bytes::Bytes;
    use futures_util::{stream, StreamExt};

    use crate::transcode::ReopenReason;

    pub(super) fn valid_start_request() -> RemoteStartRequest {
        let incarnation_id = "00000000-0000-4000-8000-0000000000a1".to_owned();
        RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: incarnation_id.clone(),
            user_id: 7,
            source_size: 123_456,
            source_mtime: 1_700_000_000,
            typeless_playlist: true,
            request: SessionRequest {
                control_sequence: None,
                file_id: 11,
                playback_id: "player-a".to_owned(),
                request_id: Some(incarnation_id),
                automatic: true,
                previous_session_id: None,
                reopen_reason: None,
                kind: SessionKind::Transcode { height: 720 },
                start_seconds: 12.5,
                audio_index: Some(1),
                subtitle_burn: None,
                audio_offset_ms: 0,
                hdr10: false,
                presentation: crate::transcode::Presentation::Vod,
                block_budget_secs: None,
            },
        }
    }

    fn valid_start_response() -> RemoteStartResponse {
        let session_id = "00000000-0000-4000-8000-0000000000b1".to_owned();
        RemoteStartResponse {
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            session_id,
            duration_ms: Some(7_200_000),
            start_seconds: 12.5,
            media_origin_seconds: 12.5,
            target_height: 720,
            kind: SessionKind::Transcode { height: 720 },
            encoder: "qsv".to_owned(),
            grade: OutputGrade::Sdr,
            vod: true,
            control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            activation_generation: Some(0),
        }
    }

    #[test]
    fn remote_start_status_carries_created_ownership_and_legacy_is_conservative() {
        let body = serde_json::to_vec(&valid_start_response()).expect("start response JSON");
        let created = decode_remote_start_response(crate::http::peer_transport::PeerResponse {
            status: reqwest::StatusCode::CREATED,
            body: body.clone(),
        })
        .expect("created response");
        assert_eq!(
            created.ownership,
            RemoteSessionStartOwnership::Created,
            "201 response carries creation ownership"
        );

        let recovered = decode_remote_start_response(crate::http::peer_transport::PeerResponse {
            status: reqwest::StatusCode::ALREADY_REPORTED,
            body: body.clone(),
        })
        .expect("recovered response");
        assert_eq!(
            recovered.ownership,
            RemoteSessionStartOwnership::Recovered,
            "208 response carries replay/recovery ownership"
        );
        assert_eq!(created.info.session_id, recovered.info.session_id);

        let mut legacy_info = valid_start_response();
        legacy_info.activation_generation = None;
        let legacy = decode_remote_start_response(crate::http::peer_transport::PeerResponse {
            status: reqwest::StatusCode::OK,
            body: serde_json::to_vec(&legacy_info).expect("legacy start response JSON"),
        })
        .expect("legacy response");
        assert_eq!(
            legacy.ownership,
            RemoteSessionStartOwnership::LegacyAmbiguous,
            "legacy success remains explicitly ambiguous"
        );
    }

    #[test]
    fn remote_activation_accepts_initial_generation_and_requires_request_claim() {
        let mut request = RemoteActivateRequest {
            target_generation: 0,
            activation: MediaSessionActivation {
                incarnation_id: "00000000-0000-4000-8000-0000000000a1".to_owned(),
                session_id: "00000000-0000-4000-8000-0000000000b1".to_owned(),
                user_id: 7,
                playback_id: "player-a".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: Some("request-a".to_owned()),
                request_fingerprint: "11".repeat(32),
                owner_node_id: "node-a".to_owned(),
                lease_expires_at_ms: 20_000,
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
                media_origin_ms: 0,
                now_ms: 10_000,
            },
        };
        assert!(request.is_valid_for("node-a"));
        request.activation.request_id = None;
        assert!(!request.is_valid_for("node-a"));
    }

    fn relay_request(resource: RelayResource, deadline_unix_ms: i64) -> RelayRequest {
        RelayRequest {
            session_id: uuid::Uuid::new_v4().to_string(),
            resource,
            deadline_unix_ms,
            headers: RelayHeaders::default(),
        }
    }

    #[test]
    fn relay_resource_deadlines_are_bounded_by_the_public_request_class() {
        let now = 1_700_000_000_000_i64;
        let short = relay_request(RelayResource::Status, now.saturating_add(90_000));
        assert_eq!(
            short.transport_budget_at(now),
            Some(RELAY_SHORT_MAX_LIFETIME),
            "an authenticated peer cannot turn status into an arbitrary wait"
        );
        assert_eq!(short.owner_budget_at(now), Some(RELAY_SHORT_MAX_LIFETIME));
        assert!(
            !short.deadline_is_plausible_at(now),
            "the authenticated envelope rejects a deadline beyond its resource ceiling"
        );

        let playlist = relay_request(RelayResource::VideoPlaylist, now.saturating_add(90_000));
        assert_eq!(
            playlist.owner_budget_at(now),
            Some(RELAY_PLAYLIST_MAX_LIFETIME)
        );
        let segment = relay_request(
            RelayResource::Segment {
                segment: "segment-1.m4s".to_owned(),
            },
            now.saturating_add(90_000),
        );
        assert_eq!(
            segment.owner_budget_at(now),
            Some(RELAY_SEGMENT_MAX_LIFETIME)
        );
    }

    #[test]
    fn relay_resource_deadline_preserves_the_earlier_origin_boundary() {
        let origin_now = 1_700_000_000_000_i64;
        let origin_deadline = origin_now.saturating_add(4_000);
        let request = relay_request(RelayResource::Status, origin_deadline);
        assert_eq!(
            request.transport_budget_at(origin_now),
            Some(Duration::from_secs(4))
        );

        // At 1.25 seconds of real elapsed time, model the maximum accepted
        // owner clock lag: its wall clock is still 0.75 seconds behind the
        // origin's send timestamp. Subtracting the allowance reconstructs the
        // same real absolute deadline as the ingress transport.
        let owner_now = origin_now.saturating_sub(750);
        let owner_remaining = request
            .owner_budget_at(owner_now)
            .expect("the origin deadline remains live");
        assert_eq!(owner_remaining, Duration::from_millis(2_750));

        let synchronized_owner_now = origin_now.saturating_add(1_250);
        assert_eq!(
            request.owner_budget_at(synchronized_owner_now),
            Some(Duration::from_millis(750)),
            "accepted skew is conservatively removed instead of becoming extra owner work"
        );
        assert!(
            request.owner_budget_at(origin_deadline).is_none(),
            "publication cannot start after ingress abandonment"
        );
    }

    #[test]
    fn relay_request_validation_rejects_a_far_future_authenticated_deadline() {
        let now = unix_ms();
        let valid = relay_request(
            RelayResource::Status,
            now.saturating_add(
                i64::try_from(
                    (RELAY_SHORT_MAX_LIFETIME + RELAY_DEADLINE_CLOCK_SKEW_ALLOWANCE).as_millis(),
                )
                .expect("relay lifetime fits in i64"),
            ),
        );
        assert!(valid.is_valid());

        let far_future = relay_request(RelayResource::Status, now.saturating_add(60_000));
        assert!(!far_future.is_valid());
    }

    pub(super) fn media_route(session_id: &str) -> MediaSessionRoute {
        MediaSessionRoute {
            incarnation_id: format!("incarnation-{session_id}"),
            session_id: session_id.to_owned(),
            user_id: 7,
            playback_id: "player-c".to_owned(),
            request_fingerprint: "c".repeat(64),
            owner_node_id: "node-c".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: unix_ms().saturating_add(10_000),
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: unix_ms(),
        }
    }

    fn owned_lease(session_id: &str) -> OwnedMediaSessionLease {
        OwnedMediaSessionLease {
            incarnation_id: format!("incarnation-{session_id}"),
            session_id: session_id.to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: unix_ms().saturating_add(10_000),
        }
    }

    struct ProbeTakeoverWorker {
        id: &'static str,
        events: Arc<StdMutex<Vec<String>>>,
        stopped: Option<Arc<tokio::sync::Notify>>,
        settled: bool,
    }

    impl ProbeTakeoverWorker {
        fn record(&self, event: &str) {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("worker:{}:{event}", self.id));
        }
    }

    impl Drop for ProbeTakeoverWorker {
        fn drop(&mut self) {
            if !self.settled {
                // Production Drop starts exact detached teardown. Recording
                // that transition makes cancellation/panic ownership visible
                // without needing a TranscodeManager fixture in every script.
                self.record("drop-teardown");
            }
        }
    }

    impl TakeoverWorkerLifecycle for ProbeTakeoverWorker {
        fn retain_until(&mut self, _expires_at_ms: i64, _monotonic_expiry: tokio::time::Instant) {
            self.record("retain");
        }

        fn stop(mut self, reason: &'static str) -> BoxFuture<'static, ()> {
            self.settled = true;
            let events = Arc::clone(&self.events);
            let stopped = self.stopped.take();
            let id = self.id;
            Box::pin(async move {
                events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("worker:{id}:stop:{reason}"));
                if let Some(stopped) = stopped {
                    stopped.notify_one();
                }
            })
        }

        fn stop_and_retain_until(
            mut self,
            reason: &'static str,
            expires_at_ms: i64,
            _monotonic_expiry: tokio::time::Instant,
        ) -> BoxFuture<'static, ()> {
            self.settled = true;
            let events = Arc::clone(&self.events);
            let stopped = self.stopped.take();
            let id = self.id;
            Box::pin(async move {
                events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("worker:{id}:stop-retain:{reason}:{expires_at_ms}"));
                if let Some(stopped) = stopped {
                    stopped.notify_one();
                }
            })
        }

        fn publish(mut self) {
            self.settled = true;
            self.record("publish");
        }
    }

    struct ProbeTakeoverAdoption(&'static str);

    struct ScriptedPause {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    struct ScriptedChildDrop(Arc<StdMutex<Vec<String>>>);

    impl Drop for ScriptedChildDrop {
        fn drop(&mut self) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push("creation-child:drop".to_owned());
        }
    }

    impl ScriptedPause {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            })
        }

        async fn wait(&self) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    enum ScriptedOutcome<T> {
        Ready(T),
        Error(&'static str),
        Never,
        Panic(&'static str),
        Paused(Arc<ScriptedPause>),
    }

    impl<T> ScriptedOutcome<T> {
        async fn resolve(self) -> Result<T, StoreError> {
            match self {
                Self::Ready(value) => Ok(value),
                Self::Error(message) => Err(StoreError::Database(message.to_owned())),
                Self::Never => std::future::pending().await,
                Self::Panic(message) => panic!("{message}"),
                Self::Paused(pause) => {
                    pause.wait().await;
                    std::future::pending().await
                }
            }
        }
    }

    struct ScriptedTakeoverIo {
        events: Arc<StdMutex<Vec<String>>>,
        claims: StdMutex<std::collections::VecDeque<ScriptedOutcome<Option<MediaSessionRoute>>>>,
        replays: StdMutex<std::collections::VecDeque<ScriptedOutcome<Option<MediaSessionRoute>>>>,
        reads: StdMutex<std::collections::VecDeque<ScriptedOutcome<Option<MediaSessionRoute>>>>,
        pins: StdMutex<std::collections::VecDeque<bool>>,
        adoptions: StdMutex<std::collections::VecDeque<bool>>,
        renewals: StdMutex<std::collections::VecDeque<ScriptedOutcome<bool>>>,
        cache_results: StdMutex<std::collections::VecDeque<bool>>,
        next_generation: AtomicU64,
    }

    impl ScriptedTakeoverIo {
        fn new(events: Arc<StdMutex<Vec<String>>>) -> Self {
            Self {
                events,
                claims: StdMutex::new(std::collections::VecDeque::new()),
                replays: StdMutex::new(std::collections::VecDeque::new()),
                reads: StdMutex::new(std::collections::VecDeque::new()),
                pins: StdMutex::new(std::collections::VecDeque::new()),
                adoptions: StdMutex::new(std::collections::VecDeque::new()),
                renewals: StdMutex::new(std::collections::VecDeque::new()),
                cache_results: StdMutex::new(std::collections::VecDeque::new()),
                next_generation: AtomicU64::new(10),
            }
        }

        fn record(&self, event: impl Into<String>) {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event.into());
        }

        fn events(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn allow_publication(&self) {
            self.pins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(true);
            self.renewals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(ScriptedOutcome::Ready(true));
            self.adoptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(true);
        }
    }

    impl TakeoverSettlementIo<ProbeTakeoverAdoption, ProbeTakeoverWorker> for ScriptedTakeoverIo {
        fn abort_inherited_preparation<'a>(
            &'a self,
            _route: &'a MediaSessionRoute,
        ) -> BoxFuture<'a, Result<(), StoreError>> {
            Box::pin(async { Ok(()) })
        }

        fn route_generation(&self, _session_id: &str) -> u64 {
            let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
            self.record(format!("generation:{generation}"));
            generation
        }

        fn claim<'a>(
            &'a self,
            _claim: &'a MediaSessionTakeover,
        ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
            Box::pin(async move {
                self.record("claim");
                let outcome = self
                    .claims
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .expect("scripted claim result");
                outcome.resolve().await
            })
        }

        fn replay<'a>(
            &'a self,
            _claim: &'a MediaSessionTakeover,
        ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
            Box::pin(async move {
                self.record("replay");
                let outcome = self
                    .replays
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .expect("scripted replay result");
                outcome.resolve().await
            })
        }

        fn read<'a>(
            &'a self,
            _incarnation_id: &'a str,
        ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
            Box::pin(async move {
                self.record("read");
                let outcome = self
                    .reads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .expect("scripted exact read result");
                outcome.resolve().await
            })
        }

        fn pin<'a>(
            &'a self,
            _provisional_id: &'a str,
            _route: &'a MediaSessionRoute,
        ) -> BoxFuture<'a, Result<bool, StoreError>> {
            Box::pin(async move {
                self.record("pin");
                Ok(self
                    .pins
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .expect("scripted pin result"))
            })
        }

        fn adopt<'a>(
            &'a self,
            _provisional_id: &'a str,
            durable_session_id: &'a str,
            worker: ProbeTakeoverWorker,
            adoption: ProbeTakeoverAdoption,
        ) -> BoxFuture<'a, Result<ProbeTakeoverWorker, ProbeTakeoverWorker>> {
            Box::pin(async move {
                self.record(format!("adopt:{}", adoption.0));
                let adopted = self
                    .adoptions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .expect("scripted adoption result");
                if adopted {
                    worker.record(&format!("adopted:{durable_session_id}"));
                    Ok(worker)
                } else {
                    Err(worker)
                }
            })
        }

        fn renew_first<'a>(
            &'a self,
            route: &'a MediaSessionRoute,
            local_session_id: &'a str,
            lease_expires_at_ms: i64,
        ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
            Box::pin(async move {
                self.record(format!("renew:{local_session_id}"));
                let outcome = self
                    .renewals
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .expect("scripted renewal result");
                let won = outcome.resolve().await?;
                Ok(won.then(|| {
                    let mut renewed = route.clone();
                    renewed.lease_expires_at_ms = lease_expires_at_ms;
                    renewed
                }))
            })
        }

        fn seed<'a>(&'a self, _route: &'a MediaSessionRoute) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.record("seed");
            })
        }

        fn cache<'a>(
            &'a self,
            _route: MediaSessionRoute,
            observed_generation: u64,
        ) -> BoxFuture<'a, bool> {
            Box::pin(async move {
                let accepted = self
                    .cache_results
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .unwrap_or(true);
                self.record(format!("cache:{observed_generation}:{accepted}"));
                accepted
            })
        }
    }

    fn scripted_takeover(
        events: Arc<StdMutex<Vec<String>>>,
    ) -> (
        PendingTakeoverSettlement<ProbeTakeoverWorker, ProbeTakeoverAdoption>,
        MediaSessionRoute,
    ) {
        let now_ms = unix_ms();
        let mut original = media_route("session-scripted-takeover");
        original.incarnation_id = "00000000-0000-4000-8000-0000000000d1".to_owned();
        original.owner_node_id = "node-old".to_owned();
        original.lease_expires_at_ms = now_ms.saturating_sub(1);
        original.updated_at_ms = now_ms.saturating_sub(2);
        let claim = MediaSessionTakeover {
            incarnation_id: original.incarnation_id.clone(),
            expected_owner_node_id: original.owner_node_id.clone(),
            expected_owner_epoch: original.owner_epoch,
            next_owner_node_id: "node-new".to_owned(),
            now_ms,
            lease_expires_at_ms: now_ms.saturating_add(60_000),
        };
        let mut winner = original.clone();
        winner.owner_node_id = claim.next_owner_node_id.clone();
        winner.owner_epoch = claim.expected_owner_epoch.saturating_add(1);
        winner.lease_expires_at_ms = claim.lease_expires_at_ms;
        winner.discontinuity_sequence = original.discontinuity_sequence.saturating_add(1);
        winner.updated_at_ms = claim.now_ms;
        (
            PendingTakeoverSettlement {
                original,
                claim,
                provisional_id: "provisional-scripted".to_owned(),
                worker: ProbeTakeoverWorker {
                    id: "worker-a",
                    events,
                    stopped: None,
                    settled: false,
                },
                adoption: ProbeTakeoverAdoption("adoption-a"),
                monotonic_expiry: tokio::time::Instant::now() + Duration::from_secs(60),
                claim_cache_generation: 9,
                metric: TakeoverMetricGuard::new(),
            },
            winner,
        )
    }

    fn terminal_relay_request() -> crate::playback_control::ControlRelayRequest {
        let generation = uuid::Uuid::new_v4().to_string();
        crate::playback_control::ControlRelayRequest {
            session_id: uuid::Uuid::new_v4().to_string(),
            generation: generation.clone(),
            expected_owner_node_id: "node-terminal".to_owned(),
            expected_owner_epoch: 1,
            deadline_unix_ms: unix_ms().saturating_add(4_000),
            control: crate::playback_control::ControlRequestV1 {
                intent: None,
                protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
                generation,
                control_epoch: 1,
                client_instance_id: uuid::Uuid::new_v4().to_string(),
                sequence: 7,
                demand: crate::playback_control::PlaybackDemand::End,
                position_ms: 10_000,
                buffered_from_ms: Some(9_000),
                buffered_through_ms: 25_000,
                playback_rate: 0.0,
                render_state: crate::playback_control::RenderState::Ended,
                seek_target_ms: None,
                observed_download_bps: Some(8_000_000),
                selection: crate::playback_control::ClientSelection {
                    quality: crate::playback_control::QualitySelection::Auto,
                    audio_track: Some(0),
                    subtitle: crate::playback_control::SubtitleSelection {
                        mode: crate::playback_control::SubtitleMode::Off,
                        track: None,
                    },
                    audio_offset_ms: 0,
                    codec: crate::playback_control::CodecPolicy::Auto,
                    dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
                },
                capabilities: Some(crate::playback_control::DynamicCapabilities {
                    platform: crate::playback_control::ClientPlatform::Web,
                    max_height: 2160,
                    codecs: vec![crate::playback_control::CodecPolicy::H264],
                    dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                    dual_player_preparation: false,
                }),
                observation: None,
                acknowledgement: None,
                supported_actions: None,
            },
        }
    }

    fn terminal_relay_response(
        request: &crate::playback_control::ControlRelayRequest,
    ) -> crate::playback_control::ControlResponseV1 {
        let server_time_unix_ms = unix_ms();
        crate::playback_control::ControlResponseV1 {
            protocol: crate::playback_control::PROTOCOL_V1.to_owned(),
            generation: request.generation.clone(),
            control_epoch: 1,
            accepted_sequence: request.control.sequence,
            server_time_unix_ms,
            lease: crate::playback_control::PlaybackLeaseView {
                state: "ended".to_owned(),
                renew_after_ms: crate::playback_control::NEXT_EXCHANGE_MS,
                expires_at_unix_ms: server_time_unix_ms,
            },
            delivery: crate::playback_control::DeliveryView {
                presentation: "vod".to_owned(),
                producer_state: "complete".to_owned(),
                produced_through_ms: Some(7_200_000),
                fetched_through_ms: 25_000,
                delivered_bps: None,
                delivered_idle_ms: None,
                recent_producer_speed: None,
                client_runway_ms: 15_000,
                admitted: Some(true),
                producer_decision: None,
                hold_reason: None,
                subtitle_readiness: None,
                owner_node_hash: "n-0123456789abcdef".to_owned(),
                owner_epoch: 1,
            },
            effective_selection: crate::playback_control::EffectiveSelection {
                quality_auto: true,
                height: 1080,
                audio_track: Some(0),
                subtitle_burn: None,
                audio_offset_ms: 0,
                codec: "source".to_owned(),
                dynamic_range: Some("sdr".to_owned()),
            },
            action: crate::playback_control::ControlAction::None,
        }
    }

    /// ffmpeg's HLS muxer carries the segment number through a C `int`. A
    /// number past `i32::MAX` is truncated into a negative filename that no
    /// allowlist accepts, so every segment of that generation 404s — reached
    /// by an ordinary second failover during one long film.
    #[test]
    fn the_published_start_number_stays_inside_what_ffmpeg_can_name() {
        // The floor, not the replicated sequence, is what a successor starts
        // from: the predecessor's frontier is seconds stale by design, so
        // segments it produced after its last heartbeat are numbered above
        // the sequence anyone replicated.
        assert_eq!(
            takeover_start_number(41, 2),
            Some(TAKEOVER_SEQUENCE_STRIDE * 2)
        );
        assert_eq!(
            takeover_start_number(TAKEOVER_SEQUENCE_STRIDE * 2 - 1, 2),
            Some(TAKEOVER_SEQUENCE_STRIDE * 2),
            "a predecessor sequence anywhere inside the prior epoch's range \
             cannot pull the successor below its own floor"
        );
        assert_eq!(
            takeover_start_number(3, 3).expect("second successor")
                - takeover_start_number(3, 2).expect("first successor"),
            TAKEOVER_SEQUENCE_STRIDE,
            "each epoch owns a strictly higher, equally wide range"
        );

        // The boundary is the whole point of the check: the epoch either side
        // of it must be decided differently.
        let ceiling = i64::from(i32::MAX);
        let last_epoch = ceiling / TAKEOVER_SEQUENCE_STRIDE;
        assert!(
            takeover_start_number(0, last_epoch).is_some_and(|start| start <= ceiling),
            "epoch {last_epoch} is the last representable one and must be allowed"
        );
        assert_eq!(
            takeover_start_number(0, last_epoch + 1),
            None,
            "the first epoch past the ceiling is refused, not truncated"
        );
        assert_eq!(takeover_start_number(i64::MAX, 2), None);
        assert_eq!(takeover_start_number(0, i64::MAX), None);
    }

    /// `fetched_through_ms` advances to the END of a segment as soon as the
    /// client requests it, so an owner that dies mid-response has published a
    /// frontier well past what the viewer holds. Overlapping by less than one
    /// whole segment of this session's own shape leaves a span of media that
    /// no generation produces, and the viewer sees a hard cut at the
    /// discontinuity.
    #[test]
    fn a_successor_overlaps_a_whole_segment_of_its_own_shape() {
        let mut route = media_route("session-resume");
        route.media_origin_ms = 90_000;
        route.produced_playable_through_ms = 600_000;
        route.fetched_through_ms = 600_000;

        let copy = SessionKind::Copy {
            convert_dolby_vision: false,
            aac: false,
            preserve_dolby_vision: false,
        };
        let (copy_offset, copy_restart) = takeover_resume(&route, &copy);
        let copy_overlap = route.fetched_through_ms - copy_offset;
        assert!(
            copy_overlap >= i64::from(plurx_core::transcode::COPY_SEGMENT_MAX_SECS) * 1_000,
            "a remux must re-cover the longest segment it could have been \
             serving when its owner died, not a fixed two seconds: {copy_overlap}"
        );
        assert_eq!(
            copy_restart,
            route.media_origin_ms + copy_offset,
            "the restart is measured from the row's write-once origin"
        );

        let transcode = SessionKind::Transcode { height: 1080 };
        let (transcode_offset, _) = takeover_resume(&route, &transcode);
        assert!(
            route.fetched_through_ms - transcode_offset
                >= i64::from(plurx_core::transcode::SEGMENT_SECONDS) * 1_000
        );
        assert!(
            transcode_offset > copy_offset,
            "a two-second segment does not pay a fifteen-second segment's overlap"
        );

        // Never behind the start of the film, and never past what was made.
        route.fetched_through_ms = 1_000;
        route.produced_playable_through_ms = 1_000;
        assert_eq!(takeover_resume(&route, &copy), (0, route.media_origin_ms));
        route.produced_playable_through_ms = 0;
        route.fetched_through_ms = 0;
        assert_eq!(takeover_resume(&route, &transcode).0, 0);
    }

    /// A rolling recipe the takeover path can act on is a transition, however
    /// long its lease has been dead. There is no deadline here on purpose: the
    /// gate that replaces a dead node's sessions is itself shut while that node
    /// is unresolved, so a survivor cannot know when a successor stops being
    /// possible — only whether it was ever possible at all.
    #[test]
    fn an_eligible_rolling_route_is_a_transition_however_long_it_has_been_dead() {
        let mut route = eligible_route("session-transition");

        assert!(matches!(
            classify_owner_loss(&route, EXPIRED_NOW_MS),
            OwnerLoss::Transitioning(_)
        ));

        route.lease_expires_at_ms = 1;
        assert!(
            matches!(
                classify_owner_loss(&route, EXPIRED_NOW_MS),
                OwnerLoss::Transitioning(_)
            ),
            "a lease dead since the epoch still says nothing about whether a successor is coming"
        );
    }

    /// The refusals that decide it are the takeover path's own, read off the
    /// durable recipe. Each is permanent: no amount of waiting turns a VOD
    /// handle, an EVENT playlist, or an exhausted sequence space into
    /// something a successor can replace.
    #[test]
    fn a_route_the_takeover_path_can_never_accept_is_unrecoverable() {
        let base = eligible_route("session-lost");
        let eligible = eligible_recipe(&base);

        let mut vod = base.clone();
        vod.recipe_json = serde_json::to_string(&RemoteStartRequest {
            request: SessionRequest {
                presentation: crate::transcode::Presentation::Vod,
                ..eligible.request.clone()
            },
            ..eligible.clone()
        })
        .expect("serialize vod recipe");
        assert!(
            matches!(
                classify_owner_loss(&vod, EXPIRED_NOW_MS),
                OwnerLoss::Unrecoverable(_)
            ),
            "an immutable VOD handle is never replaced by a renumbered successor"
        );

        let mut event = base.clone();
        event.recipe_json = serde_json::to_string(&RemoteStartRequest {
            typeless_playlist: false,
            ..eligible.clone()
        })
        .expect("serialize event recipe");
        assert!(
            matches!(
                classify_owner_loss(&event, EXPIRED_NOW_MS),
                OwnerLoss::Unrecoverable(_)
            ),
            "an EVENT playlist cannot be renumbered, so nothing will take it over"
        );

        let mut exhausted = base.clone();
        exhausted.recipe_json = event.recipe_json.clone();
        exhausted.recipe_json = serde_json::to_string(&eligible).expect("serialize");
        exhausted.owner_epoch = i64::MAX;
        assert!(
            matches!(
                classify_owner_loss(&exhausted, EXPIRED_NOW_MS),
                OwnerLoss::Unrecoverable(_)
            ),
            "an exhausted epoch space is permanent too"
        );

        let mut unreadable = base.clone();
        unreadable.recipe_json = "{}".to_owned();
        assert!(
            matches!(
                classify_owner_loss(&unreadable, EXPIRED_NOW_MS),
                OwnerLoss::Unrecoverable(_)
            ),
            "a recipe no successor could act on is refused rather than waited on"
        );

        let mut foreign = base.clone();
        foreign.recipe_json = serde_json::to_string(&RemoteStartRequest {
            user_id: eligible.user_id + 1,
            ..eligible.clone()
        })
        .expect("serialize foreign recipe");
        assert!(
            matches!(
                classify_owner_loss(&foreign, EXPIRED_NOW_MS),
                OwnerLoss::Unrecoverable(_)
            ),
            "a recipe that no longer describes its route is stale, and staleness does not heal"
        );
    }

    /// The position offered to a client is the one a successor would restart
    /// from, computed by the same overlap. `fetched_through_ms` runs to the
    /// *end* of a segment the moment the client asks for it, so handing it
    /// back unadjusted would tell a viewer to reopen past media they never
    /// saw — up to a whole copy segment of it.
    #[test]
    fn the_offered_resume_matches_what_a_successor_would_restart_from() {
        let mut route = eligible_route("session-resume");
        route.media_origin_ms = 90_000;
        route.fetched_through_ms = 45_000;
        route.produced_playable_through_ms = 61_000;
        let recipe = eligible_recipe(&route);

        let resume = classify_owner_loss(&route, EXPIRED_NOW_MS).resume();
        let (_, successor_restart_ms) = takeover_resume(&route, &recipe.request.kind);
        assert_eq!(
            resume.film_position_ms, successor_restart_ms,
            "the client is told to reopen exactly where a successor would have resumed"
        );
        assert!(
            resume.film_position_ms
                < route
                    .media_origin_ms
                    .saturating_add(route.fetched_through_ms),
            "and that is behind the fetched frontier, which overshoots by a whole segment"
        );
        assert_eq!(resume.film_frontier_ms, 90_000 + 61_000);
    }

    /// A recipe that will not parse still has to produce a usable resume, and
    /// the safe direction is early: repeating seen media is recoverable,
    /// skipping unseen media is not.
    #[test]
    fn an_unreadable_recipe_still_resumes_and_errs_early() {
        let mut route = media_route("session-unreadable");
        route.recipe_json = "{}".to_owned();
        route.media_origin_ms = 10_000;
        route.fetched_through_ms = 300_000;
        route.produced_playable_through_ms = 300_000;

        let resume = classify_owner_loss(&route, EXPIRED_NOW_MS).resume();
        assert_eq!(
            resume.film_position_ms,
            10_000 + 300_000 - resume_overlap_ms(None),
            "an unknown shape gets the wider of the two overlaps"
        );
        assert!(
            resume_overlap_ms(None)
                >= resume_overlap_ms(Some(&SessionKind::Transcode { height: 720 }))
        );
    }

    /// A session that served nothing, or whose frontier sits behind its own
    /// fetch pointer, still resumes inside the film rather than before it.
    #[test]
    fn a_resume_never_lands_before_the_session_start() {
        let mut route = media_route("session-empty");
        route.media_origin_ms = 90_000;
        route.fetched_through_ms = 0;
        route.produced_playable_through_ms = 0;

        let resume = classify_owner_loss(&route, EXPIRED_NOW_MS).resume();
        assert_eq!(resume.film_position_ms, 90_000);
        assert_eq!(resume.film_frontier_ms, 90_000);
    }

    /// The compatibility takeover path must not be expanded to VOD or EVENT
    /// sessions (remaining-roadmap §4). The VOD refusal is the recipe
    /// validator's; the EVENT refusal lives in `attempt_takeover` and is
    /// driven there by `attempt_takeover_refuses_an_event_session` in the HTTP
    /// tests, which is the only place an `AppState` fixture exists.
    #[test]
    fn the_takeover_recipe_validator_refuses_vod() {
        let route = eligible_route("session-nonexpansion");
        let eligible = eligible_recipe(&route);
        assert!(takeover_recipe_matches_route(&eligible, &route));

        let vod = RemoteStartRequest {
            request: SessionRequest {
                presentation: crate::transcode::Presentation::Vod,
                ..eligible.request.clone()
            },
            ..eligible.clone()
        };
        assert!(
            !takeover_recipe_matches_route(&vod, &route),
            "an immutable VOD handle is never replaced by a renumbered successor"
        );

        // A recipe written before the `typeless_playlist` flag existed
        // defaults to refusing rather than to guessing.
        let legacy: RemoteStartRequest = serde_json::from_value(serde_json::json!({
            "protocol_version": crate::media_pool::PROTOCOL_VERSION,
            "incarnation_id": eligible.incarnation_id,
            "user_id": eligible.user_id,
            "source_size": eligible.source_size,
            "source_mtime": eligible.source_mtime,
            "request": serde_json::to_value(&eligible.request).expect("request"),
        }))
        .expect("a recipe predating the flag still parses");
        assert!(!legacy.typeless_playlist);
    }

    const ELIGIBLE_INCARNATION: &str = "00000000-0000-4000-8000-0000000000f1";
    /// Later than any fixture lease, so the route's owner has stopped
    /// renewing and the recipe is what decides the answer.
    const EXPIRED_NOW_MS: i64 = i64::MAX / 2;

    fn eligible_route(session_id: &str) -> MediaSessionRoute {
        super::takeover_eligible_route(session_id, ELIGIBLE_INCARNATION)
    }

    /// The regression that shipped in the first version of this: a committed
    /// replacement holds a live lease for the whole of its publication
    /// window while it owns and renews. Its recipe is often ineligible — every
    /// VOD handle's is, by design — so a classifier that reads only the recipe
    /// tells a viewer their session is gone for the 372 seconds of an entirely
    /// ordinary handoff.
    #[test]
    fn a_renewing_successor_awaiting_publication_is_never_owner_loss() {
        let now_ms: i64 = 5_000_000;
        let mut route = media_route("session-publishing");
        // A VOD recipe: nothing will ever take this over, and nothing needs to.
        route.recipe_json = serde_json::to_string(&valid_start_request()).expect("serialize");
        route.publication_ready_at_ms =
            now_ms.saturating_add(plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS);
        route.lease_expires_at_ms = now_ms.saturating_add(1);

        assert!(
            matches!(
                classify_durable_route(Some(route.clone()), "node-other", now_ms),
                DurableRouteResolution::OwnerTransition(_)
            ),
            "the publication fence is the other producer of this classification"
        );
        assert!(
            matches!(
                classify_owner_loss(&route, now_ms),
                OwnerLoss::Transitioning(_)
            ),
            "a live lease is a live owner whatever its recipe says"
        );
        assert!(
            matches!(
                classify_owner_loss(&route, route.lease_expires_at_ms),
                OwnerLoss::Unrecoverable(_)
            ),
            "and once that lease stops being renewed, the recipe decides"
        );
    }

    fn eligible_recipe(route: &MediaSessionRoute) -> RemoteStartRequest {
        serde_json::from_str(&route.recipe_json).expect("the eligible fixture parses")
    }

    /// §7.3 requires a candidate to prove the source snapshot rather than
    /// assume a replicated row implies a mount. A session whose file row could
    /// not be read at creation records an impossible `0/0` snapshot, so it is
    /// refused too.
    #[test]
    fn only_an_unchanged_source_admits_a_takeover() {
        let envelope = valid_start_request();
        let (size, mtime) = (envelope.source_size, envelope.source_mtime);
        assert!(takeover_source_matches(&envelope, size, mtime));
        assert!(!takeover_source_matches(&envelope, size + 1, mtime));
        assert!(!takeover_source_matches(&envelope, size, mtime + 1));

        let unread = RemoteStartRequest {
            source_size: 0,
            source_mtime: 0,
            ..envelope
        };
        assert!(
            !takeover_source_matches(&unread, size, mtime),
            "a session started against a file the placement never read is not recoverable"
        );
    }

    #[test]
    fn takeover_claim_reconciliation_distinguishes_only_exact_authority() {
        let now_ms = 10_000;
        let mut original = media_route("session-claim");
        original.incarnation_id = "00000000-0000-4000-8000-0000000000c1".to_owned();
        original.owner_node_id = "node-old".to_owned();
        original.lease_expires_at_ms = now_ms - 1;
        original.updated_at_ms = now_ms - 2;
        let claim = MediaSessionTakeover {
            incarnation_id: original.incarnation_id.clone(),
            expected_owner_node_id: original.owner_node_id.clone(),
            expected_owner_epoch: original.owner_epoch,
            next_owner_node_id: "node-new".to_owned(),
            now_ms,
            lease_expires_at_ms: now_ms + LEASE_TTL_MS,
        };

        assert_eq!(
            takeover_claim_verdict(&original, &claim, Some(original.clone()), now_ms),
            TakeoverClaimVerdict::Pending,
            "the unchanged predecessor remains ambiguous after a lost reply"
        );

        let mut winner = original.clone();
        winner.owner_node_id = claim.next_owner_node_id.clone();
        winner.owner_epoch += 1;
        winner.discontinuity_sequence += 1;
        winner.lease_expires_at_ms = claim.lease_expires_at_ms;
        winner.updated_at_ms = claim.now_ms;
        assert!(matches!(
            takeover_claim_verdict(&original, &claim, Some(winner.clone()), now_ms),
            TakeoverClaimVerdict::Won(route) if *route == winner
        ));

        let mut renewed_winner = winner.clone();
        renewed_winner.lease_expires_at_ms += LEASE_TTL_MS;
        renewed_winner.updated_at_ms += 1;
        assert!(
            matches!(
                takeover_claim_verdict(
                    &original,
                    &claim,
                    Some(renewed_winner.clone()),
                    now_ms,
                ),
                TakeoverClaimVerdict::Won(route) if *route == renewed_winner
            ),
            "the exact target epoch remains ours after a legitimate renewal"
        );

        let mut renewed_predecessor = original.clone();
        renewed_predecessor.lease_expires_at_ms = now_ms + LEASE_TTL_MS;
        assert_eq!(
            takeover_claim_verdict(&original, &claim, Some(renewed_predecessor), now_ms,),
            TakeoverClaimVerdict::Lost,
            "a predecessor renewal makes the immutable expired-row CAS impossible"
        );

        let mut terminal = original.clone();
        terminal.state = "ended".to_owned();
        assert_eq!(
            takeover_claim_verdict(&original, &claim, Some(terminal), now_ms),
            TakeoverClaimVerdict::Lost
        );
        let mut competitor = winner.clone();
        competitor.owner_node_id = "node-other".to_owned();
        assert_eq!(
            takeover_claim_verdict(&original, &claim, Some(competitor), now_ms),
            TakeoverClaimVerdict::Lost
        );
        assert_eq!(
            takeover_claim_verdict(&original, &claim, None, now_ms),
            TakeoverClaimVerdict::Lost
        );
        assert_eq!(
            takeover_claim_verdict(&original, &claim, Some(winner), claim.lease_expires_at_ms,),
            TakeoverClaimVerdict::Lost,
            "a late commit has no authority beyond its immutable lease"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn takeover_reconciliation_retains_one_worker_across_pending_until_publication() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, winner) = scripted_takeover(Arc::clone(&events));
        let source = pending.original.clone();
        io.replays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend([ScriptedOutcome::Ready(None), ScriptedOutcome::Ready(None)]);
        io.reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend([
                ScriptedOutcome::Ready(Some(source)),
                ScriptedOutcome::Ready(Some(winner)),
            ]);
        io.pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(true);
        io.adoptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(true);
        io.renewals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Ready(true));

        reconcile_pending_takeover(&io, pending, None)
            .await
            .expect("scripted takeover publishes");
        assert_eq!(
            io.events(),
            vec![
                "generation:10",
                "replay",
                "generation:11",
                "read",
                "generation:12",
                "replay",
                "generation:13",
                "read",
                "pin",
                "worker:worker-a:retain",
                "renew:provisional-scripted",
                "adopt:adoption-a",
                "worker:worker-a:adopted:session-scripted-takeover",
                "seed",
                "worker:worker-a:publish",
                "cache:13:true",
            ],
            "Pending observations must not dispose the worker, and the winning read's cache generation follows seed/publication"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_initial_claim_error_and_timeout_reconcile_to_one_worker() {
        for mode in ["error", "timeout"] {
            let events = Arc::new(StdMutex::new(Vec::new()));
            let io = ScriptedTakeoverIo::new(Arc::clone(&events));
            let (pending, winner) = scripted_takeover(Arc::clone(&events));
            io.claims
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(if mode == "error" {
                    ScriptedOutcome::Error("scripted initial claim error")
                } else {
                    ScriptedOutcome::Never
                });
            io.replays
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(ScriptedOutcome::Ready(Some(winner)));
            io.allow_publication();

            settle_initial_takeover_claim(&io, pending)
                .await
                .expect("ambiguous initial claim must reconcile");
            let events = io.events();
            assert_eq!(events.first().map(String::as_str), Some("claim"));
            assert!(
                events.iter().any(|event| event == "replay"),
                "{mode}: {events:?}"
            );
            assert!(events.iter().any(|event| event.ends_with(":publish")));
            assert!(!events.iter().any(|event| event.contains("drop-teardown")));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_replay_error_and_timeout_fall_through_to_exact_read() {
        for mode in ["error", "timeout"] {
            let events = Arc::new(StdMutex::new(Vec::new()));
            let io = ScriptedTakeoverIo::new(Arc::clone(&events));
            let (pending, winner) = scripted_takeover(Arc::clone(&events));
            io.replays
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(if mode == "error" {
                    ScriptedOutcome::Error("scripted replay error")
                } else {
                    ScriptedOutcome::Never
                });
            io.reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(ScriptedOutcome::Ready(Some(winner)));
            io.allow_publication();

            reconcile_pending_takeover(&io, pending, None)
                .await
                .expect("ambiguous replay must reconcile through exact read");
            let events = io.events();
            assert!(events.iter().any(|event| event == "replay"));
            assert!(
                events.iter().any(|event| event == "read"),
                "{mode}: {events:?}"
            );
            assert!(events.iter().any(|event| event.ends_with(":publish")));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_exact_read_error_and_timeout_return_to_fixed_replay() {
        for mode in ["error", "timeout"] {
            let events = Arc::new(StdMutex::new(Vec::new()));
            let io = ScriptedTakeoverIo::new(Arc::clone(&events));
            let (pending, winner) = scripted_takeover(Arc::clone(&events));
            io.replays
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend([
                    ScriptedOutcome::Ready(None),
                    ScriptedOutcome::Ready(Some(winner)),
                ]);
            io.reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(if mode == "error" {
                    ScriptedOutcome::Error("scripted exact read error")
                } else {
                    ScriptedOutcome::Never
                });
            io.allow_publication();

            reconcile_pending_takeover(&io, pending, None)
                .await
                .expect("ambiguous exact read must return to fixed replay");
            let events = io.events();
            assert_eq!(events.iter().filter(|event| *event == "replay").count(), 2);
            assert!(
                events.iter().any(|event| event == "read"),
                "{mode}: {events:?}"
            );
            assert!(events.iter().any(|event| event.ends_with(":publish")));
        }
    }

    #[tokio::test]
    async fn settlement_supervisor_cancellation_drops_one_owned_worker() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = Arc::new(ScriptedTakeoverIo::new(Arc::clone(&events)));
        let (pending, _) = scripted_takeover(Arc::clone(&events));
        let pause = ScriptedPause::new();
        io.replays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Paused(Arc::clone(&pause)));
        let task = tokio::spawn({
            let io = Arc::clone(&io);
            async move { reconcile_pending_takeover(io.as_ref(), pending, None).await }
        });
        pause.entered.notified().await;

        task.abort();
        assert!(task
            .await
            .expect_err("settlement supervisor must cancel")
            .is_cancelled());
        assert_eq!(
            io.events(),
            vec!["generation:10", "replay", "worker:worker-a:drop-teardown",],
            "the reconciliation future owns exactly one teardown capability while suspended"
        );
    }

    #[tokio::test]
    async fn settlement_supervisor_panic_drops_one_owned_worker() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = Arc::new(ScriptedTakeoverIo::new(Arc::clone(&events)));
        let (pending, _) = scripted_takeover(Arc::clone(&events));
        io.replays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Panic("scripted replay panic"));
        let task = tokio::spawn({
            let io = Arc::clone(&io);
            async move { reconcile_pending_takeover(io.as_ref(), pending, None).await }
        });

        assert!(task
            .await
            .expect_err("settlement supervisor must propagate panic")
            .is_panic());
        assert_eq!(
            io.events(),
            vec!["generation:10", "replay", "worker:worker-a:drop-teardown",],
            "unwinding the supervisor cannot detach or duplicate worker ownership"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn creation_timeout_aborts_and_joins_child_before_worker_stop() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let pause = ScriptedPause::new();
        let handle: tokio::task::JoinHandle<Result<StartInfo, String>> = tokio::spawn({
            let events = Arc::clone(&events);
            let pause = Arc::clone(&pause);
            async move {
                let _drop = ScriptedChildDrop(events);
                pause.wait().await;
                std::future::pending().await
            }
        });
        pause.entered.notified().await;
        let worker = ProbeTakeoverWorker {
            id: "creation-worker",
            events: Arc::clone(&events),
            stopped: None,
            settled: false,
        };

        let result = TakeoverCreationOwner::new(handle, worker)
            .finish(tokio::time::Instant::now() + Duration::from_secs(1))
            .await;
        assert_eq!(
            result.err().as_deref(),
            Some("media-session takeover creation timed out")
        );
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![
                "creation-child:drop",
                "worker:creation-worker:stop:media-session takeover creation timed out",
            ],
            "worker teardown cannot begin until abort has joined the creation child"
        );
    }

    #[tokio::test]
    async fn creation_owner_drop_aborts_and_joins_child_before_worker_stop() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let pause = ScriptedPause::new();
        let handle: tokio::task::JoinHandle<Result<StartInfo, String>> = tokio::spawn({
            let events = Arc::clone(&events);
            let pause = Arc::clone(&pause);
            async move {
                let _drop = ScriptedChildDrop(events);
                pause.wait().await;
                std::future::pending().await
            }
        });
        pause.entered.notified().await;
        let stopped = Arc::new(tokio::sync::Notify::new());
        let worker = ProbeTakeoverWorker {
            id: "creation-worker",
            events: Arc::clone(&events),
            stopped: Some(Arc::clone(&stopped)),
            settled: false,
        };

        drop(TakeoverCreationOwner::new(handle, worker));
        tokio::time::timeout(Duration::from_secs(1), stopped.notified())
            .await
            .expect("detached creation-owner settlement must finish");
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![
                "creation-child:drop",
                "worker:creation-worker:stop:media-session takeover creation owner dropped",
            ],
            "supervisor cancellation transfers child join and worker teardown to one detached owner"
        );
    }

    #[tokio::test]
    async fn creation_panic_joins_child_before_worker_stop() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let handle: tokio::task::JoinHandle<Result<StartInfo, String>> = tokio::spawn({
            let events = Arc::clone(&events);
            async move {
                let _drop = ScriptedChildDrop(events);
                panic!("scripted creation panic");
            }
        });
        let worker = ProbeTakeoverWorker {
            id: "creation-worker",
            events: Arc::clone(&events),
            stopped: None,
            settled: false,
        };

        let error = match TakeoverCreationOwner::new(handle, worker)
            .finish(tokio::time::Instant::now() + Duration::from_secs(30))
            .await
        {
            Ok(_) => panic!("creation panic must fail settlement"),
            Err(error) => error,
        };
        assert!(error.starts_with("media-session takeover creation task failed:"));
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![
                "creation-child:drop",
                "worker:creation-worker:stop:media-session takeover creation task failed",
            ],
            "JoinError is observed only after child unwind, then the worker stops"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cache_generation_rejection_cannot_unpublish_the_owned_worker() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, winner) = scripted_takeover(Arc::clone(&events));
        io.allow_publication();
        io.cache_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(false);

        reconcile_pending_takeover(&io, pending, Some(winner))
            .await
            .expect("cache generation rejection leaves Store ownership authoritative");
        let events = io.events();
        let publish = events
            .iter()
            .position(|event| event.ends_with(":publish"))
            .expect("worker publication");
        let rejected_cache = events
            .iter()
            .position(|event| event == "cache:9:false")
            .expect("scripted generation rejection");
        assert!(publish < rejected_cache, "{events:?}");
        assert!(!events.iter().any(|event| event.contains("drop-teardown")));
    }

    #[tokio::test(start_paused = true)]
    async fn takeover_reconciliation_stops_exactly_once_on_definitive_loss() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, _) = scripted_takeover(Arc::clone(&events));
        let mut terminal = pending.original.clone();
        terminal.state = "ended".to_owned();
        io.replays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Ready(None));
        io.reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Ready(Some(terminal)));

        reconcile_pending_takeover(&io, pending, None)
            .await
            .expect("definitive loss is a settled non-error verdict");
        assert_eq!(
            io.events(),
            vec![
                "generation:10",
                "replay",
                "generation:11",
                "read",
                "worker:worker-a:stop:media-session takeover lost",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn takeover_bootstrap_renewal_loss_never_publishes_the_durable_identity() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let io = ScriptedTakeoverIo::new(Arc::clone(&events));
        let (pending, winner) = scripted_takeover(Arc::clone(&events));
        io.pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(true);
        io.renewals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ScriptedOutcome::Ready(false));

        reconcile_pending_takeover(&io, pending, Some(winner))
            .await
            .expect("a lost bootstrap renewal is a settled loss");
        let events = io.events();
        assert!(events.iter().any(|event| event == "pin"));
        assert!(events
            .iter()
            .any(|event| event == "renew:provisional-scripted"));
        assert!(events.iter().any(|event| {
            event == "worker:worker-a:stop:media-session takeover bootstrap renewal lost"
        }));
        assert!(
            !events.iter().any(|event| {
                event.starts_with("adopt:")
                    || event.contains(":adopted:")
                    || event == "seed"
                    || event.ends_with(":publish")
                    || event.starts_with("cache:")
            }),
            "the durable capability must remain unreachable unless its fresh lease commits: {events:?}"
        );
        assert!(!events.iter().any(|event| event.contains("drop-teardown")));
    }

    #[tokio::test(start_paused = true)]
    async fn bootstrap_renewal_error_and_timeout_retain_without_adoption() {
        for mode in ["error", "timeout"] {
            let events = Arc::new(StdMutex::new(Vec::new()));
            let io = ScriptedTakeoverIo::new(Arc::clone(&events));
            let (pending, winner) = scripted_takeover(Arc::clone(&events));
            io.pins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(true);
            io.renewals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(if mode == "error" {
                    ScriptedOutcome::Error("scripted bootstrap renewal error")
                } else {
                    ScriptedOutcome::Never
                });

            assert!(
                reconcile_pending_takeover(&io, pending, Some(winner))
                    .await
                    .is_err(),
                "{mode} must remain an ambiguous failure"
            );
            let events = io.events();
            assert!(events
                .iter()
                .any(|event| event == "renew:provisional-scripted"));
            assert!(
                events.iter().any(|event| {
                    event.starts_with(
                        "worker:worker-a:stop-retain:media-session takeover bootstrap renewal",
                    )
                }),
                "{mode}: {events:?}"
            );
            assert!(
                !events.iter().any(|event| {
                    event.starts_with("adopt:")
                        || event.contains(":adopted:")
                        || event == "seed"
                        || event.ends_with(":publish")
                        || event.starts_with("cache:")
                }),
                "{mode}: an ambiguous renewal cannot expose the durable identity: {events:?}"
            );
            assert!(!events.iter().any(|event| event.contains("drop-teardown")));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn takeover_publication_failures_stop_and_retain_the_known_winner() {
        for failure in ["runway", "pin", "adoption"] {
            let events = Arc::new(StdMutex::new(Vec::new()));
            let io = ScriptedTakeoverIo::new(Arc::clone(&events));
            let (mut pending, mut winner) = scripted_takeover(Arc::clone(&events));
            if failure == "runway" {
                let expires_at_ms = unix_ms().saturating_add(5_000);
                pending.claim.lease_expires_at_ms = expires_at_ms;
                pending.monotonic_expiry = tokio::time::Instant::now() + Duration::from_secs(5);
                winner.lease_expires_at_ms = expires_at_ms;
            } else {
                io.pins
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_back(failure != "pin");
                if failure == "adoption" {
                    io.renewals
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push_back(ScriptedOutcome::Ready(true));
                    io.adoptions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push_back(false);
                }
            }
            let expected_expiry = pending.claim.lease_expires_at_ms;

            assert!(
                reconcile_pending_takeover(&io, pending, Some(winner))
                    .await
                    .is_err(),
                "{failure} failure must not publish"
            );
            let events = io.events();
            assert!(events.iter().any(|event| {
                event.starts_with("worker:worker-a:stop-retain:")
                    && (failure == "adoption" || event.ends_with(&expected_expiry.to_string()))
            }), "{failure}: known-winner cleanup must retain settlement through its current exact lease: {events:?}");
            assert!(
                !events.iter().any(|event| event.ends_with(":publish")
                    || event == "seed"
                    || event.starts_with("cache:")),
                "{failure}: publication side effects are forbidden after failure: {events:?}"
            );
            if failure == "adoption" {
                assert!(events.iter().any(|event| event == "pin"));
                assert!(events
                    .iter()
                    .any(|event| event == "renew:provisional-scripted"));
                assert!(events.iter().any(|event| event == "adopt:adoption-a"));
                assert!(!events.iter().any(|event| event.contains(":adopted:")));
            }
            assert!(!events.iter().any(|event| event.contains("drop-teardown")));
        }
    }

    /// A settling takeover or replacement cannot report a frontier while its
    /// process-local worker is between generations. Left in the renewal batch,
    /// that absence reads as lost ownership; left out of `live`, stale
    /// settlement deletes the durable route needed by the activation CAS.
    #[test]
    fn a_settlement_protected_route_is_live_but_not_renewable_or_stale() {
        let routes = vec![
            owned_lease("session-live"),
            owned_lease("session-settling"),
            owned_lease("session-gone"),
        ];
        let renewable = vec!["session-live".to_owned()];

        let settling = SessionSettlementGuard::begin("session-settling");
        let live = lease_tick_live(renewable.clone());
        assert!(live.contains("session-settling"), "{live:?}");
        assert!(live.contains("session-live"), "{live:?}");
        assert!(!live.contains("session-gone"), "{live:?}");
        let ids = lease_tick_active(&routes, &live)
            .iter()
            .map(|route| route.session_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["session-live"],
            "only a route with a reportable frontier may be renewed or fenced"
        );
        let mut stale_in_flight = HashSet::new();
        let mut backoff = HashMap::new();
        let stale = take_stale_settlement_candidates(
            &routes,
            &live,
            &mut stale_in_flight,
            &mut backoff,
            tokio::time::Instant::now(),
            unix_ms(),
        );
        assert_eq!(
            stale
                .iter()
                .map(|(route, _)| route.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["session-gone"],
            "the protected predecessor must retain the pointer needed by successor activation"
        );

        // A second attempt for the same route is ordinary; the loser's guard
        // must not un-protect the winner.
        let overlapping = SessionSettlementGuard::begin("session-settling");
        drop(overlapping);
        let live = lease_tick_live(renewable);
        assert!(
            live.contains("session-settling"),
            "an overlapping attempt's guard must not release the winner's protection"
        );
        assert!(
            lease_tick_active(&routes, &live)
                .iter()
                .all(|route| route.session_id != "session-settling"),
            "the settling route is still excluded while any holder remains"
        );

        // Adoption completes: the worker is now registered under the durable
        // id, so it is renewable on its own and the guard is released.
        drop(settling);
        let live = lease_tick_live(vec![
            "session-live".to_owned(),
            "session-settling".to_owned(),
        ]);
        let ids = lease_tick_active(&routes, &live)
            .iter()
            .map(|route| route.session_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["session-live", "session-settling"],
            "once the last guard drops the route renews normally"
        );

        let no_worker = lease_tick_live(Vec::new());
        let mut stale_in_flight = HashSet::new();
        let mut backoff = HashMap::new();
        let stale = take_stale_settlement_candidates(
            &[owned_lease("session-settling")],
            &no_worker,
            &mut stale_in_flight,
            &mut backoff,
            tokio::time::Instant::now(),
            unix_ms(),
        );
        assert_eq!(
            stale.len(),
            1,
            "after the transition resolves, an actually orphaned route settles normally"
        );
    }

    #[test]
    fn a_seed_published_after_inventory_is_not_false_lease_loss() {
        let incarnation = "incarnation-fresh";
        let session = "session-fresh";
        let inventory = HashSet::new();
        let live = HashSet::from([session.to_owned()]);
        let fresh = HashSet::from([incarnation.to_owned()]);
        assert!(
            !known_generation_missing_from_inventory(
                incarnation,
                session,
                &inventory,
                &live,
                &fresh,
            ),
            "a claim published after this tick's Store snapshot belongs to the next tick"
        );
        assert!(
            known_generation_missing_from_inventory(
                incarnation,
                session,
                &inventory,
                &live,
                &HashSet::new(),
            ),
            "the exemption is exact and lasts only while the seed is fresh"
        );
    }

    #[test]
    fn remote_start_contract_rejects_unfenced_or_noncanonical_inputs() {
        let request = valid_start_request();
        assert!(request.is_valid());
        assert!(worker_session_request_is_valid(&request.request));

        let mut incompatible = request.clone();
        incompatible.protocol_version = crate::media_pool::PROTOCOL_VERSION.saturating_sub(1);
        assert!(!incompatible.is_valid());

        let mut removed_presentation = request.clone();
        removed_presentation.request.presentation = crate::transcode::Presentation::Live;
        assert!(!removed_presentation.is_valid());
        assert!(takeover_recipe_is_valid(&removed_presentation));
        assert!(!takeover_recipe_is_valid(&request));

        let mut mismatched = request.clone();
        mismatched.request.request_id = Some("00000000-0000-4000-8000-0000000000ff".to_owned());
        assert!(!mismatched.is_valid());

        let mut unbound_reopen = request.clone();
        unbound_reopen.request.reopen_reason = Some(ReopenReason::Stall);
        assert!(!unbound_reopen.is_valid());

        let mut traversal = request.clone();
        traversal.request.playback_id = "player\r\nforged".to_owned();
        assert!(!traversal.is_valid());

        let mut blank_playback = request.clone();
        blank_playback.request.playback_id = "   ".to_owned();
        assert!(!worker_session_request_is_valid(&blank_playback.request));
        assert!(!blank_playback.is_valid());

        let mut too_late = request.clone();
        too_late.request.start_seconds = MAX_MEDIA_MILLIS as f64 / 1_000.0 + 0.001;
        assert!(!worker_session_request_is_valid(&too_late.request));
        assert!(!too_late.is_valid());

        let mut audio_out_of_range = request.clone();
        audio_out_of_range.request.audio_index = Some(1_025);
        assert!(!worker_session_request_is_valid(
            &audio_out_of_range.request
        ));
        assert!(!audio_out_of_range.is_valid());

        let mut subtitle_out_of_range = request.clone();
        subtitle_out_of_range.request.subtitle_burn = Some(-1);
        assert!(!worker_session_request_is_valid(
            &subtitle_out_of_range.request
        ));
        assert!(!subtitle_out_of_range.is_valid());

        let mut json = serde_json::to_value(request).expect("serialize start request");
        json.get_mut("request")
            .and_then(serde_json::Value::as_object_mut)
            .expect("request object")
            .insert("future_unfenced_field".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<RemoteStartRequest>(json).is_err());
    }

    #[test]
    fn remote_start_response_is_bound_to_the_session_capability_and_reports_presentation() {
        let response = valid_start_response();
        assert!(response.is_valid());

        let mut recovery_presentation = response.clone();
        recovery_presentation.vod = false;
        assert!(recovery_presentation.is_valid());

        let mut wrong_path = response.clone();
        wrong_path.playlist_url = "/api/v1/hls/somebody-else/index.m3u8".to_owned();
        assert!(!wrong_path.is_valid());

        let mut mismatched_height = response.clone();
        mismatched_height.kind = SessionKind::Transcode { height: 1080 };
        assert!(!mismatched_height.is_valid());

        let mut nonfinite = response;
        nonfinite.start_seconds = f64::NAN;
        assert!(!nonfinite.is_valid());

        let mut too_late = valid_start_response();
        too_late.media_origin_seconds = MAX_MEDIA_MILLIS as f64 / 1_000.0 + 0.001;
        assert!(!too_late.is_valid());

        let mut too_long = valid_start_response();
        too_long.duration_ms = Some(MAX_MEDIA_MILLIS + 1);
        assert!(!too_long.is_valid());

        let mut unknown_copy = valid_start_response();
        unknown_copy.kind = SessionKind::Copy {
            convert_dolby_vision: false,
            aac: false,
            preserve_dolby_vision: false,
        };
        unknown_copy.target_height = 0;
        assert!(unknown_copy.is_valid());
        unknown_copy.target_height = crate::transcode::MAX_HEIGHT + 1;
        assert!(unknown_copy.is_valid());
        unknown_copy.target_height = -1;
        assert!(!unknown_copy.is_valid());

        let mut rolling = valid_start_response();
        rolling.control_lease_timeout_ms = crate::playback_control::ROLLING_LEASE_TIMEOUT_MS;
        assert!(
            rolling.is_valid(),
            "the rolling registry's real TTL is valid"
        );
        rolling.control_lease_timeout_ms = 42_000;
        assert!(
            !rolling.is_valid(),
            "a worker cannot invent a lease lifetime the owning registry does not enforce"
        );
    }

    #[test]
    fn control_admission_bounds_retries_and_uuid_spray_before_store_work() {
        let started = Instant::now();
        let mut admission = ControlAdmission {
            window_started: started,
            admitted: 0,
            sessions: HashMap::new(),
        };
        for _ in 0..CONTROL_RATE_PER_SESSION {
            assert_eq!(admission.admit(started, "one-session"), Ok(()));
        }
        assert!(admission.admit(started, "one-session").is_err());
        assert_eq!(
            admission.admit(started + CONTROL_RATE_WINDOW, "one-session"),
            Ok(()),
            "the bounded retry budget reopens after its fixed window"
        );

        let next_window = started + CONTROL_RATE_WINDOW * 2;
        let mut spray = ControlAdmission {
            window_started: next_window,
            admitted: 0,
            sessions: HashMap::new(),
        };
        for index in 0..CONTROL_RATE_GLOBAL {
            assert_eq!(
                spray.admit(next_window, &format!("random-capability-{index}")),
                Ok(())
            );
        }
        assert!(spray.admit(next_window, "one-too-many").is_err());
        assert!(
            spray.sessions.len() <= MAX_CONTROL_RATE_ENTRIES,
            "the admission map itself must stay bounded"
        );
    }

    #[test]
    fn relay_resource_names_are_single_safe_path_components() {
        for value in ["seg00001.ts", "init.mp4", "subtitle-2.vtt"] {
            assert!(valid_resource_name(value), "{value}");
        }
        for value in ["", ".", "..", "../secret", "nested/segment.ts", "x\r\ny"] {
            assert!(!valid_resource_name(value), "{value}");
        }
    }

    #[test]
    fn relay_headers_and_renewal_batches_stay_bounded() {
        let legacy_abort: RemoteAbortRequest = serde_json::from_str(&format!(
            r#"{{"incarnation_id":"{}","session_id":"{}"}}"#,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ))
        .expect("new owner accepts legacy start-abort envelope");
        assert!(legacy_abort.reason.is_none());
        assert!(legacy_abort.is_valid());
        for terminal in [
            crate::vodserve::Terminal::Deleted,
            crate::vodserve::Terminal::Superseded,
            crate::vodserve::Terminal::AdminStop,
            crate::vodserve::Terminal::Revoked,
            crate::vodserve::Terminal::Replaced,
        ] {
            let terminal_abort = RemoteAbortRequest {
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: uuid::Uuid::new_v4().to_string(),
                expected_owner_epoch: 1,
                reason: Some(terminal.control_reason().to_owned()),
            };
            assert!(terminal_abort.is_valid(), "{terminal:?}");
            assert!(serde_json::to_string(&terminal_abort)
                .expect("terminal-abort JSON")
                .contains(terminal.control_reason()));
        }
        let release_abort = RemoteAbortRequest {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            expected_owner_epoch: 1,
            reason: Some(
                crate::vodserve::Terminal::Deleted
                    .control_reason()
                    .to_owned(),
            ),
        };
        assert!(release_abort.is_valid());

        let headers = RelayHeaders {
            range: Some("bytes=0-99".to_owned()),
            if_range: Some("\"generation\"".to_owned()),
            if_none_match: Some("\"generation\"".to_owned()),
            if_modified_since: Some("Sun, 23 Aug 2026 08:00:00 GMT".to_owned()),
        };
        assert!(headers.is_valid());
        assert!(!RelayHeaders {
            range: Some("bytes=0-1\r\nx-forged: true".to_owned()),
            ..RelayHeaders::default()
        }
        .is_valid());
        assert!(!RelayHeaders {
            if_none_match: Some("x".repeat(1_025)),
            ..RelayHeaders::default()
        }
        .is_valid());
        assert!(!RelayHeaders {
            if_range: Some("x".repeat(1_025)),
            ..RelayHeaders::default()
        }
        .is_valid());

        let mut public = axum::http::HeaderMap::new();
        public.insert(
            axum::http::header::RANGE,
            "bytes=10-19".parse().expect("Range"),
        );
        public.insert(
            axum::http::header::IF_RANGE,
            "\"generation\"".parse().expect("If-Range"),
        );
        let forwarded = RelayHeaders::from_http(&public);
        assert_eq!(forwarded.range.as_deref(), Some("bytes=10-19"));
        assert_eq!(forwarded.if_range.as_deref(), Some("\"generation\""));
        let downgraded = forwarded.for_unversioned_peer();
        assert!(downgraded.range.is_none());
        assert!(downgraded.if_range.is_none());
        let encoded = serde_json::to_value(&downgraded).expect("encode old-peer headers");
        let object = encoded.as_object().expect("relay header object");
        assert!(
            !object.contains_key("if_range"),
            "a new node must not send the unknown field to an old strict peer"
        );
        assert!(
            !object.contains_key("range"),
            "the old peer must fall back to a complete representation"
        );
        let decoded: RelayHeaders = serde_json::from_str(
            r#"{"range":"bytes=0-9","if_none_match":null,"if_modified_since":null}"#,
        )
        .expect("new owner accepts the old relay JSON shape");
        assert_eq!(decoded.range.as_deref(), Some("bytes=0-9"));
        assert!(decoded.if_range.is_none());
        let legacy_range_only = decoded.for_unversioned_peer();
        assert!(
            legacy_range_only.range.is_none(),
            "an unversioned Range-only envelope has no validator proof and must become full 200"
        );

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyAbortRequest {
            incarnation_id: String,
            session_id: String,
        }
        let reason_body = serde_json::to_vec(&release_abort).expect("new abort body");
        assert!(
            serde_json::from_slice::<LegacyAbortRequest>(&reason_body).is_err(),
            "the old strict endpoint rejects the reason-bearing envelope"
        );
        let fallback =
            remote_abort_legacy_retry_body(&release_abort, reqwest::StatusCode::BAD_REQUEST)
                .expect("legacy abort fallback")
                .expect("a rejected reason request retries once");
        let decoded = serde_json::from_slice::<LegacyAbortRequest>(&fallback)
            .expect("old endpoint accepts fallback body");
        assert_eq!(decoded.incarnation_id, release_abort.incarnation_id);
        assert_eq!(decoded.session_id, release_abort.session_id);
        assert!(
            remote_abort_legacy_retry_body(&release_abort, reqwest::StatusCode::NO_CONTENT)
                .expect("new-peer response")
                .is_none()
        );

        let routes = vec![(); LEASE_RENEWAL_BATCH * 2 + 1];
        assert_eq!(
            renewal_chunks(&routes)
                .map(|chunk| chunk.len())
                .collect::<Vec<_>>(),
            vec![LEASE_RENEWAL_BATCH, LEASE_RENEWAL_BATCH, 1]
        );
        let admitted_owner = vec![(); LEASE_RENEWAL_BATCH * LEASE_RENEWAL_FANOUT];
        assert_eq!(
            renewal_chunks(&admitted_owner).count(),
            LEASE_RENEWAL_FANOUT
        );
        assert!(
            REMOTE_ACTIVATION_CONFIRMATION_WINDOW
                > START_DEADLINE
                    + OWNER_ASSIGNMENT_DEADLINE
                    + Duration::from_millis(LEASE_TTL_MS as u64),
            "the worker fallback must outlive ingress plus the fixed activation lease"
        );
        assert!(
            Duration::from_millis(TAKEOVER_CLAIM_LEASE_TTL_MS as u64)
                > TAKEOVER_PUBLICATION_MIN_RUNWAY + TAKEOVER_RECONCILIATION_STORE_DEADLINE,
            "the one-shot claim must outlive publication with a full first-renewal runway"
        );
    }

    #[test]
    fn stale_settlement_retries_retain_a_bounded_backoff_slot() {
        assert_eq!(
            (1..=6)
                .map(stale_settlement_retry_delay)
                .collect::<Vec<_>>(),
            vec![
                Duration::from_secs(30),
                Duration::from_secs(60),
                Duration::from_secs(120),
                Duration::from_secs(240),
                Duration::from_secs(300),
                Duration::from_secs(300),
            ]
        );

        let now = tokio::time::Instant::now();
        let mut backoff = (0..MAX_STALE_SETTLEMENTS_PER_TICK - 1)
            .map(|index| {
                (
                    format!("deferred-{index}"),
                    StaleSettlementBackoff {
                        failures: 1,
                        next_attempt: now + Duration::from_secs(30),
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(stale_settlement_capacity(1, &backoff), 0);

        backoff.get_mut("deferred-0").expect("fixture").next_attempt = now;
        assert_eq!(
            stale_settlement_capacity(1, &backoff),
            0,
            "a due retry retains its slot until that exact retry is selected"
        );

        let mut settling = HashSet::from(["in-flight".to_owned()]);
        let candidates = take_stale_settlement_candidates(
            &[owned_lease("fresh-first"), owned_lease("deferred-0")],
            &HashSet::new(),
            &mut settling,
            &mut backoff,
            now,
            unix_ms(),
        );
        assert_eq!(
            candidates
                .iter()
                .map(|(route, failures)| (route.session_id.as_str(), *failures))
                .collect::<Vec<_>>(),
            vec![("deferred-0", 1)],
            "a due retry reclaims its own slot before a fresh earlier route"
        );
        assert!(!settling.contains("fresh-first"));
        assert_eq!(
            settling.len() + backoff.len(),
            MAX_STALE_SETTLEMENTS_PER_TICK
        );
    }

    #[test]
    fn stale_inventory_route_is_not_settled_after_its_lease_runway_is_spent() {
        let inventory_now_ms = unix_ms();
        let mut route = owned_lease("expired-after-inventory");
        route.lease_expires_at_ms = inventory_now_ms.saturating_add(1_000);
        let candidate_now_ms = inventory_now_ms.saturating_add(1_001);
        let mut settling = HashSet::new();
        let mut backoff = HashMap::new();
        let candidates = take_stale_settlement_candidates(
            &[route],
            &HashSet::new(),
            &mut settling,
            &mut backoff,
            tokio::time::Instant::now(),
            candidate_now_ms,
        );
        assert!(
            candidates.is_empty(),
            "a pre-expiry inventory row must remain takeover-eligible once candidate evaluation crosses expiry"
        );
        assert!(settling.is_empty());
    }

    #[tokio::test]
    async fn route_misses_are_single_flight_and_activation_supersedes_them() {
        use plurx_core::cluster::membership::MembershipManager;
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let coordinator = MediaSessionCoordinator::new(MembershipManager::unavailable(), store);
        let session_id = "00000000-0000-4000-8000-0000000000c1";
        assert!(coordinator
            .route(session_id)
            .await
            .expect("first miss")
            .is_none());
        assert!(coordinator
            .route(session_id)
            .await
            .expect("negative-cache hit")
            .is_none());
        assert_eq!(
            coordinator
                .route_store_queries
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "a repeated random capability must cause only one Store read"
        );

        let mut route = media_route(session_id);
        route.incarnation_id = "00000000-0000-4000-8000-0000000000c2".to_owned();
        let generation_shard = route_hash(session_id) % coordinator.route_generations.len();
        let stale_miss_generation = coordinator.route_generations[generation_shard]
            .load(std::sync::atomic::Ordering::Acquire);
        coordinator.cache_route(route.clone()).await;
        coordinator.routes.lock().await.remove(session_id);
        assert!(
            coordinator
                .cache_queried_route_result(session_id, None, stale_miss_generation)
                .await
                .is_none(),
            "a stale in-flight miss must remain suppressed after cache eviction"
        );
        assert!(
            !coordinator.routes.lock().await.contains_key(session_id),
            "the stale miss must not be published after activation"
        );
        coordinator.cache_route(route.clone()).await;
        assert_eq!(
            coordinator
                .route(session_id)
                .await
                .expect("activated route")
                .map(|route| route.incarnation_id),
            Some("00000000-0000-4000-8000-0000000000c2".to_owned()),
            "activation must replace an earlier cached miss immediately"
        );

        let stale_active = route.clone();
        let stale_active_generation = coordinator.route_generations[generation_shard]
            .load(std::sync::atomic::Ordering::Acquire);
        route.state = "ended".to_owned();
        route.lease_expires_at_ms = unix_ms();
        coordinator.cache_route(route.clone()).await;
        coordinator.routes.lock().await.remove(session_id);
        assert!(
            coordinator
                .cache_queried_route_result(
                    session_id,
                    Some(stale_active),
                    stale_active_generation,
                )
                .await
                .is_none(),
            "an active read begun before fencing must stay suppressed after eviction"
        );
        assert!(
            !coordinator.routes.lock().await.contains_key(session_id),
            "the stale active route must not be republished after fencing"
        );
        let terminal_owner = route.owner_node_id.clone();
        coordinator.cache_route(route).await;
        assert!(
            coordinator
                .route(session_id)
                .await
                .expect("terminal route")
                .is_none(),
            "terminal routes must be negative cache entries, never authorizers"
        );
        assert!(matches!(
            coordinator
                .route_resolution_before(
                    session_id,
                    &terminal_owner,
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect("typed terminal route"),
            DurableRouteResolution::Terminal(_)
        ));

        let normalized_id = "00000000-0000-4000-8000-0000000000c4";
        let active_returned_from_end = media_route(normalized_id);
        coordinator
            .cache_terminal_route(active_returned_from_end)
            .await;
        assert!(coordinator
            .route(normalized_id)
            .await
            .expect("normalized terminal cache")
            .is_none());
        assert!(matches!(
            coordinator
                .route_resolution_before(
                    normalized_id,
                    "node-c",
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect("normalized terminal resolution"),
            DurableRouteResolution::Terminal(_)
        ));

        let unproved_id = "00000000-0000-4000-8000-0000000000c5";
        assert!(coordinator
            .complete_release_with_route(media_route(unproved_id))
            .await
            .is_none());
        assert!(
            coordinator
                .route(unproved_id)
                .await
                .expect("unproved route lookup")
                .is_none(),
            "a nonterminal End result cannot mint a durable proof or cache authority"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn release_reconciliation_elects_once_and_retries_with_one_fence() {
        use plurx_core::cluster::membership::MembershipManager;
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let coordinator = MediaSessionCoordinator::new(MembershipManager::unavailable(), store);
        let session_id = "00000000-0000-4000-8000-0000000000d1";

        let winner = match coordinator.begin_release_reconciliation(session_id).await {
            ReleaseAdmission::Won(settlement) => settlement,
            ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("first release wins"),
        };
        let follower = match coordinator.begin_release_reconciliation(session_id).await {
            ReleaseAdmission::Joined(settlement) => settlement,
            ReleaseAdmission::Won(_) | ReleaseAdmission::Full => {
                panic!("a duplicate DELETE must join the existing settlement")
            }
        };
        assert!(Arc::ptr_eq(&winner, &follower));
        assert!(coordinator.route(session_id).await.is_err());

        coordinator
            .defer_release_reconciliation(session_id, &winner)
            .await;
        assert!(coordinator
            .take_release_reconciliation_candidates()
            .await
            .is_empty());
        tokio::time::advance(RELEASE_RECONCILIATION_RETRY_BACKOFF).await;
        let candidates = coordinator.take_release_reconciliation_candidates().await;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, session_id);
        assert!(Arc::ptr_eq(&candidates[0].1, &winner));
        assert!(
            coordinator
                .take_release_reconciliation_candidates()
                .await
                .is_empty(),
            "an in-flight retry cannot be selected twice"
        );

        coordinator.complete_release_absent(session_id).await;
        assert!(matches!(
            coordinator
                .route_resolution_before(
                    session_id,
                    "local",
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect("definitive release result"),
            DurableRouteResolution::Absent
        ));
        assert!(matches!(
            coordinator.begin_release_reconciliation(session_id).await,
            ReleaseAdmission::Joined(_)
        ));
        coordinator
            .complete_release_settlement(session_id, &winner, StatusCode::NO_CONTENT)
            .await;
        assert_eq!(winner.wait().await, StatusCode::NO_CONTENT);
        assert_eq!(follower.wait().await, StatusCode::NO_CONTENT);
        let second = match coordinator.begin_release_reconciliation(session_id).await {
            ReleaseAdmission::Won(settlement) => settlement,
            ReleaseAdmission::Joined(_) | ReleaseAdmission::Full => panic!("second release wins"),
        };
        coordinator.complete_release_absent(session_id).await;
        coordinator
            .complete_release_settlement(session_id, &second, StatusCode::NO_CONTENT)
            .await;
    }

    #[test]
    fn expired_active_route_is_retryable_owner_transition_not_terminal() {
        let session_id = "00000000-0000-4000-8000-0000000000c3";
        let mut route = media_route(session_id);
        route.lease_expires_at_ms = 9_999;
        assert!(matches!(
            classify_durable_route(Some(route.clone()), &route.owner_node_id, 10_000),
            DurableRouteResolution::OwnerTransition(exact)
                if exact.incarnation_id == route.incarnation_id
        ));

        route.state = "ended".to_owned();
        assert!(matches!(
            classify_durable_route(Some(route), "test-node", 10_000),
            DurableRouteResolution::Terminal(_)
        ));
    }

    #[tokio::test]
    async fn relay_forwards_only_media_headers_without_buffering_the_body() {
        let app = Router::new().route(
            "/media",
            get(|| async {
                // Consume an earlier request/preparation budget before the
                // peer publishes headers. Body streaming must start a new,
                // bounded lifecycle after this point.
                tokio::time::sleep(Duration::from_millis(25)).await;
                let chunks =
                    stream::once(async { Ok::<Bytes, Infallible>(Bytes::from_static(b"first")) })
                        .chain(stream::pending::<Result<Bytes, Infallible>>());
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, "video/mp2t")
                    .header(header::CACHE_CONTROL, "private, max-age=1")
                    .header(header::CONTENT_RANGE, "bytes 0-4/10")
                    .header(header::CONTENT_DISPOSITION, "inline; filename=segment.ts")
                    .header(header::SET_COOKIE, "peer-secret=must-not-leak")
                    .header(header::CONNECTION, "close")
                    .header("x-peer-internal", "must-not-leak")
                    .body(Body::from_stream(chunks))
                    .expect("fixture response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind relay fixture");
        let address = listener.local_addr().expect("relay fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve relay fixture");
        });
        let preparation_deadline = tokio::time::Instant::now() + Duration::from_millis(5);
        let peer = reqwest::Client::new()
            .get(format!("http://{address}/media"))
            .send()
            .await
            .expect("request peer stream");
        assert!(
            tokio::time::Instant::now() >= preparation_deadline,
            "the fixture consumed the pre-header preparation budget"
        );

        let relayed =
            relay_response_with_limits(peer, Duration::from_millis(250), Duration::from_millis(75))
                .expect("build streamed relay");
        assert_eq!(relayed.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            relayed.headers().get(header::CONTENT_TYPE),
            Some(&"video/mp2t".parse().expect("content type"))
        );
        assert!(relayed.headers().get(header::CONTENT_RANGE).is_some());
        assert!(relayed.headers().get(header::CONTENT_DISPOSITION).is_some());
        assert!(relayed.headers().get(header::SET_COOKIE).is_none());
        assert!(relayed.headers().get(header::CONNECTION).is_none());
        assert!(relayed.headers().get("x-peer-internal").is_none());

        let mut stream = relayed.into_body().into_data_stream();
        assert_eq!(
            stream
                .next()
                .await
                .expect("first relayed chunk")
                .expect("first relayed bytes"),
            Bytes::from_static(b"first"),
            "relay_response forwards the first peer chunk without buffering"
        );
        let error = tokio::time::timeout(Duration::from_millis(150), stream.next())
            .await
            .expect("the body adapter must settle by its no-progress deadline")
            .expect("deadline body error")
            .expect_err("a peer stalled after one chunk must fail the relayed body");
        assert!(error.to_string().contains("no-progress deadline"));
        server.abort();
    }

    #[tokio::test]
    async fn relay_total_lifetime_fires_while_body_is_unpolled_and_backpressured() {
        let app = Router::new().route(
            "/media",
            get(|| async {
                let chunks = stream::iter(
                    (0..32).map(|_| Ok::<Bytes, Infallible>(Bytes::from_static(b"chunk"))),
                )
                .chain(stream::pending::<Result<Bytes, Infallible>>());
                Response::builder()
                    .body(Body::from_stream(chunks))
                    .expect("relay backpressure response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind relay backpressure fixture");
        let address = listener.local_addr().expect("relay fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve relay backpressure fixture");
        });
        let peer = reqwest::Client::new()
            .get(format!("http://{address}/media"))
            .send()
            .await
            .expect("request peer stream");
        let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
        let upstream_pulls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let relayed = relay_response_with_limits_observed(
            peer,
            Duration::from_millis(75),
            Duration::from_secs(5),
            Some(settled_tx),
            Some(Arc::clone(&upstream_pulls)),
        )
        .expect("build independently pumped relay");

        // Do not poll the body. The bounded channel fills, but the pump's
        // absolute timer remains independently scheduled and must settle.
        tokio::time::timeout(Duration::from_millis(250), settled_rx)
            .await
            .expect("unpolled relay pump obeys total lifetime")
            .expect("relay pump completion signal");
        let pulls = upstream_pulls.load(std::sync::atomic::Ordering::Acquire);
        assert!(pulls > 0, "the fixture must exercise the upstream reader");
        assert!(
            pulls <= RELAY_BODY_CHANNEL_CAPACITY,
            "the relay must reserve channel capacity before each upstream pull: {pulls}"
        );
        let mut body = relayed.into_body().into_data_stream();
        let mut chunks = 0_usize;
        let error = loop {
            match body.next().await {
                Some(Ok(_)) => chunks += 1,
                Some(Err(error)) => break error,
                None => panic!("total deadline must remain visible after buffered chunks"),
            }
        };
        assert!(chunks <= RELAY_BODY_CHANNEL_CAPACITY);
        assert!(error.to_string().contains("total lifetime"));
        server.abort();
    }

    #[tokio::test]
    async fn dropping_relay_body_cancels_its_network_pump() {
        let app = Router::new().route(
            "/media",
            get(|| async {
                Response::builder()
                    .body(Body::from_stream(stream::pending::<
                        Result<Bytes, Infallible>,
                    >()))
                    .expect("relay cancellation response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind relay cancellation fixture");
        let address = listener.local_addr().expect("relay fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve relay cancellation fixture");
        });
        let peer = reqwest::Client::new()
            .get(format!("http://{address}/media"))
            .send()
            .await
            .expect("request peer stream");
        let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
        let relayed = relay_response_with_limits_observed(
            peer,
            Duration::from_secs(30),
            Duration::from_secs(30),
            Some(settled_tx),
            None,
        )
        .expect("build cancellable relay");
        drop(relayed);
        tokio::time::timeout(Duration::from_millis(250), settled_rx)
            .await
            .expect("body Drop cancels the sole network owner")
            .expect("relay pump completion signal");
        server.abort();
    }

    #[tokio::test]
    async fn terminal_control_relay_accepts_and_replays_the_exact_ended_response() {
        let request = terminal_relay_request();
        assert!(request.is_valid());
        let expected = terminal_relay_response(&request);
        let body = serde_json::to_vec(&expected).expect("terminal relay response");

        for disposition in ["accepted", "replayed"] {
            let response = validated_control_relay_response(
                PeerResponse {
                    status: reqwest::StatusCode::OK,
                    body: body.clone(),
                },
                &request,
            )
            .unwrap_or_else(|error| panic!("{disposition} terminal relay response: {error:?}"));
            assert_eq!(response.status(), StatusCode::OK);
            let response_body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("bounded relayed terminal body");
            let decoded = serde_json::from_slice::<crate::playback_control::ControlResponseV1>(
                &response_body,
            )
            .expect("decode relayed terminal body");
            assert_eq!(decoded, expected, "{disposition} relay changed the ack");
        }
    }
}
