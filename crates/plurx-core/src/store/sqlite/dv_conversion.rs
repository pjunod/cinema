//! SQLite permanent Dolby Vision conversion ledger.

use std::collections::BTreeMap;

use async_trait::async_trait;
use rusqlite::types::Type;
use rusqlite::{params, OptionalExtension, Row};

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::dv_conversion::{eligibility_reason, validate_recovery_guard_identity};
use crate::store::{
    keys, DvConversion, DvConversionCandidate, DvConversionMode, DvConversionProgress,
    DvConversionProgressSnapshot, DvConversionQueueBatch, DvConversionState, DvConversionStore,
    DvRecoveryGuard, DvRecoveryGuardSnapshot, DvRecoveryGuardState, DvRecoveryGuardSummary,
    QueueDvConversionOutcome, DV_CONVERSION_LEDGER_READ_MAX, DV_CONVERSION_MODE_DISABLED_REASON,
    DV_CONVERSION_QUEUE_BATCH_MAX, DV_RECOVERY_GUARD_READ_MAX,
};

const JOINED_CONVERSION_COLS: &str = "d.file_id, d.state, d.el_type, d.original_path,
    d.bytes_before, d.bytes_after, d.error, d.queued_at_ms, d.finished_at_ms,
    d.recovery_guard_id, g.guard_id, g.file_id, g.library_id, g.source_path,
    g.recovery_path, g.state, g.created_at_ms, g.updated_at_ms";

const GUARD_COLS: &str = "guard_id, file_id, library_id, source_path, recovery_path,
    state, created_at_ms, updated_at_ms";

const QUALIFIED_GUARD_COLS: &str = "g.guard_id, g.file_id, g.library_id, g.source_path,
    g.recovery_path, g.state, g.created_at_ms, g.updated_at_ms";

fn invalid_state(column: usize, state: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        format!("invalid Dolby Vision conversion state `{state}`").into(),
    )
}

fn invalid_guard_state(column: usize, state: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        format!("invalid Dolby Vision recovery guard state `{state}`").into(),
    )
}

fn guard_from_row_at(row: &Row<'_>, offset: usize) -> rusqlite::Result<DvRecoveryGuard> {
    let state: String = row.get(offset + 5)?;
    Ok(DvRecoveryGuard {
        guard_id: row.get(offset)?,
        file_id: row.get(offset + 1)?,
        library_id: row.get(offset + 2)?,
        source_path: row.get(offset + 3)?,
        recovery_path: row.get(offset + 4)?,
        state: DvRecoveryGuardState::parse(&state)
            .ok_or_else(|| invalid_guard_state(offset + 5, state))?,
        created_at_ms: row.get(offset + 6)?,
        updated_at_ms: row.get(offset + 7)?,
    })
}

fn guard_from_row(row: &Row<'_>) -> rusqlite::Result<DvRecoveryGuard> {
    guard_from_row_at(row, 0)
}

fn conversion_from_row(row: &Row<'_>) -> rusqlite::Result<DvConversion> {
    let state: String = row.get(1)?;
    Ok(DvConversion {
        file_id: row.get(0)?,
        state: DvConversionState::parse(&state).ok_or_else(|| invalid_state(1, state))?,
        el_type: row.get(2)?,
        original_path: row.get(3)?,
        bytes_before: row.get(4)?,
        bytes_after: row.get(5)?,
        error: row.get(6)?,
        queued_at_ms: row.get(7)?,
        finished_at_ms: row.get(8)?,
        recovery_guard: None,
    })
}

fn joined_conversion_from_row(row: &Row<'_>) -> rusqlite::Result<DvConversion> {
    let mut conversion = conversion_from_row(row)?;
    if let Some(guard_id) = row.get::<_, Option<String>>(9)? {
        let joined_guard_id = row.get::<_, Option<String>>(10)?.ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                Type::Null,
                format!("Dolby Vision conversion links missing recovery guard `{guard_id}`").into(),
            )
        })?;
        debug_assert_eq!(guard_id, joined_guard_id);
        conversion.recovery_guard = Some(guard_from_row_at(row, 10)?);
    }
    Ok(conversion)
}

fn read_conversion(
    conn: &rusqlite::Connection,
    file_id: i64,
) -> rusqlite::Result<Option<DvConversion>> {
    conn.query_row(
        &format!(
            "SELECT {JOINED_CONVERSION_COLS} FROM dv_conversions d
             LEFT JOIN dv_recovery_guards g ON g.guard_id = d.recovery_guard_id
             WHERE d.file_id = ?1"
        ),
        [file_id],
        joined_conversion_from_row,
    )
    .optional()
}

fn queue_refusal_outcome(
    conn: &rusqlite::Connection,
    file_id: i64,
) -> rusqlite::Result<QueueDvConversionOutcome> {
    let facts = conn
        .query_row(
            "SELECT f.container, f.dv_profile, f.dv_bl_compat_id,
                    f.dv_el_present, f.dv_rpu_present,
                    CASE WHEN json_valid(s.value) AND json_type(s.value) = 'object'
                         THEN json_extract(s.value, '$.\"' || i.library_id || '\"')
                         ELSE NULL END
               FROM files f
               JOIN items i ON i.id = f.item_id
          LEFT JOIN settings s ON s.key = ?2
              WHERE f.id = ?1",
            params![file_id, keys::LIBRARY_DV_DISK_CONVERT],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?.map(|value| value != 0),
                    row.get::<_, Option<i64>>(4)?.map(|value| value != 0),
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?;
    Ok(match facts {
        None => QueueDvConversionOutcome::FileMissing,
        Some((container, profile, compat, el, rpu, mode)) => {
            if let Some(reason) = eligibility_reason(container.as_deref(), profile, compat, el, rpu)
            {
                QueueDvConversionOutcome::Ineligible(reason)
            } else if !mode.is_some_and(|mode| mode == "manual" || mode == "auto") {
                QueueDvConversionOutcome::Ineligible(DV_CONVERSION_MODE_DISABLED_REASON)
            } else {
                QueueDvConversionOutcome::Ineligible("eligible file was not admitted")
            }
        }
    })
}

#[async_trait]
impl DvConversionStore for SqliteStore {
    async fn dv_conversion(&self, file_id: i64) -> Result<Option<DvConversion>, StoreError> {
        self.with_conn(move |conn| Ok(read_conversion(conn, file_id)?))
            .await
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
        let encoded = serde_json::to_string(file_ids)
            .map_err(|error| StoreError::Database(error.to_string()))?;
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {JOINED_CONVERSION_COLS} FROM dv_conversions d
                  LEFT JOIN dv_recovery_guards g ON g.guard_id = d.recovery_guard_id
                  WHERE d.file_id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))
                  ORDER BY d.file_id"
            ))?;
            let rows = statement.query_map([encoded], joined_conversion_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
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
        let encoded = serde_json::to_string(file_ids)
            .map_err(|error| StoreError::Database(error.to_string()))?;
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id,
                        CASE WHEN LOWER(container) = 'mkv'
                                   AND dv_profile = 7
                                   AND dv_bl_compat_id IN (1, 6)
                                   AND dv_el_present = 1 AND dv_rpu_present = 1
                             THEN 1 ELSE 0 END
                   FROM files
                  WHERE id IN (SELECT CAST(value AS INTEGER) FROM json_each(?1))
                  ORDER BY id",
            )?;
            let rows = statement.query_map([encoded], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? != 0))
            })?;
            Ok(rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
        })
        .await
    }

    async fn dv_conversion_eligible(&self, file_id: i64) -> Result<bool, StoreError> {
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM files
                      WHERE id = ?1 AND LOWER(container) = 'mkv' AND dv_profile = 7
                        AND dv_bl_compat_id IN (1, 6)
                        AND dv_el_present = 1 AND dv_rpu_present = 1",
                    [file_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await
    }

    async fn queue_dv_conversion(
        &self,
        file_id: i64,
        queued_at_ms: i64,
    ) -> Result<QueueDvConversionOutcome, StoreError> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let queued = tx
                .query_row(
                    "INSERT INTO dv_conversions
                   (file_id, state, queued_at_ms)
                 SELECT f.id, 'queued', ?2
                   FROM files f
                   JOIN items i ON i.id = f.item_id
                   JOIN settings s ON s.key = ?3
                  WHERE f.id = ?1 AND LOWER(f.container) = 'mkv' AND f.dv_profile = 7
                    AND f.dv_bl_compat_id IN (1, 6)
                    AND f.dv_el_present = 1 AND f.dv_rpu_present = 1
                    AND json_valid(s.value) AND json_type(s.value) = 'object'
                    AND json_extract(s.value, '$.\"' || i.library_id || '\"')
                        IN ('manual', 'auto')
                 ON CONFLICT(file_id) DO UPDATE SET
                   state = 'queued', el_type = NULL, original_path = NULL,
                   bytes_before = NULL, bytes_after = NULL, error = NULL,
                   queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL,
                   recovery_guard_id = NULL
                 WHERE dv_conversions.state = 'failed'
                 RETURNING file_id, state, el_type, original_path, bytes_before,
                           bytes_after, error, queued_at_ms, finished_at_ms",
                    params![file_id, queued_at_ms, keys::LIBRARY_DV_DISK_CONVERT],
                    conversion_from_row,
                )
                .optional()?;
            let outcome = if let Some(queued) = queued {
                QueueDvConversionOutcome::Queued(queued)
            } else if let Some(existing) = read_conversion(&tx, file_id)? {
                match existing.state {
                    DvConversionState::Committed => {
                        QueueDvConversionOutcome::AlreadyCommitted(existing)
                    }
                    DvConversionState::Queued
                    | DvConversionState::Running
                    | DvConversionState::Verified => {
                        QueueDvConversionOutcome::AlreadyActive(existing)
                    }
                    DvConversionState::Failed => queue_refusal_outcome(&tx, file_id)?,
                }
            } else {
                queue_refusal_outcome(&tx, file_id)?
            };
            tx.commit()?;
            Ok(outcome)
        })
        .await
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
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let enabled = tx
                .query_row(
                    "SELECT json_extract(value, '$.\"' || ?2 || '\"')
                       FROM settings
                      WHERE key = ?1 AND json_valid(value) AND json_type(value) = 'object'",
                    params![keys::LIBRARY_DV_DISK_CONVERT, library_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .is_some_and(|mode| mode == "manual" || mode == "auto");
            if !enabled {
                return Err(StoreError::Task(
                    DV_CONVERSION_MODE_DISABLED_REASON.to_owned(),
                ));
            }
            // Unseen files lead retries so a permanent low-ID failure prefix
            // cannot consume every bounded manual pass forever.
            let changed = tx.execute(
                "WITH candidates(file_id) AS (
                   SELECT f.id
                   FROM files f JOIN items i ON i.id = f.item_id
                   LEFT JOIN dv_conversions d ON d.file_id = f.id
                  WHERE i.library_id = ?1
                    AND LOWER(f.container) = 'mkv'
                    AND f.dv_profile = 7
                    AND f.dv_bl_compat_id IN (1, 6)
                    AND f.dv_el_present = 1
                    AND f.dv_rpu_present = 1
                    AND (d.file_id IS NULL OR (?3 AND d.state = 'failed'))
                   ORDER BY CASE WHEN d.file_id IS NULL THEN 0 ELSE 1 END, f.id
                   LIMIT ?4
                 )
                 INSERT INTO dv_conversions (file_id, state, queued_at_ms)
                 SELECT file_id, 'queued', ?2 FROM candidates WHERE true
                 ON CONFLICT(file_id) DO UPDATE SET
                   state = 'queued', el_type = NULL, original_path = NULL,
                   bytes_before = NULL, bytes_after = NULL, error = NULL,
                   queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL
                 WHERE ?3 AND dv_conversions.state = 'failed'",
                params![library_id, queued_at_ms, retry_failed, limit],
            )?;
            tx.commit()?;
            Ok(DvConversionQueueBatch {
                queued: changed as u64,
                saturated: changed as i64 == limit,
            })
        })
        .await
    }

    async fn dv_conversion_candidates(
        &self,
        after_file_id: i64,
        limit: i64,
    ) -> Result<Vec<DvConversionCandidate>, StoreError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT d.file_id, i.library_id, d.state
                   FROM dv_conversions d
                   JOIN files f ON f.id = d.file_id
                   JOIN items i ON i.id = f.item_id
                  WHERE d.file_id > ?1
                    AND d.state IN ('verified', 'running', 'queued')
                  ORDER BY d.file_id
                  LIMIT ?2",
            )?;
            let rows = statement.query_map(params![after_file_id, limit], |row| {
                let state: String = row.get(2)?;
                Ok(DvConversionCandidate {
                    file_id: row.get(0)?,
                    library_id: row.get(1)?,
                    state: DvConversionState::parse(&state)
                        .ok_or_else(|| invalid_state(2, state))?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    async fn dv_committed_cleanup_candidate(
        &self,
        after_file_id: i64,
    ) -> Result<Option<i64>, StoreError> {
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT file_id FROM dv_conversions
                      WHERE file_id > ?1 AND state = 'committed'
                        AND recovery_guard_id IS NULL
                      ORDER BY file_id LIMIT 1",
                    [after_file_id],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    async fn dv_recovery_guard(&self, file_id: i64) -> Result<Option<DvRecoveryGuard>, StoreError> {
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {QUALIFIED_GUARD_COLS} FROM dv_recovery_guards g
                         JOIN dv_conversions d ON d.recovery_guard_id = g.guard_id
                         WHERE d.file_id = ?1"
                    ),
                    [file_id],
                    guard_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn dv_recovery_guard_by_id(
        &self,
        guard_id: &str,
    ) -> Result<Option<DvRecoveryGuard>, StoreError> {
        let guard_id = guard_id.to_owned();
        self.with_read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {GUARD_COLS} FROM dv_recovery_guards WHERE guard_id = ?1"),
                    [guard_id],
                    guard_from_row,
                )
                .optional()?)
        })
        .await
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
        let after_guard_id = after_guard_id.to_owned();
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {GUARD_COLS} FROM dv_recovery_guards g
                 WHERE g.guard_id > ?1
                   AND NOT EXISTS (SELECT 1 FROM dv_conversions d
                                   WHERE d.recovery_guard_id = g.guard_id)
                 ORDER BY g.guard_id LIMIT ?2"
            ))?;
            let rows = statement.query_map(params![after_guard_id, limit], guard_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    async fn dv_recovery_guard_summary(&self) -> Result<DvRecoveryGuardSummary, StoreError> {
        self.with_read(move |conn| {
            Ok(conn.query_row(
                "SELECT
                   COALESCE(SUM(state = 'intent'), 0),
                   COALESCE(SUM(state = 'active'), 0),
                   COALESCE(SUM(state = 'guard_removed'), 0),
                   COALESCE(SUM(state = 'scratch_removed'), 0),
                   COALESCE(SUM(NOT EXISTS (
                     SELECT 1 FROM dv_conversions d
                      WHERE d.recovery_guard_id = g.guard_id)), 0)
                 FROM dv_recovery_guards g",
                [],
                |row| {
                    Ok(DvRecoveryGuardSummary {
                        intent: row.get(0)?,
                        active: row.get(1)?,
                        guard_removed: row.get(2)?,
                        scratch_removed: row.get(3)?,
                        orphaned: row.get(4)?,
                    })
                },
            )?)
        })
        .await
    }

    async fn dv_recovery_guard_snapshot(
        &self,
        after_guard_id: &str,
        limit: i64,
    ) -> Result<DvRecoveryGuardSnapshot, StoreError> {
        let limit = limit.clamp(0, DV_RECOVERY_GUARD_READ_MAX);
        let after_guard_id = after_guard_id.to_owned();
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "WITH guard_rows AS (
                   SELECT g.*,
                          CASE WHEN NOT EXISTS (
                            SELECT 1 FROM dv_conversions d
                             WHERE d.recovery_guard_id = g.guard_id)
                          THEN 1 ELSE 0 END AS orphaned
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
                     COALESCE(SUM(orphaned), 0) AS orphaned
                   FROM guard_rows
                 ), orphan_page AS (
                   SELECT * FROM guard_rows
                    WHERE orphaned = 1 AND guard_id > ?1
                    ORDER BY guard_id LIMIT ?2
                 )
                 SELECT totals.intent, totals.active, totals.guard_removed,
                        totals.scratch_removed, totals.orphaned,
                        orphan_page.guard_id, orphan_page.file_id,
                        orphan_page.library_id, orphan_page.source_path,
                        orphan_page.recovery_path, orphan_page.state,
                        orphan_page.created_at_ms, orphan_page.updated_at_ms
                   FROM totals LEFT JOIN orphan_page ON 1 = 1
                  ORDER BY orphan_page.guard_id",
            )?;
            let mut rows = statement.query(params![after_guard_id, limit])?;
            let mut summary = None;
            let mut orphans = Vec::new();
            while let Some(row) = rows.next()? {
                let current = DvRecoveryGuardSummary {
                    intent: row.get(0)?,
                    active: row.get(1)?,
                    guard_removed: row.get(2)?,
                    scratch_removed: row.get(3)?,
                    orphaned: row.get(4)?,
                };
                if let Some(existing) = summary {
                    debug_assert_eq!(existing, current);
                } else {
                    summary = Some(current);
                }
                if row.get::<_, Option<String>>(5)?.is_some() {
                    orphans.push(guard_from_row_at(row, 5)?);
                }
            }
            Ok(DvRecoveryGuardSnapshot {
                summary: summary.ok_or_else(|| {
                    StoreError::Database("missing recovery guard snapshot row".to_owned())
                })?,
                orphans,
            })
        })
        .await
    }

    async fn dv_conversion_progress(
        &self,
        library_id: Option<i64>,
    ) -> Result<DvConversionProgress, StoreError> {
        self.with_read(move |conn| {
            Ok(conn.query_row(
                "SELECT
                   COALESCE(SUM(CASE WHEN LOWER(f.container) = 'mkv'
                                          AND f.dv_profile = 7
                                          AND f.dv_bl_compat_id IN (1, 6)
                                          AND f.dv_el_present = 1
                                          AND f.dv_rpu_present = 1
                                     THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'queued' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'running' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'verified' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'committed' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'failed' THEN 1 ELSE 0 END), 0)
                 FROM files f
                 JOIN items i ON i.id = f.item_id
                 LEFT JOIN dv_conversions d ON d.file_id = f.id
                 WHERE ?1 IS NULL OR i.library_id = ?1",
                [library_id],
                |row| {
                    Ok(DvConversionProgress {
                        eligible: row.get(0)?,
                        queued: row.get(1)?,
                        running: row.get(2)?,
                        verified: row.get(3)?,
                        committed: row.get(4)?,
                        failed: row.get(5)?,
                    })
                },
            )?)
        })
        .await
    }

    async fn dv_conversion_progress_snapshot(
        &self,
    ) -> Result<DvConversionProgressSnapshot, StoreError> {
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT l.id,
                   COALESCE(SUM(CASE WHEN LOWER(f.container) = 'mkv'
                                          AND f.dv_profile = 7
                                          AND f.dv_bl_compat_id IN (1, 6)
                                          AND f.dv_el_present = 1
                                          AND f.dv_rpu_present = 1
                                     THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'queued' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'running' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'verified' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'committed' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN d.state = 'failed' THEN 1 ELSE 0 END), 0)
                 FROM libraries l
                 LEFT JOIN items i ON i.library_id = l.id
                 LEFT JOIN files f ON f.item_id = i.id
                 LEFT JOIN dv_conversions d ON d.file_id = f.id
                 GROUP BY l.id ORDER BY l.id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    DvConversionProgress {
                        eligible: row.get(1)?,
                        queued: row.get(2)?,
                        running: row.get(3)?,
                        verified: row.get(4)?,
                        committed: row.get(5)?,
                        failed: row.get(6)?,
                    },
                ))
            })?;
            let by_library = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
            Ok(DvConversionProgressSnapshot::from_libraries(by_library))
        })
        .await
    }

    async fn set_library_dv_conversion_mode(
        &self,
        library_id: i64,
        mode: DvConversionMode,
    ) -> Result<bool, StoreError> {
        let library_key = library_id.to_string();
        let path = format!("$.\"{library_id}\"");
        let mode = mode.as_str().to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "INSERT INTO settings (key, value, updated_at)
                 SELECT ?1,
                        CASE WHEN ?3 = 'off' THEN '{}'
                             ELSE json_object(?2, ?3) END,
                        unixepoch()
                   FROM libraries WHERE id = ?4
                 ON CONFLICT(key) DO UPDATE SET
                   value = CASE WHEN ?3 = 'off'
                     THEN json_remove(
                       CASE WHEN json_valid(settings.value)
                             THEN CASE WHEN json_type(settings.value) = 'object'
                                       THEN settings.value ELSE '{}' END
                             ELSE '{}' END,
                       ?5)
                     ELSE json_set(
                       CASE WHEN json_valid(settings.value)
                             THEN CASE WHEN json_type(settings.value) = 'object'
                                       THEN settings.value ELSE '{}' END
                             ELSE '{}' END,
                       ?5, ?3)
                   END,
                   updated_at = excluded.updated_at",
                params![
                    keys::LIBRARY_DV_DISK_CONVERT,
                    library_key,
                    mode,
                    library_id,
                    path
                ],
            )? == 1)
        })
        .await
    }

    async fn begin_dv_recovery_guard(
        &self,
        file_id: i64,
        guard_id: &str,
        recovery_path: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_recovery_guard_identity(guard_id, recovery_path)?;
        let guard_id = guard_id.to_owned();
        let recovery_path = recovery_path.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO dv_recovery_guards
                   (guard_id, file_id, library_id, source_path, recovery_path,
                    state, created_at_ms, updated_at_ms)
                 SELECT ?2, d.file_id, i.library_id, f.path, ?3,
                        'intent', ?4, ?4
                   FROM dv_conversions d
                   JOIN files f ON f.id = d.file_id
                   JOIN items i ON i.id = f.item_id
                  WHERE d.file_id = ?1 AND d.state = 'verified'
                    AND (d.recovery_guard_id IS NULL OR d.recovery_guard_id = ?2)
                 ON CONFLICT(guard_id) DO NOTHING",
                params![file_id, guard_id, recovery_path, now_ms],
            )?;
            let linked = tx.execute(
                "UPDATE dv_conversions AS d SET recovery_guard_id = ?2
                  WHERE d.file_id = ?1 AND d.state = 'verified'
                    AND (d.recovery_guard_id IS NULL OR d.recovery_guard_id = ?2)
                    AND EXISTS (
                      SELECT 1 FROM dv_recovery_guards g
                      JOIN files f ON f.id = d.file_id
                      JOIN items i ON i.id = f.item_id
                      WHERE g.guard_id = ?2 AND g.file_id = d.file_id
                        AND g.library_id = i.library_id
                        AND g.source_path = f.path AND g.recovery_path = ?3
                        AND g.state = 'intent')",
                params![file_id, guard_id, recovery_path],
            )?;
            if linked != 1 {
                return Ok(false);
            }
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    async fn mark_dv_conversion_running(
        &self,
        file_id: i64,
        bytes_before: i64,
    ) -> Result<bool, StoreError> {
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_conversions
                    SET state = 'running', bytes_before = ?2, bytes_after = NULL,
                        error = NULL, finished_at_ms = NULL
                  WHERE file_id = ?1 AND state IN ('queued', 'running')",
                params![file_id, bytes_before],
            )? == 1)
        })
        .await
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
        let el_type = el_type.map(str::to_owned);
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_conversions
                    SET state = 'verified', el_type = ?2, bytes_after = ?3, error = NULL
                  WHERE file_id = ?1 AND state = 'running'",
                params![file_id, el_type, bytes_after],
            )? == 1)
        })
        .await
    }

    async fn mark_dv_conversion_committed(
        &self,
        file_id: i64,
        original_path: Option<&str>,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let Some(original_path) = original_path.map(str::to_owned) else {
            return Err(StoreError::Task(
                "a guardless committed Dolby Vision conversion requires a retained original path"
                    .to_owned(),
            ));
        };
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_conversions
                    SET state = 'committed', original_path = ?2, bytes_after = ?3,
                        error = NULL, finished_at_ms = ?4
                  WHERE file_id = ?1 AND state = 'verified'
                    AND recovery_guard_id IS NULL",
                params![file_id, original_path, bytes_after, finished_at_ms],
            )? == 1)
        })
        .await
    }

    async fn mark_dv_conversion_committed_with_guard(
        &self,
        file_id: i64,
        guard_id: &str,
        bytes_after: i64,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let guard_id = guard_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let already_committed = tx
                .query_row(
                    "SELECT 1 FROM dv_conversions d
                     JOIN dv_recovery_guards g ON g.guard_id = d.recovery_guard_id
                     JOIN files f ON f.id = d.file_id
                     JOIN items i ON i.id = f.item_id
                     WHERE d.file_id = ?1 AND d.state = 'committed'
                       AND d.original_path IS NULL AND d.recovery_guard_id = ?2
                       AND d.bytes_after = ?3 AND d.finished_at_ms = ?4
                       AND g.file_id = d.file_id
                       AND g.library_id = i.library_id
                       AND g.source_path = f.path
                       AND g.state = 'active'",
                    params![file_id, guard_id, bytes_after, finished_at_ms],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if already_committed {
                tx.commit()?;
                return Ok(true);
            }
            let committed = tx.execute(
                "UPDATE dv_conversions
                    SET state = 'committed', original_path = NULL, bytes_after = ?3,
                        error = NULL, finished_at_ms = ?4
                  WHERE file_id = ?1 AND state = 'verified' AND recovery_guard_id = ?2
                    AND EXISTS (
                      SELECT 1
                        FROM dv_recovery_guards g
                        JOIN files f ON f.id = ?1
                        JOIN items i ON i.id = f.item_id
                       WHERE g.guard_id = ?2 AND g.file_id = ?1
                         AND g.library_id = i.library_id
                         AND g.source_path = f.path
                         AND g.state = 'intent')",
                params![file_id, guard_id, bytes_after, finished_at_ms],
            )?;
            if committed != 1 {
                return Ok(false);
            }
            let activated = tx.execute(
                "UPDATE dv_recovery_guards AS g
                    SET state = 'active', updated_at_ms = ?4
                  WHERE g.guard_id = ?2 AND g.file_id = ?1 AND g.state = 'intent'
                    AND EXISTS (
                      SELECT 1
                        FROM dv_conversions d
                        JOIN files f ON f.id = d.file_id
                        JOIN items i ON i.id = f.item_id
                       WHERE d.file_id = ?1 AND d.recovery_guard_id = ?2
                         AND d.state = 'committed' AND d.original_path IS NULL
                         AND d.bytes_after = ?3 AND d.finished_at_ms = ?4
                         AND g.library_id = i.library_id
                         AND g.source_path = f.path)",
                params![file_id, guard_id, bytes_after, finished_at_ms],
            )?;
            if activated != 1 {
                return Err(StoreError::Database(
                    "Dolby Vision recovery guard activation lost its intent row".to_owned(),
                ));
            }
            tx.commit()?;
            Ok(true)
        })
        .await
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
        let guard_id = guard_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_recovery_guards SET state = ?3, updated_at_ms = ?4
                  WHERE guard_id = ?1 AND state IN (?2, ?3)
                    AND NOT EXISTS (SELECT 1 FROM dv_conversions d
                                    WHERE d.recovery_guard_id = ?1)",
                params![guard_id, expected.as_str(), next.as_str(), updated_at_ms],
            )? == 1)
        })
        .await
    }

    async fn delete_dv_recovery_guard(&self, guard_id: &str) -> Result<bool, StoreError> {
        let guard_id = guard_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM dv_recovery_guards
                  WHERE guard_id = ?1 AND state = 'scratch_removed'
                    AND NOT EXISTS (SELECT 1 FROM dv_conversions d
                                    WHERE d.recovery_guard_id = ?1)",
                [guard_id],
            )? == 1)
        })
        .await
    }

    async fn mark_dv_conversion_failed(
        &self,
        file_id: i64,
        error: &str,
        finished_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let error = error.chars().take(4096).collect::<String>();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE dv_conversions
                    SET state = 'failed', error = ?2, finished_at_ms = ?3,
                        recovery_guard_id = NULL
                  WHERE file_id = ?1 AND state != 'committed'",
                params![file_id, error, finished_at_ms],
            )? == 1)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::domain::{
        DolbyVisionFacts, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult,
    };
    use crate::store::{LibraryStore, MediaStore};

    async fn p7_file(store: &SqliteStore, name: &str, compat_id: i64) -> (i64, i64) {
        let library = store
            .create_library(&NewLibrary {
                name: name.to_owned(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from(format!("/{name}"))],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: name.to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file = store
            .upsert_file(
                item,
                &format!("/{name}/{name}.mkv"),
                80_000,
                7,
                &ProbeResult {
                    container: Some("mkv".to_owned()),
                    dolby_vision: DolbyVisionFacts {
                        profile: Some(7),
                        level: Some(6),
                        bl_compat_id: Some(compat_id),
                        el_present: Some(true),
                        rpu_present: Some(true),
                    },
                    raw_json: Some("{}".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        store
            .set_library_dv_conversion_mode(library.id, DvConversionMode::Manual)
            .await
            .expect("enable manual conversion");
        (library.id, file)
    }

    #[tokio::test]
    async fn ledger_enforces_eligibility_transitions_and_committed_terminal_state() {
        let store = SqliteStore::open_in_memory().expect("store");
        let (library_id, file_id) = p7_file(&store, "HDR10", 6).await;
        let (_, hlg_file_id) = p7_file(&store, "HLG", 4).await;

        assert!(matches!(
            store
                .queue_dv_conversion(hlg_file_id, 10)
                .await
                .expect("reject HLG"),
            QueueDvConversionOutcome::Ineligible(_)
        ));
        assert!(matches!(
            store.queue_dv_conversion(file_id, 11).await.expect("queue"),
            QueueDvConversionOutcome::Queued(_)
        ));
        assert!(store
            .mark_dv_conversion_running(file_id, 80_000)
            .await
            .expect("running"));
        assert!(store
            .mark_dv_conversion_verified(file_id, Some("fel"), 60_000)
            .await
            .expect("verified"));
        assert!(store
            .mark_dv_conversion_committed(file_id, Some("/HDR10/HDR10.mkv.p7.orig"), 60_000, 12,)
            .await
            .expect("committed"));

        let row = store
            .dv_conversion(file_id)
            .await
            .expect("row")
            .expect("conversion");
        assert_eq!(row.state, DvConversionState::Committed);
        assert_eq!(row.el_type.as_deref(), Some("fel"));
        assert_eq!(row.bytes_before, Some(80_000));
        assert_eq!(row.bytes_after, Some(60_000));
        assert!(matches!(
            store
                .queue_dv_conversion(file_id, 13)
                .await
                .expect("terminal queue"),
            QueueDvConversionOutcome::AlreadyCommitted(_)
        ));
        assert!(!store
            .mark_dv_conversion_failed(file_id, "late stale worker", 14)
            .await
            .expect("terminal failure refused"));

        let progress = store
            .dv_conversion_progress(Some(library_id))
            .await
            .expect("progress");
        assert_eq!(progress.eligible, 1);
        assert_eq!(progress.committed, 1);
    }

    #[tokio::test]
    async fn guarded_commit_refuses_a_mismatched_raw_guard_without_partial_commit() {
        let store = SqliteStore::open_in_memory().expect("store");
        let (_, file_id) = p7_file(&store, "GuardMismatch", 6).await;
        assert!(matches!(
            store.queue_dv_conversion(file_id, 1).await.expect("queue"),
            QueueDvConversionOutcome::Queued(_)
        ));
        assert!(store
            .mark_dv_conversion_running(file_id, 80_000)
            .await
            .expect("running"));
        assert!(store
            .mark_dv_conversion_verified(file_id, Some("fel"), 60_000)
            .await
            .expect("verified"));
        assert!(store
            .begin_dv_recovery_guard(
                file_id,
                "mismatched-guard",
                "/GuardMismatch/.plurx-recovery.mkv",
                2,
            )
            .await
            .expect("begin guard"));
        store
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE dv_recovery_guards
                        SET source_path = '/wrong/source.mkv'
                      WHERE guard_id = 'mismatched-guard'",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("raw-seed mismatched guard");

        assert!(!store
            .mark_dv_conversion_committed_with_guard(file_id, "mismatched-guard", 60_000, 3)
            .await
            .expect("refuse mismatched guard"));
        let conversion = store
            .dv_conversion(file_id)
            .await
            .expect("read conversion")
            .expect("conversion");
        assert_eq!(conversion.state, DvConversionState::Verified);
        assert_eq!(
            conversion.recovery_guard.expect("linked guard").state,
            DvRecoveryGuardState::Intent
        );
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn sqlite_v42_fixture_migrates_conversion_and_guard_ledgers_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v42.db");
        let connection = rusqlite::Connection::open(&path).expect("fixture");
        SqliteStore::apply_migrations_for_test(
            &connection,
            crate::store::SQLITE_SCHEMA_VERSION - 2,
        )
        .expect("v42 schema");
        drop(connection);

        let _store = SqliteStore::open(&path).expect("migrate v42");
        let connection = rusqlite::Connection::open(&path).expect("inspect");
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'table' AND name = 'dv_conversions'",
                [],
                |row| row.get(0),
            )
            .expect("table count");
        assert_eq!(tables, 1);
        let guards: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'table' AND name = 'dv_recovery_guards'",
                [],
                |row| row.get(0),
            )
            .expect("guard table count");
        assert_eq!(guards, 1);
        let columns: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('dv_conversions')",
                [],
                |row| row.get(0),
            )
            .expect("conversion columns");
        assert_eq!(columns, 10);
    }

    #[cfg(feature = "hiqlite-store")]
    #[test]
    fn sqlite_v43_guard_migration_rejects_a_malformed_preexisting_ledger() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v43-malformed-guard.db");
        let connection = rusqlite::Connection::open(&path).expect("fixture");
        // v43 exactly: the version before the guard ledger this test is about.
        // Deriving it from the current head made it drift the moment another
        // migration landed on top.
        SqliteStore::apply_migrations_for_test(&connection, 43).expect("v43 schema");
        connection
            .execute_batch(
                "CREATE TABLE dv_recovery_guards (
                    guard_id TEXT PRIMARY KEY,
                    state TEXT NOT NULL
                 ) STRICT;",
            )
            .expect("malformed guard ledger");
        drop(connection);

        let error = match SqliteStore::open(&path) {
            Ok(_) => panic!("malformed v44 guard ledger must refuse"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("already exists"), "{error}");
        let connection = rusqlite::Connection::open(&path).expect("inspect refused fixture");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        // The refusal leaves the database exactly where it was: still v43.
        assert_eq!(version, 43);
        let columns: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('dv_conversions')",
                [],
                |row| row.get(0),
            )
            .expect("rolled-back conversion columns");
        assert_eq!(columns, 9);
        connection
            .execute("DROP TABLE dv_recovery_guards", [])
            .expect("remove malformed guard ledger");
        drop(connection);

        let _store = SqliteStore::open(&path).expect("retry exact v44 migration");
    }

    #[cfg(feature = "hiqlite-store")]
    #[tokio::test]
    async fn sqlite_v43_guard_migration_refuses_an_unrecoverable_committed_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v43-unrecoverable-commit.db");
        let store = SqliteStore::open(&path).expect("current store");
        let (_, file_id) = p7_file(&store, "Unrecoverable", 6).await;
        assert!(matches!(
            store.queue_dv_conversion(file_id, 1).await.expect("queue"),
            QueueDvConversionOutcome::Queued(_)
        ));
        assert!(store
            .mark_dv_conversion_running(file_id, 80_000)
            .await
            .expect("running"));
        assert!(store
            .mark_dv_conversion_verified(file_id, Some("fel"), 60_000)
            .await
            .expect("verified"));
        assert!(store
            .mark_dv_conversion_committed(
                file_id,
                Some("/Unrecoverable/Unrecoverable.mkv.p7.orig"),
                60_000,
                2,
            )
            .await
            .expect("recoverable current commit"));
        drop(store);

        let connection = rusqlite::Connection::open(&path).expect("downgrade fixture");
        connection
            .execute_batch(
                // A v43 database predates v44's recovery guards *and* v45's
                // attempt history; leaving either behind makes the replayed
                // migration fail on a column that is already there.
                "DROP INDEX dv_conversions_recovery_guard;
                 DROP TABLE dv_recovery_guards;
                 ALTER TABLE dv_conversions DROP COLUMN recovery_guard_id;
                 ALTER TABLE cluster_fragment_index_jobs DROP COLUMN attempt_errors;
                 PRAGMA user_version = 43;",
            )
            .expect("construct unrecoverable v43 commit");
        connection
            .execute(
                "UPDATE dv_conversions SET original_path = NULL WHERE file_id = ?1",
                [file_id],
            )
            .expect("remove the only recovery path from the v43 claim");
        drop(connection);

        let error = match SqliteStore::open(&path) {
            Ok(_) => panic!("v43 unrecoverable committed claim must refuse"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("CHECK constraint failed"),
            "{error}"
        );
        let connection = rusqlite::Connection::open(&path).expect("inspect refused fixture");
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("schema version"),
            43
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('dv_conversions')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("conversion columns"),
            9,
            "the refused migration must roll its added column back"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                      WHERE type = 'table' AND name = 'dv_recovery_guards'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("guard table count"),
            0,
            "the refused migration must not leave a partial guard schema"
        );
        connection
            .execute(
                "UPDATE dv_conversions SET original_path = ?1 WHERE file_id = ?2",
                params!["/Unrecoverable/Unrecoverable.mkv.p7.orig", file_id],
            )
            .expect("repair source recovery claim");
        drop(connection);

        let _store = SqliteStore::open(&path).expect("retry repaired v44 migration");
    }
}
