-- The demand is an outbox, not another execution queue. Only the common
-- background job token owns execution. AUTOINCREMENT prevents source/reprobe ABA.
CREATE TABLE IF NOT EXISTS dv_title_jobs_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=1));
-- next statement
CREATE TABLE IF NOT EXISTS dv_title_jobs_demand(
 revision INTEGER PRIMARY KEY AUTOINCREMENT,
 file_id INTEGER NOT NULL UNIQUE REFERENCES files(id) ON DELETE CASCADE,
 scanner_digest TEXT,
 terminal INTEGER NOT NULL DEFAULT 0 CHECK(terminal IN (0,1))
) STRICT;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_invalidate
AFTER DELETE ON dv_title_jobs_demand
BEGIN
 UPDATE background_jobs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancelling' END,
  last_error_code='title_source_changed',revision=revision+1
 WHERE kind='dv_title_scan' AND state IN ('queued','running')
  AND json_extract(payload_json,'$.probe_revision')=OLD.revision
  AND revision<9223372036854775807;
 UPDATE background_job_waiters SET state='cancelled'
 WHERE state='pending' AND job_id IN (SELECT id FROM background_jobs
  WHERE kind='dv_title_scan' AND json_extract(payload_json,'$.probe_revision')=OLD.revision);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_scanner_changed
AFTER UPDATE OF scanner_digest ON dv_title_jobs_demand
WHEN OLD.scanner_digest IS NOT NEW.scanner_digest
BEGIN
 UPDATE background_jobs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancelling' END,
  last_error_code='title_scanner_changed',revision=revision+1
 WHERE kind='dv_title_scan' AND state IN ('queued','running')
  AND json_extract(payload_json,'$.probe_revision')=NEW.revision
  AND json_extract(payload_json,'$.scanner_digest') IS NOT NEW.scanner_digest
  AND revision<9223372036854775807;
 UPDATE background_job_waiters SET state='cancelled'
 WHERE state='pending' AND job_id IN (SELECT id FROM background_jobs
  WHERE kind='dv_title_scan' AND json_extract(payload_json,'$.probe_revision')=NEW.revision
  AND json_extract(payload_json,'$.scanner_digest') IS NOT NEW.scanner_digest);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_file_insert
AFTER INSERT ON files
WHEN NEW.video_codec='hevc' AND NEW.dv_profile IN (5,7,8) AND NEW.dv_rpu_present=1
 AND json_valid(NEW.probe_json)
BEGIN
 INSERT INTO dv_title_jobs_demand(file_id) VALUES(NEW.id);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_source_change
BEFORE UPDATE OF path,size,mtime ON files
WHEN NEW.path IS NOT OLD.path OR NEW.size IS NOT OLD.size OR NEW.mtime IS NOT OLD.mtime
BEGIN
 -- Invalidate OLD demand before the write. Do not modify the target files row
 -- in BEFORE, or delete fresh demand from an unordered AFTER source trigger.
 DELETE FROM dv_title_jobs_demand WHERE file_id=OLD.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_file_update
AFTER UPDATE OF path,size,mtime ON files
WHEN NEW.path IS NOT OLD.path OR NEW.size IS NOT OLD.size OR NEW.mtime IS NOT OLD.mtime
BEGIN
 -- Source-only changes cannot retain an aggregate. This idempotent cleanup is
 -- safe before OR after ordinary publication in a fresh-source/fresh-probe write.
 UPDATE files SET probe_json=json_remove(probe_json,'$.plurx_dv_title_v1')
 WHERE id=NEW.id AND json_valid(probe_json)
  AND json_type(probe_json,'$.plurx_dv_title_v1') IS NOT NULL;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_probe_published
AFTER UPDATE OF scanned_at ON files
BEGIN
 -- Explicit ordinary-publication intent: all direct/fenced upserts and the
 -- admitted ApplyProbeJob write scanned_at. Grafts/backfills do not. UPDATE OF
 -- is deliberate: identical facts/timestamps still establish a new generation.
 -- Nested title attachment and scanner cleanup write only probe_json, not this
 -- intent column. No JSON inequality or decoration-specific exemptions.
 UPDATE files SET probe_json=json_remove(probe_json,'$.plurx_dv_title_v1')
 WHERE id=NEW.id AND json_valid(probe_json)
  AND json_type(probe_json,'$.plurx_dv_title_v1') IS NOT NULL;
 DELETE FROM dv_title_jobs_demand WHERE file_id=NEW.id;
 INSERT INTO dv_title_jobs_demand(file_id)
 SELECT NEW.id WHERE NEW.video_codec='hevc' AND NEW.dv_profile IN (5,7,8)
  AND NEW.dv_rpu_present=1 AND json_valid(NEW.probe_json);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_file_delete
BEFORE DELETE ON files
BEGIN
 DELETE FROM dv_title_jobs_demand WHERE file_id=OLD.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_terminal
AFTER UPDATE OF state ON background_jobs
WHEN NEW.kind='dv_title_scan' AND NEW.state IN ('cancelling','succeeded','failed','cancelled')
BEGIN
 UPDATE dv_title_jobs_demand SET terminal=1
 WHERE revision=json_extract(NEW.payload_json,'$.probe_revision')
 AND scanner_digest=json_extract(NEW.payload_json,'$.scanner_digest');
END;
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
  AND json_extract(NEW.result_json,'$.outcome')='published';
 DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS dv_title_jobs_bind
AFTER INSERT ON background_job_commands WHEN NEW.operation='bind_dv_title'
BEGIN
 -- Rotate a previously bound scanner generation before reuse, including A->B->A.
 -- The command selected the next AUTOINCREMENT value in this same transaction.
 -- Deleting the old generation invalidates its jobs before the new one exists.
 DELETE FROM dv_title_jobs_demand
 WHERE revision=json_extract(NEW.result_json,'$.previous_revision')
  AND json_extract(NEW.result_json,'$.rotate')=1;
 INSERT INTO dv_title_jobs_demand(revision,file_id,scanner_digest)
 SELECT json_extract(NEW.result_json,'$.revision'),json_extract(NEW.result_json,'$.file_id'),
  json_extract(NEW.request_json,'$.scanner') WHERE json_extract(NEW.result_json,'$.rotate')=1;
 UPDATE dv_title_jobs_demand SET scanner_digest=json_extract(NEW.request_json,'$.scanner'),terminal=0
 WHERE revision=json_extract(NEW.result_json,'$.revision') AND json_extract(NEW.result_json,'$.changed')=1;
 UPDATE files SET probe_json=json_remove(probe_json,'$.plurx_dv_title_v1')
 WHERE id=json_extract(NEW.result_json,'$.file_id') AND json_extract(NEW.result_json,'$.changed')=1;
 DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
-- Existing probed titles acquire demand once during migration; bootstrap replay
-- does not resurrect terminal work or reseed a source awaiting fresh probe facts.
-- The marker is committed after this seed in the same migration transaction.
INSERT INTO dv_title_jobs_demand(file_id)
SELECT id FROM files WHERE video_codec='hevc' AND dv_profile IN (5,7,8)
 AND dv_rpu_present=1 AND json_valid(probe_json)
 AND NOT EXISTS(SELECT 1 FROM dv_title_jobs_schema WHERE singleton=1)
ON CONFLICT(file_id) DO NOTHING;
-- next statement
INSERT INTO dv_title_jobs_schema VALUES(1,1) ON CONFLICT(singleton) DO NOTHING;
