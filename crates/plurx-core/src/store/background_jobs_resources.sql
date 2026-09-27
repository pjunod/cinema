-- Roots on the same physical storage share a domain regardless of mount alias.
CREATE TABLE IF NOT EXISTS background_storage_domains (
    library_id INTEGER NOT NULL,
    root_path TEXT NOT NULL,
    domain_id TEXT NOT NULL,
    PRIMARY KEY (library_id, root_path)
) STRICT;
-- next statement
CREATE VIEW IF NOT EXISTS background_job_required_resources AS
WITH job_library AS (
    SELECT job.id AS job_id, job.kind, COALESCE(
        json_extract(job.payload_json, '$.library_id'), item.library_id) AS library_id
    FROM background_jobs job
    LEFT JOIN files file ON file.id = json_extract(job.payload_json, '$.file_id')
    LEFT JOIN items item ON item.id = COALESCE(file.item_id, json_extract(job.payload_json, '$.item_id'))
)
SELECT DISTINCT job.job_id, COALESCE('source_io:' || mapping.domain_id, 'source_io') AS resource_key
FROM job_library job LEFT JOIN libraries library ON library.id = job.library_id
LEFT JOIN json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
LEFT JOIN background_storage_domains mapping ON mapping.library_id = job.library_id AND mapping.root_path = root.value
UNION
SELECT id, 'provider:tmdb' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'provider:anilist' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh');
-- Atomic replacement cannot reinterpret live reservations. This restriction
-- concerns changing resource identity, never enabling a feature.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_storage_domains_replace
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'storage_domains'
BEGIN
    DELETE FROM background_storage_domains WHERE NEW.result_json = 'true';
    INSERT INTO background_storage_domains(library_id, root_path, domain_id)
    SELECT json_extract(value, '$.library_id'), json_extract(value, '$.root_path'), json_extract(value, '$.domain_id')
    FROM json_each(NEW.request_json, '$.mappings') WHERE NEW.result_json = 'true';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
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
    WHERE required.job_id = NEW.id AND NOT EXISTS (
        SELECT 1 FROM background_job_reservations held
        WHERE held.resource_key = required.resource_key AND held.slot = candidate.slot)
    GROUP BY required.resource_key;
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

