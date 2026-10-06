//! Node-local custody of actual accepted writers, shared by Source and B.
//! A restart, missing registry entry, request timeout or requested cancellation
//! never constructs closure. The accepted driver alone signals its observer.
use crate::{SharingConnectionCancellation, SharingConnectionClosure};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) const CLOSE_PATH: &str = "/internal/media-sessions/sharing-ingress-close";
const ACTIVE_DRIVERS_MAX: usize = 256;
const RETAINED_DRIVERS_MAX: usize = 512;
pub(crate) fn owner_identity_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AcceptedDriverId {
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    pub boot_id: Uuid,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    pub connection_id: Uuid,
    pub driver_sequence: u64,
}
impl AcceptedDriverId {
    pub(crate) fn valid(&self) -> bool {
        [self.boot_id, self.connection_id]
            .iter()
            .all(|id| !id.is_nil() && id.get_version_num() == 4)
            && (1..=9_007_199_254_740_991).contains(&self.driver_sequence)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DriverCloseMode {
    Drain,
    Revoke,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DriverCloseRequest {
    pub driver: AcceptedDriverId,
    pub registration_sequence: u64,
    pub principal_kind: String,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    pub incarnation_id: Uuid,
    pub owner_identity: String,
    pub expected_owner_epoch: i64,
    pub mode: DriverCloseMode,
    pub deadline_unix_ms: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DriverClosureReceipt {
    driver: AcceptedDriverId,
    confirmation: String,
}
impl DriverClosureReceipt {
    fn closed(driver: AcceptedDriverId) -> Self {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"plurx.sharing.actual-ingress-driver-closure.v1\0");
        digest.update(driver.boot_id.as_bytes());
        digest.update(driver.connection_id.as_bytes());
        digest.update(driver.driver_sequence.to_be_bytes());
        Self {
            driver,
            confirmation: format!("{:x}", digest.finalize()),
        }
    }
    pub(crate) fn matches(&self, expected: &AcceptedDriverId) -> bool {
        self.driver == *expected && self.confirmation == Self::closed(expected.clone()).confirmation
    }
    #[allow(dead_code)] // Complete physical-receipt accessor; Source/B durable-ack adapters integrate next.
    pub(crate) fn confirmation(&self) -> &str {
        &self.confirmation
    }
}
struct DriverEntry {
    id: AcceptedDriverId,
    owner_key: Weak<dyn Send + Sync>,
    allowed_owners: Mutex<Vec<String>>,
    cancel: CancellationToken,
    drain: CancellationToken,
    closed: SharingConnectionClosure,
    obligations: Mutex<Vec<PrincipalObligation>>,
    next_registration: Arc<AtomicU64>,
}
#[derive(Clone, PartialEq, Eq)]
struct PrincipalObligation {
    kind: String,
    incarnation: Uuid,
    owner_identity: String,
    acknowledged: bool,
    registration_sequence: u64,
}
#[derive(Clone, PartialEq, Eq)]
struct PrincipalObligationKey {
    kind: String,
    incarnation: Uuid,
    owner_identity: String,
    registration_sequence: u64,
}
impl From<&PrincipalObligation> for PrincipalObligationKey {
    fn from(value: &PrincipalObligation) -> Self {
        Self {
            kind: value.kind.clone(),
            incarnation: value.incarnation,
            owner_identity: value.owner_identity.clone(),
            registration_sequence: value.registration_sequence,
        }
    }
}
#[derive(Default)]
struct RegistrationGateState {
    pending: Vec<(AcceptedDriverId, PrincipalObligationKey)>,
}
/// Cancellation retains an exact unresolved reservation, without holding a gate.
/// New ordinals refuse admission until that reservation is reconciled.
#[allow(dead_code)] // Complete cancellation-safe admission API; adapters integrate next.
pub(crate) struct RegistrationPermit {
    gate: Arc<Mutex<RegistrationGateState>>,
    bound: Option<(AcceptedDriverId, PrincipalObligationKey)>,
    next_registration: Arc<AtomicU64>,
}
#[allow(dead_code)] // Source/B adapters consume definitive and reconciliation APIs next.
impl RegistrationPermit {
    pub(crate) fn complete(mut self) {
        if let Some(bound) = self.bound.take() {
            self.gate
                .lock()
                .expect("registration reservations")
                .pending
                .retain(|entry| entry != &bound);
        }
    }
}
#[derive(Clone)]
pub(crate) struct CapturedIngressObligation {
    driver: Arc<DriverEntry>,
    identity: PrincipalObligation,
}
#[allow(dead_code)] // Complete obligation API; Source/B registration and durable-ack adapters integrate next.
impl CapturedIngressObligation {
    pub(crate) fn registration_sequence(&self) -> u64 {
        self.identity.registration_sequence
    }
    pub(crate) async fn joined(&self) -> DriverClosureReceipt {
        self.driver.closed.wait().await;
        DriverClosureReceipt::closed(self.driver.id.clone())
    }
    /// Caller invokes only after its exact durable closure ack applied/replayed.
    /// Local actual closure is checked independently; a supplied hash is never
    /// enough to acknowledge an open driver.
    pub(crate) fn release_after_ack(&self, receipt: &DriverClosureReceipt) -> Result<(), ()> {
        if !self.driver.closed.is_closed() || !receipt.matches(&self.driver.id) {
            return Err(());
        }
        self.release()
    }
    /// Only a definitive never-registered refusal may discharge this reservation.
    /// A lost answer or transport failure must retain the prepared obligation.
    pub(crate) fn release_refused_registration(&self) -> Result<(), ()> {
        self.release()
    }
    fn release(&self) -> Result<(), ()> {
        let mut obligations = self
            .driver
            .obligations
            .lock()
            .expect("accepted principal obligations");
        let entry = obligations
            .iter_mut()
            .find(|entry| {
                entry.kind == self.identity.kind
                    && entry.incarnation == self.identity.incarnation
                    && entry.owner_identity == self.identity.owner_identity
            })
            .ok_or(())?;
        entry.acknowledged = true;
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct CapturedDriver(Arc<DriverEntry>);
#[allow(dead_code)] // Complete capture/closure API; Source/B principal adapters integrate next.
impl CapturedDriver {
    pub(crate) fn id(&self) -> &AcceptedDriverId {
        &self.0.id
    }
    /// Recover metadata for an exact already prepared reservation after actual
    /// closure. This never creates an obligation or authorizes a new writer.
    pub(crate) fn existing_obligation(
        &self,
        kind: &str,
        incarnation: Uuid,
        owner_identity: &str,
    ) -> Result<CapturedIngressObligation, ()> {
        let identity = self
            .0
            .obligations
            .lock()
            .expect("accepted principal obligations")
            .iter()
            .find(|entry| {
                entry.kind == kind
                    && entry.incarnation == incarnation
                    && entry.owner_identity == owner_identity
            })
            .cloned()
            .ok_or(())?;
        Ok(CapturedIngressObligation {
            driver: Arc::clone(&self.0),
            identity,
        })
    }
    pub(crate) fn prepare_obligation(
        &self,
        permit: &mut RegistrationPermit,
        kind: &str,
        incarnation: Uuid,
        owner_identity: &str,
    ) -> Result<CapturedIngressObligation, ()> {
        if !Arc::ptr_eq(&permit.next_registration, &self.0.next_registration)
            || !matches!(kind, "source" | "receiver")
            || incarnation.is_nil()
            || !owner_identity_valid(owner_identity)
            || self.0.closed.is_closed()
        {
            return Err(());
        }
        let mut gate = permit.gate.lock().expect("registration reservations");
        if gate.pending.iter().any(|(driver, entry)| {
            entry.kind == kind
                && entry.incarnation == incarnation
                && (driver != &self.0.id || entry.owner_identity != owner_identity)
        }) || (gate.pending.len() >= 256
            && !gate.pending.iter().any(|(driver, entry)| {
                driver == &self.0.id
                    && entry.kind == kind
                    && entry.incarnation == incarnation
                    && entry.owner_identity == owner_identity
            }))
            || permit.bound.as_ref().is_some_and(|(driver, entry)| {
                driver != &self.0.id
                    || entry.kind != kind
                    || entry.incarnation != incarnation
                    || entry.owner_identity != owner_identity
            })
        {
            return Err(());
        }
        let mut obligations = self
            .0
            .obligations
            .lock()
            .expect("accepted principal obligations");
        let mut identity = PrincipalObligation {
            kind: kind.into(),
            incarnation,
            owner_identity: owner_identity.into(),
            acknowledged: false,
            registration_sequence: 0,
        };
        if let Some(existing) = obligations.iter().find(|entry| {
            entry.kind == kind
                && entry.incarnation == incarnation
                && entry.owner_identity == owner_identity
        }) {
            if existing.acknowledged {
                return Err(());
            }
            identity = existing.clone();
        } else {
            if obligations.len() >= 64 {
                return Err(());
            }
            identity.registration_sequence = self
                .0
                .next_registration
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    (value < 9_007_199_254_740_991).then_some(value + 1)
                })
                .map_err(|_| ())?;
            obligations.push(identity.clone());
        }
        let pending = (self.0.id.clone(), PrincipalObligationKey::from(&identity));
        if !gate.pending.contains(&pending) {
            gate.pending.push(pending.clone());
        }
        permit.bound = Some(pending);
        Ok(CapturedIngressObligation {
            driver: Arc::clone(&self.0),
            identity,
        })
    }
    /// Awaited by an accepted-connection-owned monitor, not a detached poller.
    pub(crate) async fn joined(&self) -> DriverClosureReceipt {
        self.0.closed.wait().await;
        DriverClosureReceipt::closed(self.0.id.clone())
    }
}
pub(crate) struct AcceptedDriverRegistry {
    boot_id: Uuid,
    next_sequence: AtomicU64,
    next_registration: Arc<AtomicU64>,
    registration_gate: Arc<Mutex<RegistrationGateState>>,
    entries: Mutex<Vec<Arc<DriverEntry>>>,
}
impl Default for AcceptedDriverRegistry {
    fn default() -> Self {
        Self {
            boot_id: Uuid::new_v4(),
            next_sequence: AtomicU64::new(1),
            next_registration: Arc::new(AtomicU64::new(1)),
            registration_gate: Arc::new(Mutex::new(RegistrationGateState::default())),
            entries: Mutex::new(Vec::new()),
        }
    }
}
#[allow(dead_code)] // Complete physical registry; Source/B capture and boot publication integrate next.
impl AcceptedDriverRegistry {
    /// Reserve under a short node-local mutex, never retain a gate over an
    /// exchange. Pending immutable identities fence only their own principal.
    pub(crate) async fn registration_guard(&self) -> Result<RegistrationPermit, ()> {
        Ok(RegistrationPermit {
            gate: Arc::clone(&self.registration_gate),
            bound: None,
            next_registration: Arc::clone(&self.next_registration),
        })
    }
    pub(crate) async fn reconcile_guard(
        &self,
        obligation: &CapturedIngressObligation,
    ) -> Result<RegistrationPermit, ()> {
        let identity = (
            obligation.driver.id.clone(),
            PrincipalObligationKey::from(&obligation.identity),
        );
        if !self
            .registration_gate
            .lock()
            .expect("registration reservations")
            .pending
            .contains(&identity)
        {
            return Err(());
        }
        Ok(RegistrationPermit {
            gate: Arc::clone(&self.registration_gate),
            bound: Some(identity),
            next_registration: Arc::clone(&self.next_registration),
        })
    }

    pub(crate) fn existing_obligation(
        &self,
        driver: &AcceptedDriverId,
        kind: &str,
        incarnation: Uuid,
        owner_identity: &str,
    ) -> Result<CapturedIngressObligation, ()> {
        let entry = self
            .entries
            .lock()
            .expect("accepted ingress custody")
            .iter()
            .find(|entry| &entry.id == driver)
            .cloned()
            .ok_or(())?;
        CapturedDriver(entry).existing_obligation(kind, incarnation, owner_identity)
    }
    pub(crate) fn boot_id(&self) -> Uuid {
        self.boot_id
    }
    /// Capture one actual transport. Multiple resources/principals on it share
    /// the same physical identity; each principal owns its own durable slot.
    pub(crate) fn capture(
        &self,
        connection: &SharingConnectionCancellation,
        owner_node: &str,
    ) -> Result<CapturedDriver, ()> {
        if owner_node.is_empty()
            || owner_node.len() > 256
            || owner_node.chars().any(char::is_control)
        {
            return Err(());
        }
        if connection.closed().is_closed() {
            return Err(());
        }
        let key = connection.ownership_key();
        let mut entries = self.entries.lock().expect("accepted ingress custody");
        if let Some(entry) = entries
            .iter()
            .find(|entry| Weak::ptr_eq(&entry.owner_key, &key))
        {
            let mut owners = entry
                .allowed_owners
                .lock()
                .expect("accepted ingress owners");
            if !owners.iter().any(|node| node == owner_node) {
                if owners.len() >= 256 {
                    return Err(());
                }
                owners.push(owner_node.to_owned());
            }
            return Ok(CapturedDriver(Arc::clone(entry)));
        }
        // Only genuinely closed drivers may yield their local replay cache.
        // Eviction is never a closure receipt: an older lost reply stays
        // unresolved if its owner did not persist the authenticated ack.
        if entries.len() >= RETAINED_DRIVERS_MAX {
            entries.retain(|entry| {
                !entry.closed.is_closed()
                    || entry
                        .obligations
                        .lock()
                        .expect("accepted principal obligations")
                        .iter()
                        .any(|obligation| !obligation.acknowledged)
            });
        }
        if entries.len() >= RETAINED_DRIVERS_MAX
            || entries
                .iter()
                .filter(|entry| !entry.closed.is_closed())
                .count()
                >= ACTIVE_DRIVERS_MAX
        {
            return Err(());
        }
        let sequence = self
            .next_sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                (value < 9_007_199_254_740_991).then_some(value + 1)
            })
            .map_err(|_| ())?;
        let entry = Arc::new(DriverEntry {
            id: AcceptedDriverId {
                boot_id: self.boot_id,
                connection_id: Uuid::new_v4(),
                driver_sequence: sequence,
            },
            owner_key: key,
            allowed_owners: Mutex::new(vec![owner_node.to_owned()]),
            cancel: connection.0.clone(),
            drain: connection.drain_token(),
            closed: connection.closed(),
            obligations: Mutex::new(Vec::new()),
            next_registration: Arc::clone(&self.next_registration),
        });
        entries.push(Arc::clone(&entry));
        Ok(CapturedDriver(entry))
    }
    pub(crate) async fn close(
        &self,
        owner_node: &str,
        request: &DriverCloseRequest,
    ) -> Result<DriverClosureReceipt, ()> {
        if !request.driver.valid()
            || request.driver.boot_id != self.boot_id
            || !(1..=9_007_199_254_740_991).contains(&request.registration_sequence)
        {
            return Err(());
        }
        let now = crate::media_sessions::unix_ms();
        let remaining = request.deadline_unix_ms.saturating_sub(now);
        if !(1..=315_000).contains(&remaining) {
            return Err(());
        }
        let entry = self
            .entries
            .lock()
            .expect("accepted ingress custody")
            .iter()
            .find(|entry| entry.id == request.driver)
            .cloned()
            .ok_or(())?;
        if !entry
            .allowed_owners
            .lock()
            .expect("accepted ingress owners")
            .iter()
            .any(|node| node == owner_node)
        {
            return Err(());
        }
        if !entry
            .obligations
            .lock()
            .expect("accepted principal obligations")
            .iter()
            .any(|obligation| {
                obligation.kind == request.principal_kind
                    && obligation.incarnation == request.incarnation_id
                    && obligation.owner_identity == request.owner_identity
                    && obligation.registration_sequence == request.registration_sequence
            })
        {
            return Err(());
        }
        match request.mode {
            DriverCloseMode::Drain => entry.drain.cancel(),
            DriverCloseMode::Revoke => entry.cancel.cancel(),
        }
        tokio::time::timeout(
            Duration::from_millis(u64::try_from(remaining).map_err(|_| ())?),
            entry.closed.wait(),
        )
        .await
        .map_err(|_| ())?;
        Ok(DriverClosureReceipt::closed(entry.id.clone()))
    }
}

pub(crate) async fn close_http(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse};
    let Some(auth) = crate::http::peer_transport::exact_auth_from_headers(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !state
        .membership
        .authorize_internal_peer_request(&auth, "POST", CLOSE_PATH, &body)
        .await
        .unwrap_or(false)
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(request) = serde_json::from_slice::<DriverCloseRequest>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !matches!(request.principal_kind.as_str(), "source" | "receiver")
        || request.incarnation_id.is_nil()
        || !owner_identity_valid(&request.owner_identity)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let route = match state
        .store
        .media_session_route_by_incarnation(&request.incarnation_id.to_string())
        .await
    {
        Ok(Some(route))
            if route.owner_node_id == auth.node_id
                && route.owner_epoch == request.expected_owner_epoch =>
        {
            route
        }
        _ => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let registration = plurx_core::sharing_ingress_custody::IngressRegistration {
        node_id: state.node_id.clone(),
        boot_id: request.driver.boot_id,
        connection_id: request.driver.connection_id,
        driver_sequence: request.driver.driver_sequence,
        registration_sequence: request.registration_sequence,
        closed_confirmation: None,
    };
    if !state
        .store
        .registered_ingress_driver(
            &request.principal_kind,
            request.incarnation_id,
            &request.owner_identity,
            &registration,
        )
        .await
        .unwrap_or(false)
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    // Recovery may own a newer fence. Exact current durable ownership plus the
    // original registered driver authorizes closure, never actor adoption.
    {
        let entries = state
            .sharing
            .accepted_drivers
            .entries
            .lock()
            .expect("accepted ingress custody");
        let Some(entry) = entries.iter().find(|entry| entry.id == request.driver) else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let mut owners = entry
            .allowed_owners
            .lock()
            .expect("accepted ingress owners");
        if !owners.contains(&route.owner_node_id) {
            if owners.len() >= 256 {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            owners.push(route.owner_node_id);
        }
    }
    match state
        .sharing
        .accepted_drivers
        .close(&auth.node_id, &request)
        .await
    {
        Ok(receipt) => axum::Json(receipt).into_response(),
        Err(()) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sharing_ingress_principal_registration_order_survives_keepalive_and_unknown_answer() {
        let registry = AcceptedDriverRegistry::default();
        let old_connection = SharingConnectionCancellation::new();
        let new_connection = SharingConnectionCancellation::new();
        let old = registry
            .capture(&old_connection, "owner")
            .expect("custody fixture operation");
        let newer = registry
            .capture(&new_connection, "owner")
            .expect("custody fixture operation");
        let principal = Uuid::new_v4();
        let hash = "a".repeat(64);
        let mut permit = registry
            .registration_guard()
            .await
            .expect("custody fixture operation");
        let first = newer
            .prepare_obligation(&mut permit, "source", principal, &hash)
            .expect("custody fixture operation");
        permit.complete();
        let mut permit = registry
            .registration_guard()
            .await
            .expect("custody fixture operation");
        let late_keepalive = old
            .prepare_obligation(&mut permit, "source", principal, &hash)
            .expect("custody fixture operation");
        assert!(late_keepalive.registration_sequence() > first.registration_sequence());
        let mut unrelated =
            tokio::time::timeout(Duration::from_secs(1), registry.registration_guard())
                .await
                .expect("in-flight principal does not hold the shared gate")
                .expect("unrelated gate");
        newer
            .prepare_obligation(&mut unrelated, "source", Uuid::new_v4(), &hash)
            .expect("unrelated principal progresses while original exchange is in-flight");
        unrelated.complete();
        // Losing the caller retains the exact unknown registration, but not a
        // locked mutex or a node-wide admission blockade.
        drop(permit);
        let another_connection = SharingConnectionCancellation::new();
        let another = registry
            .capture(&another_connection, "owner")
            .expect("custody fixture operation");
        let mut permit = registry
            .registration_guard()
            .await
            .expect("custody fixture operation");
        assert!(another
            .prepare_obligation(&mut permit, "source", principal, &hash)
            .is_err());
        let other_principal = Uuid::new_v4();
        another
            .prepare_obligation(&mut permit, "source", other_principal, &hash)
            .expect("custody fixture operation");
        permit.complete();
        let mut permit = registry
            .reconcile_guard(&late_keepalive)
            .await
            .expect("custody fixture operation");
        let replay = old
            .prepare_obligation(&mut permit, "source", principal, &hash)
            .expect("custody fixture operation");
        assert_eq!(
            replay.registration_sequence(),
            late_keepalive.registration_sequence()
        );
        permit.complete();
        let mut permit = registry
            .registration_guard()
            .await
            .expect("custody fixture operation");
        let next = another
            .prepare_obligation(&mut permit, "source", principal, &hash)
            .expect("custody fixture operation");
        assert!(next.registration_sequence() > replay.registration_sequence());
        permit.complete();
    }
}
