//! Cluster placement transport and liveness for capability-authenticated HLS.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderName, Response, StatusCode};
use futures_util::{future::BoxFuture, stream, StreamExt};
use plurx_core::cluster::clock::{
    ClockDecision, ClockRefusal, ClusterClockGuard, OwnedClockAcquisitionTicket,
};
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
use crate::settings_changes::{Proved, SettingsChangeSource};
use crate::state::AppState;
use crate::transcode::{
    ClusterReplacementGuard, SessionAdoptionToken, SessionKind, SessionRequest,
    SessionTakeoverStart, StartInfo, TranscodeManager,
};

pub(crate) const START_PATH: &str = "/internal/cluster/media/sessions/start";
pub(crate) const PREPARE_PATH: &str = "/internal/cluster/media/sessions/prepare";
pub(crate) const ACTIVATE_PATH: &str = "/internal/cluster/media/sessions/activate";
pub(crate) const ABORT_PATH: &str = "/internal/cluster/media/sessions/abort";
pub(crate) const RELAY_PATH: &str = "/internal/cluster/media/sessions/relay";
pub(crate) const CONTROL_PATH: &str = "/internal/cluster/media/sessions/control";
pub(crate) const MAX_CONTROL_REQUEST_BYTES: usize = 96 * 1024;
pub(crate) const MAX_ACTIVATION_REQUEST_BYTES: usize = 128 * 1024;
pub(crate) const REMOTE_START_OWNERSHIP_HEADER: &str = "x-plurx-start-ownership";
pub(crate) const REMOTE_START_OWNERSHIP_V1: &str = "created-v1";
/// Durable marker carried by a prepared successor's persisted start response.
///
/// The preparation ledger authorizes media before commit. Commit removes that
/// ledger row, so this marker plus the playback pointer is the durable proof
/// that the same primed successor may continue serving while its control-plane
/// publication fence waits for predecessor projection.
pub(crate) const PREPARED_SUCCESSOR_PLAN_NOTE: &str = "prepared_quality_handoff";

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
/// Bytes requested from storage per read while streaming a media body.
///
/// Tokio's file reader performs one blocking-pool hop per read and caps an
/// individual read at 2 MiB. Every file-backed media body shares it: direct
/// play and ranges (`serve_file_range`), both HLS pumps, offline transfers
/// and the internal fragment-index blob endpoint.
///
/// 128 KiB, not 256 KiB, on measurement (docs/streaming/MEDIA-BODY-BUFFERS.md
/// Decision 1, §5.1.1). The two sizes were indistinguishable on direct-play
/// throughput and HLS latency, and both cut storage reads 32-64x against the
/// 4 KiB default, but `ReaderStream::with_capacity` reserves the buffer
/// eagerly and tokio sizes its blocking read to match, so the size is paid by
/// every open body whatever it carries: 256 KiB cost about 1.8 MiB per
/// concurrent large body over 4 KiB, 128 KiB about 1.1 MiB.
///
/// This is a storage-efficiency number only. It is deliberately *not* the
/// unit the HLS pump proves delivery in; see `MEDIA_BODY_ACK_GRANULARITY`.
pub(crate) const MEDIA_BODY_READ_BUFFER: usize = 128 * 1024;
/// Bytes of a media body proved delivered per downstream acknowledgement.
///
/// The HLS pump counts bytes, renews the playback lease and completes an
/// object only after the body side has acknowledged a chunk, so this constant
/// *is* the resolution of that proof: the most a response can over-credit,
/// and the smallest object for which a single body poll can look like a
/// complete delivery. Keeping it at 4 KiB bounds both at 4 KiB no matter how
/// large `MEDIA_BODY_READ_BUFFER` grows -- one storage read is split into
/// this many-byte pieces, which is a refcount bump on the buffer the read
/// already filled, not a copy. Raising the read size without this split would
/// raise the proof granularity with it and let any object at or below the
/// read size commit on one poll.
pub(crate) const MEDIA_BODY_ACK_GRANULARITY: usize = 4 * 1024;
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
const QUALITY_RATE_PER_SESSION: u32 = 32;
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

struct TakeoverClockAdmission {
    ticket: OwnedClockAcquisitionTicket,
    monotonic_expiry: tokio::time::Instant,
}

impl TakeoverClockAdmission {
    fn claim(
        &self,
        original: &MediaSessionRoute,
        next_owner_node_id: &str,
    ) -> MediaSessionTakeover {
        let now_ms = self.ticket.now_ms();
        MediaSessionTakeover {
            incarnation_id: original.incarnation_id.clone(),
            expected_owner_node_id: original.owner_node_id.clone(),
            expected_owner_epoch: original.owner_epoch,
            next_owner_node_id: next_owner_node_id.to_owned(),
            now_ms,
            lease_expires_at_ms: now_ms.saturating_add(TAKEOVER_CLAIM_LEASE_TTL_MS),
        }
    }
}

/// `owner_node_id` is the route's current owner. Its lease expiry was written
/// by its own clock, so the owner itself must be bounded: an unobserved
/// learner that coverage excuses is never contested, and every revalidation of
/// the ticket re-checks that owner.
fn acquire_takeover_clock(
    clock: &Arc<ClusterClockGuard>,
    owner_node_id: &str,
) -> Result<TakeoverClockAdmission, ClockRefusal> {
    // Capture before the serialized wall reading so this conservative local
    // lifetime cannot outlive the original fixed caller-bound claim.
    let origin = tokio::time::Instant::now();
    let ticket = clock.acquire_owned_for_owner(ClockDecision::Takeover, owner_node_id)?;
    Ok(TakeoverClockAdmission {
        ticket,
        monotonic_expiry: origin
            + Duration::from_millis(u64::try_from(TAKEOVER_CLAIM_LEASE_TTL_MS).unwrap_or_default()),
    })
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
                .staged_media_session_for_playback(&route.principal, &route.playback_id)
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
                    &route.principal,
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
                .staged_media_session_for_playback(&route.principal, &route.playback_id)
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
        // From here this hold is a teardown, not a start. Nothing is waiting on
        // it, and what it is about to await — `stop_session_until` moves the
        // guard into an inner task whose `stop_session` is unbounded — is
        // exactly the shape that wedged a player before. A later open may
        // reclaim the key instead of queueing behind it.
        replacement.mark_abandoned();
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

/// Upper bounds of `plurx_media_session_route_lock_seconds`, as nanoseconds
/// and as the `le` label they render as (C-07 M5, PLEX-FACADE-PAGING §3.5
/// item 2). One array so a bound and its label cannot drift apart.
const ROUTE_LOCK_BUCKETS: [(u64, &str); 5] = [
    (10_000, "0.00001"),
    (100_000, "0.0001"),
    (1_000_000, "0.001"),
    (10_000_000, "0.01"),
    (100_000_000, "0.1"),
];
/// Upper bounds of `plurx_media_session_route_prune_entries`: how many
/// entries one expired-entry sweep walks, up to the cache's own ceiling.
const ROUTE_PRUNE_BUCKETS: [u64; 7] =
    [0, 16, 64, 256, 1_024, 2_048, MAX_ROUTE_CACHE_ENTRIES as u64];
const ROUTE_LOOKUP_RESULTS: [&str; 3] = ["cache_hit", "single_flight_hit", "store"];
const ROUTE_LOCK_SITES: [&str; 4] = ["lookup", "insert", "queried_insert", "generation_insert"];

/// How `raw_route_before` answered one cached route lookup (an HLS media or
/// status GET, or a session DELETE, routed through `relay_if_remote`).
#[derive(Clone, Copy, Debug)]
enum RouteLookup {
    /// The first `cached_route`, before any single-flight shard is taken.
    CacheHit = 0,
    /// The second `cached_route`, after waiting on the lookup's
    /// `route_queries` shard: another lookup of the same capability did the
    /// Store read this one would otherwise have repeated.
    SingleFlightHit = 1,
    /// A `store.media_session_route` read.
    Store = 2,
}

/// The four places the route-cache map mutex is taken in shipping code.
#[derive(Clone, Copy, Debug)]
enum RouteLockSite {
    /// `cached_route`.
    Lookup = 0,
    /// `cache_route_result` (activation and terminal publication).
    Insert = 1,
    /// `cache_queried_route_result` (a Store lookup's answer).
    QueriedInsert = 2,
    /// `cache_route_if_generation` (generation-checked activation).
    GenerationInsert = 3,
}

/// Route-cache instrumentation (C-07 M5, PLEX-FACADE-PAGING §3.5). It is
/// measure-only: it records around the existing lock and sweep sites and
/// changes no constant, no lock ordering and no answer.
///
/// Held per coordinator rather than in a static, so a test's exact counts
/// are moved only by the coordinator it drives (PR #462's census lesson), and
/// rendered from atomics, so a `/metrics` scrape takes no lock a segment GET
/// can hold and reads no Store.
#[derive(Debug, Default)]
pub(crate) struct RouteCacheMetrics {
    lookups: [AtomicU64; ROUTE_LOOKUP_RESULTS.len()],
    lock_buckets: [[AtomicU64; ROUTE_LOCK_BUCKETS.len() + 1]; ROUTE_LOCK_SITES.len()],
    lock_sum_nanos: [AtomicU64; ROUTE_LOCK_SITES.len()],
    prune_buckets: [AtomicU64; ROUTE_PRUNE_BUCKETS.len() + 1],
    prune_sum: AtomicU64,
    entries: AtomicU64,
}

impl RouteCacheMetrics {
    fn lookup(&self, result: RouteLookup) {
        self.lookups[result as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn lock_waited(&self, site: RouteLockSite, waited: Duration) {
        let nanos = u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX);
        let bucket = ROUTE_LOCK_BUCKETS
            .iter()
            .position(|(bound, _)| nanos <= *bound)
            .unwrap_or(ROUTE_LOCK_BUCKETS.len());
        self.lock_buckets[site as usize][bucket].fetch_add(1, Ordering::Relaxed);
        self.lock_sum_nanos[site as usize].fetch_add(nanos, Ordering::Relaxed);
    }

    fn pruned(&self, walked: usize) {
        let walked = u64::try_from(walked).unwrap_or(u64::MAX);
        let bucket = ROUTE_PRUNE_BUCKETS
            .iter()
            .position(|bound| walked <= *bound)
            .unwrap_or(ROUTE_PRUNE_BUCKETS.len());
        self.prune_buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.prune_sum.fetch_add(walked, Ordering::Relaxed);
    }

    fn cache_entries(&self, entries: usize) {
        self.entries.store(
            u64::try_from(entries).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    #[cfg(test)]
    fn lookups(&self, result: RouteLookup) -> u64 {
        self.lookups[result as usize].load(Ordering::Relaxed)
    }

    pub(crate) fn prometheus(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_media_session_route_lookups_total Media-session route lookups through the route cache (HLS media and status GETs, session DELETEs) by how they were answered: the one-second route cache, the cache re-read after waiting on the lookup's single-flight shard, or a Store read. Control and final-status reads bypass the cache and are not counted here.\n\
             # TYPE plurx_media_session_route_lookups_total counter\n",
        );
        for (index, result) in ROUTE_LOOKUP_RESULTS.iter().enumerate() {
            out.push_str(&format!(
                "plurx_media_session_route_lookups_total{{result=\"{result}\"}} {}\n",
                self.lookups[index].load(Ordering::Relaxed)
            ));
        }
        out.push_str(
            "# HELP plurx_media_session_route_lock_seconds Wait to acquire the route-cache map lock, by call site.\n\
             # TYPE plurx_media_session_route_lock_seconds histogram\n",
        );
        for (site_index, site) in ROUTE_LOCK_SITES.iter().enumerate() {
            let mut cumulative = 0_u64;
            for (bucket_index, (_, label)) in ROUTE_LOCK_BUCKETS.iter().enumerate() {
                cumulative = cumulative.saturating_add(
                    self.lock_buckets[site_index][bucket_index].load(Ordering::Relaxed),
                );
                out.push_str(&format!(
                    "plurx_media_session_route_lock_seconds_bucket{{site=\"{site}\",le=\"{label}\"}} {cumulative}\n"
                ));
            }
            cumulative = cumulative.saturating_add(
                self.lock_buckets[site_index][ROUTE_LOCK_BUCKETS.len()].load(Ordering::Relaxed),
            );
            let sum_nanos = self.lock_sum_nanos[site_index].load(Ordering::Relaxed);
            out.push_str(&format!(
                "plurx_media_session_route_lock_seconds_bucket{{site=\"{site}\",le=\"+Inf\"}} {cumulative}\n\
                 plurx_media_session_route_lock_seconds_sum{{site=\"{site}\"}} {}.{:09}\n\
                 plurx_media_session_route_lock_seconds_count{{site=\"{site}\"}} {cumulative}\n",
                sum_nanos / 1_000_000_000,
                sum_nanos % 1_000_000_000,
            ));
        }
        out.push_str(
            "# HELP plurx_media_session_route_prune_entries Entries walked by one expired-entry sweep of the route cache; the sweep runs under the map lock on every insert.\n\
             # TYPE plurx_media_session_route_prune_entries histogram\n",
        );
        let mut cumulative = 0_u64;
        for (bucket_index, bound) in ROUTE_PRUNE_BUCKETS.iter().enumerate() {
            cumulative =
                cumulative.saturating_add(self.prune_buckets[bucket_index].load(Ordering::Relaxed));
            out.push_str(&format!(
                "plurx_media_session_route_prune_entries_bucket{{le=\"{bound}\"}} {cumulative}\n"
            ));
        }
        cumulative = cumulative
            .saturating_add(self.prune_buckets[ROUTE_PRUNE_BUCKETS.len()].load(Ordering::Relaxed));
        out.push_str(&format!(
            "plurx_media_session_route_prune_entries_bucket{{le=\"+Inf\"}} {cumulative}\n\
             plurx_media_session_route_prune_entries_sum {}\n\
             plurx_media_session_route_prune_entries_count {cumulative}\n\
             # HELP plurx_media_session_route_cache_entries Entries in the route cache after its last change; the cache holds at most {MAX_ROUTE_CACHE_ENTRIES}.\n\
             # TYPE plurx_media_session_route_cache_entries gauge\n\
             plurx_media_session_route_cache_entries {}\n",
            self.prune_sum.load(Ordering::Relaxed),
            self.entries.load(Ordering::Relaxed),
        ));
        out
    }
}

/// Times one wait for the route-cache map lock. Recorded on drop, so a wait
/// cut short by a caller's `timeout_at` is counted too, up to the moment it
/// was abandoned.
struct RouteLockWait<'a> {
    metrics: &'a RouteCacheMetrics,
    site: RouteLockSite,
    started: Instant,
}

impl Drop for RouteLockWait<'_> {
    fn drop(&mut self) {
        self.metrics.lock_waited(self.site, self.started.elapsed());
    }
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
pub(crate) struct CandidateCatalogContext {
    pub caps: plurx_core::playback::DeviceCaps,
    pub candidate: plurx_core::playback::candidate::QualityCandidate,
    pub binding: crate::media_pool::PlanningBinding,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RemoteStartRequestWire", into = "RemoteStartRequestWire")]
pub(crate) struct RemoteStartRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_output_receiver: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_output: Option<crate::transcode::RetainedOutputFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_catalog: Option<CandidateCatalogContext>,
    /// Retained route context. Tolerated by the parser floor, never minted by
    /// it and never sufficient to authorize a worker route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<plurx_core::playback::candidate::CandidateId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_target: Option<plurx_core::playback::candidate::PresentationTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoder_caps: Option<crate::playback_control::DecoderCapsSnapshot>,
    pub protocol_version: i64,
    pub incarnation_id: String,
    pub principal: plurx_core::playback_principal::PlaybackPrincipal,
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
    /// Structured following purpose delivered to and retained by every owner.
    /// This is in the worker envelope, not bolted onto the public response
    /// after placement, so takeover cannot silently turn a channel play into
    /// ordinary VOD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_channel: Option<serde_json::Value>,
    pub request: SessionRequest,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteStartRequestWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_output_receiver: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_output: Option<crate::transcode::RetainedOutputFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_catalog: Option<CandidateCatalogContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<plurx_core::playback::candidate::CandidateId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_target: Option<plurx_core::playback::candidate::PresentationTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoder_caps: Option<crate::playback_control::DecoderCapsSnapshot>,
    pub protocol_version: i64,
    pub incarnation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<plurx_core::playback_principal::PlaybackPrincipal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    pub source_size: i64,
    pub source_mtime: i64,
    #[serde(default)]
    pub typeless_playlist: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_channel: Option<serde_json::Value>,
    pub request: SessionRequest,
}
impl TryFrom<RemoteStartRequestWire> for RemoteStartRequest {
    type Error = &'static str;
    fn try_from(wire: RemoteStartRequestWire) -> Result<Self, Self::Error> {
        let principal = remote_wire_principal(wire.principal, wire.user_id)?;
        Ok(Self {
            principal,
            retained_output_receiver: wire.retained_output_receiver,
            retained_output: wire.retained_output,
            candidate_catalog: wire.candidate_catalog,
            candidate_id: wire.candidate_id,
            presentation_target: wire.presentation_target,
            decoder_caps: wire.decoder_caps,
            protocol_version: wire.protocol_version,
            incarnation_id: wire.incarnation_id,
            source_size: wire.source_size,
            source_mtime: wire.source_mtime,
            typeless_playlist: wire.typeless_playlist,
            library_channel: wire.library_channel,
            request: wire.request,
        })
    }
}
impl From<RemoteStartRequest> for RemoteStartRequestWire {
    fn from(request: RemoteStartRequest) -> Self {
        let (principal, user_id) = match request.principal {
            plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id } => {
                (None, Some(user_id))
            }
            principal => (Some(principal), None),
        };
        Self {
            principal,
            user_id,
            retained_output_receiver: request.retained_output_receiver,
            retained_output: request.retained_output,
            candidate_catalog: request.candidate_catalog,
            candidate_id: request.candidate_id,
            presentation_target: request.presentation_target,
            decoder_caps: request.decoder_caps,
            protocol_version: request.protocol_version,
            incarnation_id: request.incarnation_id,
            source_size: request.source_size,
            source_mtime: request.source_mtime,
            typeless_playlist: request.typeless_playlist,
            library_channel: request.library_channel,
            request: request.request,
        }
    }
}

/// Current ownership is typed; retained local recipes keep their old wire shape.
/// A wire envelope must choose exactly one namespace, never supply both.
fn remote_wire_principal(
    principal: Option<plurx_core::playback_principal::PlaybackPrincipal>,
    user_id: Option<i64>,
) -> Result<plurx_core::playback_principal::PlaybackPrincipal, &'static str> {
    match (principal, user_id) {
        (Some(principal), None) if principal.valid_admission_shape() => Ok(principal),
        (None, Some(user_id)) if user_id > 0 => {
            Ok(plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id })
        }
        _ => Err("invalid remote playback ownership"),
    }
}

impl RemoteStartRequest {
    pub(crate) fn is_valid(&self) -> bool {
        remote_start_envelope_is_valid(self) && worker_session_request_is_valid(&self.request)
    }
}

fn remote_start_envelope_is_valid(request: &RemoteStartRequest) -> bool {
    request.request.continuous_media.as_ref().is_none_or(|media| {
        media.autonomous_companion.is_none_or(|companion| request.candidate_id.is_some_and(|primary| primary != companion))
            && media.family_descriptor.as_ref().is_none_or(|description| {
                request.candidate_id.is_some_and(|primary| description.video.iter().any(|row| row.candidate_id == primary))
            })
    })
        && (request.request.continuous_media.is_none() || request.library_channel.is_none())
        && request.retained_output_receiver.is_none_or(|version| version == 1)
        && (request.retained_output.is_none() || request.retained_output_receiver == Some(1))
        && request.retained_output.as_ref().is_none_or(crate::transcode::RetainedOutputFacts::valid)
        && (request.candidate_id.is_none() || request.decoder_caps.is_some() && request.candidate_catalog.is_some())
        && request.candidate_catalog.as_ref().is_none_or(|context| {
            request.candidate_id == Some(context.candidate.id) && context.candidate.identity_matches()
                && context.caps.v == 2 && context.caps.video.len() <= plurx_core::playback::MAX_CLIENT_DECODER_ENTRIES
                && context.caps.validate_audio_sinks().is_ok() && context.caps.validate_progressive_hevc_sample_entries().is_ok()
                && context.binding.generation >= 0 && context.binding.source_digest.len() == 64
                && context.binding.source_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        // An explicit snapshot is a current capability constraint, including
        // on ordinary negotiated routes. Empty/all-unavailable is decoder
        // loss, not permission to fall back to legacy unconstrained dispatch.
        && request.decoder_caps.as_ref().is_none_or(|snapshot| {
            snapshot
                .video
                .iter()
                .any(|entry| entry.available && !entry.dynamic_ranges.is_empty())
        })
        && request.protocol_version == crate::media_pool::PROTOCOL_VERSION
        && uuid::Uuid::parse_str(&request.incarnation_id).is_ok()
        && request.principal.valid_admission_shape()
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

/// Source catalogue IDs are canonical nonnegative IDs. This validates only
/// request shape; a complete current Source witness must authorize admission.
pub(crate) fn source_session_request_is_valid(
    request: &SessionRequest,
    principal: &plurx_core::playback_principal::PlaybackPrincipal,
) -> bool {
    matches!(
        principal,
        plurx_core::playback_principal::PlaybackPrincipal::Sharing { .. }
    ) && principal.valid_admission_shape()
        && session_request_fields_are_valid(request, true)
        && request.presentation == crate::transcode::Presentation::Vod
}

fn worker_session_request_fields_are_valid(request: &SessionRequest) -> bool {
    session_request_fields_are_valid(request, false)
}

fn session_request_fields_are_valid(request: &SessionRequest, source_ids: bool) -> bool {
    request.dv_processing_route().is_ok() && request
        .continuous_media
        .as_ref()
        .is_none_or(|media| media.valid_for(request))
        && (!request.passive_vod
            || (request.vod_only && request.request_id.as_deref().is_some_and(|id| !id.trim().is_empty())))
        && (!request.vod_only || request.presentation == crate::transcode::Presentation::Vod)
        && request.finite_bitrate_limit_bps.is_none_or(|limit|
            request.passive_vod && request.vod_only && (64_000..=1_000_000_000).contains(&limit))
        && (request.file_id > 0 || (source_ids && request.file_id == 0))
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
        && request.audio_delivery.as_ref().is_none_or(|audio| audio.valid_snapshot())
        && request.audio_claim.as_ref().is_none_or(|claim| claim.valid_snapshot())
        && request
            .subtitle_burn
            .is_none_or(|index| (0..=1_024).contains(&index))
        && (-15_000..=15_000).contains(&request.audio_offset_ms)
        && match &request.kind {
            SessionKind::Transcode { height } => {
                let minimum = if request.dv_processing_destination.is_some() { 32 } else { crate::transcode::MIN_HEIGHT };
                (minimum..=crate::transcode::MAX_HEIGHT).contains(height)
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
        // Bounded on the peer wire too, not only at public ingress: a relayed
        // envelope reaches the durable recipe by a different door, and an
        // unbounded string there would still end up in a log line.
        && request
            .transport
            .as_deref()
            .is_none_or(crate::transcode::session_transport_is_valid)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteStartResponse {
    /// Owner answer carried separately from the strict legacy JSON body.
    #[serde(skip)]
    pub processed_dv_profile: Option<u8>,
    /// Whether the selected processing plan reconstructs the FEL. Local to
    /// the node that holds the plan: an ingress for a remote owner falls back
    /// to the owner's effective-processing report for the indicator.
    #[serde(skip)]
    pub processed_fel: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_output: Option<crate::transcode::RetainedOutputFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_delivery: Option<plurx_core::playback::audio::AudioDelivery>,
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

/// Attach a worker to a successor that the shared preparation ledger already
/// reserved for this target. The recipe stays in the durable row; the wire
/// carries only exact identity, so a peer cannot ask the target to prime bytes
/// that Store did not authorize first.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    try_from = "RemotePrepareRequestWire",
    into = "RemotePrepareRequestWire"
)]
pub(crate) struct RemotePrepareRequest {
    pub protocol_version: i64,
    pub incarnation_id: String,
    pub session_id: String,
    pub principal: plurx_core::playback_principal::PlaybackPrincipal,
    pub expected_owner_epoch: i64,
    /// The predecessor owner epoch the preparation was admitted under. The
    /// peer seals capture against exactly this epoch, so a predecessor
    /// takeover after staging fails the peer's seal instead of the origin's
    /// later one. Sent only to a peer advertising `prepared_output_capture`
    /// (the wire refuses unknown fields); absent, the peer fences on the
    /// predecessor's current epoch as before.
    pub expected_predecessor_owner_epoch: Option<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemotePrepareRequestWire {
    pub protocol_version: i64,
    pub incarnation_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<plurx_core::playback_principal::PlaybackPrincipal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    pub expected_owner_epoch: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_predecessor_owner_epoch: Option<i64>,
}
impl TryFrom<RemotePrepareRequestWire> for RemotePrepareRequest {
    type Error = &'static str;
    fn try_from(wire: RemotePrepareRequestWire) -> Result<Self, Self::Error> {
        let principal = remote_wire_principal(wire.principal, wire.user_id)?;
        Ok(Self {
            principal,
            protocol_version: wire.protocol_version,
            incarnation_id: wire.incarnation_id,
            session_id: wire.session_id,
            expected_owner_epoch: wire.expected_owner_epoch,
            expected_predecessor_owner_epoch: wire.expected_predecessor_owner_epoch,
        })
    }
}
impl From<RemotePrepareRequest> for RemotePrepareRequestWire {
    fn from(request: RemotePrepareRequest) -> Self {
        let (principal, user_id) = match request.principal {
            plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id } => {
                (None, Some(user_id))
            }
            principal => (Some(principal), None),
        };
        Self {
            principal,
            user_id,
            protocol_version: request.protocol_version,
            incarnation_id: request.incarnation_id,
            session_id: request.session_id,
            expected_owner_epoch: request.expected_owner_epoch,
            expected_predecessor_owner_epoch: request.expected_predecessor_owner_epoch,
        }
    }
}

impl RemotePrepareRequest {
    pub(crate) fn is_valid(&self) -> bool {
        self.protocol_version == crate::media_pool::PROTOCOL_VERSION
            && uuid::Uuid::parse_str(&self.incarnation_id).is_ok()
            && uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.principal.valid_admission_shape()
            && self.expected_owner_epoch == 1
            && self
                .expected_predecessor_owner_epoch
                .is_none_or(|epoch| epoch > 0)
    }
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
        .map(|mut info| {
            info.processed_dv_profile = response
                .dv_processing_header
                .as_deref()
                .and_then(|header| info.processed_dv_profile_from_header(header));
            info
        })
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
            processed_dv_profile: info.processed_dv_profile,
            processed_fel: info.processed_fel,
            retained_output: info.retained_output,
            audio_delivery: info.audio_delivery,
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
    /// Optional bounded sidecar keeps older strict START body readers valid.
    pub(crate) fn processed_dv_header(&self) -> Option<String> {
        self.processed_dv_profile
            .filter(|_| self.is_valid())
            .map(|profile| {
                serde_json::json!({
                    "session_id": self.session_id,
                    "processed_dv_profile": profile,
                })
                .to_string()
            })
    }

    fn processed_dv_profile_from_header(&self, header: &str) -> Option<u8> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            session_id: String,
            processed_dv_profile: u8,
        }
        if header.len() > 4096 {
            return None;
        }
        let wire: Wire = serde_json::from_str(header).ok()?;
        (wire.session_id == self.session_id
            && wire.processed_dv_profile == 8
            && self.vod
            && self.grade == OutputGrade::Hdr10
            && matches!(self.kind, SessionKind::Transcode { .. }))
        .then_some(wire.processed_dv_profile)
    }

    pub(crate) fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.processed_dv_profile.is_none_or(|profile| {
                profile == 8
                    && self.vod
                    && self.grade == OutputGrade::Hdr10
                    && matches!(self.kind, SessionKind::Transcode { .. })
            })
            && self
                .retained_output
                .as_ref()
                .is_none_or(|facts| self.vod && facts.valid())
            && self
                .audio_delivery
                .as_ref()
                .is_none_or(|audio| audio.valid_snapshot())
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acquisition_start_ms: Option<i64>,
    },
    Master {
        subtitle: Option<i64>,
        diagnostic: Option<String>,
    },
    VideoPlaylist,
    ContinuousFamily,
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
    ChildPlaylist {
        role: String,
        rendition: String,
    },
    ChildSegment {
        child: crate::vodserve::ChildMediaRequest,
    },
    Delete,
    /// Authenticated compatibility presence for one exact passive play.
    PassivePresence {
        user: String,
        player: String,
        request: String,
    },
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
    /// Unknown peers receive the original strict envelope. This changes only
    /// acquisition metadata, never media authority, resources, or deadlines.
    pub(crate) fn for_acquisition_peer(&self, supported: bool) -> Self {
        let mut resource = self.clone();
        if let Self::Playlist {
            acquisition_start_ms,
            ..
        } = &mut resource
        {
            if !supported {
                *acquisition_start_ms = None;
            }
        }
        resource
    }

    pub(crate) fn is_valid(&self) -> bool {
        match self {
            Self::Status | Self::VideoPlaylist | Self::ContinuousFamily | Self::Delete => true,
            Self::Playlist {
                native, subtitle, ..
            } => {
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
            Self::ChildSegment { child } => child.is_valid(),
            Self::ChildPlaylist { role, rendition } => {
                crate::vodserve::ChildMediaRequest::valid_identity(role, rendition)
            }
            Self::PassivePresence {
                user,
                player,
                request,
            } => [user, player, request].iter().all(|value| {
                !value.trim().is_empty()
                    && value.len() <= 256
                    && !value.chars().any(char::is_control)
            }),
        }
    }

    fn max_lifetime(&self) -> Duration {
        match self {
            Self::Playlist { .. }
            | Self::Master { .. }
            | Self::VideoPlaylist
            | Self::ContinuousFamily
            | Self::SubtitlePlaylist { .. }
            | Self::ChildPlaylist { .. } => RELAY_PLAYLIST_MAX_LIFETIME,
            Self::Segment { .. } | Self::ChildSegment { .. } => RELAY_SEGMENT_MAX_LIFETIME,
            Self::Status
            | Self::SubtitleSegment { .. }
            | Self::Delete
            | Self::PassivePresence { .. } => RELAY_SHORT_MAX_LIFETIME,
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
    quality_admission: Arc<StdMutex<ControlAdmission>>,
    route_cache_metrics: Arc<RouteCacheMetrics>,
    /// Test-only: while set, a lookup that reaches the Store read waits here
    /// until the semaphore is closed, so a test can hold one read open and
    /// observe what concurrent lookups of the same capability do meanwhile.
    #[cfg(test)]
    route_store_gate: Arc<StdMutex<Option<Arc<tokio::sync::Semaphore>>>>,
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
        self.admit_with_limit(now, session_id, CONTROL_RATE_PER_SESSION)
    }

    fn admit_with_limit(
        &mut self,
        now: Instant,
        session_id: &str,
        per_session: u32,
    ) -> Result<(), u32> {
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
        if entry.admitted >= per_session {
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
            quality_admission: Arc::new(StdMutex::new(ControlAdmission::default())),
            route_cache_metrics: Arc::new(RouteCacheMetrics::default()),
            #[cfg(test)]
            route_store_gate: Arc::new(StdMutex::new(None)),
        })
    }

    /// The route-cache instrumentation `/metrics` renders (C-07 M5).
    pub(crate) fn route_cache_metrics(&self) -> Arc<RouteCacheMetrics> {
        Arc::clone(&self.route_cache_metrics)
    }

    /// Take the route-cache map lock, recording how long it took to get.
    /// The lock taken, and where, is exactly what the caller took before.
    async fn lock_routes(
        &self,
        site: RouteLockSite,
    ) -> tokio::sync::MutexGuard<'_, HashMap<String, CachedRoute>> {
        let wait = RouteLockWait {
            metrics: &self.route_cache_metrics,
            site,
            started: Instant::now(),
        };
        let routes = self.routes.lock().await;
        drop(wait);
        routes
    }

    #[cfg(test)]
    async fn wait_route_store_gate(&self) {
        let gate = self
            .route_store_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(gate) = gate {
            let _ = gate.acquire().await;
        }
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

    /// Fragment reservations and completed facts have a separate bounded
    /// budget: prebuffering must not consume the manual intent/lease budget.
    pub(crate) fn admit_quality_schedule(&self, session_id: &str) -> Result<(), u32> {
        self.quality_admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admit_with_limit(Instant::now(), session_id, QUALITY_RATE_PER_SESSION)
    }

    /// Schedule admissions charged in the current node-wide window.
    #[cfg(test)]
    pub(crate) fn quality_schedule_admissions_in_window(&self) -> u32 {
        self.quality_admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admitted
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

    /// Resolve a prepared media object while its control publication remains
    /// fenced. Before commit the live preparation ledger is the authority;
    /// after commit the prepared-response marker plus the playback pointer is.
    /// Status uses the same exact preparation authority as media so a client
    /// can inspect the successor it is already decoding. Mutating control,
    /// Delete and passive presence retain `route_resolution_before` until
    /// predecessor projection finishes.
    pub(crate) async fn media_route_resolution_before(
        &self,
        session_id: &str,
        local_node_id: &str,
        request_deadline: Instant,
    ) -> Result<DurableRouteResolution, StoreError> {
        let resolution = self
            .route_resolution_before(session_id, local_node_id, request_deadline)
            .await?;
        self.authorize_prepared_media_before(resolution, local_node_id, request_deadline)
            .await
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

    /// Cache-bypassing form of [`Self::media_route_resolution_before`].
    pub(crate) async fn authoritative_media_route_resolution_before(
        &self,
        session_id: &str,
        local_node_id: &str,
        request_deadline: Instant,
    ) -> Result<DurableRouteResolution, StoreError> {
        let resolution = self
            .authoritative_route_resolution_before(session_id, local_node_id, request_deadline)
            .await?;
        self.authorize_prepared_media_before(resolution, local_node_id, request_deadline)
            .await
    }

    async fn authorize_prepared_media_before(
        &self,
        resolution: DurableRouteResolution,
        local_node_id: &str,
        request_deadline: Instant,
    ) -> Result<DurableRouteResolution, StoreError> {
        let DurableRouteResolution::OwnerTransition(route) = &resolution else {
            return Ok(resolution);
        };
        let now_ms = unix_ms();
        if route.state != "active"
            || route.publication_ready_at_ms == 0
            || route.lease_expires_at_ms <= now_ms
        {
            return Ok(resolution);
        }
        let staged = if route.publication_ready_at_ms == MEDIA_SESSION_PUBLICATION_BLOCKED {
            tokio::time::timeout_at(
                tokio::time::Instant::from_std(request_deadline),
                self.store
                    .staged_media_session_for_playback(&route.principal, &route.playback_id),
            )
            .await
            .map_err(|_| {
                StoreError::Database("staged media authorization timed out".to_owned())
            })??
        } else {
            None
        };
        let staged_authorized = staged.as_ref().is_some_and(|staged| {
            staged.staged_incarnation_id == route.incarnation_id && staged.deadline_ms > now_ms
        });
        let committed_authorized = if !staged_authorized && prepared_successor_route(route) {
            let current = tokio::time::timeout_at(
                tokio::time::Instant::from_std(request_deadline),
                self.store
                    .media_session_route_for_playback(&route.principal, &route.playback_id),
            )
            .await
            .map_err(|_| {
                StoreError::Database("prepared media authorization timed out".to_owned())
            })??;
            current.is_some_and(|current| same_media_route(&current, route))
        } else {
            false
        };
        Ok(classify_prepared_media_route(
            resolution,
            staged_authorized || committed_authorized,
            local_node_id,
            now_ms,
        ))
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
            self.route_cache_metrics.lookup(RouteLookup::CacheHit);
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
            self.route_cache_metrics
                .lookup(RouteLookup::SingleFlightHit);
            self.reject_pending_release(session_id).await?;
            return Ok(cached);
        }
        let observed_generation =
            self.route_generations[generation_shard].load(std::sync::atomic::Ordering::Acquire);
        self.route_cache_metrics.lookup(RouteLookup::Store);
        #[cfg(test)]
        self.wait_route_store_gate().await;
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
            .try_update(
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
        let mut routes = self.lock_routes(RouteLockSite::GenerationInsert).await;
        let walked = routes.len();
        routes.retain(|_, cached| cached.expires_at > now);
        self.route_cache_metrics.pruned(walked);
        self.route_cache_metrics.cache_entries(routes.len());
        let shard = route_hash(&session_id) % self.route_generations.len();
        if self.route_generations[shard].load(std::sync::atomic::Ordering::Acquire)
            != observed_generation
        {
            return false;
        }
        self.route_generations[shard].fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        crate::telemetry::diagnostics(Arc::clone(&self.store)).route(&route);
        insert_cached_route(&mut routes, &session_id, Some(route), now);
        self.route_cache_metrics.cache_entries(routes.len());
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
        let mut routes = self.lock_routes(RouteLockSite::Lookup).await;
        let cached = routes.get(session_id)?;
        if cached.expires_at > now {
            return Some(cached.route.clone());
        }
        routes.remove(session_id);
        self.route_cache_metrics.cache_entries(routes.len());
        None
    }

    async fn cache_route_result(&self, session_id: &str, route: Option<MediaSessionRoute>) {
        if let Some(route) = &route {
            crate::telemetry::diagnostics(Arc::clone(&self.store)).route(route);
        }
        let now = tokio::time::Instant::now();
        let mut routes = self.lock_routes(RouteLockSite::Insert).await;
        let walked = routes.len();
        routes.retain(|_, cached| cached.expires_at > now);
        self.route_cache_metrics.pruned(walked);
        let generation_shard = route_hash(session_id) % self.route_generations.len();
        self.route_generations[generation_shard].fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        insert_cached_route(&mut routes, session_id, route, now);
        self.route_cache_metrics.cache_entries(routes.len());
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
        let mut routes = self.lock_routes(RouteLockSite::QueriedInsert).await;
        let walked = routes.len();
        routes.retain(|_, cached| cached.expires_at > now);
        self.route_cache_metrics.pruned(walked);
        self.route_cache_metrics.cache_entries(routes.len());
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
        if let Some(route) = &route {
            crate::telemetry::diagnostics(Arc::clone(&self.store)).route(route);
        }
        insert_cached_route(&mut routes, session_id, route.clone(), now);
        self.route_cache_metrics.cache_entries(routes.len());
        route
    }

    /// Hand a freshly activated local worker to the renewal loop before its
    /// next store inventory. If the store disappears in that exact window,
    /// the worker still knows the committed expiry and self-fences on time.
    ///
    /// A prepared successor enters here immediately after its durable commit,
    /// while its publication fence is still set. That route is active and no
    /// longer has a preparation-ledger row, so it must renew while predecessor
    /// projection completes. Callers never seed an uncommitted preparation.
    pub(crate) async fn seed_owned_lease(&self, route: &MediaSessionRoute) {
        if route.state != "active" {
            debug_assert!(
                false,
                "only active media-session routes may enter lease accounting"
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

    /// Shared receiver bytes retain the existing authenticated streaming driver.
    pub(crate) async fn receiver_forward_stream(
        &self,
        owner_node_id: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
    ) -> Result<reqwest::Response, PeerTransportError> {
        if body.len() > 128 * 1024 || tokio::time::Instant::now() >= deadline {
            return Err(PeerTransportError::InvalidResponse);
        }
        let base = self.peer_base(owner_node_id, deadline).await?;
        self.transport
            .request_stream(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                "/internal/cluster/sharing/receiver/forward",
                body,
                deadline,
                PeerAuthMode::ExactRequest,
            )
            .await
    }

    /// Only the named receiver control/custody operations use signed replies.
    pub(crate) async fn receiver_forward_bounded(
        &self,
        owner_node_id: &str,
        path: &'static str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
    ) -> Result<crate::http::peer_transport::PeerResponse, PeerTransportError> {
        if !matches!(
            path,
            "/internal/cluster/sharing/receiver/control"
                | "/internal/cluster/sharing/receiver/register"
                | "/internal/cluster/sharing/receiver/ack"
        ) || body.len() > 128 * 1024
            || tokio::time::Instant::now() >= deadline
        {
            return Err(PeerTransportError::InvalidResponse);
        }
        let base = self.peer_base(owner_node_id, deadline).await?;
        self.transport
            .request(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                path,
                body,
                deadline,
                64 * 1024,
                PeerAuthMode::ExactRequestAndMemberResponse,
            )
            .await
    }

    /// Source body relay: keep byte backpressure and accepted-writer custody.
    pub(crate) async fn source_forward_stream(
        &self,
        owner_node_id: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
    ) -> Result<reqwest::Response, PeerTransportError> {
        if body.len() > 1024 * 1024 {
            return Err(PeerTransportError::InvalidResponse);
        }
        let base = self.peer_base(owner_node_id, deadline).await?;
        self.transport
            .request_stream(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                "/internal/cluster/sharing/source/forward",
                body,
                deadline,
                crate::http::peer_transport::PeerAuthMode::ExactRequest,
            )
            .await
    }

    /// Bounded Source control and custody answers authenticate status/body too.
    pub(crate) async fn source_forward_bounded(
        &self,
        owner_node_id: &str,
        path: &'static str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
    ) -> Result<crate::http::peer_transport::PeerResponse, PeerTransportError> {
        if body.len() > 1024 * 1024
            || !matches!(
                path,
                "/internal/cluster/sharing/source/locate"
                    | "/internal/cluster/sharing/source/prepare"
                    | "/internal/cluster/sharing/source/control"
                    | "/internal/cluster/sharing/source/register"
                    | "/internal/cluster/sharing/source/ack"
            )
        {
            return Err(PeerTransportError::InvalidResponse);
        }
        let base = self.peer_base(owner_node_id, deadline).await?;
        self.transport
            .request(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                path,
                body,
                deadline,
                64 * 1024,
                crate::http::peer_transport::PeerAuthMode::ExactRequestAndMemberResponse,
            )
            .await
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
        let started = decode_remote_start_response(response)?;
        if request.retained_output_receiver != Some(1) && started.info.retained_output.is_some() {
            return Err(PeerTransportError::InvalidResponse);
        }
        Ok(started)
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

    pub(crate) async fn prepare_remote(
        &self,
        owner_node_id: &str,
        request: &RemotePrepareRequest,
        deadline: tokio::time::Instant,
    ) -> Result<(), PeerTransportError> {
        let body = serde_json::to_vec(request).map_err(|_| PeerTransportError::InvalidResponse)?;
        if body.len() > MAX_CONTROL_REQUEST_BYTES || tokio::time::Instant::now() >= deadline {
            return Err(PeerTransportError::InvalidResponse);
        }
        let base = self.peer_base(owner_node_id, deadline).await?;
        let response = self
            .transport
            .request(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                PREPARE_PATH,
                body,
                deadline,
                4 * 1024,
                PeerAuthMode::ExactRequest,
            )
            .await?;
        if response.status == reqwest::StatusCode::NO_CONTENT {
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

    /// Exact authenticated closure of an outer accepted driver. No request
    /// timeout or missing registry entry is interpreted as physical closure.
    pub(crate) async fn close_sharing_ingress(
        &self,
        ingress_node: &str,
        request: &crate::sharing_connection_custody::DriverCloseRequest,
    ) -> Result<crate::sharing_connection_custody::DriverClosureReceipt, PeerTransportError> {
        use crate::sharing_connection_custody::{DriverClosureReceipt, CLOSE_PATH};
        let remaining = request.deadline_unix_ms.saturating_sub(unix_ms());
        if !request.driver.valid()
            || !(1..=9_007_199_254_740_991).contains(&request.registration_sequence)
            || request.expected_owner_epoch <= 0
            || !matches!(request.principal_kind.as_str(), "source" | "receiver")
            || request.incarnation_id.is_nil()
            || !crate::sharing_connection_custody::owner_identity_valid(&request.owner_identity)
            || !(1..=315_000).contains(&remaining)
        {
            return Err(PeerTransportError::InvalidResponse);
        }
        let deadline = deadline_after(Duration::from_millis(
            u64::try_from(remaining).map_err(|_| PeerTransportError::InvalidResponse)?,
        ));
        let base = self.peer_base(ingress_node, deadline).await?;
        let body = serde_json::to_vec(request).map_err(|_| PeerTransportError::InvalidResponse)?;
        let response = self
            .transport
            .request(
                ingress_node,
                &base,
                reqwest::Method::POST,
                CLOSE_PATH,
                body,
                deadline,
                2048,
                PeerAuthMode::ExactRequestAndMemberResponse,
            )
            .await?;
        if !response.status.is_success() {
            return Err(PeerTransportError::InvalidResponse);
        }
        let receipt: DriverClosureReceipt = serde_json::from_slice(&response.body)
            .map_err(|_| PeerTransportError::InvalidResponse)?;
        if !receipt.matches(&request.driver) {
            return Err(PeerTransportError::InvalidResponse);
        }
        Ok(receipt)
    }

    pub(crate) async fn quality_schedule(
        &self,
        owner_node_id: &str,
        request: &crate::http::hls::QualityScheduleRelayRequest,
    ) -> Result<Response<Body>, PeerTransportError> {
        let body = serde_json::to_vec(request).map_err(|_| PeerTransportError::InvalidResponse)?;
        if body.len() > crate::http::hls::QUALITY_SCHEDULE_MAX_BYTES {
            return Err(PeerTransportError::InvalidResponse);
        }
        let remaining = request
            .deadline_unix_ms
            .saturating_sub(unix_ms())
            .min(12_000);
        if remaining <= 0 {
            return Err(PeerTransportError::TimedOut);
        }
        let deadline = deadline_after(Duration::from_millis(remaining as u64));
        let base = self.peer_base(owner_node_id, deadline).await?;
        let (response, retry_after) = self
            .transport
            .request_with_retry_after(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                crate::http::hls::QUALITY_SCHEDULE_PATH,
                body,
                deadline,
                crate::http::hls::QUALITY_SCHEDULE_MAX_RESPONSE_BYTES,
                PeerAuthMode::ExactRequest,
            )
            .await?;
        if response.status.is_success() {
            serde_json::from_slice::<crate::vodserve::QualityScheduleResponse>(&response.body)
                .ok()
                .filter(|reply| reply.valid_for(&request.request))
                .ok_or(PeerTransportError::InvalidResponse)?;
        }
        relayed_owner_response(response.status.as_u16(), retry_after, response.body)
    }

    pub(crate) async fn quality_control(
        &self,
        owner_node_id: &str,
        request: &crate::http::hls::QualityControlRelayRequest,
    ) -> Result<QualityControlRelayOutcome, PeerTransportError> {
        let body = serde_json::to_vec(request).map_err(|_| PeerTransportError::InvalidResponse)?;
        if body.len() > crate::http::hls::QUALITY_CONTROL_MAX_BYTES {
            return Err(PeerTransportError::InvalidResponse);
        }
        let budget =
            crate::playback_control::inherited_exchange_budget(request.deadline_unix_ms, unix_ms())
                .ok_or(PeerTransportError::TimedOut)?;
        let deadline = deadline_after(budget);
        let base = self.peer_base(owner_node_id, deadline).await?;
        let (response, retry_after) = self
            .transport
            .request_with_retry_after(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                crate::http::hls::QUALITY_CONTROL_PATH,
                body,
                deadline,
                crate::http::hls::QUALITY_CONTROL_MAX_BYTES,
                PeerAuthMode::ExactRequest,
            )
            .await?;
        classify_quality_control_relay(response, retry_after, &request.request)
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

fn classify_prepared_media_route(
    resolution: DurableRouteResolution,
    authorized: bool,
    local_node_id: &str,
    now_ms: i64,
) -> DurableRouteResolution {
    let DurableRouteResolution::OwnerTransition(route) = resolution else {
        return resolution;
    };
    let authorized = route.state == "active"
        && route.publication_ready_at_ms != 0
        && route.lease_expires_at_ms > now_ms
        && authorized;
    if !authorized {
        return DurableRouteResolution::OwnerTransition(route);
    }
    if route.owner_node_id == local_node_id {
        DurableRouteResolution::ActiveLocal(route)
    } else {
        DurableRouteResolution::ActiveRemote(route)
    }
}

fn prepared_successor_route(route: &MediaSessionRoute) -> bool {
    #[derive(Deserialize)]
    struct ResponseMarker {
        #[serde(default)]
        plan_notes: Vec<String>,
    }

    serde_json::from_str::<ResponseMarker>(&route.response_json)
        .ok()
        .is_some_and(|response| {
            response
                .plan_notes
                .iter()
                .any(|note| note == PREPARED_SUCCESSOR_PLAN_NOTE)
        })
}

fn same_media_route(left: &MediaSessionRoute, right: &MediaSessionRoute) -> bool {
    left.incarnation_id == right.incarnation_id
        && left.session_id == right.session_id
        && left.principal == right.principal
        && left.playback_id == right.playback_id
        && left.owner_node_id == right.owner_node_id
        && left.owner_epoch == right.owner_epoch
        && left.state == right.state
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

/// What an owner answered a relayed quality-control exchange.
pub(crate) enum QualityControlRelayOutcome {
    Reply(crate::http::hls::QualityControlResponse),
    /// The owner has no quality-control endpoint (an older build).
    Unsupported,
    /// The owner refused or deferred this exchange; its status, body and
    /// retry hint reach the client unchanged.
    Refused(Response<Body>),
}

/// An owner's quality-control answer, keeping its meaning across the relay.
///
/// A 404 carrying the owner's own `session_gone` body is a route miss on a
/// node that does implement the endpoint; any other 404, or a 405, is an
/// owner without the endpoint, which is what "unsupported" means. Owner
/// refusals (409/410/425) and deferrals (429/503) pass through rather than
/// collapsing into a 503 that loses the difference between "stop" and
/// "retry".
fn classify_quality_control_relay(
    response: PeerResponse,
    retry_after: Option<u32>,
    request: &crate::http::hls::QualityControlRequest,
) -> Result<QualityControlRelayOutcome, PeerTransportError> {
    let status = response.status.as_u16();
    if response.status.is_success() {
        return serde_json::from_slice::<crate::http::hls::QualityControlResponse>(&response.body)
            .ok()
            .filter(|reply| reply.valid_for(request))
            .map(QualityControlRelayOutcome::Reply)
            .ok_or(PeerTransportError::InvalidResponse);
    }
    let error_body =
        serde_json::from_slice::<crate::playback_control::ControlErrorBody>(&response.body)
            .ok()
            .filter(|body| body.is_valid_for_status(status));
    match (status, error_body) {
        (404, Some(body)) => quality_control_refusal(status, retry_after, Some(body)),
        (404 | 405, None) => Ok(QualityControlRelayOutcome::Unsupported),
        (409 | 410 | 425 | 429 | 503, body) => {
            // The owner answers some refusals with a bare status; an
            // unparseable body is dropped rather than relayed.
            quality_control_refusal(status, retry_after, body)
        }
        _ => Err(PeerTransportError::InvalidResponse),
    }
}

fn quality_control_refusal(
    status: u16,
    retry_after: Option<u32>,
    body: Option<crate::playback_control::ControlErrorBody>,
) -> Result<QualityControlRelayOutcome, PeerTransportError> {
    let retry_after = retry_after.or_else(|| {
        body.as_ref()
            .and_then(|body| body.retry_after_ms)
            .map(|delay_ms| delay_ms.div_ceil(1000).max(1))
    });
    let bytes = match body {
        Some(body) => serde_json::to_vec(&body).map_err(|_| PeerTransportError::InvalidResponse)?,
        None => Vec::new(),
    };
    relayed_owner_response(status, retry_after, bytes).map(QualityControlRelayOutcome::Refused)
}

/// One owner answer relayed to the client: its status and body, plus its
/// retry hint. A deferral (429/503) always carries one, so a client that
/// waits on the header never spins on an owner that omitted it.
fn relayed_owner_response(
    status: u16,
    retry_after: Option<u32>,
    body: Vec<u8>,
) -> Result<Response<Body>, PeerTransportError> {
    let retry_after = retry_after.or(matches!(status, 429 | 503).then_some(1));
    let mut builder = Response::builder()
        .status(status)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(seconds) = retry_after {
        builder = builder.header(header::RETRY_AFTER, seconds.to_string());
    }
    builder
        .body(Body::from(body))
        .map_err(|_| PeerTransportError::InvalidResponse)
}

fn validated_control_relay_response(
    response: PeerResponse,
    request: &crate::playback_control::ControlRelayRequest,
) -> Result<Response<Body>, PeerTransportError> {
    let status = StatusCode::from_u16(response.status.as_u16())
        .map_err(|_| PeerTransportError::InvalidResponse)?;
    let mut processing_header = None;
    let body = if status.is_success() {
        let parsed =
            serde_json::from_slice::<crate::playback_control::ControlResponseV1>(&response.body)
                .ok()
                .filter(|parsed| parsed.is_valid_for(request))
                .ok_or(PeerTransportError::InvalidResponse)?;
        processing_header = response
            .dv_processing_header
            .as_ref()
            .and_then(|header| {
                plurx_core::transcode::dv_processing::DvEffectiveProcessingReport::from_owner_wire(
                    header.as_bytes(),
                    &request.generation,
                    parsed.effective_selection.dynamic_range.as_deref(),
                )
            })
            .and_then(|report| serde_json::to_string(&report).ok());
        serde_json::to_vec(&parsed).map_err(|_| PeerTransportError::InvalidResponse)?
    } else {
        let parsed =
            serde_json::from_slice::<crate::playback_control::ControlErrorBody>(&response.body)
                .ok()
                .filter(|parsed| parsed.is_valid_for_status(status.as_u16()))
                .ok_or(PeerTransportError::InvalidResponse)?;
        serde_json::to_vec(&parsed).map_err(|_| PeerTransportError::InvalidResponse)?
    };
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(report) = processing_header {
        builder = builder.header("x-plurx-dv-processing", report);
    }
    builder
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
/// owner lost — cannot be made correct here. A rebooting node may recover
/// later, and takeover scans are paged (`TAKEOVER_BATCH` per
/// `TAKEOVER_INTERVAL`). One unrelated stale peer no longer stops takeover,
/// but neither bounded scans nor source availability promise a fixed deadline.
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

pub(crate) fn relay_response_with_limits(
    response: reqwest::Response,
    max_lifetime: Duration,
    no_progress_timeout: Duration,
) -> Result<Response<Body>, PeerTransportError> {
    relay_response_with_limits_observed(
        response,
        max_lifetime,
        no_progress_timeout,
        None,
        None,
        None,
        None,
    )
}

pub(crate) fn relay_response_with_limits_counted_bounded(
    response: reqwest::Response,
    max_lifetime: Duration,
    no_progress_timeout: Duration,
    relayed_bytes: Arc<std::sync::atomic::AtomicU64>,
    max_body_bytes: u64,
) -> Result<Response<Body>, PeerTransportError> {
    relay_response_with_limits_observed(
        response,
        max_lifetime,
        no_progress_timeout,
        None,
        None,
        Some(relayed_bytes),
        Some(max_body_bytes),
    )
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
    relayed_bytes: Option<Arc<std::sync::atomic::AtomicU64>>,
    max_body_bytes: Option<u64>,
) -> Result<Response<Body>, PeerTransportError> {
    if max_body_bytes.is_some_and(|limit| {
        response
            .content_length()
            .is_some_and(|declared| declared > limit)
    }) {
        return Err(PeerTransportError::InvalidResponse);
    }
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
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderName::from_static("x-plurx-producer-paced"),
        HeaderName::from_static("x-plurx-subtitle-complete"),
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
        let mut body_bytes = 0_u64;
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
            crate::media_pool::observe_media_io(0, bytes.len() as u64, None);
            body_bytes = body_bytes.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            if max_body_bytes.is_some_and(|limit| body_bytes > limit) {
                fail(
                    io::ErrorKind::InvalidData,
                    "relayed media response exceeded its byte ceiling".to_owned(),
                );
                return;
            }
            if let Some(total) = relayed_bytes.as_ref() {
                total.fetch_add(
                    u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
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
            tracing::warn!(
                session = %crate::transcode::session_log_id(&session_id),
                %incarnation_id,
                owner_epoch,
                reason,
                "media-session lease lifecycle fencing worker"
            );
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
        // A drain ends on the tick the owner already runs. No timer, no
        // spawned task: the deadline is on the row, so a restart of this
        // process picks the drain up where it left it, and a node that never
        // comes back leaves it to the maintenance sweep instead.
        let drain_now_ms = unix_ms();
        let drained = lease_tick_drained(&routes, &live, drain_now_ms)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let active = lease_tick_active(&routes, &live, drain_now_ms);
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
        // End the lapsed drains against the tick's own shared deadline, the
        // same way renewals are issued, and for the same reason: this loop
        // runs inside a three-second tick that is guarding twelve-second
        // leases, so a store that has gone slow must not be able to spend the
        // tick one route at a time. A serial loop with a fresh four-second
        // budget per route could hold three drains for twelve seconds and
        // take every other session this node owns past its TTL with them.
        let drain_ends = stream::iter(
            drained
                .iter()
                .map(|lease| plurx_core::domain::MediaSessionEnd {
                    incarnation_id: lease.incarnation_id.clone(),
                    session_id: lease.session_id.clone(),
                    expected_owner_node_id: state.node_id.clone(),
                    expected_owner_epoch: lease.owner_epoch,
                    expected_lease_expires_at_ms: lease.lease_expires_at_ms,
                    terminal_reason: "superseded".to_owned(),
                    now_ms: unix_ms(),
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|end| {
                    let store = Arc::clone(&state.store);
                    async move {
                        tokio::time::timeout_at(
                            renewal_deadline,
                            store.end_media_session_if_owner(&end),
                        )
                        .await
                    }
                }),
        )
        .buffer_unordered(LEASE_RENEWAL_FANOUT)
        .collect::<Vec<_>>()
        .await;
        for (lease, outcome) in drained.iter().zip(drain_ends) {
            // Reap the worker only when the row is actually finished. A store
            // call that failed or timed out has ended nothing, and reaping on
            // it would take the session out of `live` — which is what both
            // `lease_tick_drained` and `lease_tick_active` filter on, so this
            // node would never look at the row again and the five-minute
            // cross-node sweep would become the bound for a drain on a
            // perfectly healthy node. Leaving it alone costs one more tick.
            //
            // `Ok(Ok(None))` is finished too: the CAS found no matching row,
            // which means something already ended it or ownership moved. The
            // reap below reads whatever it actually finds.
            let ended = match outcome {
                Ok(Ok(_)) => true,
                Ok(Err(error)) => {
                    tracing::debug!(%error, "draining media session could not be ended");
                    false
                }
                Err(_) => {
                    tracing::debug!("draining media session end timed out");
                    false
                }
            };
            if !ended {
                continue;
            }
            cleanup.push((
                lease.incarnation_id.clone(),
                lease.session_id.clone(),
                lease.owner_epoch,
                // The remembered flag, not this tick's set. A VOD session
                // whose capability has already been dropped from
                // `vod_capabilities` still needs the VOD arm of the reap, or
                // it is fenced and stopped instead of given the typed
                // terminal its client is waiting for. Every other cleanup
                // site in this loop carries the stored flag; this one is not
                // special.
                known
                    .get(&lease.incarnation_id)
                    .is_some_and(|(_, _, _, vod)| *vod)
                    || vod_capabilities.contains(&lease.session_id),
                "drain window elapsed",
            ));
            known.remove(&lease.incarnation_id);
        }
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

/// How long the takeover loop may hold an "off" when its change source
/// cannot report a save made on another node (`SettingsChangeSource::Polled`,
/// or a stopped Raft watch). With a live source a proof is held, on or off,
/// until a change signal says it may be stale; see [`TakeoverGate`].
const TAKEOVER_SETTINGS_REFRESH: Duration = Duration::from_secs(60);
/// How long the loop sleeps between looks while either switch is off, which
/// is the fleet default. Matches the watched drain's idle ceiling.
const TAKEOVER_IDLE_TICK: Duration = plurx_core::store::watched_drain::IDLE_TICK_MAX;

/// This node's takeover-switch writes: a generation that invalidates any
/// held answer taken before the write, and a wake for an idle sleep.
#[derive(Default)]
pub(crate) struct TakeoverSettingsSignal {
    generation: AtomicU64,
    wake: tokio::sync::Notify,
}

impl TakeoverSettingsSignal {
    pub(crate) fn changed(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.wake.notify_one();
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

static TAKEOVER_SETTINGS: LazyLock<Arc<TakeoverSettingsSignal>> =
    LazyLock::new(|| Arc::new(TakeoverSettingsSignal::default()));

/// The settings route wrote a takeover switch on this node.
pub(crate) fn takeover_settings_changed() {
    TAKEOVER_SETTINGS.changed();
}

fn takeover_switches_on(media_pool: Option<&str>, takeover: Option<&str>) -> bool {
    media_pool == Some("1") && takeover == Some("1")
}

/// The Store reads the takeover gate makes: both switches in one statement,
/// an authority read on the replicated backend; and the same two switches as
/// this node's own replica holds them, which only says whether that
/// authority read is owed.
#[plurx_core::cluster::coordination::cluster_job_async_trait]
pub(crate) trait TakeoverSwitches: Send + Sync {
    async fn takeover_switches(&self) -> Result<bool, StoreError>;
    async fn takeover_switches_hint(&self) -> Result<bool, StoreError>;
}

#[plurx_core::cluster::coordination::cluster_job_async_trait]
impl<T: Store + ?Sized> TakeoverSwitches for T {
    async fn takeover_switches(&self) -> Result<bool, StoreError> {
        let (media_pool, takeover) = self
            .get_setting_pair(
                plurx_core::store::keys::CLUSTER_MEDIA_POOL_ENABLED,
                plurx_core::store::keys::CLUSTER_SESSION_TAKEOVER_ENABLED,
            )
            .await?;
        Ok(takeover_switches_on(
            media_pool.as_deref(),
            takeover.as_deref(),
        ))
    }

    async fn takeover_switches_hint(&self) -> Result<bool, StoreError> {
        let media_pool = self
            .local_setting_hint(plurx_core::store::keys::CLUSTER_MEDIA_POOL_ENABLED)
            .await?;
        let takeover = self
            .local_setting_hint(plurx_core::store::keys::CLUSTER_SESSION_TAKEOVER_ENABLED)
            .await?;
        Ok(takeover_switches_on(
            media_pool.as_deref(),
            takeover.as_deref(),
        ))
    }
}

/// The takeover switches, held from one consistent read until a change
/// signal says they may be stale (K-03).
///
/// The loop used to read each switch with its own consistent read every 2 s
/// on every node. K-03 M3 cached an "off" for 60 s but re-read an "on" on
/// every tick, so a node with takeover enabled still paid ~43,000 authority
/// reads a day with nothing happening. A proved answer, on or off, is now
/// held until one of:
/// - a takeover switch is written on this node (the generation): the next
///   tick reads, so a local disable stops the scan and the CAS on the next
///   tick;
/// - this replica applies a Raft entry and its own copy of the switches then
///   disagrees with the held answer: a save on any node is seen once this
///   node applies it, for one consistent read. The settings route is the
///   only writer of either switch, and every write it makes is a Raft entry;
/// - the authority refuses the inventory ([`Self::refused_by_authority`]). A
///   held "on" can outlive a disable saved elsewhere for as long as this
///   replica takes to apply it, but it cannot start work: the inventory read
///   an enabled tick makes before any offer or spawn reads both switches in
///   its own statement on the leader and answers `Disabled`. Only an attempt
///   already past that read when the disable commits can still reach its
///   CAS, within its `TAKEOVER_DEADLINE`, as before the switches were held;
/// - the change source cannot report a save made elsewhere: an "on" is then
///   re-read every tick and an "off" held for at most 60 s, as before.
///
/// Fail-closed as before: a failed read holds nothing, the tick is off, and
/// the next tick reads again.
pub(crate) struct TakeoverGate {
    signal: Arc<TakeoverSettingsSignal>,
    changes: SettingsChangeSource,
    held: Option<HeldTakeoverSwitches>,
}

struct HeldTakeoverSwitches {
    proof: Proved<bool>,
    /// The local write generation, taken before the read, so a write racing
    /// the read makes this answer stale rather than hiding the write.
    generation: u64,
    read_at: tokio::time::Instant,
}

impl TakeoverGate {
    pub(crate) fn new(changes: SettingsChangeSource) -> Self {
        Self::with_signal(Arc::clone(&TAKEOVER_SETTINGS), changes)
    }

    pub(crate) fn with_signal(
        signal: Arc<TakeoverSettingsSignal>,
        changes: SettingsChangeSource,
    ) -> Self {
        Self {
            signal,
            changes,
            held: None,
        }
    }

    pub(crate) async fn enabled<S: TakeoverSwitches + ?Sized>(&mut self, store: &S) -> bool {
        let now = tokio::time::Instant::now();
        let generation = self.signal.generation();
        if let Some(held) = self.held.as_mut() {
            if held.generation == generation {
                if !self.changes.live() {
                    if !held.proof.value
                        && now.saturating_duration_since(held.read_at) < TAKEOVER_SETTINGS_REFRESH
                    {
                        return false;
                    }
                } else if self
                    .changes
                    .confirms(&mut held.proof, || store.takeover_switches_hint())
                    .await
                {
                    return held.proof.value;
                }
            }
        }
        let mark = self.changes.mark();
        match store.takeover_switches().await {
            Ok(on) => {
                self.held = Some(HeldTakeoverSwitches {
                    proof: mark.proved(on),
                    generation,
                    read_at: now,
                });
                on
            }
            // As before, an unreadable switch is off for this tick and the
            // next tick reads again.
            Err(error) => {
                tracing::debug!(%error, "media-session takeover switches unavailable");
                self.held = None;
                false
            }
        }
    }

    /// The authority refused the inventory because a switch is off there:
    /// whatever was held is stale, so drop it and prove the switches again on
    /// the next tick, which then holds the "off" and sleeps the idle period.
    pub(crate) fn refused_by_authority(&mut self) {
        tracing::debug!(
            "media-session takeover refused: a takeover switch is off at the authority"
        );
        self.held = None;
    }

    /// Wait for the loop's next tick and say whether takeover may run on it.
    ///
    /// On: one `TAKEOVER_INTERVAL` tick, exactly as before. Off (the fleet
    /// default): the rest of an idle period is slept here, so the caller
    /// never reaches the inventory read. The sleep is cut short by a local
    /// switch write and, while an "off" is held, by this replica's applied
    /// index moving past the one the held answer covers, which may be an
    /// enable saved elsewhere; the next tick then looks at the replica's
    /// switches. Any other change the Raft watch reports (leader, term,
    /// membership) leaves the index where it was and does not end the sleep. With nothing held (a failed read) only the
    /// idle period and a local write end the sleep, so a failing read keeps
    /// its existing cadence. A wake only ends the sleep: whether the held
    /// answer is stale is the generation's and the replica's call.
    /// `MissedTickBehavior::Delay` makes the tick after a long sleep fire at
    /// once, so a flip is acted on within one tick.
    pub(crate) async fn wait_for_tick<S: TakeoverSwitches + ?Sized>(
        &mut self,
        interval: &mut tokio::time::Interval,
        store: &S,
    ) -> bool {
        interval.tick().await;
        if self.enabled(store).await {
            return true;
        }
        let held = self.held.as_ref().map(|held| &held.proof);
        let watching = held.is_some() && self.changes.live();
        tokio::select! {
            () = tokio::time::sleep(TAKEOVER_IDLE_TICK.saturating_sub(TAKEOVER_INTERVAL)) => {}
            () = self.signal.wake.notified() => {}
            () = self.changes.applied_past(held), if watching => {}
        }
        false
    }
}

/// One clock-admitted inventory page. `cursor` follows the WHOLE page, so a
/// route skipped for its owner's clock cannot pin the keyset scan; `None`
/// means the page was empty and the next tick starts a fresh pass.
#[derive(Debug)]
struct ExpiryScanPage {
    contestable: Vec<MediaSessionRoute>,
    cursor: Option<MediaSessionTakeoverCursor>,
}

/// Contest expired session routes only after the separate replicated rollout
/// switch is enabled. Every candidate independently proves source/pipeline
/// eligibility; the Store CAS still admits exactly one successor epoch.
///
/// A route is contestable only when its OWNER's clock is bounded too: the
/// lease expiry was written by the owner's clock, and coverage excuses an
/// unobserved learner, which may own delegated sessions. Enforced, such a
/// route is skipped (not expired, not taken over) until its owner is measured
/// within the bound or leaves the roster; advisory, it is contested as before
/// and the refusal is counted.
///
/// `Ok(None)` is the authority's refusal: the inventory reads both takeover
/// switches in its own statement and one of them is off there, whatever this
/// node's gate held (see [`TakeoverGate::refused_by_authority`]).
async fn clock_guarded_expiry_scan<Q, F>(
    clock: &ClusterClockGuard,
    query: Q,
) -> Result<Option<ExpiryScanPage>, String>
where
    Q: FnOnce(i64) -> F,
    F: Future<Output = Result<plurx_core::domain::TakeoverInventory, StoreError>>,
{
    let ticket = clock
        .acquire_for(ClockDecision::ExpiryScan)
        .map_err(|cause| format!("media-session expiry clock refused: {cause:?}"))?;
    // One original caller timestamp, not a fresh wall read after the Store
    // await. Discard the entire page on invalidation, leaving its keyset
    // cursor unchanged so a later healthy tick can reconsider it.
    let inventory = tokio::time::timeout(Duration::from_secs(3), query(ticket.now_ms()))
        .await
        .map_err(|_| "media-session expiry inventory timed out".to_owned())?
        .map_err(|error| error.to_string())?;
    let Some(routes) = inventory.into_expired() else {
        return Ok(None);
    };
    clock
        .revalidate_for(ClockDecision::ExpiryScan, &ticket)
        .map_err(|cause| format!("media-session expiry clock refused: {cause:?}"))?;
    let cursor = routes.last().map(MediaSessionTakeoverCursor::from);
    let contestable = routes
        .into_iter()
        .filter(|route| {
            clock
                .admit_owner_for(ClockDecision::ExpiryScan, &ticket, &route.owner_node_id)
                .inspect_err(|cause| {
                    tracing::debug!(
                        owner = %route.owner_node_id,
                        ?cause,
                        "media-session expiry skipped: the owner's clock is not bounded"
                    );
                })
                .is_ok()
        })
        .collect();
    Ok(Some(ExpiryScanPage {
        contestable,
        cursor,
    }))
}

pub(crate) async fn takeover_loop(state: AppState) {
    let mut interval = tokio::time::interval(TAKEOVER_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Keep the keyset position across ticks. Permanently ineligible legacy or
    // malformed recipes remain authoritative until expiry/maintenance, but
    // cannot pin this bounded scanner to the oldest page in the meantime.
    let mut scan_cursor: Option<MediaSessionTakeoverCursor> = None;
    let mut gate = TakeoverGate::new(state.settings_changes.clone());
    loop {
        if !gate
            .wait_for_tick(&mut interval, state.store.as_ref())
            .await
        {
            scan_cursor = None;
            continue;
        }
        let clock = state.membership.clock_guard();
        let page = match clock_guarded_expiry_scan(&clock, |now_ms| {
            state
                .store
                .expired_media_sessions(now_ms, scan_cursor.clone(), TAKEOVER_BATCH)
        })
        .await
        {
            Ok(Some(page)) => page,
            // The gate held an "on" that the authority no longer has: a
            // disable saved elsewhere that this replica has not applied yet.
            // Off for this tick, exactly as a gate reading "off" would be.
            Ok(None) => {
                gate.refused_by_authority();
                scan_cursor = None;
                continue;
            }
            Err(error) => {
                tracing::debug!(%error, "media-session takeover inventory unavailable");
                continue;
            }
        };
        let Some(cursor) = page.cursor else {
            // Reaching the end starts a fresh oldest-first pass on the next
            // tick. This also revisits transient refusals without sacrificing
            // bounded progress through the current inventory.
            scan_cursor = None;
            continue;
        };
        // Past the whole page, including routes skipped for their owner's
        // clock, so they cannot pin the scanner to one page.
        scan_cursor = Some(cursor);
        let routes = page.contestable;
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
    before_submission: impl FnOnce() -> Result<(), ClockRefusal>,
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
    // The fixed proposal is still unsubmitted here. A clock refusal is not a
    // Store ambiguity: stop the exact worker, never enter replay/read. After
    // io.claim is submitted its original reconciliation remains ungated.
    if let Err(cause) = before_submission() {
        stop_pending_takeover(
            pending,
            "media-session takeover clock refused before submission",
            TAKEOVER_SKIPPED,
        )
        .await;
        return Err(format!("media-session takeover clock refused: {cause:?}"));
    }
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
    clock: TakeoverClockAdmission,
) -> Result<(), String> {
    let provisional_id = start.provisional_session_id.clone();
    let replacement = state
        .transcode
        .acquire_cluster_takeover_replacement(&request, &original.principal, creation_deadline)
        .await?;
    // The takeover id is predetermined, so it can be fenced from the moment the
    // gate is held — including by a later open that has to reclaim the key
    // because this supervisor never finished.
    replacement.publish_fenceable(&provisional_id);
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
    let creation_recovery = crate::transcode::SessionRecoveryIdentity {
        principal: original.principal.clone(),
        incarnation_id: original.incarnation_id.clone(),
        recovery_epoch: original.recovery_epoch.clone(),
    };
    let creation = tokio::spawn(async move {
        creation_manager
            .create_cluster_takeover_session_under_guard(
                &creation_request,
                &creation_recovery,
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

    // Preparation spends this original 24-second lease, rather than replacing
    // the ticket's wall time or silently extending its monotonic lifetime.
    // Existing minimum-runway checks refuse a late publication; bootstrap
    // renewal remains unchanged once the exact fixed proposal has won.
    let claim_monotonic_expiry = clock.monotonic_expiry;
    let claim_cache_generation = state.media_sessions.route_generation(&original.session_id);
    let takeover = clock.claim(&original, &state.node_id);
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
    settle_initial_takeover_claim(&state, pending, || clock.ticket.revalidate()).await
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
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
            user_id: route
                .principal
                .local_user_id()
                .expect("local test principal"),
        },
        typeless_playlist: true,
        request: SessionRequest {
            dv_processing_destination: None,
            sdr_master_codecs: None,
            continuous_media: None,
            quality_catalog: None,
            candidate_context: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
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
    let clock = acquire_takeover_clock(&state.membership.clock_guard(), &route.owner_node_id)
        .map_err(|cause| {
            metric.outcome = TAKEOVER_SKIPPED;
            format!("media-session takeover clock refused: {cause:?}")
        })?;
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
    // Takeover is the only producer of a `Presentation::Live` recipe —
    // `takeover_recipe_is_valid` requires that stamp and both public and
    // worker ingress refuse it — so it was also the one path that could put a
    // session on the retained growing-HLS engine while the switch that governs
    // that engine was off. It consults the switch now.
    //
    // This is a decision, and it was taken the other way first. The argument
    // for adopting anyway is that the session already exists and someone is
    // watching it, so refusing ends a film rather than preventing a stream.
    // The argument that won is simpler and is a rule rather than a trade: the
    // cluster does not do what a setting says it may not do. The window is
    // narrow — a session that started while the fallback was on, kept running
    // when an operator turned it off, and then outlived its owner — and a
    // cluster-wide setting means every node agrees about it, so the refusal
    // cannot be a disagreement between nodes. The message names the switch,
    // because "takeover failed" would send an operator looking at the node
    // that died.
    // A store read that fails is not an answer of "on". It is a skip, so the
    // next sweep asks again, rather than an adoption taken on a value nobody
    // read — which is the shape of the readiness rows an earlier review
    // rejected for reporting a status from a reading the code never took.
    match state.transcode.live_hls_recovery_enabled().await {
        Ok(true) => {}
        Ok(false) => {
            metric.outcome = TAKEOVER_SKIPPED;
            return Err(
                "live HLS fallback is disabled cluster-wide, so this session cannot be adopted"
                    .to_owned(),
            );
        }
        Err(error) => {
            metric.outcome = TAKEOVER_SKIPPED;
            return Err(format!(
                "takeover could not read the live HLS fallback setting: {error}"
            ));
        }
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
    // Persisted candidate envelopes omit process-local execution context. Rebind
    // the exact source/worker recipe before dispatch; never silently execute
    // an ordinary height plan after takeover.
    tokio::time::timeout_at(
        deadline,
        state
            .transcode
            .restore_candidate_context_with_deadline(&mut envelope, deadline),
    )
    .await
    .map_err(|_| "candidate takeover validation timed out".to_owned())?
    .map_err(|error| error.to_string())?;
    let user = tokio::time::timeout_at(
        deadline,
        state.store.get_user(
            route
                .principal
                .local_user_id()
                .ok_or_else(|| "sharing takeover requires typed source authority".to_owned())?,
        ),
    )
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
            clock,
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
        && route.principal == envelope.principal
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
    now_ms: i64,
) -> Vec<&'a OwnedMediaSessionLease> {
    let settling = settlement_protected_ids();
    routes
        .iter()
        .filter(|route| live.contains(&route.session_id) && !settling.contains(&route.session_id))
        // A route whose drain window has closed is ended by this same tick,
        // below. Renewing it first would move the lease boundary the end has
        // to name, and it is about to stop existing anyway. Excluding it here
        // touches nothing else: a route left out of this list is simply not
        // renewed this tick — it is still in the owner inventory, so the
        // missing-from-inventory reaper does not see it either.
        .filter(|route| !drain_window_closed(route, now_ms))
        .collect()
}

/// Whether this owned route is draining and its deadline has passed.
///
/// `None` is not draining, which is every ordinary session. The comparison is
/// against the tick's own clock rather than the deadline's origin, because the
/// node that wrote the deadline may not be this one.
fn drain_window_closed(route: &OwnedMediaSessionLease, now_ms: i64) -> bool {
    route
        .drain_deadline_ms
        .is_some_and(|deadline_ms| deadline_ms <= now_ms)
}

/// The owned routes whose drain window has closed, in inventory order.
fn lease_tick_drained<'a>(
    routes: &'a [OwnedMediaSessionLease],
    live: &HashSet<String>,
    now_ms: i64,
) -> Vec<&'a OwnedMediaSessionLease> {
    let settling = settlement_protected_ids();
    routes
        .iter()
        .filter(|route| live.contains(&route.session_id) && !settling.contains(&route.session_id))
        .filter(|route| drain_window_closed(route, now_ms))
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
mod tests;

#[cfg(test)]
#[path = "media_sessions/cleanup_tests.rs"]
mod cleanup_tests;

/// K-03: the takeover loop's switch reads, on a virtual clock.
#[cfg(test)]
mod takeover_gate_tests;
