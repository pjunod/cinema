//! Direct pinned HTTP/1 peer connection. No redirects, proxy, DNS, or generic URLs.
use crate::sharing::SharingManager;
use axum::{
    body::Body,
    http::{header, HeaderValue, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use plurx_core::{
    secrets::Secret,
    sharing::SourceId,
    sharing::{canonical_uuid, Endpoint, SharingIdentity},
    sharing_catalogue::{
        CataloguePeerBatch, CataloguePeerPage, MetadataBatch, MAX_CURSOR_BYTES, MAX_PAGE_SIZE,
    },
    sharing_tls::dial_numeric_peer,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::{net::SocketAddr, time::Duration};
use uuid::Uuid;

#[allow(dead_code)] // Used by the receiver cleanup owner as its HTTP routes qualify.
#[path = "sharing_playback_client.rs"]
mod playback;
pub(crate) use playback::{
    CleanupPeerConnection, SharedVodStatus, SourceEndReceipt, SourcePeerDirect, SourcePeerLineage,
    SourcePeerResource, SourcePeerSession, SourceStatusReceipt,
};

#[path = "sharing_asset_client.rs"]
mod asset;
pub(crate) use asset::PeerFileAsset;

const MANAGEMENT_RESPONSE_BYTES: usize = 128 * 1024;
const CATALOGUE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
#[derive(Clone, Copy)]
enum ResponseBudget {
    Management,
    Catalogue,
    Decision,
}
impl ResponseBudget {
    fn bytes(self) -> usize {
        match self {
            Self::Management => MANAGEMENT_RESPONSE_BYTES,
            Self::Catalogue | Self::Decision => CATALOGUE_RESPONSE_BYTES,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogueLibraries {
    pub libraries: Vec<plurx_core::store::sharing_catalogue_source::SourceLibrary>,
}
fn query_component(value: &str) -> String {
    let mut result = String::new();
    const HEX: &[u8] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            result.push('%');
            result.push(char::from(HEX[(byte >> 4) as usize]));
            result.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    result
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PeerError {
    Unavailable,
    IdentityMismatch,
    ProtocolUnsupported,
    Authentication,
    InvalidResponse,
    Rejected(StatusCode),
    /// An authenticated Start refusal, never a physical cleanup receipt.
    DolbyVisionUnsupported,
}
#[derive(Deserialize)]
pub(crate) struct Identity {
    #[serde(deserialize_with = "canonical_uuid")]
    pub server_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    pub catalogue_epoch: Uuid,
    pub name: String,
    pub protocol_min: u8,
    pub protocol_max: u8,
}
pub(crate) struct PeerArtwork {
    pub bytes: axum::body::Bytes,
    pub digest: [u8; 32],
    pub mime: &'static str,
}
pub(crate) struct PeerConnection {
    sender: SendRequest<Body>,
    connection: tokio::task::JoinHandle<()>,
    host: String,
    verified_endpoint: Option<Endpoint>,
}

// The socket future is dropped before its lifetime guard, including task
// cancellation and unwinding. A JoinHandle abort request is not the receipt.
type PeerDriverFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), hyper::Error>> + Send>>;
struct GuardedPeerDriver {
    connection: Option<PeerDriverFuture>,
    lifetime: Option<std::sync::Arc<dyn Send + Sync>>,
}
impl std::future::Future for GuardedPeerDriver {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        match self
            .connection
            .as_mut()
            .expect("owned peer connection")
            .as_mut()
            .poll(cx)
        {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(_) => {
                drop(self.connection.take());
                drop(self.lifetime.take());
                std::task::Poll::Ready(())
            }
        }
    }
}
impl Drop for GuardedPeerDriver {
    fn drop(&mut self) {
        drop(self.connection.take());
        drop(self.lifetime.take());
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        self.connection.abort();
    }
}
impl PeerConnection {
    pub async fn verified(
        manager: &SharingManager,
        endpoints: &[Endpoint],
        expected: &SharingIdentity,
    ) -> Result<(Self, Identity), PeerError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut peer = Self::connect_pinned(manager, endpoints).await?;
            let identity = peer.verify_identity(expected).await?;
            Ok((peer, identity))
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
    pub(crate) async fn verified_with_lifetime(
        manager: &SharingManager,
        endpoints: &[Endpoint],
        expected: &SharingIdentity,
        lifetime: std::sync::Arc<dyn Send + Sync>,
    ) -> Result<(Self, Identity), PeerError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut peer =
                Self::connect_pinned_with_lifetime(manager, endpoints, Some(lifetime)).await?;
            let identity = peer.verify_identity(expected).await?;
            Ok((peer, identity))
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
    // The cleanup-only wrapper may dial the retained approved pin while
    // identity reads are disabled. Only its authenticated exact End echo can
    // produce a cleanup receipt; this helper confers no content authority.
    async fn connect_pinned(
        manager: &SharingManager,
        endpoints: &[Endpoint],
    ) -> Result<Self, PeerError> {
        Self::connect_pinned_with_lifetime(manager, endpoints, None).await
    }
    async fn connect_pinned_with_lifetime(
        manager: &SharingManager,
        endpoints: &[Endpoint],
        lifetime: Option<std::sync::Arc<dyn Send + Sync>>,
    ) -> Result<Self, PeerError> {
        use futures_util::{stream::FuturesUnordered, StreamExt};
        plurx_core::sharing::validate_endpoints(endpoints)
            .map_err(|_| PeerError::InvalidResponse)?;
        // Four approved endpoints and at most four numeric targets each. Racing this
        // fixed set lets one offline hint coexist with a reachable replica.
        // Losing dials are dropped as soon as a pinned stream wins.
        tokio::time::timeout(Duration::from_secs(5), async {
            let egress = manager.egress();
            use futures_util::FutureExt;
            let mut attempts = FuturesUnordered::new();
            for endpoint in endpoints {
                let egress = &egress;
                // Validated numeric hints are immediately useful. A stalled
                // Quad100 lookup must not consume a control RPC's full budget
                // before the first already-approved pinned dial begins.
                let mut hints = vec![SocketAddr::new(endpoint.ipv4.into(), endpoint.port)];
                if let Some(ip) = endpoint.ipv6 {
                    hints.push(SocketAddr::new(ip.into(), endpoint.port));
                }
                for address in hints.clone() {
                    attempts.push(
                        async move {
                            dial_numeric_peer(
                                address,
                                &endpoint.ts_fqdn,
                                &endpoint.spki_sha256,
                                egress,
                            )
                            .await
                            .map(|stream| {
                                (
                                    stream,
                                    format!("{}:{}", endpoint.ts_fqdn, endpoint.port),
                                    endpoint.clone(),
                                )
                            })
                        }
                        .boxed(),
                    );
                }
                attempts.push(
                    async move {
                        let addresses =
                            plurx_core::sharing_dns::resolve_tailnet(&endpoint.ts_fqdn, egress)
                                .await
                                .unwrap_or_default();
                        let mut resolved = FuturesUnordered::new();
                        let mut considered = hints;
                        for ip in addresses {
                            let address = SocketAddr::new(ip, endpoint.port);
                            if considered.contains(&address) {
                                continue;
                            }
                            if considered.len() == 4 {
                                break;
                            }
                            considered.push(address);
                            resolved.push(async move {
                                dial_numeric_peer(
                                    address,
                                    &endpoint.ts_fqdn,
                                    &endpoint.spki_sha256,
                                    egress,
                                )
                                .await
                                .map(|stream| {
                                    (
                                        stream,
                                        format!("{}:{}", endpoint.ts_fqdn, endpoint.port),
                                        endpoint.clone(),
                                    )
                                })
                            });
                        }
                        while let Some(result) = resolved.next().await {
                            if result.is_ok() {
                                return result;
                            }
                        }
                        Err(plurx_core::error::StoreError::Identity(
                            "sharing peer unavailable".into(),
                        ))
                    }
                    .boxed(),
                );
            }
            while let Some(result) = attempts.next().await {
                if let Ok((stream, host, endpoint)) = result {
                    drop(attempts);
                    let mut peer = Self::from_stream_with_lifetime(stream, host, lifetime).await?;
                    peer.verified_endpoint = Some(endpoint);
                    return Ok(peer);
                }
            }
            Err(PeerError::Unavailable)
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
    #[cfg(test)]
    async fn from_stream<
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    >(
        stream: S,
        host: String,
    ) -> Result<Self, PeerError> {
        Self::from_stream_with_lifetime(stream, host, None).await
    }
    async fn from_stream_with_lifetime<
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    >(
        stream: S,
        host: String,
        lifetime: Option<std::sync::Arc<dyn Send + Sync>>,
    ) -> Result<Self, PeerError> {
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|_| PeerError::Unavailable)?;
        Ok(Self {
            sender,
            connection: tokio::spawn(GuardedPeerDriver {
                connection: Some(Box::pin(connection)),
                lifetime,
            }),
            host,
            verified_endpoint: None,
        })
    }
    pub(crate) fn verified_endpoint(&self) -> Option<&Endpoint> {
        self.verified_endpoint.as_ref()
    }
    async fn verify_identity(&mut self, expected: &SharingIdentity) -> Result<Identity, PeerError> {
        let identity: Identity = self.identity().await?;
        if identity.server_id != expected.server_id
            || identity.catalogue_epoch != expected.catalogue_epoch
        {
            return Err(PeerError::IdentityMismatch);
        }
        if identity.protocol_min > 1
            || identity.protocol_max < 1
            || identity.protocol_min > identity.protocol_max
        {
            return Err(PeerError::ProtocolUnsupported);
        }
        if identity.name.len() > 128 || identity.name.chars().any(char::is_control) {
            return Err(PeerError::InvalidResponse);
        }
        Ok(identity)
    }
    async fn request<T: DeserializeOwned>(
        &mut self,
        method: Method,
        path: &str,
        credential: Option<&Secret>,
        payload: Option<&Secret>,
    ) -> Result<T, PeerError> {
        self.request_with_budget(
            method,
            path,
            credential,
            payload,
            ResponseBudget::Management,
        )
        .await
    }
    async fn request_with_budget<T: DeserializeOwned>(
        &mut self,
        method: Method,
        path: &str,
        credential: Option<&Secret>,
        payload: Option<&Secret>,
        budget: ResponseBudget,
    ) -> Result<T, PeerError> {
        let deadline = if matches!(budget, ResponseBudget::Decision) {
            Duration::from_secs(12)
        } else {
            Duration::from_secs(5)
        };
        tokio::time::timeout(deadline, async {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, &self.host)
                .header(header::ACCEPT, "application/json");
            if let Some(secret) = credential {
                let mut value = HeaderValue::from_str(&format!("CinemaShare {}", secret.expose()))
                    .map_err(|_| PeerError::InvalidResponse)?;
                value.set_sensitive(true);
                request = request.header(header::AUTHORIZATION, value);
            }
            if payload.is_some() {
                request = request.header(header::CONTENT_TYPE, "application/json");
            }
            let request = request
                .body(
                    payload
                        .map(|secret| Body::from(secret.expose().to_owned()))
                        .unwrap_or_else(Body::empty),
                )
                .map_err(|_| PeerError::InvalidResponse)?;
            // Consuming a response body does not itself make Hyper's HTTP/1
            // dispatch channel ready for the next request. Wait within the
            // same deadline before reusing the pinned connection.
            self.sender
                .ready()
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let response = self
                .sender
                .send_request(request)
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let status = response.status();
            // Limit the stream even when Content-Length is absent or false.
            let bytes = axum::body::to_bytes(
                Body::new(
                    response
                        .into_body()
                        .map_err(|_| std::io::Error::other("sharing peer body")),
                ),
                if status.is_success() {
                    budget.bytes()
                } else {
                    MANAGEMENT_RESPONSE_BYTES
                },
            )
            .await
            .map_err(|_| PeerError::InvalidResponse)?;
            // Consume bounded error bodies so rotation status can be followed
            // by its mutation on the same connection. Never expose their text.
            if status == StatusCode::UNAUTHORIZED {
                return Err(PeerError::Authentication);
            }
            if status == StatusCode::UPGRADE_REQUIRED {
                return Err(PeerError::ProtocolUnsupported);
            }
            if !status.is_success() {
                return Err(PeerError::Rejected(status));
            }
            serde_json::from_slice(&bytes).map_err(|_| PeerError::InvalidResponse)
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
    /// Only this closed binary operation has the artwork response budget.
    /// Caller owns the byte reservation before any frame is collected.
    pub async fn artwork(
        &mut self,
        credential: &Secret,
        resource: &plurx_core::sharing_artwork::SourceArtResource,
    ) -> Result<PeerArtwork, PeerError> {
        use sha2::{Digest, Sha256};
        tokio::time::timeout(Duration::from_secs(5), async {
            let expected = resource
                .reference_unverified()
                .map_err(|_| PeerError::InvalidResponse)?;
            let mut auth = HeaderValue::from_str(&format!("CinemaShare {}", credential.expose()))
                .map_err(|_| PeerError::InvalidResponse)?;
            auth.set_sensitive(true);
            let request = Request::builder()
                .method(Method::GET)
                .uri(format!("/sharing/v1/art/{}", resource.as_str()))
                .header(header::HOST, &self.host)
                .header(header::AUTHORIZATION, auth)
                .header(header::ACCEPT_ENCODING, "identity")
                .body(Body::empty())
                .map_err(|_| PeerError::InvalidResponse)?;
            self.sender
                .ready()
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let response = self
                .sender
                .send_request(request)
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let status = response.status();
            if status != StatusCode::OK {
                axum::body::to_bytes(Body::new(response.into_body()), MANAGEMENT_RESPONSE_BYTES)
                    .await
                    .map_err(|_| PeerError::InvalidResponse)?;
                return Err(if status == StatusCode::UNAUTHORIZED {
                    PeerError::Authentication
                } else {
                    PeerError::Rejected(status)
                });
            }
            let headers = response.headers();
            for name in [
                "x-plurx-art-bytes",
                "content-length",
                "content-encoding",
                "x-plurx-art-variant",
                "x-plurx-art-sha256",
                "content-type",
            ] {
                if headers.get_all(name).iter().count() != 1 {
                    return Err(PeerError::InvalidResponse);
                }
            }
            let text = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(PeerError::InvalidResponse)
            };
            let len_text = text("x-plurx-art-bytes")?;
            let len = len_text
                .parse::<usize>()
                .map_err(|_| PeerError::InvalidResponse)?;
            if len == 0
                || len > plurx_core::sharing_artwork::MAX_ART_BYTES
                || len.to_string() != len_text
                || text("content-length")? != len_text
                || text("content-encoding")? != "identity"
                || text("x-plurx-art-variant")? != expected.variant.label()
            {
                return Err(PeerError::InvalidResponse);
            }
            let digest_text = text("x-plurx-art-sha256")?;
            let digest: [u8; 32] = hex::decode(digest_text)
                .map_err(|_| PeerError::InvalidResponse)?
                .try_into()
                .map_err(|_| PeerError::InvalidResponse)?;
            if hex::encode(digest) != digest_text {
                return Err(PeerError::InvalidResponse);
            }
            let mime = match text("content-type")? {
                "image/jpeg" => "image/jpeg",
                "image/png" => "image/png",
                "image/gif" => "image/gif",
                "image/webp" => "image/webp",
                _ => return Err(PeerError::InvalidResponse),
            };
            let mut body = response.into_body();
            let mut bytes = Vec::with_capacity(len);
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|_| PeerError::Unavailable)?;
                if let Ok(data) = frame.into_data() {
                    if data.len() > len.saturating_sub(bytes.len()) {
                        return Err(PeerError::InvalidResponse);
                    }
                    bytes.extend_from_slice(&data);
                } else {
                    return Err(PeerError::InvalidResponse);
                }
            }
            if bytes.len() != len || <[u8; 32]>::from(Sha256::digest(&bytes)) != digest {
                return Err(PeerError::InvalidResponse);
            }
            Ok(PeerArtwork {
                bytes: axum::body::Bytes::from(bytes),
                digest,
                mime,
            })
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
    /// One pinned H1 exchange, without redirect, retry or reply normalization.
    /// Dropping this future cancels waiting; it does not assert Source rollback.
    #[allow(dead_code)] // Candidate transport remains unwired until Source/B authority integration.
    pub async fn file_start(
        &mut self,
        credential: &Secret,
        expected: &crate::http::hls::SourcePlaybackTarget,
        viewer_hash: &str,
        request_json: &str,
    ) -> Result<crate::http::sharing_direct_wire::DecodedSourceStart, PeerError> {
        crate::http::validate_source_start_request(request_json.as_bytes(), expected)
            .map_err(|_| PeerError::InvalidResponse)?;
        // B decodes only the presentation its own canonical request names.
        let direct = serde_json::from_str::<serde_json::Value>(request_json)
            .ok()
            .and_then(|value| value.get("session").cloned())
            .is_some_and(|session| {
                crate::http::sharing_direct_wire::session_presentation_is_direct(&session)
            });
        if viewer_hash.len() != 64
            || !viewer_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PeerError::InvalidResponse);
        }
        tokio::time::timeout(Duration::from_secs(310), async {
            let mut authorization =
                HeaderValue::from_str(&format!("CinemaShare {}", credential.expose()))
                    .map_err(|_| PeerError::InvalidResponse)?;
            authorization.set_sensitive(true);
            let mut viewer =
                HeaderValue::from_str(viewer_hash).map_err(|_| PeerError::InvalidResponse)?;
            viewer.set_sensitive(true);
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/sharing/v1/items/{}/files/{}/sessions",
                    expected.item_id.as_str(),
                    expected.file_id.as_str()
                ))
                .header(header::HOST, &self.host)
                .header(header::ACCEPT, "application/json")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, authorization)
                .header("cinemashare-viewer", viewer)
                .body(Body::from(request_json.to_owned()))
                .map_err(|_| PeerError::InvalidResponse)?;
            self.sender
                .ready()
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let response = self
                .sender
                .send_request(request)
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let status = response.status();
            let bytes = axum::body::to_bytes(
                Body::new(
                    response
                        .into_body()
                        .map_err(|_| std::io::Error::other("sharing peer body")),
                ),
                if status.is_success() {
                    CATALOGUE_RESPONSE_BYTES
                } else {
                    MANAGEMENT_RESPONSE_BYTES
                },
            )
            .await
            .map_err(|_| PeerError::InvalidResponse)?;
            if status == StatusCode::UNAUTHORIZED {
                return Err(PeerError::Authentication);
            }
            if status == StatusCode::UPGRADE_REQUIRED {
                return Err(PeerError::ProtocolUnsupported);
            }
            if !status.is_success() {
                return Err(source_start_refusal(status, &bytes));
            }
            crate::http::sharing_direct_wire::decode_source_start(&bytes, expected, direct)
                .map_err(|_| PeerError::InvalidResponse)
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }

    pub async fn file_decision(
        &mut self,
        credential: &Secret,
        input: &crate::http::shared_playback::SourceDecisionRequest,
    ) -> Result<crate::http::shared_playback::SourceDecisionReply, PeerError> {
        let bytes = serde_json::to_string(input).map_err(|_| PeerError::InvalidResponse)?;
        if bytes.len() > 128 * 1024 {
            return Err(PeerError::InvalidResponse);
        }
        let payload = Secret::from_cleartext(bytes);
        let reply: crate::http::shared_playback::SourceDecisionReply = self
            .request_with_budget(
                Method::POST,
                &format!(
                    "/sharing/v1/items/{}/files/{}/decision",
                    input.reference.item_id.as_str(),
                    input.reference.file_id.as_str()
                ),
                Some(credential),
                Some(&payload),
                ResponseBudget::Decision,
            )
            .await?;
        if reply.protocol != 1 || reply.reference != input.reference {
            return Err(PeerError::InvalidResponse);
        }
        Ok(reply)
    }

    pub async fn current_scope(
        &mut self,
        credential: &Secret,
        request: &plurx_core::sharing_catalogue_details::SourceScopeRequest,
    ) -> Result<bool, PeerError> {
        request.validate().map_err(|_| PeerError::InvalidResponse)?;
        let payload = Secret::from_cleartext(
            serde_json::to_string(request).map_err(|_| PeerError::InvalidResponse)?,
        );
        let response: plurx_core::sharing_catalogue_details::SourceScopeResponse = self
            .request(
                Method::POST,
                "/sharing/v1/current-scope",
                Some(credential),
                Some(&payload),
            )
            .await?;
        Ok(response.authorized)
    }
    pub async fn catalogue_libraries(
        &mut self,
        credential: &Secret,
    ) -> Result<CatalogueLibraries, PeerError> {
        let response: CatalogueLibraries = self
            .request_with_budget(
                Method::GET,
                "/sharing/v1/libraries",
                Some(credential),
                None,
                ResponseBudget::Catalogue,
            )
            .await?;
        if response.libraries.len() > plurx_core::sharing::MAX_LIBRARIES
            || response
                .libraries
                .iter()
                .any(|l| l.name.len() > 256 || !matches!(l.kind.as_str(), "movies" | "shows"))
        {
            return Err(PeerError::InvalidResponse);
        }
        Ok(response)
    }
    pub async fn catalogue_page(
        &mut self,
        credential: &Secret,
        library: &SourceId,
        parent: Option<&SourceId>,
        q: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<CataloguePeerPage, PeerError> {
        if q.len() > 512
            || q.chars().any(char::is_control)
            || cursor.is_some_and(|s| s.len() > MAX_CURSOR_BYTES)
            || !(1..=MAX_PAGE_SIZE).contains(&limit)
        {
            return Err(PeerError::InvalidResponse);
        }
        let mut path = match parent {
            None => format!("/sharing/v1/libraries/{}/items", library.as_str()),
            Some(parent) => format!("/sharing/v1/items/{}/children", parent.as_str()),
        };
        path.push_str(&format!("?limit={limit}&q={}", query_component(q)));
        if let Some(cursor) = cursor {
            path.push_str("&cursor=");
            path.push_str(&query_component(cursor));
        }
        let response: CataloguePeerPage = self
            .request_with_budget(
                Method::GET,
                &path,
                Some(credential),
                None,
                ResponseBudget::Catalogue,
            )
            .await?;
        response
            .validate()
            .map_err(|_| PeerError::InvalidResponse)?;
        if response.items.len() > limit
            || response
                .items
                .iter()
                .any(|item| &item.library_id != library || item.parent_id.as_ref() != parent)
        {
            return Err(PeerError::InvalidResponse);
        }
        Ok(response)
    }
    pub async fn catalogue_item(
        &mut self,
        credential: &Secret,
        item: &SourceId,
    ) -> Result<plurx_core::sharing_catalogue_details::SourceItemDetails, PeerError> {
        let response: plurx_core::sharing_catalogue_details::SourceItemDetails = self
            .request_with_budget(
                Method::GET,
                &format!("/sharing/v1/items/{}", item.as_str()),
                Some(credential),
                None,
                ResponseBudget::Catalogue,
            )
            .await?;
        response
            .validate()
            .map_err(|_| PeerError::InvalidResponse)?;
        if &response.item.item_id != item {
            return Err(PeerError::InvalidResponse);
        }
        Ok(response)
    }
    pub async fn catalogue_batch(
        &mut self,
        credential: &Secret,
        batch: &MetadataBatch,
    ) -> Result<CataloguePeerBatch, PeerError> {
        batch.validate().map_err(|_| PeerError::InvalidResponse)?;
        let payload = Secret::from_cleartext(
            serde_json::to_string(batch).map_err(|_| PeerError::InvalidResponse)?,
        );
        let response: CataloguePeerBatch = self
            .request_with_budget(
                Method::POST,
                "/sharing/v1/items:batch",
                Some(credential),
                Some(&payload),
                ResponseBudget::Catalogue,
            )
            .await?;
        response
            .validate(batch)
            .map_err(|_| PeerError::InvalidResponse)?;
        Ok(response)
    }
    async fn identity(&mut self) -> Result<Identity, PeerError> {
        self.request(Method::GET, "/sharing/v1/identity", None, None)
            .await
    }
    pub async fn claim<T: DeserializeOwned>(&mut self, payload: &Secret) -> Result<T, PeerError> {
        self.request(Method::POST, "/sharing/v1/claims", None, Some(payload))
            .await
    }
    pub async fn grant<T: DeserializeOwned>(
        &mut self,
        credential: &Secret,
    ) -> Result<T, PeerError> {
        self.request(Method::GET, "/sharing/v1/grant", Some(credential), None)
            .await
    }
    pub async fn endpoints<T: DeserializeOwned>(
        &mut self,
        credential: &Secret,
    ) -> Result<T, PeerError> {
        self.request(Method::GET, "/sharing/v1/endpoints", Some(credential), None)
            .await
    }
    pub async fn rotate(
        &mut self,
        credential: &Secret,
        payload: &Secret,
    ) -> Result<serde_json::Value, PeerError> {
        self.request(
            Method::POST,
            "/sharing/v1/grant/rotation",
            Some(credential),
            Some(payload),
        )
        .await
    }
    pub async fn rotation_status(
        &mut self,
        credential: &Secret,
        request: Uuid,
        grant: Uuid,
    ) -> Result<serde_json::Value, PeerError> {
        self.request(
            Method::GET,
            &format!("/sharing/v1/grant/rotation/{request}?grant_id={grant}"),
            Some(credential),
            None,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Response;
    use bytes::Bytes;
    use http_body_util::Full;
    use std::sync::{Arc, Mutex};
    type Requests = Arc<Mutex<Vec<(String, bool, usize)>>>;
    async fn fixture(
        status: StatusCode,
        payload: serde_json::Value,
    ) -> (PeerConnection, Requests, tokio::task::JoinHandle<()>) {
        let (client, server) = tokio::io::duplex(32768);
        let requests: Requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let server = tokio::spawn(async move {
            let service =
                hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                    let log = log.clone();
                    let payload = payload.clone();
                    async move {
                        let path = request.uri().to_string();
                        let auth = request.headers().contains_key(header::AUTHORIZATION);
                        let body = request
                            .into_body()
                            .collect()
                            .await
                            .expect("fixture body")
                            .to_bytes();
                        log.lock()
                            .expect("fixture log")
                            .push((path, auth, body.len()));
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .status(status)
                                .header(
                                    header::LOCATION,
                                    "https://outside.example.invalid/credential-sink",
                                )
                                .body(Full::new(Bytes::from(
                                    serde_json::to_vec(&payload).expect("fixture response"),
                                )))
                                .expect("fixture response"),
                        )
                    }
                });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(server), service)
                .await;
        });
        (
            PeerConnection::from_stream(client, "source.fixture.ts.net:32443".into())
                .await
                .expect("fixture connection"),
            requests,
            server,
        )
    }
    #[tokio::test]
    async fn sharing_decision_client_binds_complete_reference_and_refuses_redirect_or_body_amplification(
    ) {
        use crate::http::{hls::SourcePlaybackTarget, shared_playback::SourceDecisionRequest};
        use plurx_core::sharing_catalogue_details::FileRevision;
        let input = SourceDecisionRequest {
            reference: SourcePlaybackTarget {
                server_id: Uuid::new_v4(), catalogue_epoch: Uuid::new_v4(),
                library_id: SourceId::parse("9007199254740993").expect("library"),
                item_id: SourceId::parse("9223372036854775807").expect("item"),
                file_id: SourceId::parse("0").expect("file"),
                revision: FileRevision::parse(&"d".repeat(64)).expect("revision"),
            },
            caps: serde_json::from_value(serde_json::json!({"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]})).expect("actual caps"),
            audio: Some(2), subtitle: Some(3), audio_offset_ms: Some(-100), force: None,
        };
        let credential = plurx_core::sharing::new_secret().expect("credential");
        for case in 0..10 {
            let mut payload =
                serde_json::json!({"protocol":1,"reference":input.reference,"decision":{}});
            match case {
                1 => payload["protocol"] = serde_json::json!(2),
                2 => payload["reference"]["server_id"] = serde_json::json!(Uuid::new_v4()),
                3 => payload["reference"]["catalogue_epoch"] = serde_json::json!(Uuid::new_v4()),
                4 => payload["reference"]["library_id"] = serde_json::json!("1"),
                5 => payload["reference"]["item_id"] = serde_json::json!("1"),
                6 => payload["reference"]["file_id"] = serde_json::json!("1"),
                7 => payload["reference"]["revision"] = serde_json::json!("e".repeat(64)),
                8 => (),
                9 => {
                    payload["decision"] = serde_json::json!(["x".repeat(CATALOGUE_RESPONSE_BYTES)])
                }
                _ => (),
            }
            let status = if case == 8 {
                StatusCode::TEMPORARY_REDIRECT
            } else {
                StatusCode::OK
            };
            let (mut peer, requests, server) = fixture(status, payload).await;
            assert_eq!(
                peer.file_decision(&credential, &input).await.is_ok(),
                case == 0,
                "closed envelope {case}"
            );
            {
                let requests = requests.lock().expect("requests");
                assert_eq!(requests.len(), 1, "no redirect or replacement request");
                assert_eq!(
                    requests[0].0,
                    "/sharing/v1/items/9223372036854775807/files/0/decision"
                );
                assert!(requests[0].1);
                assert_eq!(
                    requests[0].2,
                    serde_json::to_vec(&input).expect("body").len()
                );
            }
            drop(peer);
            server.abort();
            let _ = server.await;
        }
    }
    #[tokio::test]
    async fn sharing_art_client_rejects_redirects_bad_digests_encodings_and_size_claims() {
        use plurx_core::{
            secrets::CredentialKey,
            sharing_artwork::{artwork_expiry, ArtKind, ArtVariant, SourceArtReference},
            sharing_catalogue_details::CatalogueRevisionKey,
        };
        use sha2::{Digest, Sha256};
        let identity = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1,
        };
        let key = CredentialKey::from_bytes([1; 32]);
        let envelope =
            CatalogueRevisionKey::generate_sealed(&key, identity.clone()).expect("fixture key");
        let key = CatalogueRevisionKey::open(&key, identity.clone(), &envelope).expect("open");
        let now = crate::state::clock_ms();
        let resource = key
            .issue_art(
                &SourceArtReference {
                    server_id: identity.server_id,
                    catalogue_epoch: identity.catalogue_epoch,
                    grant_id: Uuid::new_v4(),
                    library_id: SourceId::parse("1").expect("lib"),
                    item_id: SourceId::parse("1").expect("item"),
                    kind: ArtKind::Poster,
                    variant: ArtVariant::Original,
                    expires_at_ms: artwork_expiry(now).expect("expiry"),
                },
                now,
            )
            .expect("resource");
        for case in 0..8 {
            let (client, server) = tokio::io::duplex(32768);
            let server = tokio::spawn(async move {
                let service = hyper::service::service_fn(
                    move |request: Request<hyper::body::Incoming>| async move {
                        assert!(request.uri().path().starts_with("/sharing/v1/art/"));
                        assert!(request.headers().contains_key(header::AUTHORIZATION));
                        let bytes = Bytes::from_static(b"\x89PNG\r\n\x1a\ncontent");
                        let len = if case == 3 {
                            plurx_core::sharing_artwork::MAX_ART_BYTES + 1
                        } else {
                            bytes.len()
                        };
                        let response = Response::builder()
                            .status(if case == 1 {
                                StatusCode::TEMPORARY_REDIRECT
                            } else {
                                StatusCode::OK
                            })
                            .header(header::LOCATION, "https://outside.invalid/credential-sink")
                            .header(
                                "x-plurx-art-bytes",
                                if case == 7 {
                                    format!("0{len}")
                                } else {
                                    len.to_string()
                                },
                            )
                            .header(header::CONTENT_LENGTH, bytes.len().to_string())
                            .header(
                                header::CONTENT_ENCODING,
                                if case == 4 { "gzip" } else { "identity" },
                            )
                            .header(
                                header::CONTENT_TYPE,
                                if case == 5 {
                                    "image/svg+xml"
                                } else {
                                    "image/png"
                                },
                            )
                            .header(
                                "x-plurx-art-variant",
                                if case == 6 { "w300" } else { "original" },
                            )
                            .header(
                                "x-plurx-art-sha256",
                                if case == 2 {
                                    "a".repeat(64)
                                } else {
                                    hex::encode(Sha256::digest(&bytes))
                                },
                            )
                            .body(Full::new(bytes))
                            .expect("response");
                        Ok::<_, std::convert::Infallible>(response)
                    },
                );
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(server), service)
                    .await;
            });
            let mut peer = PeerConnection::from_stream(client, "fixture.ts.net:32443".into())
                .await
                .expect("peer");
            let result = peer
                .artwork(
                    &plurx_core::sharing::new_secret().expect("credential"),
                    &resource,
                )
                .await;
            assert_eq!(result.is_ok(), case == 0, "closed binary case {case}");
            drop(peer);
            server.abort();
            let _ = server.await;
        }
    }
    #[tokio::test]
    async fn sharing_current_scope_client_uses_closed_control_response() {
        use plurx_core::sharing_catalogue_details::SourceScopeRequest;
        let credential = plurx_core::sharing::new_secret().expect("synthetic credential");
        let scope = SourceScopeRequest {
            server_id: uuid::Uuid::new_v4(),
            catalogue_epoch: uuid::Uuid::new_v4(),
            grant_id: uuid::Uuid::new_v4(),
            recipient_server_id: uuid::Uuid::new_v4(),
            libraries: Vec::new(),
            items: Vec::new(),
            files: Vec::new(),
        };
        for (payload, valid) in [
            (serde_json::json!({"authorized":true}), true),
            (serde_json::json!({"authorized":false}), true),
            (serde_json::json!({"authorized":1}), false),
            (
                serde_json::json!({"authorized":true,"path":"/private/injected"}),
                false,
            ),
        ] {
            let (mut peer, requests, server) = fixture(StatusCode::OK, payload).await;
            assert_eq!(peer.current_scope(&credential, &scope).await.is_ok(), valid);
            {
                let requests = requests.lock().expect("request log");
                assert_eq!(requests[0].0, "/sharing/v1/current-scope");
                assert!(requests[0].1);
                assert!(requests[0].2 < 32 * 1024);
            }
            drop(peer);
            server.abort();
            let _ = server.await;
        }
    }
    fn catalogue_item(id: &str) -> serde_json::Value {
        serde_json::json!({"item_id":id,"library_id":"12","parent_id":null,"kind":"movie","title":"Fixture","sort_title":"Fixture","year":null,"overview":"x".repeat(8192),"genres":[],"season_number":null,"episode_number":null})
    }
    #[tokio::test]
    async fn sharing_catalogue_client_bounds_closed_records_and_encodes_queries() {
        let credential = plurx_core::sharing::new_secret().expect("synthetic catalogue fixture");
        for (payload, valid) in [
            (
                serde_json::json!({"item":catalogue_item("9007199254740993"),"files":[]}),
                true,
            ),
            (
                serde_json::json!({"item":catalogue_item("2"),"files":[]}),
                false,
            ),
            (
                serde_json::json!({"item":catalogue_item("9007199254740993"),"files":[],"path":"/private/injected"}),
                false,
            ),
        ] {
            let (mut peer, requests, server) = fixture(StatusCode::OK, payload).await;
            assert_eq!(
                peer.catalogue_item(
                    &credential,
                    &SourceId::parse("9007199254740993").expect("large canonical item")
                )
                .await
                .is_ok(),
                valid
            );
            assert_eq!(
                requests.lock().expect("requests")[0].0,
                "/sharing/v1/items/9007199254740993"
            );
            drop(peer);
            server.abort();
            let _ = server.await;
        }
        for count in [64, 65] {
            let payload = serde_json::json!({"libraries":(1..=count).map(|id|serde_json::json!({"library_id":id.to_string(),"name":"Fixture","kind":"movies","anime":false})).collect::<Vec<_>>()});
            let (mut peer, _, server) = fixture(StatusCode::OK, payload).await;
            assert_eq!(
                peer.catalogue_libraries(&credential).await.is_ok(),
                count == 64
            );
            drop(peer);
            server.abort();
            let _ = server.await;
        }

        let payload = serde_json::json!({"items":(1..=25).map(|id|catalogue_item(&id.to_string())).collect::<Vec<_>>(),"next_cursor":null,"catalogue_revision":1,"scope_generation":1,"catalogue_generation":1});
        let (mut peer, requests, server) = fixture(StatusCode::OK, payload.clone()).await;
        let page = peer
            .catalogue_page(
                &credential,
                &SourceId::parse("12").expect("synthetic catalogue fixture"),
                None,
                "A &雪/?",
                Some("x+=/"),
                60,
            )
            .await
            .expect("synthetic catalogue fixture");
        assert_eq!(page.items.len(), 25);
        assert_eq!(
            requests.lock().expect("synthetic catalogue fixture")[0].0,
            "/sharing/v1/libraries/12/items?limit=60&q=A%20%26%E9%9B%AA%2F%3F&cursor=x%2B%3D%2F"
        );
        assert!(matches!(
            peer.request::<serde_json::Value>(
                Method::GET,
                "/sharing/v1/grant",
                Some(&credential),
                None
            )
            .await,
            Err(PeerError::InvalidResponse)
        ));
        drop(peer);
        server.abort();
        let _ = server.await;
        for payload in [
            serde_json::json!({"items":[{"item_id":"2","item":null}]}),
            serde_json::json!({"items":[{"item_id":"1","item":catalogue_item("2")}]}),
            serde_json::json!({"items":[{"item_id":"1","item":null}],"path":"private"}),
            serde_json::json!({"items":[{"item_id":"01","item":null}]}),
            serde_json::json!({"items":[{"item_id":"1","item":null}],"padding":"x".repeat(CATALOGUE_RESPONSE_BYTES)}),
        ] {
            let (mut peer, _, server) = fixture(StatusCode::OK, payload).await;
            let batch = MetadataBatch {
                item_ids: vec![SourceId::parse("1").expect("synthetic catalogue fixture")],
            };
            assert!(matches!(
                peer.catalogue_batch(&credential, &batch).await,
                Err(PeerError::InvalidResponse)
            ));
            drop(peer);
            server.abort();
            let _ = server.await;
        }
    }
    #[tokio::test]
    async fn sharing_peer_identity_mismatch_and_redirect_never_send_capabilities() {
        let expected = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 0,
        };
        for (server_id, epoch) in [
            (Uuid::new_v4(), expected.catalogue_epoch),
            (expected.server_id, Uuid::new_v4()),
        ] {
            let (mut peer,requests,server)=fixture(StatusCode::OK,serde_json::json!({"server_id":server_id,"catalogue_epoch":epoch,"name":"Fixture","protocol_min":1,"protocol_max":1})).await;
            assert!(matches!(
                peer.verify_identity(&expected).await,
                Err(PeerError::IdentityMismatch)
            ));
            assert_eq!(
                *requests.lock().expect("requests"),
                vec![("/sharing/v1/identity".into(), false, 0)]
            );
            drop(peer);
            server.abort();
            let _ = server.await;
        }
        let (mut peer, requests, server) = fixture(
            StatusCode::FOUND,
            serde_json::json!({"message":"do not follow"}),
        )
        .await;
        assert!(matches!(
            peer.verify_identity(&expected).await,
            Err(PeerError::Rejected(StatusCode::FOUND))
        ));
        assert_eq!(
            *requests.lock().expect("requests"),
            vec![("/sharing/v1/identity".into(), false, 0)]
        );
        drop(peer);
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn sharing_peer_bounded_error_body_allows_rotation_status_recovery() {
        let (mut peer, requests, server) = fixture(
            StatusCode::NOT_FOUND,
            serde_json::json!({"message":"opaque peer refusal"}),
        )
        .await;
        let credential = plurx_core::sharing::new_secret().expect("synthetic credential");
        let request = Uuid::new_v4();
        let grant = Uuid::new_v4();
        for _ in 0..2 {
            assert!(matches!(
                peer.rotation_status(&credential, request, grant).await,
                Err(PeerError::Rejected(StatusCode::NOT_FOUND))
            ));
        }
        {
            let requests = requests.lock().expect("requests");
            assert_eq!(requests.len(), 2);
            assert!(requests.iter().all(|(path, auth, body)| path
                == &format!("/sharing/v1/grant/rotation/{request}?grant_id={grant}")
                && *auth
                && *body == 0));
        }
        drop(peer);
        server.abort();
        let _ = server.await;
    }
}

// The receipt counts an exact socket descriptor, which only Unix exposes.
#[cfg(all(test, unix))]
mod driver_lifetime_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct LifetimeReceipt {
        socket_fd: i32,
        done: Mutex<Option<tokio::sync::oneshot::Sender<bool>>>,
    }
    impl Drop for LifetimeReceipt {
        fn drop(&mut self) {
            // No await/open/allocation separates the descriptor census from
            // observing the guard's actual Drop after the Hyper driver.
            let closed = unsafe { libc::fcntl(self.socket_fd, libc::F_GETFD) } == -1;
            if let Some(done) = self.done.get_mut().expect("receipt").take() {
                let _ = done.send(closed);
            }
        }
    }
    #[tokio::test]
    async fn source_peer_guard_survives_body_eof_and_drops_after_actual_socket_close() {
        use std::os::fd::AsRawFd;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let stream = tokio::net::TcpStream::connect(listener.local_addr().expect("address"))
            .await
            .expect("client");
        let fd = stream.as_raw_fd();
        let (server, _) = listener.accept().await.expect("server");
        let served = tokio::spawn(async move {
            hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    TokioIo::new(server),
                    hyper::service::service_fn(|_| async {
                        Ok::<_, std::convert::Infallible>(axum::http::Response::new(Body::from(
                            "actual resource bytes",
                        )))
                    }),
                )
                .await
        });
        let (done, mut receipt) = tokio::sync::oneshot::channel();
        let lifetime: Arc<dyn Send + Sync> = Arc::new(LifetimeReceipt {
            socket_fd: fd,
            done: Mutex::new(Some(done)),
        });
        let mut peer =
            PeerConnection::from_stream_with_lifetime(stream, "fixture".into(), Some(lifetime))
                .await
                .expect("driver");
        let response = peer
            .sender
            .send_request(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        assert_eq!(bytes.as_ref(), b"actual resource bytes");
        assert!(
            matches!(
                receipt.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "body EOF must retain driver guard"
        );
        drop(peer);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), receipt)
                .await
                .expect("actual guard drop")
                .expect("receipt"),
            "socket descriptor must close before guard Drop"
        );
        assert!(tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("server closure")
            .expect("server task")
            .is_ok());
    }
}

/// Preserve only the closed typed Start code; peer prose never reaches B's UI.
/// This answer says nothing about whether an older invocation owns resources.
fn source_start_refusal(status: StatusCode, bytes: &[u8]) -> PeerError {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Refusal {
        code: String,
        message: String,
    }
    if status == StatusCode::UNPROCESSABLE_ENTITY {
        if let Ok(refusal) = serde_json::from_slice::<Refusal>(bytes) {
            if refusal.code
                == crate::http::shared_source_playback::SHARING_START_DOLBY_VISION_UNSUPPORTED
                && refusal.message.len() <= 4096
            {
                return PeerError::DolbyVisionUnsupported;
            }
        }
    }
    PeerError::Rejected(status)
}

#[cfg(test)]
mod source_start_refusal_tests {
    use super::*;
    #[test]
    fn sharing_source_start_preserves_only_the_authenticated_dv_refusal_code() {
        let body =
            br#"{"code":"sharing_start_dolby_vision_unsupported","message":"source detail"}"#;
        assert!(matches!(
            source_start_refusal(StatusCode::UNPROCESSABLE_ENTITY, body),
            PeerError::DolbyVisionUnsupported
        ));
        assert!(matches!(
            source_start_refusal(StatusCode::SERVICE_UNAVAILABLE, body),
            PeerError::Rejected(StatusCode::SERVICE_UNAVAILABLE)
        ));
        assert!(matches!(
            source_start_refusal(
                StatusCode::UNPROCESSABLE_ENTITY,
                br#"{"code":"something_else","message":"detail"}"#
            ),
            PeerError::Rejected(StatusCode::UNPROCESSABLE_ENTITY)
        ));
        assert!(matches!(
            source_start_refusal(StatusCode::UNPROCESSABLE_ENTITY, b"not json"),
            PeerError::Rejected(StatusCode::UNPROCESSABLE_ENTITY)
        ));
    }
}
