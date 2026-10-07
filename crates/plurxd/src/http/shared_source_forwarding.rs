//! Source forwarding carries routing evidence, never an adopted worker proof.
//! The parent validates its actual invocation and owns every durable custody CAS.
use super::*;
use crate::{
    sharing_connection_custody::{
        AcceptedDriverId, CapturedIngressObligation, DriverClosureReceipt,
    },
    state::AppState,
};
use axum::{
    body::{Body, Bytes},
    extract::{FromRequestParts, State},
    http::{request::Parts, Request},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::{FutureExt, StreamExt};
use plurx_core::playback_principal::PlaybackPrincipal;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tower::ServiceExt;

pub(crate) const LOCATE_PATH: &str = "/internal/cluster/sharing/source/locate";
pub(crate) const PREPARE_PATH: &str = "/internal/cluster/sharing/source/prepare";
pub(crate) const CONTROL_PATH: &str = "/internal/cluster/sharing/source/control";
pub(crate) const FORWARD_PATH: &str = "/internal/cluster/sharing/source/forward";
pub(crate) const REGISTER_PATH: &str = "/internal/cluster/sharing/source/register";
pub(crate) const ACK_PATH: &str = "/internal/cluster/sharing/source/ack";
pub(crate) const MAX_WIRE_BYTES: usize = 1024 * 1024;
const MAX_PUBLIC_BODY: usize = 128 * 1024;
const MAX_REPLY_BYTES: usize = 64 * 1024;
const HELD_MAX: usize = 512;
static MEDIA_REQUESTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(16)));
static CONTROL_REQUESTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(16)));
static CLEANUP_REQUESTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(8)));
fn credit(operation: Operation) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    credit_from(
        operation,
        &MEDIA_REQUESTS,
        &CONTROL_REQUESTS,
        &CLEANUP_REQUESTS,
    )
}
fn credit_from(
    operation: Operation,
    media: &Arc<tokio::sync::Semaphore>,
    control: &Arc<tokio::sync::Semaphore>,
    cleanup: &Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    let credits = if operation.media() {
        media
    } else if matches!(operation, Operation::End | Operation::Status) {
        cleanup
    } else {
        control
    };
    Arc::clone(credits)
        .try_acquire_owned()
        .map_err(|_| unavailable())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForwardAuthority {
    pub credential_hash: String,
    pub principal: PlaybackPrincipal,
    pub owner_node_id: String,
    pub incarnation_id: Uuid,
    pub dispatch_generation: i64,
    pub expected_registry_boot: Uuid,
    pub reference: SourcePlaybackTarget,
    pub request_id: Uuid,
    pub request_fingerprint: String,
    pub playback_id: String,
    /// Claimed routing identity: the adapter compares the actual assignment's
    /// centralized custody identity, never authenticates from this string.
    pub owner_identity: String,
    pub cleanup_only: bool,
}
impl ForwardAuthority {
    pub(super) fn owner_identity(&self) -> &str {
        &self.owner_identity
    }
    fn valid(&self) -> bool {
        valid_hash(&self.credential_hash)
            && valid_hash(&self.request_fingerprint)
            && valid_hash(&self.owner_identity)
            && valid_node(&self.owner_node_id)
            && valid_uuid(self.incarnation_id)
            && valid_uuid(self.request_id)
            && self.dispatch_generation == 1
            && valid_uuid(self.expected_registry_boot)
            && self.principal.valid_admission_shape()
            && matches!(self.principal, PlaybackPrincipal::Sharing { .. })
            && !self.playback_id.is_empty()
            && self.playback_id.len() <= 128
            && !self.playback_id.chars().any(char::is_control)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FreshAuthority {
    pub credential_hash: String,
    pub principal: PlaybackPrincipal,
    pub candidate_node_id: String,
    pub expected_registry_boot: Option<Uuid>,
    pub reference: SourcePlaybackTarget,
    pub request_id: Uuid,
}
impl FreshAuthority {
    fn valid(&self) -> bool {
        valid_hash(&self.credential_hash)
            && valid_uuid(self.request_id)
            && valid_node(&self.candidate_node_id)
            && self.expected_registry_boot.is_none_or(valid_uuid)
            && self.principal.valid_admission_shape()
            && matches!(self.principal, PlaybackPrincipal::Sharing { .. })
    }
}

pub(super) enum ForwardRoute {
    Local,
    Retained(Box<ForwardAuthority>),
    Fresh(Box<FreshAuthority>),
    Pending(Box<FreshAuthority>),
}
fn remote_operation_allowed(route: &ForwardRoute, operation: Operation) -> bool {
    match route {
        ForwardRoute::Fresh(_) => operation == Operation::Start,
        ForwardRoute::Pending(authority) => {
            authority.expected_registry_boot.is_some_and(valid_uuid)
                && matches!(
                    operation,
                    Operation::Start | Operation::Status | Operation::End
                )
        }
        ForwardRoute::Retained(_) => true,
        ForwardRoute::Local => false,
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForwardIngress {
    pub node_id: String,
    pub driver: AcceptedDriverId,
    pub registration_sequence: Option<u64>,
}
impl ForwardIngress {
    fn valid(&self) -> bool {
        valid_node(&self.node_id)
            && self.driver.valid()
            && self
                .registration_sequence
                .is_none_or(|sequence| (1..=9_007_199_254_740_991).contains(&sequence))
    }
}
#[derive(Clone)]
struct ForwardAuthentication {
    authority: ForwardAuthority,
    ingress: ForwardIngress,
    deadline: Instant,
}
#[derive(Clone)]
pub(in crate::http) struct SourceHeaders {
    headers: HeaderMap,
    forwarded: Option<ForwardAuthentication>,
    fresh: Option<(String, PlaybackPrincipal, Instant)>,
}
impl From<HeaderMap> for SourceHeaders {
    fn from(headers: HeaderMap) -> Self {
        Self {
            headers,
            forwarded: None,
            fresh: None,
        }
    }
}
impl std::ops::Deref for SourceHeaders {
    type Target = HeaderMap;
    fn deref(&self) -> &HeaderMap {
        &self.headers
    }
}
impl<S: Send + Sync> FromRequestParts<S> for SourceHeaders {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self {
            headers: parts.headers.clone(),
            forwarded: parts.extensions.get::<ForwardAuthentication>().cloned(),
            fresh: None,
        })
    }
}
impl SourceHeaders {
    // Called by the actual worker only after current grant/reference authentication.
    pub(super) fn verified_fresh(
        hash: String,
        principal: PlaybackPrincipal,
        deadline: Instant,
    ) -> Self {
        Self {
            headers: HeaderMap::new(),
            forwarded: None,
            fresh: Some((hash, principal, deadline)),
        }
    }
    pub(super) fn deadline(&self, budget: Duration) -> Instant {
        let local = Instant::now() + budget;
        self.forwarded
            .as_ref()
            .map_or(local, |auth| local.min(auth.deadline))
    }
    pub(super) fn cleanup_only(&self) -> bool {
        self.forwarded
            .as_ref()
            .is_some_and(|auth| auth.authority.cleanup_only)
    }
    pub(super) fn forwarded_ingress(&self) -> Option<&ForwardIngress> {
        self.forwarded.as_ref().map(|auth| &auth.ingress)
    }
}
pub(crate) trait AuthenticationHeaders {
    fn raw_headers(&self) -> &HeaderMap;
    fn authenticated_hash(&self) -> Option<&str> {
        None
    }
    fn authenticated_principal(&self) -> Option<&PlaybackPrincipal> {
        None
    }
}
impl AuthenticationHeaders for HeaderMap {
    fn raw_headers(&self) -> &HeaderMap {
        self
    }
}
impl AuthenticationHeaders for SourceHeaders {
    fn raw_headers(&self) -> &HeaderMap {
        &self.headers
    }
    fn authenticated_hash(&self) -> Option<&str> {
        self.forwarded
            .as_ref()
            .map(|auth| auth.authority.credential_hash.as_str())
            .or_else(|| self.fresh.as_ref().map(|auth| auth.0.as_str()))
    }
    fn authenticated_principal(&self) -> Option<&PlaybackPrincipal> {
        self.forwarded
            .as_ref()
            .map(|auth| &auth.authority.principal)
            .or_else(|| self.fresh.as_ref().map(|auth| &auth.1))
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Start,
    Status,
    VodStatus,
    Control,
    Resources,
    Direct,
    End,
}
impl Operation {
    fn budget(self) -> Duration {
        Duration::from_secs(match self {
            Self::Start => 305,
            Self::Resources | Self::Direct => 25,
            _ => 9,
        })
    }
    fn media(self) -> bool {
        matches!(self, Self::Resources | Self::Direct)
    }
}
fn operation(path: &str, body: &[u8]) -> Result<Operation, ApiError> {
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["", "sharing", "v1", "items", item, "files", file, "sessions"] => {
            parse_start_request(body, item, file)?;
            Ok(Operation::Start)
        }
        ["", "sharing", "v1", "items", item, "files", file, "sessions", request, action] => {
            match *action {
                "vod-status" => {
                    parse_live_operation(body, item, file, request, false)?;
                    Ok(Operation::VodStatus)
                }
                "control" => {
                    parse_live_operation(body, item, file, request, true)?;
                    Ok(Operation::Control)
                }
                "resources" => {
                    parse_resource_request(body, item, file, request)?;
                    Ok(Operation::Resources)
                }
                "direct" => {
                    direct::parse_direct_request(body, item, file, request)?;
                    Ok(Operation::Direct)
                }
                "status" | "end" => {
                    parse_operation_request(body, item, file, request)?;
                    Ok(if *action == "end" {
                        Operation::End
                    } else {
                        Operation::Status
                    })
                }
                _ => Err(invalid()),
            }
        }
        _ => Err(invalid()),
    }
}
fn routing_recipe(path: &str, body: &[u8]) -> Result<Vec<u8>, ApiError> {
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["", "sharing", "v1", "items", item, "files", file, "sessions"] => {
            Ok(parse_start_request(body, item, file)?.canonical_recipe)
        }
        ["", "sharing", "v1", "items", item, "files", file, "sessions", request, action]
            if matches!(*action, "status" | "end") =>
        {
            Ok(parse_operation_request(body, item, file, request)?
                .start
                .canonical_recipe)
        }
        _ => Err(invalid()),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardRequest {
    path: String,
    canonical_body: String,
    operation: Operation,
    authority: ForwardAuthority,
    ingress: ForwardIngress,
    remaining_ms: u32,
    hop: u8,
}
impl ForwardRequest {
    fn valid_transport(&self, node: &str, ingress: &str, path: &str, parsed: Operation) -> bool {
        self.hop == 1
            && self.operation == parsed
            && parsed.media() == (path == FORWARD_PATH)
            && self.authority.valid()
            && self.authority.owner_node_id == node
            && self.ingress.valid()
            && self.ingress.node_id == ingress
            && (!self.authority.cleanup_only
                || matches!(parsed, Operation::Status | Operation::End))
            && (parsed == Operation::End
                || (parsed == Operation::Status && self.authority.cleanup_only))
                == self.ingress.registration_sequence.is_none()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FreshRequest {
    path: String,
    canonical_body: String,
    operation: Operation,
    ingress: ForwardIngress,
    authority: FreshAuthority,
    remaining_ms: u32,
    hop: u8,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForwardCustody {
    pub ingress: ForwardIngress,
    pub action: CustodyAction,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CustodyAction {
    Register,
    Ack { receipt: DriverClosureReceipt },
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CustodyReply {
    Registered,
    Acknowledged,
    ReconciledClosed,
    NeverRegistered,
    Unresolved,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustodyRequest {
    authority: ForwardAuthority,
    custody: ForwardCustody,
    remaining_ms: u32,
    hop: u8,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlReply {
    status: u16,
    headers: Vec<(String, String)>,
    body_hex: String,
}

struct HeldObligation {
    authority: ForwardAuthority,
    ingress: ForwardIngress,
    obligation: CapturedIngressObligation,
    registered: AtomicBool,
    monitored: AtomicBool,
}
#[derive(Clone)]
struct FreshPin {
    principal: PlaybackPrincipal,
    request: Uuid,
    reference: SourcePlaybackTarget,
    body_hash: [u8; 32],
    candidate: String,
    registry_boot: Option<Uuid>,
    credential_hash: String,
}
#[derive(Clone, Default)]
pub(super) struct ForwardingRegistry {
    held: Arc<Mutex<Vec<Arc<HeldObligation>>>>,
    fresh: Arc<Mutex<Vec<FreshPin>>>,
}
impl ForwardingRegistry {
    fn pin(&self, authority: &mut FreshAuthority, body: &[u8]) -> Result<(), ApiError> {
        use sha2::Digest;
        let body_hash: [u8; 32] = sha2::Sha256::digest(body).into();
        let mut pins = self.fresh.lock().expect("Source forwarding candidate pins");
        if let Some(pin) = pins
            .iter()
            .find(|pin| pin.principal == authority.principal && pin.request == authority.request_id)
        {
            if pin.reference != authority.reference || pin.body_hash != body_hash {
                return Err(invalid());
            }
            authority.candidate_node_id.clone_from(&pin.candidate);
            authority.expected_registry_boot = pin.registry_boot;
        } else {
            if pins.len() >= HELD_MAX {
                return Err(unavailable());
            }
            pins.push(FreshPin {
                principal: authority.principal.clone(),
                request: authority.request_id,
                reference: authority.reference.clone(),
                body_hash,
                candidate: authority.candidate_node_id.clone(),
                registry_boot: authority.expected_registry_boot,
                credential_hash: authority.credential_hash.clone(),
            });
        }
        Ok(())
    }
    pub(super) fn pending_candidate(
        &self,
        principal: &PlaybackPrincipal,
        request: Uuid,
        reference: &SourcePlaybackTarget,
        canonical_recipe: &[u8],
    ) -> Option<(String, Option<Uuid>)> {
        use sha2::Digest;
        let body_hash: [u8; 32] = sha2::Sha256::digest(canonical_recipe).into();
        self.fresh
            .lock()
            .expect("Source forwarding candidate pins")
            .iter()
            .find(|pin| {
                &pin.principal == principal
                    && pin.request == request
                    && &pin.reference == reference
                    && pin.body_hash == body_hash
            })
            .map(|pin| (pin.candidate.clone(), pin.registry_boot))
    }
    pub(super) fn pending_cleanup_route(
        &self,
        hash: &str,
        viewer: &str,
        request: Uuid,
        reference: &SourcePlaybackTarget,
        canonical_recipe: &[u8],
    ) -> Option<(PlaybackPrincipal, String, Uuid)> {
        use sha2::Digest;
        let body_hash: [u8; 32] = sha2::Sha256::digest(canonical_recipe).into();
        self.fresh
            .lock()
            .expect("Source candidate pins")
            .iter()
            .find_map(|pin| {
                let PlaybackPrincipal::Sharing { viewer_key, .. } = &pin.principal else {
                    return None;
                };
                (pin.credential_hash == hash
                    && viewer_key.as_str() == viewer
                    && pin.request == request
                    && &pin.reference == reference
                    && pin.body_hash == body_hash)
                    .then(|| {
                        pin.registry_boot
                            .map(|boot| (pin.principal.clone(), pin.candidate.clone(), boot))
                    })
                    .flatten()
            })
    }
    fn unpin_assigned(&self, authority: &FreshAuthority) {
        self.fresh
            .lock()
            .expect("Source forwarding candidate pins")
            .retain(|pin| {
                pin.principal != authority.principal || pin.request != authority.request_id
            });
    }
    fn remove(&self, held: &Arc<HeldObligation>) {
        self.held
            .lock()
            .expect("Source outer obligations")
            .retain(|entry| !Arc::ptr_eq(entry, held));
    }
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_node(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn valid_uuid(value: Uuid) -> bool {
    !value.is_nil() && value.get_version_num() == 4
}
fn remaining(deadline: Instant) -> Result<u32, ApiError> {
    u32::try_from(
        deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(unavailable)?
            .as_millis(),
    )
    .ok()
    .filter(|value| *value > 100)
    .ok_or_else(unavailable)
}
fn received_deadline(remaining_ms: u32, budget: Duration) -> Result<Instant, ApiError> {
    if remaining_ms <= 100 || u128::from(remaining_ms) > budget.as_millis() {
        return Err(unavailable());
    }
    Ok(Instant::now() + Duration::from_millis(u64::from(remaining_ms - 100)))
}
fn wire<T: Serialize>(value: &T) -> Result<Vec<u8>, ApiError> {
    serde_json::to_vec(value)
        .ok()
        .filter(|body| body.len() <= MAX_WIRE_BYTES)
        .ok_or_else(invalid)
}
async fn bounded<T: Serialize>(
    state: &AppState,
    owner: &str,
    path: &'static str,
    value: &T,
    deadline: Instant,
) -> Result<crate::http::peer_transport::PeerResponse, ApiError> {
    state
        .media_sessions
        .source_forward_bounded(
            owner,
            path,
            wire(value)?,
            tokio::time::Instant::from_std(deadline),
        )
        .await
        .map_err(|_error| {
            #[cfg(test)]
            eprintln!(
                "Source forwarding RPC {path} transport refused: {}",
                match _error {
                    crate::http::peer_transport::PeerTransportError::Unreachable => "unreachable",
                    crate::http::peer_transport::PeerTransportError::TimedOut => "timed_out",
                    crate::http::peer_transport::PeerTransportError::InvalidResponse =>
                        "invalid_response",
                }
            );
            unavailable()
        })
}
async fn custody(
    state: &AppState,
    held: &HeldObligation,
    action: CustodyAction,
    deadline: Instant,
) -> Result<CustodyReply, ApiError> {
    let path = if matches!(action, CustodyAction::Register) {
        REGISTER_PATH
    } else {
        ACK_PATH
    };
    let request = CustodyRequest {
        authority: held.authority.clone(),
        custody: ForwardCustody {
            ingress: held.ingress.clone(),
            action,
        },
        remaining_ms: remaining(deadline)?,
        hop: 1,
    };
    let response = bounded(
        state,
        &held.authority.owner_node_id,
        path,
        &request,
        deadline,
    )
    .await?;
    if !response.status.is_success() {
        return Err(unavailable());
    }
    serde_json::from_slice(&response.body).map_err(|_| unavailable())
}
async fn acknowledge(
    state: &AppState,
    registry: &ForwardingRegistry,
    held: &Arc<HeldObligation>,
    deadline: Instant,
) -> Result<(), ApiError> {
    let receipt = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        held.obligation.joined(),
    )
    .await
    .map_err(|_| unavailable())?;
    // Another exact Register may have completed after this monitor observed
    // registered=false. A missing pending reservation is never failure/closure
    // proof; the authenticated durable Ack below resolves the actual state.
    let guard = if held.registered.load(Ordering::Acquire) {
        None
    } else {
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            state
                .sharing
                .accepted_drivers
                .reconcile_guard(&held.obligation),
        )
        .await
        .map_err(|_| unavailable())?
        .ok()
    };
    match custody(
        state,
        held,
        CustodyAction::Ack {
            receipt: receipt.clone(),
        },
        deadline,
    )
    .await?
    {
        CustodyReply::Acknowledged | CustodyReply::ReconciledClosed => held
            .obligation
            .release_after_ack(&receipt)
            .map_err(|_| unavailable())?,
        CustodyReply::NeverRegistered => held
            .obligation
            .release_refused_registration()
            .map_err(|_| unavailable())?,
        _ => return Err(unavailable()),
    }
    if let Some(guard) = guard {
        guard.complete();
    }
    registry.remove(held);
    Ok(())
}
async fn reconcile_closed(state: &AppState, owner_identity: Option<&str>, deadline: Instant) {
    let registry = &state.transcode.source_http_starts.forwarding;
    // Exact retry/End repairs only its retained principal. Cross-principal
    // reclamation is admission/cache maintenance, never a per-segment sweep.
    // Only joined observers identify closure; missing rows/timeouts do not.
    let closed: Vec<_> = registry
        .held
        .lock()
        .expect("Source outer obligations")
        .iter()
        .filter(|held| owner_identity.is_none_or(|owner| held.authority.owner_identity == owner))
        .filter(|held| held.obligation.joined().now_or_never().is_some())
        .take(8)
        .cloned()
        .collect();
    for held in closed {
        if remaining(deadline).is_err() {
            break;
        }
        if acknowledge(state, registry, &held, deadline).await.is_err() {
            let mut entries = registry.held.lock().expect("Source outer obligations");
            if let Some(index) = entries.iter().position(|entry| Arc::ptr_eq(entry, &held)) {
                let retry = entries.remove(index);
                entries.push(retry);
            }
        }
    }
}

fn monitor(
    state: &AppState,
    connection: &crate::SharingConnectionCancellation,
    held: &Arc<HeldObligation>,
) -> Result<(), ApiError> {
    if held.monitored.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let state = state.clone();
    let held_copy = Arc::clone(held);
    let registry = state.transcode.source_http_starts.forwarding.clone();
    if connection
        .monitor(async move {
            let _receipt = held_copy.obligation.joined().await;
            if acknowledge(
                &state,
                &registry,
                &held_copy,
                Instant::now() + Duration::from_secs(9),
            )
            .await
            .is_err()
            {
                held_copy.monitored.store(false, Ordering::Release);
            }
        })
        .is_err()
    {
        held.monitored.store(false, Ordering::Release);
        return Err(unavailable());
    }
    Ok(())
}
async fn capture_driver(
    state: &AppState,
    connection: &crate::SharingConnectionCancellation,
    owner: &str,
    deadline: Instant,
) -> Result<crate::sharing_connection_custody::CapturedDriver, ApiError> {
    match state.sharing.accepted_drivers.capture(connection, owner) {
        Ok(driver) => Ok(driver),
        Err(()) => {
            // Capture can hit the physical registry's retained bound before
            // the held-cache admission check. Reclaim only actually closed,
            // durably acknowledged obligations, then retry capture once.
            reconcile_closed(
                state,
                None,
                deadline.min(Instant::now() + Duration::from_secs(1)),
            )
            .await;
            state
                .sharing
                .accepted_drivers
                .capture(connection, owner)
                .map_err(|_| unavailable())
        }
    }
}

async fn register(
    state: &AppState,
    authority: &ForwardAuthority,
    connection: &crate::SharingConnectionCancellation,
    deadline: Instant,
) -> Result<ForwardIngress, ApiError> {
    let registry = &state.transcode.source_http_starts.forwarding;
    let driver = capture_driver(state, connection, &authority.owner_node_id, deadline).await?;
    let existing = registry
        .held
        .lock()
        .expect("Source outer obligations")
        .iter()
        .find(|entry| {
            entry.authority.owner_identity == authority.owner_identity
                && entry.ingress.driver == *driver.id()
        })
        .cloned();
    let (held, guard) = if let Some(held) = existing {
        if held.registered.load(Ordering::Acquire) {
            monitor(state, connection, &held)?;
            return Ok(held.ingress.clone());
        }
        let guard = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            state
                .sharing
                .accepted_drivers
                .reconcile_guard(&held.obligation),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
        (held, guard)
    } else {
        // The common per-principal pending fence refuses an open unknown
        // registration. Event-driven closed-driver reconciliation runs at the
        // route boundary; no new driver or timeout proves old closure.
        reconcile_closed(
            state,
            Some(authority.owner_identity()),
            deadline.min(Instant::now() + Duration::from_secs(1)),
        )
        .await;
        let needs_capacity = registry
            .held
            .lock()
            .expect("Source outer obligations")
            .len()
            >= HELD_MAX;
        if needs_capacity {
            reconcile_closed(
                state,
                None,
                deadline.min(Instant::now() + Duration::from_secs(1)),
            )
            .await;
        }
        let mut guard = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            state.sharing.accepted_drivers.registration_guard(),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
        let mut entries = registry.held.lock().expect("Source outer obligations");
        if entries.len() >= HELD_MAX {
            return Err(unavailable());
        }
        let obligation = driver
            .prepare_obligation(
                &mut guard,
                "source",
                authority.incarnation_id,
                authority.owner_identity(),
            )
            .map_err(|_| unavailable())?;
        let held = Arc::new(HeldObligation {
            authority: authority.clone(),
            ingress: ForwardIngress {
                node_id: state.node_id.clone(),
                driver: driver.id().clone(),
                registration_sequence: Some(obligation.registration_sequence()),
            },
            obligation,
            registered: AtomicBool::new(false),
            monitored: AtomicBool::new(false),
        });
        entries.push(Arc::clone(&held));
        (held, guard)
    };
    monitor(state, connection, &held)?;
    let reply = custody(state, &held, CustodyAction::Register, deadline).await?;
    #[cfg(test)]
    eprintln!(
        "Source forwarding custody Register reply={}",
        match &reply {
            CustodyReply::Registered => "registered",
            CustodyReply::Acknowledged => "acknowledged",
            CustodyReply::NeverRegistered => "never_registered",
            CustodyReply::Unresolved => "unresolved",
            CustodyReply::ReconciledClosed => "reconciled_closed",
        }
    );
    match reply {
        CustodyReply::Registered => {
            held.registered.store(true, Ordering::Release);
            guard.complete();
            Ok(held.ingress.clone())
        }
        CustodyReply::NeverRegistered => {
            held.obligation
                .release_refused_registration()
                .map_err(|_| unavailable())?;
            guard.complete();
            registry.remove(&held);
            Err(unavailable())
        }
        _ => Err(unavailable()), // Lost/unknown answer retains exact reservation.
    }
}

fn end_to_end(headers: &HeaderMap) -> HeaderMap {
    let nominated: Vec<_> = headers
        .get_all(axum::http::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .collect();
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        if !matches!(
            name.as_str(),
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "x-plurx-response-signature"
        ) && !nominated.iter().any(|excluded| excluded == name.as_str())
        {
            result.append(name.clone(), value.clone());
        }
    }
    result
}
// Only a signed exact-target control response may retire routing cache state.
// This produces no admission, driver closure, or principal settlement proof.
fn settled_cleanup_reply(authority: &FreshAuthority, body: &[u8]) -> bool {
    let Ok(reply) = serde_json::from_slice::<ControlReply>(body) else {
        return false;
    };
    if !(200..300).contains(&reply.status) {
        return false;
    }
    let Ok(bytes) = hex::decode(reply.body_hex) else {
        return false;
    };
    if bytes.len() > 24 * 1024 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    value.get("settled").and_then(serde_json::Value::as_bool) == Some(true)
        && value.get("state").and_then(serde_json::Value::as_str) == Some("settled")
        && value.get("reference").is_some_and(|actual| {
            serde_json::to_value(&authority.reference).is_ok_and(|expected| *actual == expected)
        })
        && value.get("request_id").and_then(serde_json::Value::as_str)
            == Some(authority.request_id.to_string().as_str())
        && ["incarnation_id", "confirmation_id"]
            .into_iter()
            .all(|field| {
                value
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .and_then(|raw| Uuid::parse_str(raw).ok())
                    .is_some_and(valid_uuid)
            })
}
fn decode_control(body: &[u8]) -> Result<Response, ApiError> {
    let reply: ControlReply = serde_json::from_slice(body).map_err(|_| unavailable())?;
    let status = StatusCode::from_u16(reply.status).map_err(|_| unavailable())?;
    let bytes = hex::decode(reply.body_hex).map_err(|_| unavailable())?;
    if bytes.len() > 24 * 1024 || reply.headers.len() > 64 {
        return Err(unavailable());
    }
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    for (name, value) in reply.headers {
        let name = axum::http::HeaderName::try_from(name).map_err(|_| unavailable())?;
        let value = axum::http::HeaderValue::try_from(value).map_err(|_| unavailable())?;
        response.headers_mut().append(name, value);
    }
    *response.headers_mut() = end_to_end(response.headers());
    Ok(response)
}
fn stream_response(
    response: reqwest::Response,
    permit: tokio::sync::OwnedSemaphorePermit,
    began: Instant,
) -> Result<Response, ApiError> {
    let status = StatusCode::from_u16(response.status().as_u16()).map_err(|_| unavailable())?;
    let headers = end_to_end(response.headers());
    let deadline = tokio::time::Instant::from_std(
        began + crate::media_sessions::MAX_ADMITTED_MEDIA_BODY_LIFETIME,
    );
    let source = Box::pin(response.bytes_stream());
    let stream = futures_util::stream::unfold(
        (source, permit, false),
        move |(mut source, permit, finished)| async move {
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
                Ok(Some(Ok(bytes))) => Some((Ok(bytes), (source, permit, false))),
                Ok(Some(Err(error))) => {
                    Some((Err(std::io::Error::other(error)), (source, permit, true)))
                }
                Err(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "Source forward body deadline",
                    )),
                    (source, permit, true),
                )),
                Ok(None) => None,
            }
        },
    );
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}
pub(super) async fn route_to_owner(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request
        .extensions()
        .get::<ForwardAuthentication>()
        .is_some()
    {
        return next.run(request).await;
    }
    match route(state, request, next).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn route(state: AppState, request: Request<Body>, next: Next) -> Result<Response, ApiError> {
    let began = Instant::now();
    let (parts, body) = request.into_parts();
    if parts.method != axum::http::Method::POST || parts.uri.query().is_some() {
        return Err(invalid());
    }
    let path = parts.uri.path().to_owned();
    let bytes = axum::body::to_bytes(body, MAX_PUBLIC_BODY)
        .await
        .map_err(|_| invalid())?;
    let op = operation(&path, &bytes)?;
    let permit = credit(op)?;
    let deadline = began + op.budget();
    let headers = SourceHeaders::from(parts.headers.clone());
    let connection = parts
        .extensions
        .get::<crate::SharingConnectionCancellation>()
        .cloned()
        .ok_or_else(unavailable)?;
    let route = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        super::resolve_forward_route(&state, &headers, &path, &bytes, &connection, deadline),
    )
    .await
    .map_err(|_| {
        #[cfg(test)]
        eprintln!("Source forwarding route resolution timed out");
        unavailable()
    })?
    .inspect_err(|_error| {
        #[cfg(test)]
        eprintln!("Source forwarding route resolution refused");
    })?;
    #[cfg(test)]
    eprintln!("Source forwarding route resolution succeeded");
    if matches!(route, ForwardRoute::Local) {
        return Ok(next
            .run(Request::from_parts(parts, Body::from(bytes)))
            .await);
    }
    // A member envelope never carries the sharing bearer credential or public
    // headers. The untouched complete recipe remains in the original body.
    let canonical_body = String::from_utf8(bytes.to_vec()).map_err(|_| invalid())?;
    if !remote_operation_allowed(&route, op) {
        return Err(unavailable());
    }
    #[cfg(test)]
    eprintln!(
        "Source forwarding route resolved: {}",
        match &route {
            ForwardRoute::Fresh(_) => "fresh",
            ForwardRoute::Pending(_) => "pending",
            ForwardRoute::Retained(_) => "retained",
            ForwardRoute::Local => "local",
        }
    );
    let fresh_choice = matches!(&route, ForwardRoute::Fresh(_));
    let authority = match route {
        ForwardRoute::Retained(authority) => *authority,
        ForwardRoute::Fresh(mut fresh) | ForwardRoute::Pending(mut fresh) => {
            if !fresh_choice && fresh.expected_registry_boot.is_none() {
                return Err(unavailable());
            }
            if !matches!(op, Operation::Start | Operation::Status | Operation::End) {
                return Err(unavailable());
            }
            if fresh_choice {
                state
                    .transcode
                    .source_http_starts
                    .forwarding
                    .pin(&mut fresh, &routing_recipe(&path, &bytes)?)?;
            }
            let driver =
                capture_driver(&state, &connection, &fresh.candidate_node_id, deadline).await?;
            let prepared = bounded(
                &state,
                &fresh.candidate_node_id,
                PREPARE_PATH,
                &FreshRequest {
                    path: path.clone(),
                    canonical_body: canonical_body.clone(),
                    operation: op,
                    ingress: ForwardIngress {
                        node_id: state.node_id.clone(),
                        driver: driver.id().clone(),
                        registration_sequence: None,
                    },
                    authority: (*fresh).clone(),
                    remaining_ms: remaining(deadline)?,
                    hop: 1,
                },
                deadline,
            )
            .await?;
            #[cfg(test)]
            eprintln!(
                "Source forwarding prepare reply status={} bytes={}",
                prepared.status,
                prepared.body.len()
            );
            if op != Operation::Start || !prepared.status.is_success() {
                let response = decode_control(&prepared.body)?;
                if op == Operation::End
                    && prepared.status.is_success()
                    && settled_cleanup_reply(&fresh, &prepared.body)
                {
                    state
                        .transcode
                        .source_http_starts
                        .forwarding
                        .unpin_assigned(&fresh);
                }
                return Ok(response);
            }
            let assigned: ForwardAuthority =
                serde_json::from_slice(&prepared.body).map_err(|_| unavailable())?;
            if assigned.owner_node_id != fresh.candidate_node_id
                || assigned.principal != fresh.principal
                || assigned.credential_hash != fresh.credential_hash
                || assigned.reference != fresh.reference
                || assigned.request_id != fresh.request_id
                || fresh
                    .expected_registry_boot
                    .is_some_and(|boot| assigned.expected_registry_boot != boot)
            {
                return Err(unavailable());
            }
            state
                .transcode
                .source_http_starts
                .forwarding
                .unpin_assigned(&fresh);
            assigned
        }
        ForwardRoute::Local => return Err(unavailable()),
    };
    if !authority.valid()
        || (authority.cleanup_only && !matches!(op, Operation::Status | Operation::End))
    {
        return Err(unavailable());
    }
    if matches!(op, Operation::Start | Operation::Status | Operation::End) {
        reconcile_closed(
            &state,
            Some(authority.owner_identity()),
            deadline.min(Instant::now() + Duration::from_secs(1)),
        )
        .await;
    }
    #[cfg(test)]
    eprintln!("Source forwarding assignment validated; registering protected ingress");
    let ingress = if op == Operation::End || (op == Operation::Status && authority.cleanup_only) {
        // End never admits another media debt after seal. The actual driver
        // tuple lets the retained End owner recognize its own response cycle.
        let driver =
            capture_driver(&state, &connection, &authority.owner_node_id, deadline).await?;
        ForwardIngress {
            node_id: state.node_id.clone(),
            driver: driver.id().clone(),
            registration_sequence: None,
        }
    } else {
        register(&state, &authority, &connection, deadline).await?
    };
    #[cfg(test)]
    eprintln!("Source forwarding ingress registered; dispatching owner operation");
    let wire = ForwardRequest {
        path,
        canonical_body,
        operation: op,
        authority: authority.clone(),
        ingress,
        remaining_ms: remaining(deadline)?,
        hop: 1,
    };
    if op.media() {
        let response = state
            .media_sessions
            .source_forward_stream(
                &authority.owner_node_id,
                self::wire(&wire)?,
                tokio::time::Instant::from_std(deadline),
            )
            .await
            .map_err(|_| unavailable())?;
        stream_response(response, permit, began)
    } else {
        let response = bounded(
            &state,
            &authority.owner_node_id,
            CONTROL_PATH,
            &wire,
            deadline,
        )
        .await?;
        if !response.status.is_success() {
            return Err(unavailable());
        }
        decode_control(&response.body)
    }
}
async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    path: &'static str,
    body: &[u8],
) -> Result<plurx_core::cluster::membership::InternalPeerAuth, ApiError> {
    let auth = crate::http::peer_transport::exact_auth_from_headers(headers)
        .ok_or(ApiError::Unauthorized)?;
    if body.len() > MAX_WIRE_BYTES
        || !matches!(
            tokio::time::timeout(
                Duration::from_secs(9),
                state
                    .membership
                    .authorize_internal_peer_request(&auth, "POST", path, body)
            )
            .await,
            Ok(Ok(true))
        )
    {
        #[cfg(test)]
        eprintln!("Source internal authentication refused path={path}");
        return Err(ApiError::Unauthorized);
    }
    Ok(auth)
}
async fn signed(
    state: &AppState,
    auth: &plurx_core::cluster::membership::InternalPeerAuth,
    path: &'static str,
    status: StatusCode,
    body: Vec<u8>,
    deadline: Instant,
) -> Result<Response, ApiError> {
    if body.len() > MAX_REPLY_BYTES {
        return Err(unavailable());
    }
    let payload = crate::http::peer_transport::signed_response_payload(status.as_u16(), &body);
    let signature =
        state
            .membership
            .sign_internal_peer_response(&auth.node_id, &auth.nonce, path, &payload);
    let signature = signature.map_err(|_| unavailable())?;
    remaining(deadline)?;
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header(
            crate::http::peer_transport::RESPONSE_SIGNATURE_HEADER,
            signature,
        )
        .body(Body::from(body))
        .map_err(|_| unavailable())
}
async fn pack_control(response: Response, deadline: Instant) -> Result<Vec<u8>, ApiError> {
    let (parts, body) = response.into_parts();
    let bytes = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        axum::body::to_bytes(body, 24 * 1024),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    let headers: Vec<_> = end_to_end(&parts.headers)
        .iter()
        .map(|(name, value)| {
            Ok((
                name.to_string(),
                value.to_str().map_err(|_| unavailable())?.to_owned(),
            ))
        })
        .collect::<Result<_, ApiError>>()?;
    if headers.len() > 64 {
        return Err(unavailable());
    }
    let reply = ControlReply {
        status: parts.status.as_u16(),
        headers,
        body_hex: hex::encode(bytes),
    };
    serde_json::to_vec(&reply)
        .ok()
        .filter(|body| body.len() <= MAX_REPLY_BYTES)
        .ok_or_else(unavailable)
}
pub(crate) async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: Bytes,
) -> Response {
    receive_at(
        state,
        headers,
        connection.map(|value| value.0),
        body,
        FORWARD_PATH,
    )
    .await
    .unwrap_or_else(|error| error.into_response())
}
pub(crate) async fn receive_control(
    State(state): State<AppState>,
    headers: HeaderMap,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    body: Bytes,
) -> Response {
    receive_at(
        state,
        headers,
        connection.map(|value| value.0),
        body,
        CONTROL_PATH,
    )
    .await
    .unwrap_or_else(|error| error.into_response())
}
async fn receive_at(
    state: AppState,
    headers: HeaderMap,
    connection: Option<crate::SharingConnectionCancellation>,
    body: Bytes,
    path: &'static str,
) -> Result<Response, ApiError> {
    let began = Instant::now();
    let auth = authenticate(&state, &headers, path, &body).await?;
    let wire: ForwardRequest = serde_json::from_slice(&body).map_err(|_| invalid())?;
    let op = operation(&wire.path, wire.canonical_body.as_bytes())?;
    let permit = credit(op)?;
    if !wire.valid_transport(&state.node_id, &auth.node_id, path, op) {
        return Err(unavailable());
    }
    let deadline = received_deadline(wire.remaining_ms, op.budget())?
        .min(began + Duration::from_millis(u64::from(wire.remaining_ms - 100)));
    remaining(deadline)?;
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        super::validate_retained_forward(
            &state,
            &wire.authority,
            &wire.ingress,
            &wire.path,
            wire.canonical_body.as_bytes(),
            deadline,
        ),
    )
    .await
    .map_err(|_| unavailable())??;
    let mut request = Request::builder()
        .method("POST")
        .uri(&wire.path)
        .body(Body::from(wire.canonical_body))
        .map_err(|_| invalid())?;
    request.extensions_mut().insert(ForwardAuthentication {
        authority: wire.authority,
        ingress: wire.ingress,
        deadline,
    });
    // The parent separately captures/registers this actual internal writer.
    // Its closure cannot substitute for the outer accepted driver's closure.
    let connection = connection.ok_or_else(unavailable)?;
    request.extensions_mut().insert(connection);
    let response = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        super::peer_router(state.clone())
            .with_state(state.clone())
            .oneshot(request),
    )
    .await
    .map_err(|_| unavailable())?
    .unwrap_or_else(|never| match never {});
    if op.media() {
        let (parts, body) = response.into_parts();
        let stream = body.into_data_stream().map(move |chunk| {
            let _held_credit = &permit;
            chunk
        });
        Ok(Response::from_parts(parts, Body::from_stream(stream)))
    } else {
        signed(
            &state,
            &auth,
            path,
            StatusCode::OK,
            pack_control(response, deadline).await?,
            deadline,
        )
        .await
    }
}
pub(crate) async fn prepare(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    prepare_inner(state, headers, body)
        .await
        .unwrap_or_else(|error| error.into_response())
}
async fn prepare_inner(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let began = Instant::now();
    let auth = authenticate(&state, &headers, PREPARE_PATH, &body).await?;
    let wire: FreshRequest = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if wire.hop != 1
        || wire.authority.candidate_node_id != state.node_id
        || !wire.authority.valid()
        || !wire.ingress.valid()
        || wire.ingress.node_id != auth.node_id
        || wire.ingress.registration_sequence.is_some()
        || operation(&wire.path, wire.canonical_body.as_bytes())? != wire.operation
        || !matches!(
            wire.operation,
            Operation::Start | Operation::Status | Operation::End
        )
    {
        return Err(unavailable());
    }
    let _permit = credit(wire.operation)?;
    let deadline = received_deadline(wire.remaining_ms, wire.operation.budget())?
        .min(began + Duration::from_millis(u64::from(wire.remaining_ms - 100)));
    remaining(deadline)?;
    if wire.operation != Operation::Start {
        let response = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            super::execute_forward_unassigned_cleanup(
                &state,
                &wire.authority,
                &wire.ingress,
                &wire.path,
                wire.canonical_body.as_bytes(),
                deadline,
            ),
        )
        .await
        .map_err(|_| unavailable())??;
        return signed(
            &state,
            &auth,
            PREPARE_PATH,
            StatusCode::OK,
            pack_control(response, deadline).await?,
            deadline,
        )
        .await;
    }
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        super::prepare_forward_start(
            &state,
            &wire.authority,
            wire.canonical_body.as_bytes(),
            deadline,
        ),
    )
    .await
    .map_err(|_| unavailable())?
    {
        Ok(assignment) => {
            signed(
                &state,
                &auth,
                PREPARE_PATH,
                StatusCode::OK,
                self::wire(&assignment)?,
                deadline,
            )
            .await
        }
        Err(error) => {
            signed(
                &state,
                &auth,
                PREPARE_PATH,
                StatusCode::CONFLICT,
                pack_control(error.into_response(), deadline).await?,
                deadline,
            )
            .await
        }
    }
}
pub(crate) async fn register_http(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    custody_inner(state, headers, body, REGISTER_PATH)
        .await
        .unwrap_or_else(|error| error.into_response())
}
pub(crate) async fn ack_http(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    custody_inner(state, headers, body, ACK_PATH)
        .await
        .unwrap_or_else(|error| error.into_response())
}
async fn custody_inner(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
    path: &'static str,
) -> Result<Response, ApiError> {
    let began = Instant::now();
    let auth = authenticate(&state, &headers, path, &body).await?;
    let wire: CustodyRequest = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if wire.hop != 1
        || !wire.authority.valid()
        || wire.authority.owner_node_id != state.node_id
        || !wire.custody.ingress.valid()
        || wire.custody.ingress.registration_sequence.is_none()
        || wire.custody.ingress.node_id != auth.node_id
        || matches!(wire.custody.action, CustodyAction::Register) != (path == REGISTER_PATH)
    {
        return Err(unavailable());
    }
    if let CustodyAction::Ack { receipt } = &wire.custody.action {
        if !receipt.matches(&wire.custody.ingress.driver) {
            return Err(unavailable());
        }
    }
    let _permit = credit(if matches!(wire.custody.action, CustodyAction::Register) {
        Operation::Control
    } else {
        Operation::End
    })?;
    let deadline = received_deadline(wire.remaining_ms, Operation::Start.budget())?
        .min(began + Duration::from_millis(u64::from(wire.remaining_ms - 100)));
    remaining(deadline)?;
    let result = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        super::apply_forward_custody(&state, &wire.authority, &wire.custody, deadline),
    )
    .await
    .map_err(|_| unavailable())??;
    signed(
        &state,
        &auth,
        path,
        StatusCode::OK,
        self::wire(&result)?,
        deadline,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_headers_cannot_construct_forwarded_authority() {
        let mut raw = HeaderMap::new();
        raw.insert("x-plurx-cluster-node", "owner".parse().expect("header"));
        raw.insert(
            "cinemashare-viewer",
            "a".repeat(64).parse().expect("header"),
        );
        let headers = SourceHeaders::from(raw);
        assert!(headers.authenticated_hash().is_none());
        assert!(headers.authenticated_principal().is_none());
        assert!(headers.forwarded_ingress().is_none());
        assert!(!headers.cleanup_only());
    }
    #[test]
    fn forwarding_deadline_refuses_expansion_and_reserves_transit_credit() {
        assert!(received_deadline(9010, Duration::from_secs(9)).is_err());
        assert!(received_deadline(100, Duration::from_secs(9)).is_err());
        let began = Instant::now();
        let deadline = received_deadline(5000, Duration::from_secs(9)).expect("bounded remaining");
        assert!(deadline < began + Duration::from_secs(5));
    }
    #[test]
    fn forward_headers_preserve_direct_head_range_contract() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("content-range", "bytes 0-9/100"),
            ("accept-ranges", "bytes"),
            ("content-length", "0"),
            ("cinemashare-content-length", "10"),
            ("cinemashare-session-id", "retained"),
            ("content-type", "video/mp4"),
            ("etag", "tag"),
            ("connection", "x-hop"),
            ("x-hop", "discard"),
        ] {
            headers.insert(
                axum::http::HeaderName::from_static(name),
                value.parse().expect("header"),
            );
        }
        let forwarded = end_to_end(&headers);
        for name in [
            "content-range",
            "accept-ranges",
            "content-length",
            "cinemashare-content-length",
            "cinemashare-session-id",
            "content-type",
            "etag",
        ] {
            assert_eq!(forwarded.get(name), headers.get(name));
        }
        assert!(!forwarded.contains_key("connection"));
        assert!(!forwarded.contains_key("x-hop"));
    }
    pub(super) fn fresh() -> FreshAuthority {
        FreshAuthority {
            credential_hash: "a".repeat(64),
            principal: PlaybackPrincipal::sharing(Uuid::new_v4(), &"b".repeat(64))
                .expect("principal"),
            candidate_node_id: "worker-a".into(),
            expected_registry_boot: None,
            reference: SourcePlaybackTarget {
                server_id: Uuid::new_v4(),
                catalogue_epoch: Uuid::new_v4(),
                library_id: plurx_core::sharing::SourceId::parse("1").expect("library"),
                item_id: plurx_core::sharing::SourceId::parse("2").expect("item"),
                file_id: plurx_core::sharing::SourceId::parse("3").expect("file"),
                revision: plurx_core::sharing_catalogue_details::FileRevision::parse(
                    &"c".repeat(64),
                )
                .expect("revision"),
            },
            request_id: Uuid::new_v4(),
        }
    }
    fn ingress() -> ForwardIngress {
        ForwardIngress {
            node_id: "ingress".into(),
            driver: AcceptedDriverId {
                boot_id: Uuid::new_v4(),
                connection_id: Uuid::new_v4(),
                driver_sequence: 7,
            },
            registration_sequence: Some(11),
        }
    }
    #[test]
    fn uncertain_fresh_candidate_stays_pinned_without_assignment_authority() {
        let registry = ForwardingRegistry::default();
        let mut authority = fresh();
        let original_boot = Uuid::new_v4();
        authority.expected_registry_boot = Some(original_boot);
        registry
            .pin(&mut authority, b"canonical complete recipe")
            .expect("initial pin");
        authority.candidate_node_id = "worker-b".into();
        authority.expected_registry_boot = Some(Uuid::new_v4());
        registry
            .pin(&mut authority, b"canonical complete recipe")
            .expect("exact retry");
        assert_eq!(authority.candidate_node_id, "worker-a");
        assert_eq!(authority.expected_registry_boot, Some(original_boot));
        let cleanup = registry
            .pending_cleanup_route(
                &authority.credential_hash,
                &"b".repeat(64),
                authority.request_id,
                &authority.reference,
                b"canonical complete recipe",
            )
            .expect("exact original cleanup pin");
        assert_eq!((cleanup.1, cleanup.2), ("worker-a".into(), original_boot));
        assert_eq!(
            registry.pending_candidate(
                &authority.principal,
                authority.request_id,
                &authority.reference,
                b"canonical complete recipe"
            ),
            Some(("worker-a".into(), Some(original_boot)))
        );
        assert!(registry
            .pending_candidate(
                &authority.principal,
                Uuid::new_v4(),
                &authority.reference,
                b"canonical complete recipe"
            )
            .is_none());
        assert!(registry.pin(&mut authority, b"changed recipe").is_err());
        let value = serde_json::to_value(&authority).expect("metadata wire");
        for forbidden in ["incarnation_id", "dispatch_generation", "owner_identity"] {
            assert!(value.get(forbidden).is_none());
        }
    }
    #[test]
    fn retained_forward_refuses_second_hop_and_end_new_custody() {
        let fresh = fresh();
        let authority = ForwardAuthority {
            credential_hash: fresh.credential_hash,
            principal: fresh.principal,
            owner_node_id: fresh.candidate_node_id,
            incarnation_id: Uuid::new_v4(),
            dispatch_generation: 1,
            expected_registry_boot: Uuid::new_v4(),
            reference: fresh.reference,
            request_id: fresh.request_id,
            request_fingerprint: "d".repeat(64),
            playback_id: "player".into(),
            owner_identity: "e".repeat(64),
            cleanup_only: false,
        };
        let mut request = ForwardRequest {
            path: "not parsed by transport test".into(),
            canonical_body: "{}".into(),
            operation: Operation::Resources,
            authority,
            ingress: ingress(),
            remaining_ms: 5000,
            hop: 1,
        };
        assert!(request.valid_transport("worker-a", "ingress", FORWARD_PATH, Operation::Resources));
        request.hop = 2;
        assert!(!request.valid_transport(
            "worker-a",
            "ingress",
            FORWARD_PATH,
            Operation::Resources
        ));
        request.hop = 1;
        request.operation = Operation::End;
        assert!(!request.valid_transport("worker-a", "ingress", CONTROL_PATH, Operation::End));
        request.ingress.registration_sequence = None;
        assert!(request.valid_transport("worker-a", "ingress", CONTROL_PATH, Operation::End));
        request.operation = Operation::Status;
        assert!(!request.valid_transport("worker-a", "ingress", CONTROL_PATH, Operation::Status));
        request.authority.cleanup_only = true;
        assert!(request.valid_transport("worker-a", "ingress", CONTROL_PATH, Operation::Status));
        request.operation = Operation::Control;
        request.ingress.registration_sequence = Some(11);
        assert!(!request.valid_transport("worker-a", "ingress", CONTROL_PATH, Operation::Control));
    }
    #[test]
    fn fresh_cleanup_refuses_locality_fallback_but_exact_pending_owner_routes() {
        let authority = fresh();
        let fresh_route = ForwardRoute::Fresh(Box::new(authority.clone()));
        assert!(remote_operation_allowed(&fresh_route, Operation::Start));
        assert!(!remote_operation_allowed(&fresh_route, Operation::End));
        assert!(!remote_operation_allowed(&fresh_route, Operation::Status));
        let missing_boot = ForwardRoute::Pending(Box::new(authority.clone()));
        assert!(!remote_operation_allowed(&missing_boot, Operation::End));
        let mut authority = authority;
        authority.expected_registry_boot = Some(Uuid::new_v4());
        let pending = ForwardRoute::Pending(Box::new(authority));
        assert!(remote_operation_allowed(&pending, Operation::End));
        assert!(remote_operation_allowed(&pending, Operation::Status));
        assert!(!remote_operation_allowed(&pending, Operation::Resources));
    }
    #[tokio::test]
    async fn signed_control_projection_preserves_complete_body_and_headers() {
        let original = Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header("content-length", "3")
            .header("content-range", "bytes 0-2/3")
            .header("cinemashare-control-epoch", "17")
            .body(Body::from(vec![0u8, 128, 255]))
            .expect("bounded response");
        let wire = pack_control(original, Instant::now() + Duration::from_secs(1))
            .await
            .expect("projection");
        let response = decode_control(&wire).expect("signed projection decode");
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["content-range"], "bytes 0-2/3");
        assert_eq!(response.headers()["cinemashare-control-epoch"], "17");
        let body = axum::body::to_bytes(response.into_body(), 3)
            .await
            .expect("unchanged bytes");
        assert_eq!(body.as_ref(), &[0u8, 128, 255]);
    }
    #[test]
    fn held_media_and_fresh_control_cannot_starve_end_or_ack_capacity() {
        let media = Arc::new(tokio::sync::Semaphore::new(1));
        let control = Arc::new(tokio::sync::Semaphore::new(1));
        let cleanup = Arc::new(tokio::sync::Semaphore::new(2));
        let _body = credit_from(Operation::Direct, &media, &control, &cleanup).expect("held body");
        let _fresh =
            credit_from(Operation::Start, &media, &control, &cleanup).expect("fresh prepare");
        assert!(credit_from(Operation::Resources, &media, &control, &cleanup).is_err());
        assert!(credit_from(Operation::Control, &media, &control, &cleanup).is_err());
        let _end = credit_from(Operation::End, &media, &control, &cleanup).expect("End cleanup");
        let _ack = credit_from(Operation::End, &media, &control, &cleanup).expect("Ack cleanup");
        assert!(credit_from(Operation::End, &media, &control, &cleanup).is_err());
    }
    #[tokio::test]
    async fn bounded_control_response_refuses_oversize_and_expired_deadline() {
        let response = Response::new(Body::from(vec![0u8; 24 * 1024 + 1]));
        assert!(
            pack_control(response, Instant::now() + Duration::from_secs(1))
                .await
                .is_err()
        );
        assert!(remaining(Instant::now()).is_err());
        assert!(received_deadline(u32::MAX, Duration::from_secs(305)).is_err());
        let headers = SourceHeaders::from(HeaderMap::new());
        assert!(headers.deadline(Duration::from_secs(1)) < Instant::now() + Duration::from_secs(2));
    }
    #[test]
    fn internal_envelope_refuses_unknown_fields() {
        let request = FreshRequest {
            path: "not parsed by serde test".into(),
            canonical_body: "{}".into(),
            operation: Operation::Start,
            authority: fresh(),
            ingress: ingress(),
            remaining_ms: 5000,
            hop: 1,
        };
        let mut value = serde_json::to_value(request).expect("valid envelope");
        assert!(serde_json::from_value::<FreshRequest>(value.clone()).is_ok());
        value
            .as_object_mut()
            .expect("object")
            .insert("extra".into(), serde_json::json!("public header"));
        assert!(serde_json::from_value::<FreshRequest>(value).is_err());
        assert!(!valid_hash(&"A".repeat(64)));
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocateEnvelope {
    hop: u8,
    remaining_ms: u32,
    authority: FreshAuthority,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocationReply {
    node_id: String,
    registry_boot_id: Uuid,
    reference: SourcePlaybackTarget,
    available: bool,
    size: Option<i64>,
    mtime: Option<i64>,
}
impl LocationReply {
    fn exact(&self, node: &str, reference: &SourcePlaybackTarget) -> bool {
        self.node_id == node
            && &self.reference == reference
            && self.registry_boot_id.get_version_num() == 4
            && self.registry_boot_id.get_variant() == uuid::Variant::RFC4122
            && match (self.available, self.size, self.mtime) {
                (true, Some(size), Some(mtime)) => size >= 0 && mtime >= 0,
                (false, None, None) => true,
                _ => false,
            }
    }
}

/// Cold routing hints only. A timeout may try another read-only peer before
/// any claim exists; once FreshPrepare is sent, its original candidate remains
/// pinned and this collector must never be used to reconstruct its owner.
pub(super) async fn collect_fresh_location(
    state: &AppState,
    principal: &PlaybackPrincipal,
    credential_hash: &str,
    reference: &SourcePlaybackTarget,
    request_id: Uuid,
    deadline: Instant,
) -> Result<Option<(String, Uuid)>, ApiError> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(9));
    let mut peers = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state.membership.media_peers(),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    peers
        .retain(|peer| peer.node_id != state.node_id && peer.reachable && peer.http_base.is_some());
    peers.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    peers.dedup_by(|a, b| a.node_id == b.node_id);
    if peers.len() > 64 {
        return Err(unavailable());
    }
    // Every current peer starts before we await any one of them. A blackhole
    // cannot conceal a later eligible node. The awaited stream owns all finite
    // network futures; dropping it cancels them without a detached task.
    #[cfg(test)]
    eprintln!("Source locate current eligible peers={}", peers.len());
    let mut probes = futures_util::stream::FuturesUnordered::new();
    for peer in peers {
        let wire = LocateEnvelope {
            hop: 1,
            remaining_ms: remaining(deadline)?,
            authority: FreshAuthority {
                credential_hash: credential_hash.to_owned(),
                principal: principal.clone(),
                candidate_node_id: peer.node_id.clone(),
                expected_registry_boot: None,
                reference: reference.clone(),
                request_id,
            },
        };
        probes.push(async move {
            let response = bounded(state, &peer.node_id, LOCATE_PATH, &wire, deadline)
                .await
                .ok()?;
            #[cfg(test)]
            eprintln!(
                "Source locate RPC reply status={} bytes={}",
                response.status,
                response.body.len()
            );
            if response.status != StatusCode::OK || response.body.len() > 8192 {
                return None;
            }
            let reply = serde_json::from_slice::<LocationReply>(&response.body).ok()?;
            #[cfg(test)]
            eprintln!(
                "Source locate reply exact={} available={}",
                reply.exact(&peer.node_id, reference),
                reply.available
            );
            (reply.exact(&peer.node_id, reference) && reply.available)
                .then_some((reply.node_id, reply.registry_boot_id))
        });
    }
    let mut available = Vec::new();
    while let Some(result) = probes.next().await {
        if let Some(location) = result {
            available.push(location);
        }
    }
    available.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(available.into_iter().next())
}

pub(crate) async fn locate(
    State(state): State<AppState>,
    connection: Option<axum::Extension<crate::SharingConnectionCancellation>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    match locate_received(
        &state,
        connection.as_ref().map(|connection| &connection.0),
        &headers,
        &body,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn locate_received(
    state: &AppState,
    connection: Option<&crate::SharingConnectionCancellation>,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Response, ApiError> {
    let began = Instant::now();
    if body.is_empty() || body.len() > 16 * 1024 {
        return Err(invalid());
    }
    let wire: LocateEnvelope = serde_json::from_slice(body).map_err(|_| invalid())?;
    if wire.hop != 1 || !wire.authority.valid() || wire.authority.candidate_node_id != state.node_id
    {
        return Err(invalid());
    }
    let deadline = received_deadline(wire.remaining_ms, Duration::from_secs(9))?
        .min(began + Duration::from_millis(u64::from(wire.remaining_ms - 100)));
    let auth = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        authenticate(state, headers, LOCATE_PATH, body),
    )
    .await
    .map_err(|_| unavailable())??;
    let permit = credit(Operation::Start)?;
    let observation = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        super::source_forward_locality(
            state,
            &wire.authority,
            connection.ok_or_else(unavailable)?,
            deadline,
        ),
    )
    .await
    .map_err(|_| unavailable())??;
    let (available, size, mtime) = match observation {
        Some((size, mtime)) if size >= 0 && mtime >= 0 => (true, Some(size), Some(mtime)),
        Some(_) => return Err(unavailable()),
        None => (false, None, None),
    };
    let reply = LocationReply {
        node_id: state.node_id.clone(),
        registry_boot_id: state.sharing.accepted_drivers.boot_id(),
        reference: wire.authority.reference,
        available,
        size,
        mtime,
    };
    let body = serde_json::to_vec(&reply).map_err(|_| unavailable())?;
    if body.len() > 8192 {
        return Err(unavailable());
    }
    let response = signed(state, &auth, LOCATE_PATH, StatusCode::OK, body, deadline).await;
    drop(permit);
    response
}

#[cfg(test)]
mod locality_tests {
    use super::*;
    #[test]
    fn source_location_reply_requires_signed_target_full_reference_and_complete_file_evidence() {
        let authority = super::tests::fresh();
        let mut reply = LocationReply {
            node_id: authority.candidate_node_id.clone(),
            registry_boot_id: Uuid::new_v4(),
            reference: authority.reference.clone(),
            available: true,
            size: Some(100),
            mtime: Some(1000),
        };
        assert!(reply.exact(&authority.candidate_node_id, &authority.reference));
        reply.size = None;
        assert!(!reply.exact(&authority.candidate_node_id, &authority.reference));
        reply.size = Some(-1);
        assert!(!reply.exact(&authority.candidate_node_id, &authority.reference));
        reply.size = Some(100);
        let different = super::tests::fresh().reference;
        let mut wrong_reference = different.clone();
        wrong_reference.item_id =
            plurx_core::sharing::SourceId::parse("999").expect("bounded test item");
        assert!(!reply.exact(&authority.candidate_node_id, &wrong_reference));
        reply.node_id = "different-peer".into();
        assert!(!reply.exact(&authority.candidate_node_id, &authority.reference));
    }
    #[test]
    fn authenticated_exact_settled_end_reclaims_only_its_bounded_routing_pin() {
        use super::tests::fresh;
        let registry = ForwardingRegistry::default();
        let mut first = fresh();
        first.expected_registry_boot = Some(Uuid::new_v4());
        registry.pin(&mut first, b"recipe").expect("first pin");
        for _ in 1..HELD_MAX {
            let mut next = fresh();
            next.expected_registry_boot = Some(Uuid::new_v4());
            registry
                .pin(&mut next, b"recipe")
                .expect("bounded pending pin");
        }
        let mut overflow = fresh();
        assert!(registry.pin(&mut overflow, b"recipe").is_err());
        let mut receipt = serde_json::json!({"reference": first.reference,
            "request_id": first.request_id,"incarnation_id":Uuid::new_v4(),
            "confirmation_id":Uuid::new_v4(),"state":"settled","settled":true});
        let control = |value: &serde_json::Value| {
            serde_json::to_vec(&ControlReply {
                status: 200,
                headers: Vec::new(),
                body_hex: hex::encode(serde_json::to_vec(value).expect("receipt JSON")),
            })
            .expect("signed control payload fixture")
        };
        assert!(settled_cleanup_reply(&first, &control(&receipt)));
        receipt["request_id"] = serde_json::json!(Uuid::new_v4());
        assert!(!settled_cleanup_reply(&first, &control(&receipt)));
        assert_eq!(
            registry.fresh.lock().expect("pins").len(),
            HELD_MAX,
            "mismatch/unknown cannot reclaim routing authority"
        );
        receipt["request_id"] = serde_json::json!(first.request_id);
        if settled_cleanup_reply(&first, &control(&receipt)) {
            registry.unpin_assigned(&first);
        }
        registry
            .pin(&mut overflow, b"recipe")
            .expect("settled exact cache slot reclaimed");
        assert_eq!(registry.fresh.lock().expect("pins").len(), HELD_MAX);
    }
}
