-- Preserve terminal domain history when retiring obsolete common work.
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
