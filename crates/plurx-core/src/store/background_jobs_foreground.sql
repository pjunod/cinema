-- Foreground source custody extends the existing domain arbiter. A foreground
-- registration never waits for an old maintenance reader to stop or reap.
CREATE TABLE IF NOT EXISTS foreground_source_owners (
    owner_id TEXT PRIMARY KEY,
    identity_json TEXT NOT NULL CHECK(json_valid(identity_json)),
    resource_keys_json TEXT NOT NULL CHECK(json_valid(resource_keys_json)),
    fence INTEGER NOT NULL CHECK(fence>0),
    phase TEXT NOT NULL CHECK(phase IN ('startup','refill','sharing','transfer_service','source_idle','cleanup_pending')),
    projection_json TEXT NOT NULL CHECK(json_valid(projection_json)),
    started_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    lease_expires_at_ms INTEGER NOT NULL,
    settlement_proof TEXT CHECK(settlement_proof IN ('no_reader','aborted','joined')) DEFAULT 'no_reader',
    activation_id TEXT
) STRICT;
-- next statement
CREATE VIEW IF NOT EXISTS foreground_source_required_resources AS
SELECT owner.owner_id, resource.value AS resource_key
FROM foreground_source_owners owner, json_each(owner.resource_keys_json) resource;
-- next statement
CREATE INDEX IF NOT EXISTS foreground_source_owner_lease ON foreground_source_owners(lease_expires_at_ms);
-- next statement
CREATE TABLE IF NOT EXISTS background_source_pauses (
    job_id TEXT PRIMARY KEY REFERENCES background_jobs(id) ON DELETE CASCADE,
    fence INTEGER NOT NULL,
    node_id TEXT NOT NULL,
    boot_id TEXT NOT NULL,
    claim_id TEXT NOT NULL,
    resource_keys_json TEXT NOT NULL CHECK(json_valid(resource_keys_json)),
    phase TEXT NOT NULL CHECK(phase IN ('paused','active')),
    paused_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_source_pause_cleanup
AFTER UPDATE ON background_jobs
WHEN NEW.fence!=OLD.fence OR NEW.state NOT IN ('running','cancelling')
BEGIN
    DELETE FROM background_source_pauses WHERE job_id=OLD.id AND fence=OLD.fence;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_source_pause_effects
AFTER INSERT ON background_job_commands WHEN NEW.operation='source_pause' AND NEW.result_json='true'
BEGIN
    INSERT INTO background_source_pauses(job_id,fence,node_id,boot_id,claim_id,resource_keys_json,phase,paused_at_ms)
    SELECT job.id,job.fence,job.owner_node_id,job.owner_boot_id,job.claim_id,
        COALESCE((SELECT json_group_array(resource_key) FROM background_job_reservations held
            WHERE held.job_id=job.id AND held.fence=job.fence AND held.resource_key LIKE 'source_io%'), '[]'),
        'paused',json_extract(NEW.request_json,'$.now_ms')
    FROM background_jobs job WHERE job.id=json_extract(NEW.request_json,'$.token.job_id')
    ON CONFLICT(job_id) DO UPDATE SET phase='paused',paused_at_ms=excluded.paused_at_ms;
    UPDATE background_source_custody SET phase='paused',updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
      WHERE owner_kind='job' AND work_id=json_extract(NEW.request_json,'$.token.job_id') AND fence=json_extract(NEW.request_json,'$.token.fence');
    DELETE FROM background_job_reservations WHERE job_id=json_extract(NEW.request_json,'$.token.job_id')
        AND fence=json_extract(NEW.request_json,'$.token.fence') AND resource_key LIKE 'source_io%';
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_source_resume_effects
AFTER INSERT ON background_job_commands WHEN NEW.operation='source_resume' AND NEW.result_json='true'
BEGIN
    DELETE FROM background_job_reservations WHERE expires_at_ms<=json_extract(NEW.request_json,'$.now_ms');
    INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms)
    SELECT resource.value,MIN(candidate.slot),paused.job_id,paused.fence,job.lease_expires_ms
    FROM background_source_pauses paused JOIN background_jobs job ON job.id=paused.job_id,
        json_each(paused.resource_keys_json) resource,
        (SELECT 0 AS slot UNION ALL SELECT 1) candidate
    WHERE paused.job_id=json_extract(NEW.request_json,'$.token.job_id') AND paused.phase='paused'
      AND NOT EXISTS(SELECT 1 FROM background_job_reservations held WHERE held.resource_key=resource.value
          AND held.slot=candidate.slot AND held.expires_at_ms>json_extract(NEW.request_json,'$.now_ms'))
    GROUP BY resource.value;
    UPDATE background_source_custody SET phase='active',updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
      WHERE owner_kind='job' AND work_id=json_extract(NEW.request_json,'$.token.job_id') AND fence=json_extract(NEW.request_json,'$.token.fence');
    UPDATE background_source_pauses SET phase='active'
        WHERE job_id=json_extract(NEW.request_json,'$.token.job_id');
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
CREATE TABLE IF NOT EXISTS background_source_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=1)) STRICT;
-- next statement
INSERT INTO background_source_schema(singleton,version) VALUES(1,1) ON CONFLICT(singleton) DO NOTHING;
-- next statement
CREATE VIEW IF NOT EXISTS foreground_source_permissions AS
SELECT owner.owner_id,resource.resource_key,owner.lease_expires_at_ms,
    CASE WHEN json_extract(owner.projection_json,'$.source_quiescent')=1 THEN 0 ELSE 1 END AS custody_held,
    CASE WHEN owner.phase='source_idle' AND json_extract(owner.projection_json,'$.source_quiescent')=1 THEN owner.lease_expires_at_ms
      WHEN owner.phase='sharing' AND json_extract(owner.projection_json,'$.source_quiescent')=1
        AND json_extract(owner.projection_json,'$.contiguous_lead_ms')>json_extract(owner.projection_json,'$.low_ms')
        THEN MIN(owner.lease_expires_at_ms,json_extract(owner.projection_json,'$.observation_expires_at_ms'))
      WHEN owner.phase='transfer_service' THEN MIN(owner.lease_expires_at_ms,json_extract(owner.projection_json,'$.transfer_grant.expires_at_ms'))
      ELSE 0 END AS permits_through_ms
FROM foreground_source_owners owner JOIN foreground_source_required_resources resource ON resource.owner_id=owner.owner_id;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_source_refusal_cleanup
AFTER INSERT ON background_job_commands WHEN NEW.operation IN ('source_pause','source_resume') AND NEW.result_json='false'
BEGIN
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
-- This receipt is physical custody, not a renewable demand lease. History
-- deletion, a new job fence and a clock expiry cannot attest that its reads or
-- process group stopped. Only the exact settlement receipt deletes it.
CREATE TABLE IF NOT EXISTS background_source_custody (
    custody_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL UNIQUE,
    owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
    resource_keys_json TEXT NOT NULL CHECK(json_valid(resource_keys_json)),
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('job','analysis','lease','offline')),
    work_id TEXT NOT NULL,
    fence INTEGER NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('active','paused','cleanup_pending')),
    started_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    lease_expires_at_ms INTEGER NOT NULL,
    settlement_proof TEXT CHECK(settlement_proof IN ('no_reader','aborted','joined')) DEFAULT 'no_reader',
    activation_id TEXT
) STRICT;
-- next statement
CREATE VIEW IF NOT EXISTS source_io_occupancy AS
SELECT held.resource_key,'job' AS owner_kind,held.job_id AS work_id,held.fence,held.expires_at_ms,
    custody.custody_id,CASE WHEN custody.phase='paused' THEN 0 ELSE 1 END AS active_occupancy
FROM background_job_reservations held LEFT JOIN background_source_custody custody
    ON custody.owner_kind='job' AND custody.work_id=held.job_id AND custody.fence=held.fence
UNION ALL
SELECT held.resource_key,'analysis',held.request_id,held.fence,held.expires_at_ms,
    custody.custody_id,CASE WHEN custody.phase='paused' THEN 0 ELSE 1 END
FROM analysis_source_reservations held LEFT JOIN background_source_custody custody
    ON custody.owner_kind='analysis' AND custody.work_id=held.request_id AND custody.fence=held.fence
UNION ALL
SELECT resource.value,custody.owner_kind,custody.work_id,custody.fence,custody.authority_expires_at_ms,custody.custody_id,
    CASE WHEN custody.phase='paused' THEN 0 ELSE 1 END
FROM background_source_current_owners custody,json_each(custody.resource_keys_json) resource
WHERE NOT EXISTS(SELECT 1 FROM background_job_reservations held WHERE custody.owner_kind='job'
    AND held.job_id=custody.work_id AND held.fence=custody.fence AND held.resource_key=resource.value)
  AND NOT EXISTS(SELECT 1 FROM analysis_source_reservations held WHERE custody.owner_kind='analysis'
    AND held.request_id=custody.work_id AND held.fence=custody.fence AND held.resource_key=resource.value);
-- next statement
-- Authority expiry diagnoses custody; it never deletes the physical receipt.
CREATE VIEW IF NOT EXISTS background_source_current_owners AS
SELECT custody.*,
  COALESCE(CASE custody.owner_kind
    WHEN 'job' THEN (SELECT job.lease_expires_ms FROM background_jobs job WHERE job.id=custody.work_id AND job.fence=custody.fence
      AND job.state='running' AND job.owner_node_id=json_extract(custody.owner_json,'$.token.node_id')
      AND job.owner_boot_id=json_extract(custody.owner_json,'$.token.boot_id') AND job.claim_id=json_extract(custody.owner_json,'$.token.claim_id'))
    WHEN 'analysis' THEN (SELECT request.lease_expires_ms FROM analysis_requests request WHERE request.request_id=custody.work_id
      AND request.fence=custody.fence AND request.state='running' AND request.cancel_requested=0 AND request.owner_node_id=json_extract(custody.owner_json,'$.node_id'))
    WHEN 'lease' THEN (SELECT lease.expires_at_ms FROM job_leases lease JOIN files file ON file.id=json_extract(custody.owner_json,'$.file_id')
      AND file.size=json_extract(custody.owner_json,'$.source_size') AND file.mtime=json_extract(custody.owner_json,'$.source_mtime')
      WHERE lease.resource=custody.work_id AND lease.fence=custody.fence AND lease.owner_node_id=json_extract(custody.owner_json,'$.lease.owner_node_id'))
    WHEN 'offline' THEN (SELECT package.expires_at*1000 FROM offline_packages package JOIN files file ON file.id=package.file_id
      AND file.size=package.source_size AND file.mtime=package.source_mtime WHERE package.id=custody.work_id
      AND package.claim_generation=custody.fence AND package.node_id=json_extract(custody.owner_json,'$.node_id') AND package.state='preparing')
    ELSE 0 END,0) AS authority_expires_at_ms
FROM background_source_custody custody;
