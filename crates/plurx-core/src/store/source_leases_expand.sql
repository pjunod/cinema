-- Source leases v2 (expand). One row per source reader; its expiry is a
-- column. A row counts against its node (and any mapped storage domain it
-- names) while expires_at_ms > now - 60000; the sweep only collects garbage.
-- docs/server/SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md
CREATE TABLE source_leases (
    lease_id TEXT PRIMARY KEY,
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('playback','job','analysis','lease','offline')),
    work_id TEXT NOT NULL,
    fence INTEGER NOT NULL CHECK(fence>0),
    node_id TEXT NOT NULL,
    owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
    resource_keys_json TEXT NOT NULL CHECK(json_valid(resource_keys_json)),
    phase TEXT NOT NULL CHECK(phase IN ('active','paused','startup','refill','sharing','transfer_service','source_idle','cleanup_pending')),
    projection_json TEXT CHECK(projection_json IS NULL OR json_valid(projection_json)),
    started_at_ms INTEGER NOT NULL,
    renewed_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX source_leases_node ON source_leases(node_id, expires_at_ms);
-- next statement
CREATE INDEX source_leases_work ON source_leases(owner_kind, work_id);
-- next statement
-- Resolve each lease owner once; expand exactly one physical file/root scope.
CREATE VIEW source_lease_owner_scopes(lease_id,file_id,library_id) AS
SELECT held.lease_id,file.id,NULL FROM source_leases held
JOIN files file ON file.id=json_extract(held.owner_json,'$.file_id')
WHERE held.owner_kind IN ('playback','lease')
UNION ALL
SELECT held.lease_id,scope.file_id,scope.library_id FROM source_leases held
JOIN source_lease_job_scopes scope ON scope.job_id=held.work_id WHERE held.owner_kind='job'
UNION ALL
SELECT held.lease_id,request.file_id,NULL FROM source_leases held
JOIN analysis_requests request ON request.request_id=held.work_id
JOIN files file ON file.id=request.file_id
WHERE held.owner_kind='analysis' AND request.component IN ('fragment_index','skip_markers')
UNION ALL
SELECT held.lease_id,package.file_id,NULL FROM source_leases held
JOIN offline_packages package ON package.id=held.work_id
JOIN files file ON file.id=package.file_id WHERE held.owner_kind='offline';
-- next statement
-- Receipt provenance is not authority: no lease FK and no hold/pin. It dies
-- with the existing five-second receipt ledger and lets byte allocations be
-- projected through the same current roots as their physical source.
CREATE TABLE source_lease_receipt_sources (
 receipt_id TEXT NOT NULL REFERENCES background_source_service_grants(receipt_id) ON DELETE CASCADE,
 lease_id TEXT NOT NULL,
 library_id INTEGER, source_path TEXT,
 PRIMARY KEY(receipt_id,lease_id)
) STRICT;
-- next statement
-- Playback windows, one row per (playback lease, source key). Playback is
-- never counted as a reader; outside its window it blocks background reads.
CREATE VIEW source_lease_effective_resources AS
WITH resolved AS NOT MATERIALIZED (
 SELECT scope.lease_id,resource.value AS resource_key FROM source_lease_owner_scopes scope,
 json_each((SELECT json_group_array(resource_key) FROM source_lease_file_resources WHERE file_id=scope.file_id)) resource
 WHERE scope.file_id IS NOT NULL
 UNION
 SELECT scope.lease_id,resource.resource_key FROM source_lease_owner_scopes scope
 JOIN source_lease_root_resources resource ON resource.library_id=scope.library_id
 WHERE scope.file_id IS NULL AND scope.library_id IS NOT NULL
 UNION
 SELECT scope.lease_id,'source_io' FROM source_lease_owner_scopes scope
 WHERE scope.file_id IS NULL AND (scope.library_id IS NULL OR NOT EXISTS(SELECT 1 FROM source_lease_roots root WHERE root.library_id=scope.library_id))
)
SELECT lease_id,resource_key FROM resolved
UNION
-- Preserve the admitted physical scope until the phase closes, as well as
-- charging the current logical scope above. New parent/child phases each
-- start with their own exact root; only an in-flight root edit can charge both.
SELECT held.lease_id,stored.value FROM source_leases held,json_each(held.resource_keys_json) stored
UNION
-- Retired owner rows still charge overlapping current roots during grace.
-- Known file/scope owners above remain exact, preserving normal independent
-- budgets for configured parent/child roots.
SELECT held.lease_id,resource.resource_key
FROM source_leases held,json_each(held.resource_keys_json) stored
JOIN source_lease_roots root ON substr(stored.value,1,15)='source_io:root/' AND (
 stored.value=root.root_key
 OR substr(root.root_key,1,length(stored.value)+2)=stored.value||'2F'
 OR substr(stored.value,1,length(root.root_key)+2)=root.root_key||'2F'
 OR (substr(stored.value,-3)='/2F' AND substr(root.root_key,1,length(stored.value)-2)=substr(stored.value,1,length(stored.value)-2))
 OR (substr(root.root_key,-3)='/2F' AND substr(stored.value,1,length(root.root_key)-2)=substr(root.root_key,1,length(root.root_key)-2)))
JOIN source_lease_root_resources resource ON resource.library_id=root.library_id AND resource.root_id=root.root_id
WHERE NOT EXISTS(SELECT 1 FROM source_lease_owner_scopes known WHERE known.lease_id=held.lease_id);
-- next statement
CREATE VIEW source_lease_permissions AS
SELECT l.lease_id,l.node_id,k.value AS resource_key,l.expires_at_ms,
  CASE WHEN json_extract(l.projection_json,'$.source_quiescent')=1 THEN 1 ELSE 0 END AS source_quiescent,
  CASE WHEN l.phase='source_idle' AND json_extract(l.projection_json,'$.source_quiescent')=1 THEN l.expires_at_ms
    WHEN l.phase='sharing' AND json_extract(l.projection_json,'$.source_quiescent')=1
      AND json_extract(l.projection_json,'$.contiguous_lead_ms')>json_extract(l.projection_json,'$.low_ms')
      THEN MIN(l.expires_at_ms,json_extract(l.projection_json,'$.observation_expires_at_ms'))
    WHEN l.phase='transfer_service' THEN MIN(l.expires_at_ms,json_extract(l.projection_json,'$.transfer_grant.expires_at_ms'))
    ELSE 0 END AS permits_through_ms
FROM source_leases l JOIN (SELECT lease_id,resource_key AS value FROM source_lease_effective_resources) k ON k.lease_id=l.lease_id WHERE l.owner_kind='playback';
-- next statement
CREATE TABLE source_lease_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version IN (2,3))) STRICT;
-- next statement
INSERT INTO source_lease_schema(singleton,version) VALUES(1,2);
-- next statement
-- A job lease is written by its claim, from any binary, in the claim's
-- own transaction. No daemon path renews or forgets it.
CREATE TRIGGER source_lease_job_claim AFTER UPDATE ON background_jobs
WHEN NEW.state='running' AND NEW.fence>OLD.fence AND EXISTS(SELECT 1 FROM background_job_required_resources r
    WHERE r.job_id=NEW.id AND r.resource_key LIKE 'source_io%')
BEGIN
  INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,started_at_ms,renewed_at_ms,expires_at_ms)
  SELECT 'job:'||NEW.id||':'||NEW.fence,'job',NEW.id,NEW.fence,NEW.owner_node_id,
    json_object('kind','job','job_kind',NEW.kind,'boot_id',NEW.owner_boot_id,'claim_id',NEW.claim_id),
    (SELECT json_group_array(resource_key) FROM (SELECT DISTINCT resource_key FROM background_job_required_resources
       WHERE job_id=NEW.id AND resource_key LIKE 'source_io%' ORDER BY resource_key)),
    'active',NEW.updated_at_ms,NEW.updated_at_ms,NEW.lease_expires_ms
  WHERE NEW.owner_node_id IS NOT NULL AND NEW.lease_expires_ms IS NOT NULL
  ON CONFLICT(lease_id) DO NOTHING;
END;
-- next statement
CREATE TRIGGER source_lease_job_renew AFTER UPDATE OF lease_expires_ms ON background_jobs
WHEN NEW.state='running' AND NEW.fence=OLD.fence AND NEW.lease_expires_ms>OLD.lease_expires_ms
BEGIN
  UPDATE source_leases SET expires_at_ms=NEW.lease_expires_ms,renewed_at_ms=NEW.updated_at_ms
  WHERE lease_id='job:'||NEW.id||':'||NEW.fence;
END;
-- next statement
-- Cancelling: the reader is being stopped; charge it until release or the grace.
CREATE TRIGGER source_lease_job_cancelling AFTER UPDATE OF state ON background_jobs
WHEN OLD.state='running' AND NEW.state='cancelling'
BEGIN
  UPDATE source_leases SET expires_at_ms=MIN(expires_at_ms,NEW.updated_at_ms) WHERE lease_id='job:'||OLD.id||':'||OLD.fence;
END;
-- next statement
-- The owner settled while its job lease was live: its reader has returned.
-- An abandoned job (lease already expired) keeps its row until expiry + grace.
CREATE TRIGGER source_lease_job_settled AFTER UPDATE OF state ON background_jobs
WHEN OLD.state IN ('running','cancelling') AND NEW.state NOT IN ('running','cancelling')
  AND OLD.lease_expires_ms>NEW.updated_at_ms
BEGIN
  DELETE FROM source_leases WHERE lease_id='job:'||OLD.id||':'||OLD.fence;
END;
-- next statement
-- Analysis leases follow analysis_source_reserve_on_claim (as redefined by
-- analysis coordination: a coordinated request is charged per physical
-- phase, by begin_phase, as 'analysis:<hex request>:<fence>:<phase>'),
-- analysis_source_renew and _release_on_success; renewal and success
-- therefore match the owner, not one id.
-- A cancelled or retried request keeps its lease to expiry, as its
-- reservation does.
CREATE TRIGGER source_lease_analysis_claim AFTER UPDATE OF state,fence ON analysis_requests
WHEN NEW.state='running' AND NEW.fence>OLD.fence
 AND NEW.component IN ('fragment_index','skip_markers')
 AND NEW.pipeline_version NOT LIKE 'coordination-contract:%'
 AND NEW.owner_node_id IS NOT NULL AND NEW.lease_expires_ms IS NOT NULL
 AND EXISTS(SELECT 1 FROM analysis_required_resources r WHERE r.request_id=NEW.request_id)
BEGIN
  INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,started_at_ms,renewed_at_ms,expires_at_ms)
  SELECT 'analysis:'||hex(NEW.request_id)||':'||NEW.fence,'analysis',NEW.request_id,NEW.fence,NEW.owner_node_id,
    json_object('kind','analysis','component',NEW.component,'trigger',NEW.trigger),
    (SELECT json_group_array(resource_key) FROM (SELECT DISTINCT resource_key FROM analysis_required_resources
       WHERE request_id=NEW.request_id ORDER BY resource_key)),
    'active',NEW.updated_at_ms,NEW.updated_at_ms,NEW.lease_expires_ms
  ON CONFLICT(lease_id) DO NOTHING;
END;
-- next statement
CREATE TRIGGER source_lease_analysis_renew AFTER UPDATE OF lease_expires_ms ON analysis_requests
WHEN NEW.state='running' AND NEW.fence=OLD.fence AND NEW.lease_expires_ms>OLD.lease_expires_ms
BEGIN
  UPDATE source_leases SET expires_at_ms=NEW.lease_expires_ms,renewed_at_ms=NEW.updated_at_ms
  WHERE owner_kind='analysis' AND work_id=NEW.request_id AND fence=NEW.fence;
END;
-- next statement
CREATE TRIGGER source_lease_analysis_success AFTER UPDATE OF state ON analysis_requests
WHEN OLD.state='running' AND NEW.state IN ('submitted','ready')
BEGIN
  DELETE FROM source_leases WHERE owner_kind='analysis' AND work_id=NEW.request_id AND fence=NEW.fence;
END;
-- next statement
-- A read attestation is renewed (and closed, by lowering expiry) on its
-- job_leases row.
CREATE TRIGGER source_lease_attestation_renew AFTER UPDATE OF expires_at_ms ON job_leases
WHEN NEW.resource LIKE 'subtitle-read-attest:%'
BEGIN
  UPDATE source_leases SET expires_at_ms=NEW.expires_at_ms,renewed_at_ms=NEW.updated_at_ms
  WHERE owner_kind='lease' AND work_id=NEW.resource AND fence=NEW.fence;
END;
-- next statement
-- An offline reader renews its lease by taking service: every grant (at
-- most one epoch, 5 s, long) extends it to one job lease past the grant.
CREATE TRIGGER source_lease_offline_renew AFTER INSERT ON background_job_commands
WHEN NEW.operation='source_service' AND json_type(NEW.result_json)='object'
  AND json_extract(NEW.request_json,'$.kind')='offline'
BEGIN
  INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
  SELECT json_extract(NEW.request_json,'$.owner_key'),'offline',
    json_extract(NEW.request_json,'$.package_id'),json_extract(NEW.request_json,'$.claim_generation'),
    json_extract(NEW.request_json,'$.node_id'),
    COALESCE(json_extract(NEW.request_json,'$.owner'),
      json_object('kind','offline','package_id',json_extract(NEW.request_json,'$.package_id'),
        'node_id',json_extract(NEW.request_json,'$.node_id'),
        'claim_generation',json_extract(NEW.request_json,'$.claim_generation'))),
    json_extract(NEW.result_json,'$.resource_keys'),'active',NULL,
    json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms'),
    json_extract(NEW.request_json,'$.now_ms')+30000
  ON CONFLICT(lease_id) DO UPDATE SET
    work_id=excluded.work_id,fence=excluded.fence,node_id=excluded.node_id,
    owner_json=excluded.owner_json,resource_keys_json=excluded.resource_keys_json,
    phase='active',projection_json=NULL,renewed_at_ms=excluded.renewed_at_ms,
    expires_at_ms=MAX(source_leases.expires_at_ms,excluded.expires_at_ms)
  WHERE source_leases.owner_kind='offline';
END;
-- next statement
CREATE TRIGGER source_lease_offline_end AFTER UPDATE ON offline_packages
WHEN OLD.state='preparing' AND (NEW.state!='preparing' OR NEW.claim_generation!=OLD.claim_generation OR NEW.node_id IS NOT OLD.node_id)
BEGIN
  DELETE FROM source_leases WHERE owner_kind='offline' AND work_id=OLD.id AND fence=OLD.claim_generation;
END;
-- next statement
CREATE TRIGGER source_lease_offline_delete AFTER DELETE ON offline_packages
BEGIN
  DELETE FROM source_leases WHERE owner_kind='offline' AND work_id=OLD.id AND fence=OLD.claim_generation;
END;
-- next statement
-- Pause and resume act on the job's lease; resume takes a fresh source
-- reservation slot (source reservations are the bundle record, not a cap).
DROP TRIGGER IF EXISTS background_source_pause_effects;
-- next statement
CREATE TRIGGER background_source_pause_effects
AFTER INSERT ON background_job_commands WHEN NEW.operation='source_pause' AND NEW.result_json='true'
BEGIN
    INSERT INTO background_source_pauses(job_id,fence,node_id,boot_id,claim_id,resource_keys_json,phase,paused_at_ms)
    SELECT job.id,job.fence,job.owner_node_id,job.owner_boot_id,job.claim_id,
        COALESCE((SELECT json_group_array(resource_key) FROM background_job_reservations held
            WHERE held.job_id=job.id AND held.fence=job.fence AND held.resource_key LIKE 'source_io%'), '[]'),
        'paused',json_extract(NEW.request_json,'$.now_ms')
    FROM background_jobs job WHERE job.id=json_extract(NEW.request_json,'$.token.job_id')
    ON CONFLICT(job_id) DO UPDATE SET phase='paused',paused_at_ms=excluded.paused_at_ms;
    UPDATE source_leases SET phase='paused',renewed_at_ms=json_extract(NEW.request_json,'$.now_ms')
      WHERE lease_id='job:'||json_extract(NEW.request_json,'$.token.job_id')||':'||json_extract(NEW.request_json,'$.token.fence');
    DELETE FROM background_job_reservations WHERE job_id=json_extract(NEW.request_json,'$.token.job_id')
        AND fence=json_extract(NEW.request_json,'$.token.fence') AND resource_key LIKE 'source_io%';
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
DROP TRIGGER IF EXISTS background_source_resume_effects;
-- next statement
CREATE TRIGGER background_source_resume_effects
AFTER INSERT ON background_job_commands WHEN NEW.operation='source_resume' AND NEW.result_json='true'
BEGIN
    DELETE FROM background_job_reservations WHERE expires_at_ms<=json_extract(NEW.request_json,'$.now_ms');
    INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms)
    SELECT resource.resource_key,(SELECT COALESCE(MAX(held.slot),-1)+1 FROM background_job_reservations held
        WHERE held.resource_key=resource.resource_key),paused.job_id,paused.fence,job.lease_expires_ms
    FROM background_source_pauses paused JOIN background_jobs job ON job.id=paused.job_id
      JOIN background_job_required_resources resource ON resource.job_id=job.id AND resource.resource_key LIKE 'source_io%'
    WHERE paused.job_id=json_extract(NEW.request_json,'$.token.job_id') AND paused.phase='paused';
    UPDATE source_leases SET resource_keys_json=(SELECT json_group_array(resource_key) FROM
      (SELECT resource_key FROM background_job_required_resources WHERE job_id=source_leases.work_id AND resource_key LIKE 'source_io%' ORDER BY resource_key)),
      phase='active',renewed_at_ms=json_extract(NEW.request_json,'$.now_ms')
      WHERE lease_id='job:'||json_extract(NEW.request_json,'$.token.job_id')||':'||json_extract(NEW.request_json,'$.token.fence')
      AND EXISTS(SELECT 1 FROM background_source_pauses paused WHERE paused.job_id=source_leases.work_id AND paused.phase='paused');
    UPDATE background_source_pauses SET resource_keys_json=(SELECT json_group_array(resource_key) FROM
      (SELECT resource_key FROM background_job_required_resources WHERE job_id=background_source_pauses.job_id AND resource_key LIKE 'source_io%' ORDER BY resource_key)),phase='active'
        WHERE job_id=json_extract(NEW.request_json,'$.token.job_id') AND phase='paused';
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
-- Dispatch turns read the same leases and the same three claim terms
-- (claim_foreground_window!, claim_source_holders!, claim_maintenance_reader!
-- in background_jobs.rs). A ready scan is maintenance work.
DROP VIEW IF EXISTS dispatch_turn_ordinary_occupancy;
-- next statement
CREATE VIEW dispatch_turn_ordinary_occupancy AS
SELECT k.value AS resource_key,h.work_id,h.lease_id,h.node_id
FROM source_leases h CROSS JOIN dispatch_turn_attempt a JOIN (SELECT lease_id,resource_key AS value FROM source_lease_effective_resources) k ON k.lease_id=h.lease_id
WHERE h.owner_kind!='playback' AND h.phase!='paused' AND h.expires_at_ms>a.now_ms-60000
AND NOT (h.owner_kind='job' AND EXISTS(SELECT 1 FROM dispatch_turn_protected_jobs p WHERE p.job_id=h.work_id))
AND NOT (h.owner_kind='analysis' AND EXISTS(SELECT 1 FROM background_job_waiters viewer WHERE viewer.job_id=h.work_id
    AND viewer.request_scope='playback-analysis' AND viewer.state='pending' AND viewer.deadline_ms>a.now_ms));
-- next statement
DROP TRIGGER IF EXISTS dispatch_turn_independent;
-- next statement
CREATE TRIGGER dispatch_turn_independent BEFORE INSERT ON source_leases
WHEN NEW.owner_kind IN ('lease','offline') AND EXISTS(SELECT 1 FROM dispatch_turn_ready WHERE expires_at_ms>NEW.started_at_ms)
AND NOT EXISTS(SELECT 1 FROM source_leases WHERE lease_id=NEW.lease_id)
BEGIN
    DELETE FROM dispatch_turn_attempt;
    INSERT INTO dispatch_turn_attempt(singleton,owner,work,keys_json,now_ms,queued_ms,allowed)
    VALUES(1,NEW.owner_kind,NEW.work_id,NEW.resource_keys_json,NEW.started_at_ms,NEW.started_at_ms,0);
    SELECT CASE WHEN (SELECT allowed FROM dispatch_turn_attempt)=0 THEN RAISE(IGNORE) END;
END;
-- next statement
-- Storage-domain Rule A holders: every source lease x stored key. A
-- lease's liveness includes the 60 s grace, exactly as the claim counts it.
DROP VIEW IF EXISTS background_storage_domain_holders;
-- next statement
CREATE VIEW background_storage_domain_holders AS
SELECT CASE l.owner_kind WHEN 'playback' THEN 'foreground' ELSE l.owner_kind END AS holder_kind,
    CASE l.owner_kind WHEN 'playback' THEN l.lease_id ELSE l.work_id END AS work_id, l.fence, k.value AS resource_key,
    CASE WHEN l.phase='paused' OR (l.owner_kind='playback' AND json_extract(l.projection_json,'$.source_quiescent')=1)
      THEN l.expires_at_ms ELSE l.expires_at_ms+60000 END AS expires_at_ms,
    0 AS custody_held, l.node_id, NULL AS library_id
FROM source_leases l JOIN (
 SELECT lease_id,resource_key AS value FROM source_lease_effective_resources
 UNION SELECT stored.lease_id,key.value FROM source_leases stored,json_each(stored.resource_keys_json) key
) k ON k.lease_id=l.lease_id WHERE k.value LIKE 'source_io%'
UNION ALL
SELECT 'pause', paused.job_id, paused.fence, resource.value, job.lease_expires_ms, 0, paused.node_id, NULL
FROM background_source_pauses paused
JOIN background_jobs job ON job.id = paused.job_id AND job.fence = paused.fence
    AND job.state IN ('running','cancelling'),
json_each(paused.resource_keys_json) resource WHERE resource.value LIKE 'source_io%';
-- next statement
-- Retention no longer consults source custody: the enqueue and upkeep
-- triggers return to their v85 and v86 text (pinned by test).
DROP INDEX IF EXISTS background_source_custody_owner_work;
-- next statement
DROP TRIGGER IF EXISTS background_job_enqueue_command;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_enqueue_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'enqueue'
BEGIN
    UPDATE job_leases SET
        revision = json_extract(NEW.request_json, '$.producer_replacement.revision'),
        expires_at_ms = json_extract(NEW.request_json, '$.producer_replacement.expires_at_unix_ms'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE resource = json_extract(NEW.request_json, '$.producer_lease.resource')
        AND json_extract(NEW.result_json, '$.outcome') != 'producer_fenced';

    DELETE FROM background_jobs WHERE json_extract(NEW.result_json, '$.outcome') = 'accepted'
        AND (SELECT COUNT(*) FROM background_jobs) >= 10000
        AND id IN (SELECT job.id FROM background_jobs job
        WHERE job.state IN ('succeeded','failed','cancelled')
          AND job.id != json_extract(NEW.result_json, '$.job_id')
          AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
            AND remaining.kind = 'fragment_index_build'
            AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
          AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))
        ORDER BY job.updated_at_ms, job.id LIMIT 128);

    INSERT INTO background_jobs (
        id, kind, payload_version, payload_json, dedupe_key, priority,
        state, target_node_id, not_before_ms, created_at_ms, updated_at_ms, attempt_limit)
    SELECT json_extract(NEW.result_json, '$.job_id'),
        json_extract(NEW.request_json, '$.payload.kind'), 1,
        json_extract(NEW.request_json, '$.payload'),
        json_extract(NEW.request_json, '$.dedupe_key'),
        json_extract(NEW.request_json, '$.priority'), 'queued',
        json_extract(NEW.request_json, '$.payload.target_node_id'),
        json_extract(NEW.request_json, '$.not_before_ms'),
        json_extract(NEW.request_json, '$.now_ms'),
        json_extract(NEW.request_json, '$.now_ms'),
        COALESCE(json_extract(NEW.request_json, '$.attempt_limit'), 5)
    WHERE json_extract(NEW.result_json, '$.outcome') = 'accepted'
        AND NOT EXISTS (SELECT 1 FROM background_jobs
            WHERE id = json_extract(NEW.result_json, '$.job_id'));

    INSERT INTO background_job_waiters (
        request_scope, request_id, request_digest, job_id, consumer_kind,
        consumer_ref, priority, state, target_node_id, deadline_ms,
        receipt_expires_ms, retain_identity, created_at_ms, updated_at_ms,
        attempt_limit, not_before_ms, participation_fence)
    SELECT json_extract(NEW.request_json, '$.request.scope'),
        json_extract(NEW.request_json, '$.request.request_id'),
        json_extract(NEW.request_json, '$.request.request_digest'),
        json_extract(NEW.result_json, '$.job_id'),
        json_extract(NEW.request_json, '$.request.consumer_kind'),
        json_extract(NEW.request_json, '$.request.consumer_ref'),
        json_extract(NEW.request_json, '$.priority'), 'pending',
        json_extract(NEW.request_json, '$.request.target_node_id'),
        json_extract(NEW.request_json, '$.request.deadline_ms'),
        json_extract(NEW.result_json, '$.receipt_expires_ms'),
        COALESCE(json_extract(NEW.request_json, '$.request.retain_identity'), 0),
        json_extract(NEW.request_json, '$.now_ms'),
        json_extract(NEW.request_json, '$.now_ms'),
        COALESCE(json_extract(NEW.request_json, '$.attempt_limit'), 5),
        json_extract(NEW.request_json, '$.not_before_ms'),
        COALESCE((SELECT fence FROM background_jobs WHERE id = json_extract(NEW.result_json, '$.job_id')
            AND state = 'running' AND json_extract(NEW.request_json, '$.not_before_ms') <= json_extract(NEW.request_json, '$.now_ms')), 0)
    WHERE json_extract(NEW.result_json, '$.outcome') = 'accepted';

    UPDATE background_job_waiters SET priority = MAX(priority,
        json_extract(NEW.request_json, '$.priority'))
    WHERE request_scope = json_extract(NEW.request_json, '$.request.scope')
        AND request_id = json_extract(NEW.request_json, '$.request.request_id')
        AND state = 'pending'
        AND json_extract(NEW.result_json, '$.outcome') = 'existing';

    UPDATE background_jobs SET priority = MAX(priority,
        json_extract(NEW.request_json, '$.priority'))
    WHERE id = json_extract(NEW.result_json, '$.job_id')
        AND state IN ('queued','running')
        AND json_extract(NEW.result_json, '$.outcome') IN ('accepted','existing')
        AND EXISTS (SELECT 1 FROM background_job_waiters
            WHERE request_scope = json_extract(NEW.request_json, '$.request.scope')
            AND request_id = json_extract(NEW.request_json, '$.request.request_id') AND state = 'pending');

    UPDATE background_jobs SET
        not_before_ms = CASE WHEN state = 'queued' THEN COALESCE((SELECT MIN(not_before_ms)
            FROM background_job_waiters WHERE job_id = background_jobs.id AND state = 'pending'), not_before_ms) ELSE not_before_ms END,
        retry_deadline_ms = CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state = 'pending' AND retry_deadline_ms = 0) THEN 0
            ELSE COALESCE((SELECT MAX(retry_deadline_ms) FROM background_job_waiters
                WHERE job_id = background_jobs.id AND state = 'pending'), retry_deadline_ms) END
    WHERE id = json_extract(NEW.result_json, '$.job_id') AND kind = 'fragment_index_build'
        AND state IN ('queued','running') AND json_extract(NEW.result_json, '$.outcome') IN ('accepted','existing');

    UPDATE analysis_requests SET state = 'submitted', owner_node_id = NULL, lease_expires_ms = NULL,
        result_cache_key = json_extract(NEW.request_json, '$.fragment_domain.cache_key'),
        expected_predecessor_generation = COALESCE((SELECT generation_cache_key FROM cluster_fragment_index_heads
            WHERE logical_cache_key = json_extract(NEW.request_json, '$.logical_key')), ''),
        last_error_code = NULL, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE request_id = json_extract(NEW.request_json, '$.analysis_request.request_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    INSERT INTO background_fragment_targets (cache_key, target_node_id, job_id)
    SELECT json_extract(NEW.request_json, '$.fragment_domain.cache_key'),
        json_extract(NEW.request_json, '$.fragment_domain.target_node_id'), json_extract(NEW.result_json, '$.job_id')
    WHERE json_type(NEW.request_json, '$.fragment_domain') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted'
    ON CONFLICT(cache_key, target_node_id) DO UPDATE SET job_id = excluded.job_id;

    INSERT INTO cluster_fragment_index_jobs (cache_key, file_id, source_size, source_mtime, source_sha256,
        pipeline_sha256, priority, trigger, target_node_id, state, owner_node_id, fence, lease_expires_ms,
        attempts, not_before_ms, created_at_ms, updated_at_ms)
    SELECT json_extract(NEW.request_json, '$.fragment_domain.cache_key'),
        json_extract(NEW.request_json, '$.fragment_domain.file_id'), json_extract(NEW.request_json, '$.fragment_domain.source_size'),
        json_extract(NEW.request_json, '$.fragment_domain.source_mtime'), json_extract(NEW.request_json, '$.fragment_domain.source_sha256'),
        json_extract(NEW.request_json, '$.fragment_domain.pipeline_sha256'), json_extract(NEW.request_json, '$.fragment_domain.priority'),
        json_extract(NEW.request_json, '$.fragment_domain.trigger'), json_extract(NEW.request_json, '$.fragment_domain.target_node_id'),
        job.state, job.owner_node_id, job.fence, job.lease_expires_ms,
        job.failed_attempts + CASE WHEN job.state = 'running' THEN 1 ELSE 0 END,
        job.not_before_ms, job.created_at_ms, job.updated_at_ms
    FROM background_jobs job WHERE job.id = json_extract(NEW.result_json, '$.job_id')
        AND json_type(NEW.request_json, '$.fragment_domain') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted'
    ON CONFLICT(cache_key, target_node_id) DO UPDATE SET state = excluded.state,
        owner_node_id = excluded.owner_node_id, fence = excluded.fence, lease_expires_ms = excluded.lease_expires_ms,
        priority = excluded.priority, trigger = excluded.trigger, updated_at_ms = excluded.updated_at_ms,
        attempts = CASE WHEN cluster_fragment_index_jobs.state = 'ready'
            OR json_extract(NEW.request_json, '$.analysis_request.force_rebuild') = 1 THEN excluded.attempts ELSE cluster_fragment_index_jobs.attempts END,
        attempt_errors = CASE WHEN cluster_fragment_index_jobs.state = 'ready'
            OR json_extract(NEW.request_json, '$.analysis_request.force_rebuild') = 1 THEN '' ELSE cluster_fragment_index_jobs.attempt_errors END,
        index_retry_deadline_ms = CASE WHEN cluster_fragment_index_jobs.state = 'ready'
            OR json_extract(NEW.request_json, '$.analysis_request.force_rebuild') = 1 THEN 0 ELSE cluster_fragment_index_jobs.index_retry_deadline_ms END,
        index_diagnostic_json = CASE WHEN cluster_fragment_index_jobs.state = 'ready'
            OR json_extract(NEW.request_json, '$.analysis_request.force_rebuild') = 1 THEN '' ELSE cluster_fragment_index_jobs.index_diagnostic_json END;

    UPDATE background_fragment_targets SET job_id = json_extract(NEW.result_json, '$.job_id')
    WHERE json_type(NEW.request_json, '$.delivery_parent') IS NOT NULL
        AND cache_key = substr(json_extract(NEW.request_json, '$.payload.artifact_key'), 10)
        AND target_node_id = json_extract(NEW.request_json, '$.payload.target_node_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    -- Only the sealed snapshot importer can supply legacy_key. Admission,
    -- preserved ledgers and mapping advance in this same transaction.
    UPDATE background_job_waiters SET
        failed_attempts = json_extract(NEW.request_json, '$.legacy_failures'),
        retry_deadline_ms = COALESCE(json_extract(NEW.request_json, '$.legacy_snapshot.index_retry_deadline_ms'), 0),
        attempt_errors = COALESCE(json_extract(NEW.request_json, '$.legacy_snapshot.attempt_errors'), '') ||
            CASE WHEN json_extract(NEW.request_json, '$.legacy_snapshot.state') = 'running'
                THEN CASE WHEN COALESCE(json_extract(NEW.request_json, '$.legacy_snapshot.attempt_errors'), '') = ''
                    THEN 'legacy_abandoned' ELSE ',legacy_abandoned' END ELSE '' END,
        last_error_code = CASE WHEN json_extract(NEW.request_json, '$.legacy_snapshot.state') = 'running'
            THEN 'legacy_abandoned' ELSE json_extract(NEW.request_json, '$.legacy_snapshot.last_error_code') END,
        index_diagnostic_json = COALESCE(json_extract(NEW.request_json, '$.legacy_snapshot.index_diagnostic_json'), '')
    WHERE request_scope = json_extract(NEW.request_json, '$.request.scope')
        AND request_id = json_extract(NEW.request_json, '$.request.request_id')
        AND json_type(NEW.request_json, '$.legacy_key') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    UPDATE background_job_waiters SET state = CASE
        WHEN failed_attempts >= attempt_limit THEN 'failed'
        WHEN retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms') THEN 'failed'
        WHEN (SELECT state FROM background_jobs WHERE id = job_id) IN ('failed','cancelled')
            THEN (SELECT state FROM background_jobs WHERE id = job_id)
        WHEN (SELECT state FROM background_jobs WHERE id = job_id) = 'succeeded'
            THEN CASE WHEN target_node_id IS NULL OR EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                WHERE location.cache_key = json_extract(NEW.request_json, '$.payload.cache_key')
                    AND location.node_id = background_job_waiters.target_node_id) THEN 'succeeded' ELSE 'awaiting_hydration' END
        ELSE state END,
        last_error_code = CASE WHEN failed_attempts >= attempt_limit THEN 'attempt_limit'
            WHEN retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms') THEN 'index_retry_window_expired'
            ELSE last_error_code END,
        result_ref = (SELECT result_ref FROM background_jobs WHERE id = job_id)
    WHERE request_scope = json_extract(NEW.request_json, '$.request.scope')
        AND request_id = json_extract(NEW.request_json, '$.request.request_id')
        AND json_type(NEW.request_json, '$.legacy_key') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    INSERT INTO background_fragment_targets (cache_key, target_node_id, job_id)
    SELECT json_extract(NEW.request_json, '$.payload.cache_key'),
        COALESCE(json_extract(NEW.request_json, '$.request.target_node_id'), ''), json_extract(NEW.result_json, '$.job_id')
    WHERE json_type(NEW.request_json, '$.legacy_key') IS NOT NULL
        AND json_extract(NEW.request_json, '$.payload.kind') = 'fragment_index_build'
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted'
    ON CONFLICT(cache_key, target_node_id) DO UPDATE SET job_id = excluded.job_id;

    UPDATE background_jobs SET
        failed_attempts = CASE WHEN kind = 'transcode_prepare' THEN json_extract(NEW.request_json, '$.legacy_failures') ELSE failed_attempts END,
        checkpoint_json = CASE WHEN kind = 'transcode_prepare' THEN json_object(
            'staging_node_id', json_extract(NEW.request_json, '$.legacy_snapshot.staging_node_id'),
            'legacy_job_id', json_extract(NEW.request_json, '$.legacy_snapshot.id'),
            'prefer_until_ms', json_extract(NEW.request_json, '$.now_ms') + 30000) ELSE checkpoint_json END,
        not_before_ms = COALESCE((SELECT MIN(not_before_ms) FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state = 'pending'), not_before_ms),
        retry_deadline_ms = CASE WHEN EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state = 'pending' AND retry_deadline_ms = 0) THEN 0
            ELSE COALESCE((SELECT MAX(retry_deadline_ms) FROM background_job_waiters
                WHERE job_id = background_jobs.id AND state = 'pending'), retry_deadline_ms) END,
        state = CASE WHEN state = 'queued' AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = background_jobs.id AND state = 'pending')
            AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
                AND remaining.legacy_key != json_extract(NEW.request_json, '$.legacy_key')
                AND remaining.kind = 'fragment_index_build'
                AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(NEW.request_json, '$.payload.cache_key'))
            THEN 'failed' ELSE state END,
        created_at_ms = MIN(created_at_ms, json_extract(NEW.request_json, '$.legacy_snapshot.created_at_ms'))
    WHERE id = json_extract(NEW.result_json, '$.job_id') AND state IN ('queued','running')
        AND json_type(NEW.request_json, '$.legacy_key') IS NOT NULL
        AND json_extract(NEW.result_json, '$.outcome') = 'accepted';

    UPDATE background_job_legacy SET state = CASE WHEN json_extract(NEW.result_json, '$.outcome') IN ('accepted','existing')
        THEN 'materialized' ELSE 'failed' END,
        job_id = json_extract(NEW.result_json, '$.job_id'), outcome = json_extract(NEW.result_json, '$.outcome'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE legacy_key = json_extract(NEW.request_json, '$.legacy_key') AND state = 'awaiting_import'
        AND json_extract(NEW.result_json, '$.outcome') IN ('accepted','existing','source_changed','conflict');

    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
DROP TRIGGER IF EXISTS background_job_maintenance_command;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_maintenance_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'maintain'
BEGIN
    UPDATE background_job_waiters SET state = 'failed', last_error_code = 'index_retry_window_expired',
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE (request_scope, request_id) IN (SELECT request_scope, request_id FROM background_job_waiters
        WHERE state = 'pending' AND retry_deadline_ms > 0 AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms')
        ORDER BY retry_deadline_ms, request_scope, request_id LIMIT 128);
    UPDATE background_job_waiters SET state = 'cancelled', updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE (request_scope, request_id) IN (SELECT delivery.request_scope, delivery.request_id FROM background_job_waiters delivery
        WHERE delivery.consumer_kind = 'background_delivery' AND delivery.state = 'pending'
            AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
                WHERE interest.job_id = delivery.consumer_ref AND interest.target_node_id = delivery.target_node_id
                    AND interest.state = 'awaiting_hydration'
                    AND (interest.deadline_ms IS NULL OR interest.deadline_ms > json_extract(NEW.request_json, '$.now_ms')))
        ORDER BY delivery.request_scope, delivery.request_id LIMIT 128);

    UPDATE background_job_waiters SET state = 'cancelled',
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE (request_scope, request_id) IN (
        SELECT request_scope, request_id FROM background_job_waiters
        WHERE state IN ('pending','awaiting_hydration')
          AND deadline_ms <= json_extract(NEW.request_json, '$.now_ms')
        ORDER BY deadline_ms, request_scope, request_id LIMIT 128);
    UPDATE background_jobs SET priority = COALESCE((SELECT MAX(priority)
        FROM background_job_waiters WHERE job_id = background_jobs.id
          AND state IN ('pending','awaiting_hydration')
          AND (deadline_ms IS NULL OR deadline_ms > json_extract(NEW.request_json, '$.now_ms'))), 0)
    WHERE id IN (SELECT job_id FROM background_job_waiters
        WHERE state IN ('cancelled','failed') AND updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
        ORDER BY job_id LIMIT 128) AND state IN ('queued','running');
    UPDATE background_jobs SET state = 'failed',
        failed_attempts = failed_attempts + CASE WHEN state = 'running' THEN 1 ELSE 0 END,
        last_error_code = 'index_retry_window_expired', owner_node_id = NULL,
        owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT id FROM background_jobs WHERE retry_deadline_ms > 0
        AND retry_deadline_ms <= json_extract(NEW.request_json, '$.now_ms')
        AND (state = 'queued' OR (state = 'running' AND lease_expires_ms <= json_extract(NEW.request_json, '$.now_ms')))
        AND revision < 9223372036854775807 ORDER BY retry_deadline_ms, id LIMIT 128);
    UPDATE background_jobs SET
        state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT job.id FROM background_jobs job
        WHERE job.state IN ('queued','running') AND job.revision < 9223372036854775807
          AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))
          AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
            AND remaining.kind = 'fragment_index_build'
            AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
        ORDER BY job.id LIMIT 128);
    UPDATE background_jobs SET state = 'cancelled', owner_node_id = NULL,
        owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT id FROM background_jobs WHERE state = 'cancelling'
        AND lease_expires_ms <= json_extract(NEW.request_json, '$.now_ms')
        AND revision < 9223372036854775807 ORDER BY lease_expires_ms, id LIMIT 128);
    UPDATE background_jobs SET state = 'failed', failed_attempts = failed_attempts + 1,
        last_error_code = 'execution_abandoned', owner_node_id = NULL,
        owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id IN (SELECT id FROM background_jobs WHERE state = 'running'
        AND ((kind != 'fragment_index_build' AND failed_attempts >= attempt_limit - 1)
          OR (kind = 'fragment_index_build' AND NOT EXISTS (SELECT 1 FROM background_job_waiters interest
            WHERE interest.job_id = background_jobs.id AND interest.state = 'pending'
              AND interest.failed_attempts + CASE WHEN interest.participation_fence = background_jobs.fence THEN 1 ELSE 0 END < interest.attempt_limit
              AND (interest.retry_deadline_ms = 0 OR interest.retry_deadline_ms > json_extract(NEW.request_json, '$.now_ms')))))
        AND lease_expires_ms <= json_extract(NEW.request_json, '$.now_ms')
        AND revision < 9223372036854775807 ORDER BY lease_expires_ms, id LIMIT 128);
    DELETE FROM background_job_attempts WHERE (job_id, fence) IN (
        SELECT old.job_id, old.fence FROM background_job_attempts old
        WHERE old.finished_at_ms IS NOT NULL
          AND old.resolve_until_ms <= json_extract(NEW.request_json, '$.now_ms')
          AND ((SELECT COUNT(*) FROM background_job_attempts newer
            WHERE newer.job_id = old.job_id AND newer.finished_at_ms IS NOT NULL
              AND newer.fence > old.fence) >= 16
            OR ((SELECT COUNT(*) FROM background_job_attempts) >= 39872
              AND EXISTS (SELECT 1 FROM background_job_attempts newer
                WHERE newer.job_id = old.job_id AND newer.fence > old.fence)))
        ORDER BY old.finished_at_ms, old.job_id, old.fence LIMIT 128);
    DELETE FROM background_job_waiters WHERE (request_scope, request_id) IN (
        SELECT request_scope, request_id FROM background_job_waiters
        WHERE state IN ('succeeded','failed','cancelled') AND retain_identity = 0
          AND receipt_expires_ms <= json_extract(NEW.request_json, '$.now_ms')
        ORDER BY receipt_expires_ms, request_scope, request_id LIMIT 128);
    -- Receipt pressure (MAX_WAITERS - 8 pages = 15360): the oldest terminal
    -- INTERNAL receipts compact a page per pass. User-scoped and
    -- identity-retaining receipts keep their full seven-day window, and a
    -- receipt whose job is still active is never touched; internal producers
    -- re-derive demand from their domain tables, so nothing finished re-runs.
    DELETE FROM background_job_waiters WHERE (request_scope, request_id) IN (
        SELECT waiter.request_scope, waiter.request_id FROM background_job_waiters waiter
        WHERE (SELECT COUNT(*) FROM background_job_waiters) >= 15360
          AND waiter.state IN ('succeeded','failed','cancelled') AND waiter.retain_identity = 0
          AND waiter.request_scope NOT LIKE 'user:%'
          AND NOT EXISTS (SELECT 1 FROM background_jobs job WHERE job.id = waiter.job_id
            AND job.state IN ('queued','running','cancelling'))
        ORDER BY waiter.updated_at_ms, waiter.request_scope, waiter.request_id LIMIT 128);
    DELETE FROM background_jobs WHERE id IN (
        SELECT job.id FROM background_jobs job
        WHERE job.state IN ('succeeded','failed','cancelled')
          AND (job.updated_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 604800000
            OR (SELECT COUNT(*) FROM background_jobs) >= 9000)
          AND NOT EXISTS (SELECT 1 FROM background_job_legacy remaining WHERE remaining.state = 'awaiting_import'
            AND remaining.kind = 'fragment_index_build'
            AND json_extract(remaining.snapshot_json, '$.cache_key') = json_extract(job.payload_json, '$.cache_key'))
          AND NOT EXISTS (SELECT 1 FROM background_job_waiters
            WHERE job_id = job.id AND state IN ('pending','awaiting_hydration'))
        ORDER BY job.updated_at_ms, job.id LIMIT 128);
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
-- Carry over every live holder. An owner that is not running holds nothing,
-- so ownerless custody, settlement proofs and activation ids are dropped.
INSERT OR IGNORE INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
SELECT owner_id,'playback',COALESCE(json_extract(identity_json,'$.lifetime_id'),owner_id),fence,json_extract(identity_json,'$.node_id'),
  identity_json,CASE WHEN EXISTS(SELECT 1 FROM source_lease_file_resources WHERE file_id=json_extract(identity_json,'$.file_id'))
    THEN (SELECT json_group_array(resource_key) FROM (SELECT resource_key FROM source_lease_file_resources WHERE file_id=json_extract(identity_json,'$.file_id') ORDER BY resource_key))
    ELSE resource_keys_json END,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms
FROM foreground_source_owners WHERE json_extract(identity_json,'$.node_id') IS NOT NULL;
-- next statement
INSERT OR IGNORE INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,started_at_ms,renewed_at_ms,expires_at_ms)
SELECT 'job:'||job.id||':'||job.fence,'job',job.id,job.fence,job.owner_node_id,
  json_object('kind','job','job_kind',job.kind,'boot_id',job.owner_boot_id,'claim_id',job.claim_id),
  (SELECT json_group_array(resource_key) FROM (SELECT DISTINCT required.resource_key FROM background_job_required_resources required
     WHERE required.job_id=job.id AND required.resource_key LIKE 'source_io%' ORDER BY required.resource_key)),
  'active',job.updated_at_ms,job.updated_at_ms,job.lease_expires_ms
FROM background_jobs job WHERE job.state IN ('running','cancelling') AND job.owner_node_id IS NOT NULL AND job.lease_expires_ms IS NOT NULL
  AND EXISTS(SELECT 1 FROM background_job_reservations held WHERE held.job_id=job.id AND held.fence=job.fence AND held.resource_key LIKE 'source_io%');
-- next statement
INSERT OR IGNORE INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,started_at_ms,renewed_at_ms,expires_at_ms)
SELECT 'job:'||job.id||':'||job.fence,'job',job.id,job.fence,paused.node_id,
  json_object('kind','job','job_kind',job.kind,'boot_id',paused.boot_id,'claim_id',paused.claim_id),
  paused.resource_keys_json,'paused',paused.paused_at_ms,paused.paused_at_ms,job.lease_expires_ms
FROM background_source_pauses paused JOIN background_jobs job ON job.id=paused.job_id AND job.fence=paused.fence
WHERE paused.phase='paused' AND job.state IN ('running','cancelling') AND job.lease_expires_ms IS NOT NULL
  AND json_array_length(paused.resource_keys_json)>0;
-- next statement
INSERT OR IGNORE INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,started_at_ms,renewed_at_ms,expires_at_ms)
SELECT 'analysis:'||hex(request.request_id)||':'||request.fence,'analysis',request.request_id,request.fence,request.owner_node_id,
  json_object('kind','analysis','component',request.component,'trigger',request.trigger),
  (SELECT json_group_array(resource_key) FROM (SELECT DISTINCT required.resource_key FROM analysis_required_resources required
     WHERE required.request_id=request.request_id ORDER BY required.resource_key)),
  'active',request.updated_at_ms,request.updated_at_ms,request.lease_expires_ms
FROM analysis_requests request WHERE request.state='running' AND request.fence>0 AND request.owner_node_id IS NOT NULL
  AND request.lease_expires_ms IS NOT NULL
  AND request.pipeline_version NOT LIKE 'coordination-contract:%'
  AND EXISTS(SELECT 1 FROM analysis_source_reservations held WHERE held.request_id=request.request_id AND held.fence=request.fence);
-- next statement
INSERT OR IGNORE INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,started_at_ms,renewed_at_ms,expires_at_ms)
SELECT CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END,
  custody.owner_kind,custody.work_id,custody.fence,
  CASE custody.owner_kind WHEN 'lease' THEN json_extract(custody.owner_json,'$.lease.owner_node_id') ELSE json_extract(custody.owner_json,'$.node_id') END,
  custody.owner_json,CASE WHEN EXISTS(SELECT 1 FROM source_leases retained
      WHERE retained.lease_id=CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END
        AND retained.owner_kind=custody.owner_kind AND retained.work_id=custody.work_id AND retained.fence=custody.fence
        AND retained.phase!='paused' AND retained.expires_at_ms>custody.updated_at_ms-60000)
      THEN (SELECT retained.resource_keys_json FROM source_leases retained
        WHERE retained.lease_id=CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END)
      ELSE CASE WHEN EXISTS(SELECT 1 FROM source_lease_legacy_resources root WHERE root.custody_id=custody.custody_id)
      THEN (SELECT json_group_array(resource_key) FROM (SELECT resource_key FROM source_lease_legacy_resources root WHERE root.custody_id=custody.custody_id ORDER BY resource_key))
      ELSE custody.resource_keys_json END END,custody.phase,
  custody.started_at_ms,custody.updated_at_ms,
  CASE custody.owner_kind WHEN 'offline' THEN MIN(custody.authority_expires_at_ms,custody.updated_at_ms+30000) ELSE custody.authority_expires_at_ms END
FROM background_source_current_owners custody
WHERE (custody.owner_kind IN ('lease','offline') OR
    (custody.owner_kind='analysis' AND EXISTS(SELECT 1 FROM analysis_requests request
      WHERE request.request_id=custody.work_id AND request.pipeline_version LIKE 'coordination-contract:%')))
  AND custody.authority_expires_at_ms>0 AND custody.fence>0
  AND CASE custody.owner_kind WHEN 'lease' THEN json_extract(custody.owner_json,'$.lease.owner_node_id') ELSE json_extract(custody.owner_json,'$.node_id') END IS NOT NULL
  AND json_array_length(custody.resource_keys_json)>0;
-- next statement
-- Rolling-deploy bridge, both ways, dropped by the contract. A node still on
-- the v1 binary registers playback in foreground_source_owners: mirror it
-- into source_leases, so its playback blocks background reads decided by
-- upgraded nodes. An upgraded node's playback lease is mirrored back, so a
-- v1 node's claims see it too. Each side writes only a row that differs,
-- so a mirrored write ends at the other side, recursive triggers or not.
CREATE TRIGGER source_lease_bridge_insert AFTER INSERT ON foreground_source_owners
WHEN json_extract(NEW.identity_json,'$.node_id') IS NOT NULL
BEGIN
  INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
  VALUES(NEW.owner_id,'playback',COALESCE(json_extract(NEW.identity_json,'$.lifetime_id'),NEW.owner_id),NEW.fence,json_extract(NEW.identity_json,'$.node_id'),
    NEW.identity_json,CASE WHEN EXISTS(SELECT 1 FROM source_lease_file_resources WHERE file_id=json_extract(NEW.identity_json,'$.file_id'))
      THEN (SELECT json_group_array(resource_key) FROM (SELECT resource_key FROM source_lease_file_resources WHERE file_id=json_extract(NEW.identity_json,'$.file_id') ORDER BY resource_key))
      ELSE NEW.resource_keys_json END,NEW.phase,NEW.projection_json,NEW.started_at_ms,NEW.updated_at_ms,NEW.lease_expires_at_ms)
  ON CONFLICT(lease_id) DO UPDATE SET owner_kind=excluded.owner_kind,work_id=excluded.work_id,fence=excluded.fence,node_id=excluded.node_id,
    owner_json=excluded.owner_json,resource_keys_json=excluded.resource_keys_json,phase=excluded.phase,projection_json=excluded.projection_json,
    started_at_ms=excluded.started_at_ms,renewed_at_ms=excluded.renewed_at_ms,expires_at_ms=excluded.expires_at_ms
  WHERE (source_leases.owner_kind,source_leases.work_id,source_leases.fence,source_leases.node_id,source_leases.owner_json,source_leases.resource_keys_json,
      source_leases.phase,source_leases.projection_json,source_leases.started_at_ms,source_leases.renewed_at_ms,source_leases.expires_at_ms)
    IS NOT (excluded.owner_kind,excluded.work_id,excluded.fence,excluded.node_id,excluded.owner_json,excluded.resource_keys_json,
      excluded.phase,excluded.projection_json,excluded.started_at_ms,excluded.renewed_at_ms,excluded.expires_at_ms);
END;
-- next statement
CREATE TRIGGER source_lease_bridge_update AFTER UPDATE ON foreground_source_owners
WHEN json_extract(NEW.identity_json,'$.node_id') IS NOT NULL
BEGIN
  INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
  VALUES(NEW.owner_id,'playback',COALESCE(json_extract(NEW.identity_json,'$.lifetime_id'),NEW.owner_id),NEW.fence,json_extract(NEW.identity_json,'$.node_id'),
    NEW.identity_json,CASE WHEN EXISTS(SELECT 1 FROM source_leases retained WHERE retained.lease_id=NEW.owner_id
      AND retained.owner_kind='playback' AND retained.owner_json=NEW.identity_json
      AND COALESCE(json_extract(retained.projection_json,'$.source_quiescent'),0)=0
      AND retained.expires_at_ms>NEW.updated_at_ms-60000)
      THEN (SELECT resource_keys_json FROM source_leases WHERE lease_id=NEW.owner_id)
      WHEN EXISTS(SELECT 1 FROM source_lease_file_resources WHERE file_id=json_extract(NEW.identity_json,'$.file_id'))
      THEN (SELECT json_group_array(resource_key) FROM (SELECT resource_key FROM source_lease_file_resources WHERE file_id=json_extract(NEW.identity_json,'$.file_id') ORDER BY resource_key))
      ELSE NEW.resource_keys_json END,NEW.phase,NEW.projection_json,NEW.started_at_ms,NEW.updated_at_ms,NEW.lease_expires_at_ms)
  ON CONFLICT(lease_id) DO UPDATE SET owner_kind=excluded.owner_kind,work_id=excluded.work_id,fence=excluded.fence,node_id=excluded.node_id,
    owner_json=excluded.owner_json,resource_keys_json=excluded.resource_keys_json,phase=excluded.phase,projection_json=excluded.projection_json,
    started_at_ms=excluded.started_at_ms,renewed_at_ms=excluded.renewed_at_ms,expires_at_ms=excluded.expires_at_ms
  WHERE (source_leases.owner_kind,source_leases.work_id,source_leases.fence,source_leases.node_id,source_leases.owner_json,source_leases.resource_keys_json,
      source_leases.phase,source_leases.projection_json,source_leases.started_at_ms,source_leases.renewed_at_ms,source_leases.expires_at_ms)
    IS NOT (excluded.owner_kind,excluded.work_id,excluded.fence,excluded.node_id,excluded.owner_json,excluded.resource_keys_json,
      excluded.phase,excluded.projection_json,excluded.started_at_ms,excluded.renewed_at_ms,excluded.expires_at_ms);
END;
-- next statement
CREATE TRIGGER source_lease_bridge_delete AFTER DELETE ON foreground_source_owners
BEGIN
  DELETE FROM source_leases WHERE lease_id=OLD.owner_id AND owner_kind='playback';
END;
-- next statement
CREATE TRIGGER source_lease_bridge_out_insert AFTER INSERT ON source_leases
WHEN NEW.owner_kind='playback'
BEGIN
  INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms)
  VALUES(NEW.lease_id,NEW.owner_json,NEW.resource_keys_json,NEW.fence,NEW.phase,COALESCE(NEW.projection_json,'{}'),NEW.started_at_ms,NEW.renewed_at_ms,NEW.expires_at_ms)
  ON CONFLICT(owner_id) DO UPDATE SET identity_json=excluded.identity_json,resource_keys_json=excluded.resource_keys_json,fence=excluded.fence,
    phase=excluded.phase,projection_json=excluded.projection_json,started_at_ms=excluded.started_at_ms,updated_at_ms=excluded.updated_at_ms,
    lease_expires_at_ms=excluded.lease_expires_at_ms
  WHERE (foreground_source_owners.identity_json,foreground_source_owners.resource_keys_json,foreground_source_owners.fence,foreground_source_owners.phase,
      foreground_source_owners.projection_json,foreground_source_owners.started_at_ms,foreground_source_owners.updated_at_ms,foreground_source_owners.lease_expires_at_ms)
    IS NOT (excluded.identity_json,excluded.resource_keys_json,excluded.fence,excluded.phase,
      excluded.projection_json,excluded.started_at_ms,excluded.updated_at_ms,excluded.lease_expires_at_ms);
END;
-- next statement
CREATE TRIGGER source_lease_bridge_out_update AFTER UPDATE ON source_leases
WHEN NEW.owner_kind='playback'
BEGIN
  INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms)
  VALUES(NEW.lease_id,NEW.owner_json,NEW.resource_keys_json,NEW.fence,NEW.phase,COALESCE(NEW.projection_json,'{}'),NEW.started_at_ms,NEW.renewed_at_ms,NEW.expires_at_ms)
  ON CONFLICT(owner_id) DO UPDATE SET identity_json=excluded.identity_json,resource_keys_json=excluded.resource_keys_json,fence=excluded.fence,
    phase=excluded.phase,projection_json=excluded.projection_json,started_at_ms=excluded.started_at_ms,updated_at_ms=excluded.updated_at_ms,
    lease_expires_at_ms=excluded.lease_expires_at_ms
  WHERE (foreground_source_owners.identity_json,foreground_source_owners.resource_keys_json,foreground_source_owners.fence,foreground_source_owners.phase,
      foreground_source_owners.projection_json,foreground_source_owners.started_at_ms,foreground_source_owners.updated_at_ms,foreground_source_owners.lease_expires_at_ms)
    IS NOT (excluded.identity_json,excluded.resource_keys_json,excluded.fence,excluded.phase,
      excluded.projection_json,excluded.started_at_ms,excluded.updated_at_ms,excluded.lease_expires_at_ms);
END;
-- next statement
CREATE TRIGGER source_lease_bridge_out_delete AFTER DELETE ON source_leases
WHEN OLD.owner_kind='playback'
BEGIN
  DELETE FROM foreground_source_owners WHERE owner_id=OLD.lease_id;
END;

-- next statement
-- Legacy v1 custody rows are the other half of a rolling source-lease
-- deployment. Mirror authoritative legacy owners into the lease catalogue.
CREATE TRIGGER source_lease_custody_insert AFTER INSERT ON background_source_custody
WHEN NEW.owner_kind IN ('analysis','lease','offline')
BEGIN
  INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
  SELECT CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END,
    custody.owner_kind,custody.work_id,custody.fence,
    CASE custody.owner_kind WHEN 'lease' THEN json_extract(custody.owner_json,'$.lease.owner_node_id') ELSE json_extract(custody.owner_json,'$.node_id') END,
    custody.owner_json,CASE WHEN EXISTS(SELECT 1 FROM source_leases retained
      WHERE retained.lease_id=CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END
        AND retained.owner_kind=custody.owner_kind AND retained.work_id=custody.work_id AND retained.fence=custody.fence
        AND retained.phase!='paused' AND retained.expires_at_ms>custody.updated_at_ms-60000)
      THEN (SELECT retained.resource_keys_json FROM source_leases retained
        WHERE retained.lease_id=CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END)
      ELSE CASE WHEN EXISTS(SELECT 1 FROM source_lease_legacy_resources root WHERE root.custody_id=custody.custody_id)
      THEN (SELECT json_group_array(resource_key) FROM (SELECT resource_key FROM source_lease_legacy_resources root WHERE root.custody_id=custody.custody_id ORDER BY resource_key))
      ELSE custody.resource_keys_json END END,custody.phase,NULL,custody.started_at_ms,custody.updated_at_ms,
    CASE custody.owner_kind WHEN 'offline' THEN MIN(custody.authority_expires_at_ms,custody.updated_at_ms+30000) ELSE custody.authority_expires_at_ms END
  FROM background_source_current_owners custody
  WHERE custody.custody_id=NEW.custody_id AND custody.authority_expires_at_ms>0 AND custody.fence>0
    AND json_array_length(custody.resource_keys_json)>0
    AND CASE custody.owner_kind WHEN 'lease' THEN json_extract(custody.owner_json,'$.lease.owner_node_id') ELSE json_extract(custody.owner_json,'$.node_id') END IS NOT NULL
    AND (custody.owner_kind!='analysis' OR EXISTS(SELECT 1 FROM analysis_requests request
      WHERE request.request_id=custody.work_id AND request.pipeline_version LIKE 'coordination-contract:%'))
  ON CONFLICT(lease_id) DO UPDATE SET
    owner_kind=excluded.owner_kind,work_id=excluded.work_id,fence=excluded.fence,node_id=excluded.node_id,
    owner_json=excluded.owner_json,resource_keys_json=excluded.resource_keys_json,phase=excluded.phase,
    projection_json=excluded.projection_json,started_at_ms=excluded.started_at_ms,
    renewed_at_ms=excluded.renewed_at_ms,expires_at_ms=excluded.expires_at_ms
  WHERE source_leases.owner_kind=excluded.owner_kind
    AND (source_leases.work_id,source_leases.fence,source_leases.node_id,source_leases.owner_json,
      source_leases.resource_keys_json,source_leases.phase,source_leases.projection_json,
      source_leases.started_at_ms,source_leases.renewed_at_ms,source_leases.expires_at_ms)
      IS NOT (excluded.work_id,excluded.fence,excluded.node_id,excluded.owner_json,
      excluded.resource_keys_json,excluded.phase,excluded.projection_json,
      excluded.started_at_ms,excluded.renewed_at_ms,excluded.expires_at_ms);
END;
-- next statement
CREATE TRIGGER source_lease_custody_update AFTER UPDATE ON background_source_custody
WHEN NEW.owner_kind IN ('analysis','lease','offline')
BEGIN
  INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
  SELECT CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END,
    custody.owner_kind,custody.work_id,custody.fence,
    CASE custody.owner_kind WHEN 'lease' THEN json_extract(custody.owner_json,'$.lease.owner_node_id') ELSE json_extract(custody.owner_json,'$.node_id') END,
    custody.owner_json,CASE WHEN EXISTS(SELECT 1 FROM source_leases retained
      WHERE retained.lease_id=CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END
        AND retained.owner_kind=custody.owner_kind AND retained.work_id=custody.work_id AND retained.fence=custody.fence
        AND retained.phase!='paused' AND retained.expires_at_ms>custody.updated_at_ms-60000)
      THEN (SELECT retained.resource_keys_json FROM source_leases retained
        WHERE retained.lease_id=CASE custody.owner_kind WHEN 'analysis' THEN custody.custody_id ELSE custody.owner_key END)
      ELSE CASE WHEN EXISTS(SELECT 1 FROM source_lease_legacy_resources root WHERE root.custody_id=custody.custody_id)
      THEN (SELECT json_group_array(resource_key) FROM (SELECT resource_key FROM source_lease_legacy_resources root WHERE root.custody_id=custody.custody_id ORDER BY resource_key))
      ELSE custody.resource_keys_json END END,custody.phase,NULL,custody.started_at_ms,custody.updated_at_ms,
    CASE custody.owner_kind WHEN 'offline' THEN MIN(custody.authority_expires_at_ms,custody.updated_at_ms+30000) ELSE custody.authority_expires_at_ms END
  FROM background_source_current_owners custody
  WHERE custody.custody_id=NEW.custody_id AND custody.authority_expires_at_ms>0 AND custody.fence>0
    AND json_array_length(custody.resource_keys_json)>0
    -- Offline metadata cannot reacquire a collected reader. Only a service
    -- grant, admitted by the shared writer terms, may create it again.
    AND (custody.owner_kind!='offline' OR EXISTS(SELECT 1 FROM source_leases current
      WHERE current.lease_id=custody.owner_key AND current.owner_kind='offline'))
    AND CASE custody.owner_kind WHEN 'lease' THEN json_extract(custody.owner_json,'$.lease.owner_node_id') ELSE json_extract(custody.owner_json,'$.node_id') END IS NOT NULL
    AND (custody.owner_kind!='analysis' OR EXISTS(SELECT 1 FROM analysis_requests request
      WHERE request.request_id=custody.work_id AND request.pipeline_version LIKE 'coordination-contract:%'))
  ON CONFLICT(lease_id) DO UPDATE SET
    owner_kind=excluded.owner_kind,work_id=excluded.work_id,fence=excluded.fence,node_id=excluded.node_id,
    owner_json=excluded.owner_json,resource_keys_json=excluded.resource_keys_json,phase=excluded.phase,
    projection_json=excluded.projection_json,started_at_ms=excluded.started_at_ms,
    renewed_at_ms=CASE WHEN excluded.owner_kind='offline' THEN source_leases.renewed_at_ms ELSE excluded.renewed_at_ms END,
    expires_at_ms=CASE WHEN excluded.owner_kind='offline' THEN source_leases.expires_at_ms ELSE excluded.expires_at_ms END
  WHERE source_leases.owner_kind=excluded.owner_kind
    AND (source_leases.work_id,source_leases.fence,source_leases.node_id,source_leases.owner_json,
      source_leases.resource_keys_json,source_leases.phase,source_leases.projection_json,
      source_leases.started_at_ms,source_leases.renewed_at_ms,source_leases.expires_at_ms)
      IS NOT (excluded.work_id,excluded.fence,excluded.node_id,excluded.owner_json,
      excluded.resource_keys_json,excluded.phase,excluded.projection_json,
      excluded.started_at_ms,
      CASE WHEN excluded.owner_kind='offline' THEN source_leases.renewed_at_ms ELSE excluded.renewed_at_ms END,
      CASE WHEN excluded.owner_kind='offline' THEN source_leases.expires_at_ms ELSE excluded.expires_at_ms END);
END;
-- next statement
CREATE TRIGGER source_lease_custody_delete AFTER DELETE ON background_source_custody
WHEN OLD.owner_kind IN ('analysis','lease','offline')
BEGIN
  DELETE FROM source_leases
  WHERE lease_id=CASE OLD.owner_kind WHEN 'analysis' THEN OLD.custody_id ELSE OLD.owner_key END
    AND owner_kind=OLD.owner_kind AND work_id=OLD.work_id AND fence=OLD.fence;
END;
-- next statement
-- New binaries also publish independently-created attestation and offline
-- leases in the v1 custody table so old source_io_occupancy sees their holds.
CREATE TRIGGER source_lease_custody_out_insert AFTER INSERT ON source_leases
WHEN NEW.owner_kind IN ('lease','offline')
BEGIN
  INSERT INTO background_source_custody(custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,phase,started_at_ms,updated_at_ms,lease_expires_at_ms)
  VALUES(NEW.lease_id,NEW.lease_id,NEW.owner_json,NEW.resource_keys_json,NEW.owner_kind,NEW.work_id,NEW.fence,
    NEW.phase,NEW.started_at_ms,NEW.renewed_at_ms,NEW.expires_at_ms)
  ON CONFLICT(owner_key) DO UPDATE SET
    owner_json=excluded.owner_json,resource_keys_json=excluded.resource_keys_json,owner_kind=excluded.owner_kind,
    work_id=excluded.work_id,fence=excluded.fence,phase=excluded.phase,started_at_ms=excluded.started_at_ms,
    updated_at_ms=excluded.updated_at_ms,lease_expires_at_ms=excluded.lease_expires_at_ms
  WHERE background_source_custody.owner_kind=excluded.owner_kind
    AND (background_source_custody.owner_json,background_source_custody.resource_keys_json,
      background_source_custody.work_id,background_source_custody.fence,background_source_custody.phase,
      background_source_custody.started_at_ms,background_source_custody.updated_at_ms,
      background_source_custody.lease_expires_at_ms)
      IS NOT (excluded.owner_json,excluded.resource_keys_json,excluded.work_id,excluded.fence,
      excluded.phase,excluded.started_at_ms,excluded.updated_at_ms,excluded.lease_expires_at_ms);
END;
-- next statement
CREATE TRIGGER source_lease_custody_out_update AFTER UPDATE ON source_leases
WHEN NEW.owner_kind IN ('lease','offline')
  AND EXISTS(SELECT 1 FROM background_source_custody WHERE owner_key=NEW.lease_id)
BEGIN
  INSERT INTO background_source_custody(custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,phase,started_at_ms,updated_at_ms,lease_expires_at_ms)
  VALUES(NEW.lease_id,NEW.lease_id,NEW.owner_json,NEW.resource_keys_json,NEW.owner_kind,NEW.work_id,NEW.fence,
    NEW.phase,NEW.started_at_ms,NEW.renewed_at_ms,NEW.expires_at_ms)
  ON CONFLICT(owner_key) DO UPDATE SET
    owner_json=excluded.owner_json,resource_keys_json=excluded.resource_keys_json,owner_kind=excluded.owner_kind,
    work_id=excluded.work_id,fence=excluded.fence,phase=excluded.phase,started_at_ms=excluded.started_at_ms,
    updated_at_ms=excluded.updated_at_ms,lease_expires_at_ms=excluded.lease_expires_at_ms
  WHERE background_source_custody.owner_kind=excluded.owner_kind
    AND (background_source_custody.owner_json,background_source_custody.resource_keys_json,
      background_source_custody.work_id,background_source_custody.fence,background_source_custody.phase,
      background_source_custody.started_at_ms,background_source_custody.updated_at_ms,
      background_source_custody.lease_expires_at_ms)
      IS NOT (excluded.owner_json,excluded.resource_keys_json,excluded.work_id,excluded.fence,
      excluded.phase,excluded.started_at_ms,excluded.updated_at_ms,excluded.lease_expires_at_ms);
END;
-- next statement
CREATE TRIGGER source_lease_custody_out_delete AFTER DELETE ON source_leases
WHEN OLD.owner_kind IN ('lease','offline')
BEGIN
  DELETE FROM background_source_custody WHERE custody_id=OLD.lease_id
    AND owner_key=OLD.lease_id AND owner_kind=OLD.owner_kind;
END;


-- next statement
-- Immutable physical scope survives its owner for the rest of the receipt
-- epoch. It owns no reader and has no FK/pin on a file, library, or lease.
CREATE VIEW source_lease_service_scopes AS
WITH commands AS NOT MATERIALIZED (
 SELECT command.id,command.request_json,command.result_json,
   json_extract(command.request_json,'$.kind') AS kind,
   CASE json_extract(command.request_json,'$.kind') WHEN 'companion'
     THEN json_extract(command.request_json,'$.token.owner.owner_id')
     ELSE json_extract(command.request_json,'$.owner_key') END AS direct_lease,
   CASE json_extract(command.request_json,'$.kind')
     WHEN 'read_attestation' THEN json_extract(command.request_json,'$.file_id')
     WHEN 'companion' THEN json_extract(command.request_json,'$.token.owner.file_id')
     WHEN 'job' THEN json_extract(job.payload_json,'$.file_id')
     WHEN 'analysis' THEN analysis.file_id WHEN 'offline' THEN package.file_id END AS file_id,
   COALESCE(json_extract(job.payload_json,'$.library_id'),job_item.library_id) AS scope_library
 FROM background_job_commands command
 LEFT JOIN background_jobs job ON json_extract(command.request_json,'$.kind')='job'
   AND job.id=json_extract(command.request_json,'$.token.job_id')
 LEFT JOIN items job_item ON job_item.id=json_extract(job.payload_json,'$.item_id')
 LEFT JOIN analysis_requests analysis ON json_extract(command.request_json,'$.kind')='analysis'
   AND analysis.request_id=json_extract(command.request_json,'$.request_id')
 LEFT JOIN offline_packages package ON json_extract(command.request_json,'$.kind')='offline'
   AND package.id=json_extract(command.request_json,'$.package_id')
 WHERE command.operation='source_service' AND json_type(command.result_json)='object'
), scopes AS NOT MATERIALIZED (
 SELECT command.*,COALESCE(item.library_id,command.scope_library) AS library_id,file.path AS source_path
 FROM commands command LEFT JOIN files file ON file.id=command.file_id LEFT JOIN items item ON item.id=file.item_id
)
SELECT id,result_json,direct_lease AS lease_id,library_id,source_path FROM scopes
WHERE kind IN ('job','read_attestation','offline','companion') AND direct_lease IS NOT NULL
UNION ALL
SELECT scope.id,scope.result_json,held.lease_id,scope.library_id,scope.source_path
FROM scopes scope JOIN source_leases held ON held.owner_kind='analysis'
 AND held.work_id=json_extract(scope.request_json,'$.request_id') AND held.fence=json_extract(scope.request_json,'$.fence')
 AND (json_extract(scope.request_json,'$.custody_id') IS NULL OR held.lease_id=json_extract(scope.request_json,'$.custody_id'))
WHERE scope.kind='analysis';
-- next statement
CREATE VIEW source_lease_receipt_resources AS
WITH matched AS NOT MATERIALIZED (
 SELECT DISTINCT scope.receipt_id,scope.lease_id,root.library_id,root.root_id
 FROM source_lease_receipt_sources scope JOIN source_lease_roots root ON root.library_id=scope.library_id
 WHERE scope.source_path IS NULL OR (
  (scope.source_path=root.root_path OR (root.root_path='/' AND substr(scope.source_path,1,1)='/')
    OR (substr(scope.source_path,1,length(root.root_path))=root.root_path AND substr(scope.source_path,length(root.root_path)+1,1)='/'))
  AND NOT EXISTS(SELECT 1 FROM source_lease_roots nested WHERE nested.library_id=root.library_id
    AND length(nested.root_path)>length(root.root_path)
    AND (scope.source_path=nested.root_path OR (substr(scope.source_path,1,length(nested.root_path))=nested.root_path
      AND substr(scope.source_path,length(nested.root_path)+1,1)='/'))))
)
SELECT DISTINCT matched.receipt_id,resource.resource_key FROM matched JOIN source_lease_root_resources resource
 ON resource.library_id=matched.library_id AND resource.root_id=matched.root_id
UNION
SELECT scope.receipt_id,'source_io' FROM source_lease_receipt_sources scope
WHERE NOT EXISTS(SELECT 1 FROM matched WHERE matched.receipt_id=scope.receipt_id AND matched.lease_id=scope.lease_id);
-- next statement
-- Existing v1 receipts use domain/generic keys. Resolve their live owners;
-- opaque companion keys conservatively retain all possible matching scopes
-- only until this receipt epoch is retired.
INSERT OR IGNORE INTO source_lease_receipt_sources(receipt_id,lease_id,library_id,source_path)
SELECT receipt.receipt_id,held.lease_id,COALESCE(item.library_id,json_extract(job.payload_json,'$.library_id'),job_item.library_id),file.path
FROM background_source_service_grants receipt JOIN source_leases held
 ON held.lease_id=receipt.owner_key
 OR (held.owner_kind='analysis' AND 'analysis:'||hex(held.work_id)||':'||held.fence=receipt.owner_key)
 OR (receipt.owner_key LIKE 'companion:%' AND held.owner_kind='playback' AND EXISTS(
   SELECT 1 FROM json_each(receipt.grant_json,'$.resource_keys') a,json_each(held.resource_keys_json) b
   WHERE a.value=b.value OR a.value='source_io' OR b.value='source_io'))
LEFT JOIN background_jobs job ON held.owner_kind='job' AND job.id=held.work_id
LEFT JOIN items job_item ON job_item.id=json_extract(job.payload_json,'$.item_id')
LEFT JOIN analysis_requests analysis ON held.owner_kind='analysis' AND analysis.request_id=held.work_id
LEFT JOIN offline_packages package ON held.owner_kind='offline' AND package.id=held.work_id
LEFT JOIN files file ON file.id=COALESCE(json_extract(held.owner_json,'$.file_id'),json_extract(job.payload_json,'$.file_id'),analysis.file_id,package.file_id)
LEFT JOIN items item ON item.id=file.item_id;
-- next statement
-- Capture once per physical lease. Grant UPSERT replay must not attempt a
-- duplicate insert under the outer update conflict policy or rewrite provenance.
CREATE TRIGGER source_lease_receipt_insert AFTER INSERT ON background_source_service_grants
BEGIN
 INSERT INTO source_lease_receipt_sources(receipt_id,lease_id,library_id,source_path)
 SELECT NEW.receipt_id,scope.lease_id,scope.library_id,scope.source_path FROM source_lease_service_scopes scope
 WHERE json_extract(scope.result_json,'$.receipt_id')=NEW.receipt_id
 AND NOT EXISTS(SELECT 1 FROM source_lease_receipt_sources recorded
   WHERE recorded.receipt_id=NEW.receipt_id AND recorded.lease_id=scope.lease_id);
END;
-- next statement
CREATE TRIGGER source_lease_receipt_update AFTER UPDATE ON background_source_service_grants
BEGIN
 INSERT INTO source_lease_receipt_sources(receipt_id,lease_id,library_id,source_path)
 SELECT NEW.receipt_id,scope.lease_id,scope.library_id,scope.source_path FROM source_lease_service_scopes scope
 WHERE json_extract(scope.result_json,'$.receipt_id')=NEW.receipt_id
 AND NOT EXISTS(SELECT 1 FROM source_lease_receipt_sources recorded
   WHERE recorded.receipt_id=NEW.receipt_id AND recorded.lease_id=scope.lease_id);
END;
