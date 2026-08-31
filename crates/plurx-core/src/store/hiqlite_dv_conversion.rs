//! Replicated permanent Dolby Vision conversion ledger.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::dv_conversion::{eligibility_reason, DV_CONVERSIONS_QUEUE_INDEX, DV_CONVERSIONS_SCHEMA};
use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use super::{
    keys, DvConversion, DvConversionCandidate, DvConversionMode, DvConversionProgress,
    DvConversionProgressSnapshot, DvConversionQueueBatch, DvConversionState, DvConversionStore,
    QueueDvConversionOutcome, DV_CONVERSION_LEDGER_READ_MAX, DV_CONVERSION_QUEUE_BATCH_MAX,
};
use crate::error::StoreError;

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    for sql in [DV_CONVERSIONS_SCHEMA, DV_CONVERSIONS_QUEUE_INDEX] {
        validate_sql(sql)?;
        for result in timeout_store(client.batch(sql)).await? {
            result.map_err(database_error)?;
        }
    }
    Ok(())
}

const CONVERSION_COLS: &str = "file_id, state, el_type, original_path, bytes_before,
    bytes_after, error, queued_at_ms, finished_at_ms";

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
        })
    }
}

struct FactsRow {
    container: Option<String>,
    profile: Option<i64>,
    bl_compat_id: Option<i64>,
    el_present: Option<i64>,
    rpu_present: Option<i64>,
}

impl From<&mut Row<'_>> for FactsRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            container: row.get("container"),
            profile: row.get("dv_profile"),
            bl_compat_id: row.get("dv_bl_compat_id"),
            el_present: row.get("dv_el_present"),
            rpu_present: row.get("dv_rpu_present"),
        }
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

fn one_conversion(mut rows: Vec<ConversionRow>) -> Result<Option<DvConversion>, StoreError> {
    if rows.len() > 1 {
        return Err(StoreError::Database(
            "duplicate Dolby Vision conversion ledger row".to_owned(),
        ));
    }
    rows.pop().map(ConversionRow::finish).transpose()
}

#[async_trait]
impl DvConversionStore for HiqliteAuthStore {
    async fn dv_conversion(&self, file_id: i64) -> Result<Option<DvConversion>, StoreError> {
        let sql = format!("SELECT {CONVERSION_COLS} FROM dv_conversions WHERE file_id = $1");
        one_conversion(
            self.client()
                .query_consistent_map::<ConversionRow, _>(sql, params!(file_id))
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
            .query_consistent_map::<ConversionRow, _>(
                format!(
                    "SELECT {CONVERSION_COLS} FROM dv_conversions
                      WHERE file_id IN (
                        SELECT CAST(value AS INTEGER) FROM json_each($1))
                      ORDER BY file_id"
                ),
                params!(encoded),
            )
            .await?
            .into_iter()
            .map(ConversionRow::finish)
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
        let admitted = self
            .client()
            .execute_returning_map::<_, ConversionRow>(
                "WITH requested(file_id, queued_at_ms) AS (VALUES ($1, $2))
                 INSERT INTO dv_conversions (file_id, state, queued_at_ms)
                 SELECT files.id, 'queued', requested.queued_at_ms
                   FROM files JOIN requested ON requested.file_id = files.id
                  WHERE LOWER(files.container) = 'mkv' AND files.dv_profile = 7
                    AND files.dv_bl_compat_id IN (1, 6)
                    AND files.dv_el_present = 1 AND files.dv_rpu_present = 1
                 ON CONFLICT(file_id) DO UPDATE SET
                   state = 'queued', el_type = NULL, original_path = NULL,
                   bytes_before = NULL, bytes_after = NULL, error = NULL,
                   queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL
                 WHERE dv_conversions.state = 'failed'
                 RETURNING file_id, state, el_type, original_path, bytes_before,
                           bytes_after, error, queued_at_ms, finished_at_ms",
                params!(file_id, queued_at_ms),
            )
            .await?;
        let admitted = admitted
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        if admitted.len() > 1 {
            return Err(StoreError::Database(
                "per-file Dolby Vision admission returned duplicate rows".to_owned(),
            ));
        }
        if let Some(queued) = admitted.into_iter().next() {
            return Ok(QueueDvConversionOutcome::Queued(queued.finish()?));
        }
        if let Some(existing) = self.dv_conversion(file_id).await? {
            return Ok(match existing.state {
                DvConversionState::Committed => {
                    QueueDvConversionOutcome::AlreadyCommitted(existing)
                }
                _ => QueueDvConversionOutcome::AlreadyActive(existing),
            });
        }
        let mut facts = self
            .client()
            .query_consistent_map::<FactsRow, _>(
                "SELECT container, dv_profile, dv_bl_compat_id, dv_el_present, dv_rpu_present
                   FROM files WHERE id = $1",
                params!(file_id),
            )
            .await?;
        let Some(facts) = facts.pop() else {
            return Ok(QueueDvConversionOutcome::FileMissing);
        };
        Ok(QueueDvConversionOutcome::Ineligible(
            eligibility_reason(
                facts.container.as_deref(),
                facts.profile,
                facts.bl_compat_id,
                facts.el_present.map(|value| value != 0),
                facts.rpu_present.map(|value| value != 0),
            )
            .unwrap_or("eligible file was not admitted"),
        ))
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
                  ORDER BY file_id LIMIT 1",
                params!(after_file_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.present))
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
        Ok(self
            .client()
            .execute(
                "UPDATE dv_conversions
                    SET state = 'committed', original_path = $1, bytes_after = $2,
                        error = NULL, finished_at_ms = $3
                  WHERE file_id = $4 AND state = 'verified'",
                params!(original_path, bytes_after, finished_at_ms, file_id),
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
                    SET state = 'failed', error = $1, finished_at_ms = $2
                  WHERE file_id = $3 AND state != 'committed'",
                params!(error, finished_at_ms, file_id),
            )
            .await?
            == 1)
    }
}
