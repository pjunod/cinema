//! Cluster placement transport and liveness for capability-authenticated HLS.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, HeaderName, Response, StatusCode};
use futures_util::{stream, StreamExt, TryStreamExt};
use plurx_core::cluster::membership::MembershipManager;
use plurx_core::domain::{MediaSessionRenewal, MediaSessionRoute, OwnedMediaSessionLease};
use plurx_core::error::StoreError;
use plurx_core::store::Store;
use plurx_core::transcode::OutputGrade;
use serde::{Deserialize, Serialize};

use crate::http::peer_transport::{
    deadline_after, PeerAuthMode, PeerTransport, PeerTransportError,
};
use crate::state::AppState;
use crate::transcode::{SessionKind, SessionRequest, StartInfo};

pub(crate) const START_PATH: &str = "/internal/cluster/media/sessions/start";
pub(crate) const ABORT_PATH: &str = "/internal/cluster/media/sessions/abort";
pub(crate) const RELAY_PATH: &str = "/internal/cluster/media/sessions/relay";
pub(crate) const MAX_CONTROL_REQUEST_BYTES: usize = 96 * 1024;

const MAX_START_RESPONSE_BYTES: usize = 128 * 1024;
pub(crate) const START_DEADLINE: Duration = Duration::from_secs(50);
pub(crate) const OWNER_ASSIGNMENT_DEADLINE: Duration = Duration::from_secs(3);
pub(crate) const ACTIVATION_STORE_DEADLINE: Duration = Duration::from_secs(3);
pub(crate) const ACTIVATION_FAST_RECONCILIATION: Duration = Duration::from_secs(3);
const ABORT_DEADLINE: Duration = Duration::from_secs(5);
const RELAY_HEADERS_DEADLINE: Duration = Duration::from_secs(35);
const LEASE_INTERVAL: Duration = Duration::from_secs(3);
pub(crate) const LEASE_TTL_MS: i64 = 12_000;
pub(crate) const ACTIVATION_CONFIRMATION_WINDOW: Duration = Duration::from_secs(55);
/// Begins on the worker before the start response leaves it, so it must
/// strictly outlive every later ingress phase plus a scheduling margin.
pub(crate) const REMOTE_ACTIVATION_CONFIRMATION_WINDOW: Duration = Duration::from_secs(
    START_DEADLINE.as_secs()
        + OWNER_ASSIGNMENT_DEADLINE.as_secs()
        + ACTIVATION_STORE_DEADLINE.as_secs()
        + ACTIVATION_FAST_RECONCILIATION.as_secs()
        + ACTIVATION_CONFIRMATION_WINDOW.as_secs()
        + 10,
);
const MAX_MEDIA_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const ROUTE_CACHE_TTL: Duration = Duration::from_secs(1);
const MAX_ROUTE_CACHE_ENTRIES: usize = 4_096;
const ROUTE_QUERY_SHARDS: usize = 32;
const ROUTE_GENERATION_SHARDS: usize = 4_096;
const ROUTE_QUERY_DEADLINE: Duration = Duration::from_secs(3);
const LEASE_RENEWAL_BATCH: usize = 256;
const LEASE_RENEWAL_FANOUT: usize = 16;
const LEASE_RENEWAL_DEADLINE: Duration = Duration::from_secs(4);
const LEASE_RENEWAL_MIN_REMAINING_MS: i64 = 4_000;
const MAX_STALE_SETTLEMENTS_PER_TICK: usize = 64;
const STALE_SETTLEMENT_DEADLINE: Duration = Duration::from_secs(4);
const STALE_SETTLEMENT_RETRY_BACKOFF: Duration = Duration::from_secs(30);
const STALE_SETTLEMENT_MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteStartRequest {
    pub protocol_version: i64,
    pub incarnation_id: String,
    pub user_id: i64,
    pub request: SessionRequest,
}

impl RemoteStartRequest {
    pub(crate) fn is_valid(&self) -> bool {
        self.protocol_version == crate::media_pool::PROTOCOL_VERSION
            && uuid::Uuid::parse_str(&self.incarnation_id).is_ok()
            && self.user_id > 0
            && self.request.request_id.as_deref() == Some(self.incarnation_id.as_str())
            && worker_session_request_is_valid(&self.request)
    }
}

/// The request contract shared by public ingress and private worker ingress.
///
/// The cluster envelope separately binds the internal request id to its
/// incarnation. Public idempotency keys deliberately have a wider syntax, but
/// every field that reaches a local worker must obey the same media bounds as
/// a request sent to a peer.
pub(crate) fn worker_session_request_is_valid(request: &SessionRequest) -> bool {
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
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteAbortRequest {
    pub incarnation_id: String,
    pub session_id: String,
}

impl RemoteAbortRequest {
    pub(crate) fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.incarnation_id).is_ok()
            && uuid::Uuid::parse_str(&self.session_id).is_ok()
    }
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
    pub range: Option<String>,
    pub if_none_match: Option<String>,
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
            if_none_match: value(axum::http::header::IF_NONE_MATCH),
            if_modified_since: value(axum::http::header::IF_MODIFIED_SINCE),
        }
    }

    fn is_valid(&self) -> bool {
        [&self.range, &self.if_none_match, &self.if_modified_since]
            .into_iter()
            .flatten()
            .all(|value| {
                value.len() <= 1_024
                    && !value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
                    && value.parse::<axum::http::HeaderValue>().is_ok()
            })
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayRequest {
    pub session_id: String,
    pub resource: RelayResource,
    #[serde(default)]
    pub headers: RelayHeaders,
}

impl RelayRequest {
    pub(crate) fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.resource.is_valid()
            && self.headers.is_valid()
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
    route_queries: Arc<Vec<tokio::sync::Mutex<()>>>,
    route_generations: Arc<Vec<std::sync::atomic::AtomicU64>>,
    lease_seeds: Arc<tokio::sync::Mutex<HashMap<String, (String, i64)>>>,
    #[cfg(test)]
    route_store_queries: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
struct CachedRoute {
    route: Option<MediaSessionRoute>,
    expires_at: tokio::time::Instant,
}

impl MediaSessionCoordinator {
    pub(crate) fn new(membership: MembershipManager, store: Arc<dyn Store>) -> Arc<Self> {
        Arc::new(Self {
            transport: PeerTransport::new(membership.clone()),
            membership,
            store,
            routes: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
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
            #[cfg(test)]
            route_store_queries: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Cache active routes and short negative answers. Deterministic query
    /// shards single-flight repeated capabilities and hard-bound concurrent
    /// consensus reads even when an unauthenticated caller sprays random UUIDs.
    /// Activation overwrites a prior miss immediately, and no cache entry can
    /// extend the exact durable lease boundary.
    pub(crate) async fn route(
        &self,
        session_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        let deadline = tokio::time::Instant::now() + ROUTE_QUERY_DEADLINE;
        if let Some(cached) = tokio::time::timeout_at(deadline, self.cached_route(session_id))
            .await
            .map_err(|_| {
                StoreError::Database("media-session route admission timed out".to_owned())
            })?
        {
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
            return Ok(cached);
        }
        let observed_generation =
            self.route_generations[generation_shard].load(std::sync::atomic::Ordering::Acquire);
        #[cfg(test)]
        self.route_store_queries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let route = tokio::time::timeout_at(deadline, self.store.media_session_route(session_id))
            .await
            .map_err(|_| StoreError::Database("media-session route lookup timed out".to_owned()))??
            .filter(authorizing_route);
        tokio::time::timeout_at(
            deadline,
            self.cache_queried_route_result(session_id, route, observed_generation),
        )
        .await
        .map_err(|_| StoreError::Database("media-session route lookup timed out".to_owned()))
    }

    pub(crate) async fn cache_route(&self, route: MediaSessionRoute) {
        let session_id = route.session_id.clone();
        let route = authorizing_route(&route).then_some(route);
        self.cache_route_result(&session_id, route).await;
    }

    pub(crate) async fn cache_miss(&self, session_id: &str) {
        self.cache_route_result(session_id, None).await;
    }

    async fn cached_route(&self, session_id: &str) -> Option<Option<MediaSessionRoute>> {
        let now = tokio::time::Instant::now();
        let mut routes = self.routes.lock().await;
        let cached = routes.get(session_id)?;
        if cached.expires_at > now && cached.route.as_ref().is_none_or(authorizing_route) {
            return Some(cached.route.clone());
        }
        routes.remove(session_id);
        None
    }

    async fn cache_route_result(&self, session_id: &str, route: Option<MediaSessionRoute>) {
        let now = tokio::time::Instant::now();
        let mut routes = self.routes.lock().await;
        routes.retain(|_, cached| {
            cached.expires_at > now && cached.route.as_ref().is_none_or(authorizing_route)
        });
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
        routes.retain(|_, cached| {
            cached.expires_at > now && cached.route.as_ref().is_none_or(authorizing_route)
        });
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
        self.lease_seeds.lock().await.insert(
            route.incarnation_id.clone(),
            (route.session_id.clone(), route.lease_expires_at_ms),
        );
    }

    async fn take_lease_seeds(&self) -> HashMap<String, (String, i64)> {
        std::mem::take(&mut *self.lease_seeds.lock().await)
    }

    async fn peer_base(
        &self,
        node_id: &str,
        deadline: tokio::time::Instant,
    ) -> Result<String, PeerTransportError> {
        tokio::time::timeout_at(deadline, self.membership.activity_peers())
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
    ) -> Result<RemoteStartResponse, PeerTransportError> {
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
            .request(
                owner_node_id,
                &base,
                reqwest::Method::POST,
                START_PATH,
                body,
                deadline,
                MAX_START_RESPONSE_BYTES,
                PeerAuthMode::ExactRequest,
            )
            .await?;
        if !response.status.is_success() {
            return Err(if response.status == reqwest::StatusCode::REQUEST_TIMEOUT {
                PeerTransportError::TimedOut
            } else {
                PeerTransportError::InvalidResponse
            });
        }
        serde_json::from_slice::<RemoteStartResponse>(&response.body)
            .ok()
            .filter(RemoteStartResponse::is_valid)
            .ok_or(PeerTransportError::InvalidResponse)
    }

    pub(crate) async fn abort_remote(&self, owner_node_id: &str, request: &RemoteAbortRequest) {
        let Ok(body) = serde_json::to_vec(request) else {
            return;
        };
        let deadline = deadline_after(ABORT_DEADLINE);
        let Ok(base) = self.peer_base(owner_node_id, deadline).await else {
            return;
        };
        let _ = self
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
            .await;
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
        let deadline = deadline_after(RELAY_HEADERS_DEADLINE);
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

fn authorizing_route(route: &MediaSessionRoute) -> bool {
    route.state == "active" && route.lease_expires_at_ms > unix_ms()
}

fn relay_response(response: reqwest::Response) -> Result<Response<Body>, PeerTransportError> {
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
    let stream = response
        .bytes_stream()
        .map_err(|error| std::io::Error::other(error.to_string()));
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

async fn fence_and_reap_sessions(state: &AppState, sessions: Vec<(String, String, &'static str)>) {
    if sessions.is_empty() {
        return;
    }
    let session_ids = sessions
        .iter()
        .map(|(_, session_id, _)| session_id.clone())
        .collect::<Vec<_>>();
    state.transcode.fence_sessions(&session_ids).await;
    for (_, session_id, reason) in sessions {
        let cleanup_state = state.clone();
        tokio::spawn(async move {
            cleanup_state.media_sessions.cache_miss(&session_id).await;
            cleanup_state
                .transcode
                .stop_session(&session_id, reason)
                .await;
        });
    }
}

/// Renew only locally live workers. Lost ownership self-fences immediately;
/// a transient store outage is allowed to use the already-committed lease but
/// never past its exact expiry.
pub(crate) async fn lease_loop(state: AppState) {
    let mut interval = tokio::time::interval(LEASE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut known = HashMap::<String, (String, i64)>::new();
    let mut settling = HashSet::<String>::new();
    let mut settlement_backoff = HashMap::<String, StaleSettlementBackoff>::new();
    let (settled_tx, mut settled_rx) =
        tokio::sync::mpsc::channel::<StaleSettlementResult>(MAX_STALE_SETTLEMENTS_PER_TICK);
    loop {
        interval.tick().await;
        while let Ok(result) = settled_rx.try_recv() {
            settling.remove(&result.session_id);
            if let Some(retry) = result.retry {
                settlement_backoff.insert(result.session_id, retry);
            } else {
                settlement_backoff.remove(&result.session_id);
            }
        }
        let now_ms = unix_ms();
        let live = state
            .transcode
            .renewable_session_ids()
            .await
            .into_iter()
            .collect::<HashSet<_>>();
        known.extend(state.media_sessions.take_lease_seeds().await);
        known.retain(|_, (session_id, _)| live.contains(session_id));
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
        let Some(routes) = routes else {
            let failure_now_ms = unix_ms();
            let expired = known
                .iter()
                .filter(|(_, (session_id, expires_at_ms))| {
                    live.contains(session_id) && *expires_at_ms <= failure_now_ms
                })
                .map(|(incarnation_id, (session_id, _))| {
                    (incarnation_id.clone(), session_id.clone())
                })
                .collect::<Vec<_>>();
            let cleanup = expired
                .iter()
                .map(|(incarnation_id, session_id)| {
                    (
                        incarnation_id.clone(),
                        session_id.clone(),
                        "cluster lease expired",
                    )
                })
                .collect::<Vec<_>>();
            for (incarnation_id, _) in expired {
                known.remove(&incarnation_id);
            }
            fence_and_reap_sessions(&state, cleanup).await;
            continue;
        };
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
            .filter(|(incarnation_id, (session_id, _))| {
                live.contains(session_id) && !current.contains(incarnation_id.as_str())
            })
            .map(|(incarnation_id, (session_id, _))| (incarnation_id.clone(), session_id.clone()))
            .collect::<Vec<_>>();
        for route in &routes {
            known.insert(
                route.incarnation_id.clone(),
                (route.session_id.clone(), route.lease_expires_at_ms),
            );
        }
        let active = routes
            .iter()
            .filter(|route| live.contains(&route.session_id))
            .collect::<Vec<_>>();
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
                    .map(|route| MediaSessionRenewal {
                        incarnation_id: route.incarnation_id.clone(),
                        owner_epoch: route.owner_epoch,
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
                (chunk, renewal_now_ms, outcome)
            }
        }))
        .buffer_unordered(LEASE_RENEWAL_FANOUT)
        .collect::<Vec<_>>()
        .await;
        let mut cleanup = Vec::new();
        for (chunk, renewal_now_ms, outcome) in renewal_outcomes {
            match outcome {
                Ok(Ok(renewed)) => {
                    let renewed = renewed.into_iter().collect::<HashSet<_>>();
                    for route in &chunk {
                        if renewed.contains(&route.incarnation_id) {
                            known.insert(
                                route.incarnation_id.clone(),
                                (
                                    route.session_id.clone(),
                                    renewal_now_ms.saturating_add(LEASE_TTL_MS),
                                ),
                            );
                        } else {
                            cleanup.push((
                                route.incarnation_id.clone(),
                                route.session_id.clone(),
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
                            "cluster lease renewal ambiguous",
                        ));
                        known.remove(&route.incarnation_id);
                    }
                }
            }
        }
        for (incarnation_id, session_id) in lost {
            cleanup.push((incarnation_id.clone(), session_id, "cluster lease lost"));
            known.remove(&incarnation_id);
        }
        cleanup.sort_unstable_by(|left, right| left.1.cmp(&right.1));
        cleanup.dedup_by(|left, right| left.1 == right.1);
        fence_and_reap_sessions(&state, cleanup).await;
        let settlement_now = tokio::time::Instant::now();
        let unsettled = take_stale_settlement_candidates(
            &routes,
            &live,
            &mut settling,
            &mut settlement_backoff,
            settlement_now,
        );
        for (route, previous_failures) in unsettled {
            let cleanup_state = state.clone();
            let session_id = route.session_id.clone();
            let settled_tx = settled_tx.clone();
            tokio::spawn(async move {
                let failed = match tokio::time::timeout(
                    STALE_SETTLEMENT_DEADLINE,
                    cleanup_state
                        .store
                        .end_media_session(&session_id, unix_ms()),
                )
                .await
                {
                    Ok(Ok(_)) => false,
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "stale media-session settlement unavailable");
                        true
                    }
                    Err(_) => {
                        tracing::debug!("stale media-session settlement timed out");
                        true
                    }
                };
                cleanup_state.media_sessions.cache_miss(&session_id).await;
                let retry = failed.then(|| {
                    let failures = previous_failures.saturating_add(1);
                    StaleSettlementBackoff {
                        failures,
                        next_attempt: tokio::time::Instant::now()
                            + stale_settlement_retry_delay(failures),
                    }
                });
                let _ = settled_tx
                    .send(StaleSettlementResult { session_id, retry })
                    .await;
            });
            known.remove(&route.incarnation_id);
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

fn renewal_chunks<T>(items: &[T]) -> impl Iterator<Item = &[T]> {
    items.chunks(LEASE_RENEWAL_BATCH)
}

fn stale_settlement_retry_delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(4);
    STALE_SETTLEMENT_RETRY_BACKOFF
        .saturating_mul(1_u32 << exponent)
        .min(STALE_SETTLEMENT_MAX_BACKOFF)
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
) -> Vec<(OwnedMediaSessionLease, u32)> {
    let mut candidates = Vec::new();

    // A due retry already owns one of the fixed slots. Convert that exact slot
    // from retained backoff to in-flight before fresh rows are considered, so
    // route ordering cannot let unrelated work steal it.
    for route in routes {
        if live.contains(&route.session_id) || settling.contains(&route.session_id) {
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

    fn valid_start_request() -> RemoteStartRequest {
        let incarnation_id = "00000000-0000-4000-8000-0000000000a1".to_owned();
        RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: incarnation_id.clone(),
            user_id: 7,
            request: SessionRequest {
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
            vod: false,
        }
    }

    fn media_route(session_id: &str) -> MediaSessionRoute {
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

    #[test]
    fn remote_start_contract_rejects_unfenced_or_noncanonical_inputs() {
        let request = valid_start_request();
        assert!(request.is_valid());
        assert!(worker_session_request_is_valid(&request.request));

        let mut incompatible = request.clone();
        incompatible.protocol_version = crate::media_pool::PROTOCOL_VERSION.saturating_sub(1);
        assert!(!incompatible.is_valid());

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
    fn remote_start_response_is_bound_to_the_session_capability() {
        let response = valid_start_response();
        assert!(response.is_valid());

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
            aac: false,
            preserve_dolby_vision: false,
        };
        unknown_copy.target_height = 0;
        assert!(unknown_copy.is_valid());
        unknown_copy.target_height = crate::transcode::MAX_HEIGHT + 1;
        assert!(unknown_copy.is_valid());
        unknown_copy.target_height = -1;
        assert!(!unknown_copy.is_valid());
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
        let headers = RelayHeaders {
            range: Some("bytes=0-99".to_owned()),
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
                    + ACTIVATION_STORE_DEADLINE
                    + ACTIVATION_FAST_RECONCILIATION
                    + ACTIVATION_CONFIRMATION_WINDOW,
            "the worker fallback must begin earlier and end after every ingress phase"
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
        coordinator.cache_route(route).await;
        assert!(
            coordinator
                .route(session_id)
                .await
                .expect("terminal route")
                .is_none(),
            "terminal routes must be negative cache entries, never authorizers"
        );
    }

    #[tokio::test]
    async fn relay_forwards_only_media_headers_without_buffering_the_body() {
        let app = Router::new().route(
            "/media",
            get(|| async {
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
        let peer = reqwest::Client::new()
            .get(format!("http://{address}/media"))
            .send()
            .await
            .expect("request peer stream");

        let relayed = relay_response(peer).expect("build streamed relay");
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

        let drain = axum::body::to_bytes(relayed.into_body(), 1_024);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), drain)
                .await
                .is_err(),
            "relay_response must return before a peer finishes its body"
        );
        server.abort();
    }
}
