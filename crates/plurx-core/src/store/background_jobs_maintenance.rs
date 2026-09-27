//! Bounded queue upkeep. Candidate reads can avoid a consensus write when
//! there are no expired interests, abandoned cancellations or old receipts.

pub(super) const CANCEL_WAITER_SQL: &str = r#"
INSERT INTO background_job_commands (id, operation, request_json, result_json)
SELECT json_extract($1, '$.request_id'), 'cancel_waiter', $1,
  json_object('job_id', job_id, 'cancelled', json(
    CASE WHEN state IN ('pending','awaiting_hydration','cancelled') THEN 'true' ELSE 'false' END))
FROM background_job_waiters
WHERE request_scope = json_extract($1, '$.scope')
  AND request_id = json_extract($1, '$.request_id')
RETURNING result_json
"#;

pub(super) const MAINTENANCE_NEEDED: &str = r#"
SELECT json_object('needed', 1) AS result_json
WHERE EXISTS (SELECT 1 FROM background_artwork_locations location
    WHERE verified_at_ms <= json_extract($1, '$.now_ms') - 604800000
      AND NOT EXISTS (SELECT 1 FROM background_jobs job WHERE job.state IN ('queued','running','cancelling')
        AND ((job.kind = 'artwork_derivative' AND json_extract(job.payload_json,'$.artifact_key') = location.artifact_key)
          OR (job.kind = 'artifact_hydrate' AND json_extract(job.payload_json,'$.artifact_key') = 'artwork:' || location.artifact_key))))
  OR ((SELECT COUNT(*) FROM background_transcode_artifacts) < 32768 AND EXISTS (SELECT 1
FROM background_jobs job JOIN files file ON file.id = json_extract(job.payload_json,'$.file_id')
JOIN transcode_cache_locations location
  ON location.recipe_hash = json_extract(CASE WHEN json_valid(job.result_ref) THEN job.result_ref ELSE '{}' END,'$.recipe_hash')
  AND location.manifest_digest = json_extract(CASE WHEN json_valid(job.result_ref) THEN job.result_ref ELSE '{}' END,'$.manifest_digest')
  AND location.node_id = json_extract(CASE WHEN json_valid(job.result_ref) THEN job.result_ref ELSE '{}' END,'$.node_id')
JOIN transcode_cache_recipes recipe ON recipe.recipe_hash = location.recipe_hash AND recipe.file_id = file.id
WHERE job.kind = 'transcode_prepare' AND job.state = 'succeeded' AND location.complete = 1
  AND location.storage_class = 'local' AND length(location.recipe_hash) = 64 AND length(location.manifest_digest) = 64
  AND recipe.recipe_version = json_extract(CASE WHEN json_valid(job.result_ref) THEN job.result_ref ELSE '{}' END,'$.recipe_version')
  AND file.size = json_extract(job.payload_json,'$.source_size') AND file.mtime = json_extract(job.payload_json,'$.source_mtime')
  AND NOT EXISTS (SELECT 1 FROM background_transcode_artifacts existing
    WHERE existing.recipe_hash = location.recipe_hash AND existing.manifest_digest = location.manifest_digest)))
  OR EXISTS (SELECT 1 FROM background_transcode_artifacts artifact
    WHERE built_at_ms < json_extract($1,'$.now_ms') - 604800000
      AND NOT EXISTS (SELECT 1 FROM transcode_cache_locations location WHERE location.recipe_hash = artifact.recipe_hash AND location.manifest_digest = artifact.manifest_digest)
      AND NOT EXISTS (SELECT 1 FROM background_jobs job WHERE job.state IN ('queued','running','cancelling')
        AND ((job.kind = 'transcode_prepare' AND json_extract(job.payload_json,'$.file_id') = artifact.file_id)
          OR (job.kind = 'artifact_hydrate' AND json_extract(job.payload_json,'$.artifact_key') = 'transcode:' || artifact.recipe_hash || ':' || artifact.manifest_digest))))
  OR EXISTS (SELECT 1 FROM background_job_waiters
    WHERE state IN ('pending','awaiting_hydration') AND deadline_ms <= json_extract($1, '$.now_ms'))
  OR EXISTS (SELECT 1 FROM background_job_waiters delivery
    WHERE delivery.consumer_kind = 'background_delivery' AND delivery.state = 'pending'
      AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
        WHERE interest.job_id = delivery.consumer_ref AND interest.target_node_id = delivery.target_node_id
          AND interest.state = 'awaiting_hydration'
          AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract($1, '$.now_ms'))))
  OR EXISTS (SELECT 1 FROM background_jobs
    WHERE state = 'cancelling' AND lease_expires_ms <= json_extract($1, '$.now_ms'))
  OR EXISTS (SELECT 1 FROM background_job_waiters WHERE state = 'pending'
    AND retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract($1, '$.now_ms'))
  OR EXISTS (SELECT 1 FROM background_jobs WHERE state = 'running'
    AND ((kind != 'fragment_index_build' AND failed_attempts >= attempt_limit - 1)
      OR (kind = 'fragment_index_build' AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
        WHERE interest.job_id = background_jobs.id AND interest.state = 'pending'
          AND interest.failed_attempts + CASE WHEN interest.participation_fence = background_jobs.fence THEN 1 ELSE 0 END < interest.attempt_limit
          AND (interest.retry_deadline_ms = 0 OR interest.retry_deadline_ms > json_extract($1, '$.now_ms')))))
    AND lease_expires_ms <= json_extract($1, '$.now_ms'))
  OR EXISTS (SELECT 1 FROM background_jobs WHERE retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract($1, '$.now_ms')
    AND (state = 'queued' OR (state = 'running' AND lease_expires_ms <= json_extract($1, '$.now_ms'))))
  OR EXISTS (SELECT 1 FROM background_job_attempts old
    WHERE old.finished_at_ms IS NOT NULL AND old.resolve_until_ms <= json_extract($1, '$.now_ms')
      AND ((SELECT COUNT(*) FROM background_job_attempts newer
        WHERE newer.job_id = old.job_id AND newer.finished_at_ms IS NOT NULL
          AND newer.fence > old.fence) >= 16
        OR ((SELECT COUNT(*) FROM background_job_attempts) >= 39872
          AND EXISTS (SELECT 1 FROM background_job_attempts newer
            WHERE newer.job_id = old.job_id AND newer.fence > old.fence))))
  OR EXISTS (SELECT 1 FROM background_job_waiters
    WHERE state IN ('succeeded','failed','cancelled') AND retain_identity = 0
      AND receipt_expires_ms <= json_extract($1, '$.now_ms'))
  OR EXISTS (SELECT 1 FROM background_jobs job
    WHERE job.state IN ('succeeded','failed','cancelled')
      AND job.updated_at_ms <= json_extract($1, '$.now_ms') - 604800000
      AND NOT EXISTS (SELECT 1 FROM background_job_waiters
        WHERE job_id = job.id AND state IN ('pending','awaiting_hydration')))
"#;

pub(super) const MAINTENANCE_SQL: &str = r#"
INSERT INTO background_job_commands (id, operation, request_json, result_json)
VALUES (json_extract($1, '$.command_id'), 'maintain', $1, '{}')
RETURNING result_json
"#;
