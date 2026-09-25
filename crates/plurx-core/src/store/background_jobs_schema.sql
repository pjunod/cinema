CREATE TABLE IF NOT EXISTS background_jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    payload_version INTEGER NOT NULL CHECK (payload_version > 0),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json) AND length(CAST(payload_json AS BLOB)) <= 16384),
    dedupe_key TEXT NOT NULL,
    priority INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 3),
    state TEXT NOT NULL CHECK (state IN ('queued','running','cancelling','succeeded','failed','cancelled')),
    target_node_id TEXT,
    owner_node_id TEXT,
    owner_boot_id TEXT,
    claim_id TEXT,
    fence INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    lease_expires_ms INTEGER,
    failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    not_before_ms INTEGER NOT NULL,
    checkpoint_json TEXT CHECK (checkpoint_json IS NULL OR
        (json_valid(checkpoint_json) AND length(CAST(checkpoint_json AS BLOB)) <= 4096)),
    result_ref TEXT,
    last_error_code TEXT CHECK (last_error_code IS NULL OR length(last_error_code) <= 64),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    CHECK ((state IN ('running','cancelling') AND owner_node_id IS NOT NULL
        AND owner_boot_id IS NOT NULL AND claim_id IS NOT NULL
        AND fence > 0 AND revision > 0 AND lease_expires_ms IS NOT NULL)
        OR (state NOT IN ('running','cancelling') AND owner_node_id IS NULL
        AND owner_boot_id IS NULL AND claim_id IS NULL AND lease_expires_ms IS NULL))
) STRICT;

-- next statement
CREATE UNIQUE INDEX IF NOT EXISTS background_jobs_active_key
    ON background_jobs(dedupe_key) WHERE state IN ('queued','running','cancelling');
-- next statement
CREATE INDEX IF NOT EXISTS background_jobs_due
    ON background_jobs(state, not_before_ms, priority DESC, created_at_ms, id);
-- next statement
CREATE INDEX IF NOT EXISTS background_jobs_owner
    ON background_jobs(owner_node_id, owner_boot_id, state);

-- next statement
CREATE TABLE IF NOT EXISTS background_job_waiters (
    request_scope TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    job_id TEXT NOT NULL REFERENCES background_jobs(id),
    consumer_kind TEXT NOT NULL,
    consumer_ref TEXT NOT NULL,
    priority INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 3),
    state TEXT NOT NULL CHECK (state IN ('pending','awaiting_hydration','succeeded','failed','cancelled')),
    target_node_id TEXT,
    deadline_ms INTEGER,
    receipt_expires_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (request_scope, request_id)
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS background_job_waiters_job
    ON background_job_waiters(job_id, state);
-- next statement
CREATE INDEX IF NOT EXISTS background_job_waiters_retention
    ON background_job_waiters(state, receipt_expires_ms);

-- next statement
CREATE TABLE IF NOT EXISTS background_job_attempts (
    job_id TEXT NOT NULL REFERENCES background_jobs(id),
    fence INTEGER NOT NULL CHECK (fence > 0),
    claim_id TEXT NOT NULL UNIQUE,
    owner_node_id TEXT NOT NULL,
    owner_boot_id TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    resolve_until_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    outcome TEXT,
    error_code TEXT,
    PRIMARY KEY (job_id, fence),
    CHECK ((finished_at_ms IS NULL AND outcome IS NULL)
        OR (finished_at_ms IS NOT NULL AND outcome IS NOT NULL))
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS background_job_attempts_retention
    ON background_job_attempts(finished_at_ms, resolve_until_ms);

-- next statement
CREATE TABLE IF NOT EXISTS background_job_reservations (
    resource_key TEXT NOT NULL,
    slot INTEGER NOT NULL CHECK (slot >= 0),
    job_id TEXT NOT NULL REFERENCES background_jobs(id),
    fence INTEGER NOT NULL CHECK (fence > 0),
    expires_at_ms INTEGER NOT NULL,
    PRIMARY KEY (resource_key, slot)
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS background_job_reservations_job
    ON background_job_reservations(job_id, fence);

-- The immutable RETURNING envelope is selected before this trigger runs.
-- Its mutations and retirement share that one statement's transaction. This
-- uses the same result-envelope pattern as the existing DV admission path.
-- next statement
CREATE TABLE IF NOT EXISTS background_job_commands (
    id TEXT PRIMARY KEY,
    operation TEXT NOT NULL,
    request_json TEXT NOT NULL CHECK (json_valid(request_json)),
    result_json TEXT NOT NULL CHECK (json_valid(result_json))
) STRICT;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_enqueue_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'enqueue'
BEGIN
    INSERT INTO background_jobs (
        id, kind, payload_version, payload_json, dedupe_key, priority,
        state, target_node_id, not_before_ms, created_at_ms, updated_at_ms)
    SELECT json_extract(NEW.result_json, '$.job_id'),
        json_extract(NEW.request_json, '$.payload.kind'), 1,
        json_extract(NEW.request_json, '$.payload'),
        json_extract(NEW.request_json, '$.dedupe_key'),
        json_extract(NEW.request_json, '$.priority'), 'queued',
        json_extract(NEW.request_json, '$.payload.target_node_id'),
        json_extract(NEW.request_json, '$.not_before_ms'),
        json_extract(NEW.request_json, '$.now_ms'),
        json_extract(NEW.request_json, '$.now_ms')
    WHERE json_extract(NEW.result_json, '$.outcome') = 'accepted'
        AND NOT EXISTS (SELECT 1 FROM background_jobs
            WHERE id = json_extract(NEW.result_json, '$.job_id'));

    INSERT INTO background_job_waiters (
        request_scope, request_id, request_digest, job_id, consumer_kind,
        consumer_ref, priority, state, target_node_id, deadline_ms,
        receipt_expires_ms, created_at_ms, updated_at_ms)
    SELECT json_extract(NEW.request_json, '$.request.scope'),
        json_extract(NEW.request_json, '$.request.request_id'),
        json_extract(NEW.request_json, '$.request.request_digest'),
        json_extract(NEW.result_json, '$.job_id'),
        json_extract(NEW.request_json, '$.request.consumer_kind'),
        json_extract(NEW.request_json, '$.request.consumer_ref'),
        json_extract(NEW.request_json, '$.priority'), 'pending',
        json_extract(NEW.request_json, '$.payload.target_node_id'),
        json_extract(NEW.request_json, '$.request.deadline_ms'),
        json_extract(NEW.result_json, '$.receipt_expires_ms'),
        json_extract(NEW.request_json, '$.now_ms'),
        json_extract(NEW.request_json, '$.now_ms')
    WHERE json_extract(NEW.result_json, '$.outcome') = 'accepted';

    UPDATE background_jobs SET priority = MAX(priority,
        json_extract(NEW.request_json, '$.priority'))
    WHERE id = json_extract(NEW.result_json, '$.job_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    DELETE FROM background_job_commands WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_renew_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'renew'
BEGIN
    UPDATE background_jobs SET
        revision = revision + 1,
        lease_expires_ms = json_extract(NEW.request_json, '$.now_ms') + 30000,
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT json_extract(value, '$.token.job_id')
        FROM json_each(NEW.result_json)
        WHERE json_extract(value, '$.outcome') = 'renewed');
    UPDATE background_job_reservations SET
        expires_at_ms = json_extract(NEW.request_json, '$.now_ms') + 30000
    WHERE job_id IN (SELECT json_extract(value, '$.token.job_id')
        FROM json_each(NEW.result_json)
        WHERE json_extract(value, '$.outcome') = 'renewed');
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_settlement_effects
AFTER UPDATE ON background_jobs
WHEN OLD.state IN ('running','cancelling') AND NEW.state NOT IN ('running','cancelling')
BEGIN
    DELETE FROM background_job_reservations WHERE job_id = NEW.id AND fence = OLD.fence;
    UPDATE background_job_attempts SET finished_at_ms = NEW.updated_at_ms,
        outcome = CASE WHEN NEW.state = 'queued' AND NEW.failed_attempts = OLD.failed_attempts
          THEN 'yielded' WHEN NEW.state = 'queued' THEN 'retry' ELSE NEW.state END,
        error_code = NEW.last_error_code
    WHERE job_id = NEW.id AND fence = OLD.fence AND finished_at_ms IS NULL;
    UPDATE background_job_waiters SET state = NEW.state, updated_at_ms = NEW.updated_at_ms
    WHERE job_id = NEW.id AND state IN ('pending','awaiting_hydration')
        AND NEW.state IN ('failed','cancelled');
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_cancellation_effects
AFTER UPDATE ON background_jobs
WHEN NEW.state IN ('cancelling','cancelled') AND OLD.state IN ('queued','running')
BEGIN
    UPDATE background_job_waiters SET state = 'cancelled', updated_at_ms = NEW.updated_at_ms
    WHERE job_id = NEW.id AND state IN ('pending','awaiting_hydration');
END;

-- All claim side effects depend on the same successful row transition.
-- An unsuccessful CAS cannot reserve capacity or manufacture an attempt.
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
    INSERT INTO background_job_reservations (
        resource_key, slot, job_id, fence, expires_at_ms)
    SELECT 'source_io', candidate.slot, NEW.id, NEW.fence, NEW.lease_expires_ms
    FROM (SELECT 0 AS slot UNION ALL SELECT 1) candidate
    WHERE NOT EXISTS (SELECT 1 FROM background_job_reservations held
        WHERE held.resource_key = 'source_io' AND held.slot = candidate.slot)
    ORDER BY candidate.slot LIMIT 1;
    SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM background_job_reservations
        WHERE job_id = NEW.id AND fence = NEW.fence AND resource_key = 'source_io')
        THEN RAISE(ABORT, 'background source I/O reservation unavailable') END;
END;
