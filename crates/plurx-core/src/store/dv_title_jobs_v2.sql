-- Version 2 keeps late exact recipes in the outbox and separates quality
-- waiter completion from complete-title aggregate publication.
DROP TRIGGER dv_title_jobs_terminal;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_terminal
AFTER UPDATE OF state ON background_jobs
WHEN NEW.kind='dv_title_scan' AND NEW.state IN ('cancelling','succeeded','failed','cancelled')
BEGIN
 UPDATE dv_title_jobs_demand SET terminal=CASE WHEN NEW.state='succeeded' AND EXISTS(SELECT 1 FROM files f WHERE f.id=dv_title_jobs_demand.file_id
  AND json_valid(f.probe_json) AND EXISTS(SELECT 1 FROM json_each(CASE WHEN json_type(f.probe_json,'$.plurx_dv_fel_requests_v3')='object'
   THEN json_extract(f.probe_json,'$.plurx_dv_fel_requests_v3') ELSE '{}' END) r
   WHERE json_extract(r.value,'$.identity')=r.key AND json_extract(r.value,'$.source_size')=f.size
    AND json_extract(r.value,'$.source_mtime')=f.mtime)) THEN 0 ELSE 1 END
 WHERE revision=json_extract(NEW.payload_json,'$.probe_revision')
 AND scanner_digest=json_extract(NEW.payload_json,'$.scanner_digest');
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
-- One-time rollout repair for recipes stranded under the v1 terminal trigger.
UPDATE dv_title_jobs_demand SET terminal=0 WHERE terminal=1
 AND NOT EXISTS(SELECT 1 FROM background_jobs j WHERE j.kind='dv_title_scan'
  AND j.state IN ('cancelling','cancelled','failed')
  AND json_extract(j.payload_json,'$.probe_revision')=dv_title_jobs_demand.revision)
 AND EXISTS(SELECT 1 FROM files f WHERE f.id=dv_title_jobs_demand.file_id AND json_valid(f.probe_json)
  AND EXISTS(SELECT 1 FROM json_each(CASE WHEN json_type(f.probe_json,'$.plurx_dv_fel_requests_v3')='object'
   THEN json_extract(f.probe_json,'$.plurx_dv_fel_requests_v3') ELSE '{}' END) r
   WHERE json_extract(r.value,'$.identity')=r.key AND json_extract(r.value,'$.source_size')=f.size
    AND json_extract(r.value,'$.source_mtime')=f.mtime));
-- next statement
DROP TABLE dv_title_jobs_schema;
-- next statement
CREATE TABLE dv_title_jobs_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version IN (1,2)));
-- next statement
INSERT INTO dv_title_jobs_schema VALUES(1,2);
