-- Roots on the same physical storage share a domain regardless of mount alias.
CREATE TABLE IF NOT EXISTS background_storage_domains (
    library_id INTEGER NOT NULL,
    root_path TEXT NOT NULL,
    domain_id TEXT NOT NULL,
    PRIMARY KEY (library_id, root_path)
) STRICT;
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
UNION
SELECT id, 'provider:tmdb' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id, 'provider:anilist' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh');
-- Atomic replacement cannot reinterpret live reservations. This restriction
-- concerns changing resource identity, never enabling a feature.
-- next statement
CREATE TRIGGER IF NOT EXISTS background_storage_domains_replace
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'storage_domains'
BEGIN
    DELETE FROM background_storage_domains WHERE NEW.result_json = 'true';
    INSERT INTO background_storage_domains(library_id, root_path, domain_id)
    SELECT json_extract(value, '$.library_id'), json_extract(value, '$.root_path'), json_extract(value, '$.domain_id')
    FROM json_each(NEW.request_json, '$.mappings') WHERE NEW.result_json = 'true';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
