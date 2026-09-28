-- Preserve already-spent retry allowance when draining the legacy outbox.
-- Backup imports have no admission command and copy the durable counts intact.
CREATE TRIGGER IF NOT EXISTS background_subtitle_admitted
AFTER INSERT ON background_job_waiters WHEN NEW.request_scope = 'subtitle'
    AND EXISTS (SELECT 1 FROM background_job_commands command WHERE command.operation = 'enqueue'
        AND json_extract(command.request_json, '$.request.scope') = NEW.request_scope
        AND json_extract(command.request_json, '$.request.request_id') = NEW.request_id)
BEGIN
    UPDATE background_jobs SET
        failed_attempts = MIN(attempt_limit, MAX(0, (SELECT attempts FROM analysis_requests WHERE request_id = NEW.request_id))),
        state = CASE WHEN (SELECT attempts FROM analysis_requests WHERE request_id = NEW.request_id) >= attempt_limit THEN 'failed' ELSE state END,
        last_error_code = CASE WHEN (SELECT attempts FROM analysis_requests WHERE request_id = NEW.request_id) >= attempt_limit THEN 'attempt_limit' ELSE last_error_code END
    WHERE id = NEW.job_id AND kind = 'subtitle_extract' AND state = 'queued' AND fence = 0;
END;
-- next statement
-- Subtitle demand/history keeps its public identity. Only common claims
-- project a worker into the compatibility record.
CREATE TRIGGER IF NOT EXISTS background_subtitle_claimed
AFTER UPDATE ON background_jobs
WHEN NEW.kind = 'subtitle_extract' AND NEW.state = 'running' AND NEW.fence > OLD.fence
BEGIN
    UPDATE analysis_attempts SET phase = 'retry_wait', terminal_code = 'lease_expired', phase_updated_at_ms = NEW.updated_at_ms
    WHERE request_id = json_extract(NEW.payload_json, '$.source_generation') AND claim_epoch = (
        SELECT fence FROM analysis_requests WHERE request_id = json_extract(NEW.payload_json, '$.source_generation'));
    UPDATE analysis_requests SET state = 'running', owner_node_id = NEW.owner_node_id,
        fence = fence + 1, lease_expires_ms = NEW.lease_expires_ms,
        attempts = attempts + 1, updated_at_ms = NEW.updated_at_ms, last_error_code = NULL
    WHERE request_id = json_extract(NEW.payload_json, '$.source_generation')
        AND component = 'subtitle_source' AND state IN ('queued','running') AND cancel_requested = 0
        AND EXISTS (SELECT 1 FROM files WHERE id = analysis_requests.file_id
            AND size = analysis_requests.source_size AND mtime = analysis_requests.source_mtime);
    SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM analysis_requests
        WHERE request_id = json_extract(NEW.payload_json, '$.source_generation')
            AND state = 'running' AND owner_node_id = NEW.owner_node_id AND lease_expires_ms = NEW.lease_expires_ms)
        THEN RAISE(ABORT, 'subtitle demand no longer claimable') END;
    INSERT INTO analysis_attempts(request_id, attempt, claim_node_id, claim_epoch, claim_expires_at_ms, phase, started_at_ms, phase_updated_at_ms)
    SELECT request_id, attempts, owner_node_id, fence, lease_expires_ms, 'claimed', NEW.updated_at_ms, NEW.updated_at_ms
    FROM analysis_requests WHERE request_id = json_extract(NEW.payload_json, '$.source_generation');
    DELETE FROM analysis_attempts WHERE request_id = json_extract(NEW.payload_json, '$.source_generation') AND claim_epoch NOT IN (
        SELECT claim_epoch FROM analysis_attempts WHERE request_id = json_extract(NEW.payload_json, '$.source_generation') ORDER BY claim_epoch DESC LIMIT 64);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_subtitle_renewed
AFTER UPDATE OF lease_expires_ms ON background_jobs
WHEN NEW.kind = 'subtitle_extract' AND NEW.state = 'running' AND NEW.fence = OLD.fence
BEGIN
    UPDATE analysis_requests SET lease_expires_ms = NEW.lease_expires_ms, updated_at_ms = NEW.updated_at_ms
    WHERE request_id = json_extract(NEW.payload_json, '$.source_generation') AND state = 'running' AND owner_node_id = NEW.owner_node_id;
END;
-- A job settled while its demand is already terminal — the zombie a refused
-- claim retires — must leave the demand's finished attempt row as it was.
-- The attempt update is guarded the same way the request update below is;
-- dropped and recreated because CREATE TRIGGER IF NOT EXISTS keeps the old body.
-- next statement
DROP TRIGGER IF EXISTS background_subtitle_settled;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_subtitle_settled
AFTER UPDATE OF state ON background_jobs
WHEN NEW.kind = 'subtitle_extract' AND NEW.state <> OLD.state AND NEW.state <> 'running'
BEGIN
    UPDATE analysis_attempts SET phase = CASE NEW.state WHEN 'queued' THEN 'retry_wait' WHEN 'succeeded' THEN 'published'
        WHEN 'failed' THEN 'failed' ELSE 'canceled' END,
        terminal_code = NEW.last_error_code, phase_updated_at_ms = NEW.updated_at_ms
    WHERE request_id = json_extract(NEW.payload_json, '$.source_generation') AND claim_epoch = (
        SELECT fence FROM analysis_requests WHERE request_id = json_extract(NEW.payload_json, '$.source_generation')
            AND state IN ('queued','running'));
    UPDATE analysis_requests SET state = CASE NEW.state WHEN 'queued' THEN 'queued' WHEN 'succeeded' THEN 'ready'
        WHEN 'failed' THEN 'failed' ELSE 'cancelled' END,
        owner_node_id = NULL, lease_expires_ms = NULL, not_before_ms = NEW.not_before_ms,
        result_cache_key = NEW.result_ref, last_error_code = NEW.last_error_code, updated_at_ms = NEW.updated_at_ms
    WHERE request_id = json_extract(NEW.payload_json, '$.source_generation') AND state IN ('queued','running');
END;
-- The old Activity cancellation endpoint cancels the same common job.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_subtitle_cancelled
AFTER UPDATE OF state ON analysis_requests
WHEN NEW.component = 'subtitle_source' AND NEW.state = 'cancelled' AND OLD.state IN ('queued','running','submitted')
BEGIN
    UPDATE background_jobs SET state = CASE state WHEN 'running' THEN 'cancelling' ELSE 'cancelled' END,
        revision = revision + 1, updated_at_ms = NEW.updated_at_ms, last_error_code = 'subtitle_request_cancelled'
    WHERE kind = 'subtitle_extract' AND json_extract(payload_json, '$.source_generation') = NEW.request_id AND state IN ('queued','running');
END;
-- Priority promotion also applies when the outbox was already admitted.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_subtitle_promoted
AFTER UPDATE OF priority ON analysis_requests
WHEN NEW.component = 'subtitle_source' AND NEW.priority = 'foreground'
BEGIN
    UPDATE background_jobs SET priority = 2, revision = revision + 1
    WHERE kind = 'subtitle_extract' AND json_extract(payload_json, '$.source_generation') = NEW.request_id AND state = 'queued' AND priority < 2;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_subtitle_output
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'subtitle_output'
BEGIN
    INSERT INTO subtitle_source_publications(file_id,source_size,source_mtime,source_attestation,node_id,ordinal,kind,format,verdict,attempts,origin,sha256,bytes,published_at_ms)
    SELECT json_extract(NEW.request_json, '$.output.publication.file_id'), json_extract(NEW.request_json, '$.output.publication.source_size'),
        json_extract(NEW.request_json, '$.output.publication.source_mtime'), json_extract(NEW.request_json, '$.output.publication.source_attestation'),
        json_extract(NEW.request_json, '$.output.publication.node_id'), json_extract(NEW.request_json, '$.output.publication.ordinal'),
        json_extract(NEW.request_json, '$.output.publication.kind'), json_extract(NEW.request_json, '$.output.publication.format'),
        json_extract(NEW.request_json, '$.output.publication.verdict'), json_extract(NEW.request_json, '$.output.publication.attempts'),
        json_extract(NEW.request_json, '$.output.publication.origin'), json_extract(NEW.request_json, '$.output.publication.sha256'),
        json_extract(NEW.request_json, '$.output.publication.bytes'), json_extract(NEW.request_json, '$.output.publication.published_at_ms')
    WHERE NEW.result_json = 'true' AND json_extract(NEW.request_json, '$.output.kind') = 'representation'
    ON CONFLICT(file_id,source_size,source_mtime,node_id,ordinal,format) DO UPDATE SET
        source_attestation=excluded.source_attestation,kind=excluded.kind,verdict=excluded.verdict,attempts=excluded.attempts,
        origin=excluded.origin,sha256=excluded.sha256,bytes=excluded.bytes,published_at_ms=excluded.published_at_ms;
    UPDATE background_jobs SET state = 'succeeded', result_ref = json_extract(NEW.request_json, '$.output.result_key'),
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms'),
        owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL
    WHERE id = json_extract(NEW.request_json, '$.token.job_id') AND state = 'running'
        AND NEW.result_json = 'true' AND json_extract(NEW.request_json, '$.output.kind') = 'complete';
    UPDATE background_job_waiters SET state = 'succeeded',
        result_ref = json_extract(NEW.request_json, '$.output.result_key'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE job_id = json_extract(NEW.request_json, '$.token.job_id') AND request_scope = 'subtitle'
        AND state = 'pending' AND NEW.result_json = 'true'
        AND json_extract(NEW.request_json, '$.output.kind') = 'complete';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
