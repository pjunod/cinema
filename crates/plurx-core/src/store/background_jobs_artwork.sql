CREATE TABLE IF NOT EXISTS background_artwork_locations (
    artifact_key TEXT NOT NULL,
    node_id TEXT NOT NULL,
    spec_json TEXT NOT NULL CHECK(json_valid(spec_json)),
    blob_sha256 TEXT NOT NULL,
    bytes INTEGER NOT NULL CHECK(bytes > 0),
    built_by_node_id TEXT NOT NULL,
    built_at_ms INTEGER NOT NULL,
    verified_at_ms INTEGER NOT NULL,
    PRIMARY KEY(artifact_key, node_id)
);
-- next statement
CREATE INDEX IF NOT EXISTS background_artwork_locations_age ON background_artwork_locations(verified_at_ms);
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_publish_artwork_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_artwork'
BEGIN
    INSERT INTO background_artwork_locations(artifact_key,node_id,spec_json,blob_sha256,bytes,built_by_node_id,built_at_ms,verified_at_ms)
    SELECT json_extract(NEW.request_json,'$.location.artifact_key'),json_extract(NEW.request_json,'$.token.node_id'),
        json_extract(NEW.request_json,'$.location.spec'),json_extract(NEW.request_json,'$.location.blob_sha256'),
        json_extract(NEW.request_json,'$.location.bytes'),json_extract(NEW.request_json,'$.location.built_by_node_id'),
        json_extract(NEW.request_json,'$.location.built_at_ms'),json_extract(NEW.request_json,'$.now_ms')
    WHERE json_extract(NEW.result_json,'$.outcome') = 'published'
    ON CONFLICT(artifact_key,node_id) DO UPDATE SET verified_at_ms = excluded.verified_at_ms, spec_json = excluded.spec_json;

    UPDATE background_jobs SET state = 'succeeded', result_ref = json_extract(NEW.result_json,'$.result_ref'),
        owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE id = json_extract(NEW.result_json,'$.job_id') AND json_extract(NEW.result_json,'$.outcome') = 'published';

    UPDATE background_job_waiters SET state = CASE
        WHEN deadline_ms <= json_extract(NEW.request_json,'$.now_ms') THEN 'cancelled'
        WHEN target_node_id IS NULL OR target_node_id = json_extract(NEW.request_json,'$.token.node_id') THEN 'succeeded'
        ELSE 'awaiting_hydration' END,
        result_ref = json_extract(NEW.result_json,'$.result_ref'), updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE job_id = json_extract(NEW.result_json,'$.job_id') AND state = 'pending'
        AND json_extract(NEW.result_json,'$.outcome') = 'published';

    UPDATE background_job_waiters SET state = 'succeeded', result_ref = json_extract(NEW.result_json,'$.result_ref'),
        updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE state = 'awaiting_hydration' AND (deadline_ms IS NULL OR deadline_ms > json_extract(NEW.request_json,'$.now_ms'))
        AND target_node_id = json_extract(NEW.request_json,'$.token.node_id')
        AND CASE WHEN json_valid(result_ref) THEN json_extract(result_ref,'$.artifact_kind') END = 'artwork'
        AND CASE WHEN json_valid(result_ref) THEN json_extract(result_ref,'$.artifact_key') END = json_extract(NEW.request_json,'$.location.artifact_key')
        AND json_extract(NEW.result_json,'$.outcome') = 'published';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_artwork_retention
BEFORE INSERT ON background_job_commands WHEN NEW.operation = 'maintain'
BEGIN
    DELETE FROM background_artwork_locations WHERE (artifact_key,node_id) IN (
        SELECT artifact_key,node_id FROM background_artwork_locations location
        WHERE verified_at_ms <= json_extract(NEW.request_json,'$.now_ms') - 604800000
          AND NOT EXISTS (SELECT 1 FROM background_jobs job WHERE job.state IN ('queued','running','cancelling')
            AND ((job.kind = 'artwork_derivative' AND json_extract(job.payload_json,'$.artifact_key') = location.artifact_key)
              OR (job.kind = 'artifact_hydrate' AND json_extract(job.payload_json,'$.artifact_key') = 'artwork:' || location.artifact_key)))
        ORDER BY verified_at_ms,artifact_key,node_id LIMIT 128
    );
END;
