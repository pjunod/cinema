-- Settled history yields to new work instead of refusing it.
--
-- Terminal jobs are retained for seven days as receipts, but the table is
-- bounded at 10,000 rows and that bound used to count history. One busy day
-- (an embedding backfill, a subtitle sweep) could settle 10,000 jobs, after
-- which every enqueue -- library scans, targeted scans from Monarr, artwork --
-- answered `queue_full` for a week, while the queue held almost no live work.
--
-- Now the enqueue trigger evicts the oldest evictable settled rows when the
-- table is at its bound, upkeep starts evicting at 9,000 so the enqueue path
-- rarely has to, and the admission statement refuses only when nothing is
-- evictable. Evictable means settled, not pinned by a pending waiter, and not
-- the fragment history a legacy import still needs. Waiters keep their
-- receipts; a settled job row going early is the same state the seven-day
-- prune already produces.
DROP TRIGGER IF EXISTS background_job_enqueue_command;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_enqueue_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'enqueue'
BEGIN
    UPDATE job_leases SET
        revision = json_extract(NEW.request_json, '$.producer_replacement.revision'),
        expires_at_ms = json_extract(NEW.request_json, '$.producer_replacement.expires_at_unix_ms'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE resource = json_extract(NEW.request_json, '$.producer_lease.resource')
        AND json_extract(NEW.result_json, '$.outcome') != 'producer_fenced';

    DELETE FROM background_jobs WHERE json_extract(NEW.result_json, '$.outcome') = 'accepted'
        AND (SELECT COUNT(*) FROM background_jobs) >= 10000
        AND id IN (SELECT job.id FROM background_jobs job
        WHERE job.state IN ('succeeded','failed','cancelled')
          AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
            AND remaining.kind = 'fragment_index_build'
            AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
          AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))
        ORDER BY job.updated_at_ms, job.id LIMIT 128);

    INSERT INTO background_jobs (
        id, kind, payload_version, payload_json, dedupe_key, priority,
        state, target_node_id, not_before_ms, created_at_ms, updated_at_ms, attempt_limit)
    SELECT json_extract(NEW.result_json, '$.job_id'),
        json_extract(NEW.request_json, '$.payload.kind'), 1,
        json_extract(NEW.request_json, '$.payload'),
        json_extract(NEW.request_json, '$.dedupe_key'),
        json_extract(NEW.request_json, '$.priority'), 'queued',
        json_extract(NEW.request_json, '$.payload.target_node_id'),
        json_extract(NEW.request_json, '$.not_before_ms'),
        json_extract(NEW.request_json, '$.now_ms'),
        json_extract(NEW.request_json, '$.now_ms'),
        COALESCE(json_extract(NEW.request_json, '$.attempt_limit'), 5)
    WHERE json_extract(NEW.result_json, '$.outcome') = 'accepted'
        AND NOT EXISTS (SELECT 1 FROM background_jobs
            WHERE id = json_extract(NEW.result_json, '$.job_id'));

    INSERT INTO background_job_waiters (
        request_scope, request_id, request_digest, job_id, consumer_kind,
        consumer_ref, priority, state, target_node_id, deadline_ms,
        receipt_expires_ms, retain_identity, created_at_ms, updated_at_ms,
        attempt_limit, not_before_ms, participation_fence)
    SELECT json_extract(NEW.request_json, '$.request.scope'),
        json_extract(NEW.request_json, '$.request.request_id'),
        json_extract(NEW.request_json, '$.request.request_digest'),
        json_extract(NEW.result_json, '$.job_id'),
        json_extract(NEW.request_json, '$.request.consumer_kind'),
        json_extract(NEW.request_json, '$.request.consumer_ref'),
        json_extract(NEW.request_json, '$.priority'), 'pending',
        json_extract(NEW.request_json, '$.request.target_node_id'),
        json_extract(NEW.request_json, '$.request.deadline_ms'),
        json_extract(NEW.result_json, '$.receipt_expires_ms'),
        COALESCE(json_extract(NEW.request_json, '$.request.retain_identity'), 0),
        json_extract(NEW.request_json, '$.now_ms'),
        json_extract(NEW.request_json, '$.now_ms'),
        COALESCE(json_extract(NEW.request_json, '$.attempt_limit'), 5),
        json_extract(NEW.request_json, '$.not_before_ms'),
        COALESCE((SELECT fence FROM background_jobs WHERE id = json_extract(NEW.result_json, '$.job_id')
            AND state = 'running' AND json_extract(NEW.request_json, '$.not_before_ms') <= json_extract(NEW.request_json, '$.now_ms')), 0)
    WHERE json_extract(NEW.result_json, '$.outcome') = 'accepted';

    UPDATE background_job_waiters SET priority = MAX(priority,
        json_extract(NEW.request_json, '$.priority'))
    WHERE request_scope = json_extract(NEW.request_json, '$.request.scope')
        AND request_id = json_extract(NEW.request_json, '$.request.request_id')
        AND state = 'pending'
        AND json_extract(NEW.result_json, '$.outcome') = 'existing';

    UPDATE background_jobs SET priority = MAX(priority,
        json_extract(NEW.request_json, '$.priority'))
    WHERE id = json_extract(NEW.result_json, '$.job_id')
        AND state IN ('queued','running')
        AND json_extract(NEW.result_json, '$.outcome') IN ('accepted','existing')
        AND EXISTS (SELECT 1 FROM background_job_waiters
            WHERE request_scope = json_extract(NEW.request_json, '$.request.scope')
            AND request_id = json_extract(NEW.request_json, '$.request.request_id') AND state = 'pending');

    UPDATE background_jobs SET
        not_before_ms = CASE WHEN state = 'queued' THEN COALESCE((SELECT MIN(not_before_ms)
            FROM background_job_waiters WHERE job_id = background_jobs.id AND state = 'pending'), not_before_ms) ELSE not_before_ms END,
        retry_deadline_ms = CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state = 'pending' AND retry_deadline_ms = 0) THEN 0
            ELSE COALESCE((SELECT MAX(retry_deadline_ms) FROM background_job_waiters
                WHERE job_id = background_jobs.id AND state = 'pending'), retry_deadline_ms) END
    WHERE id = json_extract(NEW.result_json, '$.job_id') AND kind = 'fragment_index_build'
        AND state IN ('queued','running') AND json_extract(NEW.result_json, '$.outcome') IN ('accepted','existing');

    UPDATE analysis_requests SET state = 'submitted', owner_node_id = NULL, lease_expires_ms = NULL,
        result_cache_key = json_extract(NEW.request_json, '$.fragment_domain.cache_key'),
        expected_predecessor_generation = COALESCE((SELECT generation_cache_key FROM cluster_fragment_index_heads
            WHERE logical_cache_key = json_extract(NEW.request_json, '$.logical_key')), ''),
        last_error_code = NULL, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE request_id = json_extract(NEW.request_json, '$.analysis_request.request_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    INSERT INTO background_fragment_targets (cache_key, target_node_id, job_id)
    SELECT json_extract(NEW.request_json, '$.fragment_domain.cache_key'),
        json_extract(NEW.request_json, '$.fragment_domain.target_node_id'), json_extract(NEW.result_json, '$.job_id')
    WHERE json_type(NEW.request_json, '$.fragment_domain') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted'
    ON CONFLICT(cache_key, target_node_id) DO UPDATE SET job_id = excluded.job_id;

    INSERT INTO cluster_fragment_index_jobs (cache_key, file_id, source_size, source_mtime, source_sha256,
        pipeline_sha256, priority, trigger, target_node_id, state, owner_node_id, fence, lease_expires_ms,
        attempts, not_before_ms, created_at_ms, updated_at_ms)
    SELECT json_extract(NEW.request_json, '$.fragment_domain.cache_key'),
        json_extract(NEW.request_json, '$.fragment_domain.file_id'), json_extract(NEW.request_json, '$.fragment_domain.source_size'),
        json_extract(NEW.request_json, '$.fragment_domain.source_mtime'), json_extract(NEW.request_json, '$.fragment_domain.source_sha256'),
        json_extract(NEW.request_json, '$.fragment_domain.pipeline_sha256'), json_extract(NEW.request_json, '$.fragment_domain.priority'),
        json_extract(NEW.request_json, '$.fragment_domain.trigger'), json_extract(NEW.request_json, '$.fragment_domain.target_node_id'),
        job.state, job.owner_node_id, job.fence, job.lease_expires_ms,
        job.failed_attempts + CASE WHEN job.state = 'running' THEN 1 ELSE 0 END,
        job.not_before_ms, job.created_at_ms, job.updated_at_ms
    FROM background_jobs job WHERE job.id = json_extract(NEW.result_json, '$.job_id')
        AND json_type(NEW.request_json, '$.fragment_domain') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted'
    ON CONFLICT(cache_key, target_node_id) DO UPDATE SET state = excluded.state,
        owner_node_id = excluded.owner_node_id, fence = excluded.fence, lease_expires_ms = excluded.lease_expires_ms,
        priority = excluded.priority, trigger = excluded.trigger, updated_at_ms = excluded.updated_at_ms,
        attempts = CASE WHEN cluster_fragment_index_jobs.state = 'ready'
            OR json_extract(NEW.request_json, '$.analysis_request.force_rebuild') = 1 THEN excluded.attempts ELSE cluster_fragment_index_jobs.attempts END,
        attempt_errors = CASE WHEN cluster_fragment_index_jobs.state = 'ready'
            OR json_extract(NEW.request_json, '$.analysis_request.force_rebuild') = 1 THEN '' ELSE cluster_fragment_index_jobs.attempt_errors END,
        index_retry_deadline_ms = CASE WHEN cluster_fragment_index_jobs.state = 'ready'
            OR json_extract(NEW.request_json, '$.analysis_request.force_rebuild') = 1 THEN 0 ELSE cluster_fragment_index_jobs.index_retry_deadline_ms END,
        index_diagnostic_json = CASE WHEN cluster_fragment_index_jobs.state = 'ready'
            OR json_extract(NEW.request_json, '$.analysis_request.force_rebuild') = 1 THEN '' ELSE cluster_fragment_index_jobs.index_diagnostic_json END;

    UPDATE background_fragment_targets SET job_id = json_extract(NEW.result_json, '$.job_id')
    WHERE json_type(NEW.request_json, '$.delivery_parent') IS NOT NULL
        AND cache_key = substr(json_extract(NEW.request_json, '$.payload.artifact_key'), 10)
        AND target_node_id = json_extract(NEW.request_json, '$.payload.target_node_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    -- Only the sealed snapshot importer can supply legacy_key. Admission,
    -- preserved ledgers and mapping advance in this same transaction.
    UPDATE background_job_waiters SET
        failed_attempts = json_extract(NEW.request_json, '$.legacy_failures'),
        retry_deadline_ms = COALESCE(json_extract(NEW.request_json, '$.legacy_snapshot.index_retry_deadline_ms'), 0),
        attempt_errors = COALESCE(json_extract(NEW.request_json, '$.legacy_snapshot.attempt_errors'), '') ||
            CASE WHEN json_extract(NEW.request_json, '$.legacy_snapshot.state') = 'running'
                THEN CASE WHEN COALESCE(json_extract(NEW.request_json, '$.legacy_snapshot.attempt_errors'), '') = ''
                    THEN 'legacy_abandoned' ELSE ',legacy_abandoned' END ELSE '' END,
        last_error_code = CASE WHEN json_extract(NEW.request_json, '$.legacy_snapshot.state') = 'running'
            THEN 'legacy_abandoned' ELSE json_extract(NEW.request_json, '$.legacy_snapshot.last_error_code') END,
        index_diagnostic_json = COALESCE(json_extract(NEW.request_json, '$.legacy_snapshot.index_diagnostic_json'), '')
    WHERE request_scope = json_extract(NEW.request_json, '$.request.scope')
        AND request_id = json_extract(NEW.request_json, '$.request.request_id')
        AND json_type(NEW.request_json, '$.legacy_key') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    UPDATE background_job_waiters SET state = CASE
        WHEN failed_attempts >= attempt_limit THEN 'failed'
        WHEN retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms') THEN 'failed'
        WHEN (SELECT state FROM background_jobs WHERE id = job_id) IN ('failed','cancelled')
            THEN (SELECT state FROM background_jobs WHERE id = job_id)
        WHEN (SELECT state FROM background_jobs WHERE id = job_id) = 'succeeded'
            THEN CASE WHEN target_node_id IS NULL OR EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                WHERE location.cache_key = json_extract(NEW.request_json, '$.payload.cache_key')
                    AND location.node_id = background_job_waiters.target_node_id) THEN 'succeeded' ELSE 'awaiting_hydration' END
        ELSE state END,
        last_error_code = CASE WHEN failed_attempts >= attempt_limit THEN 'attempt_limit'
            WHEN retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms') THEN 'index_retry_window_expired'
            ELSE last_error_code END,
        result_ref = (SELECT result_ref FROM background_jobs WHERE id = job_id)
    WHERE request_scope = json_extract(NEW.request_json, '$.request.scope')
        AND request_id = json_extract(NEW.request_json, '$.request.request_id')
        AND json_type(NEW.request_json, '$.legacy_key') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    INSERT INTO background_fragment_targets (cache_key, target_node_id, job_id)
    SELECT json_extract(NEW.request_json, '$.payload.cache_key'),
        COALESCE(json_extract(NEW.request_json, '$.request.target_node_id'), ''), json_extract(NEW.result_json, '$.job_id')
    WHERE json_type(NEW.request_json, '$.legacy_key') IS NOT NULL
        AND json_extract(NEW.request_json, '$.payload.kind') = 'fragment_index_build'
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted'
    ON CONFLICT(cache_key, target_node_id) DO UPDATE SET job_id = excluded.job_id;

    UPDATE background_jobs SET
        failed_attempts = CASE WHEN kind = 'transcode_prepare' THEN json_extract(NEW.request_json, '$.legacy_failures') ELSE failed_attempts END,
        checkpoint_json = CASE WHEN kind = 'transcode_prepare' THEN json_object(
            'staging_node_id', json_extract(NEW.request_json, '$.legacy_snapshot.staging_node_id'),
            'legacy_job_id', json_extract(NEW.request_json, '$.legacy_snapshot.id'),
            'prefer_until_ms', json_extract(NEW.request_json, '$.now_ms') + 30000) ELSE checkpoint_json END,
        not_before_ms = COALESCE((SELECT MIN(not_before_ms) FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state = 'pending'), not_before_ms),
        retry_deadline_ms = CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state = 'pending' AND retry_deadline_ms = 0) THEN 0
            ELSE COALESCE((SELECT MAX(retry_deadline_ms) FROM background_job_waiters
                WHERE job_id = background_jobs.id AND state = 'pending'), retry_deadline_ms) END,
        state = CASE WHEN state = 'queued' AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state = 'pending')
            AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
                AND remaining.legacy_key != json_extract(NEW.request_json, '$.legacy_key')
                AND remaining.kind = 'fragment_index_build'
                AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(NEW.request_json, '$.payload.cache_key'))
            THEN 'failed' ELSE state END,
        created_at_ms = MIN(created_at_ms, json_extract(NEW.request_json, '$.legacy_snapshot.created_at_ms'))
    WHERE id = json_extract(NEW.result_json, '$.job_id') AND state IN ('queued','running')
        AND json_type(NEW.request_json, '$.legacy_key') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    UPDATE background_job_legacy SET state = CASE WHEN json_extract(NEW.result_json, '$.outcome') IN ('accepted','existing')
        THEN 'materialized' ELSE 'failed' END,
        job_id = json_extract(NEW.result_json, '$.job_id'), outcome = json_extract(NEW.result_json, '$.outcome'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE legacy_key = json_extract(NEW.request_json, '$.legacy_key') AND state = 'awaiting_import'
        AND json_extract(NEW.result_json, '$.outcome') IN ('accepted','existing','source_changed','conflict');

    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
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
    DELETE FROM background_jobs WHERE id IN (
        SELECT job.id FROM background_jobs job
        WHERE job.state IN ('succeeded','failed','cancelled')
          AND (job.updated_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 604800000
            OR (SELECT COUNT(*) FROM background_jobs) >= 9000)
          AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
            AND remaining.kind = 'fragment_index_build'
            AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
          AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))
        ORDER BY job.updated_at_ms, job.id LIMIT 128);
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
