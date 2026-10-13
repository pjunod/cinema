-- Exact fresh schema published on maina48884906da351ca0c72dc99c227aa169911d089.
-- Drain and recovery are applied by their actual additive migrations.
CREATE TABLE IF NOT EXISTS media_sessions (
    incarnation_id                TEXT PRIMARY KEY,
    session_id                    TEXT NOT NULL UNIQUE,
    user_id                       INTEGER NOT NULL,
    playback_id                   TEXT NOT NULL,
    request_fingerprint           TEXT NOT NULL,
    owner_node_id                 TEXT NOT NULL,
    owner_epoch                   INTEGER NOT NULL CHECK (owner_epoch > 0),
    lease_expires_at_ms           INTEGER NOT NULL,
    state                         TEXT NOT NULL CHECK (state IN ('starting', 'active', 'ended')),
    terminal_reason               TEXT CHECK (terminal_reason IN
                                      ('deleted', 'superseded', 'admin_stop', 'revoked', 'replaced')),
    publication_ready_at_ms       INTEGER NOT NULL DEFAULT 0 CHECK (publication_ready_at_ms >= 0),
    recipe_json                   TEXT NOT NULL,
    response_json                 TEXT NOT NULL,
    produced_playable_through_ms  INTEGER NOT NULL DEFAULT 0,
    fetched_through_ms            INTEGER NOT NULL DEFAULT 0,
    media_origin_ms               INTEGER NOT NULL DEFAULT 0,
    media_sequence                INTEGER NOT NULL DEFAULT 0,
    discontinuity_sequence        INTEGER NOT NULL DEFAULT 0,
    updated_at_ms                 INTEGER NOT NULL
) STRICT;
