CREATE TABLE IF NOT EXISTS background_embeddings (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    model_digest TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    artifact_json TEXT NOT NULL CHECK(json_valid(artifact_json) AND length(CAST(artifact_json AS BLOB)) <= 65536),
    built_by_node_id TEXT NOT NULL,
    built_at_ms INTEGER NOT NULL,
    PRIMARY KEY(item_id,model_digest)
);
-- next statement
CREATE INDEX IF NOT EXISTS background_embeddings_age ON background_embeddings(built_at_ms,item_id,model_digest);
-- next statement
CREATE INDEX IF NOT EXISTS background_jobs_embedding_identity ON background_jobs(
    json_extract(payload_json,'$.item_id'),json_extract(payload_json,'$.content_digest'),
    json_extract(payload_json,'$.model_digest'),state,updated_at_ms,id) WHERE kind = 'semantic_embedding';
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_publish_embedding_command
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_embedding'
BEGIN
    -- Vectors are a bounded rebuildable cache, never catalogue identity.
    -- Each serving node retains its prior local generation during replacement.
    DELETE FROM background_embeddings WHERE (item_id,model_digest) IN (
        SELECT item_id,model_digest FROM background_embeddings
        WHERE (SELECT COUNT(*) FROM background_embeddings) >= 200000
          AND NOT EXISTS (SELECT 1 FROM background_embeddings current
            WHERE current.item_id = json_extract(NEW.request_json,'$.artifact.item_id')
              AND current.model_digest = json_extract(NEW.request_json,'$.model_digest'))
          AND json_extract(NEW.result_json,'$.outcome') = 'published'
        ORDER BY built_at_ms,item_id,model_digest LIMIT 1
    );
    INSERT INTO background_embeddings(item_id,model_digest,content_digest,artifact_json,built_by_node_id,built_at_ms)
    SELECT json_extract(NEW.request_json,'$.artifact.item_id'), json_extract(NEW.request_json,'$.model_digest'),
        json_extract(NEW.request_json,'$.artifact.content_digest'), json_extract(NEW.request_json,'$.artifact'),
        json_extract(NEW.request_json,'$.token.node_id'),json_extract(NEW.request_json,'$.now_ms')
    WHERE json_extract(NEW.result_json,'$.outcome') = 'published'
    ON CONFLICT(item_id,model_digest) DO UPDATE SET content_digest = excluded.content_digest,
        artifact_json = excluded.artifact_json,built_by_node_id = excluded.built_by_node_id,built_at_ms = excluded.built_at_ms;
    UPDATE background_jobs SET state = 'succeeded', result_ref = json_extract(NEW.result_json,'$.result_ref'),
        owner_node_id = NULL,owner_boot_id = NULL,claim_id = NULL,lease_expires_ms = NULL,
        revision = revision + 1,updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE id = json_extract(NEW.result_json,'$.job_id') AND json_extract(NEW.result_json,'$.outcome') = 'published';
    UPDATE background_job_waiters SET state = CASE WHEN deadline_ms <= json_extract(NEW.request_json,'$.now_ms')
        THEN 'cancelled' ELSE 'succeeded' END, result_ref = json_extract(NEW.result_json,'$.result_ref'),
        updated_at_ms = json_extract(NEW.request_json,'$.now_ms')
    WHERE job_id = json_extract(NEW.result_json,'$.job_id') AND state = 'pending'
        AND json_extract(NEW.result_json,'$.outcome') = 'published';
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
