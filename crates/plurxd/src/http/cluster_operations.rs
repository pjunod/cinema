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
    ActivityPeer, ClusterNodeRecord, ClusterOperationLease, MembershipStatus, NodeRole,
    MAX_OPERATIONS_PEERS,
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
use crate::state::AppState;

pub(crate) const INTERNAL_PATH: &str = "/api/v1/internal/cluster/operations-status";
const AGGREGATE_TIMEOUT: Duration = Duration::from_secs(2);
const PEER_STATUS_TIMEOUT: Duration = Duration::from_secs(1);
const PEER_DIRECTORY_TIMEOUT: Duration = Duration::from_millis(500);
const PEER_STATUS_CACHE_TTL: Duration = Duration::from_secs(5);
const PEER_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(3);
const TRANSPORT_OBSERVATION_TTL_MS: u64 = 5 * 60 * 1_000;
const MAX_TRANSPORT_SOURCE_CLOCK_SKEW_MS: u64 = 5_000;
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

/// Claim the replicated membership-lifecycle exclusion before collecting the
/// safety evidence it protects. A failed collection releases the exact claim;
/// a successful caller owns it until restart preparation or maintenance
/// consumes/cancels it.
pub(crate) async fn acquire_planned_outage_preflight(
    state: &AppState,
    node_id: &str,
    duration: Duration,
    operation: PlannedOutageOperation,
) -> Result<(ClusterOperationLease, ClusterOperationsAggregate), ApiError> {
    let acquire_membership = state.membership.clone();
    let release_membership = state.membership.clone();
    acquire_then_collect_preflight(
        move || async move {
            match operation {
                PlannedOutageOperation::Restart => acquire_membership
                    .acquire_restart_preparation(node_id, duration)
                    .await,
                PlannedOutageOperation::Maintenance => acquire_membership
                    .acquire_maintenance_preparation(node_id, duration)
                    .await,
            }
            .map_err(api_error)
        },
        || collect_current_aggregate(state),
        move |lease| async move {
            if let Err(error) = release_membership
                .release_cluster_operation_lease(&lease)
                .await
            {
                tracing::warn!(%error, "failed to release planned-outage lease after preflight failure; expiry remains authoritative");
            }
        },
    )
    .await
}

async fn acquire_then_collect_preflight<
    Lease,
    Evidence,
    Acquire,
    AcquireFuture,
    Collect,
    CollectFuture,
    Release,
    ReleaseFuture,
>(
    acquire: Acquire,
    collect: Collect,
    release: Release,
) -> Result<(Lease, Evidence), ApiError>
where
    Acquire: FnOnce() -> AcquireFuture,
    AcquireFuture: std::future::Future<Output = Result<Lease, ApiError>>,
    Collect: FnOnce() -> CollectFuture,
    CollectFuture: std::future::Future<Output = Result<Evidence, ApiError>>,
    Release: FnOnce(Lease) -> ReleaseFuture,
    ReleaseFuture: std::future::Future<Output = ()>,
{
    let lease = acquire().await?;
    match collect().await {
        Ok(evidence) => Ok((lease, evidence)),
        Err(error) => {
            release(lease).await;
            Err(error)
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

        let local_status = local_read().await;
        let remote = collect_peers(peers, deadline).await;
        Ok(assemble_aggregate(
            local_node_id,
            membership,
            local_status,
            PeerStatusCacheRead::Fresh(remote),
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
    let duration = Duration::from_secs(request.expires_in_seconds.unwrap_or(900).clamp(60, 3_600));
    let (lease, preflight) = acquire_planned_outage_preflight(
        &state,
        &node_id,
        duration,
        PlannedOutageOperation::Restart,
    )
    .await?;
    if !preflight.verdict.safe_to_restart_one {
        if let Err(error) = state
            .membership
            .release_cluster_operation_lease(&lease)
            .await
        {
            tracing::warn!(%error, "failed to release rejected restart preparation lease; expiry remains authoritative");
        }
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_unsafe",
            "the current cluster rollout verdict does not permit preparing a voter",
        ));
    }
    if preflight.verdict.candidate_node_id.as_deref() != Some(state.node_id.as_str()) {
        if let Err(error) = state
            .membership
            .release_cluster_operation_lease(&lease)
            .await
        {
            tracing::warn!(%error, "failed to release restart candidate-mismatch lease; expiry remains authoritative");
        }
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_candidate_mismatch",
            "open the candidate node directly; this node is not the current safe restart candidate",
        ));
    }
    let local_expiry = lease.preparation_expiry_unix_ms(duration);
    if local_expiry.is_none()
        || !state
            .serving
            .begin_restart_preparation_until(local_expiry.unwrap_or_default())
            .await
    {
        if let Err(error) = state
            .membership
            .release_cluster_operation_lease(&lease)
            .await
        {
            tracing::warn!(%error, "failed to release exhausted restart preparation lease");
        }
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_expired",
            "restart preparation expired while the replicated lease was being committed; run preflight again",
        ));
    }
    state.serving.wait_for_restart_admissions().await;
    let active_sessions = local_owned_media_sessions(&state).await;
    let drain = state.serving.restart_drain_status(active_sessions).await;
    if !drain.new_admissions_blocked {
        if let Err(error) = state
            .membership
            .release_cluster_operation_lease(&lease)
            .await
        {
            tracing::warn!(%error, "failed to release expired restart preparation lease");
        }
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_expired",
            "restart preparation expired before pre-existing admissions settled; run preflight again",
        ));
    }
    Ok(Json(restart_preparation_response(
        &state.node_id,
        active_sessions,
        drain,
    )))
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
    state
        .membership
        .release_restart_preparation(&node_id)
        .await
        .map_err(api_error)?;
    let active_sessions = local_owned_media_sessions(&state).await;
    let drain = state
        .serving
        .cancel_restart_preparation(active_sessions)
        .await;
    Ok(Json(restart_preparation_response(
        &state.node_id,
        active_sessions,
        drain,
    )))
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
    let snapshot = SnapshotStatus::from(view.snapshot_metrics);
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
    stored_at: tokio::time::Instant,
    statuses: BTreeMap<String, PeerStatusOutcome>,
}

enum PeerStatusCacheRead {
    Fresh(BTreeMap<String, PeerStatusOutcome>),
    Unavailable,
    Stale,
}

impl From<BTreeMap<String, PeerStatusOutcome>> for PeerStatusCacheRead {
    fn from(statuses: BTreeMap<String, PeerStatusOutcome>) -> Self {
        Self::Fresh(statuses)
    }
}

impl PeerStatusCache {
    async fn fresh(&self) -> PeerStatusCacheRead {
        let cache = self.inner.lock().await;
        let Some(cached) = cache.as_ref() else {
            return PeerStatusCacheRead::Unavailable;
        };
        if tokio::time::Instant::now().saturating_duration_since(cached.stored_at)
            < PEER_STATUS_CACHE_TTL
        {
            PeerStatusCacheRead::Fresh(cached.statuses.clone())
        } else {
            PeerStatusCacheRead::Stale
        }
    }

    async fn store(&self, statuses: &BTreeMap<String, PeerStatusOutcome>) {
        *self.inner.lock().await = Some(CachedPeerStatuses {
            // Availability starts when the refresh is usable. Transport and
            // process samples retain their source timestamps and are aged
            // from those timestamps when the aggregate is rendered.
            stored_at: tokio::time::Instant::now(),
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
    let deadline = deadline_after(AGGREGATE_TIMEOUT);
    let statuses = collect(peers, deadline).await;
    cache.store(&statuses).await;
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

fn age_transport_observations(transport: &mut SnapshotTransportStatus, elapsed_ms: u64) {
    transport.observations.retain_mut(|observation| {
        let source_phase = observation.phase;
        let source_deadline_remaining_ms = observation.active_deadline_remaining_ms;
        observation.sample_age_ms = observation.sample_age_ms.saturating_add(elapsed_ms);
        for age_ms in [
            &mut observation.attempt_age_ms,
            &mut observation.last_acknowledgement_age_ms,
            &mut observation.last_local_receive_age_ms,
        ] {
            if let Some(age_ms) = age_ms.as_mut() {
                *age_ms = age_ms.saturating_add(elapsed_ms);
            }
        }
        let deadline_still_active =
            source_deadline_remaining_ms.is_some_and(|remaining_ms| remaining_ms > elapsed_ms);
        let deadline_crossed = source_deadline_remaining_ms
            .is_some_and(|remaining_ms| remaining_ms <= elapsed_ms)
            && source_phase != SnapshotTransportPhase::Stalled;
        let projected_stalled_retention = deadline_crossed
            && source_deadline_remaining_ms.is_some_and(|remaining_ms| {
                elapsed_ms.saturating_sub(remaining_ms) <= TRANSPORT_OBSERVATION_TTL_MS
            });
        if let Some(remaining_ms) = observation.active_deadline_remaining_ms.as_mut() {
            *remaining_ms = remaining_ms.saturating_sub(elapsed_ms);
        }
        if deadline_crossed
            && matches!(
                source_phase,
                SnapshotTransportPhase::Connecting
                    | SnapshotTransportPhase::Transferring
                    | SnapshotTransportPhase::AwaitingAcknowledgement
                    | SnapshotTransportPhase::Installing
                    | SnapshotTransportPhase::Retrying
            )
        {
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
        let membership = state.membership.clone();
        if let Err(error) = refresh_membership_status_cache_with(
            &state.membership_status_cache,
            refresh_started,
            move || async move { membership.status().await.map_err(|error| error.to_string()) },
        )
        .await
        {
            tracing::warn!(%error, "could not refresh bounded cluster membership cache");
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
            let public = tokio::time::timeout_at(peer_deadline, public(peer, peer_deadline));
            let private_deadline =
                peer_deadline.min(tokio::time::Instant::now() + Duration::from_millis(900));
            let private = tokio::time::timeout_at(private_deadline, private(peer_node_id));
            let (public_outcome, private_transport) = tokio::join!(public, private);
            let mut outcome = public_outcome.unwrap_or_else(|_| PeerStatusOutcome {
                node_id: node_id.clone(),
                state: ObservationState::TimedOut,
                status: None,
                transport: None,
            });
            outcome.transport = private_transport
                .ok()
                .and_then(Result::ok)
                .flatten()
                .filter(|status| status.observing_node_id == peer_node_id);
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
    let (mut remote, missing_state, missing_error_class) = match remote.into() {
        PeerStatusCacheRead::Fresh(statuses) => {
            (statuses, ObservationState::Unavailable, "not_observed")
        }
        PeerStatusCacheRead::Unavailable => (
            BTreeMap::new(),
            ObservationState::Unavailable,
            "cache_unavailable",
        ),
        PeerStatusCacheRead::Stale => (
            BTreeMap::new(),
            ObservationState::Unavailable,
            "cache_stale",
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
                .and_then(|transport| {
                    sanitize_transport(transport, member.raft_id, aggregate_observed_ms)
                });
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
                sanitize_transport(transport, member.raft_id, aggregate_observed_ms)
            });
            let fallback_transport = fallback_transport.and_then(|transport| {
                sanitize_transport(transport, member.raft_id, aggregate_observed_ms)
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
        let sample_age_ms = status
            .as_ref()
            .map(|status| aggregate_observed_ms.saturating_sub(status.observed_at_unix_ms));
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
    aggregate_observed_ms: u64,
) -> Option<SnapshotTransportStatus> {
    if transport.observing_node_id != expected_observer {
        return None;
    }
    let source_age_ms = if transport.observed_at_unix_ms > aggregate_observed_ms {
        let clock_skew_ms = transport
            .observed_at_unix_ms
            .saturating_sub(aggregate_observed_ms);
        if clock_skew_ms > MAX_TRANSPORT_SOURCE_CLOCK_SKEW_MS {
            return None;
        }
        0
    } else {
        aggregate_observed_ms.saturating_sub(transport.observed_at_unix_ms)
    };
    transport
        .observations
        .retain(|observation| observation.observing_node_id == expected_observer);
    age_transport_observations(&mut transport, source_age_ms);
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
            PeerStatusCacheRead::Fresh(statuses) => statuses,
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
        aggregate_observed_ms: u64,
    ) -> SnapshotTransportStatus {
        let mut statuses = fresh_cache_statuses(read);
        let transport = statuses
            .remove(node_id)
            .and_then(|outcome| outcome.transport)
            .expect("cached private transport evidence");
        sanitize_transport(transport, expected_observer, aggregate_observed_ms)
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
        assert!(matches!(cache.fresh().await, PeerStatusCacheRead::Fresh(_)));
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
    async fn full_refresh_cycle_keeps_cache_fresh_and_ages_transport_from_source_time() {
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
        assert_eq!(observation.sample_age_ms, 5_000);
        assert_eq!(observation.active_deadline_remaining_ms, Some(5_000));
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
        assert_eq!(
            observation.last_error_category.as_deref(),
            Some("snapshot_stalled")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cached_inactive_transport_observation_expires_at_five_minutes() {
        let cache = PeerStatusCache::default();
        let source_observed_ms = 1_000_000;
        let mut transport = test_transport_status(2, 299_000, None);
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

    #[test]
    fn transport_projection_uses_source_time_and_rejects_unbounded_future_skew() {
        let source_observed_ms = 1_000_000;
        let mut transport = test_transport_status(2, 1_000, Some(20_000));
        transport.observed_at_unix_ms = source_observed_ms;
        let projected = sanitize_transport(transport, 2, source_observed_ms + 6_000)
            .expect("bounded source timestamp");
        let observation = &projected.observations[0];
        assert_eq!(observation.sample_age_ms, 7_000);
        assert_eq!(observation.attempt_age_ms, Some(6_100));
        assert_eq!(observation.last_local_receive_age_ms, Some(6_010));
        assert_eq!(observation.active_deadline_remaining_ms, Some(14_000));

        let mut replayed = test_transport_status(2, 301_000, Some(30_000));
        replayed.observed_at_unix_ms = source_observed_ms;
        let projected = sanitize_transport(replayed, 2, source_observed_ms + 40_000)
            .expect("old source timestamp is valid but ages conservatively");
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

        let mut bounded_future = test_transport_status(2, 0, Some(1_000));
        bounded_future.observed_at_unix_ms =
            source_observed_ms + MAX_TRANSPORT_SOURCE_CLOCK_SKEW_MS;
        assert!(sanitize_transport(bounded_future, 2, source_observed_ms).is_some());

        let mut unbounded_future = test_transport_status(2, 0, Some(1_000));
        unbounded_future.observed_at_unix_ms =
            source_observed_ms + MAX_TRANSPORT_SOURCE_CLOCK_SKEW_MS + 1;
        assert!(sanitize_transport(unbounded_future, 2, source_observed_ms).is_none());
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
        let cache_extractor = extractor_source
            .split("impl FromRequestParts<AppState> for CacheOnlyAdminUser")
            .nth(1)
            .and_then(|tail| tail.split("#[cfg(test)]").next())
            .expect("cache-only admin extractor source");
        assert!(!cache_extractor.contains("state.store"));
        assert!(!cache_extractor.contains(".await"));
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
        assert!(restart.contains("release_cluster_operation_lease(&lease)"));
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
        assert!(maintenance.contains("release_cluster_operation_lease(&lease)"));
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
            |lease| async move { drop(lease) },
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
                move |lease| async move {
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
            .find("acquire_restart_preparation")
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
        assert!(source.contains("release_cluster_operation_lease(&lease)"));
        assert!(source.contains("release_restart_preparation(&node_id)"));
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
    fn stale_peer_sample_is_explicit_and_cannot_become_healthy() {
        let membership = test_membership("node-1", 2);
        let mut peer = test_local_status("node-2", Some(2));
        peer.observed_at_unix_ms = unix_ms().saturating_sub(MAX_FRESH_AGE_MS + 1);
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
