-- Add exact encoded preparation to the existing source-revision lifecycle.
-- The queue kind is TEXT; old nodes can list/cancel unknown payloads but do
-- not execute the new closed kind. No second queue or hydration authority.
CREATE TRIGGER IF NOT EXISTS background_job_encoded_output_target
AFTER INSERT ON background_jobs WHEN NEW.kind='encoded_output_prepare'
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
    UPDATE background_jobs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancelling' END,
        last_error_code='source_changed', revision=revision+1
    WHERE kind IN ('transcode_prepare','fragment_index_build','copy_output_prepare','encoded_output_prepare')
        AND state IN ('queued','running') AND json_extract(payload_json,'$.file_id')=OLD.id
        AND revision<9223372036854775807
        AND (json_extract(payload_json,'$.source_size')!=NEW.size
            OR json_extract(payload_json,'$.source_mtime')!=NEW.mtime);
END;
-- next statement
DROP TRIGGER IF EXISTS background_job_source_deleted;
-- next statement
CREATE TRIGGER background_job_source_deleted
AFTER DELETE ON files
BEGIN
    UPDATE background_jobs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancelling' END,
        last_error_code='source_changed', revision=revision+1
    WHERE kind IN ('transcode_prepare','fragment_index_build','copy_output_prepare','encoded_output_prepare')
        AND state IN ('queued','running') AND json_extract(payload_json,'$.file_id')=OLD.id
        AND revision<9223372036854775807;
END;
