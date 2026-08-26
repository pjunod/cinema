use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row};

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::{
    ClusterFragmentIndexArtifact, ClusterFragmentIndexJob, ClusterFragmentIndexLocation,
    ClusterFragmentIndexStore, FragmentIndexSourceObservation, NewClusterFragmentIndexJob,
};

const MAX_ATTEMPTS: i64 = 5;
const MAX_ACTIVE_JOBS: i64 = 4_096;
const MAX_ERROR_CODE_BYTES: usize = 64;

const JOB_COLS: &str = "cache_key, file_id, source_size, source_mtime, source_sha256,
    pipeline_sha256, state, COALESCE(owner_node_id, ''), fence,
    COALESCE(lease_expires_ms, 0), attempts, not_before_ms, created_at_ms, updated_at_ms";

fn job_from_row(row: &Row<'_>) -> rusqlite::Result<ClusterFragmentIndexJob> {
    Ok(ClusterFragmentIndexJob {
        cache_key: row.get(0)?,
        file_id: row.get(1)?,
        source_size: row.get(2)?,
        source_mtime: row.get(3)?,
        source_sha256: row.get(4)?,
        pipeline_sha256: row.get(5)?,
        state: row.get(6)?,
        owner_node_id: row.get(7)?,
        fence: row.get(8)?,
        lease_expires_ms: row.get(9)?,
        attempts: row.get(10)?,
        not_before_ms: row.get(11)?,
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
    })
}

fn artifact_from_row(row: &Row<'_>) -> rusqlite::Result<ClusterFragmentIndexArtifact> {
    Ok(ClusterFragmentIndexArtifact {
        cache_key: row.get(0)?,
        file_id: row.get(1)?,
        source_size: row.get(2)?,
        source_mtime: row.get(3)?,
        source_sha256: row.get(4)?,
        pipeline_sha256: row.get(5)?,
        blob_sha256: row.get(6)?,
        bytes: row.get(7)?,
        built_by_node_id: row.get(8)?,
        built_at_ms: row.get(9)?,
    })
}

fn valid_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_job(job: &NewClusterFragmentIndexJob) -> bool {
    job.cache_key.len() == 64
        && valid_hex_digest(&job.cache_key)
        && job.file_id > 0
        && job.source_size >= 0
        && valid_hex_digest(&job.source_sha256)
        && valid_hex_digest(&job.pipeline_sha256)
}

#[async_trait]
impl ClusterFragmentIndexStore for SqliteStore {
    async fn record_fragment_index_source(
        &self,
        observation: &FragmentIndexSourceObservation,
    ) -> Result<(), StoreError> {
        if observation.node_id.is_empty()
            || observation.node_id.len() > 128
            || observation.file_id <= 0
            || observation.object_version.is_empty()
            || observation.object_version.len() > 512
            || observation.source_size < 0
            || !valid_hex_digest(&observation.source_sha256)
        {
            return Err(StoreError::Task(
                "invalid fragment-index source observation".to_owned(),
            ));
        }
        let observation = observation.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_sources
                    (node_id, file_id, object_version, source_size, source_mtime,
                     source_sha256, observed_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(node_id, file_id) DO UPDATE SET
                    object_version = excluded.object_version,
                    source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    observed_at_ms = excluded.observed_at_ms",
                params![
                    observation.node_id,
                    observation.file_id,
                    observation.object_version,
                    observation.source_size,
                    observation.source_mtime,
                    observation.source_sha256,
                    observation.observed_at_ms,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn fragment_index_source(
        &self,
        node_id: &str,
        file_id: i64,
        object_version: &str,
    ) -> Result<Option<FragmentIndexSourceObservation>, StoreError> {
        let node_id = node_id.to_owned();
        let object_version = object_version.to_owned();
        self.with_read(move |conn| {
            conn.query_row(
                "SELECT node_id, file_id, object_version, source_size, source_mtime,
                        source_sha256, observed_at_ms
                   FROM cluster_fragment_index_sources
                  WHERE node_id = ?1 AND file_id = ?2 AND object_version = ?3",
                params![node_id, file_id, object_version],
                |row| {
                    Ok(FragmentIndexSourceObservation {
                        node_id: row.get(0)?,
                        file_id: row.get(1)?,
                        object_version: row.get(2)?,
                        source_size: row.get(3)?,
                        source_mtime: row.get(4)?,
                        source_sha256: row.get(5)?,
                        observed_at_ms: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
        })
        .await
    }

    async fn cluster_fragment_index_artifact(
        &self,
        cache_key: &str,
    ) -> Result<Option<ClusterFragmentIndexArtifact>, StoreError> {
        let cache_key = cache_key.to_owned();
        self.with_read(move |conn| {
            conn.query_row(
                "SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
                        pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms
                   FROM cluster_fragment_index_artifacts WHERE cache_key = ?1",
                params![cache_key],
                artifact_from_row,
            )
            .optional()
            .map_err(Into::into)
        })
        .await
    }

    async fn enqueue_cluster_fragment_index(
        &self,
        job: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError> {
        if !valid_job(job) {
            return Err(StoreError::Task(
                "invalid cluster fragment-index job".to_owned(),
            ));
        }
        let job = job.clone();
        self.with_conn(move |conn| {
            let changed = conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, state, owner_node_id, fence, lease_expires_ms,
                     attempts, not_before_ms, created_at_ms, updated_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, 'queued', NULL, 0, NULL, 0, ?7, ?8, ?8
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = ?2 AND size = ?3 AND mtime = ?4)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = ?1)
                    AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < ?9
                 ON CONFLICT(cache_key) DO UPDATE SET
                    state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = excluded.not_before_ms,
                    updated_at_ms = excluded.updated_at_ms,
                    last_error_code = NULL
                  WHERE cluster_fragment_index_jobs.state = 'failed'
                    AND cluster_fragment_index_jobs.attempts < ?10
                    AND cluster_fragment_index_jobs.not_before_ms <= excluded.created_at_ms",
                params![
                    job.cache_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    job.source_sha256,
                    job.pipeline_sha256,
                    job.not_before_ms,
                    job.created_at_ms,
                    MAX_ACTIVE_JOBS,
                    MAX_ATTEMPTS,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    async fn claim_cluster_fragment_index(
        &self,
        node_id: &str,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError> {
        if node_id.is_empty() || node_id.len() > 128 || lease_expires_ms <= now_ms {
            return Err(StoreError::Task(
                "invalid cluster fragment-index claim".to_owned(),
            ));
        }
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let candidate = transaction
                .query_row(
                    &format!(
                        "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs
                          WHERE ((state = 'queued' AND not_before_ms <= ?1)
                              OR (state = 'running' AND lease_expires_ms <= ?1))
                            AND attempts < ?2 AND fence < 9223372036854775807
                          ORDER BY created_at_ms, cache_key LIMIT 1"
                    ),
                    params![now_ms, MAX_ATTEMPTS],
                    job_from_row,
                )
                .optional()?;
            let Some(mut candidate) = candidate else {
                transaction.commit()?;
                return Ok(None);
            };
            let changed = transaction.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'running', owner_node_id = ?1, fence = fence + 1,
                        lease_expires_ms = ?2, attempts = attempts + 1,
                        last_error_code = NULL, updated_at_ms = ?3
                  WHERE cache_key = ?4 AND fence = ?5
                    AND ((state = 'queued' AND not_before_ms <= ?3)
                      OR (state = 'running' AND lease_expires_ms <= ?3))",
                params![
                    node_id,
                    lease_expires_ms,
                    now_ms,
                    candidate.cache_key,
                    candidate.fence
                ],
            )?;
            if changed != 1 {
                transaction.commit()?;
                return Ok(None);
            }
            candidate.state = "running".to_owned();
            candidate.owner_node_id = node_id;
            candidate.fence += 1;
            candidate.lease_expires_ms = lease_expires_ms;
            candidate.attempts += 1;
            candidate.updated_at_ms = now_ms;
            transaction.commit()?;
            Ok(Some(candidate))
        })
        .await
    }

    async fn renew_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError> {
        let cache_key = cache_key.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET lease_expires_ms = ?1, updated_at_ms = ?2
                  WHERE cache_key = ?3 AND state = 'running' AND owner_node_id = ?4
                    AND fence = ?5 AND lease_expires_ms > ?2 AND ?1 > ?2",
                params![lease_expires_ms, now_ms, cache_key, node_id, fence],
            )? == 1)
        })
        .await
    }

    async fn yield_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let cache_key = cache_key.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = MAX(attempts - 1, 0), not_before_ms = ?1,
                        last_error_code = 'node_local_refusal', updated_at_ms = ?2
                  WHERE cache_key = ?3 AND state = 'running' AND owner_node_id = ?4
                    AND fence = ?5 AND lease_expires_ms > ?2",
                params![retry_at_ms, now_ms, cache_key, node_id, fence],
            )? == 1)
        })
        .await
    }

    async fn complete_cluster_fragment_index(
        &self,
        job: &ClusterFragmentIndexJob,
        artifact: &ClusterFragmentIndexArtifact,
        location: &ClusterFragmentIndexLocation,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if artifact.cache_key != job.cache_key
            || location.cache_key != job.cache_key
            || artifact.built_by_node_id != job.owner_node_id
            || location.node_id != job.owner_node_id
            || artifact.bytes <= 0
            || location.bytes != artifact.bytes
            || !valid_hex_digest(&artifact.blob_sha256)
        {
            return Err(StoreError::Task(
                "invalid cluster fragment-index completion".to_owned(),
            ));
        }
        let job = job.clone();
        let artifact = artifact.clone();
        let location = location.clone();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let current: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_fragment_index_jobs
                  WHERE cache_key = ?1 AND state = 'running' AND owner_node_id = ?2
                    AND fence = ?3 AND lease_expires_ms > ?4)",
                params![job.cache_key, job.owner_node_id, job.fence, now_ms],
                |row| row.get(0),
            )?;
            if !current {
                transaction.commit()?;
                return Ok(false);
            }
            transaction.execute(
                "INSERT INTO cluster_fragment_index_artifacts
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(cache_key) DO NOTHING",
                params![
                    artifact.cache_key,
                    artifact.file_id,
                    artifact.source_size,
                    artifact.source_mtime,
                    artifact.source_sha256,
                    artifact.pipeline_sha256,
                    artifact.blob_sha256,
                    artifact.bytes,
                    artifact.built_by_node_id,
                    artifact.built_at_ms,
                ],
            )?;
            let matching: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_fragment_index_artifacts
                  WHERE cache_key = ?1 AND source_sha256 = ?2 AND pipeline_sha256 = ?3
                    AND blob_sha256 = ?4 AND bytes = ?5)",
                params![
                    artifact.cache_key,
                    artifact.source_sha256,
                    artifact.pipeline_sha256,
                    artifact.blob_sha256,
                    artifact.bytes,
                ],
                |row| row.get(0),
            )?;
            if !matching {
                return Err(StoreError::Database(
                    "fragment-index key resolved to conflicting artifacts".to_owned(),
                ));
            }
            transaction.execute(
                "INSERT INTO cluster_fragment_index_locations
                    (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(cache_key, node_id) DO UPDATE SET
                    bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                    last_seen_at_ms = excluded.last_seen_at_ms",
                params![
                    location.cache_key,
                    location.node_id,
                    location.bytes,
                    location.verified_at_ms,
                    location.last_seen_at_ms,
                ],
            )?;
            let changed = transaction.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = NULL, updated_at_ms = ?1
                  WHERE cache_key = ?2 AND state = 'running' AND owner_node_id = ?3
                    AND fence = ?4 AND lease_expires_ms > ?1",
                params![now_ms, job.cache_key, job.owner_node_id, job.fence],
            )?;
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
    }

    async fn fail_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        error_code: &str,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError> {
        if error_code.is_empty() || error_code.len() > MAX_ERROR_CODE_BYTES {
            return Err(StoreError::Task(
                "invalid fragment-index failure code".to_owned(),
            ));
        }
        let cache_key = cache_key.to_owned();
        let node_id = node_id.to_owned();
        let error_code = error_code.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = ?1, not_before_ms = ?2, updated_at_ms = ?3
                  WHERE cache_key = ?4 AND state = 'running' AND owner_node_id = ?5
                    AND fence = ?6 AND lease_expires_ms > ?3",
                params![error_code, retry_at_ms, now_ms, cache_key, node_id, fence],
            )? == 1)
        })
        .await
    }

    async fn put_cluster_fragment_index_location(
        &self,
        location: &ClusterFragmentIndexLocation,
    ) -> Result<(), StoreError> {
        if location.cache_key.len() != 64
            || location.node_id.is_empty()
            || location.node_id.len() > 128
            || location.bytes <= 0
        {
            return Err(StoreError::Task(
                "invalid fragment-index location".to_owned(),
            ));
        }
        let location = location.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_locations
                    (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5
                  WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                 WHERE cache_key = ?1 AND bytes = ?3)
                 ON CONFLICT(cache_key, node_id) DO UPDATE SET
                    bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                    last_seen_at_ms = excluded.last_seen_at_ms",
                params![
                    location.cache_key,
                    location.node_id,
                    location.bytes,
                    location.verified_at_ms,
                    location.last_seen_at_ms,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn cluster_fragment_index_locations(
        &self,
        cache_key: &str,
    ) -> Result<Vec<ClusterFragmentIndexLocation>, StoreError> {
        let cache_key = cache_key.to_owned();
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms
                   FROM cluster_fragment_index_locations
                  WHERE cache_key = ?1 ORDER BY last_seen_at_ms DESC, node_id LIMIT 128",
            )?;
            let rows = statement.query_map(params![cache_key], |row| {
                Ok(ClusterFragmentIndexLocation {
                    cache_key: row.get(0)?,
                    node_id: row.get(1)?,
                    bytes: row.get(2)?,
                    verified_at_ms: row.get(3)?,
                    last_seen_at_ms: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn forget_cluster_fragment_index_location(
        &self,
        cache_key: &str,
        node_id: &str,
    ) -> Result<bool, StoreError> {
        let cache_key = cache_key.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM cluster_fragment_index_locations
                  WHERE cache_key = ?1 AND node_id = ?2",
                params![cache_key, node_id],
            )? == 1)
        })
        .await
    }
}
