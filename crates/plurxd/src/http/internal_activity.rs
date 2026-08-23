//! Narrow cluster-only activity snapshot transport.
//!
//! This route is deliberately outside `/api/v1`: household credentials do
//! not authorize it, and its peer addresses and authority never enter a
//! public response. The aggregation/UI remains in `system.rs`.

use std::collections::HashMap;
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::Json;
use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::{ActivityPeer, ActivityPeerAuth, MembershipError};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

pub const PATH: &str = "/_internal/v1/activity-snapshot";
#[allow(dead_code)] // aggregation child #326 calls the pre-wired client
pub const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const MAX_DELIVERIES: usize = 512;
const MAX_NODE_ID_BYTES: usize = 256;
const MAX_USER_BYTES: usize = 256;
const MAX_TITLE_BYTES: usize = 2 * 1024;
const PEER_CONCURRENCY: usize = 8;
const NODE_HEADER: &str = "x-plurx-cluster-node";
const TARGET_HEADER: &str = "x-plurx-cluster-target";
const TIMESTAMP_HEADER: &str = "x-plurx-cluster-time-ms";
const SIGNATURE_HEADER: &str = "x-plurx-cluster-signature";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActivitySnapshot {
    pub node_id: String,
    pub deliveries: Vec<ActivityDelivery>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityDelivery {
    pub method: String,
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
    InvalidResponse,
}

#[allow(dead_code)] // wired now so #326 does not need state/main territory
#[derive(Clone)]
pub struct PeerActivityClient {
    membership: plurx_core::cluster::membership::MembershipManager,
    client: Result<reqwest::Client, String>,
}

#[allow(dead_code)]
impl PeerActivityClient {
    #[must_use]
    pub fn new(membership: plurx_core::cluster::membership::MembershipManager) -> Self {
        Self {
            membership,
            client: peer_http_client(),
        }
    }

    pub async fn snapshots(&self) -> Result<Vec<(String, PeerActivityOutcome)>, MembershipError> {
        let peers = self.membership.activity_peers().await?;
        let client = self.clone();
        Ok(collect_peer_outcomes(peers, TIMEOUT, move |peer| {
            let client = client.clone();
            async move {
                if !peer.reachable {
                    PeerActivityOutcome::Unhealthy
                } else if let Some(http_base) = peer.http_base {
                    client.snapshot(&peer.node_id, &http_base).await
                } else {
                    PeerActivityOutcome::Unreachable
                }
            }
        })
        .await)
    }

    async fn snapshot(&self, expected_node_id: &str, base: &str) -> PeerActivityOutcome {
        let Some(url) = activity_url(base) else {
            return PeerActivityOutcome::Unreachable;
        };
        let Some(client) = self.client.as_ref().ok() else {
            return PeerActivityOutcome::Unreachable;
        };
        let Ok(auth) = self
            .membership
            .sign_activity_request(expected_node_id, unix_ms())
        else {
            return PeerActivityOutcome::Unreachable;
        };
        let request = client
            .get(url)
            .timeout(TIMEOUT)
            .header(NODE_HEADER, &auth.node_id)
            .header(TARGET_HEADER, &auth.target_node_id)
            .header(TIMESTAMP_HEADER, auth.timestamp_ms)
            .header(SIGNATURE_HEADER, &auth.signature)
            .send()
            .await;
        match request {
            Ok(response) if response.status().is_success() => {
                read_snapshot(response, expected_node_id).await
            }
            Err(error) if error.is_timeout() => PeerActivityOutcome::TimedOut,
            Ok(_) | Err(_) => PeerActivityOutcome::Unreachable,
        }
    }
}

fn peer_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())
}

async fn collect_peer_outcomes<F, Fut>(
    peers: Vec<ActivityPeer>,
    timeout: Duration,
    fetch: F,
) -> Vec<(String, PeerActivityOutcome)>
where
    F: Fn(ActivityPeer) -> Fut + Clone,
    Fut: Future<Output = PeerActivityOutcome>,
{
    let deadline = tokio::time::Instant::now() + timeout;
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

fn activity_url(base: &str) -> Option<reqwest::Url> {
    let normalized = plurx_core::cluster::membership::normalize_internal_http_base(base)?;
    let mut url = reqwest::Url::parse(&normalized).ok()?;
    url.set_path(PATH);
    Some(url)
}

async fn read_snapshot(
    mut response: reqwest::Response,
    expected_node_id: &str,
) -> PeerActivityOutcome {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return PeerActivityOutcome::InvalidResponse;
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let Ok(next_len) = u64::try_from(body.len().saturating_add(chunk.len())) else {
                    return PeerActivityOutcome::InvalidResponse;
                };
                if next_len > MAX_RESPONSE_BYTES {
                    return PeerActivityOutcome::InvalidResponse;
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) if error.is_timeout() => return PeerActivityOutcome::TimedOut,
            Err(_) => return PeerActivityOutcome::Unreachable,
        }
    }
    let Ok(snapshot) = serde_json::from_slice::<ActivitySnapshot>(&body) else {
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
        && snapshot.deliveries.iter().all(|delivery| {
            delivery.method.len() <= 32
                && delivery.user.len() <= MAX_USER_BYTES
                && delivery.title.len() <= MAX_TITLE_BYTES
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
    let candidates = activity_candidates_bounded(
        transcodes,
        state.streams.list_bounded(MAX_DELIVERIES),
        state.direct_plays.list_bounded(MAX_DELIVERIES),
        MAX_DELIVERIES,
    );
    let transcode_ids = candidates
        .iter()
        .filter_map(|candidate| match candidate {
            ActivityCandidate::Transcode(candidate) => Some(candidate.id.clone()),
            ActivityCandidate::Remux(_) | ActivityCandidate::Direct(_) => None,
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
    let mut titles = HashMap::new();
    for candidate in candidates {
        match candidate {
            ActivityCandidate::Transcode(candidate) => {
                let Some((session, method)) = transcode_details.remove(&candidate.id) else {
                    continue;
                };
                deliveries.push(ActivityDelivery {
                    method: method.as_str().to_owned(),
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
            ActivityCandidate::Remux(stream) => {
                let title = title(state, stream.item_id, &mut titles).await;
                deliveries.push(ActivityDelivery {
                    method: "remux".to_owned(),
                    user: bounded_text(stream.user_name, MAX_USER_BYTES),
                    file_id: stream.file_id,
                    item_id: stream.item_id,
                    title: bounded_text(title, MAX_TITLE_BYTES),
                    started_unix: stream.started_unix,
                    idle_seconds: (stream.delivered_idle_ms.max(0) / 1_000) as u64,
                    delivered_bytes: Some(stream.delivered_bytes),
                    delivered_bps: stream.delivered_bps,
                });
            }
            ActivityCandidate::Direct(play) => {
                let title = title(state, play.item_id, &mut titles).await;
                deliveries.push(ActivityDelivery {
                    method: "direct".to_owned(),
                    user: bounded_text(play.user_name, MAX_USER_BYTES),
                    file_id: play.file_id,
                    item_id: play.item_id,
                    title: bounded_text(title, MAX_TITLE_BYTES),
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
    bounded_snapshot(state.node_id.clone(), deliveries)
}

#[derive(Debug)]
enum ActivityCandidate {
    Transcode(crate::transcode::DeliveryCandidate),
    Remux(crate::progressive::StreamListing),
    Direct(crate::delivery::Live),
}

impl ActivityCandidate {
    fn ordering_key(&self) -> (i64, &'static str, &str) {
        match self {
            Self::Transcode(candidate) => (candidate.started_unix, "hls", &candidate.id),
            Self::Remux(stream) => (stream.started_unix, "remux", &stream.id),
            Self::Direct(play) => (play.started_unix, "direct", &play.registry_id),
        }
    }
}

fn activity_candidates_bounded(
    transcodes: Vec<crate::transcode::DeliveryCandidate>,
    remuxes: Vec<crate::progressive::StreamListing>,
    direct: Vec<crate::delivery::Live>,
    limit: usize,
) -> Vec<ActivityCandidate> {
    let mut candidates = transcodes
        .into_iter()
        .map(ActivityCandidate::Transcode)
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

/// Keep the producer and consumer on the same exact wire budget. Serializing
/// each candidate accounts for JSON escaping without repeatedly encoding the
/// whole response, and preserves the newest-first ordering above.
fn bounded_snapshot(node_id: String, deliveries: Vec<ActivityDelivery>) -> ActivitySnapshot {
    let node_id = bounded_text(node_id, MAX_NODE_ID_BYTES);
    let mut snapshot = ActivitySnapshot {
        node_id,
        deliveries: Vec::new(),
    };
    let mut encoded_bytes = serde_json::to_vec(&snapshot)
        .map(|encoded| encoded.len())
        .unwrap_or(MAX_RESPONSE_BYTES as usize);
    for delivery in deliveries.into_iter().take(MAX_DELIVERIES) {
        let Ok(encoded) = serde_json::to_vec(&delivery) else {
            continue;
        };
        let separator = usize::from(!snapshot.deliveries.is_empty());
        let added = encoded.len().saturating_add(separator);
        if encoded_bytes.saturating_add(added) > MAX_RESPONSE_BYTES as usize {
            continue;
        }
        encoded_bytes += added;
        snapshot.deliveries.push(delivery);
    }
    snapshot
}

async fn title(state: &AppState, item_id: i64, titles: &mut HashMap<i64, String>) -> String {
    if let Some(title) = titles.get(&item_id) {
        return title.clone();
    }
    let title = state
        .store
        .get_item(item_id)
        .await
        .ok()
        .flatten()
        .map_or_else(String::new, |item| item.title);
    titles.insert(item_id, title.clone());
    title
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
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::response::Redirect;
    use axum::routing::get;
    use axum::Router;
    use bytes::Bytes;

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
        };
        assert!(snapshot_is_bounded(&snapshot, "node-b"));
        assert!(!snapshot_is_bounded(&snapshot, "node-c"));
    }

    #[test]
    fn producer_obeys_the_exact_json_byte_budget_after_escaping() {
        let delivery = ActivityDelivery {
            method: "x".repeat(32),
            user: "\0".repeat(MAX_USER_BYTES),
            file_id: i64::MAX,
            item_id: i64::MAX,
            title: "\0".repeat(MAX_TITLE_BYTES),
            started_unix: i64::MAX,
            idle_seconds: u64::MAX,
            delivered_bytes: Some(i64::MAX),
            delivered_bps: Some(i64::MAX),
        };
        let snapshot = bounded_snapshot("node-b".to_owned(), vec![delivery; MAX_DELIVERIES]);
        let encoded = serde_json::to_vec(&snapshot).expect("bounded snapshot serializes");
        assert!(encoded.len() <= MAX_RESPONSE_BYTES as usize);
        assert!(snapshot.deliveries.len() < MAX_DELIVERIES);
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

        let selected = activity_candidates_bounded(transcodes, remuxes, direct, MAX_DELIVERIES);
        assert_eq!(selected.len(), MAX_DELIVERIES);
        assert!(selected
            .iter()
            .all(|candidate| matches!(candidate, ActivityCandidate::Direct(_))));
    }

    #[tokio::test]
    async fn peer_client_refuses_redirects_and_chunked_oversized_bodies() {
        let target_hits = Arc::new(AtomicUsize::new(0));
        let hits = Arc::clone(&target_hits);
        let app = Router::new()
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/target") }),
            )
            .route(
                "/target",
                get(move || {
                    let hits = Arc::clone(&hits);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        StatusCode::OK
                    }
                }),
            )
            .route(
                "/oversized",
                get(|| async {
                    let chunks = stream::iter([
                        Ok::<Bytes, Infallible>(Bytes::from(vec![
                            b'x';
                            MAX_RESPONSE_BYTES as usize
                        ])),
                        Ok::<Bytes, Infallible>(Bytes::from_static(b"x")),
                    ]);
                    Body::from_stream(chunks)
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind peer transport fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve peer transport fixture");
        });
        let client = peer_http_client().expect("peer client");
        let redirect = client
            .get(format!("http://{address}/redirect"))
            .send()
            .await
            .expect("request redirect fixture");
        assert_eq!(redirect.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);

        let oversized = client
            .get(format!("http://{address}/oversized"))
            .send()
            .await
            .expect("request oversized fixture");
        assert!(oversized.content_length().is_none());
        assert_eq!(
            read_snapshot(oversized, "node-b").await,
            PeerActivityOutcome::InvalidResponse
        );
        server.abort();
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
        let ninth_started_ms = Arc::new(AtomicU64::new(u64::MAX));
        let ninth = Arc::clone(&ninth_started_ms);
        let outcomes = collect_peer_outcomes(peers, timeout, move |peer| {
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
