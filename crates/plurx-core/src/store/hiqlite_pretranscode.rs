//! Legacy transcode schema, read-only history and staging retention.
use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use super::{BackgroundJobStore, PretranscodeJobStore};
use crate::domain::PretranscodeJob;
use crate::error::StoreError;
use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

pub(super) const PRETRANSCODE_JOBS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS pretranscode_jobs (
    id                TEXT PRIMARY KEY,
    dedupe_key        TEXT NOT NULL,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    target_height     INTEGER NOT NULL,
    policy_generation TEXT NOT NULL,
    requirements_json TEXT NOT NULL,
    reason            TEXT NOT NULL CHECK (reason IN ('in_progress', 'next_up', 'recent')),
    priority          INTEGER NOT NULL,
    state             TEXT NOT NULL CHECK (
                          state IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
    owner_node_id     TEXT,
    staging_node_id   TEXT,
    fence             INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms  INTEGER,
    attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms     INTEGER NOT NULL,
    last_error_code   TEXT,
    recipe_hash       TEXT,
    storage_id        TEXT,
    relative_dir      TEXT,
    manifest_digest   TEXT,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL
) STRICT";

pub(super) const PRETRANSCODE_DUE_INDEX: &str = "CREATE INDEX IF NOT EXISTS pretranscode_jobs_due
        ON pretranscode_jobs(state, not_before_ms, priority DESC, created_at_ms, id)";

pub(super) const PRETRANSCODE_DEDUPE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS pretranscode_jobs_dedupe
        ON pretranscode_jobs(dedupe_key, state)";

pub(super) const PRETRANSCODE_STAGING_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS pretranscode_jobs_staging
        ON pretranscode_jobs(staging_node_id, state, id)";

pub(super) const PRETRANSCODE_ACTIVE_INDEX: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS pretranscode_jobs_active
        ON pretranscode_jobs(dedupe_key) WHERE state IN ('queued', 'running')";

pub(super) const PRETRANSCODE_SOURCE_TRIGGER: &str =
    "CREATE TRIGGER IF NOT EXISTS pretranscode_jobs_cancel_source BEFORE DELETE ON files
     BEGIN
       DELETE FROM pretranscode_jobs
        WHERE file_id = OLD.id AND state IN ('ready', 'failed', 'cancelled');
       UPDATE pretranscode_jobs
          SET state = 'cancelled', owner_node_id = NULL, staging_node_id = NULL,
              lease_expires_ms = NULL, policy_generation = '', requirements_json = '{}'
        WHERE file_id = OLD.id AND state IN ('queued', 'running');
     END";

/// v9 also binds a generation manifest to the cache location that serves it.
pub(super) const CACHE_MANIFEST_DIGEST_MIGRATION: &str =
    "ALTER TABLE transcode_cache_locations ADD COLUMN manifest_digest TEXT";
pub(super) const CACHE_SCRUB_CURSOR_MIGRATION: &str = "ALTER TABLE transcode_cache_locations
    ADD COLUMN scrub_object_index INTEGER NOT NULL DEFAULT 0";

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    for sql in [
        PRETRANSCODE_JOBS_SCHEMA,
        PRETRANSCODE_DUE_INDEX,
        PRETRANSCODE_DEDUPE_INDEX,
        PRETRANSCODE_STAGING_INDEX,
        PRETRANSCODE_ACTIVE_INDEX,
        PRETRANSCODE_SOURCE_TRIGGER,
    ] {
        validate_sql(sql)?;
        for result in timeout_store(client.batch(sql)).await? {
            result.map_err(database_error)?;
        }
    }
    Ok(())
}

const JOB_COLS: &str = "id, dedupe_key, file_id, source_size, source_mtime, target_height,
    policy_generation, requirements_json, reason, priority, state,
    COALESCE(owner_node_id, '') AS owner_node_id, fence,
    COALESCE(lease_expires_ms, 0) AS lease_expires_ms, attempts, not_before_ms,
    created_at_ms, updated_at_ms";

struct JobRow(PretranscodeJob);

impl From<&mut Row<'_>> for JobRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(PretranscodeJob {
            id: row.get("id"),
            dedupe_key: row.get("dedupe_key"),
            file_id: row.get("file_id"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            target_height: row.get("target_height"),
            policy_generation: row.get("policy_generation"),
            requirements_json: row.get("requirements_json"),
            reason: row.get("reason"),
            priority: row.get("priority"),
            state: row.get("state"),
            owner_node_id: row.get("owner_node_id"),
            fence: row.get("fence"),
            lease_expires_ms: row.get("lease_expires_ms"),
            attempts: row.get("attempts"),
            not_before_ms: row.get("not_before_ms"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        })
    }
}

#[async_trait]
impl PretranscodeJobStore for HiqliteAuthStore {
    async fn pretranscode_job(&self, id: &str) -> Result<Option<PretranscodeJob>, StoreError> {
        if let Some(job) = self.background_job(id).await? {
            return super::background_jobs_pretranscode::projection(&job).map(Some);
        }
        Ok(self
            .client()
            .query_consistent_map::<JobRow, _>(
                format!("SELECT {JOB_COLS} FROM pretranscode_jobs WHERE id = $1"),
                params!(id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn pretranscode_staging_jobs(&self, node_id: &str) -> Result<Vec<String>, StoreError> {
        let mut jobs = self.background_staging_jobs(node_id).await?;
        if node_id.is_empty() || node_id.len() > 128 {
            return Err(StoreError::Task(
                "invalid speculative-transcode staging owner".to_owned(),
            ));
        }
        struct StagingJobRow(String);
        impl From<&mut Row<'_>> for StagingJobRow {
            fn from(row: &mut Row<'_>) -> Self {
                Self(row.get("id"))
            }
        }
        let legacy = self
            .client()
            .query_consistent_map::<StagingJobRow, _>(
                "SELECT id FROM pretranscode_jobs
                  WHERE staging_node_id = $1 AND state IN ('queued', 'running')
                  ORDER BY id",
                params!(node_id),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>();
        jobs.extend(legacy);
        jobs.sort_unstable();
        jobs.dedup();
        Ok(jobs)
    }
}
