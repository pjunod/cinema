//! Read-only legacy transcode history and staging retention during cutover.
use super::SqliteStore;
use crate::domain::PretranscodeJob;
use crate::error::StoreError;
use crate::store::{BackgroundJobStore, PretranscodeJobStore};
use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row};

fn job_from_row(row: &Row<'_>) -> rusqlite::Result<PretranscodeJob> {
    Ok(PretranscodeJob {
        id: row.get(0)?,
        dedupe_key: row.get(1)?,
        file_id: row.get(2)?,
        source_size: row.get(3)?,
        source_mtime: row.get(4)?,
        target_height: row.get(5)?,
        policy_generation: row.get(6)?,
        requirements_json: row.get(7)?,
        reason: row.get(8)?,
        priority: row.get(9)?,
        state: row.get(10)?,
        owner_node_id: row.get(11)?,
        fence: row.get(12)?,
        lease_expires_ms: row.get(13)?,
        attempts: row.get(14)?,
        not_before_ms: row.get(15)?,
        created_at_ms: row.get(16)?,
        updated_at_ms: row.get(17)?,
    })
}

const JOB_COLS: &str = "id, dedupe_key, file_id, source_size, source_mtime, target_height, \
    policy_generation, requirements_json, reason, priority, state, \
    COALESCE(owner_node_id, ''), fence, COALESCE(lease_expires_ms, 0), attempts, \
    not_before_ms, created_at_ms, updated_at_ms";

#[async_trait]
impl PretranscodeJobStore for SqliteStore {
    async fn pretranscode_job(&self, id: &str) -> Result<Option<PretranscodeJob>, StoreError> {
        if let Some(job) = self.background_job(id).await? {
            return super::super::background_jobs_pretranscode::projection(&job).map(Some);
        }
        let id = id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {JOB_COLS} FROM pretranscode_jobs WHERE id = ?1"),
                    [id],
                    job_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn pretranscode_staging_jobs(&self, node_id: &str) -> Result<Vec<String>, StoreError> {
        let mut jobs = self.background_staging_jobs(node_id).await?;
        if node_id.is_empty() || node_id.len() > 128 {
            return Err(StoreError::Task(
                "invalid speculative-transcode staging owner".to_owned(),
            ));
        }
        let node_id = node_id.to_owned();
        let legacy = self
            .with_conn(move |conn| {
                let mut statement = conn.prepare(
                    "SELECT id FROM pretranscode_jobs
                  WHERE staging_node_id = ?1 AND state IN ('queued', 'running')
                  ORDER BY id",
                )?;
                let rows = statement.query_map(params![node_id], |row| row.get(0))?;
                rows.collect::<Result<Vec<String>, _>>()
                    .map_err(StoreError::from)
            })
            .await?;
        jobs.extend(legacy);
        jobs.sort_unstable();
        jobs.dedup();
        Ok(jobs)
    }
}
