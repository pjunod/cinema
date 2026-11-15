-- Desired storage identities. Saving never depends on runtime state; the
-- daemon applies the newest revision once no holder would lose a conflict
-- and no running job would lose ownership.
CREATE TABLE IF NOT EXISTS background_storage_domain_pending (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision > 0),
    mappings_json TEXT NOT NULL CHECK (json_valid(mappings_json) AND json_type(mappings_json) = 'array'),
    base_json TEXT NOT NULL CHECK (json_valid(base_json) AND json_type(base_json) = 'array'),
    saved_at_ms INTEGER NOT NULL CHECK (saved_at_ms >= 0),
    saved_by INTEGER,
    state TEXT NOT NULL CHECK (state IN ('pending','applied','superseded')),
    settled_at_ms INTEGER,
    settled_reason TEXT CHECK (settled_reason IS NULL OR length(settled_reason) <= 64)
) STRICT;
-- next statement
-- Applied rows whose root is still a library root. Rows for removed roots are inert.
CREATE VIEW IF NOT EXISTS background_storage_domains_live AS
SELECT applied.library_id, applied.root_path, applied.domain_id
FROM background_storage_domains applied JOIN libraries library ON library.id = applied.library_id
WHERE EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
    WHERE root.value = applied.root_path);
-- next statement
CREATE VIEW IF NOT EXISTS background_storage_domain_changed_roots AS
SELECT library.id AS library_id, root.value AS root_path,
    (SELECT applied.domain_id FROM background_storage_domains applied
      WHERE applied.library_id = library.id AND applied.root_path = root.value) AS applied_domain
FROM background_storage_domain_pending pending, libraries library,
    json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
WHERE pending.state = 'pending'
  AND COALESCE((SELECT applied.domain_id FROM background_storage_domains applied
        WHERE applied.library_id = library.id AND applied.root_path = root.value), '')
   != COALESCE((SELECT json_extract(wanted.value, '$.domain_id') FROM json_each(pending.mappings_json) wanted
        WHERE json_extract(wanted.value, '$.library_id') = library.id
          AND json_extract(wanted.value, '$.root_path') = root.value), '');
-- next statement
CREATE VIEW IF NOT EXISTS background_storage_domain_disturbed AS
SELECT DISTINCT 'source_io:' || applied_domain AS resource_key
FROM background_storage_domain_changed_roots WHERE applied_domain IS NOT NULL;
-- next statement
-- Rule A holders: one row per (holder, stored key). The caller's clock decides liveness.
CREATE VIEW IF NOT EXISTS background_storage_domain_holders AS
SELECT held.owner_kind AS holder_kind, held.work_id, held.fence, held.resource_key,
    held.expires_at_ms, CASE WHEN held.custody_id IS NULL THEN 0 ELSE 1 END AS custody_held,
    CASE held.owner_kind
      WHEN 'job' THEN COALESCE((SELECT job.owner_node_id FROM background_jobs job
        WHERE job.id = held.work_id AND job.fence = held.fence),
        (SELECT json_extract(custody.owner_json, '$.token.node_id') FROM background_source_custody custody
          WHERE custody.custody_id = held.custody_id))
      WHEN 'analysis' THEN COALESCE((SELECT request.owner_node_id FROM analysis_requests request
        WHERE request.request_id = held.work_id AND request.fence = held.fence),
        (SELECT json_extract(custody.owner_json, '$.node_id') FROM background_source_custody custody
          WHERE custody.custody_id = held.custody_id))
      ELSE (SELECT CASE custody.owner_kind
              WHEN 'lease' THEN json_extract(custody.owner_json, '$.lease.owner_node_id')
              ELSE json_extract(custody.owner_json, '$.node_id') END
            FROM background_source_custody custody WHERE custody.custody_id = held.custody_id)
    END AS node_id, NULL AS library_id
FROM source_io_occupancy held WHERE held.resource_key LIKE 'source_io%'
UNION ALL
SELECT 'foreground', owner.owner_id, owner.fence, fg.resource_key, fg.lease_expires_at_ms,
    fg.custody_held, json_extract(owner.identity_json, '$.node_id'), NULL
FROM foreground_source_permissions fg JOIN foreground_source_owners owner ON owner.owner_id = fg.owner_id
WHERE fg.resource_key LIKE 'source_io%'
UNION ALL
SELECT 'pause', paused.job_id, paused.fence, resource.value, job.lease_expires_ms, 0, paused.node_id, NULL
FROM background_source_pauses paused
JOIN background_jobs job ON job.id = paused.job_id AND job.fence = paused.fence
    AND job.state IN ('running','cancelling'),
json_each(paused.resource_keys_json) resource WHERE resource.value LIKE 'source_io%';
-- next statement
-- Rule B: running jobs whose required keys would change.
CREATE VIEW IF NOT EXISTS background_storage_domain_affected_jobs AS
SELECT DISTINCT job.id AS job_id, job.fence, job.owner_node_id AS node_id,
    job.lease_expires_ms, changed.library_id
FROM background_jobs job
LEFT JOIN files file ON file.id = json_extract(job.payload_json, '$.file_id')
LEFT JOIN items item ON item.id = COALESCE(file.item_id, json_extract(job.payload_json, '$.item_id'))
JOIN background_storage_domain_changed_roots changed
    ON changed.library_id = COALESCE(json_extract(job.payload_json, '$.library_id'), item.library_id)
WHERE job.state IN ('running','cancelling')
  AND EXISTS (SELECT 1 FROM background_job_required_resources required
      WHERE required.job_id = job.id AND required.resource_key LIKE 'source_io%');
-- next statement
CREATE TRIGGER IF NOT EXISTS background_storage_domains_apply
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'storage_domains_apply'
BEGIN
    DELETE FROM background_storage_domains
    WHERE json_extract(NEW.result_json, '$.verdict') = 'applied';
    INSERT INTO background_storage_domains(library_id, root_path, domain_id)
    SELECT json_extract(wanted.value, '$.library_id'), json_extract(wanted.value, '$.root_path'),
        json_extract(wanted.value, '$.domain_id')
    FROM background_storage_domain_pending pending, json_each(pending.mappings_json) wanted
    WHERE json_extract(NEW.result_json, '$.verdict') = 'applied'
      AND EXISTS (SELECT 1 FROM libraries library,
            json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
          WHERE library.id = json_extract(wanted.value, '$.library_id')
            AND root.value = json_extract(wanted.value, '$.root_path'));
    UPDATE background_storage_domain_pending
    SET state = json_extract(NEW.result_json, '$.verdict'),
        settled_at_ms = json_extract(NEW.request_json, '$.now_ms'),
        settled_reason = json_extract(NEW.result_json, '$.reason')
    WHERE state = 'pending' AND json_extract(NEW.result_json, '$.verdict') IN ('applied','superseded');
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE TABLE IF NOT EXISTS background_storage_domain_pending_schema (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    version INTEGER NOT NULL CHECK (version = 1)
) STRICT;
-- next statement
INSERT OR IGNORE INTO background_storage_domain_pending_schema (singleton, version) VALUES (1, 1);
