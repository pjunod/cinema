-- V7 narrows a per-file job's source reader to the library root that holds
-- the file, on top of v6. A job whose payload names a file row requires
-- source_io only for the root(s) whose path contains that file's path; a
-- missing file row, or a path under no root, keeps requiring every root as v6
-- did. Library-wide jobs and every other branch are unchanged. The narrowing
-- is background-only: foreground custody and pressure keys stay library-wide
-- and conservative. Changes affect only future resource requirements; existing
-- attempt reservations, receipts and leases remain owned until their ordinary
-- fenced retirement.
DROP VIEW background_job_required_resources;
-- next statement
CREATE VIEW IF NOT EXISTS background_job_required_resources AS
WITH job_library AS (
    SELECT job.id AS job_id, job.kind, json_extract(job.payload_json, '$.generation') AS generation, COALESCE(
        json_extract(job.payload_json, '$.library_id'), item.library_id) AS library_id, file.path AS file_path
    FROM background_jobs job
    LEFT JOIN files file ON file.id = json_extract(job.payload_json, '$.file_id')
    LEFT JOIN items item ON item.id = COALESCE(file.item_id, json_extract(job.payload_json, '$.item_id'))
)
SELECT DISTINCT job.job_id, COALESCE('source_io:' || mapping.domain_id, 'source_io') AS resource_key
FROM job_library job LEFT JOIN libraries library ON library.id = job.library_id
LEFT JOIN json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
LEFT JOIN background_storage_domains mapping ON mapping.library_id = job.library_id AND mapping.root_path = root.value
WHERE job.kind NOT IN ('semantic_embedding','subtitle_model_operation','subtitle_text_model_operation','subtitle_text_translate_artifact','artifact_hydrate','artifact_verify','artwork_derivative','fragment_index_publish')
AND NOT (job.kind = 'library_scan' AND job.generation IS 'library-targeted-v1')
-- A root contains the file when the path is the root or lies below it; a
-- trailing slash on the configured root is ignored, as in the media queries.
AND (job.file_path IS NULL
    OR job.file_path = rtrim(root.value, '/')
    OR substr(job.file_path, 1, length(rtrim(root.value, '/')) + 1) = rtrim(root.value, '/') || '/'
    OR NOT EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) holder
        WHERE job.file_path = rtrim(holder.value, '/')
        OR substr(job.file_path, 1, length(rtrim(holder.value, '/')) + 1) = rtrim(holder.value, '/') || '/'))
UNION
SELECT id, 'provider:tmdb' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'provider:anilist' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'subtitle_model:' || target_node_id FROM background_jobs WHERE kind='subtitle_model_operation';
-- next statement
DROP TABLE subtitle_managed_schema;
-- next statement
CREATE TABLE subtitle_managed_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=7)) STRICT;
-- next statement
INSERT INTO subtitle_managed_schema VALUES(1,7);
