-- Canonical configured roots. A root's identity is independent of its node
-- and optional aggregate storage-domain mapping.
CREATE VIEW source_lease_roots AS
WITH roots AS (
  SELECT library.id AS library_id, root.value AS mapping_path,
    CASE WHEN rtrim(root.value,'/')='' THEN '/' ELSE rtrim(root.value,'/') END AS root_path
  FROM libraries library, json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
  WHERE root.type='text' AND root.value<>''
)
SELECT library_id,mapping_path,root_path,
  CAST(library_id AS TEXT)||'/'||hex(root_path) AS root_id,
  'source_io:root/'||library_id||'/'||hex(root_path) AS root_key
FROM roots;
-- next statement
-- Current source-job classification shared with orphan fallback checks.
CREATE VIEW source_lease_job_scopes(job_id,file_id,library_id) AS
SELECT job.id,file.id,COALESCE(json_extract(job.payload_json,'$.library_id'),item.library_id)
FROM background_jobs job LEFT JOIN files file ON file.id=json_extract(job.payload_json,'$.file_id')
LEFT JOIN items item ON item.id=COALESCE(file.item_id,json_extract(job.payload_json,'$.item_id'))
WHERE job.kind NOT IN ('semantic_embedding','subtitle_model_operation','subtitle_text_model_operation','subtitle_text_translate_artifact','artifact_hydrate','artifact_verify','artwork_derivative','fragment_index_publish')
 AND NOT (job.kind='library_scan' AND json_extract(job.payload_json,'$.generation') IS 'library-targeted-v1');
-- next statement
CREATE VIEW source_lease_root_resources AS
SELECT DISTINCT library_id,root_id,root_path,root_key,root_key AS resource_key FROM source_lease_roots
UNION
SELECT DISTINCT root.library_id,root.root_id,root.root_path,root.root_key,'source_io:'||mapping.domain_id
FROM source_lease_roots root JOIN background_storage_domains mapping
  ON mapping.library_id=root.library_id AND mapping.root_path=root.mapping_path;
-- next statement
-- Match path components, not a textual prefix (/media/a is not /media/ab).
-- Only the most specific configured root owns a file.
CREATE VIEW source_lease_file_roots AS
SELECT DISTINCT source_file.id AS file_id,root.library_id,root.root_id,root.root_path,root.root_key
FROM files source_file JOIN items item ON item.id=source_file.item_id
JOIN source_lease_roots root ON root.library_id=item.library_id
WHERE (source_file.path=root.root_path OR (root.root_path='/' AND substr(source_file.path,1,1)='/')
  OR (substr(source_file.path,1,length(root.root_path))=root.root_path AND substr(source_file.path,length(root.root_path)+1,1)='/'))
AND NOT EXISTS(SELECT 1 FROM source_lease_roots nested WHERE nested.library_id=root.library_id
  AND length(nested.root_path)>length(root.root_path)
  AND (source_file.path=nested.root_path OR (substr(source_file.path,1,length(nested.root_path))=nested.root_path
    AND substr(source_file.path,length(nested.root_path)+1,1)='/')));
-- next statement
CREATE VIEW source_lease_file_resources AS
SELECT DISTINCT matched_file.file_id,resource.resource_key FROM source_lease_file_roots matched_file
JOIN source_lease_root_resources resource ON resource.library_id=matched_file.library_id AND resource.root_id=matched_file.root_id
UNION
SELECT fallback_file.id,'source_io' FROM files fallback_file
-- Existence needs any matching root, not a second expansion of the DISTINCT
-- file-root view. Correlating that view can scan the entire files catalogue.
WHERE NOT EXISTS(SELECT 1 FROM items fallback_item
JOIN source_lease_roots root ON root.library_id=fallback_item.library_id
WHERE fallback_item.id=fallback_file.item_id
 AND (fallback_file.path=root.root_path OR (root.root_path='/' AND substr(fallback_file.path,1,1)='/')
  OR (substr(fallback_file.path,1,length(root.root_path))=root.root_path
    AND substr(fallback_file.path,length(root.root_path)+1,1)='/')));
-- next statement
-- Root capacity is primary. Domains impose an aggregate limit only when
-- explicitly configured; merely mapping roots to the same disk adds no cap.
CREATE VIEW source_lease_limits AS
SELECT DISTINCT root.root_key AS resource_key,
  COALESCE((SELECT CAST(value AS INTEGER) FROM settings WHERE key='jobs.root_source_readers.'||root.root_id
      AND CAST(value AS INTEGER) BETWEEN 1 AND 16),
    (SELECT CAST(value AS INTEGER) FROM settings WHERE key='jobs.source_readers_per_root'
      AND CAST(value AS INTEGER) BETWEEN 1 AND 16),2) AS readers
FROM source_lease_roots root
UNION
SELECT DISTINCT 'source_io:'||domain.domain_id,
  (SELECT CAST(value AS INTEGER) FROM settings WHERE key='jobs.domain_source_readers.'||domain.domain_id
    AND CAST(value AS INTEGER) BETWEEN 1 AND 16)
FROM background_storage_domains domain
UNION
SELECT 'source_io',COALESCE((SELECT CAST(value AS INTEGER) FROM settings
  WHERE key='jobs.source_readers_per_root' AND CAST(value AS INTEGER) BETWEEN 1 AND 16),2);
-- next statement
CREATE VIEW source_lease_legacy_resources AS
SELECT custody.custody_id,resource.value AS resource_key FROM background_source_custody custody,
json_each((SELECT json_group_array(resource_key) FROM analysis_required_resources WHERE request_id=custody.work_id)) resource
WHERE custody.owner_kind='analysis'
UNION
SELECT custody.custody_id,resource.value FROM background_source_custody custody
JOIN offline_packages package ON package.id=custody.work_id,
json_each((SELECT json_group_array(resource_key) FROM source_lease_file_resources WHERE file_id=package.file_id)) resource
WHERE custody.owner_kind='offline'
UNION
SELECT custody.custody_id,resource.value FROM background_source_custody custody,
json_each((SELECT json_group_array(resource_key) FROM source_lease_file_resources WHERE file_id=json_extract(custody.owner_json,'$.file_id'))) resource
WHERE custody.owner_kind='lease';
