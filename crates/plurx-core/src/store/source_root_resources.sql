-- Replace only the live resource views; their historical v1 SQL remains frozen.
DROP VIEW background_job_required_resources;
-- next statement
CREATE VIEW background_job_required_resources AS
WITH source_jobs AS NOT MATERIALIZED (
 SELECT job_id,file_id,library_id FROM source_lease_job_scopes
)
SELECT job.job_id,resource.value AS resource_key FROM source_jobs job,
json_each((SELECT json_group_array(resource_key) FROM source_lease_file_resources WHERE file_id=job.file_id)) resource
WHERE job.file_id IS NOT NULL
UNION
SELECT job.job_id,resource.resource_key FROM source_jobs job
JOIN source_lease_root_resources resource ON resource.library_id=job.library_id WHERE job.file_id IS NULL
UNION
SELECT job.job_id,'source_io' FROM source_jobs job WHERE job.file_id IS NULL
 AND NOT EXISTS(SELECT 1 FROM source_lease_roots root WHERE root.library_id=job.library_id)
UNION
SELECT id,'provider:tmdb' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id,'provider:anilist' FROM background_jobs WHERE kind IN ('library_scan','metadata_refresh')
UNION
SELECT id,'subtitle_model:'||target_node_id FROM background_jobs WHERE kind='subtitle_model_operation';
-- next statement
DROP VIEW analysis_required_resources;
-- next statement
CREATE VIEW analysis_required_resources AS
SELECT request.request_id,resource.value AS resource_key FROM analysis_requests request,
json_each((SELECT json_group_array(resource_key) FROM source_lease_file_resources WHERE file_id=request.file_id)) resource
WHERE request.component IN ('fragment_index','skip_markers');
-- next statement
DROP VIEW dispatch_turn_library_resources;
-- next statement
CREATE VIEW dispatch_turn_library_resources AS
SELECT DISTINCT ready.job_id,ready.node_id,resource.resource_key
FROM dispatch_turn_ready ready JOIN background_jobs job ON job.id=ready.job_id
JOIN source_lease_roots root ON root.library_id=json_extract(job.payload_json,'$.library_id')
JOIN json_each(ready.roots) observed ON observed.value=root.mapping_path
JOIN source_lease_root_resources resource ON resource.library_id=root.library_id AND resource.root_id=root.root_id
UNION
SELECT ready.job_id,ready.node_id,'source_io' FROM dispatch_turn_ready ready
JOIN background_jobs job ON job.id=ready.job_id
WHERE NOT EXISTS(SELECT 1 FROM source_lease_roots root WHERE root.library_id=json_extract(job.payload_json,'$.library_id'))
UNION ALL SELECT job_id,node_id,'provider:tmdb' FROM dispatch_turn_ready
UNION ALL SELECT job_id,node_id,'provider:anilist' FROM dispatch_turn_ready;
-- next statement
-- Adding a mapping to an unmapped root also changes its resource identity.
-- Its rooted independent/playback readers must finish before Rule A applies it.
DROP VIEW background_storage_domain_disturbed;
-- next statement
CREATE VIEW background_storage_domain_disturbed AS
SELECT DISTINCT 'source_io:'||applied_domain AS resource_key
FROM background_storage_domain_changed_roots WHERE applied_domain IS NOT NULL
UNION
SELECT 'source_io:root/'||library_id||'/'||
  hex(CASE WHEN rtrim(root_path,'/')='' THEN '/' ELSE rtrim(root_path,'/') END)
FROM background_storage_domain_changed_roots;
-- next statement
-- A file-scoped job on a sibling root does not freeze this root's mapping.
-- Library-wide scans still declare and protect every root they may read.
DROP VIEW background_storage_domain_affected_jobs;
-- next statement
CREATE VIEW background_storage_domain_affected_jobs AS
SELECT DISTINCT job.id AS job_id,job.fence,job.owner_node_id AS node_id,
  job.lease_expires_ms,changed.library_id
FROM background_jobs job LEFT JOIN files file ON file.id=json_extract(job.payload_json,'$.file_id')
LEFT JOIN items item ON item.id=COALESCE(file.item_id,json_extract(job.payload_json,'$.item_id'))
JOIN background_storage_domain_changed_roots changed
 ON changed.library_id=COALESCE(json_extract(job.payload_json,'$.library_id'),item.library_id)
WHERE job.state IN ('running','cancelling') AND EXISTS(
 SELECT 1 FROM background_job_required_resources required WHERE required.job_id=job.id
 AND (required.resource_key='source_io' OR required.resource_key='source_io:root/'||changed.library_id||'/'||
   hex(CASE WHEN rtrim(changed.root_path,'/')='' THEN '/' ELSE rtrim(changed.root_path,'/') END)));
