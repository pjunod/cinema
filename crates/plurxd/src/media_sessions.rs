//! Cluster placement transport and liveness for capability-authenticated HLS.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, HeaderName, Response, StatusCode};
use futures_util::{stream, StreamExt, TryStreamExt};
use plurx_core::cluster::membership::MembershipManager;
use plurx_core::domain::{MediaSessionRenewal, MediaSessionRoute};
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
const ABORT_DEADLINE: Duration = Duration::from_secs(5);
const RELAY_HEADERS_DEADLINE: Duration = Duration::from_secs(35);
const LEASE_INTERVAL: Duration = Duration::from_secs(3);
pub(crate) const LEASE_TTL_MS: i64 = 12_000;
pub(crate) const ACTIVATION_CONFIRMATION_WINDOW: Duration = Duration::from_secs(55);
const MAX_MEDIA_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const ROUTE_CACHE_TTL: Duration = Duration::from_secs(1);
const MAX_ROUTE_CACHE_ENTRIES: usize = 4_096;
const LEASE_RENEWAL_BATCH: usize = 256;
const LEASE_RENEWAL_FANOUT: usize = 16;
const LEASE_RENEWAL_DEADLINE: Duration = Duration::from_secs(4);

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
            && self.request.file_id > 0
            && !self.request.playback_id.is_empty()
            && self.request.playback_id.len() <= 128
            && !self
                .request
                .playback_id
                .bytes()
                .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
            && self.request.start_seconds.is_finite()
            && self.request.start_seconds >= 0.0
            && self.request.start_seconds * 1_000.0 <= MAX_MEDIA_MILLIS as f64
            && self
                .request
                .audio_index
                .is_none_or(|index| (0..=1_024).contains(&index))
            && self
                .request
                .subtitle_burn
                .is_none_or(|index| (0..=1_024).contains(&index))
            && (-15_000..=15_000).contains(&self.request.audio_offset_ms)
            && match &self.request.kind {
                SessionKind::Transcode { height } => {
                    (crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT).contains(height)
                }
                SessionKind::Copy { .. } => true,
            }
            && matches!(
                (
                    &self.request.previous_session_id,
                    self.request.reopen_reason
                ),
                (None, None) | (Some(_), Some(_))
            )
            && self
                .request
                .previous_session_id
                .as_deref()
                .is_none_or(|value| uuid::Uuid::parse_str(value).is_ok())
    }
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
    fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.session_id).is_ok()
            && self.playlist_url == format!("/api/v1/hls/{}/index.m3u8", self.session_id)
            && self
                .duration_ms
                .is_none_or(|duration| (0..=MAX_MEDIA_MILLIS).contains(&duration))
            && self.start_seconds.is_finite()
            && (0.0..=MAX_MEDIA_MILLIS as f64 / 1_000.0).contains(&self.start_seconds)
            && self.media_origin_seconds.is_finite()
            && (0.0..=MAX_MEDIA_MILLIS as f64 / 1_000.0).contains(&self.media_origin_seconds)
            && (crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT)
                .contains(&self.target_height)
            && match &self.kind {
                SessionKind::Transcode { height } => *height == self.target_height,
                SessionKind::Copy { .. } => true,
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
    lease_seeds: Arc<tokio::sync::Mutex<HashMap<String, (String, i64)>>>,
}

#[derive(Clone)]
struct CachedRoute {
    route: MediaSessionRoute,
    expires_at: tokio::time::Instant,
}

impl MediaSessionCoordinator {
    pub(crate) fn new(membership: MembershipManager, store: Arc<dyn Store>) -> Arc<Self> {
        Arc::new(Self {
            transport: PeerTransport::new(membership.clone()),
            membership,
            store,
            routes: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            lease_seeds: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        })
    }

    /// Cache only durable route rows, never misses. A just-activated session
    /// must become visible immediately even if an earlier legacy request saw
    /// no row, while a one-second positive TTL removes a consensus read from
    /// playlist/segment bursts without extending the exact lease boundary.
    pub(crate) async fn route(
        &self,
        session_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError> {
        let now = tokio::time::Instant::now();
        {
            let mut routes = self.routes.lock().await;
            if let Some(cached) = routes.get(session_id) {
                if cached.expires_at > now
                    && (cached.route.state != "active"
                        || cached.route.lease_expires_at_ms > unix_ms())
                {
                    return Ok(Some(cached.route.clone()));
                }
            }
            routes.remove(session_id);
        }
        let route = self.store.media_session_route(session_id).await?;
        if let Some(route) = route.as_ref() {
            self.cache_route(route.clone()).await;
        }
        Ok(route)
    }

    pub(crate) async fn cache_route(&self, route: MediaSessionRoute) {
        let now = tokio::time::Instant::now();
        let mut routes = self.routes.lock().await;
        routes.retain(|_, cached| cached.expires_at > now);
        if routes.len() >= MAX_ROUTE_CACHE_ENTRIES && !routes.contains_key(&route.session_id) {
            if let Some(oldest) = routes
                .iter()
                .min_by_key(|(_, cached)| cached.expires_at)
                .map(|(session_id, _)| session_id.clone())
            {
                routes.remove(&oldest);
            }
        }
        routes.insert(
            route.session_id.clone(),
            CachedRoute {
                route,
                expires_at: now + ROUTE_CACHE_TTL,
            },
        );
    }

    pub(crate) async fn invalidate_route(&self, session_id: &str) {
        self.routes.lock().await.remove(session_id);
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

/// Renew only locally live workers. Lost ownership self-fences immediately;
/// a transient store outage is allowed to use the already-committed lease but
/// never past its exact expiry.
pub(crate) async fn lease_loop(state: AppState) {
    let mut interval = tokio::time::interval(LEASE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut known = HashMap::<String, (String, i64)>::new();
    loop {
        interval.tick().await;
        let now_ms = unix_ms();
        let live = state
            .transcode
            .active_session_ids()
            .await
            .into_iter()
            .collect::<HashSet<_>>();
        known.extend(state.media_sessions.take_lease_seeds().await);
        known.retain(|_, (session_id, _)| live.contains(session_id));
        if let Err(error) = state.store.maintain_media_sessions(now_ms).await {
            tracing::debug!(%error, "media-session lifecycle maintenance unavailable");
        }
        let routes = match state
            .store
            .owned_media_sessions(&state.node_id, now_ms)
            .await
        {
            Ok(routes) => routes,
            Err(error) => {
                tracing::debug!(%error, "media-session lease inventory unavailable");
                let expired = known
                    .iter()
                    .filter(|(_, (session_id, expires_at_ms))| {
                        live.contains(session_id) && *expires_at_ms <= now_ms
                    })
                    .map(|(incarnation_id, (session_id, _))| {
                        (incarnation_id.clone(), session_id.clone())
                    })
                    .collect::<Vec<_>>();
                for (incarnation_id, session_id) in expired {
                    state
                        .transcode
                        .stop_session(&session_id, "cluster lease expired")
                        .await;
                    state.media_sessions.invalidate_route(&session_id).await;
                    known.remove(&incarnation_id);
                }
                continue;
            }
        };
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
        for (incarnation_id, session_id) in lost {
            state
                .transcode
                .stop_session(&session_id, "cluster lease lost")
                .await;
            state.media_sessions.invalidate_route(&session_id).await;
            known.remove(&incarnation_id);
        }
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
                let renewals = chunk
                    .iter()
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
                        now_ms,
                        now_ms.saturating_add(LEASE_TTL_MS),
                    ),
                )
                .await;
                (chunk, outcome)
            }
        }))
        .buffer_unordered(LEASE_RENEWAL_FANOUT)
        .collect::<Vec<_>>()
        .await;
        for (chunk, outcome) in renewal_outcomes {
            match outcome {
                Ok(Ok(renewed)) => {
                    let renewed = renewed.into_iter().collect::<HashSet<_>>();
                    for route in &chunk {
                        if renewed.contains(&route.incarnation_id) {
                            known.insert(
                                route.incarnation_id.clone(),
                                (
                                    route.session_id.clone(),
                                    now_ms.saturating_add(LEASE_TTL_MS),
                                ),
                            );
                        } else {
                            state
                                .transcode
                                .stop_session(&route.session_id, "cluster lease lost")
                                .await;
                            state
                                .media_sessions
                                .invalidate_route(&route.session_id)
                                .await;
                            known.remove(&route.incarnation_id);
                        }
                    }
                }
                Ok(Err(error)) => {
                    tracing::debug!(%error, "media-session lease renewal chunk unavailable");
                    for route in &chunk {
                        if route.lease_expires_at_ms <= now_ms {
                            state
                                .transcode
                                .stop_session(&route.session_id, "cluster lease expired")
                                .await;
                            state
                                .media_sessions
                                .invalidate_route(&route.session_id)
                                .await;
                            known.remove(&route.incarnation_id);
                        }
                    }
                }
                Err(_) => {
                    tracing::debug!("media-session lease renewal fan-out exceeded its deadline");
                    for route in &chunk {
                        if route.lease_expires_at_ms <= now_ms {
                            state
                                .transcode
                                .stop_session(&route.session_id, "cluster lease expired")
                                .await;
                            state
                                .media_sessions
                                .invalidate_route(&route.session_id)
                                .await;
                            known.remove(&route.incarnation_id);
                        }
                    }
                }
            }
        }
        for route in routes
            .iter()
            .filter(|route| !live.contains(&route.session_id))
        {
            let _ = state
                .store
                .end_media_session(&route.session_id, now_ms)
                .await;
            state
                .media_sessions
                .invalidate_route(&route.session_id)
                .await;
            known.remove(&route.incarnation_id);
        }
    }
}

fn renewal_chunks<T>(items: &[T]) -> impl Iterator<Item = &[T]> {
    items.chunks(LEASE_RENEWAL_BATCH)
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

    #[test]
    fn remote_start_contract_rejects_unfenced_or_noncanonical_inputs() {
        let request = valid_start_request();
        assert!(request.is_valid());

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
