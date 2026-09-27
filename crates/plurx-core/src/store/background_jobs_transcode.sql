CREATE TABLE IF NOT EXISTS background_transcode_artifacts (
    recipe_hash TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    source_size INTEGER NOT NULL,
    source_mtime INTEGER NOT NULL,
    recipe_version INTEGER NOT NULL,
    built_by_node_id TEXT NOT NULL,
    built_at_ms INTEGER NOT NULL,
    PRIMARY KEY(recipe_hash, manifest_digest)
);
-- next statement
CREATE INDEX IF NOT EXISTS background_transcode_artifacts_file ON background_transcode_artifacts(file_id);
-- next statement
CREATE INDEX IF NOT EXISTS background_transcode_artifacts_age ON background_transcode_artifacts(built_at_ms);
-- next statement
DROP TRIGGER IF EXISTS background_job_publish_transcode_command;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_publish_transcode_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_transcode'
BEGIN
    INSERT INTO background_transcode_artifacts(recipe_hash,manifest_digest,file_id,source_size,source_mtime,recipe_version,built_by_node_id,built_at_ms)
    SELECT json_extract(NEW.request_json,'$.output.recipe_hash'), json_extract(NEW.request_json,'$.output.manifest_digest'),
        json_extract(job.payload_json,'$.file_id'), json_extract(job.payload_json,'$.source_size'),
        json_extract(job.payload_json,'$.source_mtime'), json_extract(NEW.request_json,'$.output.recipe_version'),
        json_extract(NEW.request_json,'$.token.node_id'), json_extract(NEW.request_json,'$.now_ms')
    FROM background_jobs job WHERE job.id = json_extract(NEW.result_json,'$.job_id')
        AND job.kind = 'transcode_prepare' AND json_extract(NEW.result_json,'$.outcome') = 'published'
    ON CONFLICT(recipe_hash,manifest_digest) DO NOTHING;

    INSERT INTO transcode_cache_recipes (recipe_hash, file_id, recipe_version, created_at)
    SELECT json_extract(NEW.request_json, '$.output.recipe_hash'),
        COALESCE(json_extract(job.payload_json, '$.file_id'),
          (SELECT file_id FROM background_transcode_artifacts WHERE recipe_hash = json_extract(NEW.request_json,'$.output.recipe_hash')
            AND manifest_digest = json_extract(NEW.request_json,'$.output.manifest_digest'))),
        json_extract(NEW.request_json, '$.output.recipe_version'),
        json_extract(NEW.request_json, '$.now_ms') / 1000
    FROM background_jobs job WHERE job.id = json_extract(NEW.result_json, '$.job_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'published'
    ON CONFLICT(recipe_hash) DO NOTHING;

    UPDATE offline_packages SET actual_bytes = actual_bytes + (
        json_extract(NEW.request_json, '$.output.bytes') - json_extract(NEW.request_json, '$.output.expected_previous_bytes')),
        updated_at = json_extract(NEW.request_json, '$.now_ms') / 1000
    WHERE state = 'ready' AND recipe_hash = json_extract(NEW.request_json, '$.output.recipe_hash')
        AND node_id = json_extract(NEW.request_json, '$.token.node_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'published'
        AND json_extract(NEW.request_json, '$.output.expected_previous_bytes') IS NOT NULL
        AND EXISTS (SELECT 1 FROM transcode_cache_locations location
            WHERE location.recipe_hash = offline_packages.recipe_hash
              AND location.node_id = offline_packages.node_id AND location.storage_class = 'local'
              AND location.relative_dir = json_extract(NEW.request_json, '$.output.relative_dir')
              AND location.complete = 1 AND location.manifest_digest IS NULL
              AND location.bytes = json_extract(NEW.request_json, '$.output.expected_previous_bytes'));

    INSERT INTO transcode_cache_locations (
        recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
        manifest_digest, scrub_object_index, publication_generation,
        last_used_at, last_seen_at, storage_id, generation_id)
    SELECT json_extract(NEW.request_json, '$.output.recipe_hash'),
        json_extract(NEW.request_json, '$.token.node_id'), 'local',
        json_extract(NEW.request_json, '$.output.relative_dir'),
        json_extract(NEW.request_json, '$.output.bytes'), 1,
        json_extract(NEW.request_json, '$.output.manifest_digest'), 0, 1,
        json_extract(NEW.request_json, '$.now_ms') / 1000,
        json_extract(NEW.request_json, '$.now_ms') / 1000,
        'node:' || json_extract(NEW.request_json, '$.token.node_id') || ':cache',
        json_extract(NEW.request_json, '$.output.relative_dir')
    WHERE json_extract(NEW.result_json, '$.outcome') = 'published'
    ON CONFLICT(recipe_hash, node_id, storage_class) DO UPDATE SET
        relative_dir = excluded.relative_dir, storage_id = excluded.storage_id,
        generation_id = excluded.generation_id, bytes = excluded.bytes, complete = 1,
        publication_generation = transcode_cache_locations.publication_generation + 1,
        manifest_digest = excluded.manifest_digest, scrub_object_index = 0,
        last_seen_at = MAX(transcode_cache_locations.last_seen_at, excluded.last_seen_at);

    UPDATE background_jobs SET state = 'succeeded',
        result_ref = json_extract(NEW.result_json, '$.result_ref'),
        owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
        revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE id = json_extract(NEW.result_json, '$.job_id')
        AND json_extract(NEW.result_json, '$.outcome') = 'published';

    UPDATE background_job_waiters SET state = CASE
        WHEN deadline_ms <= json_extract(NEW.request_json, '$.now_ms') THEN 'cancelled'
        WHEN target_node_id IS NULL OR target_node_id = json_extract(NEW.request_json, '$.token.node_id')
            THEN 'succeeded' ELSE 'awaiting_hydration' END,
        result_ref = json_extract(NEW.result_json, '$.result_ref'),
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE job_id = json_extract(NEW.result_json, '$.job_id') AND state = 'pending'
        AND json_extract(NEW.result_json, '$.outcome') = 'published';
    UPDATE background_job_waiters SET state = 'succeeded', result_ref = json_extract(NEW.result_json,'$.result_ref'),
        updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE state = 'awaiting_hydration' AND (deadline_ms IS NULL OR deadline_ms > json_extract(NEW.request_json,'$.now_ms'))
        AND target_node_id = json_extract(NEW.request_json,'$.token.node_id')
        AND CASE WHEN json_valid(result_ref) THEN json_extract(result_ref,'$.recipe_hash') END = json_extract(NEW.request_json,'$.output.recipe_hash')
        AND CASE WHEN json_valid(result_ref) THEN json_extract(result_ref,'$.manifest_digest') END = json_extract(NEW.request_json,'$.output.manifest_digest')
        AND json_extract(NEW.result_json,'$.outcome') = 'published';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_transcode_artifacts_retention
BEFORE INSERT ON background_job_commands WHEN NEW.operation = 'maintain'
BEGIN
    DELETE FROM background_transcode_artifacts WHERE (recipe_hash,manifest_digest) IN (
        SELECT recipe_hash,manifest_digest FROM background_transcode_artifacts artifact
        WHERE built_at_ms < json_extract(NEW.request_json,'$.now_ms') - 604800000
          AND NOT EXISTS (SELECT 1 FROM transcode_cache_locations location
            WHERE location.recipe_hash = artifact.recipe_hash AND location.manifest_digest = artifact.manifest_digest)
          AND NOT EXISTS (SELECT 1 FROM background_jobs job WHERE job.state IN ('queued','running','cancelling')
            AND ((job.kind = 'transcode_prepare' AND json_extract(job.payload_json,'$.file_id') = artifact.file_id)
              OR (job.kind = 'artifact_hydrate' AND json_extract(job.payload_json,'$.artifact_key') = 'transcode:' || artifact.recipe_hash || ':' || artifact.manifest_digest)))
        ORDER BY built_at_ms,recipe_hash,manifest_digest LIMIT 128
    );
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_transcode_artifacts_legacy_proof
BEFORE INSERT ON background_job_commands WHEN NEW.operation = 'maintain'
BEGIN
    INSERT INTO background_transcode_artifacts(recipe_hash,manifest_digest,file_id,source_size,source_mtime,recipe_version,built_by_node_id,built_at_ms)
    SELECT location.recipe_hash, location.manifest_digest, file.id, file.size, file.mtime,
        recipe.recipe_version, location.node_id, job.updated_at_ms
FROM background_jobs job JOIN files file ON file.id = json_extract(job.payload_json,'$.file_id')
JOIN transcode_cache_locations location
  ON location.recipe_hash = json_extract(CASE WHEN json_valid(job.result_ref) THEN job.result_ref ELSE '{}' END,'$.recipe_hash')
  AND location.manifest_digest = json_extract(CASE WHEN json_valid(job.result_ref) THEN job.result_ref ELSE '{}' END,'$.manifest_digest')
  AND location.node_id = json_extract(CASE WHEN json_valid(job.result_ref) THEN job.result_ref ELSE '{}' END,'$.node_id')
JOIN transcode_cache_recipes recipe ON recipe.recipe_hash = location.recipe_hash AND recipe.file_id = file.id
WHERE job.kind = 'transcode_prepare' AND job.state = 'succeeded' AND location.complete = 1
  AND location.storage_class = 'local' AND length(location.recipe_hash) = 64 AND length(location.manifest_digest) = 64
  AND recipe.recipe_version = json_extract(CASE WHEN json_valid(job.result_ref) THEN job.result_ref ELSE '{}' END,'$.recipe_version')
  AND file.size = json_extract(job.payload_json,'$.source_size') AND file.mtime = json_extract(job.payload_json,'$.source_mtime')
  AND NOT EXISTS (SELECT 1 FROM background_transcode_artifacts existing
    WHERE existing.recipe_hash = location.recipe_hash AND existing.manifest_digest = location.manifest_digest)
    ORDER BY job.updated_at_ms,job.id LIMIT MIN(128, MAX(0,32768 - (SELECT COUNT(*) FROM background_transcode_artifacts)))
    ON CONFLICT(recipe_hash,manifest_digest) DO NOTHING;
END;
