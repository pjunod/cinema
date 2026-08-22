//! Replicated whole-title speculative-transcode queue.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use super::PretranscodeJobStore;
use crate::cluster::coordination::Lease;
use crate::domain::{
    NewPretranscodeJob, PretranscodeJob, PretranscodeRequirements, PretranscodeWorkerCapabilities,
};
use crate::error::StoreError;

const CLAIM_SCAN_LIMIT: i64 = 128;
const MAX_INVALID_REPAIRS_PER_CLAIM: usize = 1;
const MAX_CLAIM_CAS_ATTEMPTS: usize = 8;
const MAX_REQUIREMENTS_BYTES: usize = 4096;
const MAX_ERROR_CODE_BYTES: usize = 64;
const MAX_ATTEMPTS: i64 = 5;
const MAX_ACTIVE_JOBS: i64 = 4_096;
const MAX_TOTAL_JOBS: i64 = 10_000;
const MAX_TERMINAL_TOMBSTONES: i64 = 4_096;
const MAX_READY_SUPPRESSORS: i64 = MAX_TOTAL_JOBS - MAX_ACTIVE_JOBS - MAX_TERMINAL_TOMBSTONES;
const PRUNE_BATCH: i64 = 512;
const MAX_QUEUED_AGE_MS: i64 = 24 * 60 * 60 * 1_000;
const ELIGIBILITY_RETRY_MS: i64 = 6 * 60 * 60 * 1_000;
const READY_REVALIDATE_MS: i64 = 6 * 60 * 60 * 1_000;
// A bounded scrub checks 128 of at most 10,000 locations every 15 minutes.
// Forty-eight hours covers a complete rotation with enough margin for pauses.
const HOLDER_FRESH_SECS: i64 = 48 * 60 * 60;
// Equal to the active-row ceiling: a heterogeneous-mount worker can remember
// every locally unreadable active row, so one readable lower-priority row can
// never be starved by refusal rotation.
const MAX_LOCAL_EXCLUSIONS: usize = MAX_ACTIVE_JOBS as usize + 12;

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

fn parse_requirements(json: &str) -> Option<PretranscodeRequirements> {
    if json.len() > MAX_REQUIREMENTS_BYTES {
        return None;
    }
    serde_json::from_str::<PretranscodeRequirements>(json)
        .ok()
        .filter(PretranscodeRequirements::validate)
}

fn validate_new(job: &NewPretranscodeJob) -> Result<(), StoreError> {
    let valid_reason = matches!(job.reason.as_str(), "in_progress" | "next_up" | "recent");
    let valid = uuid::Uuid::parse_str(&job.id).is_ok()
        && !job.dedupe_key.is_empty()
        && job.dedupe_key.len() <= 256
        && job.file_id > 0
        && job.source_size >= 0
        && job.target_height > 0
        && !job.policy_generation.is_empty()
        && job.policy_generation.len() <= 128
        && valid_reason
        && parse_requirements(&job.requirements_json).is_some();
    if valid {
        Ok(())
    } else {
        Err(StoreError::Task(
            "invalid speculative-transcode queue candidate".to_owned(),
        ))
    }
}

fn collect_rows(
    rows: Vec<Result<JobRow, hiqlite::Error>>,
) -> Result<Vec<PretranscodeJob>, StoreError> {
    rows.into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map(|rows| rows.into_iter().map(|row| row.0).collect())
        .map_err(database_error)
}

#[async_trait]
impl PretranscodeJobStore for HiqliteAuthStore {
    async fn pretranscode_job(&self, id: &str) -> Result<Option<PretranscodeJob>, StoreError> {
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

    async fn enqueue_pretranscode_job(
        &self,
        job: &NewPretranscodeJob,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        validate_new(job)?;
        let fence = i64::try_from(lease.fence)
            .map_err(|error| StoreError::Database(format!("lease fence is invalid: {error}")))?;
        let revision = i64::try_from(lease.revision)
            .map_err(|error| StoreError::Database(format!("lease revision is invalid: {error}")))?;
        let observed_at_unix_ms = job.created_at_ms;
        let lease_current = "EXISTS (
            SELECT 1 FROM job_leases
             WHERE resource = $1 AND owner_node_id = $2
               AND fence = $3 AND revision = $4
               AND expires_at_ms = $5)";
        let statements = vec![
            (
                format!(
                    "UPDATE pretranscode_jobs
                        SET state = 'cancelled', last_error_code = 'eligibility_expired',
                            policy_generation = '', requirements_json = '{{}}',
                            owner_node_id = NULL, staging_node_id = NULL,
                            lease_expires_ms = NULL, updated_at_ms = $6
                      WHERE id IN (
                        SELECT id FROM pretranscode_jobs
                         WHERE state = 'queued' AND updated_at_ms <= $7
                         ORDER BY updated_at_ms, id LIMIT $8)
                        AND {lease_current}"
                ),
                params!(
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    observed_at_unix_ms,
                    observed_at_unix_ms.saturating_sub(MAX_QUEUED_AGE_MS),
                    PRUNE_BATCH
                ),
            ),
            (
                format!(
                    "DELETE FROM pretranscode_jobs WHERE id IN (
                        SELECT job.id FROM pretranscode_jobs job
                         WHERE job.state = 'ready' AND NOT EXISTS (
                             SELECT 1 FROM transcode_cache_locations location
                              WHERE location.recipe_hash = job.recipe_hash
                                AND location.node_id = job.storage_id
                                AND location.storage_class = 'local'
                                AND location.complete = 1)
                         ORDER BY job.updated_at_ms, job.id LIMIT $6)
                     AND {lease_current}"
                ),
                params!(
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    PRUNE_BATCH
                ),
            ),
            (
                format!(
                    "DELETE FROM pretranscode_jobs WHERE id IN (
                        SELECT id FROM pretranscode_jobs
                         WHERE state = 'ready'
                         ORDER BY updated_at_ms DESC, id DESC
                         LIMIT -1 OFFSET $6)
                     AND {lease_current}"
                ),
                params!(
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    MAX_READY_SUPPRESSORS
                ),
            ),
            (
                format!(
                    "DELETE FROM pretranscode_jobs WHERE id IN (
                        SELECT id FROM pretranscode_jobs
                         WHERE state IN ('failed', 'cancelled')
                         ORDER BY updated_at_ms DESC, id DESC
                         LIMIT -1 OFFSET $6)
                     AND {lease_current}"
                ),
                params!(
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    MAX_TERMINAL_TOMBSTONES
                ),
            ),
            (
                "INSERT INTO pretranscode_jobs
                    (id, dedupe_key, file_id, source_size, source_mtime, target_height,
                     policy_generation, requirements_json, reason, priority, state,
                     owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
                     created_at_ms, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'queued',
                        NULL, 0, NULL, 0, $11, $12, $12
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = $3 AND size = $4 AND mtime = $5)
                    AND NOT EXISTS (
                        SELECT 1 FROM pretranscode_jobs
                         WHERE dedupe_key = $2
                           AND (state IN ('queued', 'running', 'failed')
                             OR (state = 'cancelled'
                               AND NOT (
                                 last_error_code = 'policy_changed'
                                 OR (last_error_code = 'eligibility_expired'
                                   AND updated_at_ms <= $12 - $20))))
                    AND NOT EXISTS (
                        SELECT 1
                          FROM pretranscode_jobs job
                          JOIN transcode_cache_locations location
                            ON location.recipe_hash = job.recipe_hash
                           AND location.node_id = job.storage_id
                           AND location.storage_class = 'local'
                           AND location.complete = 1
                         WHERE job.dedupe_key = $2 AND job.state = 'ready'
                           AND job.updated_at_ms > $12 - $21
                           AND MAX(location.last_seen_at, location.last_used_at)
                               > ($12 / 1000) - $22)
                    AND (SELECT COUNT(*) FROM pretranscode_jobs
                          WHERE state IN ('queued', 'running')) < $18
                    AND (SELECT COUNT(*) FROM pretranscode_jobs) < $19
                    AND EXISTS (
                        SELECT 1 FROM job_leases
                         WHERE resource = $13 AND owner_node_id = $14
                           AND fence = $15 AND revision = $16
                           AND expires_at_ms = $17)"
                    .to_owned(),
                params!(
                    &job.id,
                    &job.dedupe_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    job.target_height,
                    &job.policy_generation,
                    &job.requirements_json,
                    &job.reason,
                    job.priority,
                    job.not_before_ms,
                    job.created_at_ms,
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    MAX_ACTIVE_JOBS,
                    MAX_TOTAL_JOBS,
                    ELIGIBILITY_RETRY_MS,
                    READY_REVALIDATE_MS,
                    HOLDER_FRESH_SECS
                ),
            ),
        ];
        let results = self
            .atomic_publication(lease, replacement, statements)
            .await?;
        let changed = results.get(4).copied().unwrap_or_default();
        Ok(changed == 1)
    }

    async fn claim_pretranscode_job(
        &self,
        node_id: &str,
        capabilities: &PretranscodeWorkerCapabilities,
        excluded_job_ids: &[String],
        now_unix_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<PretranscodeJob>, StoreError> {
        if node_id.is_empty()
            || node_id.len() > 128
            || !capabilities.validate()
            || excluded_job_ids.len() > MAX_LOCAL_EXCLUSIONS
            || excluded_job_ids
                .iter()
                .any(|id| uuid::Uuid::parse_str(id).is_err())
            || lease_expires_ms <= now_unix_ms
        {
            return Err(StoreError::Task(
                "invalid speculative-transcode worker claim".to_owned(),
            ));
        }
        let excluded_job_ids = excluded_job_ids
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let mut cursor: Option<(i64, i64, String)> = None;
        let mut invalid_repairs = 0_usize;
        let mut claim_cas_attempts = 0_usize;
        loop {
            let (has_cursor, cursor_priority, cursor_created, cursor_id) = cursor
                .as_ref()
                .map_or((0, 0, 0, ""), |(priority, created, id)| {
                    (1, *priority, *created, id.as_str())
                });
            let sql = format!(
                "SELECT {JOB_COLS} FROM pretranscode_jobs
                  WHERE ((state = 'queued' AND not_before_ms <= $1)
                      OR (state = 'running' AND lease_expires_ms <= $1))
                    AND fence < 9223372036854775807
                    AND ($3 = 0 OR priority < $4
                      OR (priority = $4 AND created_at_ms > $5)
                      OR (priority = $4 AND created_at_ms = $5 AND id > $6))
                  ORDER BY priority DESC, created_at_ms, id LIMIT $2"
            );
            let candidates = self
                .client()
                .query_consistent_map::<JobRow, _>(
                    sql,
                    params!(
                        now_unix_ms,
                        CLAIM_SCAN_LIMIT,
                        has_cursor,
                        cursor_priority,
                        cursor_created,
                        cursor_id
                    ),
                )
                .await?;
            let Some(last) = candidates.last() else {
                break;
            };
            cursor = Some((last.0.priority, last.0.created_at_ms, last.0.id.clone()));
            for candidate in candidates.into_iter().map(|row| row.0) {
                if excluded_job_ids.contains(&candidate.id) {
                    continue;
                }
                let Some(requirements) = parse_requirements(&candidate.requirements_json) else {
                    if invalid_repairs < MAX_INVALID_REPAIRS_PER_CLAIM {
                        self.client()
                            .execute(
                                "UPDATE pretranscode_jobs
                                SET state = 'failed', last_error_code = 'invalid_requirements',
                                    policy_generation = '', requirements_json = '{}',
                                    owner_node_id = NULL, staging_node_id = NULL,
                                    lease_expires_ms = NULL, updated_at_ms = $2
                              WHERE id = $1 AND state = $3
                                AND COALESCE(owner_node_id, '') = $4 AND fence = $5
                                AND COALESCE(lease_expires_ms, 0) = $6",
                                params!(
                                    &candidate.id,
                                    now_unix_ms,
                                    &candidate.state,
                                    &candidate.owner_node_id,
                                    candidate.fence,
                                    candidate.lease_expires_ms
                                ),
                            )
                            .await?;
                        invalid_repairs += 1;
                    }
                    continue;
                };
                if !requirements.compatible_with(capabilities) {
                    continue;
                }
                if candidate.target_height > capabilities.max_target_height {
                    continue;
                }
                if claim_cas_attempts >= MAX_CLAIM_CAS_ATTEMPTS {
                    return Ok(None);
                }
                claim_cas_attempts += 1;
                let update = format!(
                    "UPDATE pretranscode_jobs
                    SET state = 'running', owner_node_id = $2, staging_node_id = $2,
                        fence = fence + 1,
                        lease_expires_ms = $4, updated_at_ms = $3
                  WHERE id = $1
                    AND ((state = 'queued' AND not_before_ms <= $3)
                      OR (state = 'running' AND lease_expires_ms <= $3))
                    AND fence < 9223372036854775807
                  RETURNING {JOB_COLS}"
                );
                let rows = collect_rows(
                    self.client()
                        .execute_returning_map::<_, JobRow>(
                            update,
                            params!(&candidate.id, node_id, now_unix_ms, lease_expires_ms),
                        )
                        .await?,
                )?;
                if let Some(claimed) = rows.into_iter().next() {
                    return Ok(Some(claimed));
                }
            }
        }
        Ok(None)
    }

    async fn pretranscode_staging_jobs(&self, node_id: &str) -> Result<Vec<String>, StoreError> {
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
        Ok(self
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
            .collect())
    }

    async fn active_pretranscode_job_ids(&self) -> Result<Vec<String>, StoreError> {
        struct ActiveIdRow(String);
        impl From<&mut Row<'_>> for ActiveIdRow {
            fn from(row: &mut Row<'_>) -> Self {
                Self(row.get("id"))
            }
        }
        Ok(self
            .client()
            .query_consistent_map::<ActiveIdRow, _>(
                "SELECT id FROM pretranscode_jobs
                  WHERE state IN ('queued', 'running') ORDER BY id LIMIT $1",
                params!(MAX_ACTIVE_JOBS + 1),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn renew_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        now_unix_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<PretranscodeJob>, StoreError> {
        if lease_expires_ms <= now_unix_ms || lease_expires_ms <= job.lease_expires_ms {
            return Ok(None);
        }
        let sql = format!(
            "UPDATE pretranscode_jobs
                SET lease_expires_ms = $5, updated_at_ms = $4
              WHERE id = $1 AND state = 'running' AND owner_node_id = $2
                AND fence = $3 AND lease_expires_ms = $6 AND lease_expires_ms > $4
              RETURNING {JOB_COLS}"
        );
        Ok(collect_rows(
            self.client()
                .execute_returning_map::<_, JobRow>(
                    sql,
                    params!(
                        &job.id,
                        &job.owner_node_id,
                        job.fence,
                        now_unix_ms,
                        lease_expires_ms,
                        job.lease_expires_ms
                    ),
                )
                .await?,
        )?
        .into_iter()
        .next())
    }

    async fn yield_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        now_unix_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, StoreError> {
        // The leader orders this exact-token predicate and mutation as one
        // Raft entry. The daemon captures `now_unix_ms` while holding its local
        // token lock, so either a successor claim is ordered first and this
        // rejects, or this fresh settlement is ordered first and wins.
        Ok(self
            .client()
            .execute(
                "UPDATE pretranscode_jobs
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        not_before_ms = $5, updated_at_ms = $4
                  WHERE id = $1 AND state = 'running' AND owner_node_id = $2
                    AND fence = $3 AND lease_expires_ms = $6 AND lease_expires_ms > $4",
                params!(
                    &job.id,
                    &job.owner_node_id,
                    job.fence,
                    now_unix_ms,
                    not_before_ms.max(now_unix_ms),
                    job.lease_expires_ms
                ),
            )
            .await?
            == 1)
    }

    async fn fail_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        error_code: &str,
        now_unix_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, StoreError> {
        if error_code.is_empty() || error_code.len() > MAX_ERROR_CODE_BYTES {
            return Err(StoreError::Task(
                "invalid speculative-transcode error code".to_owned(),
            ));
        }
        Ok(self
            .client()
            .execute(
                "UPDATE pretranscode_jobs
                    SET state = CASE WHEN attempts + 1 >= $7 THEN 'failed' ELSE 'queued' END,
                        owner_node_id = NULL, lease_expires_ms = NULL,
                        staging_node_id = CASE WHEN attempts + 1 >= $7
                                               THEN NULL ELSE staging_node_id END,
                        policy_generation = CASE WHEN attempts + 1 >= $7
                                                 THEN '' ELSE policy_generation END,
                        requirements_json = CASE WHEN attempts + 1 >= $7
                                                 THEN '{}' ELSE requirements_json END,
                        attempts = attempts + 1, not_before_ms = $5,
                        last_error_code = $6, updated_at_ms = $4
                  WHERE id = $1 AND state = 'running' AND owner_node_id = $2
                    AND fence = $3 AND lease_expires_ms = $8 AND lease_expires_ms > $4",
                params!(
                    &job.id,
                    &job.owner_node_id,
                    job.fence,
                    now_unix_ms,
                    not_before_ms.max(now_unix_ms),
                    error_code,
                    MAX_ATTEMPTS,
                    job.lease_expires_ms
                ),
            )
            .await?
            == 1)
    }

    async fn cancel_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        error_code: &str,
        now_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        if error_code.is_empty() || error_code.len() > MAX_ERROR_CODE_BYTES {
            return Err(StoreError::Task(
                "invalid speculative-transcode cancellation code".to_owned(),
            ));
        }
        Ok(self
            .client()
            .execute(
                "UPDATE pretranscode_jobs
                    SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                        staging_node_id = NULL, policy_generation = '',
                        requirements_json = '{}',
                        last_error_code = $5, updated_at_ms = $4
                  WHERE id = $1 AND state = 'running' AND owner_node_id = $2
                    AND fence = $3 AND lease_expires_ms = $6 AND lease_expires_ms > $4",
                params!(
                    &job.id,
                    &job.owner_node_id,
                    job.fence,
                    now_unix_ms,
                    error_code,
                    job.lease_expires_ms
                ),
            )
            .await?
            == 1)
    }

    async fn complete_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        recipe_hash: &str,
        recipe_version: i64,
        relative_dir: &str,
        bytes: i64,
        expected_previous_bytes: Option<i64>,
        manifest_digest: &str,
        now_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        if recipe_hash.is_empty()
            || relative_dir.is_empty()
            || manifest_digest.len() != 64
            || bytes < 0
            || expected_previous_bytes.is_some_and(|expected| expected < 0 || bytes < expected)
        {
            return Err(StoreError::Task(
                "invalid speculative-transcode completion".to_owned(),
            ));
        }
        let current = "EXISTS (
            SELECT 1 FROM pretranscode_jobs job JOIN files file ON file.id = job.file_id
             WHERE job.id = $1 AND job.state = 'running' AND job.owner_node_id = $2
               AND job.fence = $3 AND job.lease_expires_ms = $4
               AND job.lease_expires_ms > $5
               AND file.size = job.source_size AND file.mtime = job.source_mtime)"
            .to_owned();
        let statements = vec![
            (
                format!(
                    "INSERT INTO transcode_cache_recipes
                        (recipe_hash, file_id, recipe_version, created_at)
                     SELECT $6, $7, $8, $9 WHERE {current}
                     ON CONFLICT(recipe_hash) DO NOTHING"
                ),
                params!(
                    &job.id,
                    &job.owner_node_id,
                    job.fence,
                    job.lease_expires_ms,
                    now_unix_ms,
                    recipe_hash,
                    job.file_id,
                    recipe_version,
                    now_unix_ms / 1000
                ),
            ),
            (
                format!(
                    "UPDATE offline_packages
                        SET actual_bytes = actual_bytes + ($8 - $9), updated_at = $10
                      WHERE $9 IS NOT NULL AND state = 'ready'
                        AND recipe_hash = $6 AND node_id = $2
                        AND EXISTS (
                            SELECT 1 FROM transcode_cache_locations location
                             WHERE location.recipe_hash = $6 AND location.node_id = $2
                               AND location.storage_class = 'local'
                               AND location.relative_dir = $7 AND location.complete = 1
                               AND location.bytes = $9 AND location.manifest_digest IS NULL)
                        AND {current}"
                ),
                params!(
                    &job.id,
                    &job.owner_node_id,
                    job.fence,
                    job.lease_expires_ms,
                    now_unix_ms,
                    recipe_hash,
                    relative_dir,
                    bytes,
                    expected_previous_bytes,
                    now_unix_ms / 1000
                ),
            ),
            (
                format!(
                    "INSERT INTO transcode_cache_locations
                        (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                         manifest_digest, scrub_object_index, last_used_at, last_seen_at)
                     SELECT $6, $2, 'local', $7, $8, 1, $10, 0, $9, $9 WHERE {current}
                     ON CONFLICT(recipe_hash, node_id, storage_class) DO UPDATE SET
                        relative_dir = excluded.relative_dir, bytes = excluded.bytes,
                        complete = 1, manifest_digest = excluded.manifest_digest,
                        scrub_object_index = 0,
                        last_seen_at = excluded.last_seen_at
                     WHERE transcode_cache_locations.complete = 0
                        OR (transcode_cache_locations.relative_dir = excluded.relative_dir
                            AND (($11 IS NULL
                                  AND transcode_cache_locations.bytes = excluded.bytes
                                  AND (transcode_cache_locations.manifest_digest IS NULL
                                    OR transcode_cache_locations.manifest_digest = excluded.manifest_digest))
                              OR ($11 IS NOT NULL
                                  AND transcode_cache_locations.bytes = $11
                                  AND transcode_cache_locations.manifest_digest IS NULL
                                  AND excluded.bytes >= $11)))"
                ),
                params!(
                    &job.id,
                    &job.owner_node_id,
                    job.fence,
                    job.lease_expires_ms,
                    now_unix_ms,
                    recipe_hash,
                    relative_dir,
                    bytes,
                    now_unix_ms / 1000,
                    manifest_digest
                    ,expected_previous_bytes
                ),
            ),
            (
                "UPDATE pretranscode_jobs
                    SET state = 'ready', recipe_hash = $6, storage_id = $2,
                        relative_dir = $7, manifest_digest = $8,
                        owner_node_id = NULL, staging_node_id = NULL,
                        lease_expires_ms = NULL, policy_generation = '',
                        requirements_json = '{}', updated_at_ms = $5
                  WHERE id = $1 AND state = 'running' AND owner_node_id = $2
                    AND fence = $3 AND lease_expires_ms = $4
                    AND lease_expires_ms > $5
                    AND EXISTS (SELECT 1 FROM files
                                 WHERE id = pretranscode_jobs.file_id
                                   AND size = pretranscode_jobs.source_size
                                   AND mtime = pretranscode_jobs.source_mtime)
                    AND EXISTS (
                        SELECT 1 FROM transcode_cache_locations location
                         WHERE location.recipe_hash = $6 AND location.node_id = $2
                           AND location.storage_class = 'local' AND location.complete = 1
                           AND location.relative_dir = $7 AND location.bytes = $9
                           AND location.manifest_digest = $8)"
                    .to_owned(),
                params!(
                    &job.id,
                    &job.owner_node_id,
                    job.fence,
                    job.lease_expires_ms,
                    now_unix_ms,
                    recipe_hash,
                    relative_dir,
                    manifest_digest,
                    bytes
                ),
            ),
        ];
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        let completed = results.get(3).copied().unwrap_or_default();
        if completed == 0 && results.iter().take(3).any(|rows| *rows > 0) {
            return Err(StoreError::Database(
                "pretranscode publication changed cache state without settling its fence"
                    .to_owned(),
            ));
        }
        Ok(completed == 1)
    }
}
