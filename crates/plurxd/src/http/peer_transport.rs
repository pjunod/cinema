//! Shared, bounded HTTP transport for voter-to-voter control requests.
//!
//! Peer origins come only from committed membership. Redirects are disabled,
//! every response is read under the caller's common deadline and byte budget,
//! and exact requests bind their raw body and route into the node signature.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use plurx_core::cluster::membership::{
    normalize_internal_http_base, ActivityPeerAuth, InternalPeerAuth, MembershipManager,
};

pub(crate) const NODE_HEADER: &str = "x-plurx-cluster-node";
pub(crate) const TARGET_HEADER: &str = "x-plurx-cluster-target";
pub(crate) const TIMESTAMP_HEADER: &str = "x-plurx-cluster-time-ms";
pub(crate) const NONCE_HEADER: &str = "x-plurx-cluster-nonce";
pub(crate) const SIGNATURE_HEADER: &str = "x-plurx-cluster-signature";
pub(crate) const RESPONSE_SIGNATURE_HEADER: &str = "x-plurx-response-signature";

#[derive(Clone, Copy)]
pub(crate) enum PeerAuthMode {
    LegacyActivity,
    ExactRequest,
    /// A voter control response must prove its status and exact body too.
    ExactRequestAndResponse,
    /// A committed member, including a learner, must prove the response.
    ExactRequestAndMemberResponse,
}

#[derive(Debug)]
pub(crate) struct PeerResponse {
    pub(crate) status: reqwest::StatusCode,
    pub(crate) body: Vec<u8>,
    pub(crate) clock_timing: Option<(i64, i64, plurx_core::cluster::clock::ClockDecisionTicket)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeerTransportError {
    Unreachable,
    TimedOut,
    InvalidResponse,
}

#[derive(Clone)]
pub(crate) struct PeerTransport {
    membership: MembershipManager,
    client: Result<reqwest::Client, String>,
}

impl PeerTransport {
    pub(crate) fn new(membership: MembershipManager) -> Self {
        Self {
            membership,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| error.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request(
        &self,
        expected_node_id: &str,
        base: &str,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
        max_response_bytes: usize,
        auth_mode: PeerAuthMode,
    ) -> Result<PeerResponse, PeerTransportError> {
        self.request_with_optional_header(
            expected_node_id,
            base,
            method,
            path,
            body,
            deadline,
            max_response_bytes,
            auth_mode,
            None,
            None,
        )
        .await
        .map(|(response, _)| response)
    }

    /// `request`, plus the peer's `Retry-After` when it is a small
    /// delay-seconds value, so a relay can hand the owner's hint to the
    /// client instead of inventing one.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request_with_retry_after(
        &self,
        expected_node_id: &str,
        base: &str,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
        max_response_bytes: usize,
        auth_mode: PeerAuthMode,
    ) -> Result<(PeerResponse, Option<u32>), PeerTransportError> {
        self.request_with_optional_header(
            expected_node_id,
            base,
            method,
            path,
            body,
            deadline,
            max_response_bytes,
            auth_mode,
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request_with_static_header(
        &self,
        expected_node_id: &str,
        base: &str,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
        max_response_bytes: usize,
        auth_mode: PeerAuthMode,
        header: (&'static str, &'static str),
    ) -> Result<PeerResponse, PeerTransportError> {
        self.request_with_optional_header(
            expected_node_id,
            base,
            method,
            path,
            body,
            deadline,
            max_response_bytes,
            auth_mode,
            Some(header),
            None,
        )
        .await
        .map(|(response, _)| response)
    }

    pub(crate) async fn clock_request(
        &self,
        expected_node_id: &str,
        base: &str,
    ) -> Result<PeerResponse, PeerTransportError> {
        let guard = self.membership.clock_guard();
        self.request_with_optional_header(
            expected_node_id,
            base,
            reqwest::Method::GET,
            super::internal_clock::PATH,
            Vec::new(),
            deadline_after(Duration::from_secs(2)),
            1024,
            PeerAuthMode::ExactRequestAndMemberResponse,
            None,
            Some(&guard),
        )
        .await
        .map(|(response, _)| response)
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_with_optional_header(
        &self,
        expected_node_id: &str,
        base: &str,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
        max_response_bytes: usize,
        auth_mode: PeerAuthMode,
        extra_header: Option<(&'static str, &'static str)>,
        clock_guard: Option<&plurx_core::cluster::clock::ClusterClockGuard>,
    ) -> Result<(PeerResponse, Option<u32>), PeerTransportError> {
        let Some(url) = peer_url(base, path) else {
            return Err(PeerTransportError::Unreachable);
        };
        let Some(client) = self.client.as_ref().ok() else {
            return Err(PeerTransportError::Unreachable);
        };
        let clock_ticket = clock_guard.map(|guard| guard.ticket());
        let timestamp_ms = unix_ms();
        let method_name = method.as_str();
        let auth = match auth_mode {
            PeerAuthMode::LegacyActivity => self
                .membership
                .sign_activity_request(expected_node_id, timestamp_ms)
                .map(SignedPeerAuth::Activity),
            PeerAuthMode::ExactRequest
            | PeerAuthMode::ExactRequestAndResponse
            | PeerAuthMode::ExactRequestAndMemberResponse => self
                .membership
                .sign_internal_peer_request(
                    expected_node_id,
                    timestamp_ms,
                    &uuid::Uuid::new_v4().to_string(),
                    method_name,
                    path,
                    &body,
                )
                .map(SignedPeerAuth::Exact),
        }
        .map_err(|_| PeerTransportError::Unreachable)?;
        let response_binding = match &auth {
            SignedPeerAuth::Exact(auth) => match auth_mode {
                PeerAuthMode::ExactRequestAndResponse => {
                    Some((auth.node_id.clone(), auth.nonce.clone(), false))
                }
                PeerAuthMode::ExactRequestAndMemberResponse => {
                    Some((auth.node_id.clone(), auth.nonce.clone(), true))
                }
                _ => None,
            },
            _ => None,
        };
        let mut request = client.request(method, url);
        request = match auth {
            SignedPeerAuth::Activity(auth) => signed_headers(request, &auth),
            SignedPeerAuth::Exact(auth) => signed_headers(request, &auth),
        };
        if let Some((name, value)) = extra_header {
            request = request.header(name, value);
        }
        if !body.is_empty() {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        let response = tokio::time::timeout_at(deadline, request.send())
            .await
            .map_err(|_| PeerTransportError::TimedOut)?
            .map_err(|error| {
                if error.is_timeout() {
                    PeerTransportError::TimedOut
                } else {
                    PeerTransportError::Unreachable
                }
            })?;
        let signature = response
            .headers()
            .get(RESPONSE_SIGNATURE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let retry_after = retry_after_seconds(response.headers());
        read_then_verify(
            response,
            deadline,
            max_response_bytes,
            timestamp_ms,
            clock_guard.zip(clock_ticket),
            |response| async move {
                if let Some((target, nonce, member_scoped)) = response_binding {
                    let signature = signature.ok_or(PeerTransportError::InvalidResponse)?;
                    let payload = signed_response_payload(response.status.as_u16(), &response.body);
                    let verified = if member_scoped {
                        tokio::time::timeout_at(
                            deadline,
                            self.membership.authorize_internal_peer_member_response(
                                expected_node_id,
                                &target,
                                &nonce,
                                path,
                                &payload,
                                &signature,
                            ),
                        )
                        .await
                        .map_err(|_| PeerTransportError::TimedOut)?
                        .map_err(|_| PeerTransportError::InvalidResponse)?
                    } else {
                        tokio::time::timeout_at(
                            deadline,
                            self.membership.authorize_internal_peer_response(
                                expected_node_id,
                                &target,
                                &nonce,
                                path,
                                &payload,
                                &signature,
                            ),
                        )
                        .await
                        .map_err(|_| PeerTransportError::TimedOut)?
                        .map_err(|_| PeerTransportError::InvalidResponse)?
                    };
                    if !verified {
                        return Err(PeerTransportError::InvalidResponse);
                    }
                }
                Ok(response)
            },
        )
        .await
        .map(|response| (response, retry_after))
    }

    /// Send an authenticated peer request but leave the response body as a
    /// stream. Media segments can be much larger than control-plane JSON and
    /// must apply TCP backpressure instead of being accumulated at ingress.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request_stream(
        &self,
        expected_node_id: &str,
        base: &str,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
        auth_mode: PeerAuthMode,
    ) -> Result<reqwest::Response, PeerTransportError> {
        let Some(url) = peer_url(base, path) else {
            return Err(PeerTransportError::Unreachable);
        };
        let Some(client) = self.client.as_ref().ok() else {
            return Err(PeerTransportError::Unreachable);
        };
        let timestamp_ms = unix_ms();
        let method_name = method.as_str();
        let auth = match auth_mode {
            PeerAuthMode::LegacyActivity => self
                .membership
                .sign_activity_request(expected_node_id, timestamp_ms)
                .map(SignedPeerAuth::Activity),
            // A stream has no complete body to authenticate at this layer.
            PeerAuthMode::ExactRequestAndResponse | PeerAuthMode::ExactRequestAndMemberResponse => {
                return Err(PeerTransportError::InvalidResponse)
            }
            PeerAuthMode::ExactRequest => self
                .membership
                .sign_internal_peer_request(
                    expected_node_id,
                    timestamp_ms,
                    &uuid::Uuid::new_v4().to_string(),
                    method_name,
                    path,
                    &body,
                )
                .map(SignedPeerAuth::Exact),
        }
        .map_err(|_| PeerTransportError::Unreachable)?;
        let mut request = client.request(method, url);
        request = match auth {
            SignedPeerAuth::Activity(auth) => signed_headers(request, &auth),
            SignedPeerAuth::Exact(auth) => signed_headers(request, &auth),
        };
        if !body.is_empty() {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        tokio::time::timeout_at(deadline, request.send())
            .await
            .map_err(|_| PeerTransportError::TimedOut)?
            .map_err(|error| {
                if error.is_timeout() {
                    PeerTransportError::TimedOut
                } else {
                    PeerTransportError::Unreachable
                }
            })
    }
}

/// A peer's `Retry-After` in delay-seconds form, bounded to one minute. An
/// HTTP-date or anything larger is dropped rather than relayed verbatim.
pub(crate) fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u32> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|seconds| *seconds <= 60)
}

pub(crate) fn signed_response_payload(status: u16, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + body.len());
    payload.extend_from_slice(&status.to_be_bytes());
    payload.extend_from_slice(body);
    payload
}

enum SignedPeerAuth {
    Activity(ActivityPeerAuth),
    Exact(InternalPeerAuth),
}

trait PeerAuthHeaders {
    fn node_id(&self) -> &str;
    fn target_node_id(&self) -> &str;
    fn timestamp_ms(&self) -> i64;
    fn nonce(&self) -> Option<&str> {
        None
    }
    fn signature(&self) -> &str;
}

impl PeerAuthHeaders for ActivityPeerAuth {
    fn node_id(&self) -> &str {
        &self.node_id
    }

    fn target_node_id(&self) -> &str {
        &self.target_node_id
    }

    fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    fn signature(&self) -> &str {
        &self.signature
    }
}

impl PeerAuthHeaders for InternalPeerAuth {
    fn node_id(&self) -> &str {
        &self.node_id
    }

    fn target_node_id(&self) -> &str {
        &self.target_node_id
    }

    fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    fn nonce(&self) -> Option<&str> {
        Some(&self.nonce)
    }

    fn signature(&self) -> &str {
        &self.signature
    }
}

fn signed_headers<T: PeerAuthHeaders>(
    request: reqwest::RequestBuilder,
    auth: &T,
) -> reqwest::RequestBuilder {
    let request = request
        .header(NODE_HEADER, auth.node_id())
        .header(TARGET_HEADER, auth.target_node_id())
        .header(TIMESTAMP_HEADER, auth.timestamp_ms())
        .header(SIGNATURE_HEADER, auth.signature());
    match auth.nonce() {
        Some(nonce) => request.header(NONCE_HEADER, nonce),
        None => request,
    }
}

pub(crate) fn exact_auth_from_headers(headers: &axum::http::HeaderMap) -> Option<InternalPeerAuth> {
    let node_id = headers.get(NODE_HEADER)?.to_str().ok()?;
    let target_node_id = headers.get(TARGET_HEADER)?.to_str().ok()?;
    let nonce = headers.get(NONCE_HEADER)?.to_str().ok()?;
    let signature = headers.get(SIGNATURE_HEADER)?.to_str().ok()?;
    let nonce_is_canonical = nonce.len() == 36
        && uuid::Uuid::parse_str(nonce)
            .is_ok_and(|parsed| parsed.hyphenated().to_string() == nonce);
    if node_id.is_empty()
        || node_id.len() > 256
        || target_node_id.is_empty()
        || target_node_id.len() > 256
        || !nonce_is_canonical
        || signature.len() != 128
        || !signature.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(InternalPeerAuth {
        node_id: node_id.to_owned(),
        target_node_id: target_node_id.to_owned(),
        timestamp_ms: headers.get(TIMESTAMP_HEADER)?.to_str().ok()?.parse().ok()?,
        nonce: nonce.to_owned(),
        signature: signature.to_owned(),
    })
}

pub(crate) fn peer_url(base: &str, path: &str) -> Option<reqwest::Url> {
    if !path.starts_with('/') || path.contains('?') || path.contains('#') {
        return None;
    }
    let normalized = normalize_internal_http_base(base)?;
    let mut url = reqwest::Url::parse(&normalized).ok()?;
    url.set_path(path);
    Some(url)
}

async fn read_then_verify<F, Fut>(
    response: reqwest::Response,
    deadline: tokio::time::Instant,
    max_response_bytes: usize,
    timestamp_ms: i64,
    clock: Option<(
        &plurx_core::cluster::clock::ClusterClockGuard,
        plurx_core::cluster::clock::ClockDecisionTicket,
    )>,
    verify: F,
) -> Result<PeerResponse, PeerTransportError>
where
    F: FnOnce(PeerResponse) -> Fut,
    Fut: Future<Output = Result<PeerResponse, PeerTransportError>>,
{
    let mut response = read_bounded(response, deadline, max_response_bytes).await?;
    // Capture t4 before even constructing the verification future.
    let received_ms = unix_ms();
    if let Some((guard, ticket)) = clock {
        let current = guard.ticket();
        if current.clock_generation != ticket.clock_generation
            || current.state_generation != ticket.state_generation
        {
            return Err(PeerTransportError::InvalidResponse);
        }
        response.clock_timing = Some((timestamp_ms, received_ms, ticket));
    }
    tokio::time::timeout_at(deadline, verify(response))
        .await
        .map_err(|_| PeerTransportError::TimedOut)?
}

pub(crate) async fn read_bounded(
    mut response: reqwest::Response,
    deadline: tokio::time::Instant,
    max_response_bytes: usize,
) -> Result<PeerResponse, PeerTransportError> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(max_response_bytes).unwrap_or(u64::MAX))
    {
        return Err(PeerTransportError::InvalidResponse);
    }
    let status = response.status();
    let mut body = Vec::new();
    loop {
        let chunk = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .map_err(|_| PeerTransportError::TimedOut)?
            .map_err(|error| {
                if error.is_timeout() {
                    PeerTransportError::TimedOut
                } else {
                    PeerTransportError::Unreachable
                }
            })?;
        let Some(chunk) = chunk else {
            break;
        };
        if body.len().saturating_add(chunk.len()) > max_response_bytes {
            return Err(PeerTransportError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(PeerResponse {
        status,
        body,
        clock_timing: None,
    })
}

pub(crate) fn deadline_after(timeout: Duration) -> tokio::time::Instant {
    tokio::time::Instant::now() + timeout
}

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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::response::Redirect;
    use axum::routing::get;
    use axum::Router;
    use bytes::Bytes;
    use futures_util::{stream, StreamExt};

    #[test]
    fn peer_retry_after_is_bounded_delay_seconds_only() {
        let header = |value: &str| {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::RETRY_AFTER,
                value.parse().expect("header value"),
            );
            headers
        };
        assert_eq!(retry_after_seconds(&header("2")), Some(2));
        assert_eq!(retry_after_seconds(&header("61")), None);
        assert_eq!(
            retry_after_seconds(&header("Wed, 21 Oct 2015 07:28:00 GMT")),
            None
        );
        assert_eq!(
            retry_after_seconds(&reqwest::header::HeaderMap::new()),
            None
        );
    }

    #[test]
    fn exact_header_parser_requires_a_canonical_signed_nonce() {
        let mut household = axum::http::HeaderMap::new();
        household.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer household-session-token"
                .parse()
                .expect("household bearer"),
        );
        assert!(
            exact_auth_from_headers(&household).is_none(),
            "internal media routes must not widen a household bearer into peer authority"
        );

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(NODE_HEADER, "node-a".parse().expect("node"));
        headers.insert(TARGET_HEADER, "node-b".parse().expect("target"));
        headers.insert(TIMESTAMP_HEADER, "42".parse().expect("time"));
        headers.insert(
            NONCE_HEADER,
            "123e4567-e89b-42d3-a456-426614174000"
                .parse()
                .expect("nonce"),
        );
        headers.insert(
            SIGNATURE_HEADER,
            "a".repeat(128).parse().expect("signature"),
        );
        assert!(exact_auth_from_headers(&headers).is_some());

        headers.insert(NONCE_HEADER, "not-a-uuid".parse().expect("invalid nonce"));
        assert!(exact_auth_from_headers(&headers).is_none());
    }

    #[tokio::test]
    async fn clock_t4_excludes_verification_delay_and_failed_proof_discards_metadata() {
        let app = Router::new().route("/clock", get(|| async { "clock body" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("clock timing listener");
        let address = listener.local_addr().expect("clock timing address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("clock timing server");
        });
        let client = reqwest::Client::new();
        let guard = plurx_core::cluster::clock::ClusterClockGuard::new(false);
        let stamp = guard.ticket();
        let signed_t1 = unix_ms();
        let response = client
            .get(format!("http://{address}/clock"))
            .send()
            .await
            .expect("clock response");
        let response = read_then_verify(
            response,
            deadline_after(Duration::from_secs(2)),
            1024,
            signed_t1,
            Some((&guard, stamp)),
            |response| async move {
                let (_, t4, _) = response
                    .clock_timing
                    .expect("timing captured before verification");
                tokio::time::sleep(Duration::from_millis(60)).await;
                assert!(
                    unix_ms() - t4 >= 50,
                    "verification delay does not become t4"
                );
                Ok(response)
            },
        )
        .await
        .expect("verified clock response");
        let (t1, _, returned_stamp) = response.clock_timing.expect("verified timing");
        assert_eq!(t1, signed_t1);
        assert_eq!(returned_stamp, stamp);
        let response = client
            .get(format!("http://{address}/clock"))
            .send()
            .await
            .expect("bad proof response");
        assert_eq!(
            read_then_verify(
                response,
                deadline_after(Duration::from_secs(2)),
                1024,
                signed_t1,
                Some((&guard, stamp)),
                |_| async { Err(PeerTransportError::InvalidResponse) }
            )
            .await
            .expect_err("failed proof discards timing"),
            PeerTransportError::InvalidResponse
        );
        server.abort();
    }

    #[tokio::test]
    async fn client_refuses_redirects_and_both_declared_and_chunked_oversized_bodies() {
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
            .route("/declared-oversized", get(|| async { vec![b'x'; 1025] }))
            .route(
                "/oversized",
                get(|| async {
                    let chunks = stream::iter([
                        Ok::<Bytes, Infallible>(Bytes::from(vec![b'x'; 1024])),
                        Ok::<Bytes, Infallible>(Bytes::from_static(b"x")),
                    ]);
                    Body::from_stream(chunks)
                }),
            )
            .route(
                "/stalled",
                get(|| async {
                    let chunks =
                        stream::once(async { Ok::<Bytes, Infallible>(Bytes::from_static(b"x")) })
                            .chain(stream::pending::<Result<Bytes, Infallible>>());
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
        let transport = PeerTransport::new(MembershipManager::unavailable());
        let client = transport.client.as_ref().expect("peer client");
        let redirect = client
            .get(format!("http://{address}/redirect"))
            .send()
            .await
            .expect("request redirect fixture");
        assert_eq!(redirect.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);

        let declared_oversized = client
            .get(format!("http://{address}/declared-oversized"))
            .send()
            .await
            .expect("request declared oversized fixture");
        assert_eq!(declared_oversized.content_length(), Some(1025));
        assert_eq!(
            read_bounded(
                declared_oversized,
                deadline_after(Duration::from_secs(1)),
                1024,
            )
            .await
            .expect_err("declared body above the exact budget is rejected before streaming"),
            PeerTransportError::InvalidResponse
        );

        let oversized = client
            .get(format!("http://{address}/oversized"))
            .send()
            .await
            .expect("request oversized fixture");
        assert_eq!(
            read_bounded(oversized, deadline_after(Duration::from_secs(1)), 1024)
                .await
                .expect_err("chunked body above the exact budget is rejected"),
            PeerTransportError::InvalidResponse
        );

        let stalled = client
            .get(format!("http://{address}/stalled"))
            .send()
            .await
            .expect("request stalled fixture");
        assert_eq!(
            read_bounded(stalled, deadline_after(Duration::from_millis(50)), 1024,)
                .await
                .expect_err("a stalled response body must consume no time past its deadline"),
            PeerTransportError::TimedOut
        );
        server.abort();
    }
}
