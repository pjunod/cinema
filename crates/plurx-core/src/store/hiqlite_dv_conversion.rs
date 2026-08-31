//! Replicated permanent Dolby Vision conversion ledger.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::dv_conversion::{eligibility_reason, DV_CONVERSIONS_QUEUE_INDEX, DV_CONVERSIONS_SCHEMA};
use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use super::{
    DvConversion, DvConversionCandidate, DvConversionProgress, DvConversionState,
    DvConversionStore, QueueDvConversionOutcome,
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
    profile: Option<i64>,
    bl_compat_id: Option<i64>,
    el_present: Option<i64>,
    rpu_present: Option<i64>,
}

impl From<&mut Row<'_>> for FactsRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
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

    async fn queue_dv_conversion(
        &self,
        file_id: i64,
        queued_at_ms: i64,
    ) -> Result<QueueDvConversionOutcome, StoreError> {
        let mut facts = self
            .client()
            .query_consistent_map::<FactsRow, _>(
                "SELECT dv_profile, dv_bl_compat_id, dv_el_present, dv_rpu_present
                   FROM files WHERE id = $1",
                params!(file_id),
            )
            .await?;
        let Some(facts) = facts.pop() else {
            return Ok(QueueDvConversionOutcome::FileMissing);
        };
        if let Some(reason) = eligibility_reason(
            facts.profile,
            facts.bl_compat_id,
            facts.el_present.map(|value| value != 0),
            facts.rpu_present.map(|value| value != 0),
        ) {
            return Ok(QueueDvConversionOutcome::Ineligible(reason));
        }

        if let Some(existing) = self.dv_conversion(file_id).await? {
            match existing.state {
                DvConversionState::Committed => {
                    return Ok(QueueDvConversionOutcome::AlreadyCommitted(existing));
                }
                DvConversionState::Queued
                | DvConversionState::Running
                | DvConversionState::Verified => {
                    return Ok(QueueDvConversionOutcome::AlreadyActive(existing));
                }
                DvConversionState::Failed => {}
            }
        }

        self.client()
            .execute(
                "INSERT INTO dv_conversions (file_id, state, queued_at_ms)
                 VALUES ($1, 'queued', $2)
                 ON CONFLICT(file_id) DO UPDATE SET
                   state = 'queued', el_type = NULL, original_path = NULL,
                   bytes_before = NULL, bytes_after = NULL, error = NULL,
                   queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL
                 WHERE dv_conversions.state = 'failed'",
                params!(file_id, queued_at_ms),
            )
            .await?;
        let queued = self.dv_conversion(file_id).await?.ok_or_else(|| {
            StoreError::Database("queued Dolby Vision conversion disappeared".to_owned())
        })?;
        Ok(match queued.state {
            DvConversionState::Queued => QueueDvConversionOutcome::Queued(queued),
            DvConversionState::Committed => QueueDvConversionOutcome::AlreadyCommitted(queued),
            _ => QueueDvConversionOutcome::AlreadyActive(queued),
        })
    }

    async fn queue_library_dv_conversions(
        &self,
        library_id: i64,
        queued_at_ms: i64,
        retry_failed: bool,
    ) -> Result<u64, StoreError> {
        let changed = self
            .client()
            .execute(
                "WITH requested(library_id, queued_at_ms, retry_failed) AS
                       (VALUES ($1, $2, $3))
                 INSERT INTO dv_conversions (file_id, state, queued_at_ms)
                 SELECT f.id, 'queued', requested.queued_at_ms
                   FROM files f
                   JOIN items i ON i.id = f.item_id
                   JOIN requested ON requested.library_id = i.library_id
                  WHERE f.dv_profile = 7
                    AND f.dv_bl_compat_id IN (1, 6)
                    AND f.dv_el_present = 1
                    AND f.dv_rpu_present = 1
                 ON CONFLICT(file_id) DO UPDATE SET
                   state = 'queued', el_type = NULL, original_path = NULL,
                   bytes_before = NULL, bytes_after = NULL, error = NULL,
                   queued_at_ms = excluded.queued_at_ms, finished_at_ms = NULL
                 WHERE $3 AND dv_conversions.state = 'failed'",
                params!(library_id, queued_at_ms, retry_failed),
            )
            .await?;
        Ok(changed as u64)
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

    async fn dv_conversion_progress(
        &self,
        library_id: Option<i64>,
    ) -> Result<DvConversionProgress, StoreError> {
        let rows = self
            .client()
            .query_consistent_map::<ProgressRow, _>(
                "SELECT
                   COALESCE(SUM(CASE WHEN f.dv_profile = 7
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

    async fn mark_dv_conversion_running(
        &self,
        file_id: i64,
        bytes_before: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .client()
            .execute(
                "UPDATE dv_conversions
                    SET state = 'running', bytes_before = $2, bytes_after = NULL,
                        error = NULL, finished_at_ms = NULL
                  WHERE file_id = $1 AND state IN ('queued', 'running')",
                params!(file_id, bytes_before),
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
                    SET state = 'verified', el_type = $2, bytes_after = $3, error = NULL
                  WHERE file_id = $1 AND state = 'running'",
                params!(file_id, el_type, bytes_after),
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
                    SET state = 'committed', original_path = $2, bytes_after = $3,
                        error = NULL, finished_at_ms = $4
                  WHERE file_id = $1 AND state = 'verified'",
                params!(file_id, original_path, bytes_after, finished_at_ms),
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
                    SET state = 'failed', error = $2, finished_at_ms = $3
                  WHERE file_id = $1 AND state != 'committed'",
                params!(file_id, error, finished_at_ms),
            )
            .await?
            == 1)
    }
}
