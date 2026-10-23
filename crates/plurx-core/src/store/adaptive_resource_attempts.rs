//! Durable adaptive request coordination, never physical-resource authority.
//!
//! A committed provisional route precedes worker acquisition. Expiration, a
//! lost reply, a losing result and an ended route all retain cleanup debt.
//! There is intentionally no public transition to `Closed`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domain::{MediaSessionActivation, MediaSessionActivationOutcome},
    error::StoreError,
    playback_principal::PlaybackPrincipal,
};

mod codec;
pub mod schema;
pub(crate) mod transactions;
pub use codec::selected_recipe_digest;

#[cfg(feature = "hiqlite-contract-tests")]
pub mod validation;

/// The owner key is derived from the separately supplied typed principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestKey {
    pub request_id: String,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub request_incarnation_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestBinding {
    pub version: u32,
    pub key: RequestKey,
    pub principal: PlaybackPrincipal,
    pub request_fingerprint: String,
    pub playback_id: String,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub recovery_epoch: Uuid,
    pub planning_generation: i64,
    pub planning_source_digest: String,
    pub settings_digest: String,
    pub intent_digest: String,
    pub original_deadline_ms: i64,
    #[serde(default, deserialize_with = "optional_canonical_uuid")]
    pub expected_predecessor_incarnation_id: Option<Uuid>,
    pub expected_predecessor_owner_epoch: Option<i64>,
    pub expected_desired_revision: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptKey {
    pub request: RequestKey,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub attempt_id: Uuid,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub route_incarnation_id: Uuid,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub session_id: Uuid,
    pub worker_node_id: String,
    pub owner_epoch: i64,
    pub ordinal: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerBinding {
    pub node_id: String,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub worker_instance_id: Uuid,
    pub serving_generation: u64,
    pub planning_generation: i64,
    pub planning_source_digest: String,
    pub selected_recipe_digest: String,
    pub prepared_handle_digest: String,
    pub output_digest: String,
    pub prepared_expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationContext {
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub operation_id: Uuid,
    pub expected_request_sequence: i64,
    pub expected_attempt_sequence: i64,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptClaim {
    pub context: MutationContext,
    pub binding: RequestBinding,
    pub key: AttemptKey,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub worker_instance_id: Uuid,
    pub serving_generation: u64,
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedAttempt {
    pub context: MutationContext,
    pub binding: RequestBinding,
    pub key: AttemptKey,
    pub worker: WorkerBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WinnerSelection {
    pub context: MutationContext,
    pub binding: RequestBinding,
    pub key: AttemptKey,
    pub worker: WorkerBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveAdoption {
    pub context: MutationContext,
    pub binding: RequestBinding,
    pub key: AttemptKey,
    pub worker: WorkerBinding,
    pub activation: MediaSessionActivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptFence {
    pub context: MutationContext,
    pub binding: RequestBinding,
    pub key: AttemptKey,
    pub cancel_request: bool,
}

/// Logical Store closure after the original worker has positively finished its
/// native and application-owned cleanup. This record cannot refund a resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptClose {
    pub context: MutationContext,
    pub binding: RequestBinding,
    pub key: AttemptKey,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub worker_instance_id: Uuid,
    pub serving_generation: u64,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub cleanup_operation_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Provisional,
    Prepared,
    Winner,
    Losing,
    Cleanup,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestDisposition {
    Open,
    Cancelled,
    Adopted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord {
    pub key: AttemptKey,
    pub sequence: i64,
    pub phase: AttemptPhase,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub worker_instance_id: Uuid,
    pub serving_generation: u64,
    pub domains: Vec<String>,
    pub worker: Option<WorkerBinding>,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub last_operation_id: Uuid,
    pub last_payload_digest: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestStatus {
    pub binding: RequestBinding,
    pub sequence: i64,
    pub next_ordinal: u8,
    #[serde(default, deserialize_with = "optional_canonical_uuid")]
    pub winner_attempt_id: Option<Uuid>,
    pub disposition: RequestDisposition,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    pub last_operation_id: Uuid,
    pub last_payload_digest: String,
    pub attempts: Vec<AttemptRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptSchemaReadiness {
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptRefusal {
    Conflict,
    Expired,
    Bound,
    UnsupportedSchema,
    UnsupportedPrincipal,
    StaleBinding,
    WinnerLost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptMutation {
    Applied(RequestStatus),
    Replay(RequestStatus),
    Refused(AttemptRefusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptAdoption {
    Applied(MediaSessionActivationOutcome),
    Replay(MediaSessionActivationOutcome),
    Refused(AttemptRefusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptInventoryQuery {
    pub worker_node_id: String,
    pub after_attempt_id: Option<Uuid>,
    pub limit: u16,
}

/// Every row carries its immutable request binding, not merely its route ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptInventoryEntry {
    pub binding: RequestBinding,
    pub attempt: AttemptRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptInventoryPage {
    pub entries: Vec<AttemptInventoryEntry>,
    pub next_cursor: Option<Uuid>,
}

#[async_trait]
pub trait AdaptiveResourceAttemptStore: Send + Sync {
    async fn adaptive_attempt_schema(&self) -> Result<AttemptSchemaReadiness, StoreError>;
    async fn claim_adaptive_attempt(
        &self,
        claim: &AttemptClaim,
    ) -> Result<AttemptMutation, StoreError>;
    async fn record_adaptive_prepared(
        &self,
        prepared: &PreparedAttempt,
    ) -> Result<AttemptMutation, StoreError>;
    async fn select_adaptive_winner(
        &self,
        selection: &WinnerSelection,
    ) -> Result<AttemptMutation, StoreError>;
    async fn adopt_adaptive_winner(
        &self,
        adoption: &AdaptiveAdoption,
    ) -> Result<AttemptAdoption, StoreError>;
    async fn fence_adaptive_attempt(
        &self,
        fence: &AttemptFence,
    ) -> Result<AttemptMutation, StoreError>;
    async fn close_adaptive_attempt(
        &self,
        close: &AttemptClose,
    ) -> Result<AttemptMutation, StoreError>;
    async fn adaptive_request_status(
        &self,
        key: &RequestKey,
        principal: &PlaybackPrincipal,
    ) -> Result<Option<RequestStatus>, StoreError>;
    async fn unclosed_adaptive_attempts(
        &self,
        query: &AttemptInventoryQuery,
    ) -> Result<AttemptInventoryPage, StoreError>;
}

fn invalid() -> StoreError {
    StoreError::Task("invalid adaptive resource coordination record".into())
}

fn optional_canonical_uuid<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Uuid>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|text| {
            let id = Uuid::parse_str(&text).map_err(serde::de::Error::custom)?;
            if id.to_string() != text {
                return Err(serde::de::Error::custom("noncanonical UUID"));
            }
            Ok(id)
        })
        .transpose()
}

fn text(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.bytes().any(|b| matches!(b, 0 | b'\r' | b'\n'))
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl RequestKey {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !text(&self.request_id, 128) || self.request_incarnation_id.is_nil() {
            return Err(invalid());
        }
        Ok(())
    }
}

impl RequestBinding {
    pub fn validate(&self) -> Result<(), StoreError> {
        self.key.validate()?;
        if self.version != 1
            || !self.principal.valid_admission_shape()
            || !digest(&self.request_fingerprint)
            || !text(&self.playback_id, 128)
            || self.recovery_epoch.is_nil()
            || self.planning_generation < 0
            || !digest(&self.planning_source_digest)
            || !digest(&self.settings_digest)
            || !digest(&self.intent_digest)
            || self.original_deadline_ms <= 0
            || self.expected_predecessor_incarnation_id.is_some()
                != self.expected_predecessor_owner_epoch.is_some()
            || self
                .expected_predecessor_incarnation_id
                .is_some_and(|id| id.is_nil())
            || self
                .expected_predecessor_owner_epoch
                .is_some_and(|epoch| epoch <= 0)
            || self
                .expected_desired_revision
                .is_some_and(|revision| revision <= 0)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AttemptKey {
    pub fn validate(&self, binding: &RequestBinding) -> Result<(), StoreError> {
        binding.validate()?;
        if self.request != binding.key
            || self.attempt_id.is_nil()
            || self.route_incarnation_id.is_nil()
            || self.session_id.is_nil()
            || self.attempt_id == self.route_incarnation_id
            || self.attempt_id == self.session_id
            || self.route_incarnation_id == self.session_id
            || [self.attempt_id, self.route_incarnation_id, self.session_id]
                .contains(&self.request.request_incarnation_id)
            || !text(&self.worker_node_id, 256)
            || !(1..=3).contains(&self.ordinal)
            || self.owner_epoch != i64::from(self.ordinal)
            || serde_json::to_string(self).map_err(|_| invalid())?.len() > 2048
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl MutationContext {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.operation_id.is_nil()
            || !(0..i64::MAX).contains(&self.expected_request_sequence)
            || !(0..i64::MAX).contains(&self.expected_attempt_sequence)
            || self.now_ms <= 0
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl WorkerBinding {
    pub fn validate(&self, binding: &RequestBinding, key: &AttemptKey) -> Result<(), StoreError> {
        key.validate(binding)?;
        if self.node_id != key.worker_node_id
            || self.worker_instance_id.is_nil()
            || self.planning_generation != binding.planning_generation
            || self.planning_source_digest != binding.planning_source_digest
            || !digest(&self.selected_recipe_digest)
            || !digest(&self.prepared_handle_digest)
            || !digest(&self.output_digest)
            || self.prepared_expires_at_ms <= 0
            || self.prepared_expires_at_ms > binding.original_deadline_ms
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn validate_domains(domains: &[String]) -> Result<(), StoreError> {
    if domains.is_empty()
        || domains.len() > 32
        || domains.iter().any(|domain| !text(domain, 256))
        || domains.windows(2).any(|pair| pair[0] >= pair[1])
        || serde_json::to_string(domains).map_err(|_| invalid())?.len() > 16 * 1024
    {
        return Err(invalid());
    }
    Ok(())
}

impl AttemptClaim {
    pub fn validate(&self) -> Result<(), StoreError> {
        self.context.validate()?;
        self.key.validate(&self.binding)?;
        validate_domains(&self.domains)?;
        if self.worker_instance_id.is_nil() || self.context.expected_attempt_sequence != 0 {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AttemptClose {
    pub fn validate(&self) -> Result<(), StoreError> {
        self.context.validate()?;
        self.key.validate(&self.binding)?;
        if self.worker_instance_id.is_nil() || self.cleanup_operation_id.is_nil() {
            return Err(invalid());
        }
        Ok(())
    }
}

impl RequestStatus {
    /// Decode failures and incomplete inventories are unavailable authority,
    /// never an empty set of held resources.
    pub fn validate(&self) -> Result<(), StoreError> {
        self.binding.validate()?;
        if !self
            .binding
            .principal
            .local_user_id()
            .is_some_and(|id| id > 0)
            || self.sequence <= 0
            || !(2..=4).contains(&self.next_ordinal)
            || self.attempts.len() != usize::from(self.next_ordinal - 1)
            || self.last_operation_id.is_nil()
            || !digest(&self.last_payload_digest)
        {
            return Err(invalid());
        }
        for (index, record) in self.attempts.iter().enumerate() {
            record.key.validate(&self.binding)?;
            validate_domains(&record.domains)?;
            if usize::from(record.key.ordinal) != index + 1
                || record.sequence <= 0
                || record.worker_instance_id.is_nil()
                || record.last_operation_id.is_nil()
                || !digest(&record.last_payload_digest)
                || record.updated_at_ms <= 0
                || self.attempts[..index].iter().any(|previous| {
                    previous.key.worker_node_id == record.key.worker_node_id
                        || previous.key.attempt_id == record.key.attempt_id
                        || previous.key.session_id == record.key.session_id
                        || previous.key.route_incarnation_id == record.key.route_incarnation_id
                })
            {
                return Err(invalid());
            }
            if let Some(worker) = &record.worker {
                worker.validate(&self.binding, &record.key)?;
                if worker.worker_instance_id != record.worker_instance_id
                    || worker.serving_generation != record.serving_generation
                {
                    return Err(invalid());
                }
            } else if matches!(record.phase, AttemptPhase::Prepared | AttemptPhase::Winner) {
                return Err(invalid());
            }
            if (record.phase == AttemptPhase::Winner
                && self.winner_attempt_id != Some(record.key.attempt_id))
                || (self.winner_attempt_id == Some(record.key.attempt_id)
                    && !matches!(
                        record.phase,
                        AttemptPhase::Winner | AttemptPhase::Cleanup | AttemptPhase::Closed
                    ))
            {
                return Err(invalid());
            }
        }
        if self.winner_attempt_id.is_some_and(|winner| {
            !self
                .attempts
                .iter()
                .any(|record| record.key.attempt_id == winner)
        }) || (self.disposition == RequestDisposition::Adopted
            && self.winner_attempt_id.is_none())
        {
            return Err(invalid());
        }
        Ok(())
    }
}
