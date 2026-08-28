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
    ActivityPeer, ClusterNodeRecord, MembershipStatus, NodeRole, MAX_OPERATIONS_PEERS,
};
use plurx_core::cluster::migration::status::{
    DbSnapshotMetricsSnapshot, WalRuntimeState, WalStatusSnapshot,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::cluster::api_error;
use super::error::ApiError;
use super::extract::AdminUser;
use super::peer_transport::{
    deadline_after, exact_auth_from_headers, PeerAuthMode, PeerTransport, PeerTransportError,
};
use super::{evaluate_readiness, ReadinessEvaluation};
use crate::state::AppState;

pub(crate) const INTERNAL_PATH: &str = "/api/v1/internal/cluster/operations-status";
const AGGREGATE_TIMEOUT: Duration = Duration::from_secs(2);
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
    InvalidResponse,
    IdentityMismatch,
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
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<(HeaderMap, Json<ClusterOperationsAggregate>), ApiError> {
    let aggregate = collect_aggregate(&state).await?;
    Ok((private_no_store_headers(), Json(aggregate)))
}

pub(crate) async fn collect_aggregate(
    state: &AppState,
) -> Result<ClusterOperationsAggregate, ApiError> {
    let membership = state.membership.status().await.map_err(|_| {
        ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "cluster_roster_unavailable",
            "the committed cluster roster could not be read",
        )
    })?;
    let peers = state.membership.operations_peers().await.map_err(|_| {
        ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "cluster_peer_directory_unavailable",
            "the committed cluster peer directory could not be read",
        )
    })?;
    let observed_at_unix_ms = unix_ms();
    let local_status = local_snapshot(state).await;
    let transport = PeerTransport::new(state.membership.clone());
    let deadline = deadline_after(AGGREGATE_TIMEOUT);
    let remote = collect_peer_statuses(peers, transport, deadline).await;
    let observations = join_observations(
        &membership,
        &state.node_id,
        local_status,
        remote,
        observed_at_unix_ms,
    );
    let verdict = rollout_verdict(&membership, &observations);
    let maintenance = maintenance_entry_verdicts(&membership, &observations);
    Ok(ClusterOperationsAggregate {
        schema_version: 1,
        observed_at_unix_ms,
        membership,
        nodes: observations,
        verdict,
        maintenance,
    })
}

pub(crate) async fn support_bundle(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let aggregate = collect_aggregate(&state).await?;
    let status = serde_json::to_vec_pretty(&aggregate).map_err(bundle_error)?;
    let logs = state
        .cluster_logs
        .tail("trace", 200)
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
    let preflight = collect_aggregate(&state).await?;
    if !preflight.verdict.safe_to_restart_one {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_unsafe",
            "the current cluster rollout verdict does not permit preparing a voter",
        ));
    }
    if preflight.verdict.candidate_node_id.as_deref() != Some(state.node_id.as_str()) {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "restart_preparation_candidate_mismatch",
            "open the candidate node directly; this node is not the current safe restart candidate",
        ));
    }
    let duration = Duration::from_secs(request.expires_in_seconds.unwrap_or(900).clamp(60, 3_600));
    let lease = state
        .membership
        .acquire_restart_preparation(&node_id, duration)
        .await
        .map_err(api_error)?;
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
        serving: evaluate_readiness(state).await,
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
        media: media_drain_status(active_sessions, drain),
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
}

async fn collect_peer_statuses(
    peers: Vec<ActivityPeer>,
    transport: PeerTransport,
    deadline: tokio::time::Instant,
) -> BTreeMap<String, PeerStatusOutcome> {
    collect_peer_statuses_with(peers, deadline, move |peer| {
        let transport = transport.clone();
        async move {
            let node_id = peer.node_id.clone();
            if !peer.reachable {
                PeerStatusOutcome {
                    node_id,
                    state: ObservationState::Unreachable,
                    status: None,
                }
            } else if let Some(base) = peer.http_base {
                fetch_peer_status(&transport, &node_id, &base, deadline).await
            } else {
                PeerStatusOutcome {
                    node_id,
                    state: ObservationState::Unreachable,
                    status: None,
                }
            }
        }
    })
    .await
}

async fn collect_peer_statuses_with<F, Fut>(
    peers: Vec<ActivityPeer>,
    deadline: tokio::time::Instant,
    fetch: F,
) -> BTreeMap<String, PeerStatusOutcome>
where
    F: Fn(ActivityPeer) -> Fut + Clone,
    Fut: std::future::Future<Output = PeerStatusOutcome>,
{
    stream::iter(peers.into_iter().take(MAX_OPERATIONS_PEERS).map(|peer| {
        let fetch = fetch.clone();
        async move {
            let node_id = peer.node_id.clone();
            let outcome = tokio::time::timeout_at(deadline, fetch(peer))
                .await
                .unwrap_or_else(|_| PeerStatusOutcome {
                    node_id: node_id.clone(),
                    state: ObservationState::TimedOut,
                    status: None,
                });
            (node_id, outcome)
        }
    }))
    .buffer_unordered(MAX_OPERATIONS_PEERS)
    .collect()
    .await
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
        Err(PeerTransportError::TimedOut) => (ObservationState::TimedOut, None),
        Err(PeerTransportError::InvalidResponse) => (ObservationState::InvalidResponse, None),
        Ok(_) | Err(PeerTransportError::Unreachable) => (ObservationState::Unreachable, None),
    };
    PeerStatusOutcome {
        node_id: expected_node_id.to_owned(),
        state,
        status,
    }
}

fn join_observations(
    membership: &MembershipStatus,
    local_node_id: &str,
    mut local: ClusterNodeOperationsStatus,
    mut remote: BTreeMap<String, PeerStatusOutcome>,
    aggregate_observed_ms: u64,
) -> Vec<ClusterNodeObservation> {
    let mut rows = Vec::with_capacity(membership.nodes.len());
    for member in &membership.nodes {
        let (mut state, mut status, mut error_class) = if member.node_id == local_node_id {
            local.hostname = member.hostname.clone();
            (ObservationState::Answered, Some(local.clone()), None)
        } else if let Some(outcome) = remote.remove(&member.node_id) {
            let error = (outcome.state != ObservationState::Answered)
                .then(|| observation_error_class(outcome.state).to_owned());
            debug_assert_eq!(outcome.node_id, member.node_id);
            (outcome.state, outcome.status, error)
        } else {
            (
                ObservationState::PeerLimit,
                None,
                Some("peer_limit".to_owned()),
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
        });
    }
    rows
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
        ObservationState::InvalidResponse => "invalid_response",
        ObservationState::IdentityMismatch => "identity_mismatch",
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

    #[tokio::test(start_paused = true)]
    async fn peer_fanout_uses_one_shared_deadline() {
        let peers = (0..MAX_OPERATIONS_PEERS)
            .map(|index| ActivityPeer {
                node_id: format!("node-{index}"),
                http_base: Some(format!("http://node-{index}:8080")),
                reachable: true,
            })
            .collect();
        let started = tokio::time::Instant::now();
        let deadline = started + Duration::from_secs(2);
        let outcomes = collect_peer_statuses_with(peers, deadline, |peer| async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            PeerStatusOutcome {
                node_id: peer.node_id,
                state: ObservationState::Answered,
                status: None,
            }
        })
        .await;

        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_secs(2)
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
                http_base: Some(format!("http://node-{index}:8080")),
                reachable: true,
            })
            .collect();
        let outcomes = collect_peer_statuses_with(
            peers,
            tokio::time::Instant::now() + Duration::from_secs(2),
            |peer| async move {
                PeerStatusOutcome {
                    node_id: peer.node_id,
                    state: ObservationState::Answered,
                    status: None,
                }
            },
        )
        .await;

        assert_eq!(outcomes.len(), MAX_OPERATIONS_PEERS);
        assert_eq!(MAX_OPERATIONS_PEERS, 8);
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
        let status = test_local_status("node-a", Some(8));
        let mut remote = BTreeMap::new();
        remote.insert(
            "node-a".to_owned(),
            PeerStatusOutcome {
                node_id: "node-a".to_owned(),
                state: ObservationState::Answered,
                status: Some(status),
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
