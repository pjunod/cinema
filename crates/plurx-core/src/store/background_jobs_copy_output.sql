-- Copy output preparation shares the existing source-revision cancellation
-- contract. Replace the two guards on upgraded stores as well as fresh ones;
-- CREATE IF NOT EXISTS in the original queue schema cannot upgrade a trigger.
CREATE TRIGGER IF NOT EXISTS background_job_copy_output_target
AFTER INSERT ON background_jobs WHEN NEW.kind='copy_output_prepare'
BEGIN
    UPDATE background_jobs SET target_node_id=json_extract(NEW.payload_json,'$.intent.target_node_id')
    WHERE id=NEW.id;
END;
-- next statement
DROP TRIGGER IF EXISTS background_job_source_changed;
-- next statement
CREATE TRIGGER background_job_source_changed
AFTER UPDATE OF size, mtime ON files WHEN NEW.size != OLD.size OR NEW.mtime != OLD.mtime
BEGIN
    UPDATE background_jobs SET state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
        last_error_code = 'source_changed', revision = revision + 1
    WHERE kind IN ('transcode_prepare','fragment_index_build','copy_output_prepare') AND state IN ('queued','running')
        AND json_extract(payload_json, '$.file_id') = OLD.id AND revision < 9223372036854775807
        AND (json_extract(payload_json, '$.source_size') != NEW.size
            OR json_extract(payload_json, '$.source_mtime') != NEW.mtime);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_publish_copy_output_command
AFTER INSERT ON background_job_commands WHEN NEW.operation='publish_copy_output'
BEGIN
    UPDATE background_jobs SET state='succeeded', result_ref=json_extract(NEW.result_json,'$.result_ref'),
        owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL,
        revision=revision+1,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE id=json_extract(NEW.result_json,'$.job_id') AND json_extract(NEW.result_json,'$.outcome')='published';
    UPDATE background_job_waiters SET state=CASE
        WHEN deadline_ms<=json_extract(NEW.request_json,'$.now_ms') THEN 'cancelled'
        WHEN target_node_id IS NULL OR target_node_id=json_extract(NEW.request_json,'$.token.node_id') THEN 'succeeded'
        ELSE 'cancelled' END,
        result_ref=json_extract(NEW.result_json,'$.result_ref'),updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE job_id=json_extract(NEW.result_json,'$.job_id') AND state='pending'
        AND json_extract(NEW.result_json,'$.outcome')='published';
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
DROP TRIGGER IF EXISTS background_job_source_deleted;
-- next statement
CREATE TRIGGER background_job_source_deleted
AFTER DELETE ON files
BEGIN
    UPDATE background_jobs SET state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
        last_error_code = 'source_changed', revision = revision + 1
    WHERE kind IN ('transcode_prepare','fragment_index_build','copy_output_prepare') AND state IN ('queued','running')
        AND json_extract(payload_json, '$.file_id') = OLD.id AND revision < 9223372036854775807;
END;
