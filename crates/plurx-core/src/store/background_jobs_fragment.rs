//! Fenced fragment publication. A completed build satisfies local interests;
//! remote interests remain durable delivery obligations until hydration.

use super::background_jobs::JobToken;
use super::ClusterFragmentIndexArtifact;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishFragmentJob {
    pub token: JobToken,
    pub artifact: ClusterFragmentIndexArtifact,
    pub now_ms: i64,
}

#[derive(Debug, Clone)]
pub struct FragmentJobFailure {
    pub token: JobToken,
    pub code: crate::content_analysis::IndexFailureCode,
    pub transient_allowlisted: bool,
    pub diagnostic: crate::content_analysis::IndexDiagnostic,
    pub now_ms: i64,
}

pub(super) const PUBLISH_FRAGMENT_SQL: &str = r#"
WITH request AS (SELECT json($1) AS body), snapshot AS (
  SELECT body, job.id, job.state, job.result_ref,
    json_object('artifact_kind', 'fragment_index', 'artifact_key', json_extract(body, '$.artifact.cache_key'),
      'node_id', json_extract(body, '$.token.node_id'), 'blob_sha256', json_extract(body, '$.artifact.blob_sha256'),
      'bytes', json_extract(body, '$.artifact.bytes'), 'publication_digest', json_extract(body, '$.publication_digest')) AS output,
    CASE WHEN job.state = 'running' AND job.payload_version = 1
      AND job.owner_node_id = json_extract(body, '$.token.node_id')
      AND job.owner_boot_id = json_extract(body, '$.token.boot_id')
      AND job.claim_id = json_extract(body, '$.token.claim_id') AND job.fence = json_extract(body, '$.token.fence')
      AND job.revision = json_extract(body, '$.token.revision')
      AND job.lease_expires_ms = json_extract(body, '$.token.lease_expires_ms')
      AND (job.retry_deadline_ms = 0 OR job.retry_deadline_ms > json_extract(body, '$.now_ms'))
      AND job.lease_expires_ms > json_extract(body, '$.now_ms') AND job.revision < 9223372036854775807
      AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || job.owner_node_id)
      AND ((job.kind = 'fragment_index_build'
        AND json_extract(job.payload_json, '$.cache_key') = json_extract(body, '$.artifact.cache_key')
        AND json_extract(job.payload_json, '$.file_id') = json_extract(body, '$.artifact.file_id')
        AND json_extract(job.payload_json, '$.source_size') = json_extract(body, '$.artifact.source_size')
        AND json_extract(job.payload_json, '$.source_mtime') = json_extract(body, '$.artifact.source_mtime')
        AND json_extract(job.payload_json, '$.source_sha256') = json_extract(body, '$.artifact.source_sha256')
        AND json_extract(job.payload_json, '$.pipeline_digest') = json_extract(body, '$.artifact.pipeline_sha256')
        AND json_extract(body, '$.artifact.built_by_node_id') = job.owner_node_id)
      OR (job.kind = 'artifact_hydrate'
        AND json_extract(job.payload_json, '$.artifact_key') = 'fragment:' || json_extract(body, '$.artifact.cache_key')
        AND job.target_node_id = job.owner_node_id AND artifact.cache_key IS NOT NULL
        AND artifact.built_by_node_id = json_extract(body, '$.artifact.built_by_node_id')
        AND artifact.built_at_ms = json_extract(body, '$.artifact.built_at_ms')))
      THEN 1 ELSE 0 END AS owns,
    CASE WHEN EXISTS (SELECT 1 FROM background_job_attempts attempt WHERE attempt.job_id = job.id
      AND attempt.fence = job.fence AND attempt.fence = json_extract(body, '$.token.fence')
      AND attempt.claim_id = json_extract(body, '$.token.claim_id')
      AND attempt.owner_node_id = json_extract(body, '$.token.node_id')
      AND attempt.owner_boot_id = json_extract(body, '$.token.boot_id')) THEN 1 ELSE 0 END AS same_attempt,
    CASE WHEN file.id IS NOT NULL AND file.size = json_extract(body, '$.artifact.source_size')
      AND file.mtime = json_extract(body, '$.artifact.source_mtime') THEN 1 ELSE 0 END AS source_matches,
    CASE WHEN artifact.cache_key IS NULL OR (
      artifact.file_id = json_extract(body, '$.artifact.file_id')
      AND artifact.source_size = json_extract(body, '$.artifact.source_size')
      AND artifact.source_mtime = json_extract(body, '$.artifact.source_mtime')
      AND artifact.source_sha256 = json_extract(body, '$.artifact.source_sha256')
      AND artifact.pipeline_sha256 = json_extract(body, '$.artifact.pipeline_sha256')
      AND artifact.blob_sha256 = json_extract(body, '$.artifact.blob_sha256')
      AND artifact.bytes = json_extract(body, '$.artifact.bytes')) THEN 1 ELSE 0 END AS artifact_matches,
    CASE WHEN json_extract(body, '$.artifact.cache_key') = json_extract(body, '$.logical_key')
      OR EXISTS (SELECT 1 FROM analysis_requests WHERE component = 'fragment_index' AND state = 'submitted'
        AND result_cache_key = json_extract(body, '$.artifact.cache_key'))
      OR EXISTS (SELECT 1 FROM cluster_fragment_index_heads WHERE logical_cache_key = json_extract(body, '$.logical_key')
        AND generation_cache_key = json_extract(body, '$.artifact.cache_key')) THEN 1 ELSE 0 END AS generation_matches
  FROM request LEFT JOIN background_jobs job ON job.id = json_extract(body, '$.token.job_id')
    LEFT JOIN files file ON file.id = json_extract(body, '$.artifact.file_id')
    LEFT JOIN cluster_fragment_index_artifacts artifact ON artifact.cache_key = json_extract(body, '$.artifact.cache_key')
), classified AS (
  SELECT *, CASE
    WHEN state = 'succeeded' AND same_attempt = 1 AND result_ref = output THEN 'already_published'
    WHEN owns = 0 THEN 'lost_ownership'
    WHEN source_matches = 0 THEN 'source_changed'
    WHEN artifact_matches = 0 OR generation_matches = 0 THEN 'artifact_conflict'
    ELSE 'published' END AS outcome FROM snapshot
)
INSERT INTO background_job_commands (id, operation, request_json, result_json)
SELECT json_extract(body, '$.token.claim_id'), 'publish_fragment', body,
  CASE WHEN outcome IN ('published','already_published') THEN
    json_object('outcome', outcome, 'job_id', id, 'result_ref', output || '')
    ELSE json_object('outcome', outcome) END FROM classified RETURNING result_json
"#;
