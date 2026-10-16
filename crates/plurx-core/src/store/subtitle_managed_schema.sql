-- Independent extension: old kinds keep identical resource rows. Never replace
-- lineage-recognized baseline source triggers; managed guards have new names.
CREATE TABLE subtitle_managed_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=1)) STRICT;
-- next statement
CREATE TABLE subtitle_model_receipts(node_id TEXT NOT NULL,model_id TEXT NOT NULL,receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),PRIMARY KEY(node_id,model_id)) STRICT;
-- next statement
DROP VIEW background_job_required_resources;
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
WHERE job.kind NOT IN ('semantic_embedding','subtitle_model_operation')
UNION
SELECT id, 'provider:tmdb' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'provider:anilist' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'subtitle_model:' || target_node_id FROM background_jobs WHERE kind='subtitle_model_operation';
-- next statement
CREATE TRIGGER subtitle_managed_source_changed AFTER UPDATE OF size,mtime ON files
WHEN NEW.size!=OLD.size OR NEW.mtime!=OLD.mtime
BEGIN
 UPDATE background_jobs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancelling' END,
 last_error_code='source_changed',revision=revision+1
 WHERE kind='subtitle_transcribe_managed' AND state IN ('queued','running')
 AND json_extract(payload_json,'$.file_id')=OLD.id AND revision<9223372036854775807
 AND (json_extract(payload_json,'$.source_size')!=NEW.size OR json_extract(payload_json,'$.source_mtime')!=NEW.mtime);
END;
-- next statement
CREATE TRIGGER subtitle_managed_source_deleted AFTER DELETE ON files
BEGIN
 UPDATE background_jobs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancelling' END,
 last_error_code='source_changed',revision=revision+1
 WHERE kind='subtitle_transcribe_managed' AND state IN ('queued','running')
 AND json_extract(payload_json,'$.file_id')=OLD.id AND revision<9223372036854775807;
END;
-- next statement
CREATE TRIGGER subtitle_managed_config_write AFTER INSERT ON background_job_commands
WHEN NEW.operation='subtitle_managed_config'
BEGIN
 INSERT INTO settings(key,value,updated_at)
 SELECT 'subtitles.transcription.managed.v1',json(json_extract(NEW.request_json,'$.managed')),json_extract(NEW.request_json,'$.now_ms')/1000
 WHERE NEW.result_json='true'
 ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at;
 INSERT INTO settings(key,value,updated_at)
 SELECT 'subtitles.transcription.config',json(json_extract(NEW.request_json,'$.legacy')),json_extract(NEW.request_json,'$.now_ms')/1000
 WHERE NEW.result_json='true'
 ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at;
 INSERT INTO settings(key,value,updated_at)
 SELECT 'subtitles.transcription.next_run','0',json_extract(NEW.request_json,'$.now_ms')/1000
 WHERE NEW.result_json='true'
 ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at;
 DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
INSERT INTO subtitle_managed_schema VALUES(1,1);
