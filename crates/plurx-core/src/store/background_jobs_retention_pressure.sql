-- Retention pressure for the durable work queue.
--
-- Admission refuses at MAX_RETAINED_JOBS (10,000) rows and MAX_WAITERS
-- (16,384) receipts, counting terminal rows, while ordinary retirement waited
-- the full seven-day receipt window. One busy day of finished work (the
-- 2026-09-27 embedding backfill alone was 6,219 rows) therefore held every
-- queue closed for a week: fragment builds bounced as queue_full_or_busy,
-- library scans and subtitle extraction were refused. Upkeep now compacts the
-- oldest terminal details and internal receipts one page at a time once a
-- table is within eight pages of its cap. Fresh installs run this after the
-- shared schema; existing databases reach it as replicated v63 / SQLite v85.
DROP TRIGGER IF EXISTS background_job_maintenance_command;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_maintenance_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'maintain'
BEGIN
    UPDATE background_job_waiters SET state = 'failed', last_error_code = 'index_retry_window_expired',
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE (request_scope, request_id) IN (SELECT request_scope, request_id FROM background_job_waiters
        WHERE state = 'pending' AND retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms')
        ORDER BY retry_deadline_ms, request_scope, request_id LIMIT 128);
    UPDATE background_job_waiters SET state = 'cancelled', updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE (request_scope, request_id) IN (SELECT delivery.request_scope, delivery.request_id FROM background_job_waiters delivery
        WHERE delivery.consumer_kind = 'background_delivery' AND delivery.state = 'pending'
            AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
                WHERE interest.job_id = delivery.consumer_ref AND interest.target_node_id = delivery.target_node_id
                    AND interest.state = 'awaiting_hydration'
                    AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract(NEW.request_json, '$.now_ms')))
        ORDER BY delivery.request_scope, delivery.request_id LIMIT 128);

    UPDATE background_job_waiters SET state = 'cancelled',
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE (request_scope, request_id) IN (
        SELECT request_scope, request_id FROM background_job_waiters
        WHERE state IN ('pending','awaiting_hydration')
          AND deadline_ms <= json_extract(NEW.request_json, '$.now_ms')
        ORDER BY deadline_ms, request_scope, request_id LIMIT 128);
    UPDATE background_jobs SET priority = COALESCE((SELECT MAX(priority)
        FROM background_job_waiters WHERE job_id = background_jobs.id
          AND state IN ('pending','awaiting_hydration')
          AND (deadline_ms IS NULL OR deadline_ms > json_extract(NEW.request_json, '$.now_ms'))), 0)
    WHERE id IN (SELECT job_id FROM background_job_waiters
        WHERE state IN ('cancelled','failed') AND updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
        ORDER BY job_id LIMIT 128) AND state IN ('queued','running');
    UPDATE background_jobs SET state = 'failed',
        failed_attempts = failed_attempts + CASE WHEN state = 'running' THEN 1 ELSE 0 END,
        last_error_code = 'index_retry_window_expired', owner_node_id = NULL,
        owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT id FROM background_jobs WHERE retry_deadline_ms > 0
        AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms')
        AND (state = 'queued' OR (state = 'running' AND lease_expires_ms <= json_extract(NEW.request_json, '$.now_ms')))
        AND revision < 9223372036854775807 ORDER BY retry_deadline_ms, id LIMIT 128);
    UPDATE background_jobs SET
        state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT job.id FROM background_jobs job
        WHERE job.state IN ('queued','running') AND job.revision < 9223372036854775807
          AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))
          AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
            AND remaining.kind = 'fragment_index_build'
            AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
        ORDER BY job.id LIMIT 128);
    UPDATE background_jobs SET state = 'cancelled', owner_node_id = NULL,
        owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT id FROM background_jobs WHERE state = 'cancelling'
        AND lease_expires_ms <= json_extract(NEW.request_json, '$.now_ms')
        AND revision < 9223372036854775807 ORDER BY lease_expires_ms, id LIMIT 128);
    UPDATE background_jobs SET state = 'failed', failed_attempts = failed_attempts + 1,
        last_error_code = 'execution_abandoned', owner_node_id = NULL,
        owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT id FROM background_jobs WHERE state = 'running'
        AND ((kind != 'fragment_index_build' AND failed_attempts >= attempt_limit - 1)
          OR (kind = 'fragment_index_build' AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
            WHERE interest.job_id = background_jobs.id AND interest.state = 'pending'
              AND interest.failed_attempts + CASE WHEN interest.participation_fence = background_jobs.fence THEN 1 ELSE 0 END < interest.attempt_limit
              AND (interest.retry_deadline_ms = 0 OR interest.retry_deadline_ms > json_extract(NEW.request_json, '$.now_ms')))))
        AND lease_expires_ms <= json_extract(NEW.request_json, '$.now_ms')
        AND revision < 9223372036854775807 ORDER BY lease_expires_ms, id LIMIT 128);
    DELETE FROM background_job_attempts WHERE (job_id, fence) IN (
        SELECT old.job_id, old.fence FROM background_job_attempts old
        WHERE old.finished_at_ms IS NOT NULL
          AND old.resolve_until_ms <= json_extract(NEW.request_json, '$.now_ms')
          AND ((SELECT COUNT(*) FROM background_job_attempts newer
            WHERE newer.job_id = old.job_id AND newer.finished_at_ms IS NOT NULL
              AND newer.fence > old.fence) >= 16
            OR ((SELECT COUNT(*) FROM background_job_attempts) >= 39872
              AND EXISTS (SELECT 1 FROM background_job_attempts newer
                WHERE newer.job_id = old.job_id AND newer.fence > old.fence)))
        ORDER BY old.finished_at_ms, old.job_id, old.fence LIMIT 128);
    DELETE FROM background_job_waiters WHERE (request_scope, request_id) IN (
        SELECT request_scope, request_id FROM background_job_waiters
        WHERE state IN ('succeeded','failed','cancelled') AND retain_identity = 0
          AND receipt_expires_ms <= json_extract(NEW.request_json, '$.now_ms')
        ORDER BY receipt_expires_ms, request_scope, request_id LIMIT 128);
    -- Receipt pressure (MAX_WAITERS - 8 pages = 15360): internal terminal
    -- receipts compact early, oldest first. User-scoped and identity-retaining
    -- receipts keep their full window, and a receipt whose job is still active
    -- is never touched, so admission cannot be held closed by a day of
    -- finished internal work.
    DELETE FROM background_job_waiters WHERE (request_scope, request_id) IN (
        SELECT waiter.request_scope, waiter.request_id FROM background_job_waiters waiter
        WHERE (SELECT COUNT(*) FROM background_job_waiters) >= 15360
          AND waiter.state IN ('succeeded','failed','cancelled') AND waiter.retain_identity = 0
          AND waiter.request_scope NOT LIKE 'user:%'
          AND NOT EXISTS (SELECT 1 FROM background_jobs job WHERE job.id = waiter.job_id
            AND job.state IN ('queued','running','cancelling'))
        ORDER BY waiter.updated_at_ms, waiter.request_scope, waiter.request_id LIMIT 128);
    DELETE FROM background_jobs WHERE id IN (
        SELECT job.id FROM background_jobs job
        WHERE job.state IN ('succeeded','failed','cancelled')
          AND (job.updated_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 604800000
            -- Retained-row pressure (MAX_RETAINED_JOBS - 8 pages = 8976): terminal
            -- details compact before seven days, oldest first, under the same
            -- protections as ordinary retirement. The receipt row carries the
            -- request's outcome, so idempotent re-requests are unaffected.
            OR (SELECT COUNT(*) FROM background_jobs) >= 8976)
          AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
            AND remaining.kind = 'fragment_index_build'
            AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
          AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))
        ORDER BY job.updated_at_ms, job.id LIMIT 128);
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
