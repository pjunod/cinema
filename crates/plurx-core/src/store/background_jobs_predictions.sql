CREATE TABLE IF NOT EXISTS background_predictions (
    request_id TEXT PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    request_json TEXT NOT NULL CHECK(json_valid(request_json)),
    expires_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','retired','adopted','settled')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
-- next statement
CREATE INDEX IF NOT EXISTS background_predictions_expiry ON background_predictions(state, expires_ms);
-- next statement
CREATE TRIGGER IF NOT EXISTS background_prediction_retired
AFTER UPDATE OF state ON background_predictions WHEN OLD.state = 'pending' AND NEW.state = 'retired'
BEGIN
    UPDATE analysis_requests SET state = 'cancelled', cancel_requested = 1,
        last_error_code = 'prediction_expired', updated_at_ms = NEW.updated_at_ms
    WHERE request_id = NEW.request_id AND state IN ('queued','running','submitted');
    INSERT INTO background_job_commands(id,operation,request_json,result_json)
    SELECT 'prediction-cancel:' || NEW.request_id, 'cancel_waiter',
        json_object('scope',request_scope,'request_id',request_id,'now_ms',NEW.updated_at_ms),
        json_object('job_id',job_id,'cancelled',json('true'))
    FROM background_job_waiters WHERE request_scope IN ('analysis','subtitle')
        AND request_id = NEW.request_id AND state IN ('pending','awaiting_hydration');
END;
-- next statement
-- A real subtitle request adopts an in-flight predictive extraction in the
-- same transaction as admission. It becomes ordinary demand, with no deadline.
CREATE TRIGGER IF NOT EXISTS background_prediction_adopted
AFTER UPDATE OF state ON background_predictions WHEN OLD.state = 'pending' AND NEW.state = 'adopted'
BEGIN
    UPDATE background_job_waiters SET deadline_ms = NULL
    WHERE request_scope = 'subtitle' AND request_id = NEW.request_id AND state = 'pending';
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_prediction_upkeep
BEFORE INSERT ON background_job_commands WHEN NEW.operation = 'maintain'
BEGIN
    UPDATE background_predictions SET state = 'retired', updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE state = 'pending' AND expires_ms <= json_extract(NEW.request_json,'$.now_ms');
    DELETE FROM background_predictions WHERE request_id IN (
        SELECT request_id FROM background_predictions
        WHERE state <> 'pending' AND updated_at_ms <= json_extract(NEW.request_json,'$.now_ms') - 604800000
        ORDER BY updated_at_ms, request_id LIMIT 128);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_predictions_sync
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'sync_predictions'
BEGIN
    UPDATE background_predictions SET state = 'retired', updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE state = 'pending' AND (expires_ms <= json_extract(NEW.request_json,'$.now_ms')
        OR NOT EXISTS (SELECT 1 FROM files WHERE id = background_predictions.file_id
            AND size = json_extract(background_predictions.request_json,'$.source_size')
            AND mtime = json_extract(background_predictions.request_json,'$.source_mtime')) OR NOT EXISTS (
        SELECT 1 FROM json_each(NEW.request_json,'$.requests') wanted
        WHERE json_extract(wanted.value,'$.request_id') = background_predictions.request_id));

    INSERT INTO background_predictions(request_id,file_id,request_json,expires_ms,state,created_at_ms,updated_at_ms)
    SELECT json_extract(wanted.value,'$.request_id'), json_extract(wanted.value,'$.file_id'), wanted.value,
        json_extract(NEW.request_json,'$.now_ms') + 86400000, 'pending',
        json_extract(NEW.request_json,'$.now_ms'), json_extract(NEW.request_json,'$.now_ms')
    FROM json_each(NEW.request_json,'$.requests') wanted
    WHERE EXISTS (SELECT 1 FROM files WHERE id = json_extract(wanted.value,'$.file_id')
        AND size = json_extract(wanted.value,'$.source_size') AND mtime = json_extract(wanted.value,'$.source_mtime'))
    AND NOT EXISTS (SELECT 1 FROM background_predictions prior WHERE prior.request_id = json_extract(wanted.value,'$.request_id'))
    AND NOT EXISTS (SELECT 1 FROM analysis_requests prior WHERE prior.request_id = json_extract(wanted.value,'$.request_id'))
    AND NOT EXISTS (SELECT 1 FROM analysis_requests prior
        WHERE prior.file_id = json_extract(wanted.value,'$.file_id')
          AND prior.source_size = json_extract(wanted.value,'$.source_size') AND prior.source_mtime = json_extract(wanted.value,'$.source_mtime')
          AND prior.component = json_extract(wanted.value,'$.component') AND prior.pipeline_version = json_extract(wanted.value,'$.pipeline_version')
          AND prior.state IN ('queued','running','submitted','ready'))
    LIMIT MAX(0, MIN(512 - (SELECT COUNT(*) FROM background_predictions),
        64 - (SELECT COUNT(*) FROM background_predictions WHERE state = 'pending')
           - (SELECT COUNT(*) FROM background_job_waiters WHERE request_scope IN ('automatic:transcode','automatic:transcode-repair','automatic:hot-copy')
               AND state IN ('pending','awaiting_hydration') AND (deadline_ms IS NULL OR deadline_ms > json_extract(NEW.request_json,'$.now_ms')))))
    ON CONFLICT DO NOTHING;

    -- Cancel only the automatic encoding interest. Offline/foreground waiters
    -- on the same computation continue to own their work independently.
    INSERT INTO background_job_commands(id,operation,request_json,result_json)
    SELECT 'prediction-transcode:' || waiter.request_id, 'cancel_waiter',
        json_object('scope',waiter.request_scope,'request_id',waiter.request_id,'now_ms',json_extract(NEW.request_json,'$.now_ms')),
        json_object('job_id',waiter.job_id,'cancelled',json('true'))
    FROM background_job_waiters waiter JOIN background_jobs job ON job.id = waiter.job_id
    WHERE waiter.request_scope IN ('automatic:transcode','automatic:transcode-repair')
      AND waiter.state = 'pending' AND NOT EXISTS (SELECT 1 FROM json_each(NEW.request_json,'$.desired_files') wanted
        WHERE wanted.value = json_extract(job.payload_json,'$.file_id'));

    UPDATE job_leases SET revision = json_extract(NEW.request_json,'$.replacement.revision'),
        expires_at_ms = json_extract(NEW.request_json,'$.replacement.expires_at_unix_ms')
    WHERE resource = json_extract(NEW.request_json,'$.lease.resource');
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_prediction_completed
AFTER UPDATE OF state ON analysis_requests WHEN NEW.state IN ('ready','failed','cancelled')
BEGIN
    UPDATE background_predictions SET state = 'settled', updated_at_ms = NEW.updated_at_ms
    WHERE request_id = NEW.request_id AND state = 'pending';
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_prediction_already_complete
AFTER INSERT ON analysis_requests WHEN NEW.state IN ('ready','failed','cancelled')
BEGIN
    UPDATE background_predictions SET state = 'settled', updated_at_ms = NEW.updated_at_ms
    WHERE request_id = NEW.request_id AND state = 'pending';
END;
