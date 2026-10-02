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
    sharing::{canonical_uuid, Endpoint, SharingIdentity},
    sharing_tls::dial_numeric_peer,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::{net::SocketAddr, time::Duration};
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub(crate) enum PeerError {
    Unavailable,
    IdentityMismatch,
    ProtocolUnsupported,
    Authentication,
    InvalidResponse,
    Rejected(StatusCode),
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
pub(crate) struct PeerConnection {
    sender: SendRequest<Body>,
    connection: tokio::task::JoinHandle<()>,
    host: String,
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
        use futures_util::{stream::FuturesUnordered, StreamExt};
        plurx_core::sharing::validate_endpoints(endpoints)
            .map_err(|_| PeerError::InvalidResponse)?;
        // Four approved endpoints and at most four numeric targets each. Racing this
        // fixed set lets one offline hint coexist with a reachable replica.
        // Losing dials are dropped as soon as a pinned stream wins.
        tokio::time::timeout(Duration::from_secs(5), async {
            let egress = manager.egress();
            let mut resolutions = FuturesUnordered::new();
            for endpoint in endpoints {
                let egress = &egress;
                resolutions.push(async move {
                    (
                        endpoint,
                        plurx_core::sharing_dns::resolve_tailnet(&endpoint.ts_fqdn, egress).await,
                    )
                });
            }
            let mut resolved = Vec::with_capacity(endpoints.len());
            while let Some((endpoint, result)) = resolutions.next().await {
                resolved.push((endpoint, result.unwrap_or_default()));
            }
            let mut attempts = FuturesUnordered::new();
            for (endpoint, answers) in resolved {
                let mut addresses: Vec<_> = answers
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip, endpoint.port))
                    .collect();
                // Invitation hints and explicit recipient-side overrides are
                // already validated. They retain the approved TLS pin.
                let v4 = SocketAddr::new(endpoint.ipv4.into(), endpoint.port);
                if !addresses.contains(&v4) {
                    addresses.push(v4);
                }
                if let Some(ip) = endpoint.ipv6 {
                    let v6 = SocketAddr::new(ip.into(), endpoint.port);
                    if !addresses.contains(&v6) {
                        addresses.push(v6);
                    }
                }
                for address in addresses {
                    let egress = &egress;
                    attempts.push(async move {
                        dial_numeric_peer(address, &endpoint.ts_fqdn, &endpoint.spki_sha256, egress)
                            .await
                            .map(|stream| {
                                (stream, format!("{}:{}", endpoint.ts_fqdn, endpoint.port))
                            })
                    });
                }
            }
            while let Some(result) = attempts.next().await {
                if let Ok((stream, host)) = result {
                    drop(attempts);
                    let mut peer = Self::from_stream(stream, host).await?;
                    let identity = peer.verify_identity(expected).await?;
                    return Ok((peer, identity));
                }
            }
            Err(PeerError::Unavailable)
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
    async fn from_stream<
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    >(
        stream: S,
        host: String,
    ) -> Result<Self, PeerError> {
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|_| PeerError::Unavailable)?;
        Ok(Self {
            sender,
            connection: tokio::spawn(async move {
                let _ = connection.await;
            }),
            host,
        })
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
        tokio::time::timeout(Duration::from_secs(5), async {
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
                16 * 1024,
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
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|(path, auth, body)| path
            == &format!("/sharing/v1/grant/rotation/{request}?grant_id={grant}")
            && *auth
            && *body == 0));
        drop(peer);
        server.abort();
    }
}
