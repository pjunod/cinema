-- V3 makes text inference consume only materialized subtitle artifacts.
-- Changes affect only future resource requirements. Existing attempt reservations,
-- receipts and leases remain owned until their ordinary fenced retirement.
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
WHERE job.kind NOT IN ('semantic_embedding','subtitle_model_operation','subtitle_text_model_operation','subtitle_text_translate_artifact')
UNION
SELECT id, 'provider:tmdb' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'provider:anilist' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'subtitle_model:' || target_node_id FROM background_jobs WHERE kind='subtitle_model_operation';
-- next statement
DROP TABLE subtitle_managed_schema;
-- next statement
CREATE TABLE subtitle_managed_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=3)) STRICT;
-- next statement
INSERT INTO subtitle_managed_schema VALUES(1,3);
