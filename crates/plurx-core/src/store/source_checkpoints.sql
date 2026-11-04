-- Durable node-local SHA-256 artifacts are resume input, never attestation.
-- Bind their retention reference to the existing fenced analysis owner.
CREATE TABLE IF NOT EXISTS analysis_source_checkpoints (
    request_id TEXT PRIMARY KEY REFERENCES analysis_requests(request_id) ON DELETE CASCADE,
    fence INTEGER NOT NULL,
    owner_node_id TEXT NOT NULL,
    artifact_json TEXT NOT NULL CHECK(json_valid(artifact_json))
) STRICT;
