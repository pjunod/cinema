//! Exact-member transport for an already-owned Shared receiver session.
//!
//! Wire owner tuples are routing claims. Only the parent adapter's retained
//! actor and fresh Store authority may authorize an operation or custody CAS.
use super::*;
use crate::sharing_connection_custody::{AcceptedDriverId, DriverClosureReceipt};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};

pub(super) const FORWARD_PATH: &str = "/internal/cluster/sharing/receiver/forward";
pub(super) const CONTROL_PATH: &str = "/internal/cluster/sharing/receiver/control";
pub(super) const REGISTER_PATH: &str = "/internal/cluster/sharing/receiver/register";
pub(super) const ACK_PATH: &str = "/internal/cluster/sharing/receiver/ack";
const MAX_REQUEST: usize = 128 * 1024;
const MAX_REPLY: usize = 64 * 1024;
const MEDIA_BUDGET: Duration = Duration::from_secs(25);
const CONTROL_BUDGET: Duration = Duration::from_secs(9);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiverForwardTuple {
    #[serde(deserialize_with = "canonical_uuid")]
    pub incarnation_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    pub session_id: Uuid,
    pub owner_node_id: String,
    pub owner_epoch: i64,
    pub owner_identity: String,
}
fn canonical_uuid<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
    let text = String::deserialize(deserializer)?;
    let id = Uuid::parse_str(&text).map_err(serde::de::Error::custom)?;
    if id.get_version_num() != 4
        || id.get_variant() != uuid::Variant::RFC4122
        || id.to_string() != text
    {
        return Err(serde::de::Error::custom("noncanonical receiver UUID"));
    }
    Ok(id)
}
impl ReceiverForwardTuple {
    pub(super) fn from_read(
        proof: &plurx_core::sharing_receiver_delivery::ReceiverRelayReadAuthority,
    ) -> Self {
        Self {
            incarnation_id: proof.owner().incarnation_id,
            session_id: proof.owner().session_id,
            owner_node_id: proof.owner().owner_node_id.clone(),
            owner_epoch: proof.owner().owner_epoch,
            owner_identity: proof.owner_identity(),
        }
    }
    fn valid(&self) -> bool {
        self.incarnation_id.get_version_num() == 4
            && self.session_id.get_version_num() == 4
            && self.owner_epoch > 0
            && !self.owner_node_id.is_empty()
            && self.owner_node_id.len() <= 128
            && !self.owner_node_id.chars().any(char::is_control)
            && self.owner_identity.len() == 64
            && self
                .owner_identity
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiverDirectReference {
    pub item: plurx_core::sharing_catalogue::SharedReference,
    pub lifecycle_generation: i64,
    pub file_id: plurx_core::sharing::SourceId,
    pub revision: plurx_core::sharing_catalogue_details::FileRevision,
}
impl ReceiverDirectReference {
    pub(super) fn from_reference(
        reference: &plurx_core::sharing_file_locators::FileLocatorReference,
    ) -> Self {
        Self {
            item: reference.item.clone(),
            lifecycle_generation: reference.lifecycle_generation,
            file_id: reference.file_id.clone(),
            revision: reference.revision.clone(),
        }
    }
    fn reference(&self) -> plurx_core::sharing_file_locators::FileLocatorReference {
        plurx_core::sharing_file_locators::FileLocatorReference {
            item: self.item.clone(),
            lifecycle_generation: self.lifecycle_generation,
            file_id: self.file_id.clone(),
            revision: self.revision.clone(),
        }
    }
}
#[derive(Clone)]
pub(super) struct ReceiverForwardContext {
    pub tuple: ReceiverForwardTuple,
    pub ingress: ReceiverForwardIngress,
    pub deadline: Instant,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ReceiverForwardOperation {
    Resource {
        relative: String,
    },
    Status,
    Control {
        canonical_body: String,
    },
    End,
    Direct {
        demand: crate::http::sharing_direct_wire::DirectByteRequest,
        reference: ReceiverDirectReference,
        viewer_id: i64,
    },
}
impl ReceiverForwardOperation {
    fn valid(&self) -> bool {
        match self {
            Self::Resource { relative } => {
                plurx_core::sharing_resources::SharingHlsResource::parse(relative).is_ok()
            }
            Self::Status | Self::End => true,
            Self::Control { canonical_body } => {
                !canonical_body.is_empty()
                    && canonical_body.len() <= 64 * 1024
                    && serde_json::from_str::<crate::playback_control::ControlRequestV1>(
                        canonical_body,
                    )
                    .is_ok()
            }
            Self::Direct {
                demand, viewer_id, ..
            } => {
                *viewer_id > 0
                    && demand.range.len() <= 2
                    && demand.range.iter().all(|v| {
                        v.len() <= 1024 && !v.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0))
                    })
            }
        }
    }
    fn budget(&self) -> Duration {
        match self {
            Self::Resource { .. } | Self::Direct { .. } => MEDIA_BUDGET,
            Self::Status | Self::Control { .. } | Self::End => CONTROL_BUDGET,
        }
    }
    fn media(&self) -> bool {
        matches!(self, Self::Resource { .. } | Self::Direct { .. })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiverForwardIngress {
    pub driver: Option<AcceptedDriverId>,
    pub registration: Option<plurx_core::sharing_ingress_custody::IngressRegistration>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardEnvelope {
    hop: u8,
    remaining_ms: u32,
    authority: ReceiverForwardTuple,
    ingress: ReceiverForwardIngress,
    operation: ReceiverForwardOperation,
}
impl ForwardEnvelope {
    fn valid(&self) -> bool {
        self.hop == 1
            && self.authority.valid()
            && self.operation.valid()
            && match (&self.ingress.driver, &self.ingress.registration) {
                (None, None) => matches!(self.operation, ReceiverForwardOperation::End),
                (Some(driver), Some(registration)) => {
                    !matches!(self.operation, ReceiverForwardOperation::End)
                        && driver.valid()
                        && registration.valid()
                        && registration.boot_id == driver.boot_id
                        && registration.connection_id == driver.connection_id
                        && registration.driver_sequence == driver.driver_sequence
                        && registration.closed_confirmation.is_none()
                }
                _ => false,
            }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ReceiverCustodyOperation {
    Register,
    Ack { receipt: DriverClosureReceipt },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustodyEnvelope {
    hop: u8,
    remaining_ms: u32,
    authority: ReceiverForwardTuple,
    ingress: ReceiverForwardIngress,
    operation: ReceiverCustodyOperation,
}

fn remaining(deadline: Instant) -> Result<u32, super::super::error::ApiError> {
    let ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis();
    if ms <= 100 {
        return Err(unavailable());
    }
    u32::try_from(ms).map_err(|_| unavailable())
}
fn received_deadline(
    remaining_ms: u32,
    budget: Duration,
) -> Result<Instant, super::super::error::ApiError> {
    if remaining_ms <= 100 || u128::from(remaining_ms) > budget.as_millis() {
        return Err(unavailable());
    }
    Ok(Instant::now() + Duration::from_millis(u64::from(remaining_ms - 100)))
}
fn unavailable() -> super::super::error::ApiError {
    super::super::error::ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_receiver_forward_unresolved",
        "Shared receiver forwarding is unresolved",
    )
}

/// Keep only end-to-end headers, including Range/HEAD metadata. Connection's
/// nominated fields are removed in addition to the standard hop fields.
fn end_to_end(input: &HeaderMap) -> HeaderMap {
    let nominated = input
        .get_all(axum::http::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut output = HeaderMap::new();
    for (name, value) in input {
        if matches!(
            name.as_str(),
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        ) || nominated.iter().any(|n| n == name.as_str())
        {
            continue;
        }
        output.append(name.clone(), value.clone());
    }
    output
}

fn stream_response(
    response: reqwest::Response,
    custody: Arc<dyn Send + Sync>,
    began: Instant,
) -> Result<axum::response::Response, super::super::error::ApiError> {
    use futures_util::StreamExt;
    let status = StatusCode::from_u16(response.status().as_u16()).map_err(|_| unavailable())?;
    let headers = end_to_end(response.headers());
    let deadline = tokio::time::Instant::from_std(
        began + crate::media_sessions::MAX_ADMITTED_MEDIA_BODY_LIFETIME,
    );
    let stream = futures_util::stream::unfold(
        (Box::pin(response.bytes_stream()), custody, false),
        move |(mut source, custody, finished)| async move {
            if finished {
                return None;
            }
            let next = tokio::time::timeout_at(
                deadline.min(
                    tokio::time::Instant::now()
                        + crate::media_sessions::MEDIA_BODY_NO_PROGRESS_TIMEOUT,
                ),
                source.next(),
            )
            .await;
            match next {
                Ok(Some(Ok(bytes))) => Some((Ok(bytes), (source, custody, false))),
                Ok(Some(Err(error))) => {
                    Some((Err(std::io::Error::other(error)), (source, custody, true)))
                }
                Err(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "receiver forward body deadline",
                    )),
                    (source, custody, true),
                )),
                Ok(None) => None,
            }
        },
    );
    let mut response = axum::response::Response::new(axum::body::Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlReply {
    status: u16,
    headers: Vec<(String, String)>,
    body_hex: String,
}
async fn pack_control(
    response: axum::response::Response,
    deadline: Instant,
) -> Result<Vec<u8>, super::super::error::ApiError> {
    let (parts, body) = response.into_parts();
    let bytes = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        axum::body::to_bytes(body, 24 * 1024),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    let headers = end_to_end(&parts.headers)
        .iter()
        .map(|(name, value)| {
            Ok((
                name.as_str().to_owned(),
                value.to_str().map_err(|_| unavailable())?.to_owned(),
            ))
        })
        .collect::<Result<Vec<_>, super::super::error::ApiError>>()?;
    if headers.len() > 64 {
        return Err(unavailable());
    }
    let body = serde_json::to_vec(&ControlReply {
        status: parts.status.as_u16(),
        headers,
        body_hex: hex::encode(bytes),
    })
    .map_err(|_| unavailable())?;
    if body.len() > MAX_REPLY {
        return Err(unavailable());
    }
    Ok(body)
}
fn unpack_control(body: &[u8]) -> Result<axum::response::Response, super::super::error::ApiError> {
    if body.len() > MAX_REPLY {
        return Err(unavailable());
    }
    let reply: ControlReply = serde_json::from_slice(body).map_err(|_| unavailable())?;
    let bytes = hex::decode(reply.body_hex).map_err(|_| unavailable())?;
    if bytes.len() > 24 * 1024 || reply.headers.len() > 64 {
        return Err(unavailable());
    }
    let mut response = axum::response::Response::new(axum::body::Body::from(bytes));
    *response.status_mut() = StatusCode::from_u16(reply.status).map_err(|_| unavailable())?;
    for (name, value) in reply.headers {
        response.headers_mut().append(
            axum::http::HeaderName::try_from(name).map_err(|_| unavailable())?,
            axum::http::HeaderValue::try_from(value).map_err(|_| unavailable())?,
        );
    }
    *response.headers_mut() = end_to_end(response.headers());
    Ok(response)
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReceiverCustodyReply {
    Applied,
    Replay,
    Refused,
    ReconciledClosed,
    Unresolved,
}

/// The caller owns the reservation through ambiguity. A timeout never releases
/// an accepted driver or proves that a delayed registration cannot commit.
pub(super) async fn exchange_custody(
    state: &AppState,
    authority: &ReceiverForwardTuple,
    ingress: &ReceiverForwardIngress,
    operation: ReceiverCustodyOperation,
    deadline: Instant,
) -> Result<ReceiverCustodyReply, ReceiverStartError> {
    let path = match &operation {
        ReceiverCustodyOperation::Register => REGISTER_PATH,
        ReceiverCustodyOperation::Ack { .. } => ACK_PATH,
    };
    let Some(driver) = ingress.driver.as_ref() else {
        return Err(ReceiverStartError::Unresolved);
    };
    if !authority.valid()
        || !driver.valid()
        || ingress.registration.as_ref().is_none_or(|registration| {
            !registration.valid()
                || registration.boot_id != driver.boot_id
                || registration.connection_id != driver.connection_id
                || registration.driver_sequence != driver.driver_sequence
        })
    {
        return Err(ReceiverStartError::Unresolved);
    }
    let body = serde_json::to_vec(&CustodyEnvelope {
        hop: 1,
        remaining_ms: remaining(deadline).map_err(|_| ReceiverStartError::Unresolved)?,
        authority: authority.clone(),
        ingress: ingress.clone(),
        operation,
    })
    .map_err(|_| ReceiverStartError::Unresolved)?;
    if body.len() > MAX_REQUEST {
        return Err(ReceiverStartError::Unresolved);
    }
    let response = state
        .media_sessions
        .receiver_forward_bounded(
            &authority.owner_node_id,
            path,
            body,
            tokio::time::Instant::from_std(deadline),
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
    if response.status != reqwest::StatusCode::OK {
        return Err(ReceiverStartError::Unresolved);
    }
    serde_json::from_slice(&response.body).map_err(|_| ReceiverStartError::Unresolved)
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    path: &'static str,
    body: &[u8],
    deadline: Instant,
) -> Result<plurx_core::cluster::membership::InternalPeerAuth, super::super::error::ApiError> {
    if body.is_empty() || body.len() > MAX_REQUEST {
        return Err(unavailable());
    }
    let auth = crate::http::peer_transport::exact_auth_from_headers(headers)
        .ok_or(super::super::error::ApiError::Unauthorized)?;
    let valid = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state
            .membership
            .authorize_internal_peer_request(&auth, "POST", path, body),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    if !valid {
        return Err(super::super::error::ApiError::Unauthorized);
    }
    Ok(auth)
}
async fn signed(
    state: &AppState,
    auth: &plurx_core::cluster::membership::InternalPeerAuth,
    path: &'static str,
    body: Vec<u8>,
    deadline: Instant,
) -> Result<axum::response::Response, super::super::error::ApiError> {
    if body.len() > MAX_REPLY {
        return Err(unavailable());
    }
    let payload = crate::http::peer_transport::signed_response_payload(200, &body);
    if Instant::now() >= deadline {
        return Err(unavailable());
    }
    let signature = state
        .membership
        .sign_internal_peer_response(&auth.node_id, &auth.nonce, path, &payload)
        .map_err(|_| unavailable())?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .header(
            crate::http::peer_transport::RESPONSE_SIGNATURE_HEADER,
            signature,
        )
        .body(axum::body::Body::from(body))
        .map_err(|_| unavailable())
}

pub(super) async fn relay_public(
    state: AppState,
    route: plurx_core::domain::MediaSessionRoute,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let original = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map_or_else(
            || request.uri().path().to_owned(),
            |uri| uri.path().to_owned(),
        );
    let prefix = format!("/api/v1/hls/{}/", route.session_id);
    let suffix = if original == format!("/api/v1/hls/{}", route.session_id) {
        ""
    } else if let Some(suffix) = original.strip_prefix(&prefix) {
        suffix
    } else {
        return unavailable().into_response();
    };
    let method = request.method().clone();
    let query = request.uri().query().map(str::to_owned);
    let connection = request
        .extensions()
        .get::<crate::SharingConnectionCancellation>()
        .cloned();
    let operation = match (suffix, method.as_str(), query.as_deref()) {
        ("", "DELETE", None) => ReceiverForwardOperation::End,
        ("status", "GET", None) => ReceiverForwardOperation::Status,
        ("control", "POST", None) => {
            let bytes = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
                Ok(bytes) => bytes,
                Err(_) => return unavailable().into_response(),
            };
            let canonical_body = match String::from_utf8(bytes.to_vec()) {
                Ok(body) => body,
                Err(_) => return unavailable().into_response(),
            };
            ReceiverForwardOperation::Control { canonical_body }
        }
        (_, "GET", query) => ReceiverForwardOperation::Resource {
            relative: match query {
                Some(query) => format!("{suffix}?{query}"),
                None => suffix.to_owned(),
            },
        },
        _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
    };
    match relay_operation(state, route, operation, connection).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

pub(super) async fn relay_operation(
    state: AppState,
    route: plurx_core::domain::MediaSessionRoute,
    operation: ReceiverForwardOperation,
    connection: Option<crate::SharingConnectionCancellation>,
) -> Result<axum::response::Response, super::super::error::ApiError> {
    if !operation.valid() || route.owner_node_id == state.node_id {
        return Err(unavailable());
    }
    let began = Instant::now();
    let mut deadline = began + operation.budget();
    let state = Arc::new(state);
    let (authority, ingress, guard): (_, _, Option<Arc<dyn Send + Sync>>) =
        if matches!(operation, ReceiverForwardOperation::End) {
            if let Some(connection) = connection.as_ref() {
                super::receiver_forward_reconcile_cleanup(
                    state.clone(),
                    &route,
                    connection,
                    deadline,
                )
                .await
                .map_err(|_| unavailable())?;
            }
            let tuple = super::receiver_forward_cleanup_tuple(&state, &route)
                .await
                .map_err(|_| unavailable())?;
            (
                tuple,
                ReceiverForwardIngress {
                    driver: None,
                    registration: None,
                },
                None,
            )
        } else {
            let proof = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                state.store.receiver_relay_read_authority(&route),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?
            .ok_or_else(unavailable)?;
            let remaining_ms = proof.deadline_ms().saturating_sub(clock_ms());
            if remaining_ms <= 100 {
                return Err(unavailable());
            }
            deadline = deadline.min(Instant::now() + Duration::from_millis(remaining_ms as u64));
            let connection = connection.as_ref().ok_or_else(unavailable)?;
            let guard =
                super::receiver_forward_admit(state.clone(), &route, &proof, connection, deadline)
                    .await
                    .map_err(|_| unavailable())?;
            let ingress = guard.ingress.clone();
            (
                ReceiverForwardTuple::from_read(&proof),
                ingress,
                Some(Arc::new(guard)),
            )
        };
    let media = operation.media();
    let wire = ForwardEnvelope {
        hop: 1,
        remaining_ms: remaining(deadline)?,
        authority: authority.clone(),
        ingress,
        operation,
    };
    if !wire.valid() {
        return Err(unavailable());
    }
    let body = serde_json::to_vec(&wire).map_err(|_| unavailable())?;
    if body.len() > MAX_REQUEST {
        return Err(unavailable());
    }
    if media {
        let response = state
            .media_sessions
            .receiver_forward_stream(
                &authority.owner_node_id,
                body,
                tokio::time::Instant::from_std(deadline),
            )
            .await
            .map_err(|_| unavailable())?;
        stream_response(response, guard.ok_or_else(unavailable)?, began)
    } else {
        let response = state
            .media_sessions
            .receiver_forward_bounded(
                &authority.owner_node_id,
                CONTROL_PATH,
                body,
                tokio::time::Instant::from_std(deadline),
            )
            .await
            .map_err(|_| unavailable())?;
        if response.status != reqwest::StatusCode::OK {
            return Err(unavailable());
        }
        let response = unpack_control(&response.body)?;
        if let Some(guard) = guard {
            use futures_util::StreamExt;
            let (parts, body) = response.into_parts();
            let stream = body.into_data_stream().map(move |frame| {
                let _custody = &guard;
                frame
            });
            Ok(axum::http::Response::from_parts(
                parts,
                axum::body::Body::from_stream(stream),
            ))
        } else {
            Ok(response)
        }
    }
}

pub(super) async fn receive_media(
    axum::extract::State(state): axum::extract::State<AppState>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match receive_operation(
        state,
        connection.map(|connection| connection.0),
        headers,
        body,
        FORWARD_PATH,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
pub(super) async fn receive_control(
    axum::extract::State(state): axum::extract::State<AppState>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match receive_operation(
        state,
        connection.map(|connection| connection.0),
        headers,
        body,
        CONTROL_PATH,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn receive_operation(
    state: AppState,
    connection: Option<crate::SharingConnectionCancellation>,
    headers: HeaderMap,
    body: axum::body::Bytes,
    path: &'static str,
) -> Result<axum::response::Response, super::super::error::ApiError> {
    let began = Instant::now();
    if body.len() > MAX_REQUEST {
        return Err(unavailable());
    }
    let wire: ForwardEnvelope = serde_json::from_slice(&body).map_err(|_| unavailable())?;
    if !wire.valid()
        || wire.authority.owner_node_id != state.node_id
        || wire.operation.media() != (path == FORWARD_PATH)
    {
        return Err(unavailable());
    }
    let deadline = received_deadline(wire.remaining_ms, wire.operation.budget())?
        .min(began + Duration::from_millis(u64::from(wire.remaining_ms - 100)));
    let auth = authenticate(&state, &headers, path, &body, deadline).await?;
    if wire
        .ingress
        .registration
        .as_ref()
        .is_some_and(|registration| registration.node_id != auth.node_id)
    {
        return Err(unavailable());
    }
    // Terminal retries use retained actual retirement evidence, never actor absence.
    if matches!(wire.operation, ReceiverForwardOperation::End)
        && super::ingress_custody::confirmed_terminal_end(&state, &wire.authority).await
    {
        use axum::response::IntoResponse;
        return signed(
            &state,
            &auth,
            path,
            pack_control(StatusCode::NO_CONTENT.into_response(), deadline).await?,
            deadline,
        )
        .await;
    }
    let actor = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        super::validate_owner(&state, &wire.authority),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    if !matches!(wire.operation, ReceiverForwardOperation::End) {
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            super::validate_forward_ingress(&state, &wire.authority, &wire.ingress),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    }
    let context = ReceiverForwardContext {
        tuple: wire.authority.clone(),
        ingress: wire.ingress.clone(),
        deadline,
    };
    let response = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        Ok(match wire.operation {
            ReceiverForwardOperation::Direct {
                demand,
                reference,
                viewer_id,
            } => {
                let connection = connection.as_ref().ok_or_else(unavailable)?;
                super::receiver_direct_actor(
                    state.clone(),
                    actor,
                    &reference.reference(),
                    demand,
                    viewer_id,
                    connection,
                    Some(&context),
                )
                .await
            }
            operation => {
                let (suffix, method, bytes) = match operation {
                    ReceiverForwardOperation::Resource { relative } => {
                        (relative, axum::http::Method::GET, Vec::new())
                    }
                    ReceiverForwardOperation::Status => {
                        ("status".to_owned(), axum::http::Method::GET, Vec::new())
                    }
                    ReceiverForwardOperation::Control { canonical_body } => (
                        "control".to_owned(),
                        axum::http::Method::POST,
                        canonical_body.into_bytes(),
                    ),
                    ReceiverForwardOperation::End => {
                        (String::new(), axum::http::Method::DELETE, Vec::new())
                    }
                    ReceiverForwardOperation::Direct { .. } => return Err(unavailable()),
                };
                let uri = format!("/api/v1/hls/{}/{}", wire.authority.session_id, suffix);
                let mut request = axum::http::Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(axum::body::Body::from(bytes))
                    .map_err(|_| unavailable())?;
                if let Some(connection) = connection {
                    request.extensions_mut().insert(connection);
                }
                // The owner adapter must compare this complete tuple/registration
                // to its actual retained actor and durable custody before writing.
                request.extensions_mut().insert(context);
                let bare_suffix = suffix.split('?').next().unwrap_or(&suffix);
                super::receiver_media_actor(state.clone(), actor, bare_suffix, request).await
            }
        })
    })
    .await
    .map_err(|_| unavailable())??;
    if path == FORWARD_PATH {
        Ok(response)
    } else {
        signed(
            &state,
            &auth,
            path,
            pack_control(response, deadline).await?,
            deadline,
        )
        .await
    }
}

pub(super) async fn receive_register(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match receive_custody(state, headers, body, REGISTER_PATH).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
pub(super) async fn receive_ack(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match receive_custody(state, headers, body, ACK_PATH).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn receive_custody(
    state: AppState,
    headers: HeaderMap,
    body: axum::body::Bytes,
    path: &'static str,
) -> Result<axum::response::Response, super::super::error::ApiError> {
    let began = Instant::now();
    if body.len() > MAX_REQUEST {
        return Err(unavailable());
    }
    let wire: CustodyEnvelope = serde_json::from_slice(&body).map_err(|_| unavailable())?;
    let deadline = received_deadline(wire.remaining_ms, CONTROL_BUDGET)?
        .min(began + Duration::from_millis(u64::from(wire.remaining_ms - 100)));
    let auth = authenticate(&state, &headers, path, &body, deadline).await?;
    let driver = wire.ingress.driver.as_ref().ok_or_else(unavailable)?;
    let registration = wire.ingress.registration.as_ref().ok_or_else(unavailable)?;
    if wire.hop != 1
        || !wire.authority.valid()
        || wire.authority.owner_node_id != state.node_id
        || !driver.valid()
        || !registration.valid()
        || registration.node_id != auth.node_id
        || registration.boot_id != driver.boot_id
        || registration.connection_id != driver.connection_id
        || registration.driver_sequence != driver.driver_sequence
        || registration.closed_confirmation.is_some()
    {
        return Err(unavailable());
    }
    use plurx_core::sharing_ingress_custody::CustodyMutation;
    let reply = match wire.operation {
        ReceiverCustodyOperation::Register if path == REGISTER_PATH => {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                super::register_owner(&state, &wire.authority, &wire.ingress),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?
            {
                CustodyMutation::Applied => ReceiverCustodyReply::Applied,
                CustodyMutation::Replay => ReceiverCustodyReply::Replay,
                CustodyMutation::Refused => ReceiverCustodyReply::Refused,
            }
        }
        ReceiverCustodyOperation::Ack { receipt }
            if path == ACK_PATH && receipt.matches(driver) =>
        {
            tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                super::ack_owner(&state, &wire.authority, &wire.ingress, &receipt),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?
        }
        _ => return Err(unavailable()),
    };
    signed(
        &state,
        &auth,
        path,
        serde_json::to_vec(&reply).map_err(|_| unavailable())?,
        deadline,
    )
    .await
}

pub(super) fn internal_router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(FORWARD_PATH, axum::routing::post(receive_media))
        .route(CONTROL_PATH, axum::routing::post(receive_control))
        .route(REGISTER_PATH, axum::routing::post(receive_register))
        .route(ACK_PATH, axum::routing::post(receive_ack))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_REQUEST))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn envelope(operation: ReceiverForwardOperation) -> ForwardEnvelope {
        let boot_id = Uuid::new_v4();
        let connection_id = Uuid::new_v4();
        ForwardEnvelope {
            hop: 1,
            remaining_ms: 5000,
            authority: ReceiverForwardTuple {
                incarnation_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                owner_node_id: "actual-worker".to_owned(),
                owner_epoch: 1,
                owner_identity: "a".repeat(64),
            },
            ingress: ReceiverForwardIngress {
                driver: Some(AcceptedDriverId {
                    boot_id,
                    connection_id,
                    driver_sequence: 7,
                }),
                registration: Some(plurx_core::sharing_ingress_custody::IngressRegistration {
                    node_id: "actual-ingress".to_owned(),
                    boot_id,
                    connection_id,
                    driver_sequence: 7,
                    registration_sequence: 11,
                    closed_confirmation: None,
                }),
            },
            operation,
        }
    }
    #[test]
    fn receiver_forward_envelope_refuses_hops_and_mismatched_physical_driver() {
        let mut wire = envelope(ReceiverForwardOperation::Status);
        assert!(wire.valid());
        wire.hop = 2;
        assert!(!wire.valid());
        wire.hop = 1;
        if let Some(driver) = wire.ingress.driver.as_mut() {
            driver.driver_sequence += 1;
        }
        assert!(!wire.valid());
    }
    #[test]
    fn receiver_end_never_creates_a_new_custody_ordinal() {
        let mut wire = envelope(ReceiverForwardOperation::End);
        assert!(!wire.valid());
        wire.ingress.registration = None;
        wire.ingress.driver = None;
        assert!(wire.valid());
        wire.operation = ReceiverForwardOperation::Status;
        assert!(!wire.valid());
    }
    #[test]
    fn receiver_forward_resource_refuses_arbitrary_paths() {
        assert!(ReceiverForwardOperation::Resource {
            relative: "index.m3u8".to_owned()
        }
        .valid());
        for relative in [
            "https://other/segment.m4s",
            "../../file",
            "/api/v1/files/1/direct",
        ] {
            assert!(!ReceiverForwardOperation::Resource {
                relative: relative.to_owned()
            }
            .valid());
        }
    }
    #[test]
    fn receiver_forward_deadline_cannot_expand_operation_budget() {
        assert!(received_deadline(100, CONTROL_BUDGET).is_err());
        assert!(received_deadline(9001, CONTROL_BUDGET).is_err());
        assert!(received_deadline(9000, CONTROL_BUDGET).is_ok());
        assert!(remaining(Instant::now()).is_err());
    }
    #[test]
    fn receiver_forward_preserves_head_range_and_removes_connection_headers(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert("content-range", "bytes 5-9/10".parse()?);
        headers.insert("content-length", "5".parse()?);
        headers.insert("etag", "\"exact\"".parse()?);
        headers.insert("connection", "x-private, keep-alive".parse()?);
        headers.insert("x-private", "must-drop".parse()?);
        let projected = end_to_end(&headers);
        assert_eq!(projected.get("content-range"), headers.get("content-range"));
        assert_eq!(
            projected.get("content-length"),
            headers.get("content-length")
        );
        assert_eq!(projected.get("etag"), headers.get("etag"));
        assert!(!projected.contains_key("connection"));
        assert!(!projected.contains_key("x-private"));
        Ok(())
    }
}
