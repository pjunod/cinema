//! Replicated catalog and fenced queue for content-addressed fragment indexes.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::fragment_index_cluster::CLUSTER_FRAGMENT_INDEX_SCHEMA;
use super::hiqlite::{database_error, timeout_store, validate_sql, HiqliteAuthStore};
use super::{
    cluster_fragment_index_key, AnalysisFileLabel, AnalysisRequest, ClusterFragmentIndexArtifact,
    ClusterFragmentIndexJob, ClusterFragmentIndexLocation, ClusterFragmentIndexStore,
    FragmentIndexSourceObservation, NewAnalysisRequest, NewClusterFragmentIndexJob,
};
use crate::error::StoreError;

const MAX_ATTEMPTS: i64 = 5;
const MAX_ACTIVE_JOBS: i64 = 4_096;
const MAX_ERROR_CODE_BYTES: usize = 64;
const MAX_LOCAL_EXCLUSIONS: usize = MAX_ACTIVE_JOBS as usize;
const CLAIM_SCAN_LIMIT: i64 = MAX_ACTIVE_JOBS;
const QUEUE_ELIGIBILITY_MS: i64 = 6 * 60 * 60 * 1_000;
const MAX_ANALYSIS_REQUESTS: i64 = 4_096;
const MAX_LIST_ROWS: i64 = 500;

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    validate_sql(CLUSTER_FRAGMENT_INDEX_SCHEMA)?;
    for result in timeout_store(client.batch(CLUSTER_FRAGMENT_INDEX_SCHEMA)).await? {
        result.map_err(database_error)?;
    }
    Ok(())
}

const JOB_COLS: &str = "cache_key, file_id, source_size, source_mtime, source_sha256,
    pipeline_sha256, state, COALESCE(owner_node_id, '') AS owner_node_id, fence,
    COALESCE(lease_expires_ms, 0) AS lease_expires_ms, attempts, not_before_ms,
    created_at_ms, updated_at_ms, COALESCE(last_error_code, '') AS last_error_code";

struct JobRow(ClusterFragmentIndexJob);

impl From<&mut Row<'_>> for JobRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(ClusterFragmentIndexJob {
            cache_key: row.get("cache_key"),
            file_id: row.get("file_id"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            source_sha256: row.get("source_sha256"),
            pipeline_sha256: row.get("pipeline_sha256"),
            state: row.get("state"),
            owner_node_id: row.get("owner_node_id"),
            fence: row.get("fence"),
            lease_expires_ms: row.get("lease_expires_ms"),
            attempts: row.get("attempts"),
            not_before_ms: row.get("not_before_ms"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
            last_error_code: row.get("last_error_code"),
        })
    }
}

const REQUEST_COLS: &str = "request_id, file_id, source_size, source_mtime, component,
    force_rebuild, target_node_id, state, COALESCE(owner_node_id, '') AS owner_node_id,
    fence, COALESCE(lease_expires_ms, 0) AS lease_expires_ms, attempts, not_before_ms,
    COALESCE(result_cache_key, '') AS result_cache_key,
    COALESCE(last_error_code, '') AS last_error_code, created_at_ms, updated_at_ms";

struct RequestRow(AnalysisRequest);

impl From<&mut Row<'_>> for RequestRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(AnalysisRequest {
            request_id: row.get("request_id"),
            file_id: row.get("file_id"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            component: row.get("component"),
            force_rebuild: row.get::<i64>("force_rebuild") != 0,
            target_node_id: row.get("target_node_id"),
            state: row.get("state"),
            owner_node_id: row.get("owner_node_id"),
            fence: row.get("fence"),
            lease_expires_ms: row.get("lease_expires_ms"),
            attempts: row.get("attempts"),
            not_before_ms: row.get("not_before_ms"),
            result_cache_key: row.get("result_cache_key"),
            last_error_code: row.get("last_error_code"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        })
    }
}

struct LabelRow(AnalysisFileLabel);

impl From<&mut Row<'_>> for LabelRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(AnalysisFileLabel {
            file_id: row.get("file_id"),
            item_id: row.get("item_id"),
            title: row.get("title"),
        })
    }
}

struct SourceRow(FragmentIndexSourceObservation);

impl From<&mut Row<'_>> for SourceRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(FragmentIndexSourceObservation {
            node_id: row.get("node_id"),
            file_id: row.get("file_id"),
            object_version: row.get("object_version"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            source_sha256: row.get("source_sha256"),
            observed_at_ms: row.get("observed_at_ms"),
        })
    }
}

struct ArtifactRow(ClusterFragmentIndexArtifact);

impl From<&mut Row<'_>> for ArtifactRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(ClusterFragmentIndexArtifact {
            cache_key: row.get("cache_key"),
            file_id: row.get("file_id"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            source_sha256: row.get("source_sha256"),
            pipeline_sha256: row.get("pipeline_sha256"),
            blob_sha256: row.get("blob_sha256"),
            bytes: row.get("bytes"),
            built_by_node_id: row.get("built_by_node_id"),
            built_at_ms: row.get("built_at_ms"),
        })
    }
}

struct LocationRow(ClusterFragmentIndexLocation);

impl From<&mut Row<'_>> for LocationRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(ClusterFragmentIndexLocation {
            cache_key: row.get("cache_key"),
            node_id: row.get("node_id"),
            bytes: row.get("bytes"),
            verified_at_ms: row.get("verified_at_ms"),
            last_seen_at_ms: row.get("last_seen_at_ms"),
        })
    }
}

struct CacheKeyRow(String);

impl From<&mut Row<'_>> for CacheKeyRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("cache_key"))
    }
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

fn valid_request(request: &NewAnalysisRequest) -> bool {
    !request.request_id.is_empty()
        && request.request_id.len() <= 64
        && request.file_id > 0
        && request.source_size >= 0
        && request.component == "fragment_index"
        && !request.target_node_id.is_empty()
        && request.target_node_id.len() <= 128
}

#[async_trait]
impl ClusterFragmentIndexStore for HiqliteAuthStore {
    async fn enqueue_analysis_request(
        &self,
        request: &NewAnalysisRequest,
    ) -> Result<AnalysisRequest, StoreError> {
        if !valid_request(request) {
            return Err(StoreError::Task("invalid analysis request".to_owned()));
        }
        self.execute(
            "INSERT INTO analysis_requests
                (request_id, file_id, source_size, source_mtime, component,
                 force_rebuild, target_node_id, state, owner_node_id, fence,
                 lease_expires_ms, attempts, not_before_ms, result_cache_key,
                 last_error_code, created_at_ms, updated_at_ms)
             SELECT $1, $2, $3, $4, $5, $6, $7, 'queued', NULL, 0,
                    NULL, 0, $8, NULL, NULL, $9, $9
              WHERE EXISTS (SELECT 1 FROM files WHERE id = $2 AND size = $3 AND mtime = $4)
                AND (SELECT COUNT(*) FROM analysis_requests
                      WHERE state IN ('queued', 'running', 'submitted')) < $10
             ON CONFLICT DO NOTHING",
            params!(
                &request.request_id,
                request.file_id,
                request.source_size,
                request.source_mtime,
                &request.component,
                if request.force_rebuild { 1_i64 } else { 0_i64 },
                &request.target_node_id,
                request.not_before_ms,
                request.created_at_ms,
                MAX_ANALYSIS_REQUESTS
            ),
        )
        .await?;
        self.client()
            .query_consistent_map::<RequestRow, _>(
                format!(
                    "SELECT {REQUEST_COLS} FROM analysis_requests
                      WHERE file_id = $1 AND component = $2 AND target_node_id = $3
                        AND state IN ('queued', 'running', 'submitted')
                      ORDER BY created_at_ms, request_id LIMIT 1"
                ),
                params!(request.file_id, &request.component, &request.target_node_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0)
            .ok_or_else(|| {
                StoreError::Task(
                    "analysis source changed or the active request queue is full".to_owned(),
                )
            })
    }

    async fn claim_analysis_request(
        &self,
        node_id: &str,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        if node_id.is_empty() || node_id.len() > 128 || lease_expires_ms <= now_ms {
            return Err(StoreError::Task("invalid analysis claim".to_owned()));
        }
        self.execute(
            "UPDATE analysis_requests
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    last_error_code = 'attempt_limit', updated_at_ms = $1
              WHERE state = 'running' AND attempts >= $2
                AND COALESCE(lease_expires_ms, 0) <= $1",
            params!(now_ms, MAX_ATTEMPTS),
        )
        .await?;
        for _ in 0..8 {
            let candidate = self
                .client()
                .query_consistent_map::<RequestRow, _>(
                    format!(
                        "SELECT {REQUEST_COLS} FROM analysis_requests
                          WHERE target_node_id = $1 AND attempts < $2
                            AND ((state = 'queued' AND not_before_ms <= $3)
                              OR (state = 'running' AND lease_expires_ms <= $3))
                          ORDER BY created_at_ms, request_id LIMIT 1"
                    ),
                    params!(node_id, MAX_ATTEMPTS, now_ms),
                )
                .await?
                .into_iter()
                .next()
                .map(|row| row.0);
            let Some(mut candidate) = candidate else {
                return Ok(None);
            };
            let changed = self
                .execute(
                    "UPDATE analysis_requests
                        SET state = 'running', owner_node_id = $1, fence = fence + 1,
                            lease_expires_ms = $2, attempts = attempts + 1,
                            last_error_code = NULL, updated_at_ms = $3
                      WHERE request_id = $4 AND fence = $5
                        AND ((state = 'queued' AND not_before_ms <= $3)
                          OR (state = 'running' AND lease_expires_ms <= $3))",
                    params!(
                        node_id,
                        lease_expires_ms,
                        now_ms,
                        &candidate.request_id,
                        candidate.fence
                    ),
                )
                .await?;
            if changed == 1 {
                candidate.state = "running".to_owned();
                candidate.owner_node_id = node_id.to_owned();
                candidate.fence += 1;
                candidate.lease_expires_ms = lease_expires_ms;
                candidate.attempts += 1;
                candidate.updated_at_ms = now_ms;
                candidate.last_error_code.clear();
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    async fn renew_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "UPDATE analysis_requests SET lease_expires_ms = $1, updated_at_ms = $2
                  WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2 AND $1 > $2",
                params!(lease_expires_ms, now_ms, request_id, node_id, fence),
            )
            .await?
            == 1)
    }

    async fn submit_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        cache_key: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if !valid_hex_digest(cache_key) {
            return Err(StoreError::Task("invalid analysis result key".to_owned()));
        }
        Ok(self
            .execute(
                "UPDATE analysis_requests
                    SET state = 'submitted', owner_node_id = NULL, lease_expires_ms = NULL,
                        result_cache_key = $1, last_error_code = NULL, updated_at_ms = $2
                  WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2",
                params!(cache_key, now_ms, request_id, node_id, fence),
            )
            .await?
            == 1)
    }

    async fn fail_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        error_code: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if error_code.is_empty() || error_code.len() > MAX_ERROR_CODE_BYTES {
            return Err(StoreError::Task("invalid analysis failure code".to_owned()));
        }
        Ok(self
            .execute(
                "UPDATE analysis_requests
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = $1, updated_at_ms = $2
                  WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2",
                params!(error_code, now_ms, request_id, node_id, fence),
            )
            .await?
            == 1)
    }

    async fn settle_analysis_requests(&self, now_ms: i64) -> Result<u64, StoreError> {
        Ok(self
            .execute(
                "UPDATE analysis_requests
                    SET state = COALESCE((SELECT CASE job.state
                            WHEN 'ready' THEN 'ready' WHEN 'failed' THEN 'failed'
                            WHEN 'cancelled' THEN 'cancelled' ELSE analysis_requests.state END
                          FROM cluster_fragment_index_jobs job
                         WHERE job.cache_key = analysis_requests.result_cache_key), state),
                        last_error_code = (SELECT job.last_error_code
                          FROM cluster_fragment_index_jobs job
                         WHERE job.cache_key = analysis_requests.result_cache_key),
                        updated_at_ms = $1
                  WHERE state = 'submitted'
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs job
                         WHERE job.cache_key = analysis_requests.result_cache_key
                           AND job.state IN ('ready', 'failed', 'cancelled'))",
                params!(now_ms),
            )
            .await?)
    }

    async fn analysis_requests(&self, limit: i64) -> Result<Vec<AnalysisRequest>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        Ok(self
            .client()
            .query_consistent_map::<RequestRow, _>(
                format!(
                    "SELECT {REQUEST_COLS} FROM analysis_requests
                      ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1
                        WHEN 'submitted' THEN 2 WHEN 'failed' THEN 3 ELSE 4 END,
                        updated_at_ms DESC, request_id LIMIT $1"
                ),
                params!(limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn analysis_file_labels(&self, limit: i64) -> Result<Vec<AnalysisFileLabel>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        Ok(self
            .client()
            .query_consistent_map::<LabelRow, _>(
                "SELECT files.id AS file_id, files.item_id AS item_id, items.title AS title
                   FROM files JOIN items ON items.id = files.item_id
                  WHERE files.id IN (
                    SELECT file_id FROM analysis_requests
                    UNION SELECT file_id FROM cluster_fragment_index_jobs)
                  ORDER BY files.id LIMIT $1",
                params!(limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn cluster_fragment_index_jobs(
        &self,
        limit: i64,
    ) -> Result<Vec<ClusterFragmentIndexJob>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        Ok(self
            .client()
            .query_consistent_map::<JobRow, _>(
                format!(
                    "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs
                      ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1
                        WHEN 'failed' THEN 2 ELSE 3 END, updated_at_ms DESC, cache_key
                      LIMIT $1"
                ),
                params!(limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn cluster_fragment_index_job(
        &self,
        cache_key: &str,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<JobRow, _>(
                format!("SELECT {JOB_COLS} FROM cluster_fragment_index_jobs WHERE cache_key = $1"),
                params!(cache_key),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

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
        self.execute(
            "INSERT INTO cluster_fragment_index_sources
                (node_id, file_id, object_version, source_size, source_mtime,
                 source_sha256, observed_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT(node_id, file_id) DO UPDATE SET
                object_version = excluded.object_version,
                source_size = excluded.source_size,
                source_mtime = excluded.source_mtime,
                source_sha256 = excluded.source_sha256,
                observed_at_ms = excluded.observed_at_ms",
            params!(
                &observation.node_id,
                observation.file_id,
                &observation.object_version,
                observation.source_size,
                observation.source_mtime,
                &observation.source_sha256,
                observation.observed_at_ms
            ),
        )
        .await?;
        Ok(())
    }

    async fn fragment_index_source(
        &self,
        node_id: &str,
        file_id: i64,
        object_version: &str,
    ) -> Result<Option<FragmentIndexSourceObservation>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<SourceRow, _>(
                "SELECT node_id, file_id, object_version, source_size, source_mtime,
                        source_sha256, observed_at_ms
                   FROM cluster_fragment_index_sources
                  WHERE node_id = $1 AND file_id = $2 AND object_version = $3",
                params!(node_id, file_id, object_version),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn cluster_fragment_index_artifact(
        &self,
        cache_key: &str,
    ) -> Result<Option<ClusterFragmentIndexArtifact>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<ArtifactRow, _>(
                "SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
                        pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms
                   FROM cluster_fragment_index_artifacts WHERE cache_key = $1",
                params!(cache_key),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
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
        let eligibility_cutoff = job.created_at_ms.saturating_sub(QUEUE_ELIGIBILITY_MS);
        self.execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = $1, updated_at_ms = $1,
                    last_error_code = 'queue_expired'
              WHERE (state = 'queued' AND created_at_ms < $2)
                 OR (state = 'running' AND attempts >= $3
                   AND COALESCE(lease_expires_ms, 0) <= $1)",
            params!(job.created_at_ms, eligibility_cutoff, MAX_ATTEMPTS),
        )
        .await?;
        Ok(self
            .execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, state, owner_node_id, fence, lease_expires_ms,
                     attempts, not_before_ms, created_at_ms, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, 'queued', NULL, 0, NULL, 0, $7, $8, $8
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = $2 AND size = $3 AND mtime = $4)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = $1)
                    AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < $9
                 ON CONFLICT(cache_key) DO UPDATE SET
                    file_id = excluded.file_id, source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    pipeline_sha256 = excluded.pipeline_sha256,
                    state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                    attempts = CASE
                        WHEN cluster_fragment_index_jobs.state = 'cancelled'
                          OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                          OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN 0
                        ELSE cluster_fragment_index_jobs.attempts END,
                    not_before_ms = excluded.not_before_ms,
                    created_at_ms = CASE
                        WHEN cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                        THEN excluded.created_at_ms
                        ELSE cluster_fragment_index_jobs.created_at_ms END,
                    updated_at_ms = excluded.updated_at_ms,
                    last_error_code = NULL
                  WHERE cluster_fragment_index_jobs.state = 'cancelled'
                     OR (cluster_fragment_index_jobs.state = 'failed'
                       AND cluster_fragment_index_jobs.last_error_code = 'queue_expired')
                     OR (cluster_fragment_index_jobs.state = 'failed'
                       AND cluster_fragment_index_jobs.attempts < $10
                       AND cluster_fragment_index_jobs.not_before_ms <= excluded.created_at_ms)",
                params!(
                    &job.cache_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    &job.source_sha256,
                    &job.pipeline_sha256,
                    job.not_before_ms,
                    job.created_at_ms,
                    MAX_ACTIVE_JOBS,
                    MAX_ATTEMPTS
                ),
            )
            .await?
            == 1)
    }

    async fn force_cluster_fragment_index(
        &self,
        job: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError> {
        if !valid_job(job) {
            return Err(StoreError::Task(
                "invalid forced fragment-index job".to_owned(),
            ));
        }
        Ok(self
            .execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, state, owner_node_id, fence, lease_expires_ms,
                     attempts, not_before_ms, created_at_ms, updated_at_ms, last_error_code)
                 SELECT $1, $2, $3, $4, $5, $6, 'queued', NULL, 0, NULL, 0, $7, $8, $8, NULL
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = $2 AND size = $3 AND mtime = $4)
                    AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < $9
                 ON CONFLICT(cache_key) DO UPDATE SET
                    file_id = excluded.file_id, source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    pipeline_sha256 = excluded.pipeline_sha256,
                    state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                    attempts = 0, not_before_ms = excluded.not_before_ms,
                    created_at_ms = excluded.created_at_ms,
                    updated_at_ms = excluded.updated_at_ms, last_error_code = NULL
                  WHERE cluster_fragment_index_jobs.state IN ('ready', 'failed', 'cancelled')",
                params!(
                    &job.cache_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    &job.source_sha256,
                    &job.pipeline_sha256,
                    job.not_before_ms,
                    job.created_at_ms,
                    MAX_ACTIVE_JOBS
                ),
            )
            .await?
            == 1)
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
        let excluded_cache_keys = excluded_cache_keys
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        self.execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = $1, updated_at_ms = $1,
                    last_error_code = 'queue_expired'
              WHERE (state = 'queued' AND created_at_ms < $2)
                 OR (state = 'running' AND attempts >= $3
                   AND COALESCE(lease_expires_ms, 0) <= $1)",
            params!(
                now_ms,
                now_ms.saturating_sub(QUEUE_ELIGIBILITY_MS),
                MAX_ATTEMPTS
            ),
        )
        .await?;
        for _ in 0..8 {
            let sql = format!(
                "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs
                  WHERE ((state = 'queued' AND not_before_ms <= $1)
                      OR (state = 'running' AND lease_expires_ms <= $1))
                    AND attempts < $2 AND fence < 9223372036854775807
                  ORDER BY created_at_ms, cache_key LIMIT $3"
            );
            let candidates = self
                .client()
                .query_consistent_map::<JobRow, _>(
                    sql,
                    params!(now_ms, MAX_ATTEMPTS, CLAIM_SCAN_LIMIT),
                )
                .await?
                .into_iter()
                .map(|row| row.0)
                .collect::<Vec<_>>();
            let Some(mut candidate) = candidates
                .into_iter()
                .find(|candidate| !excluded_cache_keys.contains(&candidate.cache_key))
            else {
                return Ok(None);
            };
            let changed = self
                .execute(
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'running', owner_node_id = $1, fence = fence + 1,
                            lease_expires_ms = $2, attempts = attempts + 1,
                            last_error_code = NULL, updated_at_ms = $3
                      WHERE cache_key = $4 AND fence = $5
                        AND ((state = 'queued' AND not_before_ms <= $3)
                          OR (state = 'running' AND lease_expires_ms <= $3))",
                    params!(
                        node_id,
                        lease_expires_ms,
                        now_ms,
                        &candidate.cache_key,
                        candidate.fence
                    ),
                )
                .await?;
            if changed == 1 {
                candidate.state = "running".to_owned();
                candidate.owner_node_id = node_id.to_owned();
                candidate.fence += 1;
                candidate.lease_expires_ms = lease_expires_ms;
                candidate.attempts += 1;
                candidate.updated_at_ms = now_ms;
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    async fn requeue_cluster_fragment_index(
        &self,
        replacement: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError> {
        if !valid_job(replacement) {
            return Err(StoreError::Task(
                "invalid cluster fragment-index repair job".to_owned(),
            ));
        }
        let eligibility_cutoff = replacement
            .created_at_ms
            .saturating_sub(QUEUE_ELIGIBILITY_MS);
        self.execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = $1, updated_at_ms = $1,
                    last_error_code = 'queue_expired'
              WHERE (state = 'queued' AND created_at_ms < $2)
                 OR (state = 'running' AND attempts >= $3
                   AND COALESCE(lease_expires_ms, 0) <= $1)",
            params!(replacement.created_at_ms, eligibility_cutoff, MAX_ATTEMPTS),
        )
        .await?;
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET file_id = $2, source_size = $3, source_mtime = $4,
                        source_sha256 = $5, pipeline_sha256 = $6,
                        state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = CASE
                          WHEN state = 'ready' OR last_error_code = 'queue_expired'
                            OR file_id <> $2 THEN 0
                          ELSE attempts END,
                        not_before_ms = $7, created_at_ms = $8, updated_at_ms = $8,
                        last_error_code = 'holders_unavailable'
                  WHERE cache_key = $1
                    AND (state = 'ready'
                      OR (state = 'failed' AND last_error_code = 'queue_expired')
                      OR (state = 'failed'
                      AND attempts < $9 AND not_before_ms <= $8))
                    AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < $10
                    AND EXISTS (SELECT 1 FROM files
                      WHERE id = $2 AND size = $3 AND mtime = $4)
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                      WHERE cache_key = $1 AND source_size = $3
                        AND source_sha256 = $5 AND pipeline_sha256 = $6)",
                params!(
                    &replacement.cache_key,
                    replacement.file_id,
                    replacement.source_size,
                    replacement.source_mtime,
                    &replacement.source_sha256,
                    &replacement.pipeline_sha256,
                    replacement.not_before_ms,
                    replacement.created_at_ms,
                    MAX_ATTEMPTS,
                    MAX_ACTIVE_JOBS
                ),
            )
            .await?
            == 1)
    }

    async fn renew_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET lease_expires_ms = $1, updated_at_ms = $2
                  WHERE cache_key = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2 AND $1 > $2",
                params!(lease_expires_ms, now_ms, cache_key, node_id, fence),
            )
            .await?
            == 1)
    }

    async fn yield_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = MAX(attempts - 1, 0), not_before_ms = $1,
                        last_error_code = 'node_local_refusal', updated_at_ms = $2
                  WHERE cache_key = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2",
                params!(retry_at_ms, now_ms, cache_key, node_id, fence),
            )
            .await?
            == 1)
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
        let results = self
            .client()
            .txn(vec![
                (
                    "INSERT INTO cluster_fragment_index_artifacts
                        (cache_key, file_id, source_size, source_mtime, source_sha256,
                         pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
                      WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                        WHERE cache_key = $1 AND state = 'running' AND owner_node_id = $9
                          AND fence = $11 AND lease_expires_ms > $12
                          AND file_id = $2 AND source_size = $3 AND source_mtime = $4
                          AND source_sha256 = $5 AND pipeline_sha256 = $6)
                     ON CONFLICT(cache_key) DO NOTHING"
                        .to_owned(),
                    params!(
                        &artifact.cache_key,
                        artifact.file_id,
                        artifact.source_size,
                        artifact.source_mtime,
                        &artifact.source_sha256,
                        &artifact.pipeline_sha256,
                        &artifact.blob_sha256,
                        artifact.bytes,
                        &artifact.built_by_node_id,
                        artifact.built_at_ms,
                        job.fence,
                        now_ms
                    ),
                ),
                (
                    "INSERT INTO cluster_fragment_index_locations
                        (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                     SELECT $1, $2, $3, $4, $5
                      WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                        WHERE cache_key = $1 AND state = 'running' AND owner_node_id = $2
                          AND fence = $6 AND lease_expires_ms > $7)
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                          WHERE cache_key = $1 AND source_sha256 = $8
                            AND pipeline_sha256 = $9 AND blob_sha256 = $10 AND bytes = $3)
                     ON CONFLICT(cache_key, node_id) DO UPDATE SET
                        bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                        last_seen_at_ms = excluded.last_seen_at_ms"
                        .to_owned(),
                    params!(
                        &location.cache_key,
                        &location.node_id,
                        location.bytes,
                        location.verified_at_ms,
                        location.last_seen_at_ms,
                        job.fence,
                        now_ms,
                        &artifact.source_sha256,
                        &artifact.pipeline_sha256,
                        &artifact.blob_sha256
                    ),
                ),
                (
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                            last_error_code = NULL, updated_at_ms = $1
                      WHERE cache_key = $2 AND state = 'running' AND owner_node_id = $3
                        AND fence = $4 AND lease_expires_ms > $1
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                          WHERE cache_key = $2 AND source_sha256 = $5
                            AND pipeline_sha256 = $6 AND blob_sha256 = $7 AND bytes = $8)"
                        .to_owned(),
                    params!(
                        now_ms,
                        &job.cache_key,
                        &job.owner_node_id,
                        job.fence,
                        &artifact.source_sha256,
                        &artifact.pipeline_sha256,
                        &artifact.blob_sha256,
                        artifact.bytes
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.last().copied() == Some(1))
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
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = $1, not_before_ms = $2, updated_at_ms = $3
                  WHERE cache_key = $4 AND state = 'running' AND owner_node_id = $5
                    AND fence = $6 AND lease_expires_ms > $3",
                params!(error_code, retry_at_ms, now_ms, cache_key, node_id, fence),
            )
            .await?
            == 1)
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
        self.execute(
            "INSERT INTO cluster_fragment_index_locations
                (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
             SELECT $1, $2, $3, $4, $5
              WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                             WHERE cache_key = $1 AND bytes = $3)
             ON CONFLICT(cache_key, node_id) DO UPDATE SET
                bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                last_seen_at_ms = excluded.last_seen_at_ms",
            params!(
                &location.cache_key,
                &location.node_id,
                location.bytes,
                location.verified_at_ms,
                location.last_seen_at_ms
            ),
        )
        .await?;
        Ok(())
    }

    async fn cluster_fragment_index_locations(
        &self,
        cache_key: &str,
    ) -> Result<Vec<ClusterFragmentIndexLocation>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<LocationRow, _>(
                "SELECT cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms
                   FROM cluster_fragment_index_locations
                  WHERE cache_key = $1 ORDER BY last_seen_at_ms DESC, node_id LIMIT 128",
                params!(cache_key),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn forget_cluster_fragment_index_location(
        &self,
        cache_key: &str,
        node_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "DELETE FROM cluster_fragment_index_locations
                  WHERE cache_key = $1 AND node_id = $2",
                params!(cache_key, node_id),
            )
            .await?
            == 1)
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
        let candidates = self
            .client()
            .query_consistent_map::<CacheKeyRow, _>(
                "SELECT j.cache_key AS cache_key
                   FROM cluster_fragment_index_jobs j
                  WHERE j.state IN ('ready', 'failed', 'cancelled')
                    AND j.updated_at_ms < $1
                    AND NOT EXISTS (
                      SELECT 1 FROM cluster_fragment_index_locations l
                       WHERE l.cache_key = j.cache_key AND l.last_seen_at_ms >= $1)
                  ORDER BY j.updated_at_ms, j.cache_key LIMIT $2",
                params!(older_than_ms, limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let mut statements = Vec::with_capacity(candidates.len() * 3);
        for cache_key in &candidates {
            statements.push((
                "DELETE FROM cluster_fragment_index_artifacts
                  WHERE cache_key = $1
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                      WHERE cache_key = $1
                        AND state IN ('ready', 'failed', 'cancelled')
                        AND updated_at_ms < $2)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                      WHERE cache_key = $1 AND last_seen_at_ms >= $2)"
                    .to_owned(),
                params!(cache_key, older_than_ms),
            ));
            statements.push((
                "DELETE FROM cluster_fragment_index_locations
                  WHERE cache_key = $1 AND last_seen_at_ms < $2
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = $1)"
                    .to_owned(),
                params!(cache_key, older_than_ms),
            ));
            statements.push((
                "DELETE FROM cluster_fragment_index_jobs
                  WHERE cache_key = $1
                    AND state IN ('ready', 'failed', 'cancelled')
                    AND updated_at_ms < $2
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = $1)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                      WHERE cache_key = $1 AND last_seen_at_ms >= $2)"
                    .to_owned(),
                params!(cache_key, older_than_ms),
            ));
        }
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(candidates
            .into_iter()
            .zip(results.chunks_exact(3))
            .filter_map(|(key, result)| (result[0] == 1).then_some(key))
            .collect())
    }
}
