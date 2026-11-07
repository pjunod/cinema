-- Pipeline contract v2 (`PIPELINE-CONTRACT.md` §5.1): the facts a
-- targeted scan request carries beyond its durable intent — the creating API
-- key, Curator's idempotency key, the files it expects, what the request is
-- waiting on, and the per-file report. A sidecar keyed by request id, never a
-- field of `background_library_requests.input_json`: that column decodes into
-- a `deny_unknown_fields` enum, so a new field there would make every older
-- binary in a rolling upgrade reject the row. An independently recognized
-- extension: it creates only its own objects, and an older binary that does
-- not know it neither writes nor reads it.
CREATE TABLE IF NOT EXISTS background_scan_request_v2_schema (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    version INTEGER NOT NULL CHECK (version = 1)
) STRICT;
-- next statement
CREATE TABLE IF NOT EXISTS background_scan_request_v2 (
    request_id TEXT PRIMARY KEY,
    key_id INTEGER NOT NULL,
    request_key TEXT NOT NULL DEFAULT '',
    expected_files_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(expected_files_json) AND length(CAST(expected_files_json AS BLOB)) <= 16384),
    waiting_on TEXT NOT NULL DEFAULT '' CHECK (length(CAST(waiting_on AS BLOB)) <= 1024),
    files_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(files_json) AND length(CAST(files_json AS BLOB)) <= 65536),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE UNIQUE INDEX IF NOT EXISTS background_scan_request_v2_key
    ON background_scan_request_v2 (key_id, request_key) WHERE request_key != '';
-- The sidecar is written in the admission transaction, from the same
-- ephemeral enqueue command that creates the request row. A worker can claim
-- and finish a request between an enqueue and any later write, so a sidecar
-- written afterwards could miss the job it describes. A `(key_id,
-- request_key)` collision aborts the whole admission: a concurrent identical
-- request already won, and the caller answers with that one.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_scan_request_v2_admitted
AFTER INSERT ON background_library_requests
WHEN EXISTS (SELECT 1 FROM background_job_commands command WHERE command.operation = 'enqueue'
    AND json_extract(command.request_json, '$.request.scope') = 'library'
    AND json_extract(command.request_json, '$.request.request_id') = NEW.request_id
    AND json_type(command.request_json, '$.scan_request_v2') = 'object')
BEGIN
    INSERT INTO background_scan_request_v2
        (request_id, key_id, request_key, expected_files_json, created_at_ms, updated_at_ms)
    SELECT NEW.request_id, json_extract(command.request_json, '$.scan_request_v2.key_id'),
        json_extract(command.request_json, '$.scan_request_v2.request_key'),
        json_extract(command.request_json, '$.scan_request_v2.expected_files'),
        json_extract(command.request_json, '$.now_ms'), json_extract(command.request_json, '$.now_ms')
    FROM background_job_commands command
    WHERE command.operation = 'enqueue'
        AND json_extract(command.request_json, '$.request.scope') = 'library'
        AND json_extract(command.request_json, '$.request.request_id') = NEW.request_id
    -- One enqueue command names one request; the order only makes LIMIT 1
    -- deterministic should that ever stop being true.
    ORDER BY command.id
    LIMIT 1;
END;
-- A retired request takes its sidecar with it, whichever binary retired it.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_scan_request_v2_retired
AFTER DELETE ON background_library_requests
BEGIN
    DELETE FROM background_scan_request_v2 WHERE request_id = OLD.request_id;
END;
-- next statement
INSERT OR IGNORE INTO background_scan_request_v2_schema (singleton, version) VALUES (1, 1);
