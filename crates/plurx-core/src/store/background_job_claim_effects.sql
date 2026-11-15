-- The claim records the job's resource bundle. Shared by the base job
-- schema and the SQLite source-lease migration, which reinstalls it.
DROP TRIGGER IF EXISTS background_job_claim_effects;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_claim_effects
AFTER UPDATE ON background_jobs
WHEN NEW.state = 'running' AND NEW.fence > OLD.fence
BEGIN
    UPDATE background_job_attempts
    SET finished_at_ms = NEW.updated_at_ms, outcome = 'lease_expired'
    WHERE job_id = NEW.id AND finished_at_ms IS NULL;
    INSERT INTO background_job_attempts (
        job_id, fence, claim_id, owner_node_id, owner_boot_id,
        started_at_ms, resolve_until_ms)
    VALUES (NEW.id, NEW.fence, NEW.claim_id, NEW.owner_node_id,
        NEW.owner_boot_id, NEW.updated_at_ms, NEW.updated_at_ms + 120000);
    DELETE FROM background_job_reservations
    WHERE job_id = NEW.id OR expires_at_ms <= NEW.updated_at_ms;
    INSERT INTO background_job_reservations (resource_key, slot, job_id, fence, expires_at_ms)
    SELECT required.resource_key, MIN(candidate.slot), NEW.id, NEW.fence, NEW.lease_expires_ms
    FROM background_job_required_resources required
    CROSS JOIN (SELECT 0 AS slot UNION ALL SELECT 1) candidate
    WHERE required.job_id = NEW.id AND required.resource_key NOT LIKE 'source_io%' AND NOT EXISTS (
        SELECT 1 FROM background_job_reservations held
        WHERE held.resource_key = required.resource_key AND held.slot = candidate.slot)
    GROUP BY required.resource_key;
    -- Source readers are counted on source_leases (per node and per storage
    -- domain). A source reservation is only the job's bundle record, so it
    -- takes the next free slot instead of one of two.
    INSERT INTO background_job_reservations (resource_key, slot, job_id, fence, expires_at_ms)
    SELECT required.resource_key, (SELECT COALESCE(MAX(held.slot), -1) + 1 FROM background_job_reservations held
        WHERE held.resource_key = required.resource_key), NEW.id, NEW.fence, NEW.lease_expires_ms
    FROM background_job_required_resources required
    WHERE required.job_id = NEW.id AND required.resource_key LIKE 'source_io%';
    UPDATE background_job_waiters SET failed_attempts = failed_attempts + 1,
        attempt_errors = attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || 'lease_expired',
        last_error_code = 'lease_expired', updated_at_ms = NEW.updated_at_ms,
        state = CASE WHEN failed_attempts + 1 >= attempt_limit THEN 'failed' ELSE state END
    WHERE NEW.kind = 'fragment_index_build' AND OLD.state = 'running'
        AND job_id = NEW.id AND state = 'pending' AND participation_fence = OLD.fence;
    UPDATE background_job_waiters SET participation_fence = NEW.fence
    WHERE job_id = NEW.id AND state = 'pending' AND not_before_ms <= NEW.updated_at_ms
        AND failed_attempts < attempt_limit
        AND (retry_deadline_ms = 0 OR retry_deadline_ms > NEW.updated_at_ms)
        AND (deadline_ms IS NULL OR deadline_ms > NEW.updated_at_ms);
    SELECT CASE WHEN EXISTS (SELECT 1 FROM background_job_required_resources required
        WHERE required.job_id = NEW.id AND NOT EXISTS (SELECT 1 FROM background_job_reservations held
            WHERE held.job_id = NEW.id AND held.fence = NEW.fence AND held.resource_key = required.resource_key))
        THEN RAISE(ABORT, 'background shared resource unavailable') END;
    -- Project after participation and abandoned-attempt accounting have landed.
    UPDATE background_jobs SET priority = priority WHERE id = NEW.id AND kind = 'fragment_index_build';
END;
