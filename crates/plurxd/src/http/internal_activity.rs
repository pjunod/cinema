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

use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::Json;
use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::{ActivityPeer, ActivityPeerAuth, MembershipError};
use serde::{Deserialize, Serialize};

use super::peer_transport::{
    deadline_after, PeerAuthMode, PeerTransport, PeerTransportError, NODE_HEADER, SIGNATURE_HEADER,
    TARGET_HEADER, TIMESTAMP_HEADER,
};
use crate::state::AppState;

pub const PATH: &str = "/_internal/v1/activity-snapshot";
#[allow(dead_code)] // aggregation child #326 calls the pre-wired client
pub const TIMEOUT: Duration = Duration::from_secs(2);
const ACTIVITY_SNAPSHOT_REUSE: Duration = Duration::from_secs(1);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
// A saturated delivery list must not erase every analysis row from a peer's
// Activity snapshot. Reserve one quarter of the shared wire budget whenever
// progress exists; unused space remains available when it does not.
const ANALYSIS_RESPONSE_RESERVE_BYTES: usize = MAX_RESPONSE_BYTES / 4;
const MAX_DELIVERIES: usize = 512;
const MAX_ANALYSIS_PROGRESS: usize = 64;
const MAX_NODE_ID_BYTES: usize = 256;
const MAX_USER_BYTES: usize = 256;
const MAX_TITLE_BYTES: usize = 2 * 1024;
const PEER_CONCURRENCY: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActivitySnapshot {
    pub node_id: String,
    pub deliveries: Vec<ActivityDelivery>,
    #[serde(default)]
    pub analysis: Vec<crate::state::AnalysisProgress>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityDelivery {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<String>,
    pub user: String,
    pub file_id: i64,
    pub item_id: i64,
    pub title: String,
    pub started_unix: i64,
    pub idle_seconds: u64,
    pub delivered_bytes: Option<i64>,
    pub delivered_bps: Option<i64>,
}

#[allow(dead_code)] // consumed by #326 once this dependency merges
#[derive(Clone, Debug, PartialEq)]
pub enum PeerActivityOutcome {
    Answered(ActivitySnapshot),
    Unhealthy,
    Unreachable,
    TimedOut,
    Refused,
    HttpError,
    InvalidResponse,
}

pub type SharedPeerActivity = Arc<[(String, PeerActivityOutcome)]>;

#[derive(Default)]
struct PeerActivityReadGate {
    last_started: Option<tokio::time::Instant>,
    completed: Option<(tokio::time::Instant, SharedPeerActivity)>,
}

static ACTIVITY_AGGREGATIONS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static ACTIVITY_PEER_OUTCOMES: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

fn record_aggregation(path: usize) {
    ACTIVITY_AGGREGATIONS[path].fetch_add(1, Ordering::Relaxed);
}

fn record_peer_outcome(outcome: &PeerActivityOutcome) {
    let slot = match outcome {
        PeerActivityOutcome::Answered(_) => 0,
        PeerActivityOutcome::Unhealthy => 1,
        PeerActivityOutcome::Unreachable => 2,
        PeerActivityOutcome::TimedOut => 3,
        PeerActivityOutcome::Refused => 4,
        PeerActivityOutcome::HttpError => 5,
        PeerActivityOutcome::InvalidResponse => 6,
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
        "unreachable",
        "timed_out",
        "refused",
        "http_error",
        "invalid_response",
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
    membership: plurx_core::cluster::membership::MembershipManager,
    transport: PeerTransport,
    reads: Arc<tokio::sync::Mutex<PeerActivityReadGate>>,
}

#[allow(dead_code)]
impl PeerActivityClient {
    #[must_use]
    pub fn new(membership: plurx_core::cluster::membership::MembershipManager) -> Self {
        Self {
            transport: PeerTransport::new(membership.clone()),
            membership,
            reads: Arc::new(tokio::sync::Mutex::new(PeerActivityReadGate::default())),
        }
    }

    pub async fn snapshots(&self) -> Result<SharedPeerActivity, MembershipError> {
        let client = self.clone();
        shared_peer_activity(&self.reads, move || async move {
            let peers = match client.membership.activity_peers().await {
                Ok(peers) => peers,
                Err(error) => {
                    record_aggregation(2);
                    return Err(error);
                }
            };
            record_aggregation(0);
            let deadline = deadline_after(TIMEOUT);
            let fetcher = client.clone();
            let outcomes = collect_peer_outcomes(peers, deadline, move |peer| {
                let fetcher = fetcher.clone();
                async move {
                    if !peer.reachable {
                        PeerActivityOutcome::Unhealthy
                    } else if let Some(http_base) = peer.http_base {
                        fetcher.snapshot(&peer.node_id, &http_base, deadline).await
                    } else {
                        PeerActivityOutcome::Unreachable
                    }
                }
            })
            .await;
            for (_, outcome) in &outcomes {
                record_peer_outcome(outcome);
            }
            Ok(outcomes)
        })
        .await
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
                PeerAuthMode::LegacyActivity,
            )
            .await
        {
            Ok(response) => {
                classify_snapshot_response(response.status, &response.body, expected_node_id)
            }
            Err(PeerTransportError::TimedOut) => PeerActivityOutcome::TimedOut,
            Err(PeerTransportError::InvalidResponse) => PeerActivityOutcome::InvalidResponse,
            Err(PeerTransportError::Unreachable) => PeerActivityOutcome::Unreachable,
        }
    }
}

async fn shared_peer_activity<F, Fut>(
    gate: &tokio::sync::Mutex<PeerActivityReadGate>,
    fetch: F,
) -> Result<SharedPeerActivity, MembershipError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<(String, PeerActivityOutcome)>, MembershipError>>,
{
    let mut gate = gate.lock().await;
    let now = tokio::time::Instant::now();
    if let Some((completed_at, snapshot)) = &gate.completed {
        if now.saturating_duration_since(*completed_at) < ACTIVITY_SNAPSHOT_REUSE {
            record_aggregation(1);
            return Ok(Arc::clone(snapshot));
        }
    }
    if let Some(last_started) = gate.last_started {
        tokio::time::sleep_until(last_started + ACTIVITY_SNAPSHOT_REUSE).await;
    }
    // Reserve the physical start before the first fallible await. If this
    // caller is cancelled, the mutex is released but a successor still honors
    // the receiver-load floor.
    gate.last_started = Some(tokio::time::Instant::now());
    let snapshot: SharedPeerActivity = fetch().await?.into();
    gate.completed = Some((tokio::time::Instant::now(), Arc::clone(&snapshot)));
    Ok(snapshot)
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
) -> Result<(HeaderMap, Json<ActivitySnapshot>), StatusCode> {
    let auth = peer_auth_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if !state
        .membership
        .authorize_activity_request(&auth)
        .await
        .unwrap_or(false)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok((
        snapshot_response_headers(),
        Json(local_snapshot(&state).await),
    ))
}

fn snapshot_response_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    headers
}

fn peer_auth_from_headers(headers: &HeaderMap) -> Option<ActivityPeerAuth> {
    Some(ActivityPeerAuth {
        node_id: headers.get(NODE_HEADER)?.to_str().ok()?.to_owned(),
        target_node_id: headers.get(TARGET_HEADER)?.to_str().ok()?.to_owned(),
        timestamp_ms: headers.get(TIMESTAMP_HEADER)?.to_str().ok()?.parse().ok()?,
        signature: headers.get(SIGNATURE_HEADER)?.to_str().ok()?.to_owned(),
    })
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
    PeerActivityOutcome::Answered(snapshot)
}

fn snapshot_is_bounded(snapshot: &ActivitySnapshot, expected_node_id: &str) -> bool {
    snapshot.node_id == expected_node_id
        && !snapshot.node_id.is_empty()
        && snapshot.node_id.len() <= MAX_NODE_ID_BYTES
        && snapshot.deliveries.len() <= MAX_DELIVERIES
        && snapshot.analysis.len() <= MAX_ANALYSIS_PROGRESS
        && snapshot.deliveries.iter().all(|delivery| {
            delivery.method.len() <= 32
                && delivery.user.len() <= MAX_USER_BYTES
                && delivery.title.len() <= MAX_TITLE_BYTES
        })
        && snapshot.analysis.iter().all(|progress| {
            !progress.job_id.is_empty()
                && progress.job_id.len() <= 128
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
                    method: method.as_str().to_owned(),
                    presentation: Some(session.presentation.to_owned()),
                    user: bounded_text(session.user_name, MAX_USER_BYTES),
                    file_id: session.file_id,
                    item_id: session.item_id,
                    title: bounded_text(session.item_title, MAX_TITLE_BYTES),
                    started_unix: session.started_unix,
                    idle_seconds: session.idle_seconds,
                    delivered_bytes: Some(session.delivered_bytes),
                    delivered_bps: session.delivered_bps,
                });
            }
            ActivityCandidate::Vod(session) => {
                deliveries.push(ActivityDelivery {
                    method: "hls-copy".to_owned(),
                    presentation: Some("vod".to_owned()),
                    user: bounded_text(session.user_name, MAX_USER_BYTES),
                    file_id: session.file_id,
                    item_id: session.item_id,
                    title: bounded_text(session.item_title, MAX_TITLE_BYTES),
                    started_unix: session.started_unix,
                    idle_seconds: session.idle_seconds,
                    delivered_bytes: Some(0),
                    delivered_bps: None,
                });
            }
            ActivityCandidate::Remux(stream) => {
                deliveries.push(ActivityDelivery {
                    method: "remux".to_owned(),
                    presentation: None,
                    user: bounded_text(stream.user_name, MAX_USER_BYTES),
                    file_id: stream.file_id,
                    item_id: stream.item_id,
                    title: bounded_text(
                        titles.get(&stream.item_id).cloned().unwrap_or_default(),
                        MAX_TITLE_BYTES,
                    ),
                    started_unix: stream.started_unix,
                    idle_seconds: (stream.delivered_idle_ms.max(0) / 1_000) as u64,
                    delivered_bytes: Some(stream.delivered_bytes),
                    delivered_bps: stream.delivered_bps,
                });
            }
            ActivityCandidate::Direct(play) => {
                deliveries.push(ActivityDelivery {
                    method: "direct".to_owned(),
                    presentation: None,
                    user: bounded_text(play.user_name, MAX_USER_BYTES),
                    file_id: play.file_id,
                    item_id: play.item_id,
                    title: bounded_text(
                        titles.get(&play.item_id).cloned().unwrap_or_default(),
                        MAX_TITLE_BYTES,
                    ),
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
    bounded_snapshot(state.node_id.clone(), deliveries, analysis)
}

#[derive(Debug)]
enum ActivityCandidate {
    Transcode(crate::transcode::DeliveryCandidate),
    Vod(crate::vodserve::VodDeliveryInfo),
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
        .chain(vod.into_iter().map(ActivityCandidate::Vod))
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

/// Keep the producer and consumer on the same exact wire budget. Serializing
/// each candidate accounts for JSON escaping without repeatedly encoding the
/// whole response, and preserves the newest-first ordering above.
fn bounded_snapshot(
    node_id: String,
    deliveries: Vec<ActivityDelivery>,
    analysis: Vec<crate::state::AnalysisProgress>,
) -> ActivitySnapshot {
    let node_id = bounded_text(node_id, MAX_NODE_ID_BYTES);
    let mut snapshot = ActivitySnapshot {
        node_id,
        deliveries: Vec::new(),
        analysis: Vec::new(),
    };
    let mut encoded_bytes = serde_json::to_vec(&snapshot)
        .map(|encoded| encoded.len())
        .unwrap_or(MAX_RESPONSE_BYTES);
    let delivery_budget = if analysis.is_empty() {
        MAX_RESPONSE_BYTES
    } else {
        MAX_RESPONSE_BYTES.saturating_sub(ANALYSIS_RESPONSE_RESERVE_BYTES)
    };
    for delivery in deliveries.into_iter().take(MAX_DELIVERIES) {
        let Ok(encoded) = serde_json::to_vec(&delivery) else {
            continue;
        };
        let separator = usize::from(!snapshot.deliveries.is_empty());
        let added = encoded.len().saturating_add(separator);
        if encoded_bytes.saturating_add(added) > delivery_budget {
            continue;
        }
        encoded_bytes += added;
        snapshot.deliveries.push(delivery);
    }
    for progress in analysis.into_iter().take(MAX_ANALYSIS_PROGRESS) {
        let Ok(encoded) = serde_json::to_vec(&progress) else {
            continue;
        };
        let separator = usize::from(!snapshot.analysis.is_empty());
        let added = encoded.len().saturating_add(separator);
        if encoded_bytes.saturating_add(added) > MAX_RESPONSE_BYTES {
            continue;
        }
        encoded_bytes += added;
        snapshot.analysis.push(progress);
    }
    snapshot
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
            node_id: "node-b".to_owned(),
            deliveries: Vec::new(),
            analysis: Vec::new(),
        };
        assert!(snapshot_is_bounded(&snapshot, "node-b"));
        assert!(!snapshot_is_bounded(&snapshot, "node-c"));
    }

    #[test]
    fn producer_obeys_the_exact_json_byte_budget_after_escaping() {
        let delivery = ActivityDelivery {
            method: "x".repeat(32),
            presentation: Some("live-recovery".to_owned()),
            user: "\0".repeat(MAX_USER_BYTES),
            file_id: i64::MAX,
            item_id: i64::MAX,
            title: "\0".repeat(MAX_TITLE_BYTES),
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
    fn saturated_deliveries_preserve_analysis_progress_within_the_wire_budget() {
        let delivery = ActivityDelivery {
            method: "x".repeat(32),
            presentation: Some("live-recovery".to_owned()),
            user: "\0".repeat(MAX_USER_BYTES),
            file_id: i64::MAX,
            item_id: i64::MAX,
            title: "\0".repeat(MAX_TITLE_BYTES),
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
        assert_eq!(snapshot.analysis.len(), 1);
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
            id: "vod-session".to_owned(),
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

        assert!(matches!(selected.as_slice(), [ActivityCandidate::Vod(_)]));
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
            node_id: "node-b".to_owned(),
            deliveries: Vec::new(),
            analysis: Vec::new(),
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

    #[tokio::test(start_paused = true)]
    async fn one_completed_peer_wave_is_shared_then_refreshed() {
        let gate = Arc::new(tokio::sync::Mutex::new(PeerActivityReadGate::default()));
        let physical = Arc::new(AtomicUsize::new(0));
        let mut callers = Vec::new();
        for _ in 0..8 {
            let gate = Arc::clone(&gate);
            let physical = Arc::clone(&physical);
            callers.push(tokio::spawn(async move {
                shared_peer_activity(&gate, || async move {
                    physical.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![("node-b".to_owned(), PeerActivityOutcome::Unhealthy)])
                })
                .await
                .expect("shared Activity read")
            }));
        }
        let mut snapshots = Vec::new();
        for caller in callers {
            snapshots.push(caller.await.expect("caller task"));
        }
        assert_eq!(physical.load(Ordering::SeqCst), 1);
        assert!(snapshots
            .windows(2)
            .all(|pair| Arc::ptr_eq(&pair[0], &pair[1])));

        tokio::time::advance(ACTIVITY_SNAPSHOT_REUSE - Duration::from_millis(1)).await;
        let before_expiry = shared_peer_activity(&gate, || async {
            physical.fetch_add(1, Ordering::SeqCst);
            Ok(vec![("node-b".to_owned(), PeerActivityOutcome::Unhealthy)])
        })
        .await
        .expect("cached Activity read");
        assert!(Arc::ptr_eq(&snapshots[0], &before_expiry));
        assert_eq!(physical.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        let after_expiry = shared_peer_activity(&gate, || async {
            physical.fetch_add(1, Ordering::SeqCst);
            Ok(vec![("node-b".to_owned(), PeerActivityOutcome::Unhealthy)])
        })
        .await
        .expect("fresh Activity read");
        assert!(!Arc::ptr_eq(&snapshots[0], &after_expiry));
        assert_eq!(physical.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_owner_keeps_the_physical_start_floor() {
        let gate = Arc::new(tokio::sync::Mutex::new(PeerActivityReadGate::default()));
        let started = Arc::new(tokio::sync::Notify::new());
        let leader_gate = Arc::clone(&gate);
        let leader_started = Arc::clone(&started);
        let leader = tokio::spawn(async move {
            let _ = shared_peer_activity(&leader_gate, || async move {
                leader_started.notify_one();
                std::future::pending::<Result<Vec<(String, PeerActivityOutcome)>, MembershipError>>(
                )
                .await
            })
            .await;
        });
        started.notified().await;
        leader.abort();
        let _ = leader.await;

        let physical = Arc::new(AtomicUsize::new(0));
        let follower_gate = Arc::clone(&gate);
        let follower_physical = Arc::clone(&physical);
        let follower = tokio::spawn(async move {
            shared_peer_activity(&follower_gate, || async move {
                follower_physical.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            })
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
            "unreachable",
            "timed_out",
            "refused",
            "http_error",
            "invalid_response",
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
