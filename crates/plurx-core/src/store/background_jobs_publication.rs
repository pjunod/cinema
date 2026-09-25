//! Typed publication inputs. Output metadata and queue settlement share the
//! same statement transaction; there is no post-completion callback.

use serde::{Deserialize, Serialize};

use super::background_jobs::JobToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscodeJobOutput {
    pub recipe_hash: String,
    pub recipe_version: i64,
    pub relative_dir: String,
    pub bytes: i64,
    pub expected_previous_bytes: Option<i64>,
    pub manifest_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishTranscodeJob {
    pub token: JobToken,
    pub output: TranscodeJobOutput,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum JobPublishOutcome {
    Published { job_id: String, result_ref: String },
    AlreadyPublished { job_id: String, result_ref: String },
    LostOwnership,
    SourceChanged,
    ArtifactConflict,
}

pub(super) const PUBLISH_TRANSCODE_SQL: &str = r#"
WITH request AS (
  SELECT json($1) AS body,
    json_set(json_extract($1, '$.output'), '$.node_id', json_extract($1, '$.token.node_id')) AS artifact
), snapshot AS (
  SELECT request.*, job.id, job.state, job.result_ref,
    CASE WHEN job.state = 'running'
      AND job.kind = 'transcode_prepare' AND job.payload_version = 1
      AND job.owner_node_id = json_extract(body, '$.token.node_id')
      AND job.owner_boot_id = json_extract(body, '$.token.boot_id')
      AND job.claim_id = json_extract(body, '$.token.claim_id')
      AND job.fence = json_extract(body, '$.token.fence')
      AND job.revision = json_extract(body, '$.token.revision')
      AND job.lease_expires_ms = json_extract(body, '$.token.lease_expires_ms')
      AND job.lease_expires_ms > json_extract(body, '$.now_ms')
      AND job.revision < 9223372036854775807
      AND NOT EXISTS (SELECT 1 FROM settings WHERE key =
        'internal.cluster_job_owner_removed.' || job.owner_node_id)
      THEN 1 ELSE 0 END AS owns,
    CASE WHEN EXISTS (SELECT 1 FROM background_job_attempts attempt
      WHERE attempt.job_id = job.id AND attempt.fence = job.fence
        AND attempt.fence = json_extract(body, '$.token.fence')
        AND attempt.claim_id = json_extract(body, '$.token.claim_id')
        AND attempt.owner_node_id = json_extract(body, '$.token.node_id')
        AND attempt.owner_boot_id = json_extract(body, '$.token.boot_id')) THEN 1 ELSE 0 END AS same_attempt,
    CASE WHEN file.id IS NOT NULL
      AND file.size = json_extract(job.payload_json, '$.source_size')
      AND file.mtime = json_extract(job.payload_json, '$.source_mtime') THEN 1 ELSE 0 END AS source_matches,
    CASE WHEN recipe.recipe_hash IS NULL OR (recipe.file_id = file.id
      AND recipe.recipe_version = json_extract(body, '$.output.recipe_version')) THEN 1 ELSE 0 END AS recipe_matches,
    CASE WHEN location.recipe_hash IS NULL OR (location.publication_generation < 9223372036854775807 AND (location.complete = 0 OR (
      location.relative_dir = json_extract(body, '$.output.relative_dir') AND (
        (json_extract(body, '$.output.expected_previous_bytes') IS NULL
          AND location.bytes = json_extract(body, '$.output.bytes')
          AND (location.manifest_digest IS NULL OR location.manifest_digest = json_extract(body, '$.output.manifest_digest')))
        OR (json_extract(body, '$.output.expected_previous_bytes') IS NOT NULL
          AND location.bytes = json_extract(body, '$.output.expected_previous_bytes')
          AND location.manifest_digest IS NULL
          AND json_extract(body, '$.output.bytes') >= location.bytes))))) THEN 1 ELSE 0 END AS location_matches
  FROM request LEFT JOIN background_jobs job ON job.id = json_extract(body, '$.token.job_id')
    LEFT JOIN files file ON file.id = json_extract(job.payload_json, '$.file_id')
    LEFT JOIN transcode_cache_recipes recipe ON recipe.recipe_hash = json_extract(body, '$.output.recipe_hash')
    LEFT JOIN transcode_cache_locations location
      ON location.recipe_hash = json_extract(body, '$.output.recipe_hash')
      AND location.node_id = json_extract(body, '$.token.node_id') AND location.storage_class = 'local'
), classified AS (
  SELECT *, CASE
    WHEN state = 'succeeded' AND same_attempt = 1 AND result_ref = artifact THEN 'already_published'
    WHEN owns = 0 THEN 'lost_ownership'
    WHEN source_matches = 0 THEN 'source_changed'
    WHEN recipe_matches = 0 OR location_matches = 0 THEN 'artifact_conflict'
    ELSE 'published' END AS outcome FROM snapshot
)
INSERT INTO background_job_commands (id, operation, request_json, result_json)
SELECT json_extract(body, '$.token.claim_id'), 'publish_transcode', body,
  CASE WHEN outcome IN ('published','already_published') THEN
    json_object('outcome', outcome, 'job_id', id, 'result_ref', artifact || '')
    ELSE json_object('outcome', outcome) END
FROM classified RETURNING result_json
"#;
