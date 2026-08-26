use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row};

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::{
    cluster_fragment_index_key, ClusterFragmentIndexArtifact, ClusterFragmentIndexJob,
    ClusterFragmentIndexLocation, ClusterFragmentIndexStore, FragmentIndexSourceObservation,
    NewClusterFragmentIndexJob,
};

const MAX_ATTEMPTS: i64 = 5;
const MAX_ACTIVE_JOBS: i64 = 4_096;
const MAX_ERROR_CODE_BYTES: usize = 64;
const MAX_LOCAL_EXCLUSIONS: usize = 128;
const CLAIM_SCAN_LIMIT: i64 = 256;

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
                    file_id = excluded.file_id, source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    pipeline_sha256 = excluded.pipeline_sha256,
                    state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                    attempts = CASE
                        WHEN cluster_fragment_index_jobs.state = 'cancelled'
                          OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN 0
                        ELSE cluster_fragment_index_jobs.attempts END,
                    not_before_ms = excluded.not_before_ms,
                    updated_at_ms = excluded.updated_at_ms,
                    last_error_code = NULL
                  WHERE cluster_fragment_index_jobs.state = 'cancelled'
                     OR (cluster_fragment_index_jobs.state = 'failed'
                       AND cluster_fragment_index_jobs.attempts < ?10
                       AND cluster_fragment_index_jobs.not_before_ms <= excluded.created_at_ms)",
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
        excluded_cache_keys: &[String],
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError> {
        if node_id.is_empty()
            || node_id.len() > 128
            || excluded_cache_keys.len() > MAX_LOCAL_EXCLUSIONS
            || excluded_cache_keys.iter().any(|key| !valid_hex_digest(key))
            || lease_expires_ms <= now_ms
        {
            return Err(StoreError::Task(
                "invalid cluster fragment-index claim".to_owned(),
            ));
        }
        let node_id = node_id.to_owned();
        let excluded_cache_keys = excluded_cache_keys
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let candidates = {
                let mut statement = transaction.prepare(&format!(
                    "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs
                          WHERE ((state = 'queued' AND not_before_ms <= ?1)
                              OR (state = 'running' AND lease_expires_ms <= ?1))
                            AND attempts < ?2 AND fence < 9223372036854775807
                          ORDER BY created_at_ms, cache_key LIMIT ?3"
                ))?;
                let rows = statement.query_map(
                    params![now_ms, MAX_ATTEMPTS, CLAIM_SCAN_LIMIT],
                    job_from_row,
                )?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let candidate = candidates
                .into_iter()
                .find(|candidate| !excluded_cache_keys.contains(&candidate.cache_key));
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

    async fn requeue_cluster_fragment_index(
        &self,
        cache_key: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if !valid_hex_digest(cache_key) {
            return Err(StoreError::Task(
                "invalid cluster fragment-index repair key".to_owned(),
            ));
        }
        let cache_key = cache_key.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        not_before_ms = ?1, updated_at_ms = ?1,
                        last_error_code = 'holders_unavailable'
                  WHERE cache_key = ?2
                    AND (state = 'ready' OR (state = 'failed'
                      AND attempts < ?3 AND not_before_ms <= ?1))
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                 WHERE cache_key = ?2)",
                params![now_ms, cache_key, MAX_ATTEMPTS],
            )? == 1)
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
            || artifact.file_id != job.file_id
            || artifact.source_size != job.source_size
            || artifact.source_mtime != job.source_mtime
            || artifact.source_sha256 != job.source_sha256
            || artifact.pipeline_sha256 != job.pipeline_sha256
            || cluster_fragment_index_key(&artifact.source_sha256, &artifact.pipeline_sha256)
                .as_deref()
                != Some(job.cache_key.as_str())
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
                    AND fence = ?3 AND lease_expires_ms > ?4
                    AND file_id = ?5 AND source_size = ?6 AND source_mtime = ?7
                    AND source_sha256 = ?8 AND pipeline_sha256 = ?9)",
                params![
                    job.cache_key,
                    job.owner_node_id,
                    job.fence,
                    now_ms,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    job.source_sha256,
                    job.pipeline_sha256
                ],
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

    async fn prune_cluster_fragment_indexes(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<Vec<String>, StoreError> {
        if !(1..=512).contains(&limit) {
            return Err(StoreError::Task(
                "invalid cluster fragment-index prune bound".to_owned(),
            ));
        }
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let candidates = {
                let mut statement = transaction.prepare(
                    "SELECT j.cache_key
                       FROM cluster_fragment_index_jobs j
                      WHERE j.state IN ('ready', 'failed', 'cancelled')
                        AND j.updated_at_ms < ?1
                        AND NOT EXISTS (
                          SELECT 1 FROM cluster_fragment_index_locations l
                           WHERE l.cache_key = j.cache_key AND l.last_seen_at_ms >= ?1)
                      ORDER BY j.updated_at_ms, j.cache_key LIMIT ?2",
                )?;
                let rows = statement.query_map(params![older_than_ms, limit], |row| row.get(0))?;
                rows.collect::<Result<Vec<String>, _>>()?
            };
            let mut pruned_artifacts = Vec::new();
            for cache_key in candidates {
                transaction.execute(
                    "DELETE FROM cluster_fragment_index_locations
                      WHERE cache_key = ?1
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                          WHERE cache_key = ?1
                            AND state IN ('ready', 'failed', 'cancelled')
                            AND updated_at_ms < ?2)",
                    params![cache_key, older_than_ms],
                )?;
                let artifact = transaction.execute(
                    "DELETE FROM cluster_fragment_index_artifacts
                      WHERE cache_key = ?1
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                          WHERE cache_key = ?1
                            AND state IN ('ready', 'failed', 'cancelled')
                            AND updated_at_ms < ?2)
                        AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                          WHERE cache_key = ?1 AND last_seen_at_ms >= ?2)",
                    params![cache_key, older_than_ms],
                )?;
                transaction.execute(
                    "DELETE FROM cluster_fragment_index_jobs
                      WHERE cache_key = ?1
                        AND state IN ('ready', 'failed', 'cancelled')
                        AND updated_at_ms < ?2
                        AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                         WHERE cache_key = ?1)",
                    params![cache_key, older_than_ms],
                )?;
                if artifact == 1 {
                    pruned_artifacts.push(cache_key);
                }
            }
            transaction.commit()?;
            Ok(pruned_artifacts)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_files(store: &SqliteStore) {
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO libraries (id, name, kind, paths) VALUES (1, 'films', 'movies', '[]')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO items (id, library_id, kind, title, sort_title)
                     VALUES (1, 1, 'movie', 'one', 'one'), (2, 1, 'movie', 'two', 'two')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (1, 1, '/one.mkv', 100, 10), (2, 2, '/two.mkv', 100, 20)",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("seed files");
    }

    fn job(file_id: i64, source_mtime: i64, created_at_ms: i64) -> NewClusterFragmentIndexJob {
        let source_sha256 = "a".repeat(64);
        let pipeline_sha256 = "b".repeat(64);
        NewClusterFragmentIndexJob {
            cache_key: cluster_fragment_index_key(&source_sha256, &pipeline_sha256)
                .expect("valid key"),
            file_id,
            source_size: 100,
            source_mtime,
            source_sha256,
            pipeline_sha256,
            not_before_ms: created_at_ms,
            created_at_ms,
        }
    }

    #[tokio::test]
    async fn duplicate_content_rebinds_a_cancelled_job_to_a_surviving_file() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let original = job(1, 10, 10);
        assert!(store
            .enqueue_cluster_fragment_index(&original)
            .await
            .expect("enqueue original"));
        store
            .with_conn(|conn| {
                conn.execute("DELETE FROM files WHERE id = 1", [])?;
                Ok(())
            })
            .await
            .expect("delete original");

        let replacement = job(2, 20, 20);
        assert!(store
            .enqueue_cluster_fragment_index(&replacement)
            .await
            .expect("rebind replacement"));
        let claimed = store
            .claim_cluster_fragment_index("node-b", &[], 20, 1_020)
            .await
            .expect("claim")
            .expect("replacement claim");
        assert_eq!(claimed.file_id, 2);
        assert_eq!(claimed.source_mtime, 20);
        assert_eq!(claimed.source_sha256, replacement.source_sha256);
    }

    #[tokio::test]
    async fn node_local_exclusions_skip_an_unreadable_head_job() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let first = job(1, 10, 10);
        let mut second = job(2, 20, 20);
        second.source_sha256 = "c".repeat(64);
        second.cache_key =
            cluster_fragment_index_key(&second.source_sha256, &second.pipeline_sha256)
                .expect("second key");
        assert!(store
            .enqueue_cluster_fragment_index(&first)
            .await
            .expect("enqueue first"));
        assert!(store
            .enqueue_cluster_fragment_index(&second)
            .await
            .expect("enqueue second"));

        let claimed = store
            .claim_cluster_fragment_index(
                "node-b",
                std::slice::from_ref(&first.cache_key),
                20,
                1_020,
            )
            .await
            .expect("claim")
            .expect("later runnable job");
        assert_eq!(claimed.cache_key, second.cache_key);
    }

    #[tokio::test]
    async fn retention_prunes_only_terminal_artifacts_without_recent_holders() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let stale = job(1, 10, 10);
        let mut current = job(2, 20, 10);
        current.source_sha256 = "c".repeat(64);
        current.pipeline_sha256 = "d".repeat(64);
        current.cache_key =
            cluster_fragment_index_key(&current.source_sha256, &current.pipeline_sha256)
                .expect("current key");
        let stale_key = stale.cache_key.clone();
        let current_key = current.cache_key.clone();
        let stale_seed = stale.clone();
        let current_seed = current.clone();
        store
            .with_conn(move |conn| {
                for candidate in [&stale_seed, &current_seed] {
                    conn.execute(
                        "INSERT INTO cluster_fragment_index_jobs
                          (cache_key, file_id, source_size, source_mtime, source_sha256,
                           pipeline_sha256, state, fence, attempts, not_before_ms,
                           created_at_ms, updated_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 1, 1, 0, 10, 10)",
                        params![
                            candidate.cache_key,
                            candidate.file_id,
                            candidate.source_size,
                            candidate.source_mtime,
                            candidate.source_sha256,
                            candidate.pipeline_sha256,
                        ],
                    )?;
                    conn.execute(
                        "INSERT INTO cluster_fragment_index_artifacts
                          (cache_key, file_id, source_size, source_mtime, source_sha256,
                           pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                        params![
                            candidate.cache_key,
                            candidate.file_id,
                            candidate.source_size,
                            candidate.source_mtime,
                            candidate.source_sha256,
                            candidate.pipeline_sha256,
                            "e".repeat(64),
                        ],
                    )?;
                }
                conn.execute(
                    "INSERT INTO cluster_fragment_index_locations
                      (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                     VALUES (?1, 'node-a', 10, 20, 20), (?2, 'node-a', 10, 200, 200)",
                    params![stale_key, current_key],
                )?;
                Ok(())
            })
            .await
            .expect("seed artifacts");

        assert_eq!(
            store
                .prune_cluster_fragment_indexes(100, 128)
                .await
                .expect("prune"),
            vec![stale.cache_key.clone()]
        );
        assert!(store
            .cluster_fragment_index_artifact(&stale.cache_key)
            .await
            .expect("stale lookup")
            .is_none());
        assert!(store
            .cluster_fragment_index_artifact(&current.cache_key)
            .await
            .expect("current lookup")
            .is_some());
    }
}
