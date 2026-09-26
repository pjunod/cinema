CREATE TABLE IF NOT EXISTS background_library_requests (
    request_id TEXT PRIMARY KEY,
    library_id INTEGER NOT NULL,
    job_id TEXT NOT NULL,
    input_json TEXT NOT NULL CHECK (json_valid(input_json) AND length(CAST(input_json AS BLOB)) <= 16384),
    result_json TEXT CHECK (result_json IS NULL OR (json_valid(result_json) AND length(CAST(result_json AS BLOB)) <= 65536)),
    completed_claim_id TEXT,
    completed_at_ms INTEGER
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS background_library_requests_job ON background_library_requests(job_id, request_id);
-- next statement
CREATE INDEX IF NOT EXISTS background_library_requests_library ON background_library_requests(library_id);
-- Capture intent inside admission, after its waiter is inserted and before
-- the ephemeral command is removed. An acknowledged request cannot be lost
-- between a queue write and a second domain write. Backup import has no
-- admission command and copies domain rows explicitly, with parity checks.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_library_request_admitted
AFTER INSERT ON background_job_waiters WHEN NEW.request_scope = 'library'
    AND EXISTS (SELECT 1 FROM background_job_commands command WHERE command.operation = 'enqueue'
        AND json_extract(command.request_json, '$.request.scope') = NEW.request_scope
        AND json_extract(command.request_json, '$.request.request_id') = NEW.request_id)
BEGIN
    INSERT INTO background_library_requests(request_id, library_id, job_id, input_json)
    SELECT NEW.request_id, json_extract(command.request_json, '$.payload.library_id'), NEW.job_id,
        json_extract(command.request_json, '$.library_request')
    FROM background_job_commands command
    WHERE command.operation = 'enqueue'
        AND json_extract(command.request_json, '$.request.scope') = NEW.request_scope
        AND json_extract(command.request_json, '$.request.request_id') = NEW.request_id;
    SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM background_library_requests WHERE request_id = NEW.request_id)
        THEN RAISE(ABORT, 'library waiter requires durable intent') END;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_library_request_retired
AFTER DELETE ON background_job_waiters WHEN OLD.request_scope = 'library'
BEGIN
    DELETE FROM background_library_requests WHERE request_id = OLD.request_id;
END;
-- A completion names exactly the processed request. A late arrival remains
-- pending; the job completes only if that same transaction sees none left.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_library_request_completed
AFTER UPDATE OF result_json ON background_library_requests
WHEN OLD.result_json IS NULL AND NEW.result_json IS NOT NULL
BEGIN
    UPDATE background_job_waiters SET
        state = CASE json_extract(NEW.result_json, '$.outcome') WHEN 'completed' THEN 'completed' ELSE 'failed' END,
        result_ref = 'library-request:' || NEW.request_id,
        last_error_code = CASE json_extract(NEW.result_json, '$.outcome') WHEN 'failed' THEN 'library_request_failed' END,
        updated_at_ms = NEW.completed_at_ms
    WHERE request_scope = 'library' AND request_id = NEW.request_id AND state = 'pending';
    UPDATE background_jobs SET state = 'succeeded', result_ref = 'library-work:' || id,
        updated_at_ms = NEW.completed_at_ms, revision = revision + 1,
        owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL
    WHERE id = NEW.job_id AND state = 'running' AND claim_id = NEW.completed_claim_id
        AND NOT EXISTS (SELECT 1 FROM background_job_waiters WHERE job_id = NEW.job_id AND state = 'pending');
END;
