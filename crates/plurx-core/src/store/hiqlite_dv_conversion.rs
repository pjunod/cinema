//! Replicated permanent Dolby Vision conversion ledger.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;
use serde::Deserialize;

use super::dv_conversion::{
    eligibility_reason, validate_recovery_guard_identity, DV_CONVERSIONS_QUEUE_INDEX,
    DV_CONVERSIONS_RECOVERY_GUARD_INDEX, DV_CONVERSIONS_SCHEMA, DV_QUEUE_ADMISSION_TRIGGER,
    DV_RECOVERY_GUARDS_FILE_INDEX, DV_RECOVERY_GUARDS_SCHEMA,
};
use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use super::{
    keys, DvConversion, DvConversionCandidate, DvConversionMode, DvConversionProgress,
    DvConversionProgressSnapshot, DvConversionQueueBatch, DvConversionState, DvConversionStore,
    DvRecoveryGuard, DvRecoveryGuardSnapshot, DvRecoveryGuardState, DvRecoveryGuardSummary,
    QueueDvConversionOutcome, DV_CONVERSION_LEDGER_READ_MAX, DV_CONVERSION_QUEUE_BATCH_MAX,
    DV_RECOVERY_GUARD_READ_MAX,
};
use crate::error::StoreError;

#[cfg(feature = "hiqlite-contract-tests")]
type QueueAdmissionReturnPause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

/// Contract-only seam after the replicated statement has classified and
/// admitted the request but before its immutable result envelope is decoded.
/// A concurrent scan can change both facts and ledger while the caller is
/// paused; the returned outcome must remain the one from the Raft mutation.
#[cfg(feature = "hiqlite-contract-tests")]
static QUEUE_ADMISSION_RETURN_PAUSE: std::sync::LazyLock<
    std::sync::Mutex<Option<QueueAdmissionReturnPause>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

#[cfg(feature = "hiqlite-contract-tests")]
impl HiqliteAuthStore {
    pub fn validation_pause_next_queue_admission_after_return() -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_sender, reached_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
        let mut pause = QUEUE_ADMISSION_RETURN_PAUSE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            pause.is_none(),
            "queue admission return pause already armed"
        );
        *pause = Some((reached_sender, release_receiver));
        (reached_receiver, release_sender)
    }
}

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    for sql in [
        DV_CONVERSIONS_SCHEMA,
        DV_CONVERSIONS_QUEUE_INDEX,
        DV_RECOVERY_GUARDS_SCHEMA,
        DV_CONVERSIONS_RECOVERY_GUARD_INDEX,
        DV_RECOVERY_GUARDS_FILE_INDEX,
        DV_QUEUE_ADMISSION_TRIGGER,
    ] {
        validate_sql(sql)?;
        for result in timeout_store(client.batch(sql)).await? {
            result.map_err(database_error)?;
        }
    }
    Ok(())
}

const JOINED_CONVERSION_COLS: &str = "d.file_id AS file_id, d.state AS state,
    d.el_type AS el_type, d.original_path AS original_path,
    d.bytes_before AS bytes_before, d.bytes_after AS bytes_after, d.error AS error,
    d.queued_at_ms AS queued_at_ms, d.finished_at_ms AS finished_at_ms,
    d.recovery_guard_id AS recovery_guard_id, g.guard_id AS guard_id,
    g.file_id AS guard_file_id, g.library_id AS guard_library_id,
    g.source_path AS guard_source_path, g.recovery_path AS guard_recovery_path,
    g.state AS guard_state, g.created_at_ms AS guard_created_at_ms,
    g.updated_at_ms AS guard_updated_at_ms";

const GUARD_COLS: &str = "guard_id, file_id, library_id, source_path, recovery_path,
    state, created_at_ms, updated_at_ms";

const QUALIFIED_GUARD_COLS: &str = "g.guard_id AS guard_id, g.file_id AS file_id,
    g.library_id AS library_id, g.source_path AS source_path,
    g.recovery_path AS recovery_path, g.state AS state,
    g.created_at_ms AS created_at_ms, g.updated_at_ms AS updated_at_ms";

struct ConversionRow {
    file_id: i64,
    state: String,
    el_type: Option<String>,
    original_path: Option<String>,
    bytes_before: Option<i64>,
    bytes_after: Option<i64>,
    error: Option<String>,
    queued_at_ms: i64,
    finished_at_ms: Option<i64>,
}

impl From<&mut Row<'_>> for ConversionRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            file_id: row.get("file_id"),
            state: row.get("state"),
            el_type: row.get("el_type"),
            original_path: row.get("original_path"),
            bytes_before: row.get("bytes_before"),
            bytes_after: row.get("bytes_after"),
            error: row.get("error"),
            queued_at_ms: row.get("queued_at_ms"),
            finished_at_ms: row.get("finished_at_ms"),
        }
    }
}

impl ConversionRow {
    fn finish(self) -> Result<DvConversion, StoreError> {
        let state = DvConversionState::parse(&self.state).ok_or_else(|| {
            StoreError::Database(format!(
                "invalid Dolby Vision conversion state `{}`",
                self.state
            ))
        })?;
        Ok(DvConversion {
            file_id: self.file_id,
            state,
            el_type: self.el_type,
            original_path: self.original_path,
            bytes_before: self.bytes_before,
            bytes_after: self.bytes_after,
            error: self.error,
            queued_at_ms: self.queued_at_ms,
            finished_at_ms: self.finished_at_ms,
            recovery_guard: None,
        })
    }
}

#[derive(Deserialize)]
struct QueueAdmissionEnvelope {
    outcome: String,
    requested_file_id: i64,
    container: Option<String>,
    profile: Option<i64>,
    bl_compat_id: Option<i64>,
    el_present: Option<i64>,
    rpu_present: Option<i64>,
    conversion_file_id: Option<i64>,
    conversion_state: Option<String>,
    el_type: Option<String>,
    original_path: Option<String>,
    bytes_before: Option<i64>,
    bytes_after: Option<i64>,
    error: Option<String>,
    queued_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    recovery_guard_id: Option<String>,
    guard_id: Option<String>,
    guard_file_id: Option<i64>,
    guard_library_id: Option<i64>,
    guard_source_path: Option<String>,
    guard_recovery_path: Option<String>,
    guard_state: Option<String>,
    guard_created_at_ms: Option<i64>,
    guard_updated_at_ms: Option<i64>,
}

impl QueueAdmissionEnvelope {
    fn conversion(&self) -> Result<DvConversion, StoreError> {
        let file_id = self.conversion_file_id.ok_or_else(|| {
            StoreError::Database(format!(
                "Dolby Vision queue outcome `{}` has no conversion row",
                self.outcome
            ))
        })?;
        if file_id != self.requested_file_id {
            return Err(StoreError::Database(
                "Dolby Vision queue outcome returned the wrong file id".to_owned(),
            ));
        }
        let state_value = self.conversion_state.as_deref().ok_or_else(|| {
            StoreError::Database("Dolby Vision queue outcome has no conversion state".to_owned())
        })?;
        let state = DvConversionState::parse(state_value).ok_or_else(|| {
            StoreError::Database(format!(
                "invalid Dolby Vision conversion state `{state_value}`"
            ))
        })?;
        let mut conversion = DvConversion {
            file_id,
            state,
            el_type: self.el_type.clone(),
            original_path: self.original_path.clone(),
            bytes_before: self.bytes_before,
            bytes_after: self.bytes_after,
            error: self.error.clone(),
            queued_at_ms: self.queued_at_ms.ok_or_else(|| {
                StoreError::Database("Dolby Vision queue outcome has no queue timestamp".to_owned())
            })?,
            finished_at_ms: self.finished_at_ms,
            recovery_guard: None,
        };
        if let Some(recovery_guard_id) = self.recovery_guard_id.as_deref() {
            let guard_id = self.guard_id.clone().ok_or_else(|| {
                StoreError::Database(format!(
                    "Dolby Vision conversion links missing recovery guard `{recovery_guard_id}`"
                ))
            })?;
            if guard_id != recovery_guard_id {
                return Err(StoreError::Database(
                    "Dolby Vision queue outcome joined the wrong recovery guard".to_owned(),
                ));
            }
            let guard_state_value = self.guard_state.as_deref().ok_or_else(|| {
                StoreError::Database("linked Dolby Vision recovery guard has no state".to_owned())
            })?;
            let guard_state = DvRecoveryGuardState::parse(guard_state_value).ok_or_else(|| {
                StoreError::Database(format!(
                    "invalid Dolby Vision recovery guard state `{guard_state_value}`"
                ))
            })?;
            conversion.recovery_guard = Some(DvRecoveryGuard {
                guard_id,
                file_id: self.guard_file_id.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no file id".to_owned())
                })?,
                library_id: self.guard_library_id.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no library id".to_owned())
                })?,
                source_path: self.guard_source_path.clone().ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no source path".to_owned())
                })?,
                recovery_path: self.guard_recovery_path.clone().ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no recovery path".to_owned())
                })?,
                state: guard_state,
                created_at_ms: self.guard_created_at_ms.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no create time".to_owned())
                })?,
                updated_at_ms: self.guard_updated_at_ms.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no update time".to_owned())
                })?,
            });
        }
        Ok(conversion)
    }

    fn finish(self) -> Result<QueueDvConversionOutcome, StoreError> {
        match self.outcome.as_str() {
            "queued" => {
                let conversion = self.conversion()?;
                if conversion.state != DvConversionState::Queued {
                    return Err(StoreError::Database(
                        "queued Dolby Vision admission returned a non-queued row".to_owned(),
                    ));
                }
                Ok(QueueDvConversionOutcome::Queued(conversion))
            }
            "already_active" => {
                let conversion = self.conversion()?;
                if !matches!(
                    conversion.state,
                    DvConversionState::Queued
                        | DvConversionState::Running
                        | DvConversionState::Verified
                ) {
                    return Err(StoreError::Database(
                        "active Dolby Vision admission returned a terminal row".to_owned(),
                    ));
                }
                Ok(QueueDvConversionOutcome::AlreadyActive(conversion))
            }
            "already_committed" => {
                let conversion = self.conversion()?;
                if conversion.state != DvConversionState::Committed {
                    return Err(StoreError::Database(
                        "committed Dolby Vision admission returned a non-committed row".to_owned(),
                    ));
                }
                Ok(QueueDvConversionOutcome::AlreadyCommitted(conversion))
            }
            "ineligible" => {
                let reason = eligibility_reason(
                    self.container.as_deref(),
                    self.profile,
                    self.bl_compat_id,
                    self.el_present.map(|value| value != 0),
                    self.rpu_present.map(|value| value != 0),
                )
                .ok_or_else(|| {
                    StoreError::Database(
                        "replicated Dolby Vision admission classified eligible facts as ineligible"
                            .to_owned(),
                    )
                })?;
                Ok(QueueDvConversionOutcome::Ineligible(reason))
            }
            "file_missing" => Ok(QueueDvConversionOutcome::FileMissing),
            outcome => Err(StoreError::Database(format!(
                "invalid Dolby Vision queue outcome `{outcome}`"
            ))),
        }
    }
}

struct QueueAdmissionRow {
    envelope: String,
}

impl From<&mut Row<'_>> for QueueAdmissionRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            envelope: row.get("envelope"),
        }
    }
}

struct GuardRow {
    guard_id: String,
    file_id: i64,
    library_id: i64,
    source_path: String,
    recovery_path: String,
    state: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl From<&mut Row<'_>> for GuardRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            guard_id: row.get("guard_id"),
            file_id: row.get("file_id"),
            library_id: row.get("library_id"),
            source_path: row.get("source_path"),
            recovery_path: row.get("recovery_path"),
            state: row.get("state"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        }
    }
}

impl GuardRow {
    fn finish(self) -> Result<DvRecoveryGuard, StoreError> {
        let state = DvRecoveryGuardState::parse(&self.state).ok_or_else(|| {
            StoreError::Database(format!(
                "invalid Dolby Vision recovery guard state `{}`",
                self.state
            ))
        })?;
        Ok(DvRecoveryGuard {
            guard_id: self.guard_id,
            file_id: self.file_id,
            library_id: self.library_id,
            source_path: self.source_path,
            recovery_path: self.recovery_path,
            state,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        })
    }
}

struct JoinedConversionRow {
    conversion: ConversionRow,
    recovery_guard_id: Option<String>,
    guard_id: Option<String>,
    guard_file_id: Option<i64>,
    guard_library_id: Option<i64>,
    guard_source_path: Option<String>,
    guard_recovery_path: Option<String>,
    guard_state: Option<String>,
    guard_created_at_ms: Option<i64>,
    guard_updated_at_ms: Option<i64>,
}

impl From<&mut Row<'_>> for JoinedConversionRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            conversion: ConversionRow::from(&mut *row),
            recovery_guard_id: row.get("recovery_guard_id"),
            guard_id: row.get("guard_id"),
            guard_file_id: row.get("guard_file_id"),
            guard_library_id: row.get("guard_library_id"),
            guard_source_path: row.get("guard_source_path"),
            guard_recovery_path: row.get("guard_recovery_path"),
            guard_state: row.get("guard_state"),
            guard_created_at_ms: row.get("guard_created_at_ms"),
            guard_updated_at_ms: row.get("guard_updated_at_ms"),
        }
    }
}

impl JoinedConversionRow {
    fn finish(self) -> Result<DvConversion, StoreError> {
        let mut conversion = self.conversion.finish()?;
        if let Some(recovery_guard_id) = self.recovery_guard_id {
            let guard_id = self.guard_id.ok_or_else(|| {
                StoreError::Database(format!(
                    "Dolby Vision conversion links missing recovery guard `{recovery_guard_id}`"
                ))
            })?;
            debug_assert_eq!(recovery_guard_id, guard_id);
            let state_value = self.guard_state.ok_or_else(|| {
                StoreError::Database("linked Dolby Vision recovery guard has no state".to_owned())
            })?;
            let state = DvRecoveryGuardState::parse(&state_value).ok_or_else(|| {
                StoreError::Database(format!(
                    "invalid Dolby Vision recovery guard state `{state_value}`"
                ))
            })?;
            conversion.recovery_guard = Some(DvRecoveryGuard {
                guard_id,
                file_id: self.guard_file_id.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no file id".to_owned())
                })?,
                library_id: self.guard_library_id.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no library id".to_owned())
                })?,
                source_path: self.guard_source_path.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no source path".to_owned())
                })?,
                recovery_path: self.guard_recovery_path.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no recovery path".to_owned())
                })?,
                state,
                created_at_ms: self.guard_created_at_ms.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no create time".to_owned())
                })?,
                updated_at_ms: self.guard_updated_at_ms.ok_or_else(|| {
                    StoreError::Database("linked recovery guard has no update time".to_owned())
                })?,
            });
        }
        Ok(conversion)
    }
}

struct CandidateRow {
    file_id: i64,
    library_id: i64,
    state: String,
}

impl From<&mut Row<'_>> for CandidateRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            file_id: row.get("file_id"),
            library_id: row.get("library_id"),
            state: row.get("state"),
        }
    }
}

struct ProgressRow(DvConversionProgress);

impl From<&mut Row<'_>> for ProgressRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(DvConversionProgress {
            eligible: row.get("eligible"),
            queued: row.get("queued"),
            running: row.get("running"),
            verified: row.get("verified"),
            committed: row.get("committed"),
            failed: row.get("failed"),
        })
    }
}

struct GuardSummaryRow(DvRecoveryGuardSummary);

impl From<&mut Row<'_>> for GuardSummaryRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(DvRecoveryGuardSummary {
            intent: row.get("intent"),
            active: row.get("active"),
            guard_removed: row.get("guard_removed"),
            scratch_removed: row.get("scratch_removed"),
            orphaned: row.get("orphaned"),
        })
    }
}

struct GuardSnapshotRow {
    summary: DvRecoveryGuardSummary,
    guard_id: Option<String>,
    file_id: Option<i64>,
    library_id: Option<i64>,
    source_path: Option<String>,
    recovery_path: Option<String>,
    state: Option<String>,
    created_at_ms: Option<i64>,
    updated_at_ms: Option<i64>,
}

impl From<&mut Row<'_>> for GuardSnapshotRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            summary: GuardSummaryRow::from(&mut *row).0,
            guard_id: row.get("page_guard_id"),
            file_id: row.get("page_file_id"),
            library_id: row.get("page_library_id"),
            source_path: row.get("page_source_path"),
            recovery_path: row.get("page_recovery_path"),
            state: row.get("page_state"),
            created_at_ms: row.get("page_created_at_ms"),
            updated_at_ms: row.get("page_updated_at_ms"),
        }
    }
}

impl GuardSnapshotRow {
    fn finish(self) -> Result<Option<DvRecoveryGuard>, StoreError> {
        let Some(guard_id) = self.guard_id else {
            return Ok(None);
        };
        let state_value = self
            .state
            .ok_or_else(|| StoreError::Database("orphan snapshot row has no state".to_owned()))?;
        let state = DvRecoveryGuardState::parse(&state_value).ok_or_else(|| {
            StoreError::Database(format!(
                "invalid Dolby Vision recovery guard state `{state_value}`"
            ))
        })?;
        Ok(Some(DvRecoveryGuard {
            guard_id,
            file_id: self.file_id.ok_or_else(|| {
                StoreError::Database("orphan snapshot row has no file id".to_owned())
            })?,
            library_id: self.library_id.ok_or_else(|| {
                StoreError::Database("orphan snapshot row has no library id".to_owned())
            })?,
            source_path: self.source_path.ok_or_else(|| {
                StoreError::Database("orphan snapshot row has no source path".to_owned())
            })?,
            recovery_path: self.recovery_path.ok_or_else(|| {
                StoreError::Database("orphan snapshot row has no recovery path".to_owned())
            })?,
            state,
            created_at_ms: self.created_at_ms.ok_or_else(|| {
                StoreError::Database("orphan snapshot row has no create time".to_owned())
            })?,
            updated_at_ms: self.updated_at_ms.ok_or_else(|| {
                StoreError::Database("orphan snapshot row has no update time".to_owned())
            })?,
        }))
    }
}

struct LibraryProgressRow {
    library_id: i64,
    progress: DvConversionProgress,
}

impl From<&mut Row<'_>> for LibraryProgressRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            library_id: row.get("library_id"),
            progress: DvConversionProgress {
                eligible: row.get("eligible"),
                queued: row.get("queued"),
                running: row.get("running"),
                verified: row.get("verified"),
                committed: row.get("committed"),
                failed: row.get("failed"),
            },
        }
    }
}

struct PresentRow {
    present: i64,
}

struct EligibilityRow {
    file_id: i64,
    eligible: i64,
}

impl From<&mut Row<'_>> for EligibilityRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            file_id: row.get("file_id"),
            eligible: row.get("eligible"),
        }
    }
}

impl From<&mut Row<'_>> for PresentRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            present: row.get("present"),
        }
    }
}

fn one_joined_conversion(
    mut rows: Vec<JoinedConversionRow>,
) -> Result<Option<DvConversion>, StoreError> {
    if rows.len() > 1 {
        return Err(StoreError::Database(
            "duplicate Dolby Vision conversion ledger row".to_owned(),
        ));
    }
    rows.pop().map(JoinedConversionRow::finish).transpose()
}

fn one_guard(mut rows: Vec<GuardRow>) -> Result<Option<DvRecoveryGuard>, StoreError> {
    if rows.len() > 1 {
        return Err(StoreError::Database(
            "duplicate Dolby Vision recovery guard row".to_owned(),
        ));
    }
    rows.pop().map(GuardRow::finish).transpose()
}

#[async_trait]
impl DvConversionStore for HiqliteAuthStore {
    async fn dv_conversion(&self, file_id: i64) -> Result<Option<DvConversion>, StoreError> {
        let sql = format!(
            "SELECT {JOINED_CONVERSION_COLS} FROM dv_conversions d
             LEFT JOIN dv_recovery_guards g ON g.guard_id = d.recovery_guard_id
             WHERE d.file_id = $1"
        );
        one_joined_conversion(
            self.client()
                .query_consistent_map::<JoinedConversionRow, _>(sql, params!(file_id))
                .await?,
        )
    }

    async fn dv_conversions_for_files(
        &self,
        file_ids: &[i64],
    ) -> Result<Vec<DvConversion>, StoreError> {
        if file_ids.is_empty() {
            return Ok(Vec::new());
        }
        if file_ids.len() > DV_CONVERSION_LEDGER_READ_MAX {
            return Err(StoreError::Task(format!(
                "Dolby Vision ledger read has {} ids, maximum is {DV_CONVERSION_LEDGER_READ_MAX}",
                file_ids.len()
            )));
        }
        let encoded = serde_json::to_string(file_ids).map_err(database_error)?;
        self.client()
            .query_consistent_map::<JoinedConversionRow, _>(
                format!(
                    "SELECT {JOINED_CONVERSION_COLS} FROM dv_conversions d
                      LEFT JOIN dv_recovery_guards g ON g.guard_id = d.recovery_guard_id
                      WHERE d.file_id IN (
                        SELECT CAST(value AS INTEGER) FROM json_each($1))
                      ORDER BY d.file_id"
                ),
                params!(encoded),
            )
            .await?
            .into_iter()
            .map(JoinedConversionRow::finish)
            .collect()
    }

    async fn dv_conversion_eligibility_for_files(
        &self,
        file_ids: &[i64],
    ) -> Result<BTreeMap<i64, bool>, StoreError> {
        if file_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        if file_ids.len() > DV_CONVERSION_LEDGER_READ_MAX {
            return Err(StoreError::Task(format!(
                "Dolby Vision eligibility read has {} ids, maximum is {DV_CONVERSION_LEDGER_READ_MAX}",
                file_ids.len()
            )));
        }
        let encoded = serde_json::to_string(file_ids).map_err(database_error)?;
        Ok(self
            .client()
            .query_consistent_map::<EligibilityRow, _>(
                "SELECT id AS file_id,
                        CASE WHEN LOWER(container) = 'mkv'
                                   AND dv_profile = 7
                                   AND dv_bl_compat_id IN (1, 6)
                                   AND dv_el_present = 1 AND dv_rpu_present = 1
                             THEN 1 ELSE 0 END AS eligible
                   FROM files
                  WHERE id IN (
                    SELECT CAST(value AS INTEGER) FROM json_each($1))
                  ORDER BY id",
                params!(encoded),
            )
            .await?
            .into_iter()
            .map(|row| (row.file_id, row.eligible != 0))
            .collect())
    }

    async fn dv_conversion_eligible(&self, file_id: i64) -> Result<bool, StoreError> {
        let rows = self
            .client()
            .query_consistent_map::<PresentRow, _>(
                "SELECT 1 AS present FROM files
                  WHERE id = $1 AND LOWER(container) = 'mkv' AND dv_profile = 7
                    AND dv_bl_compat_id IN (1, 6)
                    AND dv_el_present = 1 AND dv_rpu_present = 1",
                params!(file_id),
            )
            .await?;
        Ok(rows.into_iter().next().is_some_and(|row| row.present == 1))
    }

    async fn queue_dv_conversion(
        &self,
        file_id: i64,
        queued_at_ms: i64,
    ) -> Result<QueueDvConversionOutcome, StoreError> {
        let request_key = format!(
            "__plurx_internal.dv_queue_admission.{}",
            uuid::Uuid::new_v4().simple()
        );
        let row = self
            .client()
            .execute_returning_map_one::<_, QueueAdmissionRow>(
                "WITH requested(requested_file_id, requested_queued_at_ms, request_key) AS
                       (VALUES ($1, $2, $3)),
                 snapshot AS (
                   SELECT requested.*,
                          f.id AS present_file_id, f.container AS container,
                          f.dv_profile AS profile, f.dv_bl_compat_id AS bl_compat_id,
                          f.dv_el_present AS el_present, f.dv_rpu_present AS rpu_present,
                          CASE WHEN LOWER(f.container) = 'mkv' AND f.dv_profile = 7
                                      AND f.dv_bl_compat_id IN (1, 6)
                                      AND f.dv_el_present = 1 AND f.dv_rpu_present = 1
                               THEN 1 ELSE 0 END AS eligible,
                          d.file_id AS conversion_file_id,
                          d.state AS conversion_state, d.el_type AS el_type,
                          d.original_path AS original_path,
                          d.bytes_before AS bytes_before, d.bytes_after AS bytes_after,
                          d.error AS error, d.queued_at_ms AS queued_at_ms,
                          d.finished_at_ms AS finished_at_ms,
                          d.recovery_guard_id AS recovery_guard_id,
                          g.guard_id AS guard_id, g.file_id AS guard_file_id,
                          g.library_id AS guard_library_id,
                          g.source_path AS guard_source_path,
                          g.recovery_path AS guard_recovery_path,
                          g.state AS guard_state,
                          g.created_at_ms AS guard_created_at_ms,
                          g.updated_at_ms AS guard_updated_at_ms
                     FROM requested
                LEFT JOIN files f ON f.id = requested.requested_file_id
                LEFT JOIN dv_conversions d ON d.file_id = f.id
                LEFT JOIN dv_recovery_guards g ON g.guard_id = d.recovery_guard_id
                 ), classified AS (
                   SELECT snapshot.*,
                          CASE
                            WHEN conversion_state = 'committed' THEN 'already_committed'
                            WHEN conversion_state IN ('queued', 'running', 'verified')
                              THEN 'already_active'
                            WHEN present_file_id IS NULL THEN 'file_missing'
                            WHEN eligible = 1 AND
                                 (conversion_state IS NULL OR conversion_state = 'failed')
                              THEN 'queued'
                            ELSE 'ineligible'
                          END AS outcome
                     FROM snapshot
                 )
                 INSERT INTO settings (key, value, updated_at)
                 SELECT request_key,
                        json_object(
                          'outcome', outcome,
                          'requested_file_id', requested_file_id,
                          'requested_queued_at_ms', requested_queued_at_ms,
                          'container', container,
                          'profile', profile,
                          'bl_compat_id', bl_compat_id,
                          'el_present', el_present,
                          'rpu_present', rpu_present,
                          'conversion_file_id',
                            CASE WHEN outcome = 'queued' THEN requested_file_id
                                 ELSE conversion_file_id END,
                          'conversion_state',
                            CASE WHEN outcome = 'queued' THEN 'queued'
                                 ELSE conversion_state END,
                          'el_type', CASE WHEN outcome = 'queued' THEN NULL ELSE el_type END,
                          'original_path',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE original_path END,
                          'bytes_before',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE bytes_before END,
                          'bytes_after',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE bytes_after END,
                          'error', CASE WHEN outcome = 'queued' THEN NULL ELSE error END,
                          'queued_at_ms',
                            CASE WHEN outcome = 'queued' THEN requested_queued_at_ms
                                 ELSE queued_at_ms END,
                          'finished_at_ms',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE finished_at_ms END,
                          'recovery_guard_id',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE recovery_guard_id END,
                          'guard_id', CASE WHEN outcome = 'queued' THEN NULL ELSE guard_id END,
                          'guard_file_id',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE guard_file_id END,
                          'guard_library_id',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE guard_library_id END,
                          'guard_source_path',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE guard_source_path END,
                          'guard_recovery_path',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE guard_recovery_path END,
                          'guard_state',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE guard_state END,
                          'guard_created_at_ms',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE guard_created_at_ms END,
                          'guard_updated_at_ms',
                            CASE WHEN outcome = 'queued' THEN NULL ELSE guard_updated_at_ms END),
                        requested_queued_at_ms
                   FROM classified
                 RETURNING value AS envelope",
                params!(file_id, queued_at_ms, request_key),
            )
            .await?;
        #[cfg(feature = "hiqlite-contract-tests")]
        {
            let return_pause = {
                let mut pause = QUEUE_ADMISSION_RETURN_PAUSE
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                pause.take()
            };
            if let Some((reached, release)) = return_pause {
                let _ = reached.send(());
                let _ = release.await;
            }
        }
        serde_json::from_str::<QueueAdmissionEnvelope>(&row.envelope)
            .map_err(|error| {
                StoreError::Database(format!(
                    "decoding replicated Dolby Vision queue outcome: {error}"
                ))
            })?
            .finish()
    }

    async fn queue_library_dv_conversion_batch(
        &self,
        library_id: i64,
        queued_at_ms: i64,
        retry_failed: bool,
        limit: i64,
    ) -> Result<DvConversionQueueBatch, StoreError> {
        let limit = limit.clamp(0, DV_CONVERSION_QUEUE_BATCH_MAX);
        if limit == 0 {
            return Ok(DvConversionQueueBatch::default());
        }
        // Unseen files lead retries so a permanent low-ID failure prefix
        // cannot consume every bounded manual pass forever.
        let changed = self
            .client()
            .execute(
                "WITH requested(library_id, queued_at_ms, retry_failed, batch_limit) AS
                       (VALUES ($1, $2, $3, $4)),
                 candidates(file_id) AS (
                   SELECT f.id
                     FROM files f
                     JOIN items i ON i.id = f.item_id
                     LEFT JOIN dv_conversions d ON d.file_id = f.id
                     JOIN requested ON requested.library_id = i.library_id
                    WHERE LOWER(f.container) = 'mkv'
                      AND f.dv_profile = 7
                      AND f.dv_bl_compat_id IN (1, 6)
                      AND f.dv_el_present = 1
                      AND f.dv_rpu_present = 1
                      AND (d.file_id IS NULL OR
                           (requested.retry_failed AND d.state = 'failed'))
                    ORDER BY CASE WHEN d.file_id IS NULL THEN 0 ELSE 1 END, f.id
                    LIMIT $4
                 )
                 INSERT INTO dv_conversions (file_id, state, queued_at_ms)
                 SELECT candidates.file_id, 'queued', requested.queued_at_ms
                   FROM candidates JOIN requested WHERE true
                 ON CONFLICT(file_id) DO UPDATE SET
                   state = 'queued', el_type = NULL, original_path = NULL,
                   bytes_before = NULL, bytes_after = NULL, error = NULL,
                   queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL
                 WHERE $3 AND dv_conversions.state = 'failed'",
                params!(library_id, queued_at_ms, retry_failed, limit),
            )
            .await?;
        Ok(DvConversionQueueBatch {
            queued: changed as u64,
            saturated: changed as i64 == limit,
        })
    }

    async fn dv_conversion_candidates(
        &self,
        after_file_id: i64,
        limit: i64,
    ) -> Result<Vec<DvConversionCandidate>, StoreError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        self.client()
            .query_consistent_map::<CandidateRow, _>(
                "SELECT d.file_id, i.library_id, d.state
                   FROM dv_conversions d
                   JOIN files f ON f.id = d.file_id
                   JOIN items i ON i.id = f.item_id
                  WHERE d.file_id > $1
                    AND d.state IN ('verified', 'running', 'queued')
                  ORDER BY d.file_id LIMIT $2",
                params!(after_file_id, limit),
            )
            .await?
            .into_iter()
            .map(|row| {
                let state = DvConversionState::parse(&row.state).ok_or_else(|| {
                    StoreError::Database(format!(
                        "invalid Dolby Vision conversion state `{}`",
                        row.state
                    ))
                })?;
                Ok(DvConversionCandidate {
                    file_id: row.file_id,
                    library_id: row.library_id,
                    state,
                })
            })
            .collect()
    }

    async fn dv_committed_cleanup_candidate(
        &self,
        after_file_id: i64,
    ) -> Result<Option<i64>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<PresentRow, _>(
                "SELECT file_id AS present FROM dv_conversions
                  WHERE file_id > $1 AND state = 'committed'
                    AND recovery_guard_id IS NULL
                  ORDER BY file_id LIMIT 1",
                params!(after_file_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.present))
    }

    async fn dv_recovery_guard(&self, file_id: i64) -> Result<Option<DvRecoveryGuard>, StoreError> {
        one_guard(
            self.client()
                .query_consistent_map::<GuardRow, _>(
                    format!(
                        "SELECT {QUALIFIED_GUARD_COLS} FROM dv_recovery_guards g
                         JOIN dv_conversions d ON d.recovery_guard_id = g.guard_id
                         WHERE d.file_id = $1"
                    ),
                    params!(file_id),
                )
                .await?,
        )
    }

    async fn dv_recovery_guard_by_id(
        &self,
        guard_id: &str,
    ) -> Result<Option<DvRecoveryGuard>, StoreError> {
        one_guard(
            self.client()
                .query_consistent_map::<GuardRow, _>(
                    format!("SELECT {GUARD_COLS} FROM dv_recovery_guards WHERE guard_id = $1"),
                    params!(guard_id),
                )
                .await?,
        )
    }

    async fn dv_recovery_guard_orphans(
        &self,
        after_guard_id: &str,
        limit: i64,
    ) -> Result<Vec<DvRecoveryGuard>, StoreError> {
        let limit = limit.clamp(0, DV_RECOVERY_GUARD_READ_MAX);
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.client()
            .query_consistent_map::<GuardRow, _>(
                format!(
                    "SELECT {GUARD_COLS} FROM dv_recovery_guards g
                     WHERE g.guard_id > $1
                       AND NOT EXISTS (SELECT 1 FROM dv_conversions d
                                       WHERE d.recovery_guard_id = g.guard_id)
                     ORDER BY g.guard_id LIMIT $2"
                ),
                params!(after_guard_id, limit),
            )
            .await?
            .into_iter()
            .map(GuardRow::finish)
            .collect()
    }

    async fn dv_recovery_guard_summary(&self) -> Result<DvRecoveryGuardSummary, StoreError> {
        self.client()
            .query_consistent_map::<GuardSummaryRow, _>(
                "SELECT
                   COALESCE(SUM(CASE WHEN state = 'intent' THEN 1 ELSE 0 END), 0) AS intent,
                   COALESCE(SUM(CASE WHEN state = 'active' THEN 1 ELSE 0 END), 0) AS active,
                   COALESCE(SUM(CASE WHEN state = 'guard_removed' THEN 1 ELSE 0 END), 0)
                     AS guard_removed,
                   COALESCE(SUM(CASE WHEN state = 'scratch_removed' THEN 1 ELSE 0 END), 0)
                     AS scratch_removed,
                   COALESCE(SUM(CASE WHEN NOT EXISTS (
                     SELECT 1 FROM dv_conversions d
                      WHERE d.recovery_guard_id = g.guard_id)
                     THEN 1 ELSE 0 END), 0) AS orphaned
                 FROM dv_recovery_guards g",
                params!(),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0)
            .ok_or_else(|| StoreError::Database("missing recovery guard summary row".to_owned()))
    }

    async fn dv_recovery_guard_snapshot(
        &self,
        after_guard_id: &str,
        limit: i64,
    ) -> Result<DvRecoveryGuardSnapshot, StoreError> {
        let limit = limit.clamp(0, DV_RECOVERY_GUARD_READ_MAX);
        let rows = self
            .client()
            .query_consistent_map::<GuardSnapshotRow, _>(
                "WITH guard_rows AS (
                   SELECT g.*,
                          CASE WHEN NOT EXISTS (
                            SELECT 1 FROM dv_conversions d
                             WHERE d.recovery_guard_id = g.guard_id)
                          THEN 1 ELSE 0 END AS row_orphaned
                     FROM dv_recovery_guards g
                 ), totals AS (
                   SELECT
                     COALESCE(SUM(CASE WHEN state = 'intent' THEN 1 ELSE 0 END), 0)
                       AS intent,
                     COALESCE(SUM(CASE WHEN state = 'active' THEN 1 ELSE 0 END), 0)
                       AS active,
                     COALESCE(SUM(CASE WHEN state = 'guard_removed' THEN 1 ELSE 0 END), 0)
                       AS guard_removed,
                     COALESCE(SUM(CASE WHEN state = 'scratch_removed' THEN 1 ELSE 0 END), 0)
                       AS scratch_removed,
                     COALESCE(SUM(row_orphaned), 0) AS orphaned
                   FROM guard_rows
                 ), orphan_page AS (
                   SELECT * FROM guard_rows
                    WHERE row_orphaned = 1 AND guard_id > $1
                    ORDER BY guard_id LIMIT $2
                 )
                 SELECT totals.intent AS intent, totals.active AS active,
                        totals.guard_removed AS guard_removed,
                        totals.scratch_removed AS scratch_removed,
                        totals.orphaned AS orphaned,
                        orphan_page.guard_id AS page_guard_id,
                        orphan_page.file_id AS page_file_id,
                        orphan_page.library_id AS page_library_id,
                        orphan_page.source_path AS page_source_path,
                        orphan_page.recovery_path AS page_recovery_path,
                        orphan_page.state AS page_state,
                        orphan_page.created_at_ms AS page_created_at_ms,
                        orphan_page.updated_at_ms AS page_updated_at_ms
                   FROM totals LEFT JOIN orphan_page ON 1 = 1
                  ORDER BY orphan_page.guard_id",
                params!(after_guard_id, limit),
            )
            .await?;
        let summary = rows.first().map(|row| row.summary).ok_or_else(|| {
            StoreError::Database("missing recovery guard snapshot row".to_owned())
        })?;
        let mut orphans = Vec::new();
        for row in rows {
            debug_assert_eq!(row.summary, summary);
            if let Some(guard) = row.finish()? {
                orphans.push(guard);
            }
        }
        Ok(DvRecoveryGuardSnapshot { summary, orphans })
    }

    async fn dv_conversion_progress(
        &self,
        library_id: Option<i64>,
    ) -> Result<DvConversionProgress, StoreError> {
        let rows = self
            .client()
            .query_consistent_map::<ProgressRow, _>(
                "SELECT
                   COALESCE(SUM(CASE WHEN LOWER(f.container) = 'mkv'
                                          AND f.dv_profile = 7
                                          AND f.dv_bl_compat_id IN (1, 6)
                                          AND f.dv_el_present = 1
                                          AND f.dv_rpu_present = 1
                                     THEN 1 ELSE 0 END), 0) AS eligible,
                   COALESCE(SUM(CASE WHEN d.state = 'queued' THEN 1 ELSE 0 END), 0) AS queued,
                   COALESCE(SUM(CASE WHEN d.state = 'running' THEN 1 ELSE 0 END), 0) AS running,
                   COALESCE(SUM(CASE WHEN d.state = 'verified' THEN 1 ELSE 0 END), 0) AS verified,
                   COALESCE(SUM(CASE WHEN d.state = 'committed' THEN 1 ELSE 0 END), 0) AS committed,
                   COALESCE(SUM(CASE WHEN d.state = 'failed' THEN 1 ELSE 0 END), 0) AS failed
                 FROM files f
                 JOIN items i ON i.id = f.item_id
                 LEFT JOIN dv_conversions d ON d.file_id = f.id
                 WHERE $1 IS NULL OR i.library_id = $1",
                params!(library_id),
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| row.0)
            .ok_or_else(|| StoreError::Database("missing conversion progress row".to_owned()))
    }

    async fn dv_conversion_progress_snapshot(
        &self,
    ) -> Result<DvConversionProgressSnapshot, StoreError> {
        let rows = self
            .client()
            .query_consistent_map::<LibraryProgressRow, _>(
                "SELECT l.id AS library_id,
                   COALESCE(SUM(CASE WHEN LOWER(f.container) = 'mkv'
                                          AND f.dv_profile = 7
                                          AND f.dv_bl_compat_id IN (1, 6)
                                          AND f.dv_el_present = 1
                                          AND f.dv_rpu_present = 1
                                     THEN 1 ELSE 0 END), 0) AS eligible,
                   COALESCE(SUM(CASE WHEN d.state = 'queued' THEN 1 ELSE 0 END), 0) AS queued,
                   COALESCE(SUM(CASE WHEN d.state = 'running' THEN 1 ELSE 0 END), 0) AS running,
                   COALESCE(SUM(CASE WHEN d.state = 'verified' THEN 1 ELSE 0 END), 0) AS verified,
                   COALESCE(SUM(CASE WHEN d.state = 'committed' THEN 1 ELSE 0 END), 0) AS committed,
                   COALESCE(SUM(CASE WHEN d.state = 'failed' THEN 1 ELSE 0 END), 0) AS failed
                 FROM libraries l
                 LEFT JOIN items i ON i.library_id = l.id
                 LEFT JOIN files f ON f.item_id = i.id
                 LEFT JOIN dv_conversions d ON d.file_id = f.id
                 GROUP BY l.id ORDER BY l.id",
                params!(),
            )
            .await?;
        let by_library = rows
            .into_iter()
            .map(|row| (row.library_id, row.progress))
            .collect::<BTreeMap<_, _>>();
        Ok(DvConversionProgressSnapshot::from_libraries(by_library))
    }

    async fn set_library_dv_conversion_mode(
        &self,
        library_id: i64,
        mode: DvConversionMode,
    ) -> Result<bool, StoreError> {
        let library_key = library_id.to_string();
        let path = format!("$.\"{library_id}\"");
        let changed = self
            .client()
            .execute(
                "WITH requested(setting_key, library_key, mode, updated_at, library_id, path) AS
                       (VALUES ($1, $2, $3, $4, $5, $6))
                 INSERT INTO settings (key, value, updated_at)
                 SELECT requested.setting_key,
                        CASE WHEN requested.mode = 'off' THEN '{}'
                             ELSE json_object(requested.library_key, requested.mode) END,
                        requested.updated_at
                   FROM requested JOIN libraries ON libraries.id = requested.library_id
                 ON CONFLICT(key) DO UPDATE SET
                   value = CASE WHEN (SELECT mode FROM requested) = 'off'
                     THEN json_remove(
                       CASE WHEN json_valid(settings.value)
                             THEN CASE WHEN json_type(settings.value) = 'object'
                                       THEN settings.value ELSE '{}' END
                             ELSE '{}' END,
                       (SELECT path FROM requested))
                     ELSE json_set(
                       CASE WHEN json_valid(settings.value)
                             THEN CASE WHEN json_type(settings.value) = 'object'
                                       THEN settings.value ELSE '{}' END
                             ELSE '{}' END,
                       (SELECT path FROM requested), (SELECT mode FROM requested))
                   END,
                   updated_at = excluded.updated_at",
                params!(
                    keys::LIBRARY_DV_DISK_CONVERT,
                    library_key,
                    mode.as_str(),
                    self.now()?,
                    library_id,
                    path
                ),
            )
            .await?;
        Ok(changed == 1)
    }

    async fn begin_dv_recovery_guard(
        &self,
        file_id: i64,
        guard_id: &str,
        recovery_path: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_recovery_guard_identity(guard_id, recovery_path)?;
        let results = self
            .client()
            .txn([
                (
                    "WITH requested(file_id, guard_id, recovery_path, now_ms) AS
                           (VALUES ($1, $2, $3, $4))
                     INSERT INTO dv_recovery_guards
                       (guard_id, file_id, library_id, source_path, recovery_path,
                        state, created_at_ms, updated_at_ms)
                     SELECT requested.guard_id, d.file_id, i.library_id, f.path,
                            requested.recovery_path, 'intent', requested.now_ms,
                            requested.now_ms
                       FROM dv_conversions d
                       JOIN files f ON f.id = d.file_id
                       JOIN items i ON i.id = f.item_id
                       JOIN requested
                      WHERE d.file_id = requested.file_id AND d.state = 'verified'
                        AND (d.recovery_guard_id IS NULL
                             OR d.recovery_guard_id = requested.guard_id)
                     ON CONFLICT(guard_id) DO NOTHING",
                    params!(file_id, guard_id, recovery_path, now_ms),
                ),
                (
                    "WITH requested(file_id, guard_id, recovery_path) AS
                           (VALUES ($1, $2, $3))
                     UPDATE dv_conversions AS d
                        SET recovery_guard_id = (SELECT guard_id FROM requested)
                      WHERE d.file_id = (SELECT file_id FROM requested)
                        AND d.state = 'verified'
                        AND (d.recovery_guard_id IS NULL OR d.recovery_guard_id =
                             (SELECT guard_id FROM requested))
                        AND EXISTS (
                          SELECT 1 FROM dv_recovery_guards g
                          JOIN files f ON f.id = d.file_id
                          JOIN items i ON i.id = f.item_id
                          JOIN requested
                          WHERE g.guard_id = requested.guard_id AND g.file_id = d.file_id
                            AND g.library_id = i.library_id
                            AND g.source_path = f.path
                            AND g.recovery_path = requested.recovery_path
                            AND g.state = 'intent')",
                    params!(file_id, guard_id, recovery_path),
                ),
            ])
            .await?;
        let changed = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(changed.get(1).copied() == Some(1))
    }

    async fn mark_dv_conversion_running(
        &self,
        file_id: i64,
        bytes_before: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .client()
            .execute(
                "UPDATE dv_conversions
                    SET state = 'running', bytes_before = $1, bytes_after = NULL,
                        error = NULL, finished_at_ms = NULL
                  WHERE file_id = $2 AND state IN ('queued', 'running')",
                params!(bytes_before, file_id),
            )
            .await?
            == 1)
    }

    async fn mark_dv_conversion_verified(
        &self,
        file_id: i64,
        el_type: Option<&str>,
        bytes_after: i64,
    ) -> Result<bool, StoreError> {
        if !matches!(el_type, None | Some("mel" | "fel")) {
            return Err(StoreError::Task(
                "Dolby Vision enhancement-layer type must be mel, fel, or unknown".to_owned(),
            ));
        }
        Ok(self
            .client()
            .execute(
                "UPDATE dv_conversions
                    SET state = 'verified', el_type = $1, bytes_after = $2, error = NULL
                  WHERE file_id = $3 AND state = 'running'",
                params!(el_type, bytes_after, file_id),
            )
            .await?
            == 1)
    }

    async fn mark_dv_conversion_committed(
        &self,
        file_id: i64,
        original_path: Option<&str>,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(original_path) = original_path else {
            return Err(StoreError::Task(
                "a guardless committed Dolby Vision conversion requires a retained original path"
                    .to_owned(),
            ));
        };
        Ok(self
            .client()
            .execute(
                "UPDATE dv_conversions
                    SET state = 'committed', original_path = $1, bytes_after = $2,
                        error = NULL, finished_at_ms = $3
                  WHERE file_id = $4 AND state = 'verified'
                    AND recovery_guard_id IS NULL",
                params!(original_path, bytes_after, finished_at_ms, file_id),
            )
            .await?
            == 1)
    }

    async fn mark_dv_conversion_committed_with_guard(
        &self,
        file_id: i64,
        guard_id: &str,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let results = self
            .client()
            .txn([
                (
                    "WITH requested(file_id, guard_id, bytes_after, finished_at_ms) AS
                           (VALUES ($1, $2, $3, $4))
                     UPDATE dv_conversions
                        SET state = 'committed', original_path = NULL,
                            bytes_after = (SELECT bytes_after FROM requested),
                            error = NULL,
                            finished_at_ms = (SELECT finished_at_ms FROM requested)
                      WHERE file_id = (SELECT file_id FROM requested)
                        AND recovery_guard_id = (SELECT guard_id FROM requested)
                        AND ((state = 'verified' AND EXISTS (
                               SELECT 1 FROM dv_recovery_guards
                                WHERE guard_id = (SELECT guard_id FROM requested)
                                  AND state = 'intent'))
                          OR (state = 'committed' AND original_path IS NULL
                              AND bytes_after = (SELECT bytes_after FROM requested)
                              AND finished_at_ms = (SELECT finished_at_ms FROM requested)
                              AND EXISTS (SELECT 1 FROM dv_recovery_guards
                                           WHERE guard_id =
                                             (SELECT guard_id FROM requested)
                                             AND state = 'active'))) ",
                    params!(file_id, guard_id, bytes_after, finished_at_ms),
                ),
                (
                    "WITH requested(file_id, guard_id, finished_at_ms, bytes_after) AS
                           (VALUES ($1, $2, $3, $4))
                     UPDATE dv_recovery_guards
                        SET state = 'active',
                            updated_at_ms = (SELECT finished_at_ms FROM requested)
                      WHERE guard_id = (SELECT guard_id FROM requested)
                        AND file_id = (SELECT file_id FROM requested)
                        AND state IN ('intent', 'active')
                        AND EXISTS (SELECT 1 FROM dv_conversions
                                     WHERE file_id = (SELECT file_id FROM requested)
                                       AND recovery_guard_id = (SELECT guard_id FROM requested)
                                       AND state = 'committed' AND original_path IS NULL
                                       AND bytes_after = (SELECT bytes_after FROM requested)
                                       AND finished_at_ms =
                                         (SELECT finished_at_ms FROM requested))",
                    params!(file_id, guard_id, finished_at_ms, bytes_after),
                ),
            ])
            .await?;
        let changed = results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(matches!(changed.as_slice(), [1, 1]))
    }

    async fn advance_dv_recovery_guard(
        &self,
        guard_id: &str,
        expected: DvRecoveryGuardState,
        next: DvRecoveryGuardState,
        updated_at_ms: i64,
    ) -> Result<bool, StoreError> {
        if !expected.permits_cleanup_transition(next) {
            return Err(StoreError::Task(format!(
                "invalid Dolby Vision recovery guard transition {} -> {}",
                expected.as_str(),
                next.as_str()
            )));
        }
        Ok(self
            .client()
            .execute(
                "WITH requested(guard_id, expected, next, updated_at_ms) AS
                       (VALUES ($1, $2, $3, $4))
                 UPDATE dv_recovery_guards
                    SET state = (SELECT next FROM requested),
                        updated_at_ms = (SELECT updated_at_ms FROM requested)
                  WHERE guard_id = (SELECT guard_id FROM requested)
                    AND state IN ((SELECT expected FROM requested),
                                  (SELECT next FROM requested))
                    AND NOT EXISTS (SELECT 1 FROM dv_conversions d
                                    WHERE d.recovery_guard_id =
                                      (SELECT guard_id FROM requested))",
                params!(guard_id, expected.as_str(), next.as_str(), updated_at_ms),
            )
            .await?
            == 1)
    }

    async fn delete_dv_recovery_guard(&self, guard_id: &str) -> Result<bool, StoreError> {
        Ok(self
            .client()
            .execute(
                "DELETE FROM dv_recovery_guards
                  WHERE guard_id = $1 AND state = 'scratch_removed'
                    AND NOT EXISTS (SELECT 1 FROM dv_conversions d
                                    WHERE d.recovery_guard_id = $1)",
                params!(guard_id),
            )
            .await?
            == 1)
    }

    async fn mark_dv_conversion_failed(
        &self,
        file_id: i64,
        error: &str,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let error = error.chars().take(4096).collect::<String>();
        Ok(self
            .client()
            .execute(
                "UPDATE dv_conversions
                    SET state = 'failed', error = $1, finished_at_ms = $2,
                        recovery_guard_id = NULL
                  WHERE file_id = $3 AND state != 'committed'",
                params!(error, finished_at_ms, file_id),
            )
            .await?
            == 1)
    }
}
