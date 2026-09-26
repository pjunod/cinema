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
    failure_policy TEXT CHECK (failure_policy IS NULL OR failure_policy IN ('retry','terminal','index_retry')),
    failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    attempt_limit INTEGER NOT NULL DEFAULT 5 CHECK (attempt_limit BETWEEN 1 AND 20),
    retry_deadline_ms INTEGER NOT NULL DEFAULT 0 CHECK (retry_deadline_ms >= 0),
    attempt_errors TEXT NOT NULL DEFAULT '' CHECK (length(attempt_errors) <= 2048),
    index_diagnostic_json TEXT NOT NULL DEFAULT '' CHECK (length(CAST(index_diagnostic_json AS BLOB)) <= 8192),
    yield_count INTEGER NOT NULL DEFAULT 0 CHECK (yield_count >= 0),
    abandoned_count INTEGER NOT NULL DEFAULT 0 CHECK (abandoned_count >= 0),
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
    job_id TEXT NOT NULL,
    consumer_kind TEXT NOT NULL,
    consumer_ref TEXT NOT NULL,
    priority INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 3),
    state TEXT NOT NULL CHECK (state IN ('pending','awaiting_hydration','succeeded','failed','cancelled')),
    target_node_id TEXT,
    deadline_ms INTEGER,
    receipt_expires_ms INTEGER NOT NULL,
    retain_identity INTEGER NOT NULL DEFAULT 0 CHECK (retain_identity IN (0, 1)),
    result_ref TEXT,
    failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    attempt_limit INTEGER NOT NULL DEFAULT 5 CHECK (attempt_limit BETWEEN 1 AND 20),
    not_before_ms INTEGER NOT NULL DEFAULT 0,
    retry_deadline_ms INTEGER NOT NULL DEFAULT 0 CHECK (retry_deadline_ms >= 0),
    participation_fence INTEGER NOT NULL DEFAULT 0 CHECK (participation_fence >= 0),
    attempt_errors TEXT NOT NULL DEFAULT '' CHECK (length(attempt_errors) <= 2048),
    last_error_code TEXT CHECK (last_error_code IS NULL OR length(last_error_code) <= 64),
    index_diagnostic_json TEXT NOT NULL DEFAULT '' CHECK (length(CAST(index_diagnostic_json AS BLOB)) <= 8192),
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

-- Target progress stays in the existing domain history. This link names the
-- shared computation currently responsible for a target without making the
-- legacy domain row a second independently claimable ownership record.
-- next statement
CREATE TABLE IF NOT EXISTS background_fragment_targets (
    cache_key TEXT NOT NULL,
    target_node_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    PRIMARY KEY (cache_key, target_node_id)
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS background_fragment_targets_job ON background_fragment_targets(job_id);
-- next statement
CREATE TRIGGER IF NOT EXISTS background_fragment_target_history_removed
AFTER DELETE ON cluster_fragment_index_jobs
BEGIN
    DELETE FROM background_fragment_targets WHERE cache_key = OLD.cache_key AND target_node_id = OLD.target_node_id;
END;

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
    UPDATE job_leases SET
        revision = json_extract(NEW.request_json, '$.producer_replacement.revision'),
        expires_at_ms = json_extract(NEW.request_json, '$.producer_replacement.expires_at_unix_ms'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE resource = json_extract(NEW.request_json, '$.producer_lease.resource')
        AND json_extract(NEW.result_json, '$.outcome') != 'producer_fenced';

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
WHEN (OLD.state IN ('running','cancelling') AND NEW.state NOT IN ('running','cancelling'))
    OR (OLD.state = 'queued' AND NEW.state = 'failed')
BEGIN
    DELETE FROM background_job_reservations WHERE job_id = NEW.id AND fence = OLD.fence;
    UPDATE background_job_attempts SET finished_at_ms = NEW.updated_at_ms,
        outcome = CASE WHEN NEW.last_error_code = 'execution_abandoned' THEN 'lease_expired'
          WHEN NEW.state = 'queued' AND NEW.failed_attempts = OLD.failed_attempts
          THEN 'yielded' WHEN NEW.state = 'queued' THEN 'retry' ELSE NEW.state END,
        error_code = NEW.last_error_code
    WHERE job_id = NEW.id AND fence = OLD.fence AND finished_at_ms IS NULL;
    UPDATE background_job_attempts SET outcome = 'failed', error_code = NEW.last_error_code
    WHERE job_id = NEW.id AND fence = NEW.fence AND (NEW.state = 'failed'
        OR EXISTS (SELECT 1 FROM background_jobs current WHERE current.id = NEW.id AND current.state = 'failed'));
    UPDATE background_job_waiters SET state = NEW.state,
        last_error_code = COALESCE(NEW.last_error_code, last_error_code), updated_at_ms = NEW.updated_at_ms
    WHERE job_id = NEW.id AND state IN ('pending','awaiting_hydration')
        AND NEW.state IN ('failed','cancelled')
        AND NOT (NEW.kind = 'fragment_index_build' AND NEW.last_error_code = 'execution_abandoned');
    UPDATE background_job_waiters SET state = 'failed', updated_at_ms = NEW.updated_at_ms
    WHERE NEW.kind = 'artifact_hydrate' AND NEW.state = 'failed' AND state = 'awaiting_hydration'
        AND target_node_id = json_extract(NEW.payload_json, '$.target_node_id')
        AND CASE WHEN json_valid(result_ref) THEN 'fragment:' || json_extract(result_ref, '$.artifact_key') END = json_extract(NEW.payload_json, '$.artifact_key');

END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_cancellation_effects
AFTER UPDATE ON background_jobs
WHEN NEW.state IN ('cancelling','cancelled') AND OLD.state IN ('queued','running')
BEGIN
    UPDATE background_job_waiters SET state = 'cancelled', updated_at_ms = NEW.updated_at_ms
    WHERE job_id = NEW.id AND state IN ('pending','awaiting_hydration');
    UPDATE background_job_waiters SET state = 'cancelled', updated_at_ms = NEW.updated_at_ms
    WHERE NEW.kind = 'artifact_hydrate' AND state = 'awaiting_hydration'
        AND target_node_id = json_extract(NEW.payload_json, '$.target_node_id')
        AND CASE WHEN json_valid(result_ref) THEN 'fragment:' || json_extract(result_ref, '$.artifact_key') END = json_extract(NEW.payload_json, '$.artifact_key');
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
    SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM background_job_reservations
        WHERE job_id = NEW.id AND fence = NEW.fence AND resource_key = 'source_io')
        THEN RAISE(ABORT, 'background source I/O reservation unavailable') END;
    -- Project after participation and abandoned-attempt accounting have landed.
    UPDATE background_jobs SET priority = priority WHERE id = NEW.id AND kind = 'fragment_index_build';
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_cancel_waiter_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'cancel_waiter'
BEGIN
    UPDATE background_job_waiters SET state = 'cancelled',
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE request_scope = json_extract(NEW.request_json, '$.scope')
        AND request_id = json_extract(NEW.request_json, '$.request_id')
        AND state IN ('pending','awaiting_hydration');
    UPDATE background_jobs SET priority = COALESCE((
        SELECT MAX(priority) FROM background_job_waiters
        WHERE job_id = background_jobs.id AND state IN ('pending','awaiting_hydration')
          AND (deadline_ms IS NULL OR deadline_ms > json_extract(NEW.request_json, '$.now_ms'))), 0)
    WHERE id = json_extract(NEW.result_json, '$.job_id')
        AND state IN ('queued','running');
    UPDATE background_jobs SET
        state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id = json_extract(NEW.result_json, '$.job_id')
        AND state IN ('queued','running') AND revision < 9223372036854775807
        AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state IN ('pending','awaiting_hydration')
              AND (deadline_ms IS NULL OR deadline_ms > json_extract(NEW.request_json, '$.now_ms')));
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
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
          AND (SELECT COUNT(*) FROM background_job_attempts newer
            WHERE newer.job_id = old.job_id AND newer.finished_at_ms IS NOT NULL
              AND newer.fence > old.fence) >= 16
        ORDER BY old.finished_at_ms, old.job_id, old.fence LIMIT 128);
    DELETE FROM background_job_waiters WHERE (request_scope, request_id) IN (
        SELECT request_scope, request_id FROM background_job_waiters
        WHERE state IN ('succeeded','failed','cancelled') AND retain_identity = 0
          AND receipt_expires_ms <= json_extract(NEW.request_json, '$.now_ms')
        ORDER BY receipt_expires_ms, request_scope, request_id LIMIT 128);
    DELETE FROM background_jobs WHERE id IN (
        SELECT job.id FROM background_jobs job
        WHERE job.state IN ('succeeded','failed','cancelled')
          AND job.updated_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 604800000
          AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
            AND remaining.kind = 'fragment_index_build'
            AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
          AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))
        ORDER BY job.updated_at_ms, job.id LIMIT 128);
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_attempt_compaction
BEFORE DELETE ON background_job_attempts WHEN OLD.finished_at_ms IS NOT NULL
BEGIN
    UPDATE background_jobs SET
        yield_count = yield_count + CASE WHEN OLD.outcome = 'yielded' AND yield_count < 9223372036854775807 THEN 1 ELSE 0 END,
        abandoned_count = abandoned_count + CASE WHEN OLD.outcome = 'lease_expired' AND abandoned_count < 9223372036854775807 THEN 1 ELSE 0 END
    WHERE id = OLD.job_id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_retire_details
BEFORE DELETE ON background_jobs
BEGIN
    DELETE FROM background_job_reservations WHERE job_id = OLD.id;
    DELETE FROM background_job_attempts WHERE job_id = OLD.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_publish_transcode_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_transcode'
BEGIN
    INSERT INTO transcode_cache_recipes (recipe_hash, file_id, recipe_version, created_at)
    SELECT json_extract(NEW.request_json, '$.output.recipe_hash'),
        json_extract(job.payload_json, '$.file_id'),
        json_extract(NEW.request_json, '$.output.recipe_version'),
        json_extract(NEW.request_json, '$.now_ms') / 1000
    FROM background_jobs job WHERE job.id = json_extract(NEW.result_json, '$.job_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'published'
    ON CONFLICT(recipe_hash) DO NOTHING;

    UPDATE offline_packages SET actual_bytes = actual_bytes + (
        json_extract(NEW.request_json, '$.output.bytes') - json_extract(NEW.request_json, '$.output.expected_previous_bytes')),
        updated_at = json_extract(NEW.request_json, '$.now_ms') / 1000
    WHERE state = 'ready' AND recipe_hash = json_extract(NEW.request_json, '$.output.recipe_hash')
        AND node_id = json_extract(NEW.request_json, '$.token.node_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'published'
        AND json_extract(NEW.request_json, '$.output.expected_previous_bytes') IS NOT NULL
        AND EXISTS (SELECT 1 FROM transcode_cache_locations location
            WHERE location.recipe_hash = offline_packages.recipe_hash
              AND location.node_id = offline_packages.node_id AND location.storage_class = 'local'
              AND location.relative_dir = json_extract(NEW.request_json, '$.output.relative_dir')
              AND location.complete = 1 AND location.manifest_digest IS NULL
              AND location.bytes = json_extract(NEW.request_json, '$.output.expected_previous_bytes'));

    INSERT INTO transcode_cache_locations (
        recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
        manifest_digest, scrub_object_index, publication_generation,
        last_used_at, last_seen_at, storage_id, generation_id)
    SELECT json_extract(NEW.request_json, '$.output.recipe_hash'),
        json_extract(NEW.request_json, '$.token.node_id'), 'local',
        json_extract(NEW.request_json, '$.output.relative_dir'),
        json_extract(NEW.request_json, '$.output.bytes'), 1,
        json_extract(NEW.request_json, '$.output.manifest_digest'), 0, 1,
        json_extract(NEW.request_json, '$.now_ms') / 1000,
        json_extract(NEW.request_json, '$.now_ms') / 1000,
        'node:' || json_extract(NEW.request_json, '$.token.node_id') || ':cache',
        json_extract(NEW.request_json, '$.output.relative_dir')
    WHERE json_extract(NEW.result_json, '$.outcome') = 'published'
    ON CONFLICT(recipe_hash, node_id, storage_class) DO UPDATE SET
        relative_dir = excluded.relative_dir, storage_id = excluded.storage_id,
        generation_id = excluded.generation_id, bytes = excluded.bytes, complete = 1,
        publication_generation = transcode_cache_locations.publication_generation + 1,
        manifest_digest = excluded.manifest_digest, scrub_object_index = 0,
        last_seen_at = excluded.last_seen_at;

    UPDATE background_jobs SET state = 'succeeded',
        result_ref = json_extract(NEW.result_json, '$.result_ref'),
        owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id = json_extract(NEW.result_json, '$.job_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'published';

    UPDATE background_job_waiters SET state = CASE
        WHEN deadline_ms <= json_extract(NEW.request_json, '$.now_ms') THEN 'cancelled'
        WHEN target_node_id IS NULL OR target_node_id = json_extract(NEW.request_json, '$.token.node_id')
            THEN 'succeeded' ELSE 'awaiting_hydration' END,
        result_ref = json_extract(NEW.result_json, '$.result_ref'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE job_id = json_extract(NEW.result_json, '$.job_id') AND state = 'pending'
        AND json_extract(NEW.result_json, '$.outcome') = 'published';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_source_changed
AFTER UPDATE OF size, mtime ON files WHEN NEW.size != OLD.size OR NEW.mtime != OLD.mtime
BEGIN
    UPDATE background_jobs SET state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
        last_error_code = 'source_changed', revision = revision + 1
    WHERE kind IN ('transcode_prepare','fragment_index_build') AND state IN ('queued','running')
        AND json_extract(payload_json, '$.file_id') = OLD.id AND revision < 9223372036854775807
        AND (json_extract(payload_json, '$.source_size') != NEW.size
            OR json_extract(payload_json, '$.source_mtime') != NEW.mtime);
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_source_deleted
AFTER DELETE ON files
BEGIN
    UPDATE background_jobs SET state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
        last_error_code = 'source_changed', revision = revision + 1
    WHERE kind IN ('transcode_prepare','fragment_index_build') AND state IN ('queued','running')
        AND json_extract(payload_json, '$.file_id') = OLD.id AND revision < 9223372036854775807;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_publish_fragment_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_fragment'
BEGIN
    INSERT INTO cluster_fragment_index_artifacts (cache_key, file_id, source_size, source_mtime,
        source_sha256, pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
    SELECT json_extract(NEW.request_json, '$.artifact.cache_key'),
        json_extract(NEW.request_json, '$.artifact.file_id'), json_extract(NEW.request_json, '$.artifact.source_size'),
        json_extract(NEW.request_json, '$.artifact.source_mtime'), json_extract(NEW.request_json, '$.artifact.source_sha256'),
        json_extract(NEW.request_json, '$.artifact.pipeline_sha256'), json_extract(NEW.request_json, '$.artifact.blob_sha256'),
        json_extract(NEW.request_json, '$.artifact.bytes'), json_extract(NEW.request_json, '$.artifact.built_by_node_id'),
        json_extract(NEW.request_json, '$.artifact.built_at_ms')
    WHERE json_extract(NEW.result_json, '$.outcome') = 'published'
    ON CONFLICT(cache_key) DO NOTHING;

    INSERT INTO cluster_fragment_index_locations (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
    SELECT json_extract(NEW.request_json, '$.artifact.cache_key'), json_extract(NEW.request_json, '$.token.node_id'),
        json_extract(NEW.request_json, '$.artifact.bytes'), json_extract(NEW.request_json, '$.now_ms'),
        json_extract(NEW.request_json, '$.now_ms')
    WHERE json_extract(NEW.result_json, '$.outcome') = 'published'
    ON CONFLICT(cache_key, node_id) DO UPDATE SET bytes = excluded.bytes,
        verified_at_ms = excluded.verified_at_ms, last_seen_at_ms = excluded.last_seen_at_ms;

    INSERT INTO cluster_fragment_index_heads (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
    SELECT json_extract(NEW.request_json, '$.logical_key'), json_extract(NEW.request_json, '$.artifact.cache_key'),
        request_id, json_extract(NEW.request_json, '$.now_ms') FROM analysis_requests
    WHERE component = 'fragment_index' AND state = 'submitted'
        AND EXISTS (SELECT 1 FROM background_job_waiters interest
            WHERE interest.request_scope = 'analysis' AND interest.request_id = analysis_requests.request_id
              AND interest.state = 'pending'
              AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract(NEW.request_json, '$.now_ms'))
              AND (interest.retry_deadline_ms = 0 OR interest.retry_deadline_ms > json_extract(NEW.request_json, '$.now_ms')))
        AND result_cache_key = json_extract(NEW.request_json, '$.artifact.cache_key')
        AND json_extract(NEW.result_json, '$.outcome') = 'published'
        AND (force_rebuild = 1 OR expected_predecessor_generation = '')
        AND expected_predecessor_generation = COALESCE((SELECT generation_cache_key FROM cluster_fragment_index_heads
            WHERE logical_cache_key = json_extract(NEW.request_json, '$.logical_key')), '')
        AND NOT EXISTS (SELECT 1 FROM analysis_requests newer
            WHERE newer.file_id = analysis_requests.file_id AND newer.source_size = analysis_requests.source_size
                AND newer.source_mtime = analysis_requests.source_mtime AND newer.component = 'fragment_index'
                AND newer.target_node_id = analysis_requests.target_node_id
                AND (newer.created_at_ms > analysis_requests.created_at_ms OR
                    (newer.created_at_ms = analysis_requests.created_at_ms AND newer.request_id > analysis_requests.request_id))
                AND newer.state IN ('queued','running','submitted','ready'))
    ORDER BY created_at_ms DESC, request_id DESC LIMIT 1
    ON CONFLICT(logical_cache_key) DO UPDATE SET generation_cache_key = excluded.generation_cache_key,
        request_id = excluded.request_id, updated_at_ms = excluded.updated_at_ms
    WHERE cluster_fragment_index_heads.generation_cache_key = (
        SELECT expected_predecessor_generation FROM analysis_requests WHERE request_id = excluded.request_id);

    INSERT INTO cluster_fragment_index_heads (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
    SELECT json_extract(NEW.request_json, '$.logical_key'), json_extract(NEW.request_json, '$.artifact.cache_key'),
        '', json_extract(NEW.request_json, '$.now_ms')
    WHERE json_extract(NEW.result_json, '$.outcome') = 'published'
        AND json_extract(NEW.request_json, '$.logical_key') = json_extract(NEW.request_json, '$.artifact.cache_key')
    ON CONFLICT(logical_cache_key) DO NOTHING;

    UPDATE background_jobs SET state = 'succeeded', result_ref = json_extract(NEW.result_json, '$.result_ref'),
        owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id = json_extract(NEW.result_json, '$.job_id') AND json_extract(NEW.result_json, '$.outcome') = 'published';

    UPDATE background_job_waiters SET state = CASE
        WHEN deadline_ms <= json_extract(NEW.request_json, '$.now_ms') THEN 'cancelled'
        WHEN retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms') THEN 'failed'
        WHEN target_node_id IS NULL
            OR target_node_id = json_extract(NEW.request_json, '$.token.node_id') THEN 'succeeded' ELSE 'awaiting_hydration' END,
        last_error_code = CASE WHEN retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms')
            THEN 'index_retry_window_expired' ELSE last_error_code END,
        result_ref = json_extract(NEW.result_json, '$.result_ref'), updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE job_id = json_extract(NEW.result_json, '$.job_id') AND state = 'pending'
        AND json_extract(NEW.result_json, '$.outcome') = 'published';

    -- A target receipt is completed only by publication of verified bytes on
    -- that target. The original build's awaiting waiters are the durable
    -- delivery intent, including when scheduling was interrupted by a crash.
    UPDATE background_job_waiters SET state = 'succeeded', result_ref = json_extract(NEW.result_json, '$.result_ref'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE state = 'awaiting_hydration' AND (deadline_ms IS NULL OR deadline_ms > json_extract(NEW.request_json, '$.now_ms'))
        AND target_node_id = json_extract(NEW.request_json, '$.token.node_id')
        AND CASE WHEN json_valid(result_ref) THEN json_extract(result_ref, '$.artifact_kind') END = 'fragment_index'
        AND CASE WHEN json_valid(result_ref) THEN json_extract(result_ref, '$.artifact_key') END = json_extract(NEW.request_json, '$.artifact.cache_key')
        AND json_extract(NEW.result_json, '$.outcome') = 'published';

    UPDATE cluster_fragment_index_jobs SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
        last_error_code = NULL, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE cache_key = json_extract(NEW.request_json, '$.artifact.cache_key')
        AND target_node_id = json_extract(NEW.request_json, '$.token.node_id')
        AND state IN ('queued','running') AND json_extract(NEW.result_json, '$.outcome') = 'published';

    UPDATE analysis_attempts SET phase = 'published', terminal_code = NULL,
        phase_updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE (request_id, claim_epoch) IN (SELECT request_id, fence FROM analysis_requests
        WHERE state = 'submitted' AND component = 'fragment_index'
            AND result_cache_key = json_extract(NEW.request_json, '$.artifact.cache_key')
            AND target_node_id = json_extract(NEW.request_json, '$.token.node_id'))
        AND EXISTS (SELECT 1 FROM cluster_fragment_index_heads
            WHERE generation_cache_key = json_extract(NEW.request_json, '$.artifact.cache_key'))
        AND json_extract(NEW.result_json, '$.outcome') = 'published';
    UPDATE analysis_requests SET state = 'ready', last_error_code = NULL,
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE state = 'submitted' AND component = 'fragment_index'
        AND result_cache_key = json_extract(NEW.request_json, '$.artifact.cache_key')
        AND target_node_id = json_extract(NEW.request_json, '$.token.node_id')
        AND EXISTS (SELECT 1 FROM cluster_fragment_index_heads
            WHERE generation_cache_key = json_extract(NEW.request_json, '$.artifact.cache_key'))
        AND json_extract(NEW.result_json, '$.outcome') = 'published';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_fragment_domain_projection
AFTER UPDATE ON background_jobs
WHEN EXISTS (SELECT 1 FROM background_fragment_targets WHERE job_id = NEW.id)
BEGIN
    UPDATE cluster_fragment_index_jobs SET
        state = CASE
            WHEN NEW.state = 'succeeded' THEN CASE WHEN target_node_id = json_extract(NEW.result_ref, '$.node_id')
                OR target_node_id = '' THEN 'ready' ELSE 'queued' END
            WHEN NOT EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id AND interest.state IN ('pending','awaiting_hydration'))
                THEN CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id AND interest.state = 'failed') THEN 'failed' ELSE 'cancelled' END
            WHEN NEW.state = 'running' AND NEW.kind = 'fragment_index_build' AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id
                AND interest.state = 'pending' AND interest.participation_fence = NEW.fence) THEN 'queued'
            WHEN NEW.state = 'cancelling' THEN 'cancelled' ELSE NEW.state END,
        owner_node_id = CASE WHEN NEW.state = 'running' AND (NEW.kind != 'fragment_index_build' OR EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id
                AND interest.state = 'pending' AND interest.participation_fence = NEW.fence)) THEN NEW.owner_node_id ELSE NULL END,
        lease_expires_ms = CASE WHEN NEW.state = 'running' AND (NEW.kind != 'fragment_index_build' OR EXISTS (SELECT 1 FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id
                AND interest.state = 'pending' AND interest.participation_fence = NEW.fence)) THEN NEW.lease_expires_ms ELSE NULL END,
        fence = NEW.fence,
        attempts = CASE WHEN NEW.kind = 'fragment_index_build' THEN COALESCE((SELECT MAX(interest.failed_attempts
            + CASE WHEN NEW.state = 'running' AND interest.state = 'pending'
                AND interest.participation_fence = NEW.fence THEN 1 ELSE 0 END)
            FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id), attempts)
            WHEN NEW.state = 'running' AND (OLD.state != 'running' OR NEW.fence > OLD.fence) THEN attempts + 1
            WHEN OLD.state = 'running' AND NEW.state = 'queued' AND NEW.failed_attempts = OLD.failed_attempts THEN MAX(0, attempts - 1)
            ELSE attempts END,
        attempt_errors = CASE WHEN NEW.kind = 'fragment_index_build'
            THEN COALESCE((SELECT interest.attempt_errors FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id
                ORDER BY interest.failed_attempts DESC, interest.updated_at_ms DESC, interest.request_scope, interest.request_id LIMIT 1), attempt_errors)
            WHEN NEW.failed_attempts > OLD.failed_attempts THEN attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END
                || COALESCE(NEW.last_error_code, 'execution_abandoned') ELSE attempt_errors END,
        index_retry_deadline_ms = CASE WHEN NEW.kind = 'fragment_index_build'
            THEN COALESCE((SELECT interest.retry_deadline_ms FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id
                ORDER BY interest.failed_attempts DESC, interest.updated_at_ms DESC, interest.request_scope, interest.request_id LIMIT 1), index_retry_deadline_ms)
            WHEN index_retry_deadline_ms > 0 THEN index_retry_deadline_ms ELSE NEW.retry_deadline_ms END,
        index_diagnostic_json = CASE WHEN NEW.kind = 'fragment_index_build'
            THEN COALESCE((SELECT interest.index_diagnostic_json FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id
                ORDER BY interest.failed_attempts DESC, interest.updated_at_ms DESC, interest.request_scope, interest.request_id LIMIT 1), index_diagnostic_json)
            WHEN NEW.index_diagnostic_json != '' THEN NEW.index_diagnostic_json ELSE index_diagnostic_json END,
        not_before_ms = COALESCE((SELECT MIN(interest.not_before_ms) FROM background_job_waiters interest
            WHERE interest.job_id = NEW.id AND interest.state = 'pending'
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id), NEW.not_before_ms),
        last_error_code = CASE WHEN NEW.state = 'succeeded'
            AND target_node_id != json_extract(NEW.result_ref, '$.node_id') AND target_node_id != '' THEN 'awaiting_hydration'
            WHEN NEW.kind = 'fragment_index_build' THEN (SELECT interest.last_error_code FROM background_job_waiters interest WHERE interest.job_id = NEW.id
                AND COALESCE(interest.target_node_id, '') = cluster_fragment_index_jobs.target_node_id
                ORDER BY interest.failed_attempts DESC, interest.updated_at_ms DESC, interest.request_scope, interest.request_id LIMIT 1)
            ELSE NEW.last_error_code END,
        updated_at_ms = NEW.updated_at_ms
    WHERE (cache_key, target_node_id) IN (SELECT cache_key, target_node_id FROM background_fragment_targets WHERE job_id = NEW.id);

END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_history_removed
AFTER DELETE ON analysis_requests
BEGIN
    -- Compact identities live as long as their domain history, never forever
    -- merely because an old request once opted out of seven-day pruning.
    DELETE FROM background_job_waiters WHERE request_scope = 'analysis' AND request_id = OLD.request_id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_interest_removed
AFTER DELETE ON background_job_waiters
BEGIN
    UPDATE background_jobs SET state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
        revision = revision + 1, updated_at_ms = MAX(updated_at_ms, OLD.updated_at_ms)
    WHERE id = OLD.job_id AND state IN ('queued','running') AND revision < 9223372036854775807
        AND NOT EXISTS (SELECT 1 FROM background_job_waiters WHERE job_id = OLD.job_id
            AND state IN ('pending','awaiting_hydration'));
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_cancelled
AFTER UPDATE ON analysis_requests
WHEN NEW.component = 'fragment_index' AND (NEW.state = 'cancelled' OR NEW.cancel_requested = 1)
    AND (OLD.state != 'cancelled' AND OLD.cancel_requested = 0)
BEGIN
    UPDATE background_job_legacy SET state = 'cancelled', outcome = 'request_cancelled', updated_at_ms = NEW.updated_at_ms
    WHERE legacy_key = 'analysis:' || NEW.request_id AND state = 'awaiting_import';
    INSERT INTO background_job_commands (id, operation, request_json, result_json)
    SELECT 'analysis-cancel:' || NEW.request_id, 'cancel_waiter',
        json_object('scope', 'analysis', 'request_id', NEW.request_id, 'now_ms', NEW.updated_at_ms),
        json_object('job_id', job_id, 'cancelled', json('true'))
    FROM background_job_waiters WHERE request_scope = 'analysis' AND request_id = NEW.request_id
        AND state IN ('pending','awaiting_hydration');
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_fragment_interest_failed
AFTER UPDATE ON background_jobs
WHEN NEW.kind = 'fragment_index_build' AND OLD.state = 'running' AND NEW.state = 'queued'
    AND NEW.failed_attempts > OLD.failed_attempts
BEGIN
    UPDATE background_job_waiters SET
        failed_attempts = failed_attempts + 1,
        attempt_errors = attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END ||
            CASE WHEN NEW.failure_policy = 'index_retry' OR NEW.index_diagnostic_json != OLD.index_diagnostic_json
                THEN json_extract(NEW.index_diagnostic_json, '$.code') ELSE NEW.last_error_code END,
        retry_deadline_ms = CASE WHEN NEW.failure_policy = 'index_retry' AND retry_deadline_ms = 0
            THEN MIN(9223372036854775807 - 604800000, NEW.updated_at_ms) + 604800000 ELSE retry_deadline_ms END,
        not_before_ms = CASE WHEN NEW.failure_policy = 'index_retry'
            THEN MIN(9223372036854775807 - MIN(1800000 * (1 << MIN(failed_attempts, 30)), 86400000), NEW.updated_at_ms)
                + MIN(1800000 * (1 << MIN(failed_attempts, 30)), 86400000)
            ELSE NEW.not_before_ms END,
        last_error_code = NEW.last_error_code,
        index_diagnostic_json = CASE WHEN NEW.index_diagnostic_json != OLD.index_diagnostic_json
            THEN json_set(NEW.index_diagnostic_json, '$.attempt', failed_attempts + 1)
            ELSE index_diagnostic_json END,
        updated_at_ms = NEW.updated_at_ms
    WHERE job_id = NEW.id AND state = 'pending' AND participation_fence = OLD.fence;

    UPDATE background_job_waiters SET state = 'failed',
        last_error_code = CASE WHEN failed_attempts >= attempt_limit THEN 'attempt_limit'
            WHEN retry_deadline_ms > 0 AND not_before_ms >= retry_deadline_ms THEN 'index_retry_window_expired'
            ELSE last_error_code END,
        updated_at_ms = NEW.updated_at_ms
    WHERE job_id = NEW.id AND state = 'pending' AND (
        NEW.failure_policy = 'terminal' OR failed_attempts >= attempt_limit
        OR (retry_deadline_ms > 0 AND not_before_ms >= retry_deadline_ms));

    UPDATE background_job_waiters SET index_diagnostic_json = json_set(index_diagnostic_json,
        '$.retryable', json(CASE WHEN state = 'pending' THEN 'true' ELSE 'false' END))
    WHERE job_id = NEW.id AND participation_fence = OLD.fence AND json_valid(index_diagnostic_json)
        AND updated_at_ms = NEW.updated_at_ms;

    UPDATE background_jobs SET
        state = CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = NEW.id AND state = 'pending') THEN 'queued' ELSE 'failed' END,
        index_diagnostic_json = CASE WHEN json_valid(NEW.index_diagnostic_json) THEN json_set(NEW.index_diagnostic_json,
            '$.retryable', json(CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters
                WHERE job_id = NEW.id AND state = 'pending') THEN 'true' ELSE 'false' END)) ELSE NEW.index_diagnostic_json END,
        not_before_ms = COALESCE((SELECT MIN(not_before_ms) FROM background_job_waiters
            WHERE job_id = NEW.id AND state = 'pending'), NEW.not_before_ms),
        retry_deadline_ms = CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = NEW.id AND state = 'pending' AND retry_deadline_ms = 0) THEN 0
            ELSE COALESCE((SELECT MAX(retry_deadline_ms) FROM background_job_waiters WHERE job_id = NEW.id), 0) END,
        last_error_code = CASE WHEN NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = NEW.id AND state = 'pending') THEN COALESCE((SELECT last_error_code
                FROM background_job_waiters WHERE job_id = NEW.id AND state = 'failed'
                ORDER BY updated_at_ms DESC, request_scope, request_id LIMIT 1), NEW.last_error_code)
            ELSE NEW.last_error_code END
    WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_interest_terminal
AFTER UPDATE OF state ON background_job_waiters
WHEN NEW.request_scope = 'analysis' AND NEW.state IN ('failed','cancelled') AND OLD.state != NEW.state
BEGIN
    UPDATE analysis_attempts SET phase = CASE NEW.state WHEN 'failed' THEN 'failed' ELSE 'canceled' END,
        terminal_code = NEW.last_error_code, phase_updated_at_ms = NEW.updated_at_ms
    WHERE (request_id, claim_epoch) IN (SELECT request_id, fence FROM analysis_requests
        WHERE request_id = NEW.request_id AND state = 'submitted');
    UPDATE analysis_requests SET state = NEW.state, last_error_code = NEW.last_error_code,
        updated_at_ms = NEW.updated_at_ms
    WHERE request_id = NEW.request_id AND state = 'submitted';
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_fragment_last_abandonment
AFTER UPDATE ON background_jobs
WHEN NEW.kind = 'fragment_index_build' AND OLD.state = 'running' AND NEW.state = 'failed'
    AND NEW.last_error_code = 'execution_abandoned' AND NEW.failed_attempts > OLD.failed_attempts
BEGIN
    UPDATE background_job_waiters SET failed_attempts = failed_attempts + 1,
        attempt_errors = attempt_errors || CASE WHEN attempt_errors = '' THEN '' ELSE ',' END || 'lease_expired',
        last_error_code = CASE WHEN failed_attempts + 1 >= attempt_limit THEN 'attempt_limit' ELSE 'lease_expired' END,
        state = 'failed', updated_at_ms = NEW.updated_at_ms
    WHERE job_id = NEW.id AND state = 'pending' AND participation_fence = OLD.fence;
    UPDATE background_jobs SET priority = priority WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_fragment_interest_retired
AFTER UPDATE OF state ON background_job_waiters
WHEN OLD.state = 'pending' AND NEW.state IN ('failed','cancelled')
BEGIN
    UPDATE background_jobs SET
        not_before_ms = COALESCE((SELECT MIN(not_before_ms) FROM background_job_waiters
            WHERE job_id = NEW.job_id AND state = 'pending'), not_before_ms),
        retry_deadline_ms = CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = NEW.job_id AND state = 'pending' AND retry_deadline_ms = 0) THEN 0
            ELSE COALESCE((SELECT MAX(retry_deadline_ms) FROM background_job_waiters
                WHERE job_id = NEW.job_id AND state = 'pending'), retry_deadline_ms) END
    WHERE id = NEW.job_id AND kind = 'fragment_index_build' AND state IN ('queued','running');
END;

-- A one-time sealed inbox preserves accepted legacy work beyond live queue
-- capacity. The schema transaction captures source rows before workers start.
-- next statement
CREATE TABLE IF NOT EXISTS background_job_migration (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    source_count INTEGER NOT NULL CHECK (source_count >= 0)
) STRICT;
-- next statement
CREATE TABLE IF NOT EXISTS background_job_legacy (
    legacy_key TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    snapshot_json TEXT NOT NULL CHECK (json_valid(snapshot_json)),
    state TEXT NOT NULL CHECK (state IN ('awaiting_import','materialized','failed','cancelled')),
    job_id TEXT,
    outcome TEXT,
    updated_at_ms INTEGER NOT NULL DEFAULT 0
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS background_job_legacy_pending ON background_job_legacy(state, legacy_key);
-- next statement
INSERT INTO background_job_legacy (legacy_key, kind, snapshot_json, state)
SELECT 'transcode:' || id, 'transcode_prepare', json_object(
    'id', id, 'dedupe_key', dedupe_key, 'file_id', file_id, 'source_size', source_size,
    'source_mtime', source_mtime, 'target_height', target_height, 'policy_generation', policy_generation,
    'requirements_json', requirements_json, 'reason', reason, 'priority', priority,
    'state', state, 'attempts', attempts, 'not_before_ms', not_before_ms,
    'last_error_code', last_error_code, 'created_at_ms', created_at_ms, 'staging_node_id', staging_node_id),
    'awaiting_import'
FROM pretranscode_jobs WHERE state IN ('queued','running')
    AND NOT EXISTS (SELECT 1 FROM background_job_migration) ON CONFLICT DO NOTHING;
-- next statement
INSERT INTO background_job_legacy (legacy_key, kind, snapshot_json, state)
SELECT 'fragment:' || json_array(cache_key, target_node_id), 'fragment_index_build', json_object(
    'cache_key', cache_key, 'file_id', file_id, 'source_size', source_size, 'source_mtime', source_mtime,
    'source_sha256', source_sha256, 'pipeline_sha256', pipeline_sha256, 'priority', priority,
    'trigger', trigger, 'target_node_id', target_node_id, 'state', state, 'attempts', attempts,
    'not_before_ms', not_before_ms, 'created_at_ms', created_at_ms, 'last_error_code', last_error_code,
    'attempt_errors', attempt_errors, 'index_retry_deadline_ms', index_retry_deadline_ms,
    'index_diagnostic_json', index_diagnostic_json,
    'attempt_limit', MAX(1, MIN(20, COALESCE((SELECT CASE WHEN trim(value) GLOB '[0-9]*' THEN CAST(value AS INTEGER) ELSE 5 END
        FROM settings WHERE key = 'analysis.max_attempts'), 5)))), 'awaiting_import'
FROM cluster_fragment_index_jobs WHERE state IN ('queued','running')
    AND NOT EXISTS (SELECT 1 FROM background_job_migration) ON CONFLICT DO NOTHING;
-- next statement
INSERT INTO background_job_legacy (legacy_key, kind, snapshot_json, state)
SELECT 'analysis:' || request.request_id, 'fragment_index_build',
    json_set(legacy.snapshot_json, '$.analysis_request_id', request.request_id), 'awaiting_import'
FROM analysis_requests request JOIN background_job_legacy legacy
    ON legacy.legacy_key = 'fragment:' || json_array(request.result_cache_key, request.target_node_id)
WHERE request.state = 'submitted' AND request.component = 'fragment_index'
    AND NOT EXISTS (SELECT 1 FROM background_job_migration) ON CONFLICT DO NOTHING;
-- next statement
DELETE FROM background_job_legacy WHERE kind = 'fragment_index_build'
    AND legacy_key LIKE 'fragment:%' AND NOT EXISTS (SELECT 1 FROM background_job_migration)
    AND EXISTS (SELECT 1 FROM analysis_requests request WHERE request.state = 'submitted'
        AND request.component = 'fragment_index' AND request.result_cache_key = json_extract(background_job_legacy.snapshot_json, '$.cache_key')
        AND request.target_node_id = json_extract(background_job_legacy.snapshot_json, '$.target_node_id'));
-- next statement
INSERT INTO background_job_migration (singleton, format_version, source_count)
SELECT 1, 1, COUNT(*) FROM background_job_legacy WHERE true ON CONFLICT DO NOTHING;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_legacy_reject_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'legacy_reject'
BEGIN
    UPDATE background_job_legacy SET state = 'failed', outcome = json_extract(NEW.request_json, '$.code'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE legacy_key = json_extract(NEW.request_json, '$.legacy_key') AND state = 'awaiting_import';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_legacy_terminal
AFTER UPDATE OF state ON background_job_legacy WHEN NEW.state IN ('failed','cancelled') AND OLD.state = 'awaiting_import'
BEGIN
    UPDATE analysis_requests SET state = NEW.state, last_error_code = NEW.outcome, updated_at_ms = NEW.updated_at_ms
    WHERE request_id = json_extract(NEW.snapshot_json, '$.analysis_request_id') AND state = 'submitted';
    UPDATE cluster_fragment_index_jobs SET state = NEW.state, owner_node_id = NULL, lease_expires_ms = NULL,
        last_error_code = NEW.outcome, updated_at_ms = NEW.updated_at_ms
    WHERE NEW.kind = 'fragment_index_build' AND cache_key = json_extract(NEW.snapshot_json, '$.cache_key')
        AND target_node_id = json_extract(NEW.snapshot_json, '$.target_node_id') AND state IN ('queued','running')
        AND NOT EXISTS (SELECT 1 FROM background_fragment_targets shared
            WHERE shared.cache_key = cluster_fragment_index_jobs.cache_key AND shared.target_node_id = cluster_fragment_index_jobs.target_node_id);
    UPDATE pretranscode_jobs SET state = NEW.state, owner_node_id = NULL, lease_expires_ms = NULL,
        last_error_code = NEW.outcome, updated_at_ms = NEW.updated_at_ms
    WHERE NEW.kind = 'transcode_prepare' AND id = json_extract(NEW.snapshot_json, '$.id') AND state IN ('queued','running');
END;

-- The old transcode row is retained only as history and a staging reference.
-- Common ownership is the sole execution authority after import.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_legacy_transcode_projection
AFTER UPDATE ON background_jobs WHEN NEW.kind = 'transcode_prepare'
BEGIN
    UPDATE pretranscode_jobs SET state = CASE NEW.state WHEN 'succeeded' THEN 'ready'
        WHEN 'cancelling' THEN 'running' ELSE NEW.state END,
        owner_node_id = NEW.owner_node_id, lease_expires_ms = NEW.lease_expires_ms,
        fence = NEW.fence, attempts = NEW.failed_attempts, not_before_ms = NEW.not_before_ms,
        last_error_code = NEW.last_error_code, updated_at_ms = NEW.updated_at_ms
    WHERE id = NEW.id;
END;
