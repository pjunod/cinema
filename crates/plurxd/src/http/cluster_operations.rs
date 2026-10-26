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
use crate::redact::redact_operator_text;
use crate::serving_fence::{
    OwnedWorkCounts, OwnedWorkInventory, OwnedWorkUnknown, PlannedOutageFenceToken,
};
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
    #[serde(default)]
    pub owned_work: OwnedWorkInventory,
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
    AccountedInAdmissions,
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
        let (active_sessions, owned_work, drain) = local_media_drain_snapshot(&state).await;
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
            owned_work,
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
        state
            .serving
            .cancel_current_restart_preparation(cancellation, active_sessions)
            .await;
        let (active_sessions, owned_work, drain) = local_media_drain_snapshot(&state).await;
        Ok(Json(restart_preparation_response(
            &state.node_id,
            active_sessions,
            owned_work,
            drain,
        )))
    })
    .await
}

fn restart_preparation_response(
    node_id: &str,
    active_sessions: usize,
    owned_work: OwnedWorkInventory,
    drain: crate::serving_fence::RestartDrainStatus,
) -> RestartPreparationResponse {
    let restart_commands = (drain.new_admissions_blocked && drain.drained && owned_work.is_empty())
        .then(restart_commands)
        .unwrap_or_default();
    RestartPreparationResponse {
        schema_version: 1,
        node_id: node_id.to_owned(),
        media: media_drain_status(active_sessions, owned_work, drain),
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
    let (active_sessions, owned_work, drain) = local_media_drain_snapshot(state).await;
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
        media: media_drain_status(active_sessions, owned_work, drain),
    }
}

/// Store-free readiness projection for operations diagnostics.
///
/// Replicated nodes already publish readiness through the passive serving
/// fence. A recovery/SQLite process has no cached Store proof on this surface,
/// so it reports unavailable rather than turning an operations request into a
/// Store ping.
pub(super) fn operations_readiness(state: &AppState) -> ReadinessEvaluation {
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
        super::clock_readiness_failure(state).unwrap_or(ReadinessEvaluation {
            ready: true,
            reason: None,
        })
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
        .saturating_add(state.live_tv.recording_activities().len())
}

/// These are original registered owners, not sampled CPU/RSS or a refund.
fn local_owned_work_inventory(state: &AppState) -> OwnedWorkInventory {
    let read = || -> Result<OwnedWorkCounts, OwnedWorkUnknown> {
        #[cfg(target_os = "linux")]
        let registered_adaptive_attempts = state.transcode.adaptive_attempt_drain_count()?;
        // The adaptive attempt module cannot register on these platforms.
        #[cfg(not(target_os = "linux"))]
        let registered_adaptive_attempts = 0;
        let (pool, background_heavy_held) = state.transcode.original_legacy_drain_counts();
        let mut counts = OwnedWorkCounts {
            registered_adaptive_attempts,
            legacy_hardware_slots: pool.hardware_used,
            legacy_software_threads: pool.software_used,
            legacy_live_waiters: pool.live_waiting,
            legacy_reservations: pool.reservations,
            legacy_background_active: pool.background_active,
            background_heavy_held,
            ..OwnedWorkCounts::default()
        };
        if let Some(runtime) = state.resource_runtime.as_ref() {
            let ledger = runtime
                .accounting()
                .map_err(|_| OwnedWorkUnknown::ResourceLedgerUnavailable)?;
            counts.planning_metadata_roles = ledger.planning_metadata_held;
            counts.native_producer_roles = ledger
                .unknown_peak_producers
                .checked_add(ledger.active_bounded_producers)
                .ok_or(OwnedWorkUnknown::CounterOverflow)?;
            counts.unknown_resource_roles = ledger.unknown_held;
        }
        Ok(counts)
    };
    match read() {
        Ok(counts) => OwnedWorkInventory::Known { counts },
        Err(reason) => OwnedWorkInventory::Unknown { reason },
    }
}

/// Public session maps are resampled only after the in-flight admission
/// barrier, under the same mutex that fences all newly admitted work. The
/// first count is a compatibility diagnostic hint used ONLY on an Unknown
/// busy result; it can never authorize a drained/ready verdict.
pub(crate) async fn local_media_drain_snapshot(
    state: &AppState,
) -> (
    usize,
    OwnedWorkInventory,
    crate::serving_fence::RestartDrainStatus,
) {
    let observed_public_sessions = local_owned_media_sessions(state).await;
    state
        .serving
        .restart_drain_status_with_owned_work(observed_public_sessions, || async {
            let public_sessions = local_owned_media_sessions(state).await;
            let owned = local_owned_work_inventory(state);
            (public_sessions, owned)
        })
        .await
}

fn media_drain_status(
    active_sessions: usize,
    owned_work: OwnedWorkInventory,
    drain: crate::serving_fence::RestartDrainStatus,
) -> MediaDrainStatus {
    let drained = drain.drained && owned_work.is_empty();
    let ready_for_supervisor = drain.new_admissions_blocked && drained;
    MediaDrainStatus {
        local_active_sessions: active_sessions,
        owned_work,
        drained,
        new_admissions_blocked: drain.new_admissions_blocked,
        admissions_in_flight: drain.admissions_in_flight,
        preparation_expires_at_unix_ms: drain.expires_at_unix_ms,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        direct_play_connections: DirectPlayConnectionStatus::AccountedInAdmissions,
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
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

/// Revalidate a native shutdown against fresh cluster evidence while excluding
/// only this process's already-owned local preparation from "second outage".
/// Other members' fences, compatibility, quorum and role checks stay intact.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) async fn recheck_desktop_preparation(
    state: &AppState,
    token: crate::serving_fence::PlannedOutageFenceToken,
) -> Result<ClusterOperationsAggregate, ApiError> {
    let mut aggregate = collect_current_aggregate(state).await?;
    if !state.serving.planned_outage_is_current(token).await {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_expired",
            "the exact local preparation is no longer current",
        ));
    }
    for observation in &mut aggregate.nodes {
        if observation.membership.node_id == state.node_id {
            if let Some(status) = &mut observation.status {
                status.media.new_admissions_blocked = false;
            }
        }
    }
    aggregate.verdict = rollout_verdict(&aggregate.membership, &aggregate.nodes);
    Ok(aggregate)
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
mod tests;
