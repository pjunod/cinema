-- Publisher provenance stays separate from the published document. Rotation
-- fences reuse immediately while readers keep the last source-valid result.
ALTER TABLE dv_title_jobs_demand ADD COLUMN scanner_generation INTEGER NOT NULL DEFAULT 0 CHECK(scanner_generation>=0);
-- next statement
ALTER TABLE dv_title_jobs_demand ADD COLUMN aggregate_revision INTEGER;
-- next statement
UPDATE dv_title_jobs_demand SET aggregate_revision=revision
 WHERE scanner_digest IS NOT NULL AND EXISTS(SELECT 1 FROM files f WHERE f.id=file_id
  AND json_valid(f.probe_json) AND json_extract(f.probe_json,'$.plurx_dv_title_v1.complete')=1);
-- next statement
CREATE TRIGGER dv_title_jobs_scanner_authority
BEFORE UPDATE OF scanner_digest,scanner_generation ON dv_title_jobs_demand
WHEN NEW.scanner_generation<OLD.scanner_generation
 OR (OLD.scanner_digest IS NOT NULL AND NEW.scanner_digest IS NOT OLD.scanner_digest
  AND NEW.scanner_generation<=OLD.scanner_generation)
BEGIN
 -- A draining legacy worker sees no carried work and continues to its job
 -- page even if some predecessor titles remain settled.
 SELECT RAISE(IGNORE);
END;
-- next statement
CREATE TRIGGER dv_title_jobs_bind_authority
BEFORE INSERT ON background_job_commands WHEN NEW.operation='bind_dv_title'
BEGIN
 -- Rotation deletes the old row, so validate authority before that delete.
 -- A draining legacy worker may repeat its existing binding or first bind
 -- an unowned row. It cannot replace an existing scanner.
 -- Legacy workers always bind before claiming and assume any visible
 -- aggregate belongs to that revision. Leave retained results for readers
 -- and let an updated worker take any demand that needs a fresh read.
 SELECT RAISE(IGNORE)
 WHERE json_extract(NEW.request_json,'$.generation') IS NULL
  AND EXISTS(SELECT 1 FROM dv_title_jobs_demand d JOIN files f ON f.id=d.file_id
   WHERE d.revision=json_extract(NEW.result_json,'$.previous_revision')
    AND d.aggregate_revision IS NOT d.revision
    AND json_type(f.probe_json,'$.plurx_dv_title_v1') IS NOT NULL);
 SELECT RAISE(ABORT,'stale or conflicting title scanner generation')
 WHERE NOT EXISTS(SELECT 1 FROM dv_title_jobs_demand d
  WHERE d.revision=json_extract(NEW.result_json,'$.previous_revision')
   AND ((json_extract(NEW.request_json,'$.rescan')=1
     AND d.scanner_digest IS json_extract(NEW.request_json,'$.scanner')
     AND json_extract(NEW.request_json,'$.generation')>=d.scanner_generation)
    OR d.scanner_digest IS NULL
    OR json_extract(NEW.request_json,'$.generation')>d.scanner_generation
    OR (d.scanner_digest IS json_extract(NEW.request_json,'$.scanner')
     AND COALESCE(json_extract(NEW.request_json,'$.generation'),d.scanner_generation)=d.scanner_generation
     AND json_extract(NEW.result_json,'$.rotate')=0)));
END;
-- next statement
DROP TRIGGER dv_title_jobs_bind;
-- next statement
CREATE TRIGGER dv_title_jobs_bind
AFTER INSERT ON background_job_commands WHEN NEW.operation='bind_dv_title'
BEGIN
 DELETE FROM dv_title_jobs_demand
 WHERE revision=json_extract(NEW.result_json,'$.previous_revision')
  AND json_extract(NEW.result_json,'$.rotate')=1;
 INSERT INTO dv_title_jobs_demand(revision,file_id,scanner_digest,scanner_generation)
 SELECT json_extract(NEW.result_json,'$.revision'),json_extract(NEW.result_json,'$.file_id'),
  json_extract(NEW.request_json,'$.scanner'),json_extract(NEW.request_json,'$.generation')
 WHERE json_extract(NEW.result_json,'$.rotate')=1;
 UPDATE dv_title_jobs_demand SET scanner_digest=json_extract(NEW.request_json,'$.scanner'),
  scanner_generation=COALESCE(json_extract(NEW.request_json,'$.generation'),scanner_generation),
  terminal=CASE WHEN scanner_digest IS NOT json_extract(NEW.request_json,'$.scanner') THEN 0 ELSE terminal END
 WHERE revision=json_extract(NEW.result_json,'$.revision')
  AND (scanner_digest IS NOT json_extract(NEW.request_json,'$.scanner')
   OR scanner_generation<json_extract(NEW.request_json,'$.generation'));
 DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
DROP TRIGGER dv_title_jobs_publish;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_publish
AFTER INSERT ON background_job_commands WHEN NEW.operation='publish_dv_title'
BEGIN
 UPDATE files SET probe_json=json_set(probe_json,'$.plurx_dv_title_v1',
  json(json_extract(NEW.request_json,'$.output.stats')))
 WHERE id=json_extract(NEW.request_json,'$.output.file_id')
  AND json_extract(NEW.result_json,'$.outcome')='published';
 UPDATE dv_title_jobs_demand SET aggregate_revision=revision
 WHERE file_id=json_extract(NEW.request_json,'$.output.file_id')
  AND json_extract(NEW.result_json,'$.outcome')='published';
 UPDATE background_jobs SET state='succeeded',result_ref=json_extract(NEW.result_json,'$.result_ref'),
  owner_node_id=NULL,owner_boot_id=NULL,claim_id=NULL,lease_expires_ms=NULL,
  revision=revision+1,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE id=json_extract(NEW.result_json,'$.job_id')
  AND json_extract(NEW.result_json,'$.outcome')='published';
 UPDATE background_job_waiters SET state='succeeded',result_ref='dv-title:'||job_id,
  updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE job_id=json_extract(NEW.result_json,'$.job_id') AND state='pending'
  AND json_extract(NEW.result_json,'$.outcome')='published'
  AND NOT (consumer_kind='playback_qualification' AND EXISTS(SELECT 1 FROM files f
   WHERE f.id=json_extract(NEW.request_json,'$.output.file_id')
    AND json_extract(f.probe_json,'$.plurx_dv_fel_requests_v3.'||background_job_waiters.consumer_ref||'.identity')=background_job_waiters.consumer_ref
    AND json_extract(f.probe_json,'$.plurx_dv_fel_requests_v3.'||background_job_waiters.consumer_ref||'.source_size')=f.size
    AND json_extract(f.probe_json,'$.plurx_dv_fel_requests_v3.'||background_job_waiters.consumer_ref||'.source_mtime')=f.mtime));
 DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
DROP TABLE dv_title_jobs_schema;
-- next statement
CREATE TABLE dv_title_jobs_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version IN (1,2,3)));
-- next statement
INSERT INTO dv_title_jobs_schema VALUES(1,3);
