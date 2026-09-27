CREATE INDEX IF NOT EXISTS background_jobs_probe_coordinator ON background_jobs(json_extract(payload_json,'$.coordinator'),state) WHERE kind = 'media_probe';
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_publish_probe_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_probe'
BEGIN
    UPDATE background_jobs SET state = 'succeeded', result_ref = json_extract(NEW.result_json,'$.result_ref'),
        owner_node_id = NULL,owner_boot_id = NULL,claim_id = NULL,lease_expires_ms = NULL,
        revision = revision + 1,updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE id = json_extract(NEW.result_json,'$.job_id') AND json_extract(NEW.result_json,'$.outcome') = 'published';
    UPDATE background_job_waiters SET state = CASE WHEN deadline_ms <= json_extract(NEW.request_json,'$.now_ms') THEN 'cancelled' ELSE 'succeeded' END,
        result_ref = 'probe:' || job_id, updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE job_id = json_extract(NEW.result_json,'$.job_id') AND state = 'pending' AND json_extract(NEW.result_json,'$.outcome') = 'published';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;

-- next statement
DROP VIEW IF EXISTS background_job_required_resources;
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
WHERE job.kind != 'semantic_embedding'
UNION
SELECT id, 'provider:tmdb' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'provider:anilist' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh');
