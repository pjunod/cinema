CREATE VIEW adaptive_source_scope_holders AS
SELECT a.attempt_id, a.sequence, a.worker_node_id, a.phase,
    json_extract(r.binding_json, '$.source_scope.library_id') AS library_id,
    resource.value AS resource_key
FROM media_resource_attempts a JOIN media_resource_requests r
    USING (owner_key,request_id,request_incarnation_id),
    json_each(COALESCE(json_extract(r.binding_json, '$.source_scope.resource_keys'), '["source_io"]')) resource
WHERE a.phase <> 'closed';
-- next statement
CREATE VIEW adaptive_source_scope_changes AS
SELECT changed.library_id, COALESCE('source_io:' || changed.applied_domain, 'source_io') AS resource_key
FROM background_storage_domain_changed_roots changed
UNION
SELECT changed.library_id, COALESCE('source_io:' || (
    SELECT json_extract(wanted.value, '$.domain_id')
    FROM background_storage_domain_pending pending, json_each(pending.mappings_json) wanted
    WHERE json_extract(wanted.value, '$.library_id') = changed.library_id
      AND json_extract(wanted.value, '$.root_path') = changed.root_path
), 'source_io')
FROM background_storage_domain_changed_roots changed;
-- next statement
CREATE VIEW adaptive_source_scope_mapping_blockers AS
SELECT DISTINCT held.attempt_id, held.sequence, held.worker_node_id, held.phase, held.library_id, held.resource_key
FROM adaptive_source_scope_holders held JOIN adaptive_source_scope_changes changed
ON held.library_id IS NULL OR held.library_id = changed.library_id
   OR held.resource_key = changed.resource_key
   OR held.resource_key = 'source_io' OR changed.resource_key = 'source_io';
-- next statement
CREATE TRIGGER adaptive_source_scope_legacy_apply
BEFORE INSERT ON background_job_commands
WHEN NEW.operation = 'storage_domains_apply'
    AND json_extract(NEW.result_json, '$.verdict') = 'applied'
    AND EXISTS (SELECT 1 FROM adaptive_source_scope_mapping_blockers)
BEGIN
    SELECT RAISE(ABORT, 'adaptive source storage-domain transition blocked');
END;
