//! Continuous-quality media facts, independent of optional-target intent.
//!
//! Scheduling reserves dependencies before the client can append. A cancel
//! carrying a lost append acknowledgement promotes that fact first; it cannot
//! erase already appended media. Storage persists the whole ledger under the
//! parent route's owner fence, while clients remain the authority for actual
//! completed append and presentation observations.
use serde::{Deserialize, Serialize};

pub const CONTINUOUS_QUALITY_VERSION: u8 = 1;
pub const MAX_QUALITY_TRANSACTIONS: usize = 16;
// Normal 60-second forward + 30-second back buffers retain about 90
// independent two-second video/AAC intervals, with room for a pending join.
pub const MAX_QUALITY_INTERVALS: usize = 128;
pub const MAX_QUALITY_PINNED_BYTES: u64 = 256 * 1024 * 1024;
/// Decoding bound for stored ledgers. Acceptance retains exactly one replay
/// receipt (see [`QualityLedger::apply`]); older rows may still hold more.
pub const MAX_QUALITY_RECEIPTS: usize = 128;
/// Retention horizon for inactive parents' ledgers in Store maintenance.
pub const QUALITY_RECEIPT_HORIZON_MS: i64 = 90_000;
pub const MAX_QUALITY_LEDGER_BYTES: usize = 128 * 1024;
const JS_MAX_INTEGER: u64 = 9_007_199_254_740_991;

fn valid_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}
fn valid_artifact(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityAttachment {
    pub client_instance_id: String,
    pub lifetime_id: String,
    pub attachment_id: String,
    pub family_id: String,
}
impl QualityAttachment {
    pub fn valid(&self) -> bool {
        valid_uuid(&self.client_instance_id)
            && valid_uuid(&self.attachment_id)
            && valid_artifact(&self.family_id)
            && !self.lifetime_id.is_empty()
            && self.lifetime_id.len() <= super::MAX_LIFETIME_ID
            && !self.lifetime_id.bytes().any(|byte| byte.is_ascii_control())
    }
}

/// A published fragment's exact video sample interval, never range-growth
/// inference. Film-global integer ticks preserve the rational output grid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityInterval {
    pub artifact_id: String,
    pub rendition_id: String,
    pub timescale: u32,
    pub from_tick: u64,
    pub through_tick: u64,
    pub byte_length: u64,
}
impl QualityInterval {
    pub fn valid(&self) -> bool {
        valid_artifact(&self.artifact_id)
            && valid_artifact(&self.rendition_id)
            && (1..=1_000_000).contains(&self.timescale)
            && self.from_tick < self.through_tick
            && self.through_tick <= JS_MAX_INTEGER
            && self.byte_length > 0
            && self.byte_length <= MAX_QUALITY_PINNED_BYTES
    }
    /// Artifact identity is SHA-256 of the immutable media payload. Init
    /// identity remains a separate verified rendition-family fact.
    pub fn matches_bytes(&self, bytes: &[u8]) -> bool {
        use sha2::{Digest, Sha256};
        self.valid()
            && self.byte_length == bytes.len() as u64
            && self.artifact_id == hex::encode(Sha256::digest(bytes))
    }
    fn contains(&self, tick: u64) -> bool {
        self.from_tick <= tick && tick < self.through_tick
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityState {
    Preparing,
    Ready,
    Scheduled,
    Appended,
    Presented,
    Cancelling,
    RetainedCurrent,
    Superseded,
    RecoveryOwned,
    Disposed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityPreparationBinding {
    pub timescale: u32,
    pub through_tick: u64,
    pub deadline_ms: i64,
}
impl QualityPreparationBinding {
    fn valid(&self) -> bool {
        (1..=1_000_000).contains(&self.timescale)
            && self.through_tick <= JS_MAX_INTEGER
            && self.deadline_ms > 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityTransaction {
    pub transaction_id: String,
    pub intent_revision: u64,
    pub target_rendition_id: String,
    pub state: QualityState,
    pub intent_superseded: bool,
    pub cancel_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<QualityPreparationBinding>,
    pub ready: Vec<QualityInterval>,
    pub reserved: Vec<QualityInterval>,
    pub appended: Vec<QualityInterval>,
    pub ever_appended: bool,
    pub disposed: Vec<String>,
    pub first_presented_tick: Option<u64>,
    pub first_presented_at_ms: Option<i64>,
}
impl QualityTransaction {
    fn unresolved(&self) -> bool {
        !self.reserved.is_empty()
            || !matches!(
                self.state,
                QualityState::RetainedCurrent
                    | QualityState::Superseded
                    | QualityState::RecoveryOwned
                    | QualityState::Disposed
            )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QualityOperation {
    Prepare {
        intent_revision: u64,
        target_rendition_id: String,
    },
    Scheduled {
        intervals: Vec<QualityInterval>,
    },
    Appended {
        intervals: Vec<QualityInterval>,
    },
    Presented {
        artifact_id: String,
        film_tick: u64,
        observed_at_ms: i64,
    },
    // A cancel can carry completed facts whose earlier acknowledgement was
    // lost. Missing facts are not proof that a scheduled append never ran.
    CancelUnappended {
        completed: Vec<QualityInterval>,
    },
    RecoveryOwned {
        disposed_artifacts: Vec<String>,
    },
    Disposed {
        artifacts: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityTransitionRequest {
    pub version: u8,
    pub generation: String,
    pub control_epoch: u64,
    pub sequence: u64,
    pub attachment: QualityAttachment,
    pub transaction_id: String,
    pub operation: QualityOperation,
}

impl QualityTransitionRequest {
    /// Wire shape and bounds, independent of the current transaction state.
    pub fn valid(&self) -> bool {
        let intervals_valid = |intervals: &[QualityInterval], allow_empty: bool| {
            let mut ids = std::collections::HashSet::new();
            (allow_empty || !intervals.is_empty())
                && intervals.len() <= MAX_QUALITY_INTERVALS
                && intervals
                    .iter()
                    .all(|interval| interval.valid() && ids.insert(&interval.artifact_id))
        };
        let artifacts_valid = |artifacts: &[String]| {
            let mut ids = std::collections::HashSet::new();
            artifacts.len() <= MAX_QUALITY_INTERVALS
                && artifacts
                    .iter()
                    .all(|id| valid_artifact(id) && ids.insert(id))
        };
        self.version == CONTINUOUS_QUALITY_VERSION
            && valid_uuid(&self.generation)
            && (1..=JS_MAX_INTEGER).contains(&self.control_epoch)
            && (1..=JS_MAX_INTEGER).contains(&self.sequence)
            && self.attachment.valid()
            && valid_uuid(&self.transaction_id)
            && match &self.operation {
                QualityOperation::Prepare {
                    intent_revision,
                    target_rendition_id,
                } => {
                    (1..=JS_MAX_INTEGER).contains(intent_revision)
                        && valid_artifact(target_rendition_id)
                }
                QualityOperation::Scheduled { intervals }
                | QualityOperation::Appended { intervals } => intervals_valid(intervals, false),
                QualityOperation::CancelUnappended { completed } => {
                    intervals_valid(completed, true)
                }
                QualityOperation::Presented {
                    artifact_id,
                    film_tick,
                    observed_at_ms,
                } => {
                    valid_artifact(artifact_id)
                        && *film_tick <= JS_MAX_INTEGER
                        && *observed_at_ms > 0
                }
                QualityOperation::Disposed { artifacts }
                | QualityOperation::RecoveryOwned {
                    disposed_artifacts: artifacts,
                } => artifacts_valid(artifacts),
            }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityTransitionReceipt {
    pub version: u8,
    pub generation: String,
    pub control_epoch: u64,
    pub accepted_sequence: u64,
    pub attachment: QualityAttachment,
    pub transaction: QualityTransaction,
}

impl QualityTransitionReceipt {
    pub fn valid_for(&self, request: &QualityTransitionRequest) -> bool {
        if !request.valid()
            || self.version != CONTINUOUS_QUALITY_VERSION
            || self.generation != request.generation
            || self.control_epoch != request.control_epoch
            || self.accepted_sequence != request.sequence
            || self.attachment != request.attachment
            || self.transaction.transaction_id != request.transaction_id
        {
            return false;
        }
        if let QualityOperation::Prepare {
            intent_revision,
            target_rendition_id,
        } = &request.operation
        {
            if self.transaction.intent_revision != *intent_revision
                || self.transaction.target_rendition_id != *target_rendition_id
            {
                return false;
            }
        }
        let candidate = QualityLedger {
            version: self.version,
            generation: self.generation.clone(),
            control_epoch: self.control_epoch,
            attachment: self.attachment.clone(),
            latest_intent_revision: self.transaction.intent_revision,
            accepted_sequence: self.accepted_sequence,
            transactions: vec![self.transaction.clone()],
            shared_audio_rendition_id: None,
            shared_audio_reserved: Vec::new(),
            receipts: vec![],
        };
        candidate.valid()
            && (!matches!(
                self.transaction.state,
                QualityState::RetainedCurrent | QualityState::Superseded
            ) || (!self.transaction.ever_appended && self.transaction.reserved.is_empty()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayReceipt {
    sequence: u64,
    transaction_id: String,
    request_digest: String,
    transaction: QualityTransaction,
    accepted_at_ms: i64,
}

impl ReplayReceipt {
    fn valid_for_ledger(&self, ledger: &QualityLedger) -> bool {
        let candidate = QualityLedger {
            version: ledger.version,
            generation: ledger.generation.clone(),
            control_epoch: ledger.control_epoch,
            attachment: ledger.attachment.clone(),
            latest_intent_revision: self.transaction.intent_revision,
            accepted_sequence: self.sequence,
            transactions: vec![self.transaction.clone()],
            shared_audio_rendition_id: None,
            shared_audio_reserved: vec![],
            receipts: vec![],
        };
        valid_artifact(&self.request_digest)
            && self.transaction_id == self.transaction.transaction_id
            && candidate.valid()
            && (!matches!(
                self.transaction.state,
                QualityState::RetainedCurrent | QualityState::Superseded
            ) || !self.transaction.ever_appended)
    }

    fn response(&self, ledger: &QualityLedger) -> QualityTransitionReceipt {
        QualityTransitionReceipt {
            version: CONTINUOUS_QUALITY_VERSION,
            generation: ledger.generation.clone(),
            control_epoch: ledger.control_epoch,
            attachment: ledger.attachment.clone(),
            accepted_sequence: self.sequence,
            transaction: self.transaction.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityLedger {
    pub version: u8,
    pub generation: String,
    pub control_epoch: u64,
    pub attachment: QualityAttachment,
    pub latest_intent_revision: u64,
    pub accepted_sequence: u64,
    pub transactions: Vec<QualityTransaction>,
    /// Server-derived dependencies for the attachment's one shared soundtrack.
    /// Kept separately from video transactions because video disposal is not
    /// evidence that the shared audio transport has disposed its media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shared_audio_rendition_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    shared_audio_reserved: Vec<QualityInterval>,
    receipts: Vec<ReplayReceipt>,
}

/// Owner-side physical publication contract. Inputs are server-reduced facts
/// and a locally verified family, never a client-supplied ledger or family.
/// A deadline can lose an acknowledgement; implementations retain physical
/// guards until any submitted durable mutation actually settles.
pub trait QualityReservationPublisher: Send + Sync {
    fn commit_quality_reservations(
        &self,
        owner_node_id: &str,
        expected: &crate::store::QualityLedgerSnapshot,
        candidate: &QualityLedger,
        family: &crate::transcode::VodPresentationFamily,
        now_ms: i64,
        deadline: std::time::Instant,
    ) -> impl std::future::Future<Output = Result<bool, String>> + Send;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QualityCapacityUsage {
    pub transactions: usize,
    pub unresolved_transactions: usize,
    pub pinned_intervals: usize,
    pub pinned_bytes: u64,
    pub receipts: usize,
    pub encoded_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualityTransitionError {
    Invalid,
    OwnerChanged,
    StaleSequence,
    ConflictingReplay,
    UnknownTransaction,
    InvalidTransition,
    Capacity,
}
impl std::fmt::Display for QualityTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "continuous quality transition: {self:?}")
    }
}
impl std::error::Error for QualityTransitionError {}

fn quality_request_digest(
    request: &QualityTransitionRequest,
) -> Result<String, QualityTransitionError> {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(request).map_err(|_| QualityTransitionError::Invalid)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

impl QualityLedger {
    pub fn new(
        generation: String,
        control_epoch: u64,
        attachment: QualityAttachment,
    ) -> Result<Self, QualityTransitionError> {
        if !valid_uuid(&generation)
            || control_epoch == 0
            || control_epoch > JS_MAX_INTEGER
            || !attachment.valid()
        {
            return Err(QualityTransitionError::Invalid);
        }
        Ok(Self {
            version: CONTINUOUS_QUALITY_VERSION,
            generation,
            control_epoch,
            attachment,
            latest_intent_revision: 0,
            accepted_sequence: 0,
            transactions: Vec::new(),
            shared_audio_rendition_id: None,
            shared_audio_reserved: Vec::new(),
            receipts: Vec::new(),
        })
    }

    pub fn valid(&self) -> bool {
        if self.version != CONTINUOUS_QUALITY_VERSION
            || !valid_uuid(&self.generation)
            || self.control_epoch == 0
            || self.control_epoch > JS_MAX_INTEGER
            || !self.attachment.valid()
            || self.accepted_sequence > JS_MAX_INTEGER
            || self.latest_intent_revision > JS_MAX_INTEGER
            || self.transactions.len() > MAX_QUALITY_TRANSACTIONS
            || self.receipts.len() > MAX_QUALITY_RECEIPTS
        {
            return false;
        }
        let mut ids = std::collections::HashSet::new();
        if self.shared_audio_reserved.len() > MAX_QUALITY_INTERVALS {
            return false;
        }
        let mut audio_artifacts = std::collections::HashSet::new();
        let audio_rendition = self.shared_audio_rendition_id.as_deref();
        if audio_rendition.is_some_and(|id| !valid_artifact(id))
            || self
                .transactions
                .iter()
                .any(|tx| Some(tx.target_rendition_id.as_str()) == audio_rendition)
        {
            return false;
        }
        for interval in &self.shared_audio_reserved {
            if !interval.valid()
                || interval.timescale != crate::transcode::VOD_AUDIO_RATE
                || Some(interval.rendition_id.as_str()) != audio_rendition
                || !audio_artifacts.insert(&interval.artifact_id)
                || self
                    .transactions
                    .iter()
                    .any(|tx| tx.target_rendition_id == interval.rendition_id)
            {
                return false;
            }
        }
        for tx in &self.transactions {
            if !valid_uuid(&tx.transaction_id)
                || !ids.insert(&tx.transaction_id)
                || tx.intent_revision == 0
                || tx.intent_revision > self.latest_intent_revision
                || !valid_artifact(&tx.target_rendition_id)
                || tx
                    .preparation
                    .as_ref()
                    .is_some_and(|binding| !binding.valid())
                || tx.ready.len() > MAX_QUALITY_INTERVALS
                || tx.appended.len() > MAX_QUALITY_INTERVALS
                || tx.disposed.len() > MAX_QUALITY_INTERVALS
                || tx.disposed.iter().any(|id| !valid_artifact(id))
                || tx
                    .ready
                    .iter()
                    .chain(&tx.reserved)
                    .chain(&tx.appended)
                    .any(|interval| {
                        !interval.valid() || interval.rendition_id != tx.target_rendition_id
                    })
                || tx
                    .appended
                    .iter()
                    .any(|interval| !tx.reserved.contains(interval))
                || (!tx.ever_appended
                    && (!tx.appended.is_empty() || tx.first_presented_tick.is_some()))
                || tx.first_presented_tick.is_some() != tx.first_presented_at_ms.is_some()
                || tx
                    .first_presented_tick
                    .is_some_and(|tick| tick > JS_MAX_INTEGER)
                || tx.first_presented_at_ms.is_some_and(|at| at <= 0)
            {
                return false;
            }
        }
        self.pinned_dependency_usage().is_ok_and(|(count, bytes)| {
            count <= MAX_QUALITY_INTERVALS && bytes <= MAX_QUALITY_PINNED_BYTES
        }) && self.receipts.iter().all(|receipt| {
            receipt.sequence > 0
                && receipt.sequence <= self.accepted_sequence
                && receipt.accepted_at_ms > 0
                && receipt.transaction.ready.is_empty()
                && receipt.transaction.reserved.is_empty()
                && receipt.transaction.appended.is_empty()
                && receipt.transaction.disposed.is_empty()
                && receipt.valid_for_ledger(self)
        })
    }

    /// A Store-fenced takeover changes command authority, never media facts.
    /// The new owner must reverify producer readiness before scheduling more.
    pub fn adopt_epoch(&mut self, control_epoch: u64) -> Result<(), QualityTransitionError> {
        if control_epoch <= self.control_epoch || control_epoch > JS_MAX_INTEGER {
            return Err(QualityTransitionError::OwnerChanged);
        }
        self.control_epoch = control_epoch;
        self.accepted_sequence = 0;
        self.receipts.clear();
        for tx in &mut self.transactions {
            tx.ready.clear();
            if matches!(tx.state, QualityState::Preparing | QualityState::Ready)
                && !tx.cancel_requested
            {
                tx.state = QualityState::Preparing;
            }
        }
        Ok(())
    }

    /// Bounded facts for diagnosing a [`QualityTransitionError::Capacity`]
    /// refusal: which of the ledger's bounds the attachment is close to.
    pub fn capacity_usage(&self) -> QualityCapacityUsage {
        let (pinned_intervals, pinned_bytes) = self.pinned_dependency_usage().unwrap_or((0, 0));
        QualityCapacityUsage {
            transactions: self.transactions.len(),
            unresolved_transactions: self
                .transactions
                .iter()
                .filter(|tx| tx.unresolved())
                .count(),
            pinned_intervals,
            pinned_bytes,
            receipts: self.receipts.len(),
            encoded_bytes: serde_json::to_vec(self).map_or(0, |bytes| bytes.len()),
        }
    }

    pub fn shared_audio_rendition_id(&self) -> Option<&str> {
        self.shared_audio_rendition_id.as_deref()
    }

    pub fn shared_audio_reserved(&self) -> &[QualityInterval] {
        &self.shared_audio_reserved
    }

    /// Install only owner-verified immutable AAC facts. A client cannot supply
    /// this list through the transition envelope. Pin budget refusal is atomic.
    pub fn reserve_shared_audio(
        &mut self,
        intervals: &[QualityInterval],
    ) -> Result<(), QualityTransitionError> {
        if intervals.len() > MAX_QUALITY_INTERVALS {
            return Err(QualityTransitionError::Capacity);
        }
        let mut next = self.clone();
        for interval in intervals {
            if !interval.valid()
                || interval.timescale != crate::transcode::VOD_AUDIO_RATE
                || next
                    .shared_audio_rendition_id
                    .as_ref()
                    .is_some_and(|id| id != &interval.rendition_id)
            {
                return Err(QualityTransitionError::Invalid);
            }
            next.shared_audio_rendition_id
                .get_or_insert_with(|| interval.rendition_id.clone());
            if next
                .shared_audio_reserved
                .iter()
                .any(|old| old.artifact_id == interval.artifact_id && old != interval)
            {
                return Err(QualityTransitionError::Invalid);
            }
            if !next.shared_audio_reserved.contains(interval) {
                next.shared_audio_reserved.push(interval.clone());
            }
        }
        if !next.valid() {
            return Err(QualityTransitionError::Capacity);
        }
        *self = next;
        Ok(())
    }

    /// Apply atomically in memory; the Store then CASes this candidate against
    /// the exact parent epoch and ledger revision. Refusal changes no facts.
    pub fn apply(
        &mut self,
        request: &QualityTransitionRequest,
        now_ms: i64,
    ) -> Result<QualityTransitionReceipt, QualityTransitionError> {
        if !request.valid() || now_ms <= 0 {
            return Err(QualityTransitionError::Invalid);
        }
        if request.generation != self.generation
            || request.control_epoch != self.control_epoch
            || request.attachment != self.attachment
        {
            return Err(QualityTransitionError::OwnerChanged);
        }
        if let Some(receipt) = self
            .receipts
            .iter()
            .find(|receipt| receipt.sequence == request.sequence)
        {
            return if receipt.request_digest == quality_request_digest(request)? {
                Ok(receipt.response(self))
            } else {
                Err(QualityTransitionError::ConflictingReplay)
            };
        }
        if request.sequence <= self.accepted_sequence {
            return Err(QualityTransitionError::StaleSequence);
        }
        let mut next = self.clone();
        // One attachment is one serialized command channel: a client sends
        // sequence N+1 only after N settled, by its acknowledgement or by an
        // authoritative ledger read. Accepting a newer sequence therefore
        // acknowledges every older one cumulatively, and only the newest
        // command can still be replayed. Retaining older receipts for a time
        // horizon would turn ordinary fact cadence into a hidden rate limit.
        next.receipts.clear();
        next.apply_operation(request)?;
        next.accepted_sequence = request.sequence;
        let mut transaction = next
            .transactions
            .iter()
            .find(|tx| tx.transaction_id == request.transaction_id)
            .ok_or(QualityTransitionError::UnknownTransaction)?
            .clone();
        // A canonical acknowledgement records the accepted command state.
        // Media facts live once in the current ledger, never in every replay.
        transaction.ready.clear();
        transaction.reserved.clear();
        transaction.appended.clear();
        transaction.disposed.clear();
        let response = QualityTransitionReceipt {
            version: CONTINUOUS_QUALITY_VERSION,
            generation: next.generation.clone(),
            control_epoch: next.control_epoch,
            accepted_sequence: request.sequence,
            attachment: next.attachment.clone(),
            transaction,
        };
        next.receipts.push(ReplayReceipt {
            sequence: request.sequence,
            transaction_id: request.transaction_id.clone(),
            request_digest: quality_request_digest(request)?,
            transaction: response.transaction.clone(),
            accepted_at_ms: now_ms,
        });
        if serde_json::to_vec(&next)
            .map_err(|_| QualityTransitionError::Invalid)?
            .len()
            > MAX_QUALITY_LEDGER_BYTES
        {
            return Err(QualityTransitionError::Capacity);
        }
        *self = next;
        Ok(response)
    }

    fn apply_operation(
        &mut self,
        request: &QualityTransitionRequest,
    ) -> Result<(), QualityTransitionError> {
        if let QualityOperation::Prepare {
            intent_revision,
            target_rendition_id,
        } = &request.operation
        {
            if *intent_revision == 0
                || *intent_revision > JS_MAX_INTEGER
                || !valid_artifact(target_rendition_id)
            {
                return Err(QualityTransitionError::Invalid);
            }
            if let Some(existing) = self
                .transactions
                .iter()
                .find(|tx| tx.transaction_id == request.transaction_id)
            {
                return if existing.intent_revision == *intent_revision
                    && existing.target_rendition_id == *target_rendition_id
                {
                    Ok(())
                } else {
                    Err(QualityTransitionError::ConflictingReplay)
                };
            }
            if *intent_revision <= self.latest_intent_revision {
                return Err(QualityTransitionError::InvalidTransition);
            }
            self.transactions.retain(|tx| tx.unresolved());
            if self.transactions.len() >= MAX_QUALITY_TRANSACTIONS {
                return Err(QualityTransitionError::Capacity);
            }
            for previous in &mut self.transactions {
                if *intent_revision == self.latest_intent_revision {
                    continue;
                }
                previous.intent_superseded = true;
                previous.cancel_requested = true;
                if previous.reserved.is_empty()
                    && matches!(
                        previous.state,
                        QualityState::Preparing | QualityState::Ready
                    )
                {
                    previous.state = QualityState::Cancelling;
                }
            }
            self.latest_intent_revision = *intent_revision;
            self.transactions.push(QualityTransaction {
                transaction_id: request.transaction_id.clone(),
                intent_revision: *intent_revision,
                target_rendition_id: target_rendition_id.clone(),
                state: QualityState::Preparing,
                intent_superseded: false,
                cancel_requested: false,
                preparation: None,
                ready: Vec::new(),
                reserved: Vec::new(),
                appended: Vec::new(),
                ever_appended: false,
                disposed: Vec::new(),
                first_presented_tick: None,
                first_presented_at_ms: None,
            });
            return Ok(());
        }
        let at = self
            .transactions
            .iter()
            .position(|tx| tx.transaction_id == request.transaction_id)
            .ok_or(QualityTransitionError::UnknownTransaction)?;
        match &request.operation {
            QualityOperation::Scheduled { intervals } => {
                if intervals.is_empty()
                    || !matches!(
                        self.transactions[at].state,
                        QualityState::Ready
                            | QualityState::Scheduled
                            | QualityState::Appended
                            | QualityState::Presented
                    )
                    || self.transactions[at].intent_superseded
                    || self.transactions[at].cancel_requested
                {
                    return Err(QualityTransitionError::InvalidTransition);
                }
                for interval in intervals {
                    if !self.transactions[at].ready.contains(interval) {
                        return Err(QualityTransitionError::InvalidTransition);
                    }
                    if !self.transactions[at].reserved.contains(interval) {
                        self.transactions[at].reserved.push(interval.clone());
                    }
                }
                self.transactions[at].state = if self.transactions[at].appended.is_empty() {
                    QualityState::Scheduled
                } else {
                    self.transactions[at].state
                };
            }
            QualityOperation::Appended { intervals } => {
                self.record_appended(at, intervals)?;
            }
            QualityOperation::Presented {
                artifact_id,
                film_tick,
                observed_at_ms,
            } => {
                let tx = &mut self.transactions[at];
                if *observed_at_ms <= 0
                    || !tx.appended.iter().any(|interval| {
                        interval.artifact_id == *artifact_id && interval.contains(*film_tick)
                    })
                {
                    return Err(QualityTransitionError::InvalidTransition);
                }
                if tx.first_presented_tick.is_none() {
                    tx.first_presented_tick = Some(*film_tick);
                    tx.first_presented_at_ms = Some(*observed_at_ms);
                }
                tx.state = if tx.reserved.is_empty() {
                    QualityState::Disposed
                } else {
                    QualityState::Presented
                };
            }
            QualityOperation::CancelUnappended { completed } => {
                if !completed.is_empty() {
                    self.record_appended(at, completed)?;
                }
                let tx = &mut self.transactions[at];
                tx.cancel_requested = true;
                if tx.reserved.is_empty() && !tx.ever_appended {
                    tx.state = QualityState::Cancelling;
                }
                // Reserved dependencies stay pinned even if the owner has not
                // yet received append evidence. Cleanup needs an absence barrier
                // or completed transport disposal, never an older observation.
            }
            QualityOperation::Disposed { artifacts }
            | QualityOperation::RecoveryOwned {
                disposed_artifacts: artifacts,
            } => {
                let tx = &mut self.transactions[at];
                if artifacts.len() > MAX_QUALITY_INTERVALS
                    || artifacts.iter().any(|artifact| {
                        !tx.reserved
                            .iter()
                            .any(|interval| interval.artifact_id == *artifact)
                            && !self
                                .shared_audio_reserved
                                .iter()
                                .any(|interval| interval.artifact_id == *artifact)
                            && !tx.disposed.contains(artifact)
                    })
                {
                    return Err(QualityTransitionError::InvalidTransition);
                }
                for artifact in artifacts {
                    if !tx.disposed.contains(artifact) {
                        tx.disposed.push(artifact.clone());
                    }
                }
                self.shared_audio_reserved
                    .retain(|interval| !artifacts.contains(&interval.artifact_id));
                tx.reserved
                    .retain(|interval| !artifacts.contains(&interval.artifact_id));
                tx.appended
                    .retain(|interval| !artifacts.contains(&interval.artifact_id));
                tx.ready
                    .retain(|interval| !artifacts.contains(&interval.artifact_id));
                if tx.disposed.len() > MAX_QUALITY_INTERVALS {
                    tx.disposed
                        .drain(..tx.disposed.len() - MAX_QUALITY_INTERVALS);
                }
                if tx.reserved.is_empty() && tx.ever_appended {
                    tx.state = QualityState::Disposed;
                }
                if matches!(request.operation, QualityOperation::RecoveryOwned { .. }) {
                    tx.intent_superseded = true;
                    tx.state = QualityState::RecoveryOwned;
                }
            }
            QualityOperation::Prepare { .. } => unreachable!("handled above"),
        }
        let (count, bytes) = self.pinned_dependency_usage()?;
        if count > MAX_QUALITY_INTERVALS || bytes > MAX_QUALITY_PINNED_BYTES {
            return Err(QualityTransitionError::Capacity);
        }
        Ok(())
    }

    /// Logical intent owners can share one immutable physical dependency.
    /// The same identity with changed interval facts is never an alias.
    fn pinned_dependency_usage(&self) -> Result<(usize, u64), QualityTransitionError> {
        let mut unique = std::collections::BTreeMap::new();
        let mut bytes = 0_u64;
        for interval in self
            .transactions
            .iter()
            .flat_map(|tx| &tx.reserved)
            .chain(&self.shared_audio_reserved)
        {
            let key = (&interval.rendition_id, &interval.artifact_id);
            if let Some(previous) = unique.get(&key) {
                if *previous != interval {
                    return Err(QualityTransitionError::Invalid);
                }
            } else {
                bytes = bytes
                    .checked_add(interval.byte_length)
                    .ok_or(QualityTransitionError::Capacity)?;
                unique.insert(key, interval);
            }
        }
        Ok((unique.len(), bytes))
    }

    fn record_appended(
        &mut self,
        at: usize,
        intervals: &[QualityInterval],
    ) -> Result<(), QualityTransitionError> {
        let tx = &mut self.transactions[at];
        if intervals.is_empty()
            || intervals.iter().any(|interval| {
                !tx.reserved.contains(interval) || tx.disposed.contains(&interval.artifact_id)
            })
        {
            return Err(QualityTransitionError::InvalidTransition);
        }
        tx.ever_appended = true;
        for interval in intervals {
            if !tx.appended.contains(interval) {
                tx.appended.push(interval.clone());
            }
        }
        if tx.first_presented_tick.is_none() {
            tx.state = QualityState::Appended;
        }
        Ok(())
    }

    /// Bind the first owner's append frontier durably. A replay may not move
    /// the requested boundary or extend its original preparation deadline.
    pub fn bind_preparation(
        &mut self,
        transaction_id: &str,
        binding: QualityPreparationBinding,
    ) -> Result<(), QualityTransitionError> {
        if !binding.valid() {
            return Err(QualityTransitionError::Invalid);
        }
        let tx = self
            .transactions
            .iter_mut()
            .find(|tx| tx.transaction_id == transaction_id)
            .ok_or(QualityTransitionError::UnknownTransaction)?;
        if let Some(old) = &tx.preparation {
            return if old.timescale == binding.timescale && old.through_tick == binding.through_tick
            {
                Ok(())
            } else {
                Err(QualityTransitionError::ConflictingReplay)
            };
        }
        if tx.state != QualityState::Preparing || tx.cancel_requested || tx.intent_superseded {
            return Err(QualityTransitionError::InvalidTransition);
        }
        tx.preparation = Some(binding);
        Ok(())
    }

    /// Producer-only readiness, after immutable artifact/sample verification.
    pub fn ready(
        &mut self,
        transaction_id: &str,
        intervals: Vec<QualityInterval>,
    ) -> Result<(), QualityTransitionError> {
        let mut next = self.clone();
        next.record_ready(transaction_id, intervals)?;
        if serde_json::to_vec(&next)
            .map_err(|_| QualityTransitionError::Invalid)?
            .len()
            > MAX_QUALITY_LEDGER_BYTES
        {
            return Err(QualityTransitionError::Capacity);
        }
        *self = next;
        Ok(())
    }

    fn record_ready(
        &mut self,
        transaction_id: &str,
        intervals: Vec<QualityInterval>,
    ) -> Result<(), QualityTransitionError> {
        let tx = self
            .transactions
            .iter_mut()
            .find(|tx| tx.transaction_id == transaction_id)
            .ok_or(QualityTransitionError::UnknownTransaction)?;
        if tx.intent_superseded
            || tx.cancel_requested
            || !matches!(
                tx.state,
                QualityState::Preparing
                    | QualityState::Ready
                    | QualityState::Scheduled
                    | QualityState::Appended
                    | QualityState::Presented
                    | QualityState::Disposed
            )
            || intervals.is_empty()
            || intervals.len() > MAX_QUALITY_INTERVALS
            || intervals.iter().any(|interval| {
                !interval.valid() || interval.rendition_id != tx.target_rendition_id
            })
            || intervals.windows(2).any(|pair| {
                pair[0].timescale != pair[1].timescale || pair[0].through_tick != pair[1].from_tick
            })
        {
            return Err(QualityTransitionError::InvalidTransition);
        }
        if intervals.iter().any(|interval| {
            tx.reserved
                .iter()
                .chain(&tx.appended)
                .chain(&tx.ready)
                .any(|known| known.artifact_id == interval.artifact_id && known != interval)
        }) {
            return Err(QualityTransitionError::ConflictingReplay);
        }
        tx.ready = intervals;
        if tx.reserved.is_empty() {
            tx.state = QualityState::Ready;
        }
        Ok(())
    }

    /// Owner refusal before any transport reservation. This cannot erase
    /// scheduled or appended facts, and leaves canonical command receipts intact.
    pub fn refuse_preparation(
        &mut self,
        transaction_id: &str,
    ) -> Result<(), QualityTransitionError> {
        let tx = self
            .transactions
            .iter_mut()
            .find(|tx| tx.transaction_id == transaction_id)
            .ok_or(QualityTransitionError::UnknownTransaction)?;
        if matches!(
            tx.state,
            QualityState::RetainedCurrent | QualityState::Superseded
        ) {
            return Ok(());
        }
        if !tx.reserved.is_empty()
            || tx.ever_appended
            || !matches!(
                tx.state,
                QualityState::Preparing | QualityState::Ready | QualityState::Cancelling
            )
        {
            return Err(QualityTransitionError::InvalidTransition);
        }
        tx.ready.clear();
        tx.state = if tx.intent_superseded {
            QualityState::Superseded
        } else {
            QualityState::RetainedCurrent
        };
        Ok(())
    }

    /// Called only after optional producer cleanup has settled. A scheduled
    /// reservation is never cleared by cleanup alone, even before an append
    /// acknowledgement reached the owner.
    pub fn cancelled(&mut self, transaction_id: &str) -> Result<(), QualityTransitionError> {
        let tx = self
            .transactions
            .iter_mut()
            .find(|tx| tx.transaction_id == transaction_id)
            .ok_or(QualityTransitionError::UnknownTransaction)?;
        if matches!(
            tx.state,
            QualityState::RetainedCurrent | QualityState::Superseded
        ) {
            return Ok(());
        }
        if tx.state != QualityState::Cancelling || !tx.reserved.is_empty() || tx.ever_appended {
            return Err(QualityTransitionError::InvalidTransition);
        }
        tx.state = if tx.intent_superseded {
            QualityState::Superseded
        } else {
            QualityState::RetainedCurrent
        };
        tx.ready.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GENERATION: &str = "00000000-0000-4000-8000-00000000ca01";
    const TRANSACTION: &str = "00000000-0000-4000-8000-00000000ca02";
    fn ledger() -> QualityLedger {
        QualityLedger::new(
            GENERATION.into(),
            1,
            QualityAttachment {
                client_instance_id: "00000000-0000-4000-8000-00000000ca03".into(),
                lifetime_id: "movie".into(),
                attachment_id: "00000000-0000-4000-8000-00000000ca04".into(),
                family_id: "a".repeat(64),
            },
        )
        .expect("ledger")
    }
    fn interval() -> QualityInterval {
        QualityInterval {
            artifact_id: "b".repeat(64),
            rendition_id: "c".repeat(64),
            timescale: 24_000,
            from_tick: 240_240,
            through_tick: 288_288,
            byte_length: 500_000,
        }
    }
    fn request(
        ledger: &QualityLedger,
        sequence: u64,
        operation: QualityOperation,
    ) -> QualityTransitionRequest {
        QualityTransitionRequest {
            version: 1,
            generation: ledger.generation.clone(),
            control_epoch: ledger.control_epoch,
            sequence,
            attachment: ledger.attachment.clone(),
            transaction_id: TRANSACTION.into(),
            operation,
        }
    }
    #[test]
    fn immutable_pin_aliases_keep_one_physical_budget_and_exact_replay_after_restore() {
        let mut ledger = ledger();
        let videos: Vec<_> = (0..64_u64)
            .map(|i| QualityInterval {
                artifact_id: format!("{:064x}", i + 1),
                rendition_id: "c".repeat(64),
                timescale: 24_000,
                from_tick: i * 48_000,
                through_tick: (i + 1) * 48_000,
                byte_length: 500_000,
            })
            .collect();
        let audio: Vec<_> = (0..64_u64)
            .map(|i| QualityInterval {
                artifact_id: format!("{:064x}", i + 100),
                rendition_id: "e".repeat(64),
                timescale: 48_000,
                from_tick: i * 96_000,
                through_tick: (i + 1) * 96_000,
                byte_length: 40_000,
            })
            .collect();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: "c".repeat(64),
            },
        );
        ledger.apply(&prepare, 1000).expect("first owner");
        ledger
            .ready(TRANSACTION, videos.clone())
            .expect("first ready");
        ledger
            .reserve_shared_audio(&audio)
            .expect("one physical AAC budget");
        let scheduled = request(
            &ledger,
            2,
            QualityOperation::Scheduled {
                intervals: videos.clone(),
            },
        );
        let canonical = ledger
            .apply(&scheduled, 1100)
            .expect("first physical reservation");
        let second = "00000000-0000-4000-8000-00000000ca06";
        let mut prepare = request(
            &ledger,
            3,
            QualityOperation::Prepare {
                intent_revision: 2,
                target_rendition_id: "c".repeat(64),
            },
        );
        prepare.transaction_id = second.into();
        ledger.apply(&prepare, 1200).expect("new logical owner");
        ledger
            .ready(second, videos.clone())
            .expect("same retained immutable media");
        let mut alias = request(
            &ledger,
            4,
            QualityOperation::Scheduled { intervals: videos },
        );
        alias.transaction_id = second.into();
        let canonical_alias = ledger
            .apply(&alias, 1300)
            .expect("logical aliases do not double physical demand");
        assert_eq!(ledger.pinned_dependency_usage(), Ok((128, 64 * 540_000)));
        let encoded = serde_json::to_vec(&ledger).expect("durable owner facts");
        assert!(encoded.len() < MAX_QUALITY_LEDGER_BYTES);
        let mut restored: QualityLedger = serde_json::from_slice(&encoded).expect("restore");
        assert_eq!(
            restored.apply(&alias, 1400).expect("lost ACK exact replay"),
            canonical_alias
        );
        assert_ne!(canonical, canonical_alias);
        assert_eq!(
            restored.apply(&scheduled, 1450),
            Err(QualityTransitionError::StaleSequence),
            "a newer accepted command cumulatively acknowledges older ones"
        );
        let mut conflict = alias.clone();
        if let QualityOperation::Scheduled { intervals } = &mut conflict.operation {
            intervals[0].byte_length += 1;
        }
        assert_eq!(
            restored.apply(&conflict, 1500),
            Err(QualityTransitionError::ConflictingReplay)
        );
        restored.transactions[1].reserved[0].through_tick += 1000;
        assert!(
            !restored.valid(),
            "an immutable identity cannot alias changed interval facts"
        );
        let extra = QualityInterval {
            artifact_id: "f".repeat(64),
            from_tick: 64 * 96_000,
            through_tick: 65 * 96_000,
            ..audio[0].clone()
        };
        assert_eq!(
            ledger.reserve_shared_audio(&[extra]),
            Err(QualityTransitionError::Capacity)
        );
    }

    fn scheduled() -> QualityLedger {
        let mut ledger = ledger();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: "c".repeat(64),
            },
        );
        ledger.apply(&prepare, 1000).expect("prepare");
        ledger
            .ready(TRANSACTION, vec![interval()])
            .expect("verified ready");
        let schedule = request(
            &ledger,
            2,
            QualityOperation::Scheduled {
                intervals: vec![interval()],
            },
        );
        ledger.apply(&schedule, 1100).expect("schedule");
        ledger
    }
    #[test]
    fn unbatched_fact_cadence_never_exhausts_replay_receipts() {
        // Per-artifact facts at 2x playback: seven accepted commands every
        // two wall seconds (about 3.5/s), several times the rate a 90-second
        // receipt horizon with 128 entries could absorb.
        let mut ledger = ledger();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: interval().rendition_id,
            },
        );
        ledger.apply(&prepare, 1000).expect("prepare");
        let mut sequence = 2;
        for window in 0..300_u64 {
            let now = 2000 + window as i64 * 2000;
            let videos: Vec<_> = (0..2_u64)
                .map(|offset| QualityInterval {
                    artifact_id: format!("{:064x}", window * 4 + offset + 1),
                    rendition_id: interval().rendition_id,
                    timescale: 24_000,
                    from_tick: (window * 2 + offset) * 48_048,
                    through_tick: (window * 2 + offset + 1) * 48_048,
                    byte_length: 500_000,
                })
                .collect();
            let audio: Vec<_> = (0..2_u64)
                .map(|offset| QualityInterval {
                    artifact_id: format!("{:064x}", window * 4 + offset + 3),
                    rendition_id: "e".repeat(64),
                    timescale: 48_000,
                    from_tick: (window * 2 + offset) * 96_096,
                    through_tick: (window * 2 + offset + 1) * 96_096,
                    byte_length: 40_000,
                })
                .collect();
            ledger
                .ready(TRANSACTION, videos.clone())
                .expect("rolling readiness");
            ledger.reserve_shared_audio(&audio).expect("shared AAC");
            let scheduled = request(
                &ledger,
                sequence,
                QualityOperation::Scheduled {
                    intervals: videos.clone(),
                },
            );
            let canonical = ledger.apply(&scheduled, now).expect("schedule");
            assert!(
                canonical.transaction.reserved.is_empty(),
                "ACK does not duplicate current facts"
            );
            assert_eq!(ledger.transactions[0].reserved, videos);
            sequence += 1;
            for (offset, video) in videos.iter().enumerate() {
                let appended = request(
                    &ledger,
                    sequence,
                    QualityOperation::Appended {
                        intervals: vec![video.clone()],
                    },
                );
                ledger
                    .apply(&appended, now + 1 + offset as i64)
                    .expect("one completed append per command");
                sequence += 1;
            }
            if window == 0 {
                let presented = request(
                    &ledger,
                    sequence,
                    QualityOperation::Presented {
                        artifact_id: videos[0].artifact_id.clone(),
                        film_tick: 0,
                        observed_at_ms: now + 3,
                    },
                );
                ledger
                    .apply(&presented, now + 3)
                    .expect("first actual frame");
                sequence += 1;
            }
            let mut last = None;
            for (offset, artifact) in videos.iter().chain(&audio).enumerate() {
                let disposed = request(
                    &ledger,
                    sequence,
                    QualityOperation::Disposed {
                        artifacts: vec![artifact.artifact_id.clone()],
                    },
                );
                let receipt = ledger
                    .apply(&disposed, now + 4 + offset as i64)
                    .expect("one completed removal per command");
                last = Some((disposed, receipt));
                sequence += 1;
            }
            assert_eq!(ledger.receipts.len(), 1);
            let bytes = serde_json::to_vec(&ledger).expect("durable ledger");
            assert!(bytes.len() < MAX_QUALITY_LEDGER_BYTES);
            ledger = serde_json::from_slice(&bytes).expect("restore exact durable facts");
            assert!(ledger.valid());
            let (latest, receipt) = last.expect("disposal");
            assert_eq!(
                ledger
                    .apply(&latest, now + 1000)
                    .expect("lost canonical ACK replay of the newest command"),
                receipt
            );
            assert_eq!(
                ledger.apply(&scheduled, now + 1001),
                Err(QualityTransitionError::StaleSequence),
                "older commands are cumulatively acknowledged"
            );
            let mut conflict = latest.clone();
            if let QualityOperation::Disposed { artifacts } = &mut conflict.operation {
                artifacts[0] = "f".repeat(64);
            }
            assert_eq!(
                ledger.apply(&conflict, now + 1002),
                Err(QualityTransitionError::ConflictingReplay)
            );
            assert!(ledger.transactions[0].reserved.is_empty());
            assert!(ledger.shared_audio_reserved().is_empty());
        }
    }

    #[test]
    fn preparation_replay_cannot_move_frontier_or_extend_deadline() {
        let mut ledger = ledger();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: interval().rendition_id,
            },
        );
        ledger.apply(&prepare, 1000).expect("prepare");
        let binding = QualityPreparationBinding {
            timescale: 24000,
            through_tick: 240240,
            deadline_ms: 9000,
        };
        ledger
            .bind_preparation(TRANSACTION, binding.clone())
            .expect("first frontier");
        let mut replay = binding.clone();
        replay.deadline_ms = 19000;
        ledger
            .bind_preparation(TRANSACTION, replay.clone())
            .expect("same frontier");
        assert_eq!(ledger.transactions[0].preparation, Some(binding));
        let before = ledger.clone();
        replay.through_tick += 1;
        assert_eq!(
            ledger.bind_preparation(TRANSACTION, replay),
            Err(QualityTransitionError::ConflictingReplay)
        );
        assert_eq!(ledger, before);
        assert!(ledger.valid());
    }

    #[test]
    fn owner_refusal_preserves_replay_and_cannot_erase_scheduled_media() {
        let mut ledger = ledger();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: interval().rendition_id,
            },
        );
        let receipt = ledger.apply(&prepare, 1000).expect("prepare");
        ledger.ready(TRANSACTION, vec![interval()]).expect("ready");
        ledger
            .refuse_preparation(TRANSACTION)
            .expect("retain incumbent");
        assert_eq!(ledger.transactions[0].state, QualityState::RetainedCurrent);
        assert!(ledger.transactions[0].ready.is_empty());
        assert_eq!(
            ledger.apply(&prepare, 1100).expect("canonical replay"),
            receipt
        );
        assert!(ledger.valid());
        let mut scheduled = scheduled();
        let before = scheduled.clone();
        assert_eq!(
            scheduled.refuse_preparation(TRANSACTION),
            Err(QualityTransitionError::InvalidTransition)
        );
        assert_eq!(scheduled, before);
    }

    #[test]
    fn shared_audio_pins_survive_video_disposal_and_owner_takeover() {
        let mut ledger = scheduled();
        let audio = QualityInterval {
            artifact_id: "d".repeat(64),
            rendition_id: "e".repeat(64),
            timescale: 48_000,
            from_tick: 480_000,
            through_tick: 576_256,
            byte_length: 40_000,
        };
        ledger
            .reserve_shared_audio(&[audio.clone(), audio.clone()])
            .expect("verified AAC");
        assert_eq!(ledger.shared_audio_reserved(), std::slice::from_ref(&audio));
        let appended = request(
            &ledger,
            3,
            QualityOperation::Appended {
                intervals: vec![interval()],
            },
        );
        ledger.apply(&appended, 1200).expect("append");
        let dispose_video = request(
            &ledger,
            4,
            QualityOperation::Disposed {
                artifacts: vec![interval().artifact_id],
            },
        );
        ledger.apply(&dispose_video, 1300).expect("video consumed");
        assert!(ledger.transactions[0].reserved.is_empty());
        assert_eq!(
            ledger.shared_audio_reserved(),
            std::slice::from_ref(&audio),
            "video disposal cannot release shared audio"
        );
        ledger.adopt_epoch(2).expect("takeover");
        assert_eq!(ledger.shared_audio_reserved(), std::slice::from_ref(&audio));
        let dispose_audio = request(
            &ledger,
            1,
            QualityOperation::Disposed {
                artifacts: vec![audio.artifact_id.clone()],
            },
        );
        let receipt = ledger
            .apply(&dispose_audio, 1400)
            .expect("named audio transport disposal");
        assert!(ledger.shared_audio_reserved().is_empty());
        assert_eq!(
            ledger.shared_audio_rendition_id(),
            Some("e".repeat(64).as_str())
        );
        let mut different_audio = audio.clone();
        different_audio.rendition_id = "f".repeat(64);
        assert_eq!(
            ledger.reserve_shared_audio(&[different_audio]),
            Err(QualityTransitionError::Invalid),
            "disposing bytes does not authorize substituting the attachment's soundtrack"
        );
        assert_eq!(
            ledger.apply(&dispose_audio, 1500).expect("disposal replay"),
            receipt
        );
    }

    #[test]
    fn shared_audio_pin_capacity_and_conflicting_artifacts_refuse_atomically() {
        let mut ledger = scheduled();
        let audio = QualityInterval {
            artifact_id: "d".repeat(64),
            rendition_id: "e".repeat(64),
            timescale: 48_000,
            from_tick: 0,
            through_tick: 96_256,
            byte_length: MAX_QUALITY_PINNED_BYTES,
        };
        let before = ledger.clone();
        assert_eq!(
            ledger.reserve_shared_audio(std::slice::from_ref(&audio)),
            Err(QualityTransitionError::Capacity)
        );
        assert_eq!(ledger, before);
        let mut audio = audio;
        audio.byte_length = 40_000;
        ledger
            .reserve_shared_audio(std::slice::from_ref(&audio))
            .expect("bounded AAC");
        let before = ledger.clone();
        audio.through_tick += 1;
        assert_eq!(
            ledger.reserve_shared_audio(&[audio]),
            Err(QualityTransitionError::Invalid)
        );
        assert_eq!(ledger, before);
    }

    #[test]
    fn cancellation_carries_lost_append_facts_and_preserves_dependency_pins() {
        let mut ledger = scheduled();
        // Append completed, but its acknowledgement did not reach the owner.
        let cancel = request(
            &ledger,
            3,
            QualityOperation::CancelUnappended {
                completed: vec![interval()],
            },
        );
        let reply = ledger
            .apply(&cancel, 1200)
            .expect("cancel with append fact");
        assert_eq!(reply.transaction.state, QualityState::Appended);
        assert_eq!(reply.transaction.reserved, vec![interval()]);
        assert!(reply.transaction.ever_appended);
        assert_eq!(
            ledger.cancelled(TRANSACTION),
            Err(QualityTransitionError::InvalidTransition)
        );
        assert_eq!(
            ledger
                .apply(&cancel, 1300)
                .expect("lost cancel acknowledgement replay"),
            reply
        );
    }
    #[test]
    fn scheduled_cancellation_without_observation_cannot_release_a_reservation() {
        let mut ledger = scheduled();
        let cancel = request(
            &ledger,
            3,
            QualityOperation::CancelUnappended { completed: vec![] },
        );
        let reply = ledger
            .apply(&cancel, 1200)
            .expect("unknown append observation");
        assert_eq!(reply.transaction.state, QualityState::Scheduled);
        assert_eq!(reply.transaction.reserved, vec![interval()]);
        assert_eq!(
            ledger.cancelled(TRANSACTION),
            Err(QualityTransitionError::InvalidTransition)
        );
        let appended = request(
            &ledger,
            4,
            QualityOperation::Appended {
                intervals: vec![interval()],
            },
        );
        assert_eq!(
            ledger
                .apply(&appended, 1300)
                .expect("late completed append")
                .transaction
                .state,
            QualityState::Appended
        );
    }
    #[test]
    fn superseding_intent_does_not_erase_an_earlier_append_or_its_presentation() {
        let mut ledger = scheduled();
        let appended = request(
            &ledger,
            3,
            QualityOperation::Appended {
                intervals: vec![interval()],
            },
        );
        ledger.apply(&appended, 1200).expect("append");
        let mut newer = request(
            &ledger,
            4,
            QualityOperation::Prepare {
                intent_revision: 2,
                target_rendition_id: "d".repeat(64),
            },
        );
        newer.transaction_id = "00000000-0000-4000-8000-00000000ca05".into();
        ledger.apply(&newer, 1300).expect("newer intent");
        let presented = request(
            &ledger,
            5,
            QualityOperation::Presented {
                artifact_id: "b".repeat(64),
                film_tick: 241_241,
                observed_at_ms: 1400,
            },
        );
        let first = ledger
            .apply(&presented, 1450)
            .expect("earlier target really presents");
        assert!(first.transaction.intent_superseded);
        assert_eq!(first.transaction.state, QualityState::Presented);
        assert_eq!(first.transaction.first_presented_tick, Some(241_241));
        assert_eq!(first.transaction.reserved, vec![interval()]);
        assert_eq!(
            ledger.apply(&presented, 1600).expect("presentation replay"),
            first
        );
    }
    #[test]
    fn owner_takeover_preserves_appended_facts_but_fences_old_epoch_commands() {
        let mut ledger = scheduled();
        let appended = request(
            &ledger,
            3,
            QualityOperation::Appended {
                intervals: vec![interval()],
            },
        );
        ledger.apply(&appended, 1200).expect("append");
        ledger.adopt_epoch(2).expect("takeover");
        assert_eq!(
            ledger.apply(&appended, 1300),
            Err(QualityTransitionError::OwnerChanged)
        );
        assert_eq!(ledger.transactions[0].reserved, vec![interval()]);
        assert!(ledger.transactions[0].ever_appended);
        let presented = request(
            &ledger,
            1,
            QualityOperation::Presented {
                artifact_id: "b".repeat(64),
                film_tick: 241_241,
                observed_at_ms: 1400,
            },
        );
        assert_eq!(
            ledger
                .apply(&presented, 1450)
                .expect("new owner observation")
                .transaction
                .state,
            QualityState::Presented
        );
    }
    #[test]
    fn conflicting_replay_and_invalid_presentation_are_atomic_refusals() {
        let mut ledger = scheduled();
        let before = ledger.clone();
        let wrong = request(
            &ledger,
            2,
            QualityOperation::CancelUnappended { completed: vec![] },
        );
        assert_eq!(
            ledger.apply(&wrong, 1200),
            Err(QualityTransitionError::ConflictingReplay)
        );
        let premature = request(
            &ledger,
            3,
            QualityOperation::Presented {
                artifact_id: "b".repeat(64),
                film_tick: 241_241,
                observed_at_ms: 1200,
            },
        );
        assert_eq!(
            ledger.apply(&premature, 1200),
            Err(QualityTransitionError::InvalidTransition)
        );
        assert_eq!(ledger, before);
    }
    #[test]
    fn disposal_never_rewrites_an_appended_target_as_retained_current() {
        let mut ledger = scheduled();
        let appended = request(
            &ledger,
            3,
            QualityOperation::Appended {
                intervals: vec![interval()],
            },
        );
        ledger.apply(&appended, 1200).expect("append");
        let dispose = request(
            &ledger,
            4,
            QualityOperation::Disposed {
                artifacts: vec!["b".repeat(64)],
            },
        );
        ledger
            .apply(&dispose, 1300)
            .expect("completed transport disposal");
        let cancel = request(
            &ledger,
            5,
            QualityOperation::CancelUnappended { completed: vec![] },
        );
        let receipt = ledger.apply(&cancel, 1400).expect("cancel after disposal");
        assert_eq!(receipt.transaction.state, QualityState::Disposed);
        assert!(receipt.transaction.ever_appended);
        assert_eq!(
            ledger.cancelled(TRANSACTION),
            Err(QualityTransitionError::InvalidTransition)
        );
    }
    #[test]
    fn unknown_wire_fields_and_operations_are_rejected_separately_from_legacy_control() {
        let ledger = ledger();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: "c".repeat(64),
            },
        );
        let mut wire = serde_json::to_value(&prepare).expect("wire");
        assert_eq!(
            serde_json::from_value::<QualityTransitionRequest>(wire.clone())
                .expect("strict roundtrip"),
            prepare
        );
        wire["unknown_extension"] = serde_json::json!(true);
        assert!(serde_json::from_value::<QualityTransitionRequest>(wire.clone()).is_err());
        wire.as_object_mut()
            .expect("object")
            .remove("unknown_extension");
        wire["operation"]["kind"] = serde_json::json!("replace_legacy_control");
        assert!(serde_json::from_value::<QualityTransitionRequest>(wire).is_err());
    }
    #[test]
    fn cross_language_continuous_quality_fixture_preserves_the_lost_append_race() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/playback/continuous-quality/contract.json"
        ))
        .expect("fixture");
        let attachment =
            serde_json::from_value(fixture["initial_attachment"].clone()).expect("attachment");
        let mut ledger = QualityLedger::new(
            fixture["generation"].as_str().expect("generation").into(),
            1,
            attachment,
        )
        .expect("ledger");
        let mut now = 1000;
        for step in fixture["steps"].as_array().expect("steps") {
            now += 100;
            if let Some(ready) = step.get("owner_ready") {
                ledger
                    .ready(
                        ready["transaction_id"].as_str().expect("transaction"),
                        serde_json::from_value(ready["intervals"].clone()).expect("intervals"),
                    )
                    .expect("ready");
            } else if let Some(epoch) = step.get("adopt_epoch") {
                ledger
                    .adopt_epoch(epoch.as_u64().expect("epoch"))
                    .expect("takeover");
            } else {
                let request =
                    serde_json::from_value(step["request"].clone()).expect("strict request");
                let receipt = ledger.apply(&request, now).expect("transition");
                assert_eq!(
                    serde_json::to_value(receipt.transaction.state).expect("state"),
                    step["state"]
                );
                assert_eq!(
                    ledger
                        .transactions
                        .iter()
                        .find(|tx| tx.transaction_id == request.transaction_id)
                        .expect("current transaction")
                        .reserved
                        .len() as u64,
                    step["reserved"].as_u64().expect("pins")
                );
                assert!(ledger.valid());
            }
        }
    }

    #[test]
    fn transport_validation_refuses_duplicate_facts_and_mismatched_receipts() {
        let mut ledger = ledger();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: "c".repeat(64),
            },
        );
        assert!(prepare.valid());
        let receipt = ledger.apply(&prepare, 1000).expect("prepare");
        assert!(receipt.valid_for(&prepare));
        let mut wrong = receipt.clone();
        wrong.accepted_sequence += 1;
        assert!(!wrong.valid_for(&prepare));
        let mut wrong = receipt.clone();
        wrong.attachment.lifetime_id = "another player".into();
        assert!(!wrong.valid_for(&prepare));
        let mut wrong = receipt;
        wrong.transaction.state = QualityState::RetainedCurrent;
        wrong.transaction.ever_appended = true;
        assert!(!wrong.valid_for(&prepare));
        let duplicate = request(
            &ledger,
            2,
            QualityOperation::CancelUnappended {
                completed: vec![interval(), interval()],
            },
        );
        assert!(!duplicate.valid());
        let before = ledger.clone();
        assert_eq!(
            ledger.apply(&duplicate, 1100),
            Err(QualityTransitionError::Invalid)
        );
        assert_eq!(ledger, before);
        let oversized = request(
            &ledger,
            2,
            QualityOperation::Scheduled {
                intervals: vec![interval(); MAX_QUALITY_INTERVALS + 1],
            },
        );
        assert!(!oversized.valid());
        let mut uppercase = prepare;
        uppercase.operation = QualityOperation::Prepare {
            intent_revision: 2,
            target_rendition_id: "C".repeat(64),
        };
        assert!(!uppercase.valid());
    }

    #[test]
    fn normal_forward_and_back_buffers_fit_without_releasing_live_pins() {
        let mut ledger = ledger();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: interval().rendition_id,
            },
        );
        ledger.apply(&prepare, 1000).expect("prepare");
        let video: Vec<_> = (0..45_u64)
            .map(|index| QualityInterval {
                artifact_id: format!("{:064x}", index + 1),
                rendition_id: interval().rendition_id,
                timescale: 24_000,
                from_tick: index * 48_048,
                through_tick: (index + 1) * 48_048,
                byte_length: 500_000,
            })
            .collect();
        let audio: Vec<_> = (0..45_u64)
            .map(|index| QualityInterval {
                artifact_id: format!("{:064x}", index + 100),
                rendition_id: "e".repeat(64),
                timescale: 48_000,
                from_tick: index * 96_096,
                through_tick: (index + 1) * 96_096,
                byte_length: 40_000,
            })
            .collect();
        ledger
            .ready(TRANSACTION, video.clone())
            .expect("verified video");
        ledger
            .reserve_shared_audio(&audio)
            .expect("normal AAC buffer");
        let schedule = request(
            &ledger,
            2,
            QualityOperation::Scheduled {
                intervals: video.clone(),
            },
        );
        ledger.apply(&schedule, 1100).expect("normal video buffer");
        assert!(ledger.valid());
        assert_eq!(ledger.transactions[0].reserved, video);
        assert_eq!(ledger.shared_audio_reserved(), audio);
        assert!(serde_json::to_vec(&ledger).expect("bounded ledger").len() < 128 * 1024);
        let before = ledger.clone();
        let excess: Vec<_> = (0..39_u64)
            .map(|index| QualityInterval {
                artifact_id: format!("{:064x}", index + 200),
                rendition_id: "e".repeat(64),
                timescale: 48_000,
                from_tick: (index + 45) * 96_096,
                through_tick: (index + 46) * 96_096,
                byte_length: 40_000,
            })
            .collect();
        assert_eq!(
            ledger.reserve_shared_audio(&excess),
            Err(QualityTransitionError::Capacity)
        );
        assert_eq!(ledger, before, "capacity refusal preserves live pins");
    }

    #[test]
    fn pin_byte_backpressure_never_evicts_a_scheduled_interval() {
        let mut ledger = ledger();
        let prepare = request(
            &ledger,
            1,
            QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: "c".repeat(64),
            },
        );
        ledger.apply(&prepare, 1000).expect("prepare");
        let mut first = interval();
        first.byte_length = 129 * 1024 * 1024;
        let mut second = first.clone();
        second.artifact_id = "d".repeat(64);
        second.from_tick = first.through_tick;
        second.through_tick += 48048;
        ledger
            .ready(TRANSACTION, vec![first.clone(), second.clone()])
            .expect("verified intervals");
        let schedule = request(
            &ledger,
            2,
            QualityOperation::Scheduled {
                intervals: vec![first.clone()],
            },
        );
        ledger.apply(&schedule, 1100).expect("first reservation");
        let before = ledger.clone();
        let over = request(
            &ledger,
            3,
            QualityOperation::Scheduled {
                intervals: vec![second],
            },
        );
        assert_eq!(
            ledger.apply(&over, 1200),
            Err(QualityTransitionError::Capacity)
        );
        assert_eq!(ledger, before);
        assert_eq!(ledger.transactions[0].reserved, vec![first]);
    }
}
