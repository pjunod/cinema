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
pub const MAX_QUALITY_INTERVALS: usize = 64;
pub const MAX_QUALITY_PINNED_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_QUALITY_RECEIPTS: usize = 128;
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
pub struct QualityTransaction {
    pub transaction_id: String,
    pub intent_revision: u64,
    pub target_rendition_id: String,
    pub state: QualityState,
    pub intent_superseded: bool,
    pub cancel_requested: bool,
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
    request: QualityTransitionRequest,
    response: QualityTransitionReceipt,
    accepted_at_ms: i64,
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
        family: &crate::transcode::VodVideoFamily,
        now_ms: i64,
        deadline: std::time::Instant,
    ) -> impl std::future::Future<Output = Result<bool, String>> + Send;
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
        let mut count = 0_usize;
        let mut bytes = 0_u64;
        for tx in &self.transactions {
            if !valid_uuid(&tx.transaction_id)
                || !ids.insert(&tx.transaction_id)
                || tx.intent_revision == 0
                || tx.intent_revision > self.latest_intent_revision
                || !valid_artifact(&tx.target_rendition_id)
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
            count = count.saturating_add(tx.reserved.len());
            for interval in &tx.reserved {
                let Some(sum) = bytes.checked_add(interval.byte_length) else {
                    return false;
                };
                bytes = sum;
            }
        }
        count <= MAX_QUALITY_INTERVALS
            && bytes <= MAX_QUALITY_PINNED_BYTES
            && self.receipts.iter().all(|receipt| {
                receipt.request.generation == self.generation
                    && receipt.request.control_epoch == self.control_epoch
                    && receipt.request.attachment == self.attachment
                    && receipt.request.sequence > 0
                    && receipt.request.sequence <= self.accepted_sequence
                    && receipt.accepted_at_ms > 0
                    && receipt.response.accepted_sequence == receipt.request.sequence
                    && receipt.response.valid_for(&receipt.request)
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
            .find(|receipt| receipt.request.sequence == request.sequence)
        {
            return if receipt.request == *request {
                Ok(receipt.response.clone())
            } else {
                Err(QualityTransitionError::ConflictingReplay)
            };
        }
        if request.sequence <= self.accepted_sequence {
            return Err(QualityTransitionError::StaleSequence);
        }
        let mut next = self.clone();
        next.receipts.retain(|receipt| {
            receipt.accepted_at_ms >= now_ms.saturating_sub(QUALITY_RECEIPT_HORIZON_MS)
        });
        if next.receipts.len() >= MAX_QUALITY_RECEIPTS {
            return Err(QualityTransitionError::Capacity);
        }
        next.apply_operation(request)?;
        next.accepted_sequence = request.sequence;
        let transaction = next
            .transactions
            .iter()
            .find(|tx| tx.transaction_id == request.transaction_id)
            .ok_or(QualityTransitionError::UnknownTransaction)?
            .clone();
        let response = QualityTransitionReceipt {
            version: CONTINUOUS_QUALITY_VERSION,
            generation: next.generation.clone(),
            control_epoch: next.control_epoch,
            accepted_sequence: request.sequence,
            attachment: next.attachment.clone(),
            transaction,
        };
        next.receipts.push(ReplayReceipt {
            request: request.clone(),
            response: response.clone(),
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
        let mut reserved = self.transactions.iter().flat_map(|tx| &tx.reserved);
        let count = reserved.clone().count();
        let bytes = reserved
            .try_fold(0_u64, |sum, interval| sum.checked_add(interval.byte_length))
            .ok_or(QualityTransitionError::Capacity)?;
        if count > MAX_QUALITY_INTERVALS || bytes > MAX_QUALITY_PINNED_BYTES {
            return Err(QualityTransitionError::Capacity);
        }
        Ok(())
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
                    receipt.transaction.reserved.len() as u64,
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
