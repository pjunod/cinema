-- A factory installs these rows atomically with active purpose material.
-- Retired material remains sealed and census-visible after explicit restore.
CREATE TABLE sharing_purpose_key_installation (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton=1),
    server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    master_key_id TEXT NOT NULL CHECK(length(master_key_id)=8),
    state TEXT NOT NULL CHECK(state IN ('ready','restore_pending')),
    generation INTEGER NOT NULL CHECK(generation>0),
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms>0)
) STRICT;
CREATE TABLE sharing_purpose_key_archive (
    purpose TEXT NOT NULL CHECK(purpose IN ('catalogue_revision','file_locator')),
    server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    envelope TEXT NOT NULL CHECK(length(envelope) BETWEEN 1 AND 4096),
    PRIMARY KEY(purpose,server_id,catalogue_epoch)
) STRICT;
CREATE TABLE sharing_purpose_census_intents (
    node_id TEXT NOT NULL PRIMARY KEY CHECK(length(node_id) BETWEEN 1 AND 256),
    raft_id INTEGER NOT NULL CHECK(raft_id>0),
    attempt_id TEXT NOT NULL CHECK(length(attempt_id)=36),
    generation INTEGER NOT NULL CHECK(generation>0),
    claimed_at_ms INTEGER NOT NULL CHECK(claimed_at_ms>0)
) STRICT;
CREATE TABLE sharing_purpose_transaction_guard (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton=1),
    passed INTEGER NOT NULL CHECK(passed=1)
) STRICT;
