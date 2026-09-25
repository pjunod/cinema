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
WHERE EXISTS (SELECT 1 FROM background_job_waiters
    WHERE state IN ('pending','awaiting_hydration') AND deadline_ms <= json_extract($1, '$.now_ms'))
  OR EXISTS (SELECT 1 FROM background_job_waiters delivery
    WHERE delivery.consumer_kind = 'background_delivery' AND delivery.state = 'pending'
      AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
        WHERE interest.job_id = delivery.consumer_ref AND interest.target_node_id = delivery.target_node_id
          AND interest.state = 'awaiting_hydration'
          AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract($1, '$.now_ms'))))
  OR EXISTS (SELECT 1 FROM background_jobs
    WHERE state = 'cancelling' AND lease_expires_ms <= json_extract($1, '$.now_ms'))
  OR EXISTS (SELECT 1 FROM background_jobs WHERE state = 'running' AND failed_attempts >= attempt_limit - 1
    AND lease_expires_ms <= json_extract($1, '$.now_ms'))
  OR EXISTS (SELECT 1 FROM background_jobs WHERE retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract($1, '$.now_ms')
    AND (state = 'queued' OR (state = 'running' AND lease_expires_ms <= json_extract($1, '$.now_ms'))))
  OR EXISTS (SELECT 1 FROM background_job_attempts old
    WHERE old.finished_at_ms IS NOT NULL AND old.resolve_until_ms <= json_extract($1, '$.now_ms')
      AND (SELECT COUNT(*) FROM background_job_attempts newer
        WHERE newer.job_id = old.job_id AND newer.finished_at_ms IS NOT NULL
          AND newer.fence > old.fence) >= 16)
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
