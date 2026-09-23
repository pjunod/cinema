//! Read-only cluster operations status and bounded peer aggregation.
//!
//! Membership remains the authority for identity and roles. Each process is
//! the only authority for its local readiness, Raft watch, WAL, snapshots,
//! and media sessions; the aggregate joins those two sources without turning
//! a roster heartbeat into a successful direct observation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::{
    ActivityPeer, ClusterNodeRecord, ClusterOperationCleanupOutcome, ClusterOperationLease,
    MembershipError, MembershipManager, MembershipStatus, NodeRole, MAX_OPERATIONS_PEERS,
};
use plurx_core::cluster::migration::status::{
    DbSnapshotMetricsSnapshot, SnapshotTransportPhase, SnapshotTransportStatus, WalRuntimeState,
    WalStatusSnapshot,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::cluster::api_error;
use super::error::ApiError;
use super::extract::{AdminUser, CacheOnlyAdminUser};
use super::peer_transport::{
    deadline_after, exact_auth_from_headers, PeerAuthMode, PeerTransport, PeerTransportError,
};
use super::{ReadinessEvaluation, ReadinessFailure};
use crate::serving_fence::PlannedOutageFenceToken;
use crate::state::AppState;

pub(crate) const INTERNAL_PATH: &str = "/api/v1/internal/cluster/operations-status";
const AGGREGATE_TIMEOUT: Duration = Duration::from_secs(2);
const PEER_STATUS_TIMEOUT: Duration = Duration::from_secs(1);
const PEER_DIRECTORY_TIMEOUT: Duration = Duration::from_millis(500);
const PEER_STATUS_CACHE_TTL: Duration = Duration::from_secs(5);
const PEER_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(3);
const TRANSPORT_FALLBACK_STALL_MS: u64 = 30 * 1_000;
const TRANSPORT_OBSERVATION_TTL_MS: u64 = 5 * 60 * 1_000;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_FRESH_AGE_MS: u64 = 5_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ClusterNodeOperationsStatus {
    pub schema_version: u32,
    pub observed_at_unix_ms: u64,
    pub node_id: String,
    pub raft_id: Option<u64>,
    pub hostname: String,
    pub build: String,
    pub protocol_min: i64,
    pub protocol_max: i64,
    pub process: ProcessStatus,
    pub serving: ReadinessEvaluation,
    pub raft: RaftStatus,
    pub wal: WalStatus,
    pub snapshot: SnapshotStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<SnapshotTransportStatus>,
    pub media: MediaDrainStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ProcessStatus {
    /// A direct response is the proof. Aggregation never synthesizes this for
    /// a silent process.
    pub live: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RaftStatus {
    pub running: Option<bool>,
    pub sample_valid: bool,
    pub sample_age_seconds: Option<u64>,
    pub current_term: Option<u64>,
    pub leader_id: Option<u64>,
    pub is_leader: Option<bool>,
    pub applied_index: Option<u64>,
    pub commit_index: Option<u64>,
    pub apply_lag_entries: Option<u64>,
    pub watermark_valid: bool,
    pub watermark_age_millis: Option<u64>,
    pub observation_errors: u64,
    pub watermark_errors: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct WalStatus {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<WalStatusSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SnapshotStatus {
    pub available: bool,
    #[serde(default)]
    pub db_bytes: Option<u64>,
    pub build_ok_count: u64,
    pub build_error_count: u64,
    pub install_ok_count: u64,
    pub install_error_count: u64,
    #[serde(default)]
    pub last_build_outcome: Option<String>,
    #[serde(default)]
    pub last_build_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_install_outcome: Option<String>,
    #[serde(default)]
    pub last_install_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MediaDrainStatus {
    pub local_active_sessions: usize,
    pub drained: bool,
    #[serde(default)]
    pub new_admissions_blocked: bool,
    #[serde(default)]
    pub admissions_in_flight: u64,
    #[serde(default)]
    pub preparation_expires_at_unix_ms: Option<u64>,
    pub direct_play_connections: DirectPlayConnectionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restart_commands: Vec<RestartCommand>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RestartPreparationResponse {
    pub schema_version: u32,
    pub node_id: String,
    pub media: MediaDrainStatus,
    /// The daemon does not invoke a supervisor. These are exact commands for
    /// the supported deployment shapes; the operator selects the one that
    /// owns this host after `media.drained=true`.
    pub restart_commands: Vec<RestartCommand>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RestartCommand {
    pub supervisor: String,
    pub command: String,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RestartPreparationRequest {
    pub expires_in_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DirectPlayConnectionStatus {
    UnknownRequiresProxy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationState {
    Answered,
    Unreachable,
    TimedOut,
    /// The peer answered and refused the proof: 401 or 403. A refusal is
    /// evidence the node is up and reachable, which is the opposite of what
    /// `Unreachable` tells an operator, so the two are never merged. This is
    /// the same split the activity fan-out already draws.
    Refused,
    /// The peer answered with some other non-success status.
    HttpError,
    InvalidResponse,
    IdentityMismatch,
    /// No cache-backed direct observation is currently available. The
    /// `error_class` distinguishes an uninitialized cache from an expired one.
    Unavailable,
    PeerLimit,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ClusterNodeObservation {
    pub membership: ClusterNodeRecord,
    pub observation: ObservationState,
    pub sample_age_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ClusterNodeOperationsStatus>,
    /// Node-owned evidence fetched over the private cluster listener. Kept
    /// outside `status` so a closed public listener cannot hide recovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<SnapshotTransportStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ClusterOperationsAggregate {
    pub schema_version: u32,
    pub observed_at_unix_ms: u64,
    pub membership: MembershipStatus,
    pub nodes: Vec<ClusterNodeObservation>,
    pub verdict: RolloutVerdict,
    #[serde(default)]
    pub maintenance: Vec<MaintenanceEntryVerdict>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RolloutVerdict {
    pub safe_to_restart_one: bool,
    pub candidate_node_id: Option<String>,
    pub blockers: Vec<RolloutFinding>,
    pub warnings: Vec<RolloutFinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct RolloutFinding {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MaintenanceEntryVerdict {
    pub node_id: String,
    pub safe_to_enter: bool,
    pub blockers: Vec<RolloutFinding>,
}

pub(crate) async fn local(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<ClusterNodeOperationsStatus>), StatusCode> {
    let auth = exact_auth_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if !state
        .membership
        .authorize_internal_peer_read_request(&auth, "GET", INTERNAL_PATH, &[])
        .await
        .unwrap_or(false)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok((
        private_no_store_headers(),
        Json(local_snapshot(&state).await),
    ))
}

pub(crate) async fn aggregate(
    _admin: CacheOnlyAdminUser,
    State(state): State<AppState>,
) -> Result<(HeaderMap, Json<ClusterOperationsAggregate>), ApiError> {
    aggregate_response_with(|| collect_aggregate(&state)).await
}

/// Complete GET response boundary whose only input is the node-owned
/// projection collector. Keeping `AppState` outside this function makes Store
/// or peer-network work impossible to hide inside the request boundary.
async fn aggregate_response_with<Collect, CollectFuture>(
    collect: Collect,
) -> Result<(HeaderMap, Json<ClusterOperationsAggregate>), ApiError>
where
    Collect: FnOnce() -> CollectFuture,
    CollectFuture: std::future::Future<Output = Result<ClusterOperationsAggregate, ApiError>>,
{
    let aggregate = collect().await?;
    Ok((private_no_store_headers(), Json(aggregate)))
}

pub(crate) async fn collect_aggregate(
    state: &AppState,
) -> Result<ClusterOperationsAggregate, ApiError> {
    collect_aggregate_with_cached_sources(
        &state.node_id,
        || state.membership_status_cache.fresh(),
        || local_snapshot(state),
        || state.peer_status_cache.fresh(),
    )
    .await
}

/// Safety-changing operations re-read committed membership and make fresh,
/// authenticated observations of the bounded peer directory. The complete
/// preflight shares one absolute deadline so a slow Store read cannot consume
/// an unbounded amount of request time before peer probes begin.
pub(crate) async fn collect_current_aggregate(
    state: &AppState,
) -> Result<ClusterOperationsAggregate, ApiError> {
    let deadline = deadline_after(AGGREGATE_TIMEOUT);
    let membership_for_status = state.membership.clone();
    let membership_for_directory = state.membership.clone();
    let transport = PeerTransport::new(state.membership.clone());
    let replication = state.replication.clone();
    collect_current_aggregate_with_sources(
        &state.node_id,
        deadline,
        move || async move {
            membership_for_status
                .status()
                .await
                .map_err(|error| error.to_string())
        },
        move || async move {
            membership_for_directory
                .operations_peers()
                .await
                .map_err(|error| error.to_string())
        },
        || local_snapshot(state),
        move |peers, deadline| async move {
            collect_peer_statuses(peers, transport, replication, deadline).await
        },
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) enum PlannedOutageOperation {
    Restart,
    Maintenance,
}

/// Cancellation-safe owner of the exact replicated planned-outage claim.
/// Every early return and task abort retries exact release from `Drop`; only a
/// successful restart preparation or maintenance commit disarms it.
pub(crate) struct PlannedOutageLeaseGuard {
    lease: Option<ClusterOperationLease>,
    membership: MembershipManager,
    serving: crate::serving_fence::ServingFence,
    local_fence: Option<PlannedOutageFenceToken>,
    shutdown: tokio_util::sync::CancellationToken,
    release_runtime: tokio::runtime::Handle,
}

impl PlannedOutageLeaseGuard {
    fn new(
        lease: ClusterOperationLease,
        membership: MembershipManager,
        serving: crate::serving_fence::ServingFence,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            lease: Some(lease),
            membership,
            serving,
            local_fence: None,
            shutdown,
            release_runtime: tokio::runtime::Handle::current(),
        }
    }

    #[cfg(test)]
    fn local_fence_only_for_test(
        serving: crate::serving_fence::ServingFence,
        local_fence: PlannedOutageFenceToken,
    ) -> Self {
        Self {
            lease: None,
            membership: MembershipManager::unavailable(),
            serving,
            local_fence: Some(local_fence),
            shutdown: tokio_util::sync::CancellationToken::new(),
            release_runtime: tokio::runtime::Handle::current(),
        }
    }

    pub(crate) fn claim(&self) -> &ClusterOperationLease {
        self.lease.as_ref().expect("armed planned-outage lease")
    }

    pub(crate) fn arm_local_fence(&mut self, token: PlannedOutageFenceToken) {
        self.local_fence = Some(token);
    }

    fn retain_local_fence(&self) {
        if let Some(token) = self.local_fence {
            self.serving
                .retain_planned_outage_preparation_until_cancelled(token);
        }
    }

    async fn cancel_local_fence(&self, active_sessions: usize) {
        if let Some(token) = self.local_fence {
            self.serving
                .cancel_planned_outage_preparation(token, active_sessions)
                .await;
        }
    }

    /// Order an exact replicated release before any local unfence. This is
    /// also the barrier used after an outcome-unknown maintenance commit: the
    /// older transaction can create the maintenance row only before this
    /// delete, or it runs afterwards without the exact claim and is a no-op.
    pub(crate) async fn release_replicated_claim(&mut self) -> Result<(), ApiError> {
        let Some(lease) = self.lease.as_ref() else {
            return Ok(());
        };
        if self.local_fence.is_some() {
            // Maintenance calls this lower-level barrier directly. Latch here,
            // before the release await, so every caller remains fail-closed
            // across an ambiguous response and the original lease deadline.
            self.retain_local_fence();
        }
        release_exact_claim_with_retry(&self.membership, lease, &self.shutdown).await?;
        if let Some(token) = self.local_fence {
            // The exact replicated response is now definitive. Keep the timed
            // fence for maintenance reconciliation, but remove only this
            // attempt's ambiguity latch.
            self.serving.resolve_planned_outage_release(token);
        }
        self.lease.take();
        Ok(())
    }

    pub(crate) async fn release(mut self) {
        if self.local_fence.is_some() {
            self.retain_local_fence();
        }
        if let Err(error) = self.release_replicated_claim().await {
            tracing::warn!(
                ?error,
                "planned-outage release ended before a definitive response"
            );
            if !self.shutdown.is_cancelled() {
                // A non-ambiguous failure did not apply the deletion. Remove
                // the ambiguity latch but retain the bounded timed fence; the
                // replicated lease expires through heartbeat ownership.
                if let Some(token) = self.local_fence {
                    self.serving.resolve_planned_outage_release(token);
                }
            }
            self.local_fence = None;
            self.lease.take();
            return;
        }
        if self.local_fence.is_some() {
            self.cancel_local_fence(0).await;
            self.local_fence = None;
        }
    }

    pub(crate) fn disarm(mut self) {
        self.local_fence = None;
        self.lease.take();
    }
}

impl Drop for PlannedOutageLeaseGuard {
    fn drop(&mut self) {
        let lease = self.lease.take();
        let local_fence = self.local_fence.take();
        if lease.is_none() && local_fence.is_none() {
            return;
        }
        if let Some(token) = local_fence {
            // Latch synchronously: the configured deadline could expire
            // before the async Drop cleanup first runs.
            self.serving
                .retain_planned_outage_preparation_until_cancelled(token);
        }
        if self.shutdown.is_cancelled() {
            // The process is already draining. Leave any local latch closed;
            // spawning an immortal retry during runtime teardown cannot make
            // the replicated outcome safer.
            return;
        }
        let membership = self.membership.clone();
        let serving = self.serving.clone();
        let shutdown = self.shutdown.clone();
        self.release_runtime.spawn(async move {
            let _operation_guard = serving.planned_outage_cleanup_guard().await;
            if let Some(lease) = lease {
                if let Err(error) =
                    release_exact_claim_with_retry(&membership, &lease, &shutdown).await
                {
                    tracing::warn!(
                        ?error,
                        "cancelled planned-outage release ended without a definitive response"
                    );
                    if !shutdown.is_cancelled() {
                        if let Some(token) = local_fence {
                            serving.resolve_planned_outage_release(token);
                        }
                    }
                    return;
                }
            }
            if let Some(token) = local_fence {
                serving.resolve_planned_outage_release(token);
                serving.cancel_planned_outage_preparation(token, 0).await;
            }
        });
    }
}

const PLANNED_OUTAGE_CLEANUP_RETRY_INITIAL: Duration = Duration::from_millis(50);
const PLANNED_OUTAGE_CLEANUP_RETRY_MAX: Duration = Duration::from_secs(2);

fn next_planned_outage_cleanup_delay(current: Duration) -> Duration {
    current
        .saturating_mul(2)
        .min(PLANNED_OUTAGE_CLEANUP_RETRY_MAX)
}

async fn wait_for_planned_outage_cleanup_retry(
    shutdown: &tokio_util::sync::CancellationToken,
    delay: Duration,
) -> Result<(), ApiError> {
    tokio::select! {
        () = shutdown.cancelled() => Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "planned_outage_cleanup_shutdown",
            "planned-outage cleanup stopped because this process is shutting down",
        )),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

async fn release_exact_claim_with_retry(
    membership: &MembershipManager,
    lease: &ClusterOperationLease,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<(), ApiError> {
    let mut delay = PLANNED_OUTAGE_CLEANUP_RETRY_INITIAL;
    loop {
        match membership
            .release_cluster_operation_lease(lease)
            .await
            .map_err(api_error)?
        {
            ClusterOperationCleanupOutcome::Confirmed => return Ok(()),
            ClusterOperationCleanupOutcome::Ambiguous => {
                wait_for_planned_outage_cleanup_retry(shutdown, delay).await?;
                delay = next_planned_outage_cleanup_delay(delay);
            }
        }
    }
}

async fn release_current_restart_with_retry(
    membership: &MembershipManager,
    node_id: &str,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<(), ApiError> {
    let mut delay = PLANNED_OUTAGE_CLEANUP_RETRY_INITIAL;
    loop {
        match membership
            .release_restart_preparation(node_id)
            .await
            .map_err(api_error)?
        {
            ClusterOperationCleanupOutcome::Confirmed => return Ok(()),
            ClusterOperationCleanupOutcome::Ambiguous => {
                wait_for_planned_outage_cleanup_retry(shutdown, delay).await?;
                delay = next_planned_outage_cleanup_delay(delay);
            }
        }
    }
}

/// Keep ownership-bearing planned-outage work alive when an HTTP request is
/// cancelled. Values sent after the receiver disappears are dropped in the
/// spawned task, so their RAII cleanup still runs after the replicated outcome
/// is known instead of racing an in-flight consensus write.
pub(crate) async fn run_planned_outage_task<T, Work>(work: Work) -> Result<T, ApiError>
where
    T: Send + 'static,
    Work: std::future::Future<Output = Result<T, ApiError>> + Send + 'static,
{
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = result_tx.send(work.await);
    });
    result_rx.await.unwrap_or_else(|_| {
        Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "planned_outage_owner_failed",
            "the planned-outage ownership task ended before reporting its replicated outcome",
        ))
    })
}

/// Acquire the one process-local prepare/cancel lane before detaching accepted
/// ownership. Contending requests fail fast instead of building an unbounded
/// Tokio mutex queue; once work starts, its gate survives the HTTP waiter until
/// every replicated and local outcome is definitive.
pub(crate) async fn run_serialized_planned_outage_task<T, Work>(
    serving: &crate::serving_fence::ServingFence,
    work: Work,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    Work: std::future::Future<Output = Result<T, ApiError>> + Send + 'static,
{
    let operation_guard = serving.try_planned_outage_operation_guard().map_err(|_| {
        ApiError::typed(
            StatusCode::CONFLICT,
            "planned_outage_operation_pending",
            "another restart or maintenance operation is still resolving; retry after it completes",
        )
    })?;
    run_planned_outage_task(async move {
        let _operation_guard = operation_guard;
        work.await
    })
    .await
}

/// Claim the replicated membership-lifecycle exclusion before collecting the
/// safety evidence it protects. A failed collection releases the exact claim;
/// a successful caller owns it until restart preparation or maintenance
/// consumes/cancels it.
pub(crate) async fn acquire_planned_outage_preflight(
    state: &AppState,
    node_id: &str,
    duration: Duration,
    operation: PlannedOutageOperation,
) -> Result<(PlannedOutageLeaseGuard, ClusterOperationsAggregate), ApiError> {
    let prepared_lease = match operation {
        PlannedOutageOperation::Restart => {
            MembershipManager::prepare_restart_preparation_claim(node_id, duration)
        }
        PlannedOutageOperation::Maintenance => {
            MembershipManager::prepare_maintenance_preparation_claim(node_id, duration)
        }
    }
    .map_err(api_error)?;
    // Own the exact intended claim before the replicated write is first
    // polled. If the HTTP waiter disappears, the detached task still resolves
    // the commit attempt before this guard can perform an exact release.
    let lease = PlannedOutageLeaseGuard::new(
        prepared_lease,
        state.membership.clone(),
        state.serving.clone(),
        state.shutdown.clone(),
    );
    let membership = state.membership.clone();
    let lease = run_planned_outage_task(async move {
        match membership
            .commit_cluster_operation_lease(lease.claim())
            .await
        {
            Ok(()) => Ok(lease),
            Err(error @ MembershipError::ClusterOperationPending) => {
                // A definitive zero-row compare-and-swap never submitted this
                // claim. Do not create a permanent receipt for every rejected
                // operator retry.
                lease.disarm();
                Err(api_error(error))
            }
            Err(error) => {
                let error = api_error(error);
                // An expired ambiguous acquire still needs its exact one-shot
                // receipt. Resolve it under this detached owner rather than
                // leaving a new cleanup task per disconnected waiter.
                lease.release().await;
                Err(error)
            }
        }
    })
    .await?;
    let evidence = match collect_current_aggregate(state).await {
        Ok(evidence) => evidence,
        Err(error) => {
            lease.release().await;
            return Err(error);
        }
    };
    Ok((lease, evidence))
}

#[cfg(test)]
async fn acquire_then_collect_preflight<
    Lease,
    Evidence,
    Acquire,
    AcquireFuture,
    Collect,
    CollectFuture,
    CancelRelease,
>(
    acquire: Acquire,
    collect: Collect,
    cancel_release: CancelRelease,
) -> Result<(Lease, Evidence), ApiError>
where
    Acquire: FnOnce() -> AcquireFuture,
    AcquireFuture: std::future::Future<Output = Result<Lease, ApiError>>,
    Collect: FnOnce() -> CollectFuture,
    CollectFuture: std::future::Future<Output = Result<Evidence, ApiError>>,
    CancelRelease: FnOnce(Lease),
{
    let lease = acquire().await?;
    let guard = CancellationReleaseGuard::new(lease, cancel_release);
    match collect().await {
        Ok(evidence) => Ok((guard.disarm(), evidence)),
        Err(error) => Err(error),
    }
}

/// Runs the exact lease cleanup from `Drop`, including when the owning future
/// is aborted at an await point after acquisition. Only a successful evidence
/// collection disarms the guard and transfers ownership to the caller.
#[cfg(test)]
struct CancellationReleaseGuard<Lease, CancelRelease>
where
    CancelRelease: FnOnce(Lease),
{
    lease: Option<Lease>,
    cancel_release: Option<CancelRelease>,
}

#[cfg(test)]
impl<Lease, CancelRelease> CancellationReleaseGuard<Lease, CancelRelease>
where
    CancelRelease: FnOnce(Lease),
{
    fn new(lease: Lease, cancel_release: CancelRelease) -> Self {
        Self {
            lease: Some(lease),
            cancel_release: Some(cancel_release),
        }
    }

    fn disarm(mut self) -> Lease {
        self.cancel_release.take();
        self.lease.take().expect("armed lease guard")
    }
}

#[cfg(test)]
impl<Lease, CancelRelease> Drop for CancellationReleaseGuard<Lease, CancelRelease>
where
    CancelRelease: FnOnce(Lease),
{
    fn drop(&mut self) {
        if let (Some(lease), Some(cancel_release)) = (self.lease.take(), self.cancel_release.take())
        {
            cancel_release(lease);
        }
    }
}

async fn collect_current_aggregate_with_sources<
    MembershipRead,
    MembershipFuture,
    DirectoryRead,
    DirectoryFuture,
    LocalRead,
    LocalFuture,
    CollectPeers,
    CollectPeersFuture,
>(
    local_node_id: &str,
    deadline: tokio::time::Instant,
    membership_read: MembershipRead,
    directory_read: DirectoryRead,
    local_read: LocalRead,
    collect_peers: CollectPeers,
) -> Result<ClusterOperationsAggregate, ApiError>
where
    MembershipRead: FnOnce() -> MembershipFuture,
    MembershipFuture: std::future::Future<Output = Result<MembershipStatus, String>>,
    DirectoryRead: FnOnce() -> DirectoryFuture,
    DirectoryFuture: std::future::Future<Output = Result<Vec<ActivityPeer>, String>>,
    LocalRead: FnOnce() -> LocalFuture,
    LocalFuture: std::future::Future<Output = ClusterNodeOperationsStatus>,
    CollectPeers: FnOnce(Vec<ActivityPeer>, tokio::time::Instant) -> CollectPeersFuture,
    CollectPeersFuture: std::future::Future<Output = BTreeMap<String, PeerStatusOutcome>>,
{
    let preflight = async {
        let membership_deadline =
            deadline.min(tokio::time::Instant::now() + PEER_DIRECTORY_TIMEOUT);
        let membership = tokio::time::timeout_at(membership_deadline, membership_read())
            .await
            .map_err(|_| {
                ApiError::typed(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cluster_roster_timed_out",
                    "the current committed cluster roster did not respond before the preflight deadline",
                )
            })?
            .map_err(|_| {
                ApiError::typed(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cluster_roster_unavailable",
                    "the current committed cluster roster is unavailable",
                )
            })?;

        let directory_deadline = deadline.min(tokio::time::Instant::now() + PEER_DIRECTORY_TIMEOUT);
        let peers = tokio::time::timeout_at(directory_deadline, directory_read())
            .await
            .map_err(|_| {
                ApiError::typed(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cluster_peer_directory_timed_out",
                    "the current committed peer directory did not respond before the preflight deadline",
                )
            })?
            .map_err(|_| {
                ApiError::typed(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cluster_peer_directory_unavailable",
                    "the current committed peer directory is unavailable",
                )
            })?;

        let mut local_status = local_read().await;
        let peer_collection_started = tokio::time::Instant::now();
        let remote = collect_peers(peers, deadline).await;
        let peer_observation_age =
            tokio::time::Instant::now().saturating_duration_since(peer_collection_started);
        if let Some(transport) = local_status.transport.as_mut() {
            age_transport_observations(
                transport,
                u64::try_from(peer_observation_age.as_millis()).unwrap_or(u64::MAX),
            );
        }
        Ok(assemble_aggregate(
            local_node_id,
            membership,
            local_status,
            PeerStatusCacheRead::Fresh(remote, peer_observation_age),
        ))
    };

    tokio::time::timeout_at(deadline, preflight)
        .await
        .map_err(|_| {
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_preflight_timed_out",
                "the current cluster preflight did not complete before its absolute deadline",
            )
        })?
}

async fn collect_aggregate_with_cached_sources<
    MembershipRead,
    MembershipFuture,
    LocalRead,
    LocalFuture,
    PeerRead,
    PeerFuture,
>(
    local_node_id: &str,
    membership_read: MembershipRead,
    local_read: LocalRead,
    peer_read: PeerRead,
) -> Result<ClusterOperationsAggregate, ApiError>
where
    MembershipRead: FnOnce() -> MembershipFuture,
    MembershipFuture: std::future::Future<Output = MembershipStatusCacheRead>,
    LocalRead: FnOnce() -> LocalFuture,
    LocalFuture: std::future::Future<Output = ClusterNodeOperationsStatus>,
    PeerRead: FnOnce() -> PeerFuture,
    PeerFuture: std::future::Future<Output = PeerStatusCacheRead>,
{
    let membership = match membership_read().await {
        MembershipStatusCacheRead::Fresh(membership) => *membership,
        MembershipStatusCacheRead::Unavailable => {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_roster_unavailable",
                "the cached committed cluster roster is not available yet",
            ));
        }
        MembershipStatusCacheRead::Stale => {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_roster_stale",
                "the cached committed cluster roster has expired",
            ));
        }
    };
    let local_status = local_read().await;
    let remote = peer_read().await;
    Ok(assemble_aggregate(
        local_node_id,
        membership,
        local_status,
        remote,
    ))
}

fn assemble_aggregate<Remote>(
    local_node_id: &str,
    membership: MembershipStatus,
    local_status: ClusterNodeOperationsStatus,
    remote: Remote,
) -> ClusterOperationsAggregate
where
    Remote: Into<PeerStatusCacheRead>,
{
    let observed_at_unix_ms = unix_ms();
    let observations = join_observations(
        &membership,
        local_node_id,
        local_status,
        remote,
        observed_at_unix_ms,
    );
    let verdict = rollout_verdict(&membership, &observations);
    let maintenance = maintenance_entry_verdicts(&membership, &observations);
    ClusterOperationsAggregate {
        schema_version: 1,
        observed_at_unix_ms,
        membership,
        nodes: observations,
        verdict,
        maintenance,
    }
}

pub(crate) async fn support_bundle(
    _admin: CacheOnlyAdminUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    support_bundle_response_with(
        || collect_aggregate(&state),
        || state.cluster_logs.tail("trace", 200),
    )
    .await
}

/// Complete support response boundary. Its two dependencies are deliberately
/// narrower than `AppState`: the same cache-only aggregate projection as GET,
/// and one bounded process-local log snapshot.
async fn support_bundle_response_with<Collect, CollectFuture, Logs>(
    collect: Collect,
    logs: Logs,
) -> Result<Response, ApiError>
where
    Collect: FnOnce() -> CollectFuture,
    CollectFuture: std::future::Future<Output = Result<ClusterOperationsAggregate, ApiError>>,
    Logs: FnOnce() -> Vec<crate::logbuf::LogEntry>,
{
    let aggregate = collect().await?;
    let status = serde_json::to_vec_pretty(&aggregate).map_err(bundle_error)?;
    let logs = logs()
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "ts_ms": entry.ts_ms,
                "level": entry.level,
                "target": entry.target,
                "message": redact_operator_text(&entry.message),
            })
        })
        .collect::<Vec<_>>();
    let logs = serde_json::to_vec_pretty(&logs).map_err(bundle_error)?;
    let readme = b"Plurx cluster support bundle v1\n\nThis archive contains bounded, redacted operator evidence. It intentionally excludes credentials, account names, media paths, WAL contents, database rows, and request headers.\n".to_vec();
    let created_at_unix_ms = unix_ms();
    let files = [
        ("cluster-status.json", status.as_slice()),
        ("cluster-log.json", logs.as_slice()),
        ("README.txt", readme.as_slice()),
    ];
    let manifest = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1,
        "created_at_unix_ms": created_at_unix_ms,
        "files": files.iter().map(|(name, bytes)| serde_json::json!({
            "name": name,
            "size_bytes": bytes.len(),
            "sha256": hex::encode(Sha256::digest(bytes)),
        })).collect::<Vec<_>>(),
    }))
    .map_err(bundle_error)?;

    let cursor = Cursor::new(Vec::new());
    let mut archive = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, bytes) in files {
        archive.start_file(name, options).map_err(bundle_error)?;
        archive.write_all(bytes).map_err(bundle_error)?;
    }
    archive
        .start_file("manifest.json", options)
        .map_err(bundle_error)?;
    archive.write_all(&manifest).map_err(bundle_error)?;
    let body = archive.finish().map_err(bundle_error)?.into_inner();
    let mut response = Body::from(body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=plurx-cluster-support.zip"),
    );
    Ok(response)
}

pub(crate) async fn prepare_restart(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Json(request): Json<RestartPreparationRequest>,
) -> Result<Json<RestartPreparationResponse>, ApiError> {
    if node_id != state.node_id {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_node_mismatch",
            "restart preparation must be sent directly to the node named in the route",
        ));
    }
    let serving = state.serving.clone();
    run_serialized_planned_outage_task(&serving, async move {
        let duration =
            Duration::from_secs(request.expires_in_seconds.unwrap_or(900).clamp(60, 3_600));
        let (mut lease, preflight) = acquire_planned_outage_preflight(
            &state,
            &node_id,
            duration,
            PlannedOutageOperation::Restart,
        )
        .await?;
        if !preflight.verdict.safe_to_restart_one {
            lease.release().await;
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "restart_preparation_unsafe",
                "the current cluster rollout verdict does not permit preparing a voter",
            ));
        }
        if preflight.verdict.candidate_node_id.as_deref() != Some(state.node_id.as_str()) {
            lease.release().await;
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "restart_preparation_candidate_mismatch",
                "open the candidate node directly; this node is not the current safe restart candidate",
            ));
        }
        let local_expiry = lease.claim().preparation_expiry_unix_ms(duration);
        let local_fence = if let Some(local_expiry) = local_expiry {
            state
                .serving
                .begin_restart_preparation_until(local_expiry)
                .await
        } else {
            None
        };
        let Some(local_fence) = local_fence else {
            lease.release().await;
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "restart_preparation_expired",
                "restart preparation expired while the replicated lease was being committed; run preflight again",
            ));
        };
        lease.arm_local_fence(local_fence);
        if !state
            .serving
            .wait_for_restart_admissions_until(local_fence, &state.shutdown)
            .await
        {
            lease.release().await;
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "restart_preparation_expired",
                "restart preparation expired before pre-existing admissions settled; run preflight again",
            ));
        }
        let active_sessions = local_owned_media_sessions(&state).await;
        let drain = state.serving.restart_drain_status(active_sessions).await;
        if !drain.new_admissions_blocked {
            lease.release().await;
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "restart_preparation_expired",
                "restart preparation expired before pre-existing admissions settled; run preflight again",
            ));
        }
        lease.disarm();
        Ok(Json(restart_preparation_response(
            &state.node_id,
            active_sessions,
            drain,
        )))
    })
    .await
}

pub(crate) async fn cancel_restart(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Result<Json<RestartPreparationResponse>, ApiError> {
    if node_id != state.node_id {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_node_mismatch",
            "restart preparation cancellation must be sent directly to the node named in the route",
        ));
    }
    let serving = state.serving.clone();
    run_serialized_planned_outage_task(&serving, async move {
        // This attempt has its own latch. Exact preparation guards use their
        // own generations, so overlapping cleanup cannot overwrite or erase
        // either owner.
        let cancellation = state.serving.retain_restart_cancellation();
        if let Err(error) =
            release_current_restart_with_retry(&state.membership, &node_id, &state.shutdown).await
        {
            if !state.shutdown.is_cancelled() {
                state.serving.resolve_planned_outage_release(cancellation);
            }
            return Err(error);
        }
        let active_sessions = local_owned_media_sessions(&state).await;
        let drain = state
            .serving
            .cancel_current_restart_preparation(cancellation, active_sessions)
            .await;
        Ok(Json(restart_preparation_response(
            &state.node_id,
            active_sessions,
            drain,
        )))
    })
    .await
}

fn restart_preparation_response(
    node_id: &str,
    active_sessions: usize,
    drain: crate::serving_fence::RestartDrainStatus,
) -> RestartPreparationResponse {
    let restart_commands = (drain.new_admissions_blocked && drain.drained)
        .then(restart_commands)
        .unwrap_or_default();
    RestartPreparationResponse {
        schema_version: 1,
        node_id: node_id.to_owned(),
        media: media_drain_status(active_sessions, drain),
        restart_commands,
    }
}

fn bundle_error(error: impl std::fmt::Display) -> ApiError {
    tracing::warn!(%error, "could not build cluster support bundle");
    ApiError::typed(
        StatusCode::INTERNAL_SERVER_ERROR,
        "support_bundle_failed",
        "the redacted support bundle could not be built",
    )
}

async fn local_snapshot(state: &AppState) -> ClusterNodeOperationsStatus {
    let observed_at_unix_ms = unix_ms();
    let view = state.replication.metrics_handle().snapshot();
    let sample = view.sample;
    let valid_sample = view.valid.then_some(sample).flatten();
    let valid_watermark = view.watermark_valid.then_some(view.watermark).flatten();
    let mut snapshot = SnapshotStatus::from(view.snapshot_metrics);
    snapshot.db_bytes = view.state_machine_bytes.map(|bytes| bytes.db);
    let active_sessions = local_owned_media_sessions(state).await;
    let drain = state.serving.restart_drain_status(active_sessions).await;
    let mut wal_snapshot = state.replication.wal_status_snapshot();
    if let Some(error) = wal_snapshot
        .as_mut()
        .and_then(|snapshot| snapshot.last_error.as_mut())
    {
        error.message = redact_operator_text(&error.message);
    }
    ClusterNodeOperationsStatus {
        schema_version: 1,
        observed_at_unix_ms,
        node_id: state.node_id.clone(),
        raft_id: sample.map(|sample| sample.node_id),
        hostname: local_hostname(),
        build: crate::version::BUILD.to_owned(),
        protocol_min: plurx_core::store::AUTH_PROTOCOL_MIN,
        protocol_max: plurx_core::store::AUTH_PROTOCOL_MAX,
        process: ProcessStatus { live: true },
        serving: operations_readiness(state),
        raft: RaftStatus {
            running: sample.map(|_| true),
            sample_valid: view.valid,
            sample_age_seconds: view.age_seconds,
            current_term: valid_sample.map(|sample| sample.current_term),
            leader_id: valid_sample.and_then(|sample| sample.leader_id),
            is_leader: valid_sample.map(|sample| sample.is_leader),
            applied_index: valid_sample.and_then(|sample| sample.last_applied_index),
            commit_index: valid_watermark.map(|watermark| watermark.committed_index),
            apply_lag_entries: valid_watermark.and_then(|watermark| watermark.apply_lag_entries),
            watermark_valid: view.watermark_valid,
            watermark_age_millis: view.watermark_age_millis,
            observation_errors: view.errors,
            watermark_errors: view.watermark_errors,
        },
        wal: WalStatus {
            available: wal_snapshot.is_some(),
            reason: wal_snapshot
                .is_none()
                .then(|| "local_live_wal_unavailable".to_owned()),
            snapshot: wal_snapshot,
        },
        snapshot,
        transport: state.replication.transport_status_snapshot(),
        media: media_drain_status(active_sessions, drain),
    }
}

/// Store-free readiness projection for operations diagnostics.
///
/// Replicated nodes already publish readiness through the passive serving
/// fence. A recovery/SQLite process has no cached Store proof on this surface,
/// so it reports unavailable rather than turning an operations request into a
/// Store ping.
fn operations_readiness(state: &AppState) -> ReadinessEvaluation {
    if state.membership.local_maintenance_active() {
        return ReadinessEvaluation {
            ready: false,
            reason: Some(ReadinessFailure::Maintenance),
        };
    }
    if !state.serving.is_quorum_managed() {
        return ReadinessEvaluation {
            ready: false,
            reason: Some(ReadinessFailure::StoreUnavailable),
        };
    }
    if state.serving.is_ready() {
        ReadinessEvaluation {
            ready: true,
            reason: None,
        }
    } else {
        ReadinessEvaluation {
            ready: false,
            reason: Some(ReadinessFailure::QuorumUnavailable),
        }
    }
}

pub(crate) async fn local_owned_media_sessions(state: &AppState) -> usize {
    let (hls, offline) = tokio::join!(
        state.transcode.active_sessions(),
        state.offline.active_preparations()
    );
    hls.saturating_add(state.streams.active_count())
        .saturating_add(offline)
        .saturating_add(state.live_tv.activities().len())
}

fn media_drain_status(
    active_sessions: usize,
    drain: crate::serving_fence::RestartDrainStatus,
) -> MediaDrainStatus {
    let ready_for_supervisor = drain.new_admissions_blocked && drain.drained;
    MediaDrainStatus {
        local_active_sessions: active_sessions,
        drained: drain.drained,
        new_admissions_blocked: drain.new_admissions_blocked,
        admissions_in_flight: drain.admissions_in_flight,
        preparation_expires_at_unix_ms: drain.expires_at_unix_ms,
        direct_play_connections: DirectPlayConnectionStatus::UnknownRequiresProxy,
        restart_commands: ready_for_supervisor
            .then(restart_commands)
            .unwrap_or_default(),
    }
}

fn restart_commands() -> Vec<RestartCommand> {
    vec![
        RestartCommand {
            supervisor: "docker_compose".to_owned(),
            command: "docker compose up -d --no-deps --force-recreate plurxd".to_owned(),
        },
        RestartCommand {
            supervisor: "systemd".to_owned(),
            command: "sudo systemctl restart plurxd".to_owned(),
        },
        RestartCommand {
            supervisor: "ansible".to_owned(),
            command: "ansible-playbook deploy.yml --limit <this-host>".to_owned(),
        },
    ]
}

impl From<Option<DbSnapshotMetricsSnapshot>> for SnapshotStatus {
    fn from(snapshot: Option<DbSnapshotMetricsSnapshot>) -> Self {
        match snapshot {
            Some(snapshot) => Self {
                available: true,
                db_bytes: None,
                build_ok_count: snapshot.build_ok.count,
                build_error_count: snapshot.build_error.count,
                install_ok_count: snapshot.install_ok.count,
                install_error_count: snapshot.install_error.count,
                last_build_outcome: snapshot
                    .last_build
                    .map(|outcome| if outcome.ok { "ok" } else { "error" }.to_owned()),
                last_build_unix_ms: snapshot
                    .last_build
                    .map(|outcome| outcome.observed_at_unix_ms),
                last_install_outcome: snapshot
                    .last_install
                    .map(|outcome| if outcome.ok { "ok" } else { "error" }.to_owned()),
                last_install_unix_ms: snapshot
                    .last_install
                    .map(|outcome| outcome.observed_at_unix_ms),
            },
            None => Self {
                available: false,
                db_bytes: None,
                build_ok_count: 0,
                build_error_count: 0,
                install_ok_count: 0,
                install_error_count: 0,
                last_build_outcome: None,
                last_build_unix_ms: None,
                last_install_outcome: None,
                last_install_unix_ms: None,
            },
        }
    }
}

#[derive(Clone, Debug)]
struct PeerStatusOutcome {
    node_id: String,
    state: ObservationState,
    status: Option<ClusterNodeOperationsStatus>,
    transport: Option<SnapshotTransportStatus>,
}

/// Five-second, node-owned projection of committed membership.
///
/// The operations request path cannot own a Store or Raft read. A dedicated
/// background loop refreshes this projection and an absent or expired sample
/// fails closed instead of silently inventing a roster.
#[derive(Clone, Default)]
pub(crate) struct MembershipStatusCache {
    inner: std::sync::Arc<tokio::sync::Mutex<Option<CachedMembershipStatus>>>,
}

#[derive(Clone)]
struct CachedMembershipStatus {
    stored_at: tokio::time::Instant,
    status: MembershipStatus,
}

enum MembershipStatusCacheRead {
    Fresh(Box<MembershipStatus>),
    Unavailable,
    Stale,
}

impl MembershipStatusCache {
    async fn fresh(&self) -> MembershipStatusCacheRead {
        let cache = self.inner.lock().await;
        let Some(cached) = cache.as_ref() else {
            return MembershipStatusCacheRead::Unavailable;
        };
        if tokio::time::Instant::now().saturating_duration_since(cached.stored_at)
            < PEER_STATUS_CACHE_TTL
        {
            MembershipStatusCacheRead::Fresh(Box::new(cached.status.clone()))
        } else {
            MembershipStatusCacheRead::Stale
        }
    }

    async fn store(&self, status: MembershipStatus) {
        *self.inner.lock().await = Some(CachedMembershipStatus {
            stored_at: tokio::time::Instant::now(),
            status,
        });
    }
}

/// Five-second, node-owned cache for bounded peer status fan-out.
///
/// It caches only authenticated read results. It never creates Store work and
/// does not survive process restart.
#[derive(Clone, Default)]
pub(crate) struct PeerStatusCache {
    inner: std::sync::Arc<tokio::sync::Mutex<Option<CachedPeerStatuses>>>,
}

#[derive(Clone)]
struct CachedPeerStatuses {
    /// Conservative lower bound for when any result in this refresh could
    /// have been observed locally. Starting age at collection entry includes
    /// time an early response spends waiting for the other bounded probes.
    stored_at: tokio::time::Instant,
    statuses: BTreeMap<String, PeerStatusOutcome>,
}

enum PeerStatusCacheRead {
    Fresh(BTreeMap<String, PeerStatusOutcome>, Duration),
    Unavailable,
    Stale,
}

impl From<BTreeMap<String, PeerStatusOutcome>> for PeerStatusCacheRead {
    fn from(statuses: BTreeMap<String, PeerStatusOutcome>) -> Self {
        Self::Fresh(statuses, Duration::ZERO)
    }
}

impl PeerStatusCache {
    async fn fresh(&self) -> PeerStatusCacheRead {
        let cache = self.inner.lock().await;
        let Some(cached) = cache.as_ref() else {
            return PeerStatusCacheRead::Unavailable;
        };
        let age = tokio::time::Instant::now().saturating_duration_since(cached.stored_at);
        if age < PEER_STATUS_CACHE_TTL {
            PeerStatusCacheRead::Fresh(cached.statuses.clone(), age)
        } else {
            PeerStatusCacheRead::Stale
        }
    }

    #[cfg(test)]
    async fn store(&self, statuses: &BTreeMap<String, PeerStatusOutcome>) {
        self.store_observed_at(statuses, tokio::time::Instant::now())
            .await;
    }

    async fn store_observed_at(
        &self,
        statuses: &BTreeMap<String, PeerStatusOutcome>,
        observed_at: tokio::time::Instant,
    ) {
        *self.inner.lock().await = Some(CachedPeerStatuses {
            // A peer's Unix timestamp remains diagnostic metadata and never
            // participates in freshness. Use a local monotonic lower bound so
            // collection dwell cannot renew evidence or its active deadline.
            stored_at: observed_at,
            statuses: statuses.clone(),
        });
    }
}

async fn refresh_peer_status_cache_with<Directory, DirectoryFuture, Collect, CollectFuture>(
    cache: &PeerStatusCache,
    refresh_started: tokio::time::Instant,
    directory: Directory,
    collect: Collect,
) -> Result<(), String>
where
    Directory: FnOnce() -> DirectoryFuture,
    DirectoryFuture: std::future::Future<Output = Result<Vec<ActivityPeer>, String>>,
    Collect: FnOnce(Vec<ActivityPeer>, tokio::time::Instant) -> CollectFuture,
    CollectFuture: std::future::Future<Output = BTreeMap<String, PeerStatusOutcome>>,
{
    let directory_deadline = refresh_started + PEER_DIRECTORY_TIMEOUT;
    let peers = tokio::time::timeout_at(directory_deadline, directory())
        .await
        .map_err(|_| "peer directory timed out".to_owned())??;
    let collection_started = tokio::time::Instant::now();
    let deadline = deadline_after(AGGREGATE_TIMEOUT);
    let statuses = collect(peers, deadline).await;
    cache.store_observed_at(&statuses, collection_started).await;
    Ok(())
}

async fn refresh_membership_status_cache_with<Refresh, RefreshFuture>(
    cache: &MembershipStatusCache,
    refresh_started: tokio::time::Instant,
    refresh: Refresh,
) -> Result<(), String>
where
    Refresh: FnOnce() -> RefreshFuture,
    RefreshFuture: std::future::Future<Output = Result<MembershipStatus, String>>,
{
    let deadline = refresh_started + PEER_DIRECTORY_TIMEOUT;
    let status = tokio::time::timeout_at(deadline, refresh())
        .await
        .map_err(|_| "membership projection timed out".to_owned())??;
    cache.store(status).await;
    Ok(())
}

async fn refresh_cache_admin_revocation_capability_with<Refresh, RefreshFuture>(
    cache: &super::extract::CacheOnlyAdminProofCache,
    refresh_started: tokio::time::Instant,
    refresh: Refresh,
) -> Result<bool, String>
where
    Refresh: FnOnce() -> RefreshFuture,
    RefreshFuture: std::future::Future<Output = Result<bool, String>>,
{
    let deadline = refresh_started + PEER_DIRECTORY_TIMEOUT;
    let result = tokio::time::timeout_at(deadline, refresh()).await;
    match result {
        Ok(Ok(ready)) => {
            cache.set_cluster_revocation_capability_ready(ready);
            Ok(ready)
        }
        Ok(Err(error)) => {
            cache.set_cluster_revocation_capability_ready(false);
            Err(error)
        }
        Err(_) => {
            cache.set_cluster_revocation_capability_ready(false);
            Err("cache admin revocation capability projection timed out".to_owned())
        }
    }
}

fn age_transport_observations(transport: &mut SnapshotTransportStatus, elapsed_ms: u64) {
    transport.observations.retain_mut(|observation| {
        let source_phase = observation.phase;
        let source_sample_age_ms = observation.sample_age_ms;
        let source_deadline_remaining_ms = observation.active_deadline_remaining_ms;
        observation.sample_age_ms = source_sample_age_ms.saturating_add(elapsed_ms);
        for age_ms in [
            &mut observation.attempt_age_ms,
            &mut observation.last_acknowledgement_age_ms,
            &mut observation.last_local_receive_age_ms,
        ] {
            if let Some(age_ms) = age_ms.as_mut() {
                *age_ms = age_ms.saturating_add(elapsed_ms);
            }
        }
        let active_phase = matches!(
            source_phase,
            SnapshotTransportPhase::Connecting
                | SnapshotTransportPhase::Transferring
                | SnapshotTransportPhase::AwaitingAcknowledgement
                | SnapshotTransportPhase::Installing
                | SnapshotTransportPhase::Retrying
        );
        // Older peers serialize `None` for the producer's 30-second fallback
        // stall boundary. Derive that same boundary from producer sample age
        // so mixed-version cached evidence follows the current contract.
        let fallback_boundary = active_phase && source_deadline_remaining_ms.is_none();
        let effective_remaining_ms = source_deadline_remaining_ms.or_else(|| {
            fallback_boundary
                .then(|| TRANSPORT_FALLBACK_STALL_MS.saturating_sub(source_sample_age_ms))
        });
        let stalled_age_ms = if fallback_boundary {
            source_sample_age_ms
                .saturating_add(elapsed_ms)
                .saturating_sub(TRANSPORT_FALLBACK_STALL_MS)
        } else {
            effective_remaining_ms
                .map(|remaining_ms| elapsed_ms.saturating_sub(remaining_ms))
                .unwrap_or(0)
        };
        let deadline_still_active =
            effective_remaining_ms.is_some_and(|remaining_ms| remaining_ms > elapsed_ms);
        let deadline_crossed = active_phase
            && effective_remaining_ms.is_some_and(|remaining_ms| remaining_ms <= elapsed_ms);
        let projected_stalled_retention =
            deadline_crossed && stalled_age_ms <= TRANSPORT_OBSERVATION_TTL_MS;
        if active_phase {
            observation.active_deadline_remaining_ms =
                effective_remaining_ms.map(|remaining_ms| remaining_ms.saturating_sub(elapsed_ms));
        }
        if deadline_crossed {
            // A producer that observes its own deadline starts the stalled
            // retention age at that boundary. Preserve the same wire meaning
            // when an aggregator's monotonic cache clock crosses it later.
            observation.sample_age_ms = stalled_age_ms;
            observation.phase = SnapshotTransportPhase::Stalled;
            observation.last_error_category = Some("snapshot_stalled".to_owned());
        }
        observation.sample_age_ms <= TRANSPORT_OBSERVATION_TTL_MS
            || deadline_still_active
            || projected_stalled_retention
    });
}

/// Refresh authenticated peer observations away from page renders and
/// Prometheus scrapes. The request path reads the five-second cache only.
pub(crate) async fn membership_status_cache_loop(
    state: AppState,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if !state.membership.is_replicated() {
        return;
    }
    loop {
        let refresh_started = tokio::time::Instant::now();
        let status_membership = state.membership.clone();
        let capability_membership = state.membership.clone();
        let (status_result, capability_result) = tokio::join!(
            refresh_membership_status_cache_with(
                &state.membership_status_cache,
                refresh_started,
                move || async move {
                    status_membership
                        .status()
                        .await
                        .map_err(|error| error.to_string())
                },
            ),
            refresh_cache_admin_revocation_capability_with(
                &state.cache_only_admin_proofs,
                refresh_started,
                move || async move {
                    capability_membership
                        .cache_admin_revocation_ready()
                        .await
                        .map_err(|error| error.to_string())
                },
            ),
        );
        if let Err(error) = status_result {
            tracing::warn!(%error, "could not refresh bounded cluster membership cache");
        }
        if let Err(error) = capability_result {
            tracing::warn!(%error, "could not refresh cache admin revocation capability");
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep_until(refresh_started + PEER_STATUS_REFRESH_INTERVAL) => {}
        }
    }
}

/// Refresh authenticated peer observations away from page renders and
/// Prometheus scrapes. The request path reads the five-second cache only.
pub(crate) async fn peer_status_cache_loop(
    state: AppState,
    shutdown: tokio_util::sync::CancellationToken,
) {
    loop {
        let refresh_started = tokio::time::Instant::now();
        let membership = state.membership.clone();
        let transport = PeerTransport::new(state.membership.clone());
        let replication = state.replication.clone();
        if let Err(error) = refresh_peer_status_cache_with(
            &state.peer_status_cache,
            refresh_started,
            move || async move {
                membership
                    .operations_peers()
                    .await
                    .map_err(|error| error.to_string())
            },
            move |peers, deadline| async move {
                collect_peer_statuses(peers, transport, replication, deadline).await
            },
        )
        .await
        {
            tracing::warn!(%error, "could not refresh bounded cluster operations cache");
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep_until(refresh_started + PEER_STATUS_REFRESH_INTERVAL) => {}
        }
    }
}

async fn collect_peer_statuses(
    peers: Vec<ActivityPeer>,
    transport: PeerTransport,
    replication: plurx_core::cluster::migration::status::ReplicationMonitor,
    deadline: tokio::time::Instant,
) -> BTreeMap<String, PeerStatusOutcome> {
    let public = move |peer: ActivityPeer, peer_deadline| {
        let transport = transport.clone();
        async move {
            let node_id = peer.node_id.clone();
            if !peer.reachable {
                PeerStatusOutcome {
                    node_id,
                    state: ObservationState::Unreachable,
                    status: None,
                    transport: None,
                }
            } else if let Some(base) = peer.http_base {
                fetch_peer_status(&transport, &node_id, &base, peer_deadline).await
            } else {
                PeerStatusOutcome {
                    node_id,
                    state: ObservationState::Unreachable,
                    status: None,
                    transport: None,
                }
            }
        }
    };
    let private = move |peer_node_id| {
        let replication = replication.clone();
        async move { replication.peer_transport_status(peer_node_id).await }
    };
    collect_peer_statuses_with_sources(peers, deadline, public, private).await
}

async fn collect_peer_statuses_with_sources<Public, PublicFuture, Private, PrivateFuture>(
    mut peers: Vec<ActivityPeer>,
    deadline: tokio::time::Instant,
    public: Public,
    private: Private,
) -> BTreeMap<String, PeerStatusOutcome>
where
    Public: Fn(ActivityPeer, tokio::time::Instant) -> PublicFuture + Clone,
    PublicFuture: std::future::Future<Output = PeerStatusOutcome>,
    Private: Fn(u64) -> PrivateFuture + Clone,
    PrivateFuture: std::future::Future<Output = Result<Option<SnapshotTransportStatus>, String>>,
{
    let omitted = peers.split_off(peers.len().min(MAX_OPERATIONS_PEERS));
    let mut outcomes: BTreeMap<_, _> = stream::iter(peers.into_iter().map(|peer| {
        let public = public.clone();
        let private = private.clone();
        async move {
            let node_id = peer.node_id.clone();
            let peer_node_id = peer.raft_id;
            let peer_deadline = deadline.min(tokio::time::Instant::now() + PEER_STATUS_TIMEOUT);
            let public_started = tokio::time::Instant::now();
            let public = async {
                let outcome =
                    tokio::time::timeout_at(peer_deadline, public(peer, peer_deadline)).await;
                (outcome, tokio::time::Instant::now())
            };
            let private_deadline =
                peer_deadline.min(tokio::time::Instant::now() + Duration::from_millis(900));
            let private_started = tokio::time::Instant::now();
            let private = async {
                let outcome =
                    tokio::time::timeout_at(private_deadline, private(peer_node_id)).await;
                (outcome, tokio::time::Instant::now())
            };
            let ((public_outcome, public_receipt), (private_transport, private_receipt)) =
                tokio::join!(public, private);
            let mut outcome = public_outcome.unwrap_or_else(|_| PeerStatusOutcome {
                node_id: node_id.clone(),
                state: ObservationState::TimedOut,
                status: None,
                transport: None,
            });
            if let Some(transport) = outcome
                .status
                .as_mut()
                .and_then(|status| status.transport.as_mut())
            {
                transport.local_request_started_at = Some(public_started);
                transport.local_receipt_at = Some(public_receipt);
            }
            outcome.transport = private_transport
                .ok()
                .and_then(Result::ok)
                .flatten()
                .filter(|status| status.observing_node_id == peer_node_id)
                .map(|mut status| {
                    status.local_request_started_at = Some(private_started);
                    status.local_receipt_at = Some(private_receipt);
                    status
                });
            (node_id, outcome)
        }
    }))
    .buffer_unordered(MAX_OPERATIONS_PEERS)
    .collect()
    .await;
    outcomes.extend(omitted.into_iter().map(|peer| {
        let node_id = peer.node_id;
        (
            node_id.clone(),
            PeerStatusOutcome {
                node_id,
                state: ObservationState::PeerLimit,
                status: None,
                transport: None,
            },
        )
    }));
    outcomes
}

#[cfg(test)]
async fn collect_peer_statuses_with<F, Fut>(
    mut peers: Vec<ActivityPeer>,
    deadline: tokio::time::Instant,
    fetch: F,
) -> BTreeMap<String, PeerStatusOutcome>
where
    F: Fn(ActivityPeer, tokio::time::Instant) -> Fut + Clone,
    Fut: std::future::Future<Output = PeerStatusOutcome>,
{
    let omitted = peers.split_off(peers.len().min(MAX_OPERATIONS_PEERS));
    let mut outcomes: BTreeMap<_, _> = stream::iter(peers.into_iter().map(|peer| {
        let fetch = fetch.clone();
        async move {
            let node_id = peer.node_id.clone();
            let peer_deadline = deadline.min(tokio::time::Instant::now() + PEER_STATUS_TIMEOUT);
            let outcome = tokio::time::timeout_at(peer_deadline, fetch(peer, peer_deadline))
                .await
                .unwrap_or_else(|_| PeerStatusOutcome {
                    node_id: node_id.clone(),
                    state: ObservationState::TimedOut,
                    status: None,
                    transport: None,
                });
            (node_id, outcome)
        }
    }))
    .buffer_unordered(MAX_OPERATIONS_PEERS)
    .collect()
    .await;
    outcomes.extend(omitted.into_iter().map(|peer| {
        let node_id = peer.node_id;
        (
            node_id.clone(),
            PeerStatusOutcome {
                node_id,
                state: ObservationState::PeerLimit,
                status: None,
                transport: None,
            },
        )
    }));
    outcomes
}

async fn fetch_peer_status(
    transport: &PeerTransport,
    expected_node_id: &str,
    base: &str,
    deadline: tokio::time::Instant,
) -> PeerStatusOutcome {
    let response = transport
        .request(
            expected_node_id,
            base,
            reqwest::Method::GET,
            INTERNAL_PATH,
            Vec::new(),
            deadline,
            MAX_RESPONSE_BYTES,
            PeerAuthMode::ExactRequest,
        )
        .await;
    let (state, status) = match response {
        Ok(response) if response.status.is_success() => {
            match serde_json::from_slice::<ClusterNodeOperationsStatus>(&response.body) {
                Ok(status) if status.node_id == expected_node_id => {
                    (ObservationState::Answered, Some(status))
                }
                Ok(_) => (ObservationState::IdentityMismatch, None),
                Err(_) => (ObservationState::InvalidResponse, None),
            }
        }
        // A non-success status is an answer, not silence. Collapsing it into
        // `Unreachable` printed "unreachable" on a node that had just replied,
        // which is how a refused proof read as a dead machine.
        Ok(response)
            if matches!(
                response.status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            ) =>
        {
            (ObservationState::Refused, None)
        }
        Ok(_) => (ObservationState::HttpError, None),
        Err(PeerTransportError::TimedOut) => (ObservationState::TimedOut, None),
        Err(PeerTransportError::InvalidResponse) => (ObservationState::InvalidResponse, None),
        Err(PeerTransportError::Unreachable) => (ObservationState::Unreachable, None),
    };
    PeerStatusOutcome {
        node_id: expected_node_id.to_owned(),
        state,
        status,
        transport: None,
    }
}

fn join_observations<Remote>(
    membership: &MembershipStatus,
    local_node_id: &str,
    mut local: ClusterNodeOperationsStatus,
    remote: Remote,
    aggregate_observed_ms: u64,
) -> Vec<ClusterNodeObservation>
where
    Remote: Into<PeerStatusCacheRead>,
{
    let (mut remote, missing_state, missing_error_class, remote_observation_age_ms) =
        match remote.into() {
            PeerStatusCacheRead::Fresh(statuses, cache_age) => (
                statuses,
                ObservationState::Unavailable,
                "not_observed",
                u64::try_from(cache_age.as_millis()).unwrap_or(u64::MAX),
            ),
            PeerStatusCacheRead::Unavailable => (
                BTreeMap::new(),
                ObservationState::Unavailable,
                "cache_unavailable",
                0,
            ),
            PeerStatusCacheRead::Stale => (
                BTreeMap::new(),
                ObservationState::Unavailable,
                "cache_stale",
                0,
            ),
        };
    let mut rows = Vec::with_capacity(membership.nodes.len());
    for member in &membership.nodes {
        let (mut state, mut status, transport, mut error_class) = if member.node_id == local_node_id
        {
            local.hostname = member.hostname.clone();
            let transport = local
                .transport
                .take()
                .filter(|_| local.raft_id == Some(member.raft_id))
                .and_then(|transport| sanitize_transport(transport, member.raft_id, 0));
            (
                ObservationState::Answered,
                Some(local.clone()),
                transport,
                None,
            )
        } else if let Some(mut outcome) = remote.remove(&member.node_id) {
            let error = (outcome.state != ObservationState::Answered)
                .then(|| observation_error_class(outcome.state).to_owned());
            debug_assert_eq!(outcome.node_id, member.node_id);
            let fallback_transport = outcome.status.as_mut().and_then(|status| {
                let transport = status.transport.take();
                (status.raft_id == Some(member.raft_id))
                    .then_some(transport)
                    .flatten()
            });
            let private_transport = outcome.transport.and_then(|transport| {
                sanitize_transport(transport, member.raft_id, remote_observation_age_ms)
            });
            let fallback_transport = fallback_transport.and_then(|transport| {
                sanitize_transport(transport, member.raft_id, remote_observation_age_ms)
            });
            (
                outcome.state,
                outcome.status,
                private_transport.or(fallback_transport),
                error,
            )
        } else {
            (
                missing_state,
                None,
                None,
                Some(missing_error_class.to_owned()),
            )
        };
        if status
            .as_ref()
            .is_some_and(|status| status.raft_id != Some(member.raft_id))
        {
            state = ObservationState::IdentityMismatch;
            status = None;
            error_class = Some("raft_identity_mismatch".to_owned());
        }
        let sample_age_ms = status.as_ref().map(|status| {
            if member.node_id == local_node_id {
                aggregate_observed_ms.saturating_sub(status.observed_at_unix_ms)
            } else {
                // The response was produced on another machine. Its Unix
                // timestamp is useful metadata but cannot establish age:
                // clock skew would make current evidence look stale or
                // future-dated evidence look permanently fresh.
                remote_observation_age_ms
            }
        });
        if sample_age_ms.is_some_and(|age| age > MAX_FRESH_AGE_MS) {
            state = ObservationState::InvalidResponse;
            error_class = Some("stale_peer_sample".to_owned());
        }
        rows.push(ClusterNodeObservation {
            membership: member.clone(),
            observation: state,
            sample_age_ms,
            error_class,
            status,
            transport,
        });
    }
    rows
}

fn sanitize_transport(
    mut transport: SnapshotTransportStatus,
    expected_observer: u64,
    fallback_elapsed_ms: u64,
) -> Option<SnapshotTransportStatus> {
    if transport.observing_node_id != expected_observer {
        return None;
    }
    transport
        .observations
        .retain(|observation| observation.observing_node_id == expected_observer);
    let now = tokio::time::Instant::now();
    let (locally_elapsed_ms, age_uncertainty_ms) = transport
        .local_request_started_at
        .zip(transport.local_receipt_at)
        .map(|(request_started, receipt)| {
            let elapsed = u64::try_from(now.saturating_duration_since(request_started).as_millis())
                .unwrap_or(u64::MAX);
            let uncertainty = u64::try_from(
                receipt
                    .saturating_duration_since(request_started)
                    .as_millis(),
            )
            .unwrap_or(u64::MAX);
            (elapsed, uncertainty)
        })
        .unwrap_or((fallback_elapsed_ms, 0));
    age_transport_observations(&mut transport, locally_elapsed_ms);
    for observation in &mut transport.observations {
        observation.age_uncertainty_ms = age_uncertainty_ms;
    }
    Some(transport)
}

fn rollout_verdict(
    membership: &MembershipStatus,
    observations: &[ClusterNodeObservation],
) -> RolloutVerdict {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    let voters = observations
        .iter()
        .filter(|row| row.membership.is_voter)
        .collect::<Vec<_>>();
    let majority = voters.len() / 2 + 1;

    for row in observations {
        if row.membership.removal_pending {
            blocker(
                &mut blockers,
                "removal_pending",
                "a committed node removal is still pending",
                Some(&row.membership.node_id),
            );
        }
    }
    for row in &voters {
        if row.observation != ObservationState::Answered {
            blocker(
                &mut blockers,
                "voter_not_observed",
                "a voter did not answer the direct authenticated status probe",
                Some(&row.membership.node_id),
            );
            continue;
        }
        let Some(status) = row.status.as_ref() else {
            continue;
        };
        if !status.process.live {
            blocker(
                &mut blockers,
                "process_not_live",
                "a voter did not report a live process",
                Some(&row.membership.node_id),
            );
        }
        if !status.serving.ready {
            blocker(
                &mut blockers,
                "voter_not_ready",
                "a voter is fenced from serving new work",
                Some(&row.membership.node_id),
            );
        }
        if status.media.new_admissions_blocked {
            blocker(
                &mut blockers,
                "restart_preparation_active",
                "a voter is already preparing for restart; do not prepare or restart another voter",
                Some(&row.membership.node_id),
            );
        }
        if !status.raft.sample_valid || !status.raft.watermark_valid {
            blocker(
                &mut blockers,
                "raft_sample_stale",
                "a voter lacks a fresh local Raft and quorum-watermark sample",
                Some(&row.membership.node_id),
            );
        }
        if status.raft.apply_lag_entries != Some(0) {
            blocker(
                &mut blockers,
                "apply_lag_not_zero",
                "a voter's current apply lag is stale, unknown, or non-zero",
                Some(&row.membership.node_id),
            );
        }
        if !wal_is_healthy(&status.wal) {
            blocker(
                &mut blockers,
                "wal_not_healthy",
                "a voter's live WAL is unavailable, not open, not durable, or reports an error",
                Some(&row.membership.node_id),
            );
        }
    }

    if voters.len().saturating_sub(1) < majority {
        blocker(
            &mut blockers,
            "majority_not_preserved",
            "removing one voter would leave fewer voters than the committed majority",
            None,
        );
    }

    let answered = voters
        .iter()
        .filter_map(|row| row.status.as_ref())
        .collect::<Vec<_>>();
    let terms = answered
        .iter()
        .filter_map(|status| status.raft.current_term)
        .collect::<BTreeSet<_>>();
    let leaders = answered
        .iter()
        .filter_map(|status| status.raft.leader_id)
        .collect::<BTreeSet<_>>();
    if terms.len() != 1 || leaders.len() != 1 || answered.len() != voters.len() {
        blocker(
            &mut blockers,
            "leader_term_disagreement",
            "voters do not agree on one current term and known leader",
            None,
        );
    }

    let protocol_min = answered
        .iter()
        .map(|status| status.protocol_min)
        .max()
        .unwrap_or(i64::MAX);
    let protocol_max = answered
        .iter()
        .map(|status| status.protocol_max)
        .min()
        .unwrap_or(i64::MIN);
    if answered.len() != voters.len()
        || protocol_min > protocol_max
        || membership.protocol.active_min < protocol_min
        || membership.protocol.active_max > protocol_max
    {
        blocker(
            &mut blockers,
            "protocol_incompatible",
            "the observed voter binaries do not share the cluster's active protocol range",
            None,
        );
    }

    let builds = answered
        .iter()
        .map(|status| status.build.as_str())
        .collect::<BTreeSet<_>>();
    if builds.len() > 1 {
        warning(
            &mut warnings,
            "mixed_builds",
            "voters are directly observed on mixed builds; this is expected only during a rollout",
            None,
        );
    }

    let known_leader = leaders.iter().next().copied();
    let candidate_is_eligible = |row: &&ClusterNodeObservation| {
        let Some(status) = row.status.as_ref() else {
            return false;
        };
        status.media.local_active_sessions == 0
            && status.media.drained
            && known_leader != Some(row.membership.raft_id)
            && status.serving.ready
            && wal_is_healthy(&status.wal)
    };
    // Select from committed-roster order, never from the identity of the node
    // serving this aggregate. Every observer with the same evidence must name
    // the same candidate, or two operators could concurrently prepare two
    // different voters. The mutation still has to be sent directly to the
    // selected node; the aggregator never proxies it.
    let candidate = voters.iter().copied().find(candidate_is_eligible);
    if candidate.is_none() {
        blocker(
            &mut blockers,
            "no_drained_non_leader_candidate",
            "no directly observed, ready, drained non-leader voter is available",
            None,
        );
    }

    RolloutVerdict {
        safe_to_restart_one: blockers.is_empty(),
        candidate_node_id: blockers
            .is_empty()
            .then(|| candidate.map(|row| row.membership.node_id.clone()))
            .flatten(),
        blockers,
        warnings,
    }
}

/// Direct, target-specific proof for starting reversible maintenance.
///
/// This deliberately differs from the rollout verdict. A rollout candidate
/// must already be a drained non-leader voter because the next operator action
/// may be an immediate process stop. Maintenance begins by fencing admissions;
/// it therefore has to admit a busy target so existing work can drain, and it
/// has to admit the leader so the core can hand leadership off before the
/// durable maintenance fence is committed. A stable learner has no vote to
/// subtract, but still needs a healthy direct process and a healthy voter set.
pub(crate) fn maintenance_entry_verdicts(
    membership: &MembershipStatus,
    observations: &[ClusterNodeObservation],
) -> Vec<MaintenanceEntryVerdict> {
    let voters = observations
        .iter()
        .filter(|row| row.membership.is_voter)
        .collect::<Vec<_>>();
    let directly_live_voters = voters
        .iter()
        .filter(|row| {
            row.observation == ObservationState::Answered
                && row
                    .status
                    .as_ref()
                    .is_some_and(|status| status.process.live)
        })
        .count();
    let terms = voters
        .iter()
        .filter_map(|row| row.status.as_ref()?.raft.current_term)
        .collect::<BTreeSet<_>>();
    let leaders = voters
        .iter()
        .filter_map(|row| row.status.as_ref()?.raft.leader_id)
        .collect::<BTreeSet<_>>();
    let protocol_min = voters
        .iter()
        .filter_map(|row| row.status.as_ref().map(|status| status.protocol_min))
        .max()
        .unwrap_or(i64::MAX);
    let protocol_max = voters
        .iter()
        .filter_map(|row| row.status.as_ref().map(|status| status.protocol_max))
        .min()
        .unwrap_or(i64::MIN);

    observations
        .iter()
        .map(|target| {
            let mut blockers = Vec::new();
            if membership.recovery.required || !membership.recovery.quorum_available {
                blocker(
                    &mut blockers,
                    "maintenance_quorum_unavailable",
                    "the voter majority and an elected leader must be directly healthy before maintenance begins",
                    None,
                );
            }
            for row in observations {
                if row.membership.removal_pending {
                    blocker(
                        &mut blockers,
                        "membership_lifecycle_pending",
                        "a committed node removal is still pending",
                        Some(&row.membership.node_id),
                    );
                }
                if row.membership.maintenance {
                    blocker(
                        &mut blockers,
                        "maintenance_already_active",
                        "another durable maintenance fence is already active",
                        Some(&row.membership.node_id),
                    );
                }
            }
            for row in &voters {
                if row.observation != ObservationState::Answered {
                    blocker(
                        &mut blockers,
                        "voter_not_observed",
                        "a voter did not answer the direct authenticated status probe",
                        Some(&row.membership.node_id),
                    );
                    continue;
                }
                let Some(status) = row.status.as_ref() else {
                    continue;
                };
                if !status.process.live {
                    blocker(
                        &mut blockers,
                        "process_not_live",
                        "a voter did not report a live process",
                        Some(&row.membership.node_id),
                    );
                }
                if !status.serving.ready {
                    blocker(
                        &mut blockers,
                        "voter_not_ready",
                        "a voter is already fenced from serving new work",
                        Some(&row.membership.node_id),
                    );
                }
                if status.media.new_admissions_blocked {
                    blocker(
                        &mut blockers,
                        "restart_preparation_active",
                        "a node is already preparing for restart or maintenance",
                        Some(&row.membership.node_id),
                    );
                }
                if !status.raft.sample_valid || !status.raft.watermark_valid {
                    blocker(
                        &mut blockers,
                        "raft_sample_stale",
                        "a voter lacks a fresh local Raft and quorum-watermark sample",
                        Some(&row.membership.node_id),
                    );
                }
                if status.raft.apply_lag_entries != Some(0) {
                    blocker(
                        &mut blockers,
                        "apply_lag_not_zero",
                        "a voter's current apply lag is stale, unknown, or non-zero",
                        Some(&row.membership.node_id),
                    );
                }
                if !wal_is_healthy(&status.wal) {
                    blocker(
                        &mut blockers,
                        "wal_not_healthy",
                        "a voter's live WAL is unavailable, not open, not durable, or reports an error",
                        Some(&row.membership.node_id),
                    );
                }
            }
            if terms.len() != 1 || leaders.len() != 1 || voters.len() != directly_live_voters {
                blocker(
                    &mut blockers,
                    "leader_term_disagreement",
                    "voters do not agree on one current term and known leader",
                    None,
                );
            }
            if voters.len() != directly_live_voters
                || protocol_min > protocol_max
                || membership.protocol.active_min < protocol_min
                || membership.protocol.active_max > protocol_max
            {
                blocker(
                    &mut blockers,
                    "protocol_incompatible",
                    "the observed voter binaries do not share the cluster's active protocol range",
                    None,
                );
            }

            if target.membership.role == NodeRole::Voter && !target.membership.is_voter {
                blocker(
                    &mut blockers,
                    "membership_lifecycle_pending",
                    "a voter is still joining and cannot begin maintenance",
                    Some(&target.membership.node_id),
                );
            }
            if target.observation != ObservationState::Answered {
                blocker(
                    &mut blockers,
                    "target_not_observed",
                    "the target did not answer its direct authenticated status probe",
                    Some(&target.membership.node_id),
                );
            } else if let Some(status) = target.status.as_ref() {
                if !status.process.live || !status.serving.ready {
                    blocker(
                        &mut blockers,
                        "target_not_ready",
                        "the target process is not live and ready to fence new work",
                        Some(&target.membership.node_id),
                    );
                }
                if status.media.new_admissions_blocked {
                    blocker(
                        &mut blockers,
                        "restart_preparation_active",
                        "the target is already preparing for restart or maintenance",
                        Some(&target.membership.node_id),
                    );
                }
                if !target.membership.is_voter {
                    if !status.raft.sample_valid || status.raft.apply_lag_entries != Some(0) {
                        blocker(
                            &mut blockers,
                            "learner_not_caught_up",
                            "the learner needs a fresh zero-lag Raft sample before maintenance",
                            Some(&target.membership.node_id),
                        );
                    }
                    if !wal_is_healthy(&status.wal) {
                        blocker(
                            &mut blockers,
                            "wal_not_healthy",
                            "the learner's live WAL is unavailable, not open, not durable, or reports an error",
                            Some(&target.membership.node_id),
                        );
                    }
                    if status.protocol_min > membership.protocol.active_min
                        || status.protocol_max < membership.protocol.active_max
                    {
                        blocker(
                            &mut blockers,
                            "protocol_incompatible",
                            "the learner binary does not cover the cluster's active protocol range",
                            Some(&target.membership.node_id),
                        );
                    }
                }
            } else {
                blocker(
                    &mut blockers,
                    "target_status_missing",
                    "the target direct probe returned no process status",
                    Some(&target.membership.node_id),
                );
            }

            if target.membership.is_voter
                && membership.capacity.voting_nodes > 1
                && directly_live_voters.saturating_sub(1)
                    < membership.capacity.voting_quorum
            {
                blocker(
                    &mut blockers,
                    "maintenance_would_lose_quorum",
                    "fencing this voter would leave fewer directly live voters than quorum requires",
                    Some(&target.membership.node_id),
                );
            }

            MaintenanceEntryVerdict {
                node_id: target.membership.node_id.clone(),
                safe_to_enter: blockers.is_empty(),
                blockers,
            }
        })
        .collect()
}

fn wal_is_healthy(wal: &WalStatus) -> bool {
    wal.snapshot.as_ref().is_some_and(|snapshot| {
        wal.available
            && snapshot.state == WalRuntimeState::Open
            && snapshot.lock_owned
            && snapshot.last_error.is_none()
            && snapshot.last_log_index == snapshot.last_durable_index
    })
}

fn blocker(findings: &mut Vec<RolloutFinding>, code: &str, message: &str, node_id: Option<&str>) {
    findings.push(RolloutFinding {
        code: code.to_owned(),
        message: message.to_owned(),
        node_id: node_id.map(str::to_owned),
    });
}

fn warning(findings: &mut Vec<RolloutFinding>, code: &str, message: &str, node_id: Option<&str>) {
    blocker(findings, code, message, node_id);
}

fn observation_error_class(state: ObservationState) -> &'static str {
    match state {
        ObservationState::Answered => "none",
        ObservationState::Unreachable => "unreachable",
        ObservationState::TimedOut => "timeout",
        ObservationState::Refused => "refused",
        ObservationState::HttpError => "http_error",
        ObservationState::InvalidResponse => "invalid_response",
        ObservationState::IdentityMismatch => "identity_mismatch",
        ObservationState::Unavailable => "unavailable",
        ObservationState::PeerLimit => "peer_limit",
    }
}

fn redact_operator_text(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let sensitive = [
        "authorization",
        "bearer ",
        "token",
        "secret",
        "password",
        "signature",
        "api_key",
        "apikey",
        "username",
        "plxjoin:",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || value.contains('/')
        || value.contains('\\');
    if sensitive {
        return "[redacted potentially sensitive operator text]".to_owned();
    }
    value.chars().take(512).collect()
}

fn private_no_store_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        "private, no-store".parse().expect("static cache control"),
    );
    headers
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(unix)]
fn local_hostname() -> String {
    let mut buffer = [0_u8; 256];
    // SAFETY: the buffer is writable for exactly the length passed to libc.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return "unknown".to_owned();
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).trim().to_owned()
}

#[cfg(not(unix))]
fn local_hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::cluster::membership::{ClusterRecoveryStatus, NodeRole};

    fn fresh_cache_statuses(read: PeerStatusCacheRead) -> BTreeMap<String, PeerStatusOutcome> {
        match read {
            PeerStatusCacheRead::Fresh(statuses, _) => statuses,
            PeerStatusCacheRead::Unavailable => panic!("cache is unavailable"),
            PeerStatusCacheRead::Stale => panic!("cache is stale"),
        }
    }

    fn fresh_membership_status(read: MembershipStatusCacheRead) -> MembershipStatus {
        match read {
            MembershipStatusCacheRead::Fresh(status) => *status,
            MembershipStatusCacheRead::Unavailable => panic!("membership cache is unavailable"),
            MembershipStatusCacheRead::Stale => panic!("membership cache is stale"),
        }
    }

    fn cached_private_transport(
        read: PeerStatusCacheRead,
        node_id: &str,
        expected_observer: u64,
        _aggregate_observed_ms: u64,
    ) -> SnapshotTransportStatus {
        let (mut statuses, cache_age) = match read {
            PeerStatusCacheRead::Fresh(statuses, age) => (statuses, age),
            PeerStatusCacheRead::Unavailable => panic!("cache is unavailable"),
            PeerStatusCacheRead::Stale => panic!("cache is stale"),
        };
        let transport = statuses
            .remove(node_id)
            .and_then(|outcome| outcome.transport)
            .expect("cached private transport evidence");
        sanitize_transport(
            transport,
            expected_observer,
            u64::try_from(cache_age.as_millis()).unwrap_or(u64::MAX),
        )
        .expect("valid cached private transport evidence")
    }

    fn test_transport_status(
        observing_node_id: u64,
        sample_age_ms: u64,
        active_deadline_remaining_ms: Option<u64>,
    ) -> SnapshotTransportStatus {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "observing_node_id": observing_node_id,
            "observed_at_unix_ms": unix_ms(),
            "observations": [{
                "observing_node_id": observing_node_id,
                "peer_node_id": 1,
                "raft_group": "sqlite",
                "boot_id": "test-boot",
                "attempt_id": 7,
                "snapshot_id": "test-snapshot",
                "socket_epoch": 3,
                "direction": "inbound",
                "attempted_offset": 64,
                "acknowledged_offset": null,
                "locally_received_bytes": 64,
                "total_bytes": null,
                "attempt_age_ms": 100,
                "last_acknowledgement_age_ms": null,
                "last_local_receive_age_ms": 10,
                "active_deadline_remaining_ms": active_deadline_remaining_ms,
                "sample_age_ms": sample_age_ms,
                "phase": "transferring",
                "reconnect_count": 0,
                "retry_count": 0,
                "last_error_category": null,
                "operation_owns_work": false
            }]
        }))
        .expect("deserialize transport fixture through the public schema")
    }

    #[tokio::test(start_paused = true)]
    async fn peer_fanout_applies_the_one_second_per_peer_deadline() {
        let peers = (0..MAX_OPERATIONS_PEERS)
            .map(|index| ActivityPeer {
                node_id: format!("node-{index}"),
                raft_id: index as u64,
                http_base: Some(format!("http://node-{index}:8080")),
                reachable: true,
            })
            .collect();
        let started = tokio::time::Instant::now();
        let deadline = started + Duration::from_secs(2);
        let outcomes =
            collect_peer_statuses_with(peers, deadline, |peer, _peer_deadline| async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                PeerStatusOutcome {
                    node_id: peer.node_id,
                    state: ObservationState::Answered,
                    status: None,
                    transport: None,
                }
            })
            .await;

        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_secs(1)
        );
        assert_eq!(outcomes.len(), MAX_OPERATIONS_PEERS);
        assert!(outcomes
            .values()
            .all(|outcome| outcome.state == ObservationState::TimedOut));
    }

    #[tokio::test]
    async fn peer_fanout_never_exceeds_the_eight_peer_bound() {
        let peers = (0..(MAX_OPERATIONS_PEERS + 4))
            .map(|index| ActivityPeer {
                node_id: format!("node-{index}"),
                raft_id: index as u64,
                http_base: Some(format!("http://node-{index}:8080")),
                reachable: true,
            })
            .collect();
        let outcomes = collect_peer_statuses_with(
            peers,
            tokio::time::Instant::now() + Duration::from_secs(2),
            |peer, _peer_deadline| async move {
                PeerStatusOutcome {
                    node_id: peer.node_id,
                    state: ObservationState::Answered,
                    status: None,
                    transport: None,
                }
            },
        )
        .await;

        assert_eq!(outcomes.len(), MAX_OPERATIONS_PEERS + 4);
        assert_eq!(
            outcomes
                .values()
                .filter(|outcome| outcome.state == ObservationState::PeerLimit)
                .count(),
            4
        );
        assert_eq!(MAX_OPERATIONS_PEERS, 8);
    }

    #[tokio::test]
    async fn production_collector_labels_directory_overflow_without_probing_it() {
        let peers = (0..(MAX_OPERATIONS_PEERS + 4))
            .map(|index| ActivityPeer {
                node_id: format!("node-{index}"),
                raft_id: index as u64,
                http_base: Some(format!("http://node-{index}:32400")),
                reachable: true,
            })
            .collect();
        let public_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let private_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let outcomes = collect_peer_statuses_with_sources(
            peers,
            tokio::time::Instant::now() + AGGREGATE_TIMEOUT,
            {
                let public_calls = public_calls.clone();
                move |peer, _peer_deadline| {
                    public_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    async move {
                        PeerStatusOutcome {
                            node_id: peer.node_id,
                            state: ObservationState::Answered,
                            status: None,
                            transport: None,
                        }
                    }
                }
            },
            {
                let private_calls = private_calls.clone();
                move |_raft_id| {
                    private_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    async { Ok(None) }
                }
            },
        )
        .await;

        assert_eq!(outcomes.len(), MAX_OPERATIONS_PEERS + 4);
        assert_eq!(
            public_calls.load(std::sync::atomic::Ordering::Relaxed),
            MAX_OPERATIONS_PEERS
        );
        assert_eq!(
            private_calls.load(std::sync::atomic::Ordering::Relaxed),
            MAX_OPERATIONS_PEERS
        );
        assert_eq!(
            outcomes
                .values()
                .filter(|outcome| outcome.state == ObservationState::PeerLimit)
                .count(),
            4
        );
    }

    #[tokio::test(start_paused = true)]
    async fn peer_status_cache_is_fresh_for_five_seconds_then_expires() {
        let cache = PeerStatusCache::default();
        let statuses = BTreeMap::from([(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Answered,
                status: None,
                transport: None,
            },
        )]);
        cache.store(&statuses).await;
        assert_eq!(fresh_cache_statuses(cache.fresh().await).len(), 1);

        tokio::time::advance(PEER_STATUS_CACHE_TTL - Duration::from_millis(1)).await;
        assert!(matches!(
            cache.fresh().await,
            PeerStatusCacheRead::Fresh(_, _)
        ));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(matches!(cache.fresh().await, PeerStatusCacheRead::Stale));
    }

    #[tokio::test(start_paused = true)]
    async fn membership_status_cache_is_fresh_for_five_seconds_then_expires() {
        let cache = MembershipStatusCache::default();
        cache.store(test_membership("node-1", 3)).await;
        assert_eq!(fresh_membership_status(cache.fresh().await).nodes.len(), 3);

        tokio::time::advance(PEER_STATUS_CACHE_TTL - Duration::from_millis(1)).await;
        assert!(matches!(
            cache.fresh().await,
            MembershipStatusCacheRead::Fresh(_)
        ));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(matches!(
            cache.fresh().await,
            MembershipStatusCacheRead::Stale
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn unclustered_membership_cache_loop_exits_without_refresh_or_retry() {
        let (_app, state) = crate::http::tests::test_app_with_state();
        assert!(!state.membership.is_replicated());
        let cache = state.membership_status_cache.clone();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(membership_status_cache_loop(state, shutdown));

        tokio::task::yield_now().await;
        tokio::time::advance(PEER_STATUS_REFRESH_INTERVAL * 3).await;
        assert!(task.is_finished(), "unclustered loop kept retrying");
        task.await.expect("unclustered cache loop");
        assert!(matches!(
            cache.fresh().await,
            MembershipStatusCacheRead::Unavailable
        ));
    }

    #[test]
    fn automatic_activation_runs_outside_the_membership_projection() {
        let source = include_str!("cluster_operations.rs")
            .split_once("pub(crate) async fn membership_status_cache_loop")
            .expect("membership cache loop")
            .1
            .split_once("pub(crate) async fn peer_status_cache_loop")
            .expect("membership cache loop end")
            .0;
        assert!(source.contains("cache_admin_revocation_ready()"));
        assert!(!source.contains("activate_if_ready"));
        assert!(include_str!("../main.rs")
            .contains("internal_auth_revocation::cache_admin_revocation_activation_loop"));
    }

    #[tokio::test(start_paused = true)]
    async fn membership_projection_refresh_is_bounded_and_preserves_last_good_sample() {
        let cache = MembershipStatusCache::default();
        let refresh_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let refresh_started = tokio::time::Instant::now();
        refresh_membership_status_cache_with(&cache, refresh_started, {
            let refresh_calls = refresh_calls.clone();
            move || async move {
                refresh_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(test_membership("node-1", 3))
            }
        })
        .await
        .expect("initial membership projection");
        assert_eq!(refresh_calls.load(std::sync::atomic::Ordering::Relaxed), 1);

        let timed_out =
            refresh_membership_status_cache_with(&cache, tokio::time::Instant::now(), || async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(test_membership("node-1", 1))
            })
            .await;
        assert_eq!(
            timed_out.expect_err("slow membership projection must time out"),
            "membership projection timed out"
        );
        assert_eq!(fresh_membership_status(cache.fresh().await).nodes.len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn capability_refresh_error_and_rollback_clear_cache_only_admin_authority() {
        let cache = crate::http::extract::CacheOnlyAdminProofCache::new(true);
        let digest = plurx_core::auth::hash_token("capability-refresh-admin");
        let admin = plurx_core::domain::User {
            id: 77,
            username: "owner".to_owned(),
            password_hash: String::new(),
            is_admin: true,
            created_at: 1,
        };

        refresh_cache_admin_revocation_capability_with(
            &cache,
            tokio::time::Instant::now(),
            || async { Ok(true) },
        )
        .await
        .expect("all committed heartbeat proofs");
        let ticket = cache.authentication_ticket();
        cache.record_authenticated(ticket, digest.clone(), &admin);
        assert!(cache.authenticate(&digest));

        let ready = refresh_cache_admin_revocation_capability_with(
            &cache,
            tokio::time::Instant::now(),
            || async { Ok(false) },
        )
        .await
        .expect("an old-binary heartbeat is a valid closed verdict");
        assert!(!ready);
        assert!(!cache.authenticate(&digest));

        refresh_cache_admin_revocation_capability_with(
            &cache,
            tokio::time::Instant::now(),
            || async { Ok(true) },
        )
        .await
        .expect("roll forward");
        let ticket = cache.authentication_ticket();
        cache.record_authenticated(ticket, digest.clone(), &admin);
        assert!(cache.authenticate(&digest));

        let timeout = refresh_cache_admin_revocation_capability_with(
            &cache,
            tokio::time::Instant::now(),
            || async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(true)
            },
        )
        .await
        .expect_err("a stale capability projection must close authority");
        assert!(timeout.contains("timed out"), "{timeout}");
        assert!(!cache.authenticate(&digest));
        assert!(cache.authentication_ticket().is_none());
    }

    #[tokio::test]
    async fn full_roster_projection_activates_without_a_credential_mutation() {
        let activations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_activations = activations.clone();
        let shutdown = tokio_util::sync::CancellationToken::new();
        super::super::internal_auth_revocation::cache_admin_revocation_activation_loop_with(
            shutdown,
            move || {
                observed_activations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async {
                    Ok(
                        super::super::internal_auth_revocation::CacheAdminRevocationActivation::Complete,
                    )
                }
            },
        )
        .await;
        assert_eq!(activations.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_activation_stops_the_one_time_worker() {
        let shutdown = tokio_util::sync::CancellationToken::new();
        let activations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_activations = activations.clone();
        super::super::internal_auth_revocation::cache_admin_revocation_activation_loop_with(
            shutdown,
            move || {
                observed_activations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async {
                    Ok(
                        super::super::internal_auth_revocation::CacheAdminRevocationActivation::Complete,
                    )
                }
            },
        )
        .await;
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(activations.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn stuck_automatic_activation_does_not_block_membership_refresh_or_shutdown() {
        let shutdown = tokio_util::sync::CancellationToken::new();
        let activation_shutdown = shutdown.clone();
        let activations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_activations = activations.clone();
        let activation =
            tokio::spawn(
                super::super::internal_auth_revocation::cache_admin_revocation_activation_loop_with(
                    activation_shutdown,
                    move || {
                        observed_activations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        std::future::pending::<Result<
                        super::super::internal_auth_revocation::CacheAdminRevocationActivation,
                        String,
                    >>()
                    },
                ),
            );
        tokio::task::yield_now().await;
        assert_eq!(activations.load(std::sync::atomic::Ordering::Relaxed), 1);

        let cache = MembershipStatusCache::default();
        for node_count in [1, 2, 3] {
            refresh_membership_status_cache_with(
                &cache,
                tokio::time::Instant::now(),
                || async move { Ok(test_membership("node-1", node_count)) },
            )
            .await
            .expect("membership projection remains independently refreshable");
            tokio::time::advance(PEER_STATUS_REFRESH_INTERVAL).await;
            assert_eq!(
                fresh_membership_status(cache.fresh().await).nodes.len(),
                node_count
            );
        }

        shutdown.cancel();
        tokio::task::yield_now().await;
        activation.await.expect("activation loop stops on shutdown");
    }

    #[tokio::test(start_paused = true)]
    async fn full_refresh_cycle_keeps_cache_fresh_and_ages_transport_from_local_cache_time() {
        assert!(
            PEER_STATUS_REFRESH_INTERVAL + PEER_DIRECTORY_TIMEOUT + PEER_STATUS_TIMEOUT
                < PEER_STATUS_CACHE_TTL,
            "the refresh schedule needs strict overhead beyond directory and peer deadlines"
        );
        let cache = PeerStatusCache::default();
        cache.store(&BTreeMap::new()).await;
        tokio::time::advance(PEER_STATUS_REFRESH_INTERVAL).await;

        let source_observed_ms = 1_000_000;
        let mut private_transport = test_transport_status(2, 0, Some(10_000));
        private_transport.observed_at_unix_ms = source_observed_ms;
        let refresh_started = tokio::time::Instant::now();
        refresh_peer_status_cache_with(
            &cache,
            refresh_started,
            || async {
                tokio::time::sleep(PEER_DIRECTORY_TIMEOUT - Duration::from_millis(1)).await;
                Ok(vec![ActivityPeer {
                    node_id: "node-2".to_owned(),
                    raft_id: 2,
                    http_base: Some("http://blackhole.invalid:32400".to_owned()),
                    reachable: true,
                }])
            },
            move |peers, deadline| {
                let private_transport = private_transport.clone();
                async move {
                    collect_peer_statuses_with_sources(
                        peers,
                        deadline,
                        |_peer, _deadline| async move {
                            std::future::pending::<PeerStatusOutcome>().await
                        },
                        move |_raft_id| {
                            let private_transport = private_transport.clone();
                            async move { Ok(Some(private_transport)) }
                        },
                    )
                    .await
                }
            },
        )
        .await
        .expect("bounded refresh");
        assert_eq!(
            tokio::time::Instant::now() - refresh_started,
            PEER_DIRECTORY_TIMEOUT - Duration::from_millis(1) + PEER_STATUS_TIMEOUT
        );

        let elapsed = PEER_STATUS_CACHE_TTL
            - PEER_STATUS_REFRESH_INTERVAL
            - PEER_DIRECTORY_TIMEOUT
            - PEER_STATUS_TIMEOUT
            + Duration::from_millis(1);
        tokio::time::advance(elapsed).await;
        let projected = cached_private_transport(
            cache.fresh().await,
            "node-2",
            2,
            source_observed_ms + PEER_STATUS_CACHE_TTL.as_millis() as u64,
        );
        let observation = &projected.observations[0];
        assert_eq!(observation.sample_age_ms, 1_501);
        assert_eq!(observation.active_deadline_remaining_ms, Some(8_499));
    }

    #[tokio::test(start_paused = true)]
    async fn absent_and_expired_cache_are_unavailable_never_peer_limited() {
        let membership = test_membership("node-1", 2);
        let cache = PeerStatusCache::default();
        let aggregate_observed_ms = unix_ms();
        let rows = join_observations(
            &membership,
            "node-1",
            test_local_status("node-1", Some(1)),
            cache.fresh().await,
            aggregate_observed_ms,
        );
        assert_eq!(rows[1].observation, ObservationState::Unavailable);
        assert_eq!(rows[1].error_class.as_deref(), Some("cache_unavailable"));

        cache.store(&BTreeMap::new()).await;
        tokio::time::advance(PEER_STATUS_CACHE_TTL).await;
        let rows = join_observations(
            &membership,
            "node-1",
            test_local_status("node-1", Some(1)),
            cache.fresh().await,
            aggregate_observed_ms,
        );
        assert_eq!(rows[1].observation, ObservationState::Unavailable);
        assert_eq!(rows[1].error_class.as_deref(), Some("cache_stale"));
        assert!(rows
            .iter()
            .all(|row| row.observation != ObservationState::PeerLimit));
    }

    #[tokio::test(start_paused = true)]
    async fn long_install_crosses_to_stalled_then_expires_after_the_post_deadline_window() {
        let source = test_transport_status(2, 301_000, Some(4_000));

        let mut at_deadline = source.clone();
        age_transport_observations(&mut at_deadline, 4_000);
        let observation = &at_deadline.observations[0];
        assert_eq!(observation.phase, SnapshotTransportPhase::Stalled);
        assert_eq!(observation.active_deadline_remaining_ms, Some(0));
        assert_eq!(observation.sample_age_ms, 0);
        assert_eq!(
            observation.last_error_category.as_deref(),
            Some("snapshot_stalled")
        );

        let mut just_before_expiry = source.clone();
        age_transport_observations(
            &mut just_before_expiry,
            4_000 + TRANSPORT_OBSERVATION_TTL_MS - 1,
        );
        assert_eq!(
            just_before_expiry.observations[0].phase,
            SnapshotTransportPhase::Stalled
        );
        assert_eq!(
            just_before_expiry.observations[0].sample_age_ms,
            TRANSPORT_OBSERVATION_TTL_MS - 1
        );

        let mut expired = source;
        age_transport_observations(&mut expired, 4_000 + TRANSPORT_OBSERVATION_TTL_MS + 1);
        assert!(expired.observations.is_empty());

        let mut already_stalled =
            test_transport_status(2, TRANSPORT_OBSERVATION_TTL_MS + 1, Some(0));
        already_stalled.observations[0].phase = SnapshotTransportPhase::Stalled;
        age_transport_observations(&mut already_stalled, 0);
        assert!(
            already_stalled.observations.is_empty(),
            "an already-stalled source sample must not receive a fresh retention window"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cached_fresh_active_transport_stalls_exactly_when_deadline_reaches_zero() {
        let cache = PeerStatusCache::default();
        let source_observed_ms = 1_000_000;
        let mut transport = test_transport_status(2, 1_000, Some(1_000));
        transport.observed_at_unix_ms = source_observed_ms;
        let statuses = BTreeMap::from([(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Unreachable,
                status: None,
                transport: Some(transport),
            },
        )]);
        cache.store(&statuses).await;

        tokio::time::advance(Duration::from_millis(999)).await;
        let still_active =
            cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms + 999);
        let observation = &still_active.observations[0];
        assert_eq!(observation.phase, SnapshotTransportPhase::Transferring);
        assert_eq!(observation.active_deadline_remaining_ms, Some(1));
        assert_eq!(observation.last_error_category, None);

        tokio::time::advance(Duration::from_millis(1)).await;
        let expired =
            cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms + 1_000);
        let observation = &expired.observations[0];
        assert_eq!(observation.phase, SnapshotTransportPhase::Stalled);
        assert_eq!(observation.active_deadline_remaining_ms, Some(0));
        assert_eq!(observation.sample_age_ms, 0);
        assert_eq!(
            observation.last_error_category.as_deref(),
            Some("snapshot_stalled")
        );
    }

    #[test]
    fn cached_deadline_less_active_transport_uses_the_producer_fallback_boundary() {
        let source = test_transport_status(2, 1_000, None);

        let mut just_before_stall = source.clone();
        age_transport_observations(&mut just_before_stall, 28_999);
        let observation = &just_before_stall.observations[0];
        assert_eq!(observation.phase, SnapshotTransportPhase::Transferring);
        assert_eq!(observation.active_deadline_remaining_ms, Some(1));
        assert_eq!(observation.sample_age_ms, 29_999);

        let mut at_stall = source.clone();
        age_transport_observations(&mut at_stall, 29_000);
        let observation = &at_stall.observations[0];
        assert_eq!(observation.phase, SnapshotTransportPhase::Stalled);
        assert_eq!(observation.active_deadline_remaining_ms, Some(0));
        assert_eq!(observation.sample_age_ms, 0);

        let mut at_expiry = source.clone();
        age_transport_observations(&mut at_expiry, 29_000 + TRANSPORT_OBSERVATION_TTL_MS);
        assert_eq!(
            at_expiry.observations[0].phase,
            SnapshotTransportPhase::Stalled
        );
        assert_eq!(
            at_expiry.observations[0].sample_age_ms,
            TRANSPORT_OBSERVATION_TTL_MS
        );

        let mut expired = source;
        age_transport_observations(&mut expired, 29_000 + TRANSPORT_OBSERVATION_TTL_MS + 1);
        assert!(expired.observations.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn cached_terminal_transport_observation_expires_at_five_minutes() {
        let cache = PeerStatusCache::default();
        let source_observed_ms = 1_000_000;
        let mut transport = test_transport_status(2, 299_000, None);
        transport.observations[0].phase = SnapshotTransportPhase::Failed;
        transport.observed_at_unix_ms = source_observed_ms;
        let statuses = BTreeMap::from([(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Unreachable,
                status: None,
                transport: Some(transport),
            },
        )]);
        cache.store(&statuses).await;

        tokio::time::advance(Duration::from_millis(999)).await;
        let fresh =
            cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms + 999);
        assert_eq!(fresh.observations[0].sample_age_ms, 299_999);

        tokio::time::advance(Duration::from_millis(2)).await;
        let fresh =
            cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms + 1_001);
        assert!(fresh.observations.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn transport_projection_uses_local_monotonic_age_despite_remote_clock_skew() {
        let source_observed_ms = 1_000_000;
        let mut transport = test_transport_status(2, 1_000, Some(20_000));
        transport.observed_at_unix_ms = source_observed_ms;
        let projected =
            sanitize_transport(transport, 2, 6_000).expect("locally timed authenticated transport");
        let observation = &projected.observations[0];
        assert_eq!(observation.sample_age_ms, 7_000);
        assert_eq!(observation.attempt_age_ms, Some(6_100));
        assert_eq!(observation.last_local_receive_age_ms, Some(6_010));
        assert_eq!(observation.active_deadline_remaining_ms, Some(14_000));

        let mut replayed = test_transport_status(2, 301_000, Some(30_000));
        replayed.observed_at_unix_ms = source_observed_ms;
        let projected = sanitize_transport(replayed, 2, 40_000)
            .expect("local cache age crosses the deadline conservatively");
        assert_eq!(projected.observations.len(), 1);
        assert_eq!(
            projected.observations[0].phase,
            SnapshotTransportPhase::Stalled,
            "a crossed cached deadline remains visible only in its fixed post-deadline window"
        );
        assert_eq!(
            projected.observations[0].active_deadline_remaining_ms,
            Some(0)
        );

        let mut six_minutes_behind = test_transport_status(2, 0, Some(1_000));
        six_minutes_behind.observed_at_unix_ms = source_observed_ms - 6 * 60 * 1_000;
        let cache = PeerStatusCache::default();
        cache
            .store(&BTreeMap::from([(
                "node-2".to_owned(),
                PeerStatusOutcome {
                    node_id: "node-2".to_owned(),
                    state: ObservationState::Unreachable,
                    status: None,
                    transport: Some(six_minutes_behind),
                },
            )]))
            .await;
        let behind = cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms);
        assert_eq!(behind.observed_at_unix_ms, source_observed_ms - 360_000);
        assert_eq!(behind.observations[0].sample_age_ms, 0);

        let mut five_seconds_ahead = test_transport_status(2, 0, Some(1_000));
        five_seconds_ahead.observed_at_unix_ms = source_observed_ms + 5_001;
        cache
            .store(&BTreeMap::from([(
                "node-2".to_owned(),
                PeerStatusOutcome {
                    node_id: "node-2".to_owned(),
                    state: ObservationState::Unreachable,
                    status: None,
                    transport: Some(five_seconds_ahead),
                },
            )]))
            .await;
        let ahead = cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms);
        assert_eq!(ahead.observed_at_unix_ms, source_observed_ms + 5_001);
        assert_eq!(ahead.observations[0].sample_age_ms, 0);
    }

    #[test]
    fn peer_status_refresh_keeps_strict_cycle_overhead_margin() {
        assert_eq!(
            PEER_STATUS_CACHE_TTL
                - PEER_STATUS_REFRESH_INTERVAL
                - PEER_DIRECTORY_TIMEOUT
                - PEER_STATUS_TIMEOUT,
            Duration::from_millis(500)
        );
        assert!(PEER_STATUS_REFRESH_INTERVAL < PEER_STATUS_CACHE_TTL);
    }

    #[tokio::test]
    async fn aggregate_request_reads_only_node_owned_projections() {
        use axum::body::Body;
        use axum::http::Request;
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let (_unused_app, mut state) = crate::http::tests::test_app_with_state();
        state.node_id = "node-1".to_owned();
        let admin = state
            .store
            .create_user("cluster-admin", "unused-test-hash", true)
            .await
            .expect("create admin");
        let token = "cache-only-cluster-status-token";
        let token_digest = plurx_core::auth::hash_token(token);
        state
            .store
            .create_token(&token_digest, admin.id, Some("cluster-status-test"))
            .await
            .expect("create admin token");
        let app = crate::http::router(state.clone());

        // One ordinary Store-backed request publishes the bounded admin
        // proof. The two recovery routes below must use only that digest.
        let authenticated = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/me")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("authenticated request"),
            )
            .await
            .expect("authenticated response");
        assert_eq!(authenticated.status(), StatusCode::OK);
        assert!(state
            .store
            .delete_token(&token_digest)
            .await
            .expect("make Store proof unavailable"));
        assert!(state
            .store
            .user_for_token(&token_digest)
            .await
            .expect("read missing Store proof")
            .is_none());

        // The fixture is deliberately not a replicated cluster. A production
        // request that reaches MembershipManager::status cannot succeed, and
        // its current peer directory is empty. Seed only the two node-owned
        // projections that GET and support are allowed to consume.
        assert!(state.membership.status().await.is_err());
        assert!(state
            .membership
            .operations_peers()
            .await
            .expect("unclustered peer directory")
            .is_empty());
        state
            .membership_status_cache
            .store(test_membership("node-1", 2))
            .await;
        state
            .peer_status_cache
            .store(&BTreeMap::from([(
                "node-2".to_owned(),
                PeerStatusOutcome {
                    node_id: "node-2".to_owned(),
                    state: ObservationState::Answered,
                    status: Some(test_local_status("node-2", Some(2))),
                    transport: None,
                },
            )]))
            .await;

        let status_request = Request::builder()
            .uri("/api/v1/cluster/status")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .expect("cluster status request");
        let response = tokio::time::timeout(
            Duration::from_millis(250),
            app.clone().oneshot(status_request),
        )
        .await
        .expect("cache-only status request must not wait for Store")
        .expect("cluster status response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("private, no-store"))
        );
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("cluster status body")
            .to_bytes();
        let aggregate: ClusterOperationsAggregate =
            serde_json::from_slice(&bytes).expect("cluster status JSON");
        assert_eq!(aggregate.nodes.len(), 2);
        assert_eq!(aggregate.nodes[1].observation, ObservationState::Answered);

        let support_request = Request::builder()
            .uri("/api/v1/cluster/support-bundle")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .expect("support bundle request");
        let response = tokio::time::timeout(
            Duration::from_millis(250),
            app.clone().oneshot(support_request),
        )
        .await
        .expect("cache-only support request must not wait for Store")
        .expect("support bundle response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/zip"))
        );

        // Logout is a mutating route and therefore revalidates the bearer
        // against Store before it may fan out a cluster-wide proof fence.
        // Restore the same token only after the two cache-only reads have
        // proved that their request paths do not touch Store.
        state
            .store
            .create_token(&token_digest, admin.id, Some("cluster-status-test"))
            .await
            .expect("restore token for authenticated logout");
        let logout = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("logout request"),
            )
            .await
            .expect("logout response");
        assert_eq!(logout.status(), StatusCode::OK);
        let revoked = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/cluster/status")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("revoked cluster status request"),
            )
            .await
            .expect("revoked cluster status response");
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

        // Supplemental capability guard: both real handlers above execute the
        // production collector. Keep its narrow wiring explicit so Store and
        // peer transports cannot be slipped in beside the projection reads.
        let source = include_str!("cluster_operations.rs");
        let collector = source
            .split("pub(crate) async fn collect_aggregate(")
            .nth(1)
            .and_then(|tail| tail.split("/// Safety-changing operations").next())
            .expect("read-only collector source");
        assert!(collector.contains("state.membership_status_cache.fresh()"));
        assert!(collector.contains("state.peer_status_cache.fresh()"));
        assert!(!collector.contains("state.store"));
        assert!(!collector.contains("state.membership.status()"));
        assert!(!collector.contains("operations_peers"));
        assert!(!collector.contains("collect_peer_statuses"));

        let aggregate_handler = source
            .split("pub(crate) async fn aggregate(")
            .nth(1)
            .and_then(|tail| tail.split("/// Complete GET response boundary").next())
            .expect("aggregate handler source");
        assert!(aggregate_handler.contains("_admin: CacheOnlyAdminUser"));
        assert!(!aggregate_handler.contains("_admin: AdminUser"));
        assert!(aggregate_handler
            .contains("aggregate_response_with(|| collect_aggregate(&state)).await"));
        assert!(!aggregate_handler.contains("state.membership"));
        assert!(!aggregate_handler.contains("collect_peer_statuses"));

        let support_handler = source
            .split("pub(crate) async fn support_bundle(")
            .nth(1)
            .and_then(|tail| tail.split("/// Complete support response boundary").next())
            .expect("support handler source");
        assert!(support_handler.contains("_admin: CacheOnlyAdminUser"));
        assert!(!support_handler.contains("_admin: AdminUser"));
        assert!(support_handler.contains("|| collect_aggregate(&state)"));
        assert!(support_handler.contains("state.cluster_logs.tail(\"trace\", 200)"));
        assert!(!support_handler.contains("state.membership"));
        assert!(!support_handler.contains("collect_peer_statuses"));

        let extractor_source = include_str!("extract.rs");
        // Scoped to the impl block itself, not "everything until the tests":
        // a helper added between the two would silently join the slice and
        // make the counting assertion below fail for the wrong reason.
        let cache_extractor = extractor_source
            .split("impl FromRequestParts<AppState> for CacheOnlyAdminUser")
            .nth(1)
            .and_then(|tail| tail.split("\n}\n").next())
            .expect("cache-only admin extractor source");
        // The Store may be a fallback and may never be a prerequisite. The
        // cached answer has to be reached, and returned, before the source
        // mentions the Store at all — otherwise a wedged Store would delay the
        // one read whose purpose is to describe the wedge.
        let cache_answer = cache_extractor
            .find("cache_only_admin_proofs.authenticate(&digest)")
            .expect("cache-only answer");
        let early_return = cache_extractor
            .find("return Ok(Self);")
            .expect("cache-only early return");
        let store_read = cache_extractor
            .find("state.store.user_for_token(&digest)")
            .expect("bounded Store fallback");
        assert!(cache_answer < early_return);
        assert!(early_return < store_read);
        // The publication ticket is captured before the Store read and not
        // after it. Taking it afterwards compiles, passes every test in the
        // suite, and reintroduces the stale-proof race the cache generation
        // exists to close: a proof derived from a pre-revocation Store read
        // would publish once the revocation's generation bump had settled.
        let ticket = cache_extractor
            .find("authentication_ticket()")
            .expect("publication ticket");
        assert!(ticket < store_read);
        // And the fallback is bounded, so a Store that never answers becomes a
        // named refusal rather than a hung request.
        assert!(cache_extractor.contains("CACHE_ONLY_ADMIN_STORE_FALLBACK_TIMEOUT,"));
        assert!(cache_extractor.contains("tokio::time::timeout("));
        // A caller the Store recognises is never told its credential is bad:
        // the only Unauthorized answers here are a request with no token at
        // all and a token the Store has no row for. A known non-admin gets
        // Forbidden, and an unanswerable Store gets the named 503.
        assert_eq!(cache_extractor.matches("ApiError::Unauthorized").count(), 2);
        assert!(cache_extractor.contains("Err(ApiError::Forbidden)"));
        assert!(cache_extractor.contains("cache_only_admin_unavailable()"));
    }

    #[tokio::test]
    async fn mutation_preflight_refreshes_roster_directory_and_bounded_peer_observations() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let membership_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let directory_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let public_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let private_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let peers = (2..=13)
            .map(|index| ActivityPeer {
                node_id: format!("node-{index}"),
                raft_id: index,
                http_base: Some(format!("http://node-{index}:32400")),
                reachable: true,
            })
            .collect::<Vec<_>>();
        let deadline = tokio::time::Instant::now() + AGGREGATE_TIMEOUT;

        let aggregate = collect_current_aggregate_with_sources(
            "node-1",
            deadline,
            {
                let membership_calls = membership_calls.clone();
                move || async move {
                    membership_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(test_membership("node-1", 13))
                }
            },
            {
                let directory_calls = directory_calls.clone();
                move || {
                    let peers = peers.clone();
                    async move {
                        directory_calls.fetch_add(1, Ordering::Relaxed);
                        Ok(peers)
                    }
                }
            },
            || async { test_local_status("node-1", Some(1)) },
            {
                let public_calls = public_calls.clone();
                let private_calls = private_calls.clone();
                move |peers, received_deadline| async move {
                    assert_eq!(received_deadline, deadline);
                    collect_peer_statuses_with_sources(
                        peers,
                        received_deadline,
                        move |peer, _peer_deadline| {
                            public_calls.fetch_add(1, Ordering::Relaxed);
                            async move {
                                PeerStatusOutcome {
                                    node_id: peer.node_id,
                                    state: ObservationState::Answered,
                                    status: None,
                                    transport: None,
                                }
                            }
                        },
                        move |_raft_id| {
                            private_calls.fetch_add(1, Ordering::Relaxed);
                            async { Ok(None) }
                        },
                    )
                    .await
                }
            },
        )
        .await
        .expect("fresh bounded mutation preflight");

        assert_eq!(membership_calls.load(Ordering::Relaxed), 1);
        assert_eq!(directory_calls.load(Ordering::Relaxed), 1);
        assert_eq!(public_calls.load(Ordering::Relaxed), MAX_OPERATIONS_PEERS);
        assert_eq!(private_calls.load(Ordering::Relaxed), MAX_OPERATIONS_PEERS);
        assert_eq!(aggregate.nodes.len(), 13);
        assert_eq!(
            aggregate
                .nodes
                .iter()
                .filter(|node| node.observation == ObservationState::PeerLimit)
                .count(),
            4
        );

        let source = include_str!("cluster_operations.rs");
        let acquisition = source
            .split_once("pub(crate) async fn acquire_planned_outage_preflight(")
            .expect("planned-outage acquisition")
            .1
            .split_once("#[cfg(test)]\nasync fn acquire_then_collect_preflight")
            .expect("planned-outage acquisition end")
            .0;
        let prepared_claim = acquisition
            .find("prepare_restart_preparation_claim")
            .expect("prepare exact claim before commit");
        let guard = acquisition
            .find("PlannedOutageLeaseGuard::new")
            .expect("guard exact prepared claim");
        let detached_owner = acquisition
            .find("run_planned_outage_task(async move")
            .expect("detach replicated acquisition owner");
        let replicated_commit = acquisition
            .find("commit_cluster_operation_lease(lease.claim())")
            .expect("commit the guard-owned exact claim");
        assert!(
            prepared_claim < guard && guard < detached_owner && detached_owner < replicated_commit
        );
        let exact_release = source
            .split_once("pub(crate) async fn release_replicated_claim")
            .expect("exact planned-outage release")
            .1
            .split_once("pub(crate) async fn release(mut self)")
            .expect("exact planned-outage release end")
            .0;
        let local_latch = exact_release
            .find("retain_local_fence()")
            .expect("synchronous unresolved-release latch");
        let replicated_release = exact_release
            .find("release_exact_claim_with_retry(&self.membership, lease, &self.shutdown)")
            .expect("bounded replicated exact release owner");
        assert!(local_latch < replicated_release);
        let release_retry = source
            .split_once("async fn release_exact_claim_with_retry(")
            .expect("exact-release retry owner")
            .1
            .split_once("async fn release_current_restart_with_retry(")
            .expect("exact-release retry owner end")
            .0;
        assert!(release_retry.contains("release_cluster_operation_lease(lease)"));
        assert!(release_retry.contains("wait_for_planned_outage_cleanup_retry"));
        let current_collector = source
            .split_once("pub(crate) async fn collect_current_aggregate(")
            .expect("current preflight collector")
            .1
            .split_once("async fn collect_current_aggregate_with_sources")
            .expect("current preflight collector end")
            .0;
        assert!(current_collector.contains("membership_for_status"));
        assert!(current_collector.contains(".status()"));
        assert!(current_collector.contains("membership_for_directory"));
        assert!(current_collector.contains(".operations_peers()"));
        assert!(current_collector.contains("PeerTransport::new"));
        assert!(current_collector.contains("collect_peer_statuses("));
        let restart = source
            .split_once("pub(crate) async fn prepare_restart(")
            .expect("restart handler")
            .1
            .split_once("fn restart_preparation_response(")
            .expect("restart handler end")
            .0;
        assert!(restart.contains("acquire_planned_outage_preflight("));
        assert!(restart.contains("PlannedOutageOperation::Restart"));
        assert!(restart.contains("lease.arm_local_fence(local_fence)"));
        assert!(restart.contains("run_serialized_planned_outage_task"));
        assert!(restart.contains("lease.release().await"));
        assert!(restart.contains("lease.disarm()"));
        let maintenance_source = include_str!("cluster.rs");
        let maintenance = maintenance_source
            .split_once("pub async fn enter_maintenance(")
            .expect("maintenance handler")
            .1
            .split_once("pub async fn exit_maintenance(")
            .expect("maintenance handler end")
            .0;
        assert!(maintenance.contains("acquire_planned_outage_preflight("));
        assert!(maintenance.contains("PlannedOutageOperation::Maintenance"));
        assert!(maintenance.contains("lease.arm_local_fence(local_fence)"));
        assert!(maintenance.contains("run_serialized_planned_outage_task"));
        assert!(maintenance.contains("reconcile_local_maintenance_commit()"));
        assert!(maintenance.contains("lease.release().await"));
        assert!(maintenance.contains("lease.disarm()"));
    }

    #[tokio::test(start_paused = true)]
    async fn mutation_preflight_ages_local_transport_across_peer_collection() {
        let deadline = tokio::time::Instant::now() + AGGREGATE_TIMEOUT;
        let aggregate = collect_current_aggregate_with_sources(
            "node-1",
            deadline,
            || async { Ok(test_membership("node-1", 1)) },
            || async { Ok(Vec::new()) },
            || async {
                let mut status = test_local_status("node-1", Some(1));
                status.transport = Some(test_transport_status(1, 0, Some(500)));
                status
            },
            |_peers, received_deadline| async move {
                assert_eq!(received_deadline, deadline);
                tokio::time::advance(Duration::from_secs(1)).await;
                BTreeMap::new()
            },
        )
        .await
        .expect("fresh mutation preflight");

        let observation = aggregate.nodes[0]
            .transport
            .as_ref()
            .and_then(|transport| transport.observations.first())
            .expect("local transport observation");
        assert_eq!(observation.phase, SnapshotTransportPhase::Stalled);
        assert_eq!(observation.active_deadline_remaining_ms, Some(0));
        assert_eq!(
            observation.sample_age_ms, 500,
            "stalled retention starts when the 500 ms deadline is crossed, not when collection starts"
        );
        assert_eq!(
            observation.last_error_category.as_deref(),
            Some("snapshot_stalled")
        );
    }

    async fn wait_for_local_planned_outage_cleanup(serving: &crate::serving_fence::ServingFence) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !serving.restart_drain_status(0).await.new_admissions_blocked {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("planned-outage Drop cleanup must clear the local fence");
    }

    #[tokio::test]
    async fn cancelled_acquisition_waiter_releases_only_after_ambiguous_outcome_is_known() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;

        let serving =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let local_fence = serving
            .begin_restart_preparation_until(unix_ms().saturating_add(60_000))
            .await
            .expect("restart fence token");
        let acquisition_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let publish_outcome = std::sync::Arc::new(tokio::sync::Notify::new());
        let waiter = tokio::spawn(run_planned_outage_task({
            let serving = serving.clone();
            let acquisition_started = acquisition_started.clone();
            let publish_outcome = publish_outcome.clone();
            async move {
                acquisition_started.notify_one();
                publish_outcome.notified().await;
                Ok(PlannedOutageLeaseGuard::local_fence_only_for_test(
                    serving,
                    local_fence,
                ))
            }
        }));
        acquisition_started.notified().await;

        waiter.abort();
        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
        assert!(
            serving.restart_drain_status(0).await.new_admissions_blocked,
            "cancellation must not guess the outcome or clean up before acquisition returns"
        );

        publish_outcome.notify_one();
        wait_for_local_planned_outage_cleanup(&serving).await;
    }

    #[tokio::test]
    async fn abort_during_drain_cancels_the_guard_owned_local_fence() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;

        let serving =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let local_fence = serving
            .begin_restart_preparation_until(unix_ms().saturating_add(60_000))
            .await
            .expect("restart fence token");
        let drain_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let drain = tokio::spawn({
            let serving = serving.clone();
            let drain_started = drain_started.clone();
            async move {
                let _guard =
                    PlannedOutageLeaseGuard::local_fence_only_for_test(serving, local_fence);
                drain_started.notify_one();
                std::future::pending::<()>().await;
            }
        });
        drain_started.notified().await;

        drain.abort();
        assert!(drain.await.expect_err("drain is cancelled").is_cancelled());
        wait_for_local_planned_outage_cleanup(&serving).await;
    }

    #[tokio::test]
    async fn cancelled_maintenance_waiter_does_not_cancel_the_owned_commit() {
        let commit_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let finish_commit = std::sync::Arc::new(tokio::sync::Notify::new());
        let committed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waiter = tokio::spawn(run_planned_outage_task({
            let commit_started = commit_started.clone();
            let finish_commit = finish_commit.clone();
            let committed = committed.clone();
            async move {
                commit_started.notify_one();
                finish_commit.notified().await;
                committed.store(true, std::sync::atomic::Ordering::Release);
                Ok::<_, ApiError>(())
            }
        }));
        commit_started.notified().await;

        waiter.abort();
        assert!(waiter
            .await
            .expect_err("waiter is cancelled")
            .is_cancelled());
        assert!(!committed.load(std::sync::atomic::Ordering::Acquire));
        finish_commit.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !committed.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached maintenance owner must finish after HTTP cancellation");
    }

    #[tokio::test]
    async fn cancelled_waiter_keeps_the_serialized_operation_gate_with_its_owner() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;

        let serving =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let first_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let finish_first = std::sync::Arc::new(tokio::sync::Notify::new());
        let first_finished = std::sync::Arc::new(tokio::sync::Notify::new());
        let second_started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let first_waiter = tokio::spawn({
            let serving = serving.clone();
            let first_started = first_started.clone();
            let finish_first = finish_first.clone();
            let first_finished = first_finished.clone();
            async move {
                run_serialized_planned_outage_task(&serving, async move {
                    first_started.notify_one();
                    finish_first.notified().await;
                    first_finished.notify_one();
                    Ok::<_, ApiError>(())
                })
                .await
            }
        });
        first_started.notified().await;
        first_waiter.abort();
        assert!(first_waiter
            .await
            .expect_err("HTTP waiter is cancelled")
            .is_cancelled());

        for _ in 0..1_024 {
            let second_started = second_started.clone();
            let error = run_serialized_planned_outage_task(&serving, async move {
                second_started.store(true, std::sync::atomic::Ordering::Release);
                Ok::<_, ApiError>(())
            })
            .await
            .expect_err("a contending request must fail fast");
            assert!(matches!(
                error,
                ApiError::Typed {
                    code: "planned_outage_operation_pending",
                    ..
                }
            ));
        }
        assert!(
            !second_started.load(std::sync::atomic::Ordering::Acquire),
            "a later cancel/prepare must neither queue nor cross the detached first owner"
        );

        finish_first.notify_one();
        first_finished.notified().await;
        let admitted_second = second_started.clone();
        run_serialized_planned_outage_task(&serving, async move {
            admitted_second.store(true, std::sync::atomic::Ordering::Release);
            Ok::<_, ApiError>(())
        })
        .await
        .expect("second serialized operation after owner completion");
        assert!(second_started.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn cleanup_retry_backoff_is_capped_and_shutdown_interruptible() {
        let mut delay = PLANNED_OUTAGE_CLEANUP_RETRY_INITIAL;
        for _ in 0..16 {
            delay = next_planned_outage_cleanup_delay(delay);
        }
        assert_eq!(delay, PLANNED_OUTAGE_CLEANUP_RETRY_MAX);

        let shutdown = tokio_util::sync::CancellationToken::new();
        shutdown.cancel();
        assert!(
            wait_for_planned_outage_cleanup_retry(&shutdown, Duration::from_secs(60))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn planned_outage_lease_blocks_membership_mutation_during_fresh_preflight() {
        let lifecycle_fence = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let mutation_crossed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let (lease, ()) = acquire_then_collect_preflight(
            {
                let lifecycle_fence = lifecycle_fence.clone();
                move || async move { Ok::<_, ApiError>(lifecycle_fence.lock_owned().await) }
            },
            {
                let lifecycle_fence = lifecycle_fence.clone();
                let mutation_crossed = mutation_crossed.clone();
                move || async move {
                    let mutation =
                        tokio::spawn(async move { lifecycle_fence.try_lock_owned().is_ok() });
                    mutation_crossed.store(
                        mutation.await.expect("membership mutation attempt"),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    Ok::<_, ApiError>(())
                }
            },
            drop,
        )
        .await
        .expect("lease-protected fresh preflight");

        assert!(
            !mutation_crossed.load(std::sync::atomic::Ordering::Relaxed),
            "a membership lifecycle mutation must not cross the preflight"
        );
        drop(lease);
        assert!(lifecycle_fence.try_lock_owned().is_ok());
    }

    #[tokio::test]
    async fn failed_fresh_preflight_releases_its_planned_outage_lease() {
        let lifecycle_fence = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let result = acquire_then_collect_preflight(
            {
                let lifecycle_fence = lifecycle_fence.clone();
                move || async move { Ok::<_, ApiError>(lifecycle_fence.lock_owned().await) }
            },
            || async {
                Err::<(), _>(ApiError::typed(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "test_preflight_failed",
                    "test preflight failed",
                ))
            },
            {
                let released = released.clone();
                move |lease| {
                    drop(lease);
                    released.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
        )
        .await;

        assert!(result.is_err());
        assert!(released.load(std::sync::atomic::Ordering::Relaxed));
        assert!(lifecycle_fence.try_lock_owned().is_ok());
    }

    #[tokio::test]
    async fn aborted_fresh_preflight_releases_the_exact_planned_outage_claim() {
        let collect_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let released = std::sync::Arc::new(std::sync::Mutex::new(None));
        let task = tokio::spawn(acquire_then_collect_preflight(
            || async { Ok::<_, ApiError>("exact-claim-a".to_owned()) },
            {
                let collect_started = collect_started.clone();
                move || async move {
                    collect_started.notify_one();
                    std::future::pending::<Result<(), ApiError>>().await
                }
            },
            {
                let released = released.clone();
                move |claim| {
                    *released.lock().expect("release observation") = Some(claim);
                }
            },
        ));

        collect_started.notified().await;
        task.abort();
        let _ = task.await;
        assert_eq!(
            released.lock().expect("release observation").as_deref(),
            Some("exact-claim-a")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn mutation_preflight_bounds_the_current_membership_read() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let directory_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let probe_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let started = tokio::time::Instant::now();
        let result = collect_current_aggregate_with_sources(
            "node-1",
            started + AGGREGATE_TIMEOUT,
            || async { std::future::pending::<Result<MembershipStatus, String>>().await },
            {
                let directory_calls = directory_calls.clone();
                move || async move {
                    directory_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(Vec::new())
                }
            },
            || async { test_local_status("node-1", Some(1)) },
            {
                let probe_calls = probe_calls.clone();
                move |_peers, _deadline| async move {
                    probe_calls.fetch_add(1, Ordering::Relaxed);
                    BTreeMap::new()
                }
            },
        )
        .await;

        match result.expect_err("hung membership read must fail closed") {
            ApiError::Typed { status, code, .. } => {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(code, "cluster_roster_timed_out");
            }
            error => panic!("unexpected membership deadline error: {error:?}"),
        }
        assert_eq!(
            tokio::time::Instant::now() - started,
            PEER_DIRECTORY_TIMEOUT
        );
        assert_eq!(directory_calls.load(Ordering::Relaxed), 0);
        assert_eq!(probe_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn support_text_redacts_credentials_paths_and_secret_shapes() {
        for secret in [
            "Authorization: Bearer household-token",
            "join token plxjoin:v1:secret",
            "cluster_secret=abc",
            "password=hunter2",
            "signature=012345",
            "api_key=tmdb-secret",
            "username=alice",
            "opened /srv/media/private/movie.mkv",
            r"opened C:\media\private\movie.mkv",
        ] {
            let redacted = redact_operator_text(secret);
            assert_eq!(redacted, "[redacted potentially sensitive operator text]");
            assert!(!redacted.contains(secret));
        }
    }

    #[test]
    fn supervisor_commands_require_an_active_and_fully_drained_preparation() {
        let idle = crate::serving_fence::RestartDrainStatus {
            new_admissions_blocked: false,
            admissions_in_flight: 0,
            expires_at_unix_ms: None,
            drained: true,
        };
        assert!(restart_preparation_response("node-a", 0, idle)
            .restart_commands
            .is_empty());

        let draining = crate::serving_fence::RestartDrainStatus {
            new_admissions_blocked: true,
            admissions_in_flight: 1,
            expires_at_unix_ms: Some(unix_ms() + 60_000),
            drained: false,
        };
        assert!(restart_preparation_response("node-a", 0, draining)
            .restart_commands
            .is_empty());

        let drained = crate::serving_fence::RestartDrainStatus {
            admissions_in_flight: 0,
            drained: true,
            ..draining
        };
        assert!(!restart_preparation_response("node-a", 0, drained)
            .restart_commands
            .is_empty());
    }

    #[test]
    fn restart_preparation_claims_and_releases_the_replicated_outage_slot() {
        let source = include_str!("cluster_operations.rs")
            .split_once("pub(crate) async fn prepare_restart(")
            .expect("restart preparation")
            .1
            .split_once("fn restart_preparation_response(")
            .expect("restart preparation handlers end")
            .0;
        let claim = source
            .find("acquire_planned_outage_preflight")
            .expect("replicated lease acquisition");
        let bound = source
            .find("preparation_expiry_unix_ms")
            .expect("committed lease bounds local fence");
        let local_fence = source
            .find("begin_restart_preparation")
            .expect("local admission fence");
        let commands = source
            .find("restart_preparation_response")
            .expect("reboot-ready response");
        assert!(claim < bound && bound < local_fence && local_fence < commands);
        assert!(source.contains("lease.release().await"));
        assert!(source.contains("lease.disarm()"));
        assert!(source.contains("retain_restart_cancellation"));
        assert!(source.contains("run_serialized_planned_outage_task"));
        assert!(source.contains("release_current_restart_with_retry"));
    }

    #[test]
    fn identity_mismatch_discards_peer_local_state() {
        let member = ClusterNodeRecord {
            node_id: "node-a".to_owned(),
            hostname: "a".to_owned(),
            advertised_host: "a".to_owned(),
            raft_id: 7,
            role: NodeRole::Voter,
            is_voter: true,
            is_leader: false,
            reachable: true,
            last_seen_at: 1,
            removal_pending: false,
            learner_protocol_ready: true,
            bounded_read_ready: true,
            apply_lag_entries: Some(0),
            storage_headroom_bytes: Some(1),
            voter_storage_ready: true,
            maintenance: false,
            maintenance_requested_at: None,
            maintenance_acknowledged: false,
            maintenance_ready: false,
            active_media_sessions: 0,
        };
        let membership = MembershipStatus {
            local_node_id: "local".to_owned(),
            availability: plurx_core::cluster::membership::ClusterAvailability::SingleNode,
            nodes: vec![member],
            replication: test_replication_status(),
            capacity: plurx_core::cluster::membership::ClusterCapacityStatus {
                voting_nodes: 1,
                voting_quorum: 1,
                voting_failure_tolerance: 0,
                non_voting_replicas: 0,
                ready_read_workers: 0,
            },
            protocol: plurx_core::cluster::membership::ClusterProtocolStatus {
                active_min: 1,
                active_max: 1,
                binary_min: 1,
                binary_max: 1,
                learner_protocol_active: false,
                learner_protocol_pending: Vec::new(),
            },
            recovery: ClusterRecoveryStatus {
                required: false,
                quorum_available: true,
                reachable_voters: 1,
                required_voters: 1,
                leader_elected: true,
                permanent_majority_loss_supported: false,
            },
        };
        let mut status = test_local_status("node-a", Some(8));
        status.transport = Some(test_transport_status(7, 0, Some(1_000)));
        let mut remote = BTreeMap::new();
        remote.insert(
            "node-a".to_owned(),
            PeerStatusOutcome {
                node_id: "node-a".to_owned(),
                state: ObservationState::Answered,
                status: Some(status),
                transport: None,
            },
        );
        let rows = join_observations(
            &membership,
            "local",
            test_local_status("local", Some(1)),
            remote,
            unix_ms(),
        );
        assert_eq!(rows[0].observation, ObservationState::IdentityMismatch);
        assert!(rows[0].status.is_none());
        assert!(rows[0].transport.is_none());
    }

    #[test]
    fn public_transport_fallback_requires_matching_observer_identity() {
        let membership = test_membership("node-1", 2);
        let mut peer = test_local_status("node-2", Some(2));
        peer.transport = Some(test_transport_status(7, 0, Some(1_000)));
        let mut remote = BTreeMap::new();
        remote.insert(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Answered,
                status: Some(peer),
                transport: None,
            },
        );

        let rows = join_observations(
            &membership,
            "node-1",
            test_local_status("node-1", Some(1)),
            remote,
            unix_ms(),
        );
        assert_eq!(rows[1].observation, ObservationState::Answered);
        assert!(rows[1].status.is_some());
        assert!(rows[1]
            .status
            .as_ref()
            .is_some_and(|status| status.transport.is_none()));
        assert!(rows[1].transport.is_none());

        let mut peer = test_local_status("node-2", Some(2));
        peer.transport = Some(test_transport_status(2, 0, Some(1_000)));
        let mut remote = BTreeMap::new();
        remote.insert(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Answered,
                status: Some(peer),
                transport: None,
            },
        );
        let rows = join_observations(
            &membership,
            "node-1",
            test_local_status("node-1", Some(1)),
            remote,
            unix_ms(),
        );
        assert_eq!(
            rows[1]
                .transport
                .as_ref()
                .map(|transport| transport.observing_node_id),
            Some(2)
        );

        let mut peer = test_local_status("node-2", Some(2));
        let mut transport = test_transport_status(2, 0, Some(1_000));
        transport.observations[0].observing_node_id = 7;
        peer.transport = Some(transport);
        let mut remote = BTreeMap::new();
        remote.insert(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Answered,
                status: Some(peer),
                transport: None,
            },
        );
        let rows = join_observations(
            &membership,
            "node-1",
            test_local_status("node-1", Some(1)),
            remote,
            unix_ms(),
        );
        assert!(rows[1]
            .transport
            .as_ref()
            .is_some_and(|transport| transport.observations.is_empty()));
        assert!(rows[1]
            .status
            .as_ref()
            .is_some_and(|status| status.transport.is_none()));
    }

    #[test]
    fn remote_status_freshness_uses_local_monotonic_age_despite_clock_skew() {
        let membership = test_membership("node-1", 2);
        let aggregate_observed_ms = unix_ms();

        for observed_at_unix_ms in [
            aggregate_observed_ms.saturating_sub(60_000),
            aggregate_observed_ms.saturating_add(60_000),
        ] {
            let mut peer = test_local_status("node-2", Some(2));
            peer.observed_at_unix_ms = observed_at_unix_ms;
            let remote = BTreeMap::from([(
                "node-2".to_owned(),
                PeerStatusOutcome {
                    node_id: "node-2".to_owned(),
                    state: ObservationState::Answered,
                    status: Some(peer),
                    transport: None,
                },
            )]);

            let rows = join_observations(
                &membership,
                "node-1",
                test_local_status("node-1", Some(1)),
                PeerStatusCacheRead::Fresh(remote, Duration::from_millis(250)),
                aggregate_observed_ms,
            );

            assert_eq!(rows[1].observation, ObservationState::Answered);
            assert_eq!(rows[1].sample_age_ms, Some(250));
            assert_eq!(
                rows[1]
                    .status
                    .as_ref()
                    .map(|status| status.observed_at_unix_ms),
                Some(observed_at_unix_ms),
                "the peer timestamp remains diagnostic metadata"
            );
        }
    }

    #[test]
    fn stale_peer_sample_uses_local_elapsed_time_and_cannot_become_healthy() {
        let membership = test_membership("node-1", 2);
        let mut peer = test_local_status("node-2", Some(2));
        peer.observed_at_unix_ms = unix_ms().saturating_add(60_000);
        let mut remote = BTreeMap::new();
        remote.insert(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Answered,
                status: Some(peer),
                transport: None,
            },
        );

        let rows = join_observations(
            &membership,
            "node-1",
            test_local_status("node-1", Some(1)),
            PeerStatusCacheRead::Fresh(
                remote,
                Duration::from_millis(MAX_FRESH_AGE_MS.saturating_add(1)),
            ),
            unix_ms(),
        );

        assert_eq!(rows[1].observation, ObservationState::InvalidResponse);
        assert_eq!(rows[1].error_class.as_deref(), Some("stale_peer_sample"));
        assert!(!rollout_verdict(&membership, &rows).safe_to_restart_one);
    }

    #[tokio::test]
    async fn production_collector_preserves_private_transport_when_public_listener_is_closed() {
        let membership = test_membership("node-1", 2);
        let peers = vec![ActivityPeer {
            node_id: "node-2".to_owned(),
            raft_id: 2,
            http_base: Some("http://127.0.0.1:32400".to_owned()),
            reachable: true,
        }];
        let private_transport = test_transport_status(2, 0, Some(1_000));
        let remote = collect_peer_statuses_with_sources(
            peers,
            tokio::time::Instant::now() + Duration::from_secs(2),
            |peer, _deadline| async move {
                assert_eq!(peer.node_id, "node-2");
                PeerStatusOutcome {
                    node_id: peer.node_id,
                    state: ObservationState::Unreachable,
                    status: None,
                    transport: None,
                }
            },
            move |raft_id| {
                let private_transport = private_transport.clone();
                async move {
                    assert_eq!(raft_id, 2);
                    Ok(Some(private_transport))
                }
            },
        )
        .await;
        let rows = join_observations(
            &membership,
            "node-1",
            test_local_status("node-1", Some(1)),
            remote,
            unix_ms(),
        );
        assert_eq!(rows[1].observation, ObservationState::Unreachable);
        assert_eq!(
            rows[1]
                .transport
                .as_ref()
                .map(|status| status.observing_node_id),
            Some(2)
        );
        assert_eq!(
            rows[1]
                .transport
                .as_ref()
                .and_then(|status| status.observations.first())
                .map(|observation| observation.locally_received_bytes),
            Some(Some(64))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn peer_transport_age_retains_request_to_receipt_uncertainty() {
        let peers = vec![
            ActivityPeer {
                node_id: "node-2".to_owned(),
                raft_id: 2,
                http_base: None,
                reachable: true,
            },
            ActivityPeer {
                node_id: "node-3".to_owned(),
                raft_id: 3,
                http_base: None,
                reachable: true,
            },
        ];
        let mut remote = collect_peer_statuses_with_sources(
            peers,
            tokio::time::Instant::now() + Duration::from_secs(2),
            |peer, _deadline| async move {
                PeerStatusOutcome {
                    node_id: peer.node_id,
                    state: ObservationState::Unreachable,
                    status: None,
                    transport: None,
                }
            },
            |raft_id| async move {
                let mut transport = test_transport_status(raft_id, 0, Some(20_000));
                transport.observations[0].attempt_age_ms =
                    Some(if raft_id == 2 { 1_000 } else { 500 });
                if raft_id == 2 {
                    // Construct the older peer sample before delaying its
                    // delivery. Sleeping first would test delayed sampling and
                    // would not reproduce response-transit uncertainty.
                    tokio::time::sleep(Duration::from_millis(900)).await;
                }
                Ok(Some(transport))
            },
        )
        .await;

        let old = sanitize_transport(
            remote
                .remove("node-2")
                .and_then(|outcome| outcome.transport)
                .expect("early private transport"),
            2,
            0,
        )
        .expect("valid early private transport");
        let successor = sanitize_transport(
            remote
                .remove("node-3")
                .and_then(|outcome| outcome.transport)
                .expect("delayed private transport"),
            3,
            0,
        )
        .expect("valid delayed private transport");

        assert_eq!(old.observations[0].attempt_age_ms, Some(1_900));
        assert_eq!(successor.observations[0].attempt_age_ms, Some(1_400));
        assert_eq!(old.observations[0].age_uncertainty_ms, 900);
        assert_eq!(successor.observations[0].age_uncertainty_ms, 0);
        assert!(
            old.observations[0]
                .attempt_age_ms
                .expect("older attempt age")
                .saturating_sub(old.observations[0].age_uncertainty_ms)
                < successor.observations[0]
                    .attempt_age_ms
                    .expect("successor attempt age"),
            "the delayed predecessor and successor intervals must overlap"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn slow_public_probe_cannot_discard_completed_private_transport() {
        let peers = vec![ActivityPeer {
            node_id: "node-2".to_owned(),
            raft_id: 2,
            http_base: Some("http://blackhole.invalid:32400".to_owned()),
            reachable: true,
        }];
        let private_transport = test_transport_status(2, 0, Some(1_000));
        let remote = collect_peer_statuses_with_sources(
            peers,
            tokio::time::Instant::now() + Duration::from_secs(2),
            |_peer, _deadline| async move { std::future::pending::<PeerStatusOutcome>().await },
            move |raft_id| {
                let private_transport = private_transport.clone();
                async move {
                    assert_eq!(raft_id, 2);
                    Ok(Some(private_transport))
                }
            },
        )
        .await;
        let outcome = remote.get("node-2").expect("timed-out public outcome");
        assert_eq!(outcome.state, ObservationState::TimedOut);
        assert_eq!(
            outcome
                .transport
                .as_ref()
                .map(|transport| transport.observing_node_id),
            Some(2)
        );
    }

    #[test]
    fn every_aggregator_selects_the_same_roster_order_candidate() {
        let mut membership = test_membership("node-1", 3);
        let observations = test_observations(&membership);
        let first = rollout_verdict(&membership, &observations);
        assert!(first.safe_to_restart_one);
        assert_eq!(first.candidate_node_id.as_deref(), Some("node-2"));

        membership.local_node_id = "node-3".to_owned();
        let second = rollout_verdict(&membership, &observations);
        assert!(second.safe_to_restart_one);
        assert_eq!(second.candidate_node_id, first.candidate_node_id);
    }

    #[test]
    fn an_active_restart_preparation_blocks_a_second_restart() {
        let membership = test_membership("node-1", 3);
        let mut observations = test_observations(&membership);
        observations[1]
            .status
            .as_mut()
            .expect("healthy fixture status")
            .media
            .new_admissions_blocked = true;

        let verdict = rollout_verdict(&membership, &observations);
        assert!(!verdict.safe_to_restart_one);
        assert!(verdict.candidate_node_id.is_none());
        assert!(verdict
            .blockers
            .iter()
            .any(|finding| finding.code == "restart_preparation_active"));
    }

    #[test]
    fn maintenance_entry_admits_the_leader_a_busy_follower_and_a_stable_learner() {
        let mut membership = test_membership("node-1", 3);
        membership.nodes.push(ClusterNodeRecord {
            node_id: "node-4".to_owned(),
            hostname: "node-4".to_owned(),
            advertised_host: "node-4:8080".to_owned(),
            raft_id: 4,
            role: NodeRole::Learner,
            is_voter: false,
            is_leader: false,
            reachable: true,
            last_seen_at: 1,
            removal_pending: false,
            learner_protocol_ready: true,
            bounded_read_ready: true,
            apply_lag_entries: Some(0),
            storage_headroom_bytes: Some(1),
            voter_storage_ready: false,
            maintenance: false,
            maintenance_requested_at: None,
            maintenance_acknowledged: false,
            maintenance_ready: false,
            active_media_sessions: 0,
        });
        membership.capacity.non_voting_replicas = 1;
        membership.capacity.ready_read_workers = 1;
        let mut observations = test_observations(&membership);
        observations[1]
            .status
            .as_mut()
            .expect("busy follower status")
            .media = MediaDrainStatus {
            local_active_sessions: 2,
            drained: false,
            new_admissions_blocked: false,
            admissions_in_flight: 0,
            preparation_expires_at_unix_ms: None,
            direct_play_connections: DirectPlayConnectionStatus::UnknownRequiresProxy,
            restart_commands: Vec::new(),
        };

        let verdicts = maintenance_entry_verdicts(&membership, &observations);
        for node_id in ["node-1", "node-2", "node-4"] {
            let verdict = verdicts
                .iter()
                .find(|verdict| verdict.node_id == node_id)
                .expect("target verdict");
            assert!(
                verdict.safe_to_enter,
                "{node_id} should enter maintenance: {:?}",
                verdict.blockers
            );
        }
    }

    #[test]
    fn maintenance_entry_subtracts_only_a_voters_vote() {
        let mut membership = test_membership("node-1", 2);
        membership.nodes.push(ClusterNodeRecord {
            node_id: "node-3".to_owned(),
            hostname: "node-3".to_owned(),
            advertised_host: "node-3:8080".to_owned(),
            raft_id: 3,
            role: NodeRole::Learner,
            is_voter: false,
            is_leader: false,
            reachable: true,
            last_seen_at: 1,
            removal_pending: false,
            learner_protocol_ready: true,
            bounded_read_ready: true,
            apply_lag_entries: Some(0),
            storage_headroom_bytes: Some(1),
            voter_storage_ready: false,
            maintenance: false,
            maintenance_requested_at: None,
            maintenance_acknowledged: false,
            maintenance_ready: false,
            active_media_sessions: 0,
        });
        let observations = test_observations(&membership);
        let verdicts = maintenance_entry_verdicts(&membership, &observations);
        let voter = verdicts
            .iter()
            .find(|verdict| verdict.node_id == "node-1")
            .expect("voter verdict");
        assert!(!voter.safe_to_enter);
        assert!(voter
            .blockers
            .iter()
            .any(|finding| finding.code == "maintenance_would_lose_quorum"));
        assert!(
            verdicts
                .iter()
                .find(|verdict| verdict.node_id == "node-3")
                .expect("learner verdict")
                .safe_to_enter
        );
    }

    #[test]
    fn one_unreachable_voter_in_a_four_voter_roster_is_not_restart_safe() {
        let membership = test_membership("node-1", 4);
        let mut observations = test_observations(&membership);
        observations[3].observation = ObservationState::Unreachable;
        observations[3].status = None;
        observations[3].error_class = Some("unreachable".to_owned());

        let verdict = rollout_verdict(&membership, &observations);
        assert!(!verdict.safe_to_restart_one);
        assert!(verdict
            .blockers
            .iter()
            .any(|finding| finding.code == "voter_not_observed"));
    }

    #[test]
    fn election_protocol_wal_and_build_skew_are_classified_conservatively() {
        let membership = test_membership("node-1", 3);

        let mut mixed_builds = test_observations(&membership);
        mixed_builds[2]
            .status
            .as_mut()
            .expect("healthy fixture status")
            .build = "test-next".to_owned();
        let verdict = rollout_verdict(&membership, &mixed_builds);
        assert!(verdict.safe_to_restart_one);
        assert!(verdict
            .warnings
            .iter()
            .any(|finding| finding.code == "mixed_builds"));

        let mut election = test_observations(&membership);
        election[2]
            .status
            .as_mut()
            .expect("healthy fixture status")
            .raft
            .current_term = Some(2);
        assert!(rollout_verdict(&membership, &election)
            .blockers
            .iter()
            .any(|finding| finding.code == "leader_term_disagreement"));

        let mut protocol = test_observations(&membership);
        protocol[2]
            .status
            .as_mut()
            .expect("healthy fixture status")
            .protocol_min = 2;
        protocol[2]
            .status
            .as_mut()
            .expect("healthy fixture status")
            .protocol_max = 2;
        assert!(rollout_verdict(&membership, &protocol)
            .blockers
            .iter()
            .any(|finding| finding.code == "protocol_incompatible"));

        let mut wal_error = test_observations(&membership);
        wal_error[2]
            .status
            .as_mut()
            .expect("healthy fixture status")
            .wal
            .snapshot
            .as_mut()
            .expect("healthy fixture WAL")
            .last_error = Some(plurx_core::cluster::migration::status::BoundedWalError {
            observed_at_unix_ms: unix_ms(),
            message: "injected test failure".to_owned(),
        });
        assert!(rollout_verdict(&membership, &wal_error)
            .blockers
            .iter()
            .any(|finding| finding.code == "wal_not_healthy"));
    }

    fn test_local_status(node_id: &str, raft_id: Option<u64>) -> ClusterNodeOperationsStatus {
        ClusterNodeOperationsStatus {
            schema_version: 1,
            observed_at_unix_ms: unix_ms(),
            node_id: node_id.to_owned(),
            raft_id,
            hostname: node_id.to_owned(),
            build: "test".to_owned(),
            protocol_min: 1,
            protocol_max: 1,
            process: ProcessStatus { live: true },
            serving: ReadinessEvaluation {
                ready: true,
                reason: None,
            },
            raft: RaftStatus {
                running: Some(true),
                sample_valid: true,
                sample_age_seconds: Some(0),
                current_term: Some(1),
                leader_id: Some(1),
                is_leader: Some(false),
                applied_index: Some(1),
                commit_index: Some(1),
                apply_lag_entries: Some(0),
                watermark_valid: true,
                watermark_age_millis: Some(0),
                observation_errors: 0,
                watermark_errors: 0,
            },
            wal: WalStatus {
                available: true,
                reason: None,
                snapshot: Some(WalStatusSnapshot {
                    state: WalRuntimeState::Open,
                    lock_owned: true,
                    unclean_start_observed: false,
                    sync_policy: "immediate".to_owned(),
                    segment_size_bytes: 1_048_576,
                    segment_count: 1,
                    allocated_bytes: 1_048_576,
                    first_retained_index: Some(1),
                    last_log_index: Some(1),
                    last_purged_index: None,
                    last_durable_index: Some(1),
                    last_sync_unix_ms: Some(unix_ms()),
                    last_compaction_unix_ms: None,
                    last_recovery: None,
                    last_error: None,
                }),
            },
            snapshot: SnapshotStatus::from(None),
            transport: None,
            media: MediaDrainStatus {
                local_active_sessions: 0,
                drained: true,
                new_admissions_blocked: false,
                admissions_in_flight: 0,
                preparation_expires_at_unix_ms: None,
                direct_play_connections: DirectPlayConnectionStatus::UnknownRequiresProxy,
                restart_commands: Vec::new(),
            },
        }
    }

    fn test_membership(local_node_id: &str, voters: usize) -> MembershipStatus {
        let nodes = (1..=voters)
            .map(|index| ClusterNodeRecord {
                node_id: format!("node-{index}"),
                hostname: format!("node-{index}"),
                advertised_host: format!("node-{index}:8080"),
                raft_id: u64::try_from(index).expect("small fixture index"),
                role: NodeRole::Voter,
                is_voter: true,
                is_leader: index == 1,
                reachable: true,
                last_seen_at: 1,
                removal_pending: false,
                learner_protocol_ready: true,
                bounded_read_ready: true,
                apply_lag_entries: Some(0),
                storage_headroom_bytes: Some(1),
                voter_storage_ready: true,
                maintenance: false,
                maintenance_requested_at: None,
                maintenance_acknowledged: false,
                maintenance_ready: false,
                active_media_sessions: 0,
            })
            .collect::<Vec<_>>();
        let quorum = voters / 2 + 1;
        MembershipStatus {
            local_node_id: local_node_id.to_owned(),
            availability: plurx_core::cluster::membership::ClusterAvailability::SingleNode,
            nodes,
            replication: test_replication_status(),
            capacity: plurx_core::cluster::membership::ClusterCapacityStatus {
                voting_nodes: voters,
                voting_quorum: quorum,
                voting_failure_tolerance: voters.saturating_sub(quorum),
                non_voting_replicas: 0,
                ready_read_workers: 0,
            },
            protocol: plurx_core::cluster::membership::ClusterProtocolStatus {
                active_min: 1,
                active_max: 1,
                binary_min: 1,
                binary_max: 1,
                learner_protocol_active: false,
                learner_protocol_pending: Vec::new(),
            },
            recovery: ClusterRecoveryStatus {
                required: false,
                quorum_available: true,
                reachable_voters: voters,
                required_voters: quorum,
                leader_elected: true,
                permanent_majority_loss_supported: false,
            },
        }
    }

    fn test_observations(membership: &MembershipStatus) -> Vec<ClusterNodeObservation> {
        membership
            .nodes
            .iter()
            .map(|member| {
                let mut status = test_local_status(&member.node_id, Some(member.raft_id));
                status.raft.is_leader = Some(member.raft_id == 1);
                ClusterNodeObservation {
                    membership: member.clone(),
                    observation: ObservationState::Answered,
                    sample_age_ms: Some(0),
                    error_class: None,
                    status: Some(status),
                    transport: None,
                }
            })
            .collect()
    }

    fn test_replication_status() -> plurx_core::cluster::migration::status::ReplicationStatus {
        serde_json::from_value(serde_json::json!({
            "backend": "replicated",
            "health": "in_sync",
            "clustered": true,
            "last_applied_term": 1,
            "last_applied_index": 1,
            "behind_by": 0,
            "last_converged_at": 1,
            "checked_at": 1,
            "explanation": "test"
        }))
        .expect("test replication status")
    }
}
