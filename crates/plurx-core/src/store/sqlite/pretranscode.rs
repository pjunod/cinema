//! Durable whole-title speculative-transcode queue.
//!
//! Queue ownership is a row-local monotone fence. The candidate scheduler's
//! lease authorizes enqueue only; workers settle with this token so candidate
//! renewal and execution failure remain independent failure domains.

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row, Transaction, TransactionBehavior};

use super::SqliteStore;
use crate::cluster::coordination::Lease;
use crate::domain::{
    NewPretranscodeJob, PretranscodeJob, PretranscodeRequirements, PretranscodeWorkerCapabilities,
};
use crate::error::StoreError;
use crate::store::PretranscodeJobStore;

const CLAIM_SCAN_LIMIT: i64 = 128;
const MAX_INVALID_REPAIRS_PER_CLAIM: usize = 1;
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

fn execution_unix_ms() -> Result<i64, StoreError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| StoreError::Task(format!("system clock precedes unix epoch: {error}")))?
        .as_millis()
        .min(i64::MAX as u128) as i64)
}

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

#[async_trait]
impl PretranscodeJobStore for SqliteStore {
    async fn pretranscode_job(&self, id: &str) -> Result<Option<PretranscodeJob>, StoreError> {
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

    async fn enqueue_pretranscode_job(
        &self,
        job: &NewPretranscodeJob,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        validate_new(job)?;
        let job = job.clone();
        self.with_fenced_conn(lease, replacement, move |conn| {
            let observed_at_unix_ms = job.created_at_ms;
            conn.execute(
                "UPDATE pretranscode_jobs
                    SET state = 'cancelled', last_error_code = 'eligibility_expired',
                        policy_generation = '', requirements_json = '{}',
                        owner_node_id = NULL, staging_node_id = NULL,
                        lease_expires_ms = NULL, updated_at_ms = ?1
                  WHERE id IN (
                    SELECT id FROM pretranscode_jobs
                     WHERE state = 'queued' AND updated_at_ms <= ?2
                     ORDER BY updated_at_ms, id LIMIT ?3)",
                params![
                    observed_at_unix_ms,
                    observed_at_unix_ms.saturating_sub(MAX_QUEUED_AGE_MS),
                    PRUNE_BATCH,
                ],
            )?;
            conn.execute(
                "DELETE FROM pretranscode_jobs WHERE id IN (
                    SELECT job.id FROM pretranscode_jobs job
                     WHERE job.state = 'ready' AND NOT EXISTS (
                         SELECT 1 FROM transcode_cache_locations location
                          WHERE location.recipe_hash = job.recipe_hash
                            AND location.node_id = job.storage_id
                            AND location.storage_class = 'local'
                            AND location.complete = 1)
                     ORDER BY job.updated_at_ms, job.id LIMIT ?1)",
                [PRUNE_BATCH],
            )?;
            conn.execute(
                "DELETE FROM pretranscode_jobs WHERE id IN (
                    SELECT id FROM pretranscode_jobs
                     WHERE state IN ('failed', 'cancelled')
                     ORDER BY updated_at_ms DESC, id DESC
                     LIMIT -1 OFFSET ?1)",
                [MAX_TERMINAL_TOMBSTONES],
            )?;
            conn.execute(
                "DELETE FROM pretranscode_jobs WHERE id IN (
                    SELECT id FROM pretranscode_jobs
                     WHERE state = 'ready'
                     ORDER BY updated_at_ms DESC, id DESC
                     LIMIT -1 OFFSET ?1)",
                [MAX_READY_SUPPRESSORS],
            )?;
            let changed = conn.execute(
                "INSERT INTO pretranscode_jobs
                    (id, dedupe_key, file_id, source_size, source_mtime, target_height,
                     policy_generation, requirements_json, reason, priority, state,
                     owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
                     created_at_ms, updated_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'queued',
                        NULL, 0, NULL, 0, ?11, ?12, ?12
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = ?3 AND size = ?4 AND mtime = ?5)
                    AND NOT EXISTS (
                        SELECT 1 FROM pretranscode_jobs
                         WHERE dedupe_key = ?2
                           AND (state IN ('queued', 'running', 'failed')
                             OR (state = 'cancelled'
                               AND NOT (
                                 last_error_code = 'policy_changed'
                                 OR (last_error_code = 'eligibility_expired'
                                   AND updated_at_ms <= ?12 - ?15))))
                    AND NOT EXISTS (
                        SELECT 1
                          FROM pretranscode_jobs job
                          JOIN transcode_cache_locations location
                            ON location.recipe_hash = job.recipe_hash
                           AND location.node_id = job.storage_id
                           AND location.storage_class = 'local'
                           AND location.complete = 1
                         WHERE job.dedupe_key = ?2 AND job.state = 'ready'
                           AND job.updated_at_ms > ?12 - ?16
                           AND MAX(location.last_seen_at, location.last_used_at)
                               > (?12 / 1000) - ?17)
                    AND (SELECT COUNT(*) FROM pretranscode_jobs
                          WHERE state IN ('queued', 'running')) < ?13
                    AND (SELECT COUNT(*) FROM pretranscode_jobs) < ?14",
                params![
                    job.id,
                    job.dedupe_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    job.target_height,
                    job.policy_generation,
                    job.requirements_json,
                    job.reason,
                    job.priority,
                    job.not_before_ms,
                    job.created_at_ms,
                    MAX_ACTIVE_JOBS,
                    MAX_TOTAL_JOBS,
                    ELIGIBILITY_RETRY_MS,
                    READY_REVALIDATE_MS,
                    HOLDER_FRESH_SECS,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
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
        let node_id = node_id.to_owned();
        let capabilities = capabilities.clone();
        let excluded_job_ids = excluded_job_ids
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        self.with_conn(move |conn| {
            // IMMEDIATE obtains the WAL writer reservation before the ordered
            // read. A deferred read followed by CAS can fail its snapshot
            // upgrade with SQLITE_BUSY_SNAPSHOT when separate daemon handles
            // race on the same top row.
            let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            let mut cursor: Option<(i64, i64, String)> = None;
            let mut invalid_repairs = 0_usize;
            loop {
                let (has_cursor, cursor_priority, cursor_created, cursor_id) = cursor
                    .as_ref()
                    .map_or((0, 0, 0, ""), |(priority, created, id)| {
                        (1, *priority, *created, id.as_str())
                    });
                let candidates = {
                    let mut statement = tx.prepare(&format!(
                        "SELECT {JOB_COLS} FROM pretranscode_jobs
                          WHERE ((state = 'queued' AND not_before_ms <= ?1)
                              OR (state = 'running' AND lease_expires_ms <= ?1))
                            AND fence < 9223372036854775807
                            AND (?3 = 0 OR priority < ?4
                              OR (priority = ?4 AND created_at_ms > ?5)
                              OR (priority = ?4 AND created_at_ms = ?5 AND id > ?6))
                          ORDER BY priority DESC, created_at_ms, id
                          LIMIT ?2"
                    ))?;
                    let rows = statement.query_map(
                        params![
                            now_unix_ms,
                            CLAIM_SCAN_LIMIT,
                            has_cursor,
                            cursor_priority,
                            cursor_created,
                            cursor_id,
                        ],
                        job_from_row,
                    )?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };
                let Some(last) = candidates.last() else {
                    break;
                };
                cursor = Some((last.priority, last.created_at_ms, last.id.clone()));
                for mut candidate in candidates {
                    if excluded_job_ids.contains(&candidate.id) {
                        continue;
                    }
                    let Some(requirements) = parse_requirements(&candidate.requirements_json)
                    else {
                        if invalid_repairs < MAX_INVALID_REPAIRS_PER_CLAIM {
                            tx.execute(
                                "UPDATE pretranscode_jobs
                            SET state = 'failed', last_error_code = 'invalid_requirements',
                                policy_generation = '', requirements_json = '{}',
                                owner_node_id = NULL, staging_node_id = NULL,
                                lease_expires_ms = NULL, updated_at_ms = ?2
                          WHERE id = ?1 AND state = ?3
                            AND COALESCE(owner_node_id, '') = ?4 AND fence = ?5
                            AND COALESCE(lease_expires_ms, 0) = ?6",
                                params![
                                    candidate.id,
                                    now_unix_ms,
                                    candidate.state,
                                    candidate.owner_node_id,
                                    candidate.fence,
                                    candidate.lease_expires_ms,
                                ],
                            )?;
                            invalid_repairs += 1;
                        }
                        continue;
                    };
                    if !requirements.compatible_with(&capabilities) {
                        continue;
                    }
                    if candidate.target_height > capabilities.max_target_height {
                        continue;
                    }
                    let changed = tx.execute(
                        "UPDATE pretranscode_jobs
                        SET state = 'running', owner_node_id = ?2, staging_node_id = ?2,
                            fence = fence + 1,
                            lease_expires_ms = ?4, updated_at_ms = ?3
                      WHERE id = ?1
                        AND ((state = 'queued' AND not_before_ms <= ?3)
                          OR (state = 'running' AND lease_expires_ms <= ?3))
                        AND fence < 9223372036854775807",
                        params![candidate.id, node_id, now_unix_ms, lease_expires_ms],
                    )?;
                    if changed == 1 {
                        candidate.state = "running".to_owned();
                        candidate.owner_node_id = node_id.clone();
                        candidate.fence += 1;
                        candidate.lease_expires_ms = lease_expires_ms;
                        candidate.updated_at_ms = now_unix_ms;
                        tx.commit()?;
                        return Ok(Some(candidate));
                    }
                }
            }
            tx.commit()?;
            Ok(None)
        })
        .await
    }

    async fn pretranscode_staging_jobs(&self, node_id: &str) -> Result<Vec<String>, StoreError> {
        if node_id.is_empty() || node_id.len() > 128 {
            return Err(StoreError::Task(
                "invalid speculative-transcode staging owner".to_owned(),
            ));
        }
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id FROM pretranscode_jobs
                  WHERE staging_node_id = ?1 AND state IN ('queued', 'running')
                  ORDER BY id",
            )?;
            let rows = statement.query_map(params![node_id], |row| row.get(0))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn active_pretranscode_job_ids(&self) -> Result<Vec<String>, StoreError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id FROM pretranscode_jobs
                  WHERE state IN ('queued', 'running') ORDER BY id LIMIT ?1",
            )?;
            let rows = statement
                .query_map([MAX_ACTIVE_JOBS + 1], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
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
        let job = job.clone();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "UPDATE pretranscode_jobs
                    SET lease_expires_ms = ?5, updated_at_ms = ?4
                  WHERE id = ?1 AND state = 'running' AND owner_node_id = ?2
                    AND fence = ?3 AND lease_expires_ms = ?6 AND lease_expires_ms > ?4",
                params![
                    job.id,
                    job.owner_node_id,
                    job.fence,
                    now_unix_ms,
                    lease_expires_ms,
                    job.lease_expires_ms,
                ],
            )?;
            if changed == 1 {
                let mut replacement = job;
                replacement.lease_expires_ms = lease_expires_ms;
                replacement.updated_at_ms = now_unix_ms;
                Ok(Some(replacement))
            } else {
                Ok(None)
            }
        })
        .await
    }

    async fn yield_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        _now_unix_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, StoreError> {
        let job = job.clone();
        self.with_conn(move |conn| {
            let now_unix_ms = execution_unix_ms()?;
            Ok(conn.execute(
                "UPDATE pretranscode_jobs
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        not_before_ms = ?5, updated_at_ms = ?4
                  WHERE id = ?1 AND state = 'running' AND owner_node_id = ?2
                    AND fence = ?3 AND lease_expires_ms = ?6 AND lease_expires_ms > ?4",
                params![
                    job.id,
                    job.owner_node_id,
                    job.fence,
                    now_unix_ms,
                    not_before_ms.max(now_unix_ms),
                    job.lease_expires_ms,
                ],
            )? == 1)
        })
        .await
    }

    async fn fail_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        error_code: &str,
        _now_unix_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, StoreError> {
        if error_code.is_empty() || error_code.len() > MAX_ERROR_CODE_BYTES {
            return Err(StoreError::Task(
                "invalid speculative-transcode error code".to_owned(),
            ));
        }
        let job = job.clone();
        let error_code = error_code.to_owned();
        self.with_conn(move |conn| {
            let now_unix_ms = execution_unix_ms()?;
            Ok(conn.execute(
                "UPDATE pretranscode_jobs
                    SET state = CASE WHEN attempts + 1 >= ?7 THEN 'failed' ELSE 'queued' END,
                        owner_node_id = NULL, lease_expires_ms = NULL,
                        staging_node_id = CASE WHEN attempts + 1 >= ?7
                                               THEN NULL ELSE staging_node_id END,
                        policy_generation = CASE WHEN attempts + 1 >= ?7
                                                 THEN '' ELSE policy_generation END,
                        requirements_json = CASE WHEN attempts + 1 >= ?7
                                                 THEN '{}' ELSE requirements_json END,
                        attempts = attempts + 1, not_before_ms = ?5,
                        last_error_code = ?6, updated_at_ms = ?4
                  WHERE id = ?1 AND state = 'running' AND owner_node_id = ?2
                    AND fence = ?3 AND lease_expires_ms = ?8 AND lease_expires_ms > ?4",
                params![
                    job.id,
                    job.owner_node_id,
                    job.fence,
                    now_unix_ms,
                    not_before_ms.max(now_unix_ms),
                    error_code,
                    MAX_ATTEMPTS,
                    job.lease_expires_ms,
                ],
            )? == 1)
        })
        .await
    }

    async fn cancel_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        error_code: &str,
        _now_unix_ms: i64,
    ) -> Result<bool, StoreError> {
        if error_code.is_empty() || error_code.len() > MAX_ERROR_CODE_BYTES {
            return Err(StoreError::Task(
                "invalid speculative-transcode cancellation code".to_owned(),
            ));
        }
        let job = job.clone();
        let error_code = error_code.to_owned();
        self.with_conn(move |conn| {
            let now_unix_ms = execution_unix_ms()?;
            Ok(conn.execute(
                "UPDATE pretranscode_jobs
                    SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                        staging_node_id = NULL, policy_generation = '',
                        requirements_json = '{}',
                        last_error_code = ?5, updated_at_ms = ?4
                  WHERE id = ?1 AND state = 'running' AND owner_node_id = ?2
                    AND fence = ?3 AND lease_expires_ms = ?6 AND lease_expires_ms > ?4",
                params![
                    job.id,
                    job.owner_node_id,
                    job.fence,
                    now_unix_ms,
                    error_code,
                    job.lease_expires_ms,
                ],
            )? == 1)
        })
        .await
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
        _now_unix_ms: i64,
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
        let job = job.clone();
        let recipe_hash = recipe_hash.to_owned();
        let relative_dir = relative_dir.to_owned();
        let manifest_digest = manifest_digest.to_owned();
        self.with_conn(move |conn| {
            let now_unix_ms = execution_unix_ms()?;
            let tx = conn.unchecked_transaction()?;
            let current = tx
                .query_row(
                    "SELECT 1 FROM pretranscode_jobs job
                      JOIN files file ON file.id = job.file_id
                     WHERE job.id = ?1 AND job.state = 'running'
                       AND job.owner_node_id = ?2 AND job.fence = ?3
                       AND job.lease_expires_ms = ?4 AND job.lease_expires_ms > ?5
                       AND file.size = job.source_size AND file.mtime = job.source_mtime",
                    params![
                        job.id,
                        job.owner_node_id,
                        job.fence,
                        job.lease_expires_ms,
                        now_unix_ms,
                    ],
                    |_| Ok(()),
                )
                .optional()?;
            if current.is_none() {
                tx.commit()?;
                return Ok(false);
            }

            tx.execute(
                "INSERT INTO transcode_cache_recipes
                    (recipe_hash, file_id, recipe_version, created_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(recipe_hash) DO NOTHING",
                params![
                    recipe_hash,
                    job.file_id,
                    recipe_version,
                    now_unix_ms / 1000
                ],
            )?;
            if let Some(previous_bytes) = expected_previous_bytes {
                tx.execute(
                    "UPDATE offline_packages
                        SET actual_bytes = actual_bytes + (?3 - ?4), updated_at = ?5
                      WHERE state = 'ready' AND recipe_hash = ?1 AND node_id = ?2
                        AND EXISTS (
                            SELECT 1 FROM transcode_cache_locations location
                             WHERE location.recipe_hash = ?1 AND location.node_id = ?2
                               AND location.storage_class = 'local'
                               AND location.relative_dir = ?6 AND location.complete = 1
                               AND location.bytes = ?4 AND location.manifest_digest IS NULL)",
                    params![
                        recipe_hash,
                        job.owner_node_id,
                        bytes,
                        previous_bytes,
                        now_unix_ms / 1000,
                        relative_dir,
                    ],
                )?;
            }
            let location_changed = tx.execute(
                "INSERT INTO transcode_cache_locations
                    (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                     manifest_digest, scrub_object_index, last_used_at, last_seen_at)
                 VALUES (?1, ?2, 'local', ?3, ?4, 1, ?6, 0, ?5, ?5)
                 ON CONFLICT(recipe_hash, node_id, storage_class) DO UPDATE SET
                    relative_dir = excluded.relative_dir,
                    bytes = excluded.bytes,
                    complete = 1,
                    manifest_digest = excluded.manifest_digest,
                    scrub_object_index = 0,
                    last_seen_at = excluded.last_seen_at
                 WHERE transcode_cache_locations.complete = 0
                    OR (transcode_cache_locations.relative_dir = excluded.relative_dir
                        AND ((?7 IS NULL
                              AND transcode_cache_locations.bytes = excluded.bytes
                              AND (transcode_cache_locations.manifest_digest IS NULL
                                OR transcode_cache_locations.manifest_digest = excluded.manifest_digest))
                          OR (?7 IS NOT NULL
                              AND transcode_cache_locations.bytes = ?7
                              AND transcode_cache_locations.manifest_digest IS NULL
                              AND excluded.bytes >= ?7)))",
                params![
                    recipe_hash,
                    job.owner_node_id,
                    relative_dir,
                    bytes,
                    now_unix_ms / 1000,
                    manifest_digest,
                    expected_previous_bytes,
                ],
            )?;
            if location_changed != 1 {
                // A complete copy appeared after this worker's cache-miss
                // check. Do not settle the job against a different path or
                // manifest; the next claim will validate and reuse that row.
                tx.commit()?;
                return Ok(false);
            }
            let changed = tx.execute(
                "UPDATE pretranscode_jobs
                    SET state = 'ready', recipe_hash = ?6, storage_id = ?2,
                        relative_dir = ?7, manifest_digest = ?8,
                        owner_node_id = NULL, staging_node_id = NULL,
                        lease_expires_ms = NULL, policy_generation = '',
                        requirements_json = '{}', updated_at_ms = ?5
                  WHERE id = ?1 AND state = 'running' AND owner_node_id = ?2
                    AND fence = ?3 AND lease_expires_ms = ?4",
                params![
                    job.id,
                    job.owner_node_id,
                    job.fence,
                    job.lease_expires_ms,
                    now_unix_ms,
                    recipe_hash,
                    relative_dir,
                    manifest_digest,
                ],
            )?;
            if changed != 1 {
                return Err(rusqlite::Error::ExecuteReturnedResults.into());
            }
            tx.commit()?;
            Ok(true)
        })
        .await
    }
}
