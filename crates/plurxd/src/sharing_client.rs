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

const MANAGEMENT_RESPONSE_BYTES: usize = 128 * 1024;
const CATALOGUE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
#[derive(Clone, Copy)]
enum ResponseBudget {
    Management,
    Catalogue,
}
impl ResponseBudget {
    fn bytes(self) -> usize {
        match self {
            Self::Management => MANAGEMENT_RESPONSE_BYTES,
            Self::Catalogue => CATALOGUE_RESPONSE_BYTES,
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
                                (stream, format!("{}:{}", endpoint.ts_fqdn, endpoint.port))
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
                                    (stream, format!("{}:{}", endpoint.ts_fqdn, endpoint.port))
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
