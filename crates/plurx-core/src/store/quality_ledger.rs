//! Parent-owned continuous-quality ledger persistence and bounded CAS writes.
use crate::error::StoreError;
use crate::playback::continuous_quality::{QualityLedger, MAX_QUALITY_LEDGER_BYTES};

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS continuous_quality_ledgers (
    generation TEXT PRIMARY KEY,
    owner_node_id TEXT NOT NULL,
    owner_epoch INTEGER NOT NULL CHECK(owner_epoch > 0),
    revision INTEGER NOT NULL CHECK(revision > 0),
    attachment_id TEXT NOT NULL,
    ledger_json TEXT NOT NULL CHECK(json_valid(ledger_json) AND length(ledger_json) <= 131072),
    updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS continuous_quality_ledger_retention
    ON continuous_quality_ledgers(updated_at_ms, generation);";

pub(crate) const WRITE: &str = "INSERT INTO continuous_quality_ledgers
    (generation, owner_node_id, owner_epoch, revision, attachment_id, ledger_json, updated_at_ms)
    SELECT $1, $2, $3, $4 + 1, $5, $6, $7
    WHERE EXISTS (SELECT 1 FROM media_sessions parent WHERE parent.incarnation_id = $1
        AND parent.owner_node_id = $2 AND parent.owner_epoch = $3
        AND parent.state = 'active' AND parent.lease_expires_at_ms > $7)
      AND (($4 = 0 AND NOT EXISTS (SELECT 1 FROM continuous_quality_ledgers WHERE generation = $1))
        OR ($4 > 0 AND EXISTS (SELECT 1 FROM continuous_quality_ledgers WHERE generation = $1 AND revision = $4)))
    ON CONFLICT(generation) DO UPDATE SET owner_node_id = excluded.owner_node_id,
        owner_epoch = excluded.owner_epoch, revision = excluded.revision,
        ledger_json = excluded.ledger_json, updated_at_ms = excluded.updated_at_ms
    WHERE continuous_quality_ledgers.revision = $4
      AND continuous_quality_ledgers.attachment_id = $5
      AND continuous_quality_ledgers.owner_epoch <= $3";

pub(crate) const TERMINAL_WRITE: &str = "UPDATE continuous_quality_ledgers SET
    owner_node_id = $2, owner_epoch = $3, revision = $4 + 1, ledger_json = $6, updated_at_ms = $7
    WHERE generation = $1 AND revision = $4 AND attachment_id = $5 AND owner_epoch <= $3
      AND ledger_json = $8
      AND EXISTS (SELECT 1 FROM media_sessions parent WHERE parent.incarnation_id = $1
        AND parent.owner_node_id = $2 AND parent.owner_epoch = $3 AND parent.state = 'ended')";

pub(crate) struct TerminalQualityWrite {
    pub json: String,
    pub previous_json: String,
    pub epoch: i64,
    pub receipt: crate::playback::continuous_quality::QualityTransitionReceipt,
}

/// Terminal writes accept only client-completed facts for existing immutable
/// dependencies. SQL additionally compares the exact old ledger, so even a
/// forged snapshot cannot introduce a terminal reservation.
pub(crate) fn reduce_terminal_write(
    expected: &QualityLedgerSnapshot,
    request: &crate::playback::continuous_quality::QualityTransitionRequest,
    owner: &str,
    now_ms: i64,
) -> Result<TerminalQualityWrite, StoreError> {
    use crate::playback::continuous_quality::QualityOperation;
    if expected.revision <= 0
        || !expected.ledger.valid()
        || !request.valid()
        || matches!(
            request.operation,
            QualityOperation::Prepare { .. } | QualityOperation::Scheduled { .. }
        )
    {
        return Err(StoreError::Task(
            "terminal quality writes require existing transport facts".into(),
        ));
    }
    let mut ledger = expected.ledger.clone();
    if ledger.control_epoch < request.control_epoch {
        ledger
            .adopt_epoch(request.control_epoch)
            .map_err(|error| StoreError::Task(error.to_string()))?;
    }
    let receipt = ledger
        .apply(request, now_ms)
        .map_err(|error| StoreError::Task(error.to_string()))?;
    let json = encode_write(&ledger, owner, expected.revision, now_ms)?;
    let previous_json = serde_json::to_string(&expected.ledger)
        .map_err(|error| StoreError::Task(error.to_string()))?;
    let epoch =
        i64::try_from(ledger.control_epoch).map_err(|error| StoreError::Task(error.to_string()))?;
    Ok(TerminalQualityWrite {
        json,
        previous_json,
        epoch,
        receipt,
    })
}

pub(crate) const COLUMNS: &str = "owner_node_id, owner_epoch, revision, ledger_json, updated_at_ms";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualityLedgerSnapshot {
    pub owner_node_id: String,
    pub owner_epoch: i64,
    pub revision: i64,
    pub ledger: QualityLedger,
    pub updated_at_ms: i64,
}

pub(crate) fn encode_write(
    ledger: &QualityLedger,
    owner: &str,
    expected_revision: i64,
    now_ms: i64,
) -> Result<String, StoreError> {
    if owner.is_empty()
        || owner.len() > 256
        || expected_revision < 0
        || expected_revision == i64::MAX
        || now_ms <= 0
        || !ledger.valid()
        || ledger.version != 1
        || uuid::Uuid::parse_str(&ledger.generation).is_err()
        || !(1..=9_007_199_254_740_991).contains(&ledger.control_epoch)
        || !ledger.attachment.valid()
    {
        return Err(StoreError::Task(
            "invalid continuous-quality ledger write".into(),
        ));
    }
    let json =
        serde_json::to_string(ledger).map_err(|error| StoreError::Task(error.to_string()))?;
    if json.len() > MAX_QUALITY_LEDGER_BYTES {
        return Err(StoreError::Task(
            "continuous-quality ledger exceeds its byte bound".into(),
        ));
    }
    Ok(json)
}

pub(crate) fn decode_snapshot(
    owner_node_id: String,
    owner_epoch: i64,
    revision: i64,
    json: String,
    updated_at_ms: i64,
) -> Result<QualityLedgerSnapshot, StoreError> {
    if json.len() > MAX_QUALITY_LEDGER_BYTES {
        return Err(StoreError::Task(
            "oversized continuous-quality ledger".into(),
        ));
    }
    let ledger: QualityLedger =
        serde_json::from_str(&json).map_err(|error| StoreError::Task(error.to_string()))?;
    if !ledger.valid()
        || revision <= 0
        || i64::try_from(ledger.control_epoch).ok() != Some(owner_epoch)
    {
        return Err(StoreError::Task(
            "continuous-quality owner projection mismatch".into(),
        ));
    }
    Ok(QualityLedgerSnapshot {
        owner_node_id,
        owner_epoch,
        revision,
        ledger,
        updated_at_ms,
    })
}

/// Scheduled dependencies are read directly from the atomic ledger, including
/// historical appends and takeover epochs. Disposal removes them in that same
/// CAS write; receipt expiry and producer termination do not.
pub(crate) const RESERVED_INTERVALS: &str = "SELECT DISTINCT interval_json FROM (
    SELECT interval.value AS interval_json
    FROM continuous_quality_ledgers ledger,
         json_each(ledger.ledger_json, '$.transactions') transaction_fact,
         json_each(transaction_fact.value, '$.reserved') interval
    UNION ALL
    SELECT interval.value AS interval_json
    FROM continuous_quality_ledgers ledger,
         json_each(ledger.ledger_json, '$.shared_audio_reserved') interval
    ) WHERE json_extract(interval_json, '$.rendition_id') = $1
    LIMIT 4097";

pub(crate) fn decode_reserved_intervals(
    json: Vec<String>,
    rendition_id: &str,
) -> Result<Vec<crate::playback::continuous_quality::QualityInterval>, StoreError> {
    if json.len() > 4096 {
        return Err(StoreError::Task(
            "continuous dependency lookup exceeds its interval allowance".into(),
        ));
    }
    json.into_iter()
        .map(|value| {
            if value.len() > 1024 {
                return Err(StoreError::Task("oversized continuous dependency".into()));
            }
            let interval: crate::playback::continuous_quality::QualityInterval =
                serde_json::from_str(&value)
                    .map_err(|error| StoreError::Task(error.to_string()))?;
            if !interval.valid() || interval.rendition_id != rendition_id {
                return Err(StoreError::Task(
                    "invalid continuous dependency projection".into(),
                ));
            }
            Ok(interval)
        })
        .collect()
}
