-- Original typed producer intent survives retirement of its execution history.
ALTER TABLE background_transcode_artifacts ADD COLUMN producer_payload TEXT
    CHECK (producer_payload IS NULL OR (json_valid(producer_payload) AND length(CAST(producer_payload AS BLOB)) <= 16384));
-- next statement
CREATE TRIGGER IF NOT EXISTS background_transcode_producer_recorded
AFTER INSERT ON background_transcode_artifacts WHEN NEW.producer_payload IS NULL
BEGIN
    UPDATE background_transcode_artifacts SET producer_payload = (
        SELECT job.payload_json FROM background_job_commands command JOIN background_jobs job
          ON job.id = json_extract(command.request_json,'$.token.job_id')
        WHERE command.operation = 'publish_transcode' AND job.kind = 'transcode_prepare' AND job.payload_version = 1
          AND json_extract(command.result_json,'$.outcome') = 'published'
          AND json_extract(command.request_json,'$.output.recipe_hash') = NEW.recipe_hash
          AND json_extract(command.request_json,'$.output.manifest_digest') = NEW.manifest_digest LIMIT 1)
    WHERE recipe_hash = NEW.recipe_hash AND manifest_digest = NEW.manifest_digest;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_transcode_producer_backfill
BEFORE INSERT ON background_job_commands WHEN NEW.operation = 'maintain'
BEGIN
    UPDATE background_transcode_artifacts SET producer_payload = (
        SELECT job.payload_json FROM background_jobs job WHERE job.kind = 'transcode_prepare' AND job.payload_version = 1
          AND job.state = 'succeeded' AND json_valid(job.result_ref)
          AND json_extract(job.result_ref,'$.recipe_hash') = background_transcode_artifacts.recipe_hash
          AND json_extract(job.result_ref,'$.manifest_digest') = background_transcode_artifacts.manifest_digest
          AND json_extract(job.payload_json,'$.file_id') = background_transcode_artifacts.file_id
          AND json_extract(job.payload_json,'$.source_size') = background_transcode_artifacts.source_size
          AND json_extract(job.payload_json,'$.source_mtime') = background_transcode_artifacts.source_mtime
        ORDER BY job.updated_at_ms DESC,job.id DESC LIMIT 1)
    WHERE (recipe_hash,manifest_digest) IN (
        SELECT artifact.recipe_hash,artifact.manifest_digest FROM background_transcode_artifacts artifact
        WHERE artifact.producer_payload IS NULL AND EXISTS (SELECT 1 FROM background_jobs job
          WHERE job.kind = 'transcode_prepare' AND job.state = 'succeeded' AND job.payload_version = 1
            AND json_valid(job.result_ref) AND json_extract(job.result_ref,'$.recipe_hash') = artifact.recipe_hash
            AND json_extract(job.result_ref,'$.manifest_digest') = artifact.manifest_digest)
        ORDER BY artifact.built_at_ms,artifact.recipe_hash,artifact.manifest_digest LIMIT 128);
END;
-- next statement
CREATE TABLE IF NOT EXISTS background_artifact_repairs (
    id TEXT PRIMARY KEY,
    original_key TEXT NOT NULL,
    target_node_id TEXT NOT NULL,
    location_generation TEXT NOT NULL,
    artifact_key TEXT NOT NULL,
    producer_payload TEXT CHECK (producer_payload IS NULL OR (json_valid(producer_payload) AND length(CAST(producer_payload AS BLOB)) <= 16384)),
    phase TEXT NOT NULL CHECK (phase IN ('copy','copying','build','building','deliver','delivering','ready','failed')),
    job_id TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    expires_ms INTEGER NOT NULL,
    UNIQUE(original_key,target_node_id,location_generation)
);
-- next statement
CREATE INDEX IF NOT EXISTS background_artifact_repairs_due ON background_artifact_repairs(phase,updated_at_ms,id);
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_verify_transcode_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'verify_transcode'
BEGIN
    INSERT INTO background_artifact_repairs(id,original_key,target_node_id,location_generation,artifact_key,producer_payload,phase,created_at_ms,updated_at_ms,expires_ms)
    SELECT json_extract(NEW.request_json,'$.token.job_id'),json_extract(NEW.request_json,'$.artifact_key'),
        json_extract(NEW.request_json,'$.token.node_id'),json_extract(NEW.request_json,'$.location_generation'),
        json_extract(NEW.request_json,'$.artifact_key'),
        (SELECT producer_payload FROM background_transcode_artifacts WHERE recipe_hash = json_extract(NEW.request_json,'$.recipe_hash')
          AND manifest_digest = json_extract(NEW.request_json,'$.manifest_digest')),
        'copy',json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms') + 604800000
    WHERE json_extract(NEW.result_json,'$.outcome') = 'published' AND json_extract(NEW.request_json,'$.valid') = 0
      AND (SELECT COUNT(*) FROM background_artifact_repairs) < 4096
    ON CONFLICT(original_key,target_node_id,location_generation) DO NOTHING;
    UPDATE offline_packages SET state = 'failed',phase = 'integrity',error_code = 'cache_integrity',
        error_message = 'Prepared media failed its generation integrity check.',updated_at = json_extract(NEW.request_json,'$.now_ms') / 1000
    WHERE node_id = json_extract(NEW.request_json,'$.token.node_id') AND recipe_hash = json_extract(NEW.request_json,'$.recipe_hash')
      AND state = 'ready' AND json_extract(NEW.result_json,'$.outcome') = 'published' AND json_extract(NEW.request_json,'$.valid') = 0;
    DELETE FROM cache_consumer_pins WHERE EXISTS (SELECT 1 FROM transcode_cache_locations location
        WHERE location.recipe_hash = json_extract(NEW.request_json,'$.recipe_hash') AND location.node_id = json_extract(NEW.request_json,'$.token.node_id')
          AND location.storage_class = 'local' AND location.relative_dir = json_extract(NEW.request_json,'$.relative_dir')
          AND cache_consumer_pins.storage_id = location.storage_id AND cache_consumer_pins.recipe_hash = location.recipe_hash
          AND cache_consumer_pins.generation_id = location.generation_id)
      AND json_extract(NEW.result_json,'$.outcome') = 'published' AND json_extract(NEW.request_json,'$.valid') = 0;
    DELETE FROM transcode_cache_locations WHERE recipe_hash = json_extract(NEW.request_json,'$.recipe_hash')
      AND node_id = json_extract(NEW.request_json,'$.token.node_id') AND storage_class = 'local'
      AND relative_dir = json_extract(NEW.request_json,'$.relative_dir') AND manifest_digest = json_extract(NEW.request_json,'$.manifest_digest')
      AND publication_generation = json_extract(NEW.request_json,'$.publication_generation')
      AND json_extract(NEW.result_json,'$.outcome') = 'published' AND json_extract(NEW.request_json,'$.valid') = 0;
    DELETE FROM transcode_cache_recipes WHERE recipe_hash = json_extract(NEW.request_json,'$.recipe_hash')
      AND NOT EXISTS (SELECT 1 FROM transcode_cache_locations WHERE recipe_hash = transcode_cache_recipes.recipe_hash)
      AND json_extract(NEW.result_json,'$.outcome') = 'published' AND json_extract(NEW.request_json,'$.valid') = 0;
    UPDATE transcode_cache_locations SET last_seen_at = json_extract(NEW.request_json,'$.now_ms') / 1000,
        scrub_object_index = json_extract(NEW.request_json,'$.next_object_index')
    WHERE recipe_hash = json_extract(NEW.request_json,'$.recipe_hash') AND node_id = json_extract(NEW.request_json,'$.token.node_id')
      AND storage_class = 'local' AND relative_dir = json_extract(NEW.request_json,'$.relative_dir')
      AND manifest_digest = json_extract(NEW.request_json,'$.manifest_digest')
      AND publication_generation = json_extract(NEW.request_json,'$.publication_generation')
      AND json_extract(NEW.result_json,'$.outcome') = 'published' AND json_extract(NEW.request_json,'$.valid') = 1;
    UPDATE background_jobs SET state = 'succeeded',result_ref = json_extract(NEW.result_json,'$.result_ref'),
        last_error_code = CASE WHEN json_extract(NEW.request_json,'$.valid') = 0 AND NOT EXISTS (
            SELECT 1 FROM background_artifact_repairs WHERE original_key = json_extract(NEW.request_json,'$.artifact_key')
              AND target_node_id = json_extract(NEW.request_json,'$.token.node_id')
              AND location_generation = json_extract(NEW.request_json,'$.location_generation')) THEN 'repair_capacity' ELSE NULL END,
        owner_node_id = NULL,owner_boot_id = NULL,claim_id = NULL,lease_expires_ms = NULL,
        revision = revision + 1,updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE id = json_extract(NEW.result_json,'$.job_id') AND json_extract(NEW.result_json,'$.outcome') = 'published';
    UPDATE background_job_waiters SET state = CASE WHEN deadline_ms <= json_extract(NEW.request_json,'$.now_ms') THEN 'cancelled' ELSE 'succeeded' END,
        result_ref = json_extract(NEW.result_json,'$.result_ref'),updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE job_id = json_extract(NEW.result_json,'$.job_id') AND state = 'pending' AND json_extract(NEW.result_json,'$.outcome') = 'published';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_artifact_repair_admitted
AFTER INSERT ON background_job_waiters
WHEN NEW.request_scope = 'artifact-repair' AND EXISTS (SELECT 1 FROM background_job_commands command
    WHERE command.operation = 'enqueue' AND json_type(command.request_json,'$.repair_id') = 'text'
      AND json_extract(command.request_json,'$.request.request_id') = NEW.request_id)
BEGIN
    UPDATE background_artifact_repairs SET phase = CASE phase WHEN 'copy' THEN 'copying' WHEN 'build' THEN 'building' WHEN 'deliver' THEN 'delivering' END,
        job_id = NEW.job_id,updated_at_ms = NEW.created_at_ms
    WHERE id = (SELECT json_extract(request_json,'$.repair_id') FROM background_job_commands
        WHERE operation = 'enqueue' AND json_extract(request_json,'$.request.request_id') = NEW.request_id)
      AND phase IN ('copy','build','deliver');
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_artifact_repair_advance
BEFORE INSERT ON background_job_commands WHEN NEW.operation = 'maintain'
BEGIN
    UPDATE background_artifact_repairs SET
        artifact_key = CASE WHEN phase = 'building' AND (
            SELECT state FROM background_job_waiters waiter WHERE waiter.request_scope = 'artifact-repair'
              AND waiter.request_id = background_artifact_repairs.id || ':build') = 'succeeded'
          THEN (SELECT COALESCE(CASE json_extract(waiter.result_ref,'$.artifact_kind') WHEN 'artwork' THEN 'artwork:' END || json_extract(waiter.result_ref,'$.artifact_key'),
              'transcode:' || json_extract(waiter.result_ref,'$.recipe_hash') || ':' || json_extract(waiter.result_ref,'$.manifest_digest'))
            FROM background_job_waiters waiter WHERE waiter.request_scope = 'artifact-repair' AND waiter.request_id = background_artifact_repairs.id || ':build')
          ELSE artifact_key END,
        phase = CASE
          WHEN expires_ms <= json_extract(NEW.request_json,'$.now_ms') OR EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || target_node_id) OR (phase NOT IN ('ready','failed') AND json_extract(producer_payload,'$.kind') = 'transcode_prepare' AND NOT EXISTS (SELECT 1 FROM files WHERE id = json_extract(producer_payload,'$.file_id') AND size = json_extract(producer_payload,'$.source_size') AND mtime = json_extract(producer_payload,'$.source_mtime'))) THEN 'failed'
          WHEN (SELECT state FROM background_job_waiters waiter WHERE waiter.request_scope = 'artifact-repair'
            AND waiter.request_id = background_artifact_repairs.id || ':' || CASE background_artifact_repairs.phase WHEN 'copying' THEN 'copy' WHEN 'building' THEN 'build' ELSE 'deliver' END) = 'succeeded'
            THEN CASE phase WHEN 'building' THEN 'deliver' ELSE 'ready' END
          ELSE CASE WHEN phase = 'copying' AND producer_payload IS NOT NULL AND (SELECT state FROM background_job_waiters WHERE request_scope = 'artifact-repair' AND request_id = background_artifact_repairs.id || ':copy') = 'failed' THEN 'build' ELSE 'failed' END END,
        updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE id IN (SELECT id FROM background_artifact_repairs WHERE phase NOT IN ('ready','failed') AND (expires_ms <= json_extract(NEW.request_json,'$.now_ms')
      OR EXISTS (SELECT 1 FROM settings WHERE key = 'internal.cluster_job_owner_removed.' || target_node_id)
      OR (phase NOT IN ('ready','failed') AND json_extract(producer_payload,'$.kind') = 'transcode_prepare' AND NOT EXISTS (SELECT 1 FROM files WHERE id = json_extract(producer_payload,'$.file_id') AND size = json_extract(producer_payload,'$.source_size') AND mtime = json_extract(producer_payload,'$.source_mtime')))
      OR (phase IN ('copying','building','delivering') AND EXISTS (SELECT 1 FROM background_job_waiters waiter
        WHERE waiter.request_scope = 'artifact-repair' AND waiter.request_id = background_artifact_repairs.id || ':' ||
            CASE background_artifact_repairs.phase WHEN 'copying' THEN 'copy' WHEN 'building' THEN 'build' ELSE 'deliver' END
          AND waiter.state IN ('succeeded','failed','cancelled')))) ORDER BY updated_at_ms,id LIMIT 128);
    DELETE FROM background_artifact_repairs WHERE id IN (SELECT id FROM background_artifact_repairs
      WHERE phase IN ('ready','failed') AND updated_at_ms < json_extract(NEW.request_json,'$.now_ms') - 604800000
      ORDER BY updated_at_ms,id LIMIT 128);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_verify_artwork_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'verify_artwork'
BEGIN
    INSERT INTO background_artifact_repairs(id,original_key,target_node_id,location_generation,artifact_key,producer_payload,phase,created_at_ms,updated_at_ms,expires_ms)
    SELECT json_extract(NEW.request_json,'$.token.job_id'),json_extract(NEW.request_json,'$.artifact_key'),
        json_extract(NEW.request_json,'$.token.node_id'),json_extract(NEW.request_json,'$.location_generation'),
        json_extract(NEW.request_json,'$.artifact_key'),json_extract(NEW.request_json,'$.producer_payload'),
        'copy',json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms') + 604800000
    WHERE json_extract(NEW.result_json,'$.outcome') = 'published' AND json_extract(NEW.request_json,'$.valid') = 0
      AND (SELECT COUNT(*) FROM background_artifact_repairs) < 4096
    ON CONFLICT(original_key,target_node_id,location_generation) DO NOTHING;
    DELETE FROM background_artwork_locations WHERE artifact_key = json_extract(NEW.request_json,'$.location.artifact_key')
      AND node_id = json_extract(NEW.request_json,'$.token.node_id') AND json_extract(NEW.result_json,'$.outcome') = 'published'
      AND json_extract(NEW.request_json,'$.valid') = 0;
    UPDATE background_artwork_locations SET verified_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE artifact_key = json_extract(NEW.request_json,'$.location.artifact_key') AND node_id = json_extract(NEW.request_json,'$.token.node_id')
      AND json_extract(NEW.result_json,'$.outcome') = 'published' AND json_extract(NEW.request_json,'$.valid') = 1;
    UPDATE background_jobs SET state = 'succeeded',result_ref = json_extract(NEW.result_json,'$.result_ref'),
        last_error_code = CASE WHEN json_extract(NEW.request_json,'$.valid') = 0 AND NOT EXISTS (
            SELECT 1 FROM background_artifact_repairs WHERE original_key = json_extract(NEW.request_json,'$.artifact_key')
              AND target_node_id = json_extract(NEW.request_json,'$.token.node_id')
              AND location_generation = json_extract(NEW.request_json,'$.location_generation')) THEN 'repair_capacity' ELSE NULL END,
        owner_node_id = NULL,owner_boot_id = NULL,claim_id = NULL,lease_expires_ms = NULL,
        revision = revision + 1,updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE id = json_extract(NEW.result_json,'$.job_id') AND json_extract(NEW.result_json,'$.outcome') = 'published';
    UPDATE background_job_waiters SET state = CASE WHEN deadline_ms <= json_extract(NEW.request_json,'$.now_ms') THEN 'cancelled' ELSE 'succeeded' END,
        result_ref = json_extract(NEW.result_json,'$.result_ref'),updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE job_id = json_extract(NEW.result_json,'$.job_id') AND state = 'pending' AND json_extract(NEW.result_json,'$.outcome') = 'published';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE INDEX IF NOT EXISTS background_verification_locator ON background_jobs(kind,target_node_id,json_extract(payload_json,'$.artifact_key'),updated_at_ms);
-- next statement
CREATE TRIGGER IF NOT EXISTS background_artifact_repair_stopped
AFTER UPDATE OF phase ON background_artifact_repairs WHEN NEW.phase = 'failed' AND OLD.phase IN ('copying','building','delivering')
BEGIN
    INSERT INTO background_job_commands(id,operation,request_json,result_json)
    SELECT 'repair-stop:' || NEW.id,'cancel_waiter',json_object('scope',waiter.request_scope,'request_id',waiter.request_id,'now_ms',NEW.updated_at_ms),
      json_object('job_id',waiter.job_id,'cancelled',json('true'))
    FROM background_job_waiters waiter WHERE waiter.request_scope = 'artifact-repair'
      AND waiter.request_id = NEW.id || ':' || CASE OLD.phase WHEN 'copying' THEN 'copy' WHEN 'building' THEN 'build' ELSE 'deliver' END
      AND waiter.state IN ('pending','awaiting_hydration');
END;
