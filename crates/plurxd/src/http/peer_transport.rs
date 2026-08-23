//! Shared, bounded HTTP transport for voter-to-voter control requests.
//!
//! Peer origins come only from committed membership. Redirects are disabled,
//! every response is read under the caller's common deadline and byte budget,
//! and exact requests bind their raw body and route into the node signature.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use plurx_core::cluster::membership::{
    normalize_internal_http_base, ActivityPeerAuth, InternalPeerAuth, MembershipManager,
};

pub(crate) const NODE_HEADER: &str = "x-plurx-cluster-node";
pub(crate) const TARGET_HEADER: &str = "x-plurx-cluster-target";
pub(crate) const TIMESTAMP_HEADER: &str = "x-plurx-cluster-time-ms";
pub(crate) const SIGNATURE_HEADER: &str = "x-plurx-cluster-signature";

#[derive(Clone, Copy)]
pub(crate) enum PeerAuthMode {
    LegacyActivity,
    ExactRequest,
}

#[derive(Debug)]
pub(crate) struct PeerResponse {
    pub(crate) status: reqwest::StatusCode,
    pub(crate) body: Vec<u8>,
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
        path: &'static str,
        body: Vec<u8>,
        deadline: tokio::time::Instant,
        max_response_bytes: usize,
        auth_mode: PeerAuthMode,
    ) -> Result<PeerResponse, PeerTransportError> {
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
            PeerAuthMode::ExactRequest => self
                .membership
                .sign_internal_peer_request(
                    expected_node_id,
                    timestamp_ms,
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
        read_bounded(response, deadline, max_response_bytes).await
    }
}

enum SignedPeerAuth {
    Activity(ActivityPeerAuth),
    Exact(InternalPeerAuth),
}

trait PeerAuthHeaders {
    fn node_id(&self) -> &str;
    fn target_node_id(&self) -> &str;
    fn timestamp_ms(&self) -> i64;
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

    fn signature(&self) -> &str {
        &self.signature
    }
}

fn signed_headers<T: PeerAuthHeaders>(
    request: reqwest::RequestBuilder,
    auth: &T,
) -> reqwest::RequestBuilder {
    request
        .header(NODE_HEADER, auth.node_id())
        .header(TARGET_HEADER, auth.target_node_id())
        .header(TIMESTAMP_HEADER, auth.timestamp_ms())
        .header(SIGNATURE_HEADER, auth.signature())
}

pub(crate) fn exact_auth_from_headers(headers: &axum::http::HeaderMap) -> Option<InternalPeerAuth> {
    let node_id = headers.get(NODE_HEADER)?.to_str().ok()?;
    let target_node_id = headers.get(TARGET_HEADER)?.to_str().ok()?;
    let signature = headers.get(SIGNATURE_HEADER)?.to_str().ok()?;
    if node_id.is_empty()
        || node_id.len() > 256
        || target_node_id.is_empty()
        || target_node_id.len() > 256
        || signature.len() != 128
        || !signature.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(InternalPeerAuth {
        node_id: node_id.to_owned(),
        target_node_id: target_node_id.to_owned(),
        timestamp_ms: headers.get(TIMESTAMP_HEADER)?.to_str().ok()?.parse().ok()?,
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

async fn read_bounded(
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
    Ok(PeerResponse { status, body })
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

    #[tokio::test]
    async fn client_refuses_redirects_and_chunked_oversized_bodies() {
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
