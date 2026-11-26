//! Narrow cluster-only activity snapshot transport.
//!
//! This route is deliberately outside `/api/v1`: household credentials do
//! not authorize it, and its peer addresses and authority never enter a
//! public response. The aggregation/UI remains in `system.rs`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::ActivityPeer;
use serde::{Deserialize, Serialize};

use super::peer_transport::{deadline_after, PeerAuthMode, PeerTransport, PeerTransportError};
#[cfg(test)]
use super::peer_transport::{NODE_HEADER, SIGNATURE_HEADER, TARGET_HEADER, TIMESTAMP_HEADER};
use crate::cluster::{ClusterError, Directory, Purpose};
use crate::state::AppState;

pub const PATH: &str = "/_internal/v1/activity-snapshot";
pub const SOURCE_STOP_PATH: &str = "/_internal/v1/activity-source-processed-stop";
#[allow(dead_code)] // aggregation child #326 calls the pre-wired client
pub const TIMEOUT: Duration = Duration::from_secs(2);
const SOURCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const ACTIVITY_SNAPSHOT_REUSE: Duration = Duration::from_secs(1);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
// Ancillary family maxima. The allocator charges actual serialized rows
// before admitting analysis and deliveries; unused capacity is shared.
const LIVE_TV_RESPONSE_RESERVE_BYTES: usize = 24 * 1024;
const DVR_RESPONSE_RESERVE_BYTES: usize = 64 * 1024;
const WORKER_RESPONSE_RESERVE_BYTES: usize = 64 * 1024;
// Analysis may use at most a quarter of the wire; a first delivery is held
// aside before analysis takes any of the remaining space.
const ANALYSIS_RESPONSE_RESERVE_BYTES: usize = MAX_RESPONSE_BYTES / 4;
const MAX_DELIVERIES: usize = 512;
const MAX_ANALYSIS_PROGRESS: usize = 64;
const MAX_SOURCE_PROCESSED: usize = 64;
const SOURCE_RESPONSE_RESERVE_BYTES: usize = 32 * 1024;
const MAX_NODE_ID_BYTES: usize = 256;
const MAX_USER_BYTES: usize = 256;
const MAX_TITLE_BYTES: usize = 2 * 1024;
const PEER_CONCURRENCY: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActivitySnapshot {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<Box<ActivityWorkers>>,
    pub deliveries: Vec<ActivityDelivery>,
    #[serde(default)]
    pub source_processed: Vec<SourceProcessedRow>,
    #[serde(default)]
    pub receiver_processed: Vec<ReceiverProcessedRow>,
    #[serde(default)]
    pub analysis: Vec<crate::state::AnalysisProgress>,
    #[serde(default)]
    pub(crate) live_tv: Vec<crate::live_tv::LiveTvActivity>,
    /// Absent means this peer predates runtime DVR observations. A present,
    /// empty snapshot means the peer supports them and currently owns no sink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dvr: Option<crate::live_tv::dvr::DvrObservationSnapshot>,
}

/// Source-owned hardware work, including the trial before a VOD session exists.
/// These ids are one-way diagnostics, never a playback capability.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceProcessedRow {
    pub stop_id: String,
    pub session_id: Option<String>,
    pub node_id: String,
    pub title: String,
    pub viewer: String,
    pub grant: String,
    pub file_id: i64,
    pub item_id: i64,
    pub started_unix: i64,
    pub reason: String,
}

/// B's retained processed successor. Source server is the work location;
/// receiver_node_id is where the exact B retirement owner lives.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiverProcessedRow {
    pub stop_id: String,
    pub receiver_node_id: String,
    pub source_server_id: String,
    pub source_name: String,
    pub title: String,
    pub viewer: String,
    pub reason: String,
}

fn valid_receiver_stop_id(id: &str) -> bool {
    id.strip_prefix("receiver-s-").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

pub(super) async fn local_receiver_processed(state: &AppState) -> Vec<ReceiverProcessedRow> {
    let activities = state.sharing.receiver_starts.processed_activities();
    if activities.is_empty() {
        return Vec::new();
    }
    let imports = state.store.sharing_imports().await.unwrap_or_default();
    activities
        .into_iter()
        .filter(|row| valid_receiver_stop_id(&row.stop_id))
        .map(|row| {
            let source_name = imports
                .iter()
                .find(|import| {
                    import.id == row.import_id
                        && import.source_server_id == row.source_server_id
                        && import.catalogue_epoch == row.catalogue_epoch
                        && import.state == "active"
                })
                .map(|import| bounded_text(import.source_name.clone(), MAX_USER_BYTES))
                .unwrap_or_else(|| row.source_server_id.to_string());
            ReceiverProcessedRow {
                stop_id: row.stop_id,
                receiver_node_id: state.node_id.clone(),
                source_server_id: row.source_server_id.to_string(),
                source_name,
                title: bounded_text(format!("Shared item {}", row.item_id), MAX_TITLE_BYTES),
                viewer: format!("User #{}", row.viewer_id),
                reason: "Processed Dolby Vision upgrade runs on Source".to_owned(),
            }
        })
        .collect()
}

fn valid_source_stop_id(id: &str) -> bool {
    id.strip_prefix("source-s-").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

pub(super) async fn local_source_processed(state: &AppState) -> Vec<SourceProcessedRow> {
    let mut activities = state.transcode.source_processed_activities();
    activities.sort_by_key(|row| std::cmp::Reverse(row.started_unix));
    activities.truncate(MAX_SOURCE_PROCESSED);
    if activities.is_empty() {
        return Vec::new();
    }
    let ids = activities.iter().map(|row| row.item_id).collect::<Vec<_>>();
    let titles = state.store.item_titles(&ids).await.unwrap_or_default();
    activities
        .into_iter()
        .filter(|row| valid_source_stop_id(&row.stop_id))
        .map(|row| SourceProcessedRow {
            stop_id: row.stop_id,
            session_id: row.session_id,
            node_id: state.node_id.clone(),
            title: bounded_text(
                titles.get(&row.item_id).cloned().unwrap_or(row.title),
                MAX_TITLE_BYTES,
            ),
            viewer: bounded_text(row.viewer, MAX_USER_BYTES),
            grant: bounded_text(row.grant, 64),
            file_id: row.file_id,
            item_id: row.item_id,
            started_unix: row.started_unix,
            reason: bounded_text(row.reason, 256),
        })
        .collect()
}

/// Bounded physical observations, not a promise that a particular job can run.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityWorkers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subtitles: Option<crate::subtitle_models::WorkerObservation>,
    /// Exact current indexing engine; absent on peers predating reconciliation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_engine: Option<String>,
    pub observed_at_ms: i64,
    pub heavy_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_batch_limit: Option<usize>,
    pub heavy_in_use: usize,
    pub heavy_available: usize,
    pub accepting_work: bool,
    pub hardware_used: usize,
    pub hardware_limit: usize,
    pub software_used: usize,
    pub software_limit: usize,
    pub children: Vec<String>,
    pub child_count: usize,
}

impl ActivityWorkers {
    /// Member-scoped operations carry capacity, never detailed process or
    /// model/engine evidence from the voter-scoped Activity channel.
    pub(crate) fn capacity_only(mut self) -> Self {
        self.subtitles = None;
        self.analysis_engine = None;
        self.children.clear();
        self
    }

    pub(crate) fn bounded(&self, expected_node_id: &str) -> bool {
        self.subtitles
            .as_ref()
            .is_none_or(|observation| observation.bounded(expected_node_id))
            && self.children.len() <= 32
            && self.analysis_engine.as_ref().is_none_or(|engine| {
                engine.len() == 64 && engine.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            && self.children.iter().all(|child| child.len() <= 256)
            && self.child_count >= self.children.len()
            && self.heavy_in_use <= self.heavy_limit
            && self
                .probe_batch_limit
                .is_none_or(|limit| (1..=2).contains(&limit))
            && self.heavy_available <= self.heavy_limit.saturating_sub(self.heavy_in_use)
    }
}

pub(super) async fn local_workers(state: &AppState) -> ActivityWorkers {
    local_workers_with_details(state, true).await
}

pub(super) async fn local_worker_capacity(state: &AppState) -> ActivityWorkers {
    local_workers_with_details(state, false).await
}

async fn local_workers_with_details(state: &AppState, detailed: bool) -> ActivityWorkers {
    let runtime = state.transcode.media_node_runtime().await;
    let accepting_work =
        state.cluster.authority().state().ready && !state.cluster.maintenance_active();
    let children = plurx_core::process::priority::running();
    let heavy_in_use = usize::from(state.transcode.background_worker_in_use());
    ActivityWorkers {
        subtitles: if detailed {
            state.subtitle_models.local_observation().await
        } else {
            None
        },
        analysis_engine: if detailed && crate::ffmpeg::fragment_index_engine_is_current().await {
            Some(crate::ffmpeg::fragment_index_engine_digest().await)
        } else {
            None
        },
        observed_at_ms: crate::state::clock_ms(),
        heavy_limit: crate::transcode::BACKGROUND_HEAVY_LIMIT,
        probe_batch_limit: Some(state.transcode.software_budget().await.clamp(1, 2)),
        heavy_in_use,
        heavy_available: usize::from(
            heavy_in_use == 0 && accepting_work && state.transcode.fragment_worker_may_start(),
        ),
        accepting_work,
        hardware_used: runtime.hardware_slots_used,
        hardware_limit: runtime.hardware_slots_max,
        software_used: runtime.software_threads_used,
        software_limit: runtime.software_threads_max,
        child_count: children.len(),
        children: if detailed {
            children
                .into_iter()
                .take(32)
                .map(|child| {
                    bounded_text(format!("{} · {}", child.class.as_str(), child.purpose), 256)
                })
                .collect()
        } else {
            Vec::new()
        },
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityDelivery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vod_observation: Option<crate::vodserve::VodActivityObservation>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<String>,
    pub user: String,
    pub file_id: i64,
    pub item_id: i64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub started_unix: i64,
    pub idle_seconds: u64,
    pub delivered_bytes: Option<i64>,
    pub delivered_bps: Option<i64>,
}

#[allow(dead_code)] // consumed by #326 once this dependency merges
#[derive(Clone, Debug, PartialEq)]
pub enum PeerActivityOutcome {
    Answered(Box<ActivitySnapshot>),
    Unhealthy,
    Unsupported,
    Unreachable,
    LocalUnavailable,
    TimedOut,
    Refused,
    HttpError,
    InvalidResponse,
}

pub type SharedPeerActivity = Arc<[(String, PeerActivityOutcome)]>;

fn age_dvr_observations(
    mut outcome: PeerActivityOutcome,
    elapsed: Duration,
) -> PeerActivityOutcome {
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    if let PeerActivityOutcome::Answered(snapshot) = &mut outcome {
        for delivery in &mut snapshot.deliveries {
            if let Some(age) = delivery
                .vod_observation
                .as_mut()
                .and_then(|facts| facts.control_age_ms.as_mut())
            {
                *age = age.saturating_add(elapsed_ms);
            }
        }
        if let Some(dvr) = &mut snapshot.dvr {
            for observation in &mut dvr.observations {
                observation.observation_age_ms =
                    observation.observation_age_ms.saturating_add(elapsed_ms);
            }
        }
    }
    outcome
}

/// Whether re-aging this entry would move anything.
///
/// [`age_dvr_observations`] only ever advances capture observations, so an
/// outcome without any is byte-identical before and after. Asking first is
/// what lets the cache hand out the allocation the single-flight gate exists
/// to share instead of deep-cloning every peer's whole snapshot per reader.
fn carries_dvr_observations(entry: &(String, PeerActivityOutcome)) -> bool {
    match &entry.1 {
        PeerActivityOutcome::Answered(snapshot) => {
            snapshot
                .deliveries
                .iter()
                .any(|delivery| delivery.vod_observation.is_some())
                || snapshot
                    .dvr
                    .as_ref()
                    .is_some_and(|dvr| !dvr.observations.is_empty())
        }
        _ => false,
    }
}

#[derive(Default)]
struct PeerActivityReadGate {
    last_started: Option<tokio::time::Instant>,
    completed: Option<(tokio::time::Instant, SharedPeerActivity)>,
}

static ACTIVITY_AGGREGATIONS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static ACTIVITY_PEER_OUTCOMES: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];

fn record_aggregation(path: usize) {
    ACTIVITY_AGGREGATIONS[path].fetch_add(1, Ordering::Relaxed);
}

fn record_peer_outcome(outcome: &PeerActivityOutcome) {
    let slot = match outcome {
        PeerActivityOutcome::Answered(_) => 0,
        PeerActivityOutcome::Unhealthy => 1,
        PeerActivityOutcome::Unsupported => 2,
        PeerActivityOutcome::Unreachable => 3,
        PeerActivityOutcome::TimedOut => 4,
        PeerActivityOutcome::Refused => 5,
        PeerActivityOutcome::HttpError => 6,
        PeerActivityOutcome::InvalidResponse => 7,
        PeerActivityOutcome::LocalUnavailable => 8,
    };
    ACTIVITY_PEER_OUTCOMES[slot].fetch_add(1, Ordering::Relaxed);
}

/// Render process-lifetime Activity fanout and peer-outcome counters.
///
/// This reads atomics only: scraping `/metrics` never acquires the Activity
/// gate, resolves membership, or touches the Store.
#[must_use]
pub fn prometheus_cluster_activity() -> String {
    let mut out = String::from(
        "# HELP plurx_cluster_activity_aggregations_total Cluster Activity aggregations by execution path.\n\
         # TYPE plurx_cluster_activity_aggregations_total counter\n",
    );
    for (slot, path) in ["fanout", "reuse", "directory_error"].iter().enumerate() {
        out.push_str(&format!(
            "plurx_cluster_activity_aggregations_total{{path=\"{path}\"}} {}\n",
            ACTIVITY_AGGREGATIONS[slot].load(Ordering::Relaxed)
        ));
    }
    out.push_str(
        "# HELP plurx_cluster_activity_peer_outcomes_total Physical Activity peer reads by typed outcome.\n\
         # TYPE plurx_cluster_activity_peer_outcomes_total counter\n",
    );
    for (slot, outcome) in [
        "answered",
        "unhealthy",
        "unsupported",
        "unreachable",
        "timed_out",
        "refused",
        "http_error",
        "invalid_response",
        "local_unavailable",
    ]
    .iter()
    .enumerate()
    {
        out.push_str(&format!(
            "plurx_cluster_activity_peer_outcomes_total{{outcome=\"{outcome}\"}} {}\n",
            ACTIVITY_PEER_OUTCOMES[slot].load(Ordering::Relaxed)
        ));
    }
    out
}

#[allow(dead_code)] // wired now so #326 does not need state/main territory
#[derive(Clone)]
pub struct PeerActivityClient {
    directory: Directory,
    transport: PeerTransport,
    reads: Arc<tokio::sync::Mutex<PeerActivityReadGate>>,
    owner_reads:
        Arc<tokio::sync::Mutex<HashMap<String, (tokio::time::Instant, PeerActivityOutcome)>>>,
}

#[derive(Debug)]
pub(crate) enum PeerActivityStopError {
    Directory(ClusterError),
    Transport(PeerTransportError),
}

impl std::fmt::Display for PeerActivityStopError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Directory(error) => write!(formatter, "{error}"),
            Self::Transport(error) => write!(formatter, "peer transport: {error:?}"),
        }
    }
}

impl std::error::Error for PeerActivityStopError {}

#[allow(dead_code)]
impl PeerActivityClient {
    #[must_use]
    pub(crate) fn new(transport: PeerTransport, directory: Directory) -> Self {
        Self {
            transport,
            directory,
            reads: Arc::new(tokio::sync::Mutex::new(PeerActivityReadGate::default())),
            owner_reads: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn snapshots(&self) -> Result<SharedPeerActivity, ClusterError> {
        let directory = self.clone();
        let fetcher = self.clone();
        shared_peer_activity(
            &self.reads,
            move || async move {
                directory
                    .directory
                    .peers(Purpose::Operations)
                    .await
                    .inspect_err(|_| record_aggregation(2))
                    .map(|peers| peers.all().to_vec())
            },
            move |peers| async move {
                record_aggregation(0);
                let deadline = deadline_after(TIMEOUT);
                let client = fetcher.clone();
                let outcomes = collect_peer_outcomes(peers, deadline, move |peer| {
                    fetch_peer_activity(client.clone(), peer, deadline)
                })
                .await;
                for (_, outcome) in &outcomes {
                    record_peer_outcome(outcome);
                }
                outcomes
            },
        )
        .await
    }

    /// Read only the configured recorder owner. The one-second result cache
    /// and the held async lock make concurrent overview requests one physical
    /// peer read without turning the lightweight DVR poll into an all-node
    /// Activity fan-out.
    pub(crate) async fn owner_snapshot(
        &self,
        owner_node_id: &str,
    ) -> Result<PeerActivityOutcome, ClusterError> {
        let mut gate = self.owner_reads.lock().await;
        if let Some((completed_at, outcome)) = gate.get(owner_node_id) {
            if completed_at.elapsed() <= ACTIVITY_SNAPSHOT_REUSE {
                return Ok(age_dvr_observations(
                    outcome.clone(),
                    completed_at.elapsed(),
                ));
            }
        }
        let peers = self.directory.peers(Purpose::Operations).await?;
        let peer = peers
            .all()
            .iter()
            .find(|peer| peer.node_id == owner_node_id);
        let outcome = match peer {
            Some(peer) => {
                let deadline = deadline_after(TIMEOUT);
                fetch_peer_activity(self.clone(), peer.clone(), deadline).await
            }
            None => PeerActivityOutcome::Unsupported,
        };
        record_peer_outcome(&outcome);
        gate.clear();
        gate.insert(
            owner_node_id.to_owned(),
            (tokio::time::Instant::now(), outcome.clone()),
        );
        Ok(outcome)
    }

    async fn snapshot(
        &self,
        expected_node_id: &str,
        base: &str,
        deadline: tokio::time::Instant,
    ) -> PeerActivityOutcome {
        match self
            .transport
            .request(
                expected_node_id,
                base,
                reqwest::Method::GET,
                PATH,
                Vec::new(),
                deadline,
                MAX_RESPONSE_BYTES,
                PeerAuthMode::ExactRequestAndMemberResponse,
            )
            .await
        {
            Ok(response) => {
                classify_snapshot_response(response.status, &response.body, expected_node_id)
            }
            Err(PeerTransportError::TimedOut) => PeerActivityOutcome::TimedOut,
            Err(PeerTransportError::InvalidResponse) => PeerActivityOutcome::InvalidResponse,
            Err(PeerTransportError::Unreachable) => PeerActivityOutcome::Unreachable,
            Err(PeerTransportError::LocalUnavailable) => PeerActivityOutcome::LocalUnavailable,
            Err(PeerTransportError::AuthenticationRefused(_)) => PeerActivityOutcome::Refused,
            Err(PeerTransportError::UnauthenticatedStatus(status)) => {
                unauthenticated_status_outcome(status)
            }
        }
    }

    /// Route one admin Stop only to the committed member named by a fresh
    /// Activity row. Membership supplies the address; the signed body binds
    /// both that node and the exact opaque Source operation id.
    pub(crate) async fn stop_processed_on(
        &self,
        owner_node_id: &str,
        stop_id: &str,
    ) -> Result<StatusCode, PeerActivityStopError> {
        if !valid_source_stop_id(stop_id) && !valid_receiver_stop_id(stop_id) {
            return Err(PeerActivityStopError::Transport(
                PeerTransportError::InvalidResponse,
            ));
        }
        let (_, base) = self
            .directory
            .peers(Purpose::Operations)
            .await
            .map_err(PeerActivityStopError::Directory)?
            .find(owner_node_id)
            .ok_or(PeerActivityStopError::Transport(
                PeerTransportError::Unreachable,
            ))?;
        let body = serde_json::to_vec(&SourceStopRequest {
            node_id: owner_node_id.to_owned(),
            stop_id: stop_id.to_owned(),
        })
        .map_err(|_| PeerActivityStopError::Transport(PeerTransportError::InvalidResponse))?;
        let response = self
            .transport
            .request(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                SOURCE_STOP_PATH,
                body,
                deadline_after(if valid_receiver_stop_id(stop_id) {
                    Duration::from_secs(40)
                } else {
                    SOURCE_STOP_TIMEOUT
                }),
                1_024,
                PeerAuthMode::ExactRequestAndMemberResponse,
            )
            .await
            .map_err(PeerActivityStopError::Transport)?;
        match response.status {
            reqwest::StatusCode::NO_CONTENT
            | reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::SERVICE_UNAVAILABLE
                if response.body.is_empty() =>
            {
                Ok(StatusCode::from_u16(response.status.as_u16())
                    .expect("known Source stop status"))
            }
            _ => Err(PeerActivityStopError::Transport(
                PeerTransportError::InvalidResponse,
            )),
        }
    }
}

async fn fetch_peer_activity(
    client: PeerActivityClient,
    peer: ActivityPeer,
    deadline: tokio::time::Instant,
) -> PeerActivityOutcome {
    let started = tokio::time::Instant::now();
    let outcome = if !peer.reachable {
        PeerActivityOutcome::Unhealthy
    } else if let Some(http_base) = peer.http_base {
        client.snapshot(&peer.node_id, &http_base, deadline).await
    } else {
        PeerActivityOutcome::Unsupported
    };
    age_dvr_observations(outcome, started.elapsed())
}

async fn shared_peer_activity<R, RFut, T, F, Fut>(
    gate: &tokio::sync::Mutex<PeerActivityReadGate>,
    resolve: R,
    fetch: F,
) -> Result<SharedPeerActivity, ClusterError>
where
    R: FnOnce() -> RFut,
    RFut: Future<Output = Result<T, ClusterError>>,
    F: FnOnce(T) -> Fut,
    Fut: Future<Output = Vec<(String, PeerActivityOutcome)>>,
{
    let mut gate = gate.lock().await;
    let now = tokio::time::Instant::now();
    if let Some((completed_at, snapshot)) = &gate.completed {
        if now.saturating_duration_since(*completed_at) < ACTIVITY_SNAPSHOT_REUSE {
            record_aggregation(1);
            let age = now.saturating_duration_since(*completed_at);
            // Rebuild only when re-aging would actually move something.
            // Otherwise every reader on a cache hit allocated a fresh Vec and
            // cloned every peer's whole snapshot, which is exactly the sharing
            // this gate exists to provide.
            if age.is_zero() || !snapshot.iter().any(carries_dvr_observations) {
                return Ok(Arc::clone(snapshot));
            }
            return Ok(snapshot
                .iter()
                .cloned()
                .map(|(node, outcome)| (node, age_dvr_observations(outcome, age)))
                .collect::<Vec<_>>()
                .into());
        }
    }
    if let Some(last_started) = gate.last_started {
        tokio::time::sleep_until(last_started + ACTIVITY_SNAPSHOT_REUSE).await;
    }
    // Reserve the attempt before the first fallible await. Cancellation during
    // directory resolution still leaves a bounded retry floor.
    gate.last_started = Some(tokio::time::Instant::now());
    let resolved = resolve().await?;
    // Directory resolution may itself take longer than the floor. Refresh the
    // reservation at the actual fanout boundary so cancellation just after a
    // signed HTTP request starts cannot let a successor burst immediately.
    gate.last_started = Some(tokio::time::Instant::now());
    let snapshot: SharedPeerActivity = fetch(resolved).await.into();
    gate.completed = Some((tokio::time::Instant::now(), Arc::clone(&snapshot)));
    Ok(snapshot)
}

#[cfg(test)]
async fn read_snapshot_response(
    response: reqwest::Response,
    deadline: tokio::time::Instant,
    expected_node_id: &str,
) -> PeerActivityOutcome {
    let status = response.status();
    if !status.is_success() {
        return classify_snapshot_response(status, &[], expected_node_id);
    }
    match super::peer_transport::read_bounded(response, deadline, MAX_RESPONSE_BYTES).await {
        Ok(response) => {
            classify_snapshot_response(response.status, &response.body, expected_node_id)
        }
        Err(PeerTransportError::TimedOut) => PeerActivityOutcome::TimedOut,
        Err(PeerTransportError::InvalidResponse) => PeerActivityOutcome::InvalidResponse,
        Err(PeerTransportError::Unreachable) => PeerActivityOutcome::Unreachable,
        Err(PeerTransportError::LocalUnavailable) => PeerActivityOutcome::LocalUnavailable,
        Err(PeerTransportError::AuthenticationRefused(_)) => PeerActivityOutcome::Refused,
        Err(PeerTransportError::UnauthenticatedStatus(status)) => {
            unauthenticated_status_outcome(status)
        }
    }
}

/// An unsigned error status is an HTTP error on the path to the peer: never a
/// snapshot, and never the peer's own authentication refusal (an unsigned 403
/// may come from any hop).
fn unauthenticated_status_outcome(status: u16) -> PeerActivityOutcome {
    match StatusCode::from_u16(status) {
        Ok(status) if !status.is_success() => PeerActivityOutcome::HttpError,
        _ => PeerActivityOutcome::InvalidResponse,
    }
}

fn classify_snapshot_response(
    status: StatusCode,
    body: &[u8],
    expected_node_id: &str,
) -> PeerActivityOutcome {
    if status.is_success() {
        decode_snapshot(body, expected_node_id)
    } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        PeerActivityOutcome::Refused
    } else {
        PeerActivityOutcome::HttpError
    }
}

async fn collect_peer_outcomes<F, Fut>(
    peers: Vec<ActivityPeer>,
    deadline: tokio::time::Instant,
    fetch: F,
) -> Vec<(String, PeerActivityOutcome)>
where
    F: Fn(ActivityPeer) -> Fut + Clone,
    Fut: Future<Output = PeerActivityOutcome>,
{
    let mut outcomes = stream::iter(peers.into_iter().map(|peer| {
        let fetch = fetch.clone();
        async move {
            let node_id = peer.node_id.clone();
            let outcome = tokio::time::timeout_at(deadline, fetch(peer))
                .await
                .unwrap_or(PeerActivityOutcome::TimedOut);
            (node_id, outcome)
        }
    }))
    .buffer_unordered(PEER_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    outcomes.sort_by(|left, right| left.0.cmp(&right.0));
    outcomes
}

pub async fn snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let auth =
        super::peer_transport::exact_auth_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if !state
        .cluster
        .authentication()
        .authorize_read_request(&auth, "GET", PATH, &[])
        .await
        .unwrap_or(false)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let body = serde_json::to_vec(&local_snapshot(&state).await)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    signed_activity_response(&state, &auth, PATH, StatusCode::OK, body)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceStopRequest {
    node_id: String,
    stop_id: String,
}

pub async fn stop_source_processed(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    if body.len() > 256 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let auth =
        super::peer_transport::exact_auth_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if !state
        .cluster
        .authentication()
        .authorize_exact_request(&auth, "POST", SOURCE_STOP_PATH, &body)
        .await
        .unwrap_or(false)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let request: SourceStopRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if request.node_id != state.node_id
        || (!valid_source_stop_id(&request.stop_id) && !valid_receiver_stop_id(&request.stop_id))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let stopped = if valid_receiver_stop_id(&request.stop_id) {
        state
            .sharing
            .receiver_starts
            .stop_processed_activity(std::sync::Arc::new(state.clone()), &request.stop_id)
            .await
    } else {
        state
            .transcode
            .stop_source_processed_session(&request.stop_id)
            .await
            .map_err(|_| ())
    };
    let status = match stopped {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    signed_activity_response(&state, &auth, SOURCE_STOP_PATH, status, Vec::new())
}

fn signed_activity_response(
    state: &AppState,
    auth: &plurx_core::cluster::membership::InternalPeerAuth,
    path: &str,
    status: StatusCode,
    body: Vec<u8>,
) -> Result<Response, StatusCode> {
    let payload = super::peer_transport::signed_response_payload(status.as_u16(), &body);
    let signature = state
        .cluster
        .authentication()
        .sign_exact_response(&auth.node_id, &auth.nonce, path, &payload)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let mut response = (status, body).into_response();
    response.headers_mut().extend(snapshot_response_headers());
    response.headers_mut().insert(
        super::peer_transport::RESPONSE_SIGNATURE_HEADER,
        HeaderValue::from_str(&signature).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
    );
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(response)
}

fn snapshot_response_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    headers
}

#[cfg(test)]
fn activity_url(base: &str) -> Option<reqwest::Url> {
    super::peer_transport::peer_url(base, PATH)
}

fn decode_snapshot(body: &[u8], expected_node_id: &str) -> PeerActivityOutcome {
    let Ok(snapshot) = serde_json::from_slice::<ActivitySnapshot>(body) else {
        return PeerActivityOutcome::InvalidResponse;
    };
    if !snapshot_is_bounded(&snapshot, expected_node_id) {
        return PeerActivityOutcome::InvalidResponse;
    }
    PeerActivityOutcome::Answered(Box::new(snapshot))
}

fn snapshot_is_bounded(snapshot: &ActivitySnapshot, expected_node_id: &str) -> bool {
    snapshot.node_id == expected_node_id
        && !snapshot.node_id.is_empty()
        && snapshot.node_id.len() <= MAX_NODE_ID_BYTES
        && snapshot
            .workers
            .as_ref()
            .is_none_or(|workers| workers.bounded(expected_node_id))
        && snapshot.deliveries.len() <= MAX_DELIVERIES
        && snapshot.source_processed.len() <= MAX_SOURCE_PROCESSED
        && snapshot.receiver_processed.len() <= MAX_SOURCE_PROCESSED
        && snapshot.analysis.len() <= MAX_ANALYSIS_PROGRESS
        && snapshot.live_tv.len() <= 4
        && snapshot.dvr.as_ref().is_none_or(|dvr| {
            dvr.observations.len() <= crate::live_tv::dvr::DVR_OBSERVATIONS_MAX
                && dvr.observed_sink_count >= dvr.observations.len()
                && dvr.observations.iter().all(|observation| {
                    observation.owner_node_id == snapshot.node_id
                        && !observation.recording_id.is_empty()
                        && observation.recording_id.len() <= 128
                        && !observation.channel_id.is_empty()
                        && observation.channel_id.len() <= 256
                        && observation.config_generation >= 0
                        && observation.attempt >= 1
                        && !observation.phase.is_empty()
                        && observation.phase.len() <= 32
                        && observation
                            .reason_code
                            .as_ref()
                            .is_none_or(|reason| reason.len() <= 64)
                })
        })
        && snapshot.live_tv.iter().all(|live| {
            live.owner_node_id == snapshot.node_id
                && live.channel_number.len() <= 32
                && live.channel_name.len() <= 256
                && live.user.len() <= MAX_USER_BYTES
                && live.encoder.len() <= 32
                && live.state.len() <= 32
                && live
                    .programme_title
                    .as_ref()
                    .is_none_or(|title| title.len() <= 256)
        })
        && snapshot.deliveries.iter().all(|delivery| {
            delivery.method.len() <= 32
                && delivery.user.len() <= MAX_USER_BYTES
                && delivery.title.len() <= MAX_TITLE_BYTES
                && delivery.session_id.as_ref().is_none_or(|id| {
                    id.strip_prefix("s-").is_some_and(|digest| {
                        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                })
                && delivery
                    .vod_observation
                    .as_ref()
                    .is_none_or(|facts| facts.bounded())
        })
        && snapshot.source_processed.iter().all(|row| {
            row.node_id == snapshot.node_id
                && valid_source_stop_id(&row.stop_id)
                && row.session_id.as_ref().is_none_or(|id| {
                    id.strip_prefix("s-").is_some_and(|digest| {
                        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                })
                && row.title.len() <= MAX_TITLE_BYTES
                && row.viewer.len() <= MAX_USER_BYTES
                && row.grant.len() <= 64
                && row.reason.len() <= 256
        })
        && snapshot.receiver_processed.iter().all(|row| {
            row.receiver_node_id == snapshot.node_id
                && valid_receiver_stop_id(&row.stop_id)
                && row.source_server_id.len() == 36
                && row.source_name.len() <= MAX_USER_BYTES
                && row.title.len() <= MAX_TITLE_BYTES
                && row.viewer.len() <= MAX_USER_BYTES
                && row.reason.len() <= 256
        })
        && snapshot.analysis.iter().all(|progress| {
            !progress.job_id.is_empty()
                && progress.job_id.len() <= 128
                && match (&progress.durable_job_id, progress.durable_fence) {
                    (None, None) => true,
                    (Some(id), Some(fence)) => {
                        uuid::Uuid::parse_str(id).is_ok() && id.len() <= 36 && fence > 0
                    }
                    _ => false,
                }
                && progress.component.len() <= 32
                && progress.stage.len() <= 32
                && progress.title.len() <= MAX_TITLE_BYTES
        })
}

fn bounded_text(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
        .last()
        .unwrap_or_default();
    value[..boundary].to_owned()
}

async fn local_snapshot(state: &AppState) -> ActivitySnapshot {
    let transcodes = state
        .transcode
        .delivery_candidates_bounded(MAX_DELIVERIES)
        .await;
    let vod = state
        .transcode
        .vod_delivery_infos_bounded(MAX_DELIVERIES)
        .await;
    let candidates = activity_candidates_bounded(
        transcodes,
        vod,
        state.streams.list_bounded(MAX_DELIVERIES),
        state.direct_plays.list_bounded(MAX_DELIVERIES),
        MAX_DELIVERIES,
    );
    let transcode_ids = candidates
        .iter()
        .filter_map(|candidate| match candidate {
            ActivityCandidate::Transcode(candidate) => Some(candidate.id.clone()),
            ActivityCandidate::Vod(_)
            | ActivityCandidate::Remux(_)
            | ActivityCandidate::Direct(_) => None,
        })
        .collect::<Vec<_>>();
    let mut transcode_details = state
        .transcode
        .delivery_details_bounded(&transcode_ids, MAX_DELIVERIES)
        .await
        .into_iter()
        .map(|detail| (detail.0.id.clone(), detail))
        .collect::<HashMap<_, _>>();
    let mut deliveries = Vec::with_capacity(candidates.len());
    let titles = candidate_titles(&candidates, |ids| async move {
        state.store.item_titles(&ids).await
    })
    .await
    .unwrap_or_default();
    for candidate in candidates {
        match candidate {
            ActivityCandidate::Transcode(candidate) => {
                let Some((session, method)) = transcode_details.remove(&candidate.id) else {
                    continue;
                };
                deliveries.push(ActivityDelivery {
                    vod_observation: session.vod_observation.clone(),
                    method: method.as_str().to_owned(),
                    presentation: Some(session.presentation.to_owned()),
                    user: bounded_text(session.user_name, MAX_USER_BYTES),
                    file_id: session.file_id,
                    item_id: session.item_id,
                    title: bounded_text(session.item_title, MAX_TITLE_BYTES),
                    session_id: Some(crate::transcode::session_log_id(&session.id)),
                    started_unix: session.started_unix,
                    idle_seconds: session.idle_seconds,
                    delivered_bytes: Some(session.delivered_bytes),
                    delivered_bps: session.delivered_bps,
                });
            }
            ActivityCandidate::Vod(session) => {
                let session = *session;
                deliveries.push(ActivityDelivery {
                    vod_observation: session.observation.clone(),
                    method: session.method.as_str().to_owned(),
                    presentation: Some("vod".to_owned()),
                    user: bounded_text(session.user_name, MAX_USER_BYTES),
                    file_id: session.file_id,
                    item_id: session.item_id,
                    title: bounded_text(session.item_title, MAX_TITLE_BYTES),
                    session_id: Some(crate::transcode::session_log_id(&session.id)),
                    started_unix: session.started_unix,
                    idle_seconds: session.idle_seconds,
                    delivered_bytes: Some(session.delivered_bytes),
                    delivered_bps: session.delivered_bps,
                });
            }
            ActivityCandidate::Remux(stream) => {
                deliveries.push(ActivityDelivery {
                    vod_observation: None,
                    method: "remux".to_owned(),
                    presentation: None,
                    user: bounded_text(stream.user_name, MAX_USER_BYTES),
                    file_id: stream.file_id,
                    item_id: stream.item_id,
                    title: bounded_text(
                        titles.get(&stream.item_id).cloned().unwrap_or_default(),
                        MAX_TITLE_BYTES,
                    ),
                    session_id: None,
                    started_unix: stream.started_unix,
                    idle_seconds: (stream.delivered_idle_ms.max(0) / 1_000) as u64,
                    delivered_bytes: Some(stream.delivered_bytes),
                    delivered_bps: stream.delivered_bps,
                });
            }
            ActivityCandidate::Direct(play) => {
                deliveries.push(ActivityDelivery {
                    vod_observation: None,
                    method: "direct".to_owned(),
                    presentation: None,
                    user: bounded_text(play.user_name, MAX_USER_BYTES),
                    file_id: play.file_id,
                    item_id: play.item_id,
                    title: bounded_text(
                        titles.get(&play.item_id).cloned().unwrap_or_default(),
                        MAX_TITLE_BYTES,
                    ),
                    session_id: None,
                    started_unix: play.started_unix,
                    idle_seconds: play.idle_seconds,
                    delivered_bytes: None,
                    delivered_bps: None,
                });
            }
        }
    }
    deliveries.sort_by(|left, right| {
        right
            .started_unix
            .cmp(&left.started_unix)
            .then(left.method.cmp(&right.method))
            .then(left.file_id.cmp(&right.file_id))
            .then(left.user.cmp(&right.user))
    });
    let mut analysis = state.jobs.analysis_progress_snapshot();
    let labels = state
        .store
        .analysis_file_labels((MAX_ANALYSIS_PROGRESS * 2) as i64)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|label| (label.file_id, (label.item_id, label.title)))
        .collect::<HashMap<_, _>>();
    for progress in &mut analysis {
        if let Some((item_id, title)) = labels.get(&progress.file_id) {
            progress.item_id = *item_id;
            progress.title = bounded_text(title.clone(), MAX_TITLE_BYTES);
        }
    }
    // Charge the ancillary families before the two variable-length lists.
    // Their unused reservations are then available to real delivery rows.
    let mut snapshot = empty_snapshot(state.node_id.clone());
    attach_source_processed(&mut snapshot, local_source_processed(state).await);
    attach_receiver_processed(&mut snapshot, local_receiver_processed(state).await);
    attach_workers(&mut snapshot, local_workers(state).await);
    attach_live_tv(&mut snapshot, state.live_tv.activities());
    attach_dvr_snapshot(&mut snapshot, state.live_tv.capture_observation_snapshot());
    fill_snapshot(&mut snapshot, deliveries, analysis);
    snapshot
}

#[derive(Debug)]
enum ActivityCandidate {
    Transcode(crate::transcode::DeliveryCandidate),
    Vod(Box<crate::vodserve::VodDeliveryInfo>),
    Remux(crate::progressive::StreamListing),
    Direct(crate::delivery::Live),
}

impl ActivityCandidate {
    fn ordering_key(&self) -> (i64, &'static str, &str) {
        match self {
            Self::Transcode(candidate) => (candidate.started_unix, "hls", &candidate.id),
            Self::Vod(session) => (session.started_unix, "hls-copy", &session.id),
            Self::Remux(stream) => (stream.started_unix, "remux", &stream.id),
            Self::Direct(play) => (play.started_unix, "direct", &play.registry_id),
        }
    }
}

fn activity_candidates_bounded(
    transcodes: Vec<crate::transcode::DeliveryCandidate>,
    vod: Vec<crate::vodserve::VodDeliveryInfo>,
    remuxes: Vec<crate::progressive::StreamListing>,
    direct: Vec<crate::delivery::Live>,
    limit: usize,
) -> Vec<ActivityCandidate> {
    let mut candidates = transcodes
        .into_iter()
        .map(ActivityCandidate::Transcode)
        .chain(
            vod.into_iter()
                .map(|session| ActivityCandidate::Vod(Box::new(session))),
        )
        .chain(remuxes.into_iter().map(ActivityCandidate::Remux))
        .chain(direct.into_iter().map(ActivityCandidate::Direct))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left = left.ordering_key();
        let right = right.ordering_key();
        right
            .0
            .cmp(&left.0)
            .then(left.1.cmp(right.1))
            .then(left.2.cmp(right.2))
    });
    candidates.truncate(limit);
    candidates
}

async fn candidate_titles<F, Fut>(
    candidates: &[ActivityCandidate],
    fetch: F,
) -> Result<BTreeMap<i64, String>, plurx_core::error::StoreError>
where
    F: FnOnce(Vec<i64>) -> Fut,
    Fut: Future<Output = Result<BTreeMap<i64, String>, plurx_core::error::StoreError>>,
{
    let ids = candidates
        .iter()
        .filter_map(|candidate| match candidate {
            ActivityCandidate::Remux(stream) => Some(stream.item_id),
            ActivityCandidate::Direct(play) => Some(play.item_id),
            ActivityCandidate::Transcode(_) | ActivityCandidate::Vod(_) => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    fetch(ids).await
}

fn empty_snapshot(node_id: String) -> ActivitySnapshot {
    ActivitySnapshot {
        workers: None,
        node_id: bounded_text(node_id, MAX_NODE_ID_BYTES),
        deliveries: Vec::new(),
        source_processed: Vec::new(),
        receiver_processed: Vec::new(),
        analysis: Vec::new(),
        live_tv: Vec::new(),
        dvr: None,
    }
}

/// Keep the producer and consumer on the same exact wire budget. Ancillary
/// families have already spent only their actual serialized bytes. Reserve
/// the first delivery before filling analysis, then keep both lists in their
/// original order; JSON escaping is charged for every admitted row.
fn fill_snapshot(
    snapshot: &mut ActivitySnapshot,
    deliveries: Vec<ActivityDelivery>,
    analysis: Vec<crate::state::AnalysisProgress>,
) {
    let mut encoded_bytes = serde_json::to_vec(snapshot)
        .map(|encoded| encoded.len())
        .unwrap_or(MAX_RESPONSE_BYTES);
    let first_delivery_bytes = deliveries
        .iter()
        .take(MAX_DELIVERIES)
        .filter_map(|delivery| serde_json::to_vec(delivery).ok().map(|wire| wire.len()))
        .find(|bytes| encoded_bytes.saturating_add(*bytes) <= MAX_RESPONSE_BYTES)
        .unwrap_or(0);
    let mut analysis_bytes: usize = 0;
    for progress in analysis.into_iter().take(MAX_ANALYSIS_PROGRESS) {
        let Ok(encoded) = serde_json::to_vec(&progress) else {
            continue;
        };
        let separator = usize::from(!snapshot.analysis.is_empty());
        let added = encoded.len().saturating_add(separator);
        if analysis_bytes.saturating_add(added) > ANALYSIS_RESPONSE_RESERVE_BYTES
            || encoded_bytes.saturating_add(added)
                > MAX_RESPONSE_BYTES.saturating_sub(first_delivery_bytes)
        {
            continue;
        }
        encoded_bytes += added;
        analysis_bytes += added;
        snapshot.analysis.push(progress);
    }
    for delivery in deliveries.into_iter().take(MAX_DELIVERIES) {
        let Ok(encoded) = serde_json::to_vec(&delivery) else {
            continue;
        };
        let separator = usize::from(!snapshot.deliveries.is_empty());
        let added = encoded.len().saturating_add(separator);
        if encoded_bytes.saturating_add(added) > MAX_RESPONSE_BYTES {
            continue;
        }
        encoded_bytes += added;
        snapshot.deliveries.push(delivery);
    }
}

#[cfg(test)]
fn bounded_snapshot(
    node_id: String,
    deliveries: Vec<ActivityDelivery>,
    analysis: Vec<crate::state::AnalysisProgress>,
) -> ActivitySnapshot {
    let mut snapshot = empty_snapshot(node_id);
    fill_snapshot(&mut snapshot, deliveries, analysis);
    snapshot
}

fn attach_source_processed(snapshot: &mut ActivitySnapshot, rows: Vec<SourceProcessedRow>) {
    let mut bytes = serde_json::to_vec(snapshot).map_or(MAX_RESPONSE_BYTES, |wire| wire.len());
    let limit = bytes
        .saturating_add(SOURCE_RESPONSE_RESERVE_BYTES / 2)
        .min(MAX_RESPONSE_BYTES);
    for row in rows.into_iter().take(MAX_SOURCE_PROCESSED) {
        let Ok(wire) = serde_json::to_vec(&row) else {
            continue;
        };
        let added = wire.len() + usize::from(!snapshot.source_processed.is_empty());
        if bytes.saturating_add(added) > limit {
            break;
        }
        bytes += added;
        snapshot.source_processed.push(row);
    }
}

fn attach_receiver_processed(snapshot: &mut ActivitySnapshot, rows: Vec<ReceiverProcessedRow>) {
    let mut bytes = serde_json::to_vec(snapshot).map_or(MAX_RESPONSE_BYTES, |wire| wire.len());
    let limit = bytes
        .saturating_add(SOURCE_RESPONSE_RESERVE_BYTES / 2)
        .min(MAX_RESPONSE_BYTES);
    for row in rows.into_iter().take(MAX_SOURCE_PROCESSED) {
        let Ok(wire) = serde_json::to_vec(&row) else {
            continue;
        };
        let added = wire.len() + usize::from(!snapshot.receiver_processed.is_empty());
        if bytes.saturating_add(added) > limit {
            break;
        }
        bytes += added;
        snapshot.receiver_processed.push(row);
    }
}

fn attach_workers(snapshot: &mut ActivitySnapshot, workers: ActivityWorkers) {
    let before = serde_json::to_vec(&*snapshot)
        .map(|wire| wire.len())
        .unwrap_or(MAX_RESPONSE_BYTES);
    snapshot.workers = Some(Box::new(workers));
    let added = serde_json::to_vec(&*snapshot)
        .map(|wire| wire.len().saturating_sub(before))
        .unwrap_or(MAX_RESPONSE_BYTES);
    if added > WORKER_RESPONSE_RESERVE_BYTES {
        let workers = snapshot.workers.take().expect("worker snapshot");
        snapshot.workers = Some(Box::new((*workers).capacity_only()));
    }
}

fn attach_live_tv(
    snapshot: &mut ActivitySnapshot,
    rows: impl IntoIterator<Item = crate::live_tv::LiveTvActivity>,
) {
    let mut bytes = serde_json::to_vec(&*snapshot)
        .map(|wire| wire.len())
        .unwrap_or(MAX_RESPONSE_BYTES);
    let limit = bytes
        .saturating_add(LIVE_TV_RESPONSE_RESERVE_BYTES)
        .min(MAX_RESPONSE_BYTES);
    for row in rows.into_iter().take(4) {
        let Ok(wire) = serde_json::to_vec(&row) else {
            continue;
        };
        let added = wire.len() + usize::from(!snapshot.live_tv.is_empty());
        if bytes.saturating_add(added) > limit {
            continue;
        }
        bytes += added;
        snapshot.live_tv.push(row);
    }
}

fn attach_dvr_snapshot(
    snapshot: &mut ActivitySnapshot,
    source: crate::live_tv::dvr::DvrObservationSnapshot,
) {
    let mut bounded = crate::live_tv::dvr::DvrObservationSnapshot {
        observed_sink_count: source.observed_sink_count,
        truncated: source.truncated,
        observations: Vec::new(),
        recording_transports: source.recording_transports,
        storage_free_bytes: source.storage_free_bytes,
    };
    snapshot.dvr = Some(bounded.clone());
    let base_bytes = serde_json::to_vec(snapshot)
        .map(|encoded| encoded.len())
        .unwrap_or(MAX_RESPONSE_BYTES);
    let limit = base_bytes
        .saturating_add(DVR_RESPONSE_RESERVE_BYTES)
        .min(MAX_RESPONSE_BYTES);
    let mut encoded_bytes = base_bytes;
    for observation in source
        .observations
        .into_iter()
        .take(crate::live_tv::dvr::DVR_OBSERVATIONS_MAX)
    {
        let Ok(encoded) = serde_json::to_vec(&observation) else {
            bounded.truncated = true;
            continue;
        };
        let added = encoded
            .len()
            .saturating_add(usize::from(!bounded.observations.is_empty()));
        if encoded_bytes.saturating_add(added) > limit {
            bounded.truncated = true;
            break;
        }
        encoded_bytes += added;
        bounded.observations.push(observation);
    }
    if bounded.observations.len() < bounded.observed_sink_count {
        bounded.truncated = true;
    }
    snapshot.dvr = Some(bounded);
}

#[allow(dead_code)]
fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn worker_observations_are_bounded_and_old_peers_remain_unknown() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let mut workers = local_workers(&state).await;
        assert_eq!(
            workers.heavy_limit,
            crate::transcode::BACKGROUND_HEAVY_LIMIT
        );
        assert_eq!(workers.heavy_in_use, 0);
        workers.children = vec!["\u{0000}".repeat(256); 32];
        workers.child_count = 32;
        assert!(
            serde_json::to_vec(&workers).expect("worker wire").len()
                < WORKER_RESPONSE_RESERVE_BYTES
        );
        let mut snapshot = bounded_snapshot("node-a".into(), Vec::new(), Vec::new());
        snapshot.workers = Some(Box::new(workers.clone()));
        assert!(snapshot_is_bounded(&snapshot, "node-a"));
        workers.children.push("overflow".into());
        snapshot.workers = Some(Box::new(workers));
        assert!(!snapshot_is_bounded(&snapshot, "node-a"));
        let old: ActivitySnapshot = serde_json::from_value(serde_json::json!({
            "node_id":"old-node", "deliveries":[]
        }))
        .expect("old peer wire remains supported");
        assert!(old.workers.is_none());
    }

    #[test]
    fn authenticated_snapshot_is_never_share_cacheable() {
        let headers = snapshot_response_headers();
        assert_eq!(
            headers
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, no-store")
        );
    }

    #[tokio::test]
    async fn ordinary_bearer_and_missing_cluster_authority_are_rejected() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let mut household = HeaderMap::new();
        household.insert(
            "authorization",
            "Bearer household-token"
                .parse()
                .expect("static household authorization is a valid header value"),
        );
        assert_eq!(
            snapshot(State(state.clone()), household)
                .await
                .expect_err("a household bearer must not authorize a cluster snapshot"),
            StatusCode::UNAUTHORIZED
        );

        let mut forged = HeaderMap::new();
        forged.insert(
            NODE_HEADER,
            "removed-node".parse().expect("static node header"),
        );
        forged.insert(
            TARGET_HEADER,
            state.node_id.parse().expect("state node header"),
        );
        forged.insert(
            TIMESTAMP_HEADER,
            unix_ms()
                .to_string()
                .parse()
                .expect("a decimal timestamp is a valid header value"),
        );
        forged.insert(
            SIGNATURE_HEADER,
            "00".repeat(32)
                .parse()
                .expect("a hexadecimal signature is a valid header value"),
        );
        assert_eq!(
            snapshot(State(state), forged)
                .await
                .expect_err("a forged signature must not authorize a cluster snapshot"),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn source_processed_snapshot_preserves_grant_and_rejects_wrong_node_or_id() {
        let row = SourceProcessedRow {
            stop_id: format!("source-s-{}", "a".repeat(64)),
            session_id: Some(format!("s-{}", "b".repeat(64))),
            node_id: "source-node".into(),
            title: "The film".into(),
            viewer: "share:grant-a:viewer-a".into(),
            grant: "grant-a".into(),
            file_id: 7,
            item_id: 9,
            started_unix: 100,
            reason: "processed Dolby Vision upgrade: Profile81; Source DV_FEL_REENCODE".into(),
        };
        let mut snapshot = bounded_snapshot("source-node".into(), Vec::new(), Vec::new());
        attach_source_processed(&mut snapshot, vec![row.clone()]);
        let wire = serde_json::to_vec(&snapshot).expect("bounded Source snapshot");
        assert!(wire.len() <= MAX_RESPONSE_BYTES);
        let PeerActivityOutcome::Answered(decoded) = decode_snapshot(&wire, "source-node") else {
            panic!("same-node Source activity must decode")
        };
        assert_eq!(decoded.source_processed[0].grant, "grant-a");
        assert_eq!(decoded.source_processed[0].viewer, "share:grant-a:viewer-a");
        snapshot.source_processed[0].node_id = "another-node".into();
        assert!(!snapshot_is_bounded(&snapshot, "source-node"));
        snapshot.source_processed[0] = row;
        snapshot.source_processed[0].stop_id = "source-other".into();
        assert!(!snapshot_is_bounded(&snapshot, "source-node"));
    }

    #[test]
    fn receiver_processed_snapshot_binds_retirement_owner_and_opaque_id() {
        let row = ReceiverProcessedRow {
            stop_id: format!("receiver-s-{}", "a".repeat(64)),
            receiver_node_id: "receiver-node".into(),
            source_server_id: uuid::Uuid::new_v4().to_string(),
            source_name: "Source".into(),
            title: "Shared item 100".into(),
            viewer: "User #7".into(),
            reason: "Processed Dolby Vision upgrade runs on Source".into(),
        };
        let mut snapshot = bounded_snapshot("receiver-node".into(), Vec::new(), Vec::new());
        attach_receiver_processed(&mut snapshot, vec![row.clone()]);
        let wire = serde_json::to_vec(&snapshot).expect("bounded receiver snapshot");
        assert!(matches!(
            decode_snapshot(&wire, "receiver-node"),
            PeerActivityOutcome::Answered(_)
        ));
        snapshot.receiver_processed[0].receiver_node_id = "other-node".into();
        assert!(!snapshot_is_bounded(&snapshot, "receiver-node"));
        snapshot.receiver_processed[0] = row;
        snapshot.receiver_processed[0].stop_id = "receiver-secret".into();
        assert!(!snapshot_is_bounded(&snapshot, "receiver-node"));
    }

    #[tokio::test]
    async fn source_processed_stop_rejects_household_bearer() {
        let (_, state) = crate::http::tests::test_app_with_state();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            "Bearer household-token".parse().expect("header"),
        );
        let body = Bytes::from_static(b"{\"node_id\":\"source-node\",\"stop_id\":\"source-s-0\"}");
        assert_eq!(
            stop_source_processed(State(state), headers, body)
                .await
                .expect_err("household bearer cannot stop Source work"),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn sqlite_peer_directory_is_local_only() {
        let (_, state) = crate::http::tests::test_app_with_state();
        assert!(state
            .peer_activity
            .snapshots()
            .await
            .expect("SQLite has an empty peer directory")
            .is_empty());
    }

    #[tokio::test]
    async fn reachable_peer_without_activity_origin_is_unsupported_not_unreachable() {
        let membership = plurx_core::cluster::membership::MembershipManager::unavailable();
        let client = PeerActivityClient::new(
            crate::cluster::test_calls(membership.clone()),
            Directory::new(membership),
        );
        let outcome = fetch_peer_activity(
            client,
            ActivityPeer {
                node_id: "legacy-node".to_owned(),
                raft_id: 2,
                http_base: None,
                reachable: true,
            },
            deadline_after(Duration::from_secs(1)),
        )
        .await;
        assert_eq!(outcome, PeerActivityOutcome::Unsupported);
    }

    #[test]
    fn internal_origins_and_snapshot_identity_are_bounded() {
        assert_eq!(
            activity_url("http://node-b:32400")
                .expect("plain internal origin")
                .as_str(),
            "http://node-b:32400/_internal/v1/activity-snapshot"
        );
        for invalid in [
            "file:///tmp/socket",
            "http://user:secret@node-b:32400",
            "http://node-b:32400/prefix",
            "http://node-b:32400?next=http://metadata",
            "http://node-b:32400#fragment",
        ] {
            assert!(activity_url(invalid).is_none(), "accepted {invalid}");
        }

        let snapshot = ActivitySnapshot {
            workers: None,
            node_id: "node-b".to_owned(),
            deliveries: Vec::new(),
            source_processed: Vec::new(),
            receiver_processed: Vec::new(),
            analysis: Vec::new(),
            live_tv: Vec::new(),
            dvr: None,
        };
        assert!(snapshot_is_bounded(&snapshot, "node-b"));
        assert!(!snapshot_is_bounded(&snapshot, "node-c"));
    }

    #[test]
    fn durable_progress_wire_identity_is_paired_and_bounded() {
        let mut row = crate::state::AnalysisProgress::test_row("artifact-key", "Title");
        let mut snapshot = ActivitySnapshot {
            workers: None,
            node_id: "node".into(),
            deliveries: vec![],
            source_processed: vec![],
            receiver_processed: vec![],
            analysis: vec![row.clone()],
            live_tv: vec![],
            dvr: None,
        };
        assert!(
            snapshot_is_bounded(&snapshot, "node"),
            "old peers remain readable"
        );
        row.durable_job_id = Some(uuid::Uuid::new_v4().to_string());
        snapshot.analysis[0] = row.clone();
        assert!(!snapshot_is_bounded(&snapshot, "node"), "unpaired identity");
        row.durable_fence = Some(1);
        snapshot.analysis[0] = row.clone();
        assert!(snapshot_is_bounded(&snapshot, "node"));
        row.durable_fence = Some(0);
        snapshot.analysis[0] = row.clone();
        assert!(!snapshot_is_bounded(&snapshot, "node"));
        row.durable_fence = Some(1);
        row.durable_job_id = Some("x".repeat(1024));
        snapshot.analysis[0] = row;
        assert!(!snapshot_is_bounded(&snapshot, "node"));
    }

    #[test]
    fn initial_dvr_serving_generation_is_a_valid_peer_snapshot() {
        let snapshot = ActivitySnapshot {
            workers: None,
            node_id: "node-b".to_owned(),
            deliveries: Vec::new(),
            source_processed: Vec::new(),
            receiver_processed: Vec::new(),
            analysis: Vec::new(),
            live_tv: Vec::new(),
            dvr: Some(crate::live_tv::dvr::DvrObservationSnapshot {
                observed_sink_count: 1,
                truncated: false,
                observations: vec![crate::live_tv::dvr::DvrCaptureObservation {
                    recording_id: "recording-a".to_owned(),
                    channel_id: "channel-a".to_owned(),
                    airing_start: 1_789_000_800,
                    owner_node_id: "node-b".to_owned(),
                    config_generation: 0,
                    serving_generation: 0,
                    attempt: 1,
                    phase: "writing".to_owned(),
                    observation_age_ms: 0,
                    last_write_age_ms: Some(250),
                    first_write_at_ms: Some(1_789_000_000_000),
                    attempt_bytes_written: 4_096,
                    prior_attempt_bytes: Some(0),
                    write_bps: Some(2_048),
                    reason_code: None,
                }],
                recording_transports: 1,
                storage_free_bytes: Some(1_000_000_000),
            }),
        };
        let body = serde_json::to_vec(&snapshot).expect("snapshot JSON");

        assert!(matches!(
            decode_snapshot(&body, "node-b"),
            PeerActivityOutcome::Answered(_)
        ));
    }

    #[test]
    fn vod_activity_observations_are_additive_bounded_and_age_in_cache() {
        let old = serde_json::json!({"method":"hls-copy","presentation":"vod","user":"viewer",
            "file_id":1,"item_id":2,"title":"fixture","started_unix":1,"idle_seconds":0,
            "delivered_bytes":0,"delivered_bps":null});
        let delivery: ActivityDelivery = serde_json::from_value(old.clone()).expect("old peer");
        assert!(delivery.vod_observation.is_none());
        let mut new = old;
        new["vod_observation"] = serde_json::json!({"control_demand":"hold","render_state":"rendering",
            "position_ms":12000,"client_runway_ms":3000,"control_age_ms":5,"producer_state":"held","producer_hold":"ahead"});
        new["future_optional_field"] = serde_json::Value::Bool(true);
        let delivery: ActivityDelivery =
            serde_json::from_value(new.clone()).expect("additive fields");
        #[derive(serde::Deserialize)]
        struct OldConsumer {
            method: String,
        }
        let old_consumer: OldConsumer =
            serde_json::from_value(new).expect("old ignores unknown fields");
        assert_eq!(old_consumer.method, "hls-copy");
        let mut snapshot = bounded_snapshot("node-b".into(), vec![delivery], Vec::new());
        assert!(snapshot_is_bounded(&snapshot, "node-b"));
        snapshot.deliveries[0]
            .vod_observation
            .as_mut()
            .expect("observations")
            .display_target = Some(crate::vodserve::DisplayTargetActivity {
            reported_peak_nits: None,
            peak_source: None,
            bucket_nits: Some(100),
            live_rendering: true,
            note: "explicit SDR display".into(),
        });
        let bytes = serde_json::to_vec(&snapshot).expect("SDR peer snapshot");
        assert!(matches!(
            decode_snapshot(&bytes, "node-b"),
            PeerActivityOutcome::Answered(_)
        ));
        for bucket in [0, 99, 101, 399, 5000] {
            let mut invalid = snapshot.clone();
            invalid.deliveries[0]
                .vod_observation
                .as_mut()
                .expect("observations")
                .display_target
                .as_mut()
                .expect("target")
                .bucket_nits = Some(bucket);
            assert!(matches!(
                decode_snapshot(&serde_json::to_vec(&invalid).expect("snapshot"), "node-b"),
                PeerActivityOutcome::InvalidResponse
            ));
        }
        let aged = age_dvr_observations(
            PeerActivityOutcome::Answered(Box::new(snapshot)),
            Duration::from_secs(1),
        );
        let PeerActivityOutcome::Answered(aged) = aged else {
            panic!("snapshot")
        };
        assert_eq!(
            aged.deliveries[0]
                .vod_observation
                .as_ref()
                .expect("observations")
                .control_age_ms,
            Some(1005)
        );
    }

    #[test]
    fn producer_obeys_the_exact_json_byte_budget_after_escaping() {
        let delivery = ActivityDelivery {
            vod_observation: None,
            method: "x".repeat(32),
            presentation: Some("live-recovery".to_owned()),
            user: "\0".repeat(MAX_USER_BYTES),
            file_id: i64::MAX,
            item_id: i64::MAX,
            title: "\0".repeat(MAX_TITLE_BYTES),
            session_id: None,
            started_unix: i64::MAX,
            idle_seconds: u64::MAX,
            delivered_bytes: Some(i64::MAX),
            delivered_bps: Some(i64::MAX),
        };
        let snapshot = bounded_snapshot(
            "node-b".to_owned(),
            vec![delivery; MAX_DELIVERIES],
            Vec::new(),
        );
        let encoded = serde_json::to_vec(&snapshot).expect("bounded snapshot serializes");
        assert!(encoded.len() <= MAX_RESPONSE_BYTES);
        assert!(snapshot.deliveries.len() < MAX_DELIVERIES);
        assert!(snapshot_is_bounded(&snapshot, "node-b"));
    }

    #[test]
    fn live_tv_activity_reservation_handles_escaped_rows_and_checks_owner_identity() {
        let node_id = "\0".repeat(MAX_NODE_ID_BYTES);
        let row = crate::live_tv::LiveTvActivity {
            channel_number: "\0".repeat(32),
            channel_name: "\0".repeat(256),
            user: "\0".repeat(MAX_USER_BYTES),
            owner_node_id: node_id.clone(),
            encoder: "\0".repeat(32),
            age_seconds: u64::MAX,
            output_height: 1080,
            state: "\0".repeat(32),
            delivery: None,
            programme_title: Some("\0".repeat(256)),
        };
        let delivery = ActivityDelivery {
            vod_observation: None,
            method: "direct".into(),
            presentation: None,
            user: "\0".repeat(MAX_USER_BYTES),
            file_id: 1,
            item_id: 1,
            title: "\0".repeat(MAX_TITLE_BYTES),
            session_id: None,
            started_unix: 0,
            idle_seconds: 0,
            delivered_bytes: None,
            delivered_bps: None,
        };
        let mut snapshot = empty_snapshot(node_id.clone());
        attach_live_tv(&mut snapshot, vec![row; 4]);
        fill_snapshot(&mut snapshot, vec![delivery; MAX_DELIVERIES], Vec::new());
        let encoded = serde_json::to_vec(&snapshot).expect("snapshot JSON");
        assert!(encoded.len() <= MAX_RESPONSE_BYTES);
        assert!(snapshot_is_bounded(&snapshot, &node_id));
        let value = serde_json::to_value(&snapshot).expect("public activity fields");
        assert!(value["live_tv"][0].get("capability").is_none());
        assert!(value["live_tv"][0].get("activation_token").is_none());
        snapshot.live_tv[0].owner_node_id = "forged-owner".into();
        assert!(!snapshot_is_bounded(&snapshot, &node_id));
    }

    #[test]
    fn saturated_deliveries_preserve_analysis_progress_within_the_wire_budget() {
        let delivery = ActivityDelivery {
            vod_observation: None,
            method: "x".repeat(32),
            presentation: Some("live-recovery".to_owned()),
            user: "\0".repeat(MAX_USER_BYTES),
            file_id: i64::MAX,
            item_id: i64::MAX,
            title: "\0".repeat(MAX_TITLE_BYTES),
            session_id: None,
            started_unix: i64::MAX,
            idle_seconds: u64::MAX,
            delivered_bytes: Some(i64::MAX),
            delivered_bps: Some(i64::MAX),
        };
        let progress =
            crate::state::AnalysisProgress::test_row("shared-cache", &"\0".repeat(MAX_TITLE_BYTES));
        let snapshot = bounded_snapshot(
            "node-b".to_owned(),
            vec![delivery; MAX_DELIVERIES],
            vec![progress],
        );
        let encoded = serde_json::to_vec(&snapshot).expect("bounded snapshot serializes");
        assert!(encoded.len() <= MAX_RESPONSE_BYTES);
        assert!(!snapshot.deliveries.is_empty());
        assert!(snapshot.deliveries.len() < MAX_DELIVERIES);
        assert_eq!(snapshot.analysis.len(), 1);
        assert!(snapshot_is_bounded(&snapshot, "node-b"));
    }

    #[test]
    fn saturated_ancillary_activity_keeps_stop_rows_analysis_and_a_delivery() {
        let mut snapshot = empty_snapshot("node-b".to_owned());
        let source_stop = format!("source-s-{}", "a".repeat(64));
        let receiver_stop = format!("receiver-s-{}", "b".repeat(64));
        attach_source_processed(
            &mut snapshot,
            vec![SourceProcessedRow {
                stop_id: source_stop.clone(),
                session_id: None,
                node_id: "node-b".into(),
                title: "\0".repeat(MAX_TITLE_BYTES),
                viewer: "\0".repeat(MAX_USER_BYTES),
                grant: "grant-a".into(),
                file_id: 7,
                item_id: 9,
                started_unix: 1,
                reason: "processed upgrade".into(),
            }],
        );
        attach_receiver_processed(
            &mut snapshot,
            vec![ReceiverProcessedRow {
                stop_id: receiver_stop.clone(),
                receiver_node_id: "node-b".into(),
                source_server_id: uuid::Uuid::new_v4().to_string(),
                source_name: "\0".repeat(MAX_USER_BYTES),
                title: "\0".repeat(MAX_TITLE_BYTES),
                viewer: "viewer".into(),
                reason: "processed upgrade".into(),
            }],
        );
        attach_workers(
            &mut snapshot,
            ActivityWorkers {
                subtitles: None,
                analysis_engine: None,
                observed_at_ms: 1,
                heavy_limit: 1,
                probe_batch_limit: Some(1),
                heavy_in_use: 0,
                heavy_available: 1,
                accepting_work: true,
                hardware_used: 0,
                hardware_limit: 1,
                software_used: 0,
                software_limit: 1,
                children: vec!["\0".repeat(256); 32],
                child_count: 32,
            },
        );
        attach_live_tv(
            &mut snapshot,
            vec![
                crate::live_tv::LiveTvActivity {
                    channel_number: "\0".repeat(32),
                    channel_name: "\0".repeat(256),
                    user: "\0".repeat(MAX_USER_BYTES),
                    owner_node_id: "node-b".into(),
                    encoder: "\0".repeat(32),
                    age_seconds: u64::MAX,
                    output_height: 1080,
                    state: "\0".repeat(32),
                    delivery: None,
                    programme_title: Some("\0".repeat(256)),
                };
                4
            ],
        );
        attach_dvr_snapshot(
            &mut snapshot,
            crate::live_tv::dvr::DvrObservationSnapshot {
                observed_sink_count: 64,
                truncated: false,
                observations: vec![
                    crate::live_tv::dvr::DvrCaptureObservation {
                        recording_id: "\0".repeat(128),
                        channel_id: "\0".repeat(256),
                        airing_start: 1,
                        owner_node_id: "node-b".into(),
                        config_generation: 0,
                        serving_generation: 0,
                        attempt: 1,
                        phase: "writing".into(),
                        observation_age_ms: 0,
                        last_write_age_ms: None,
                        first_write_at_ms: None,
                        attempt_bytes_written: 0,
                        prior_attempt_bytes: None,
                        write_bps: None,
                        reason_code: None,
                    };
                    64
                ],
                recording_transports: 1,
                storage_free_bytes: None,
            },
        );
        fill_snapshot(
            &mut snapshot,
            vec![
                ActivityDelivery {
                    vod_observation: None,
                    method: "x".repeat(32),
                    presentation: Some("live-recovery".into()),
                    user: "\0".repeat(MAX_USER_BYTES),
                    file_id: i64::MAX,
                    item_id: i64::MAX,
                    title: "\0".repeat(MAX_TITLE_BYTES),
                    session_id: None,
                    started_unix: i64::MAX,
                    idle_seconds: u64::MAX,
                    delivered_bytes: Some(i64::MAX),
                    delivered_bps: Some(i64::MAX),
                };
                MAX_DELIVERIES
            ],
            vec![crate::state::AnalysisProgress::test_row(
                "shared-cache",
                &"\0".repeat(MAX_TITLE_BYTES),
            )],
        );
        assert_eq!(snapshot.source_processed[0].stop_id, source_stop);
        assert_eq!(snapshot.receiver_processed[0].stop_id, receiver_stop);
        assert!(snapshot
            .workers
            .as_ref()
            .is_some_and(|workers| !workers.children.is_empty()));
        assert!(!snapshot.live_tv.is_empty());
        assert!(snapshot
            .dvr
            .as_ref()
            .is_some_and(|dvr| !dvr.observations.is_empty()));
        assert_eq!(snapshot.analysis.len(), 1);
        assert!(!snapshot.deliveries.is_empty());
        assert!(snapshot.deliveries.len() < MAX_DELIVERIES);
        assert!(serde_json::to_vec(&snapshot).expect("activity wire").len() <= MAX_RESPONSE_BYTES);
        assert!(snapshot_is_bounded(&snapshot, "node-b"));
    }

    #[test]
    fn global_candidate_cap_precedes_all_expensive_enrichment() {
        let transcodes = (0..MAX_DELIVERIES)
            .map(|index| crate::transcode::DeliveryCandidate {
                id: format!("transcode-{index:04}"),
                started_unix: index as i64,
            })
            .collect();
        let remuxes = (0..MAX_DELIVERIES)
            .map(|index| crate::progressive::StreamListing {
                id: format!("remux-{index:04}"),
                user_name: "viewer".to_owned(),
                file_id: index as i64,
                item_id: index as i64,
                started_unix: MAX_DELIVERIES as i64 + index as i64,
                delivered_bytes: 0,
                delivered_bps: None,
                delivered_idle_ms: 0,
            })
            .collect();
        let direct = (0..MAX_DELIVERIES)
            .map(|index| crate::delivery::Live {
                registry_id: format!("direct-{index:04}"),
                user_name: "viewer".to_owned(),
                file_id: index as i64,
                item_id: index as i64,
                started_unix: (MAX_DELIVERIES * 2) as i64 + index as i64,
                idle_seconds: 0,
            })
            .collect();

        let selected =
            activity_candidates_bounded(transcodes, Vec::new(), remuxes, direct, MAX_DELIVERIES);
        assert_eq!(selected.len(), MAX_DELIVERIES);
        assert!(selected
            .iter()
            .all(|candidate| matches!(candidate, ActivityCandidate::Direct(_))));
    }

    #[test]
    fn vod_session_participates_in_the_bounded_peer_inventory() {
        let session = crate::vodserve::VodDeliveryInfo {
            dolby_vision_refit: None,
            dv_refit_digest: None,
            dv_refit_identity_reason: None,
            observation: None,
            // Non-zero on purpose: the peer snapshot hard-coded zero here, so
            // a fixture that also says zero cannot see the difference.
            delivered_bytes: 4_096,
            delivered_bps: Some(12_000_000),
            delivered_idle_ms: 250,
            id: "vod-session".to_owned(),
            method: crate::delivery::Method::Transcode,
            file_id: 7,
            item_id: 9,
            item_title: "VOD title".to_owned(),
            user_name: "viewer".to_owned(),
            target_height: 1080,
            started_unix: 20,
            idle_seconds: 3,
        };
        let selected = activity_candidates_bounded(
            vec![crate::transcode::DeliveryCandidate {
                id: "historical-live".to_owned(),
                started_unix: 10,
            }],
            vec![session],
            Vec::new(),
            Vec::new(),
            1,
        );

        assert!(matches!(
            selected.as_slice(),
            [ActivityCandidate::Vod(info)] if info.method == crate::delivery::Method::Transcode
        ));
    }

    #[tokio::test]
    async fn selected_titles_cost_one_bounded_store_call() {
        let candidates = (0..MAX_DELIVERIES)
            .map(|index| {
                ActivityCandidate::Direct(crate::delivery::Live {
                    registry_id: format!("direct-{index:04}"),
                    user_name: "viewer".to_owned(),
                    file_id: index as i64,
                    item_id: index as i64,
                    started_unix: index as i64,
                    idle_seconds: 0,
                })
            })
            .collect::<Vec<_>>();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let titles = candidate_titles(&candidates, move |ids| async move {
            observed.fetch_add(1, Ordering::SeqCst);
            assert_eq!(ids.len(), MAX_DELIVERIES);
            Ok(ids
                .into_iter()
                .map(|id| (id, format!("title-{id}")))
                .collect())
        })
        .await
        .expect("bounded title batch");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(titles.len(), MAX_DELIVERIES);
    }

    #[test]
    fn http_failures_are_not_reported_as_transport_failures() {
        let body = serde_json::to_vec(&ActivitySnapshot {
            workers: None,
            node_id: "node-b".to_owned(),
            deliveries: Vec::new(),
            source_processed: Vec::new(),
            receiver_processed: Vec::new(),
            analysis: Vec::new(),
            live_tv: Vec::new(),
            dvr: None,
        })
        .expect("snapshot JSON");
        assert_eq!(
            classify_snapshot_response(StatusCode::UNAUTHORIZED, &body, "node-b"),
            PeerActivityOutcome::Refused
        );
        assert_eq!(
            classify_snapshot_response(StatusCode::FORBIDDEN, &body, "node-b"),
            PeerActivityOutcome::Refused
        );
        assert_eq!(
            classify_snapshot_response(StatusCode::SERVICE_UNAVAILABLE, &body, "node-b"),
            PeerActivityOutcome::HttpError
        );
        assert_eq!(
            classify_snapshot_response(StatusCode::OK, &body, "node-c"),
            PeerActivityOutcome::InvalidResponse
        );
        assert_eq!(
            classify_snapshot_response(StatusCode::OK, b"not JSON", "node-b"),
            PeerActivityOutcome::InvalidResponse
        );
    }

    #[test]
    fn unsigned_error_status_reports_the_answer_not_an_invalid_response() {
        // A response-bound Activity read whose 503 came from a layer in front
        // of the signing handler is a busy peer, not a protocol violation.
        assert_eq!(
            unauthenticated_status_outcome(503),
            PeerActivityOutcome::HttpError
        );
        assert_eq!(
            unauthenticated_status_outcome(403),
            PeerActivityOutcome::HttpError
        );
        // An unsigned success never stands for a snapshot.
        assert_eq!(
            unauthenticated_status_outcome(200),
            PeerActivityOutcome::InvalidResponse
        );
        assert_eq!(
            unauthenticated_status_outcome(1_000),
            PeerActivityOutcome::InvalidResponse
        );
    }

    #[tokio::test]
    async fn error_status_is_classified_without_buffering_its_body() {
        use std::convert::Infallible;

        use axum::body::Body;
        use axum::response::IntoResponse;
        use axum::routing::get;
        use axum::Router;
        use bytes::Bytes;

        let app = Router::new()
            .route(
                "/refused-large",
                get(|| async { (StatusCode::UNAUTHORIZED, vec![b'x'; MAX_RESPONSE_BYTES + 1]) }),
            )
            .route(
                "/error-stalled",
                get(|| async {
                    let chunks = stream::once(async {
                        Ok::<Bytes, Infallible>(Bytes::from_static(b"error"))
                    })
                    .chain(stream::pending::<Result<Bytes, Infallible>>());
                    (StatusCode::SERVICE_UNAVAILABLE, Body::from_stream(chunks)).into_response()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Activity HTTP fixture");
        let address = listener.local_addr().expect("Activity fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve Activity HTTP fixture");
        });
        let client = reqwest::Client::new();

        let refused = client
            .get(format!("http://{address}/refused-large"))
            .send()
            .await
            .expect("large refusal response headers");
        assert_eq!(
            read_snapshot_response(refused, deadline_after(Duration::from_millis(50)), "node-b",)
                .await,
            PeerActivityOutcome::Refused
        );

        let failed = client
            .get(format!("http://{address}/error-stalled"))
            .send()
            .await
            .expect("stalled error response headers");
        assert_eq!(
            read_snapshot_response(failed, deadline_after(Duration::from_millis(50)), "node-b",)
                .await,
            PeerActivityOutcome::HttpError
        );
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn one_in_flight_peer_wave_is_shared_then_refreshed() {
        let gate = Arc::new(tokio::sync::Mutex::new(PeerActivityReadGate::default()));
        let physical = Arc::new(AtomicUsize::new(0));
        let fetch_started = Arc::new(tokio::sync::Notify::new());
        let release_fetch = Arc::new(tokio::sync::Notify::new());
        let leader_gate = Arc::clone(&gate);
        let leader_physical = Arc::clone(&physical);
        let leader_started = Arc::clone(&fetch_started);
        let leader_release = Arc::clone(&release_fetch);
        let leader = tokio::spawn(async move {
            shared_peer_activity(
                &leader_gate,
                || async { Ok(()) },
                |()| async move {
                    leader_physical.fetch_add(1, Ordering::SeqCst);
                    leader_started.notify_one();
                    leader_release.notified().await;
                    vec![("node-b".to_owned(), PeerActivityOutcome::Unhealthy)]
                },
            )
            .await
            .expect("leading Activity read")
        });
        fetch_started.notified().await;

        let followers_ready = Arc::new(tokio::sync::Barrier::new(8));
        let followers_entered = Arc::new(AtomicUsize::new(0));
        let mut callers = Vec::new();
        for _ in 0..7 {
            let gate = Arc::clone(&gate);
            let physical = Arc::clone(&physical);
            let ready = Arc::clone(&followers_ready);
            let entered = Arc::clone(&followers_entered);
            callers.push(tokio::spawn(async move {
                ready.wait().await;
                entered.fetch_add(1, Ordering::SeqCst);
                shared_peer_activity(
                    &gate,
                    || async { Ok(()) },
                    |()| async move {
                        physical.fetch_add(1, Ordering::SeqCst);
                        vec![("node-b".to_owned(), PeerActivityOutcome::Unhealthy)]
                    },
                )
                .await
                .expect("shared Activity read")
            }));
        }
        followers_ready.wait().await;
        while followers_entered.load(Ordering::SeqCst) != 7 {
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;
        // No assertion belongs here: the gate guard is held across the leading
        // fetch, so `physical` cannot exceed one at this point whatever the
        // coalescing does, and an assertion no mutation can fail is noise. The
        // post-join count and the shared-Arc identity below are the oracle.
        release_fetch.notify_one();

        let mut snapshots = vec![leader.await.expect("leader task")];
        for caller in callers {
            snapshots.push(caller.await.expect("caller task"));
        }
        assert_eq!(physical.load(Ordering::SeqCst), 1);
        assert!(snapshots
            .windows(2)
            .all(|pair| Arc::ptr_eq(&pair[0], &pair[1])));

        tokio::time::advance(ACTIVITY_SNAPSHOT_REUSE - Duration::from_millis(1)).await;
        let before_expiry = shared_peer_activity(
            &gate,
            || async { Ok(()) },
            |()| async {
                physical.fetch_add(1, Ordering::SeqCst);
                vec![("node-b".to_owned(), PeerActivityOutcome::Unhealthy)]
            },
        )
        .await
        .expect("cached Activity read");
        assert!(Arc::ptr_eq(&snapshots[0], &before_expiry));
        assert_eq!(physical.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        let after_expiry = shared_peer_activity(
            &gate,
            || async { Ok(()) },
            |()| async {
                physical.fetch_add(1, Ordering::SeqCst);
                vec![("node-b".to_owned(), PeerActivityOutcome::Unhealthy)]
            },
        )
        .await
        .expect("fresh Activity read");
        assert!(!Arc::ptr_eq(&snapshots[0], &after_expiry));
        assert_eq!(physical.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_directory_then_cancelled_owner_keeps_the_physical_start_floor() {
        let gate = Arc::new(tokio::sync::Mutex::new(PeerActivityReadGate::default()));
        let wire_started = Arc::new(tokio::sync::Notify::new());
        let leader_gate = Arc::clone(&gate);
        let leader_started = Arc::clone(&wire_started);
        let leader = tokio::spawn(async move {
            let _ = shared_peer_activity(
                &leader_gate,
                || async {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok(())
                },
                |()| async move {
                    leader_started.notify_one();
                    std::future::pending::<Vec<(String, PeerActivityOutcome)>>().await
                },
            )
            .await;
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        wire_started.notified().await;
        leader.abort();
        let _ = leader.await;

        let physical = Arc::new(AtomicUsize::new(0));
        let follower_gate = Arc::clone(&gate);
        let follower_physical = Arc::clone(&physical);
        let follower = tokio::spawn(async move {
            shared_peer_activity(
                &follower_gate,
                || async { Ok(()) },
                |()| async move {
                    follower_physical.fetch_add(1, Ordering::SeqCst);
                    Vec::new()
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert_eq!(physical.load(Ordering::SeqCst), 0);
        tokio::time::advance(ACTIVITY_SNAPSHOT_REUSE - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(physical.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_millis(1)).await;
        follower
            .await
            .expect("follower task")
            .expect("follower Activity read");
        assert_eq!(physical.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn activity_metrics_publish_only_fixed_bounded_labels() {
        let metrics = prometheus_cluster_activity();
        for path in ["fanout", "reuse", "directory_error"] {
            assert!(metrics.contains(&format!("path=\"{path}\"")));
        }
        for outcome in [
            "answered",
            "unhealthy",
            "unsupported",
            "unreachable",
            "timed_out",
            "refused",
            "http_error",
            "invalid_response",
            "local_unavailable",
        ] {
            assert!(metrics.contains(&format!("outcome=\"{outcome}\"")));
        }
        assert!(!metrics.contains("node_id="));
        assert!(!metrics.contains("hostname="));
    }

    #[tokio::test(start_paused = true)]
    async fn ninth_peer_cannot_extend_the_common_deadline() {
        let peers = (0..9)
            .map(|index| ActivityPeer {
                node_id: format!("node-{index}"),
                raft_id: index as u64,
                http_base: Some(format!("http://node-{index}:32400")),
                reachable: true,
            })
            .collect();
        let started = tokio::time::Instant::now();
        let timeout = Duration::from_millis(100);
        let deadline = started + timeout;
        let ninth_started_ms = Arc::new(AtomicU64::new(u64::MAX));
        let ninth = Arc::clone(&ninth_started_ms);
        let outcomes = collect_peer_outcomes(peers, deadline, move |peer| {
            if peer.node_id == "node-8" {
                ninth.store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
            }
            async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                PeerActivityOutcome::Unreachable
            }
        })
        .await;
        assert_eq!(outcomes.len(), 9);
        assert!(outcomes
            .iter()
            .all(|(_, outcome)| *outcome == PeerActivityOutcome::TimedOut));
        let ninth_started_ms = ninth_started_ms.load(Ordering::SeqCst);
        let completed_ms = started.elapsed().as_millis() as u64;
        assert_eq!(
            ninth_started_ms, 100,
            "the ninth peer did not start at the shared deadline"
        );
        assert_eq!(
            completed_ms, 100,
            "the ninth peer received time beyond the shared deadline"
        );
    }
}
