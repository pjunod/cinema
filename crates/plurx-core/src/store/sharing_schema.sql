CREATE TABLE sharing_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    server_id TEXT NOT NULL UNIQUE,
    catalogue_epoch TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE sharing_invitations (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    library_ids_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open','consumed','cancelled')),
    claim_id TEXT UNIQUE,
    claim_digest TEXT,
    CHECK (expires_at_ms > created_at_ms)
) STRICT;
CREATE TABLE sharing_exports (
    id TEXT PRIMARY KEY,
    invitation_id TEXT NOT NULL UNIQUE REFERENCES sharing_invitations(id),
    recipient_server_id TEXT NOT NULL,
    recipient_name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    scope_generation INTEGER NOT NULL CHECK (scope_generation > 0),
    credential_generation INTEGER NOT NULL CHECK (credential_generation > 0),
    catalogue_generation INTEGER NOT NULL CHECK (catalogue_generation > 0),
    mutation_generation INTEGER NOT NULL CHECK (mutation_generation > 0),
    state TEXT NOT NULL CHECK (state IN ('pending','active','disabled','revoked')),
    pending_expires_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE TABLE sharing_export_libraries (
    grant_id TEXT NOT NULL REFERENCES sharing_exports(id) ON DELETE CASCADE,
    library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE RESTRICT,
    PRIMARY KEY (grant_id, library_id)
) STRICT;
CREATE TABLE sharing_imports (
    id TEXT PRIMARY KEY,
    source_server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    source_name TEXT NOT NULL,
    claim_id TEXT NOT NULL UNIQUE,
    remote_grant_id TEXT,
    credential_envelope TEXT NOT NULL,
    claim_envelope TEXT,
    endpoints_json TEXT NOT NULL,
    assignment_generation INTEGER NOT NULL CHECK (assignment_generation > 0),
    lifecycle_generation INTEGER NOT NULL CHECK (lifecycle_generation > 0),
    endpoint_generation INTEGER NOT NULL CHECK (endpoint_generation > 0),
    observed_scope_generation INTEGER,
    observed_credential_generation INTEGER,
    observed_catalogue_generation INTEGER,
    observed_endpoint_revision INTEGER,
    state TEXT NOT NULL CHECK
      (state IN ('claiming','pending','active','disabled','revoked')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (source_server_id, catalogue_epoch)
) STRICT;
CREATE TABLE sharing_viewers (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    viewer_id TEXT NOT NULL UNIQUE
) STRICT;
CREATE TABLE sharing_assignments (
    import_id TEXT NOT NULL REFERENCES sharing_imports(id) ON DELETE CASCADE,
    remote_library_id TEXT NOT NULL,
    user_id INTEGER NOT NULL REFERENCES sharing_viewers(user_id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL CHECK (enabled IN (0,1)),
    PRIMARY KEY (import_id, remote_library_id, user_id)
) STRICT;
CREATE TABLE sharing_watch (
    source_server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    remote_library_id TEXT NOT NULL,
    remote_item_id TEXT NOT NULL,
    user_id INTEGER NOT NULL REFERENCES sharing_viewers(user_id) ON DELETE CASCADE,
    position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
    duration_ms INTEGER CHECK (duration_ms >= 0),
    watched INTEGER NOT NULL CHECK (watched IN (0,1)),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (source_server_id, catalogue_epoch, remote_item_id, user_id)
) STRICT;
CREATE TABLE sharing_rotations (
    grant_id TEXT PRIMARY KEY REFERENCES sharing_exports(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL UNIQUE,
    old_hash TEXT NOT NULL,
    new_hash TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > created_at_ms)
) STRICT;
CREATE TABLE sharing_import_rotations (
    import_id TEXT PRIMARY KEY REFERENCES sharing_imports(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL UNIQUE,
    credential_envelope TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL CHECK (expires_at_ms > created_at_ms)
) STRICT;
CREATE TABLE sharing_catalogue_revisions (
    library_id INTEGER PRIMARY KEY REFERENCES libraries(id) ON DELETE CASCADE,
    order_revision INTEGER NOT NULL CHECK (order_revision > 0)
) STRICT;
CREATE TABLE sharing_endpoint_manifest (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    endpoints_json TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0)
) STRICT;
CREATE TABLE sharing_relay_upstream (
    incarnation_id TEXT PRIMARY KEY REFERENCES media_sessions(incarnation_id) ON DELETE CASCADE,
    import_id TEXT NOT NULL,
    lifecycle_generation INTEGER NOT NULL CHECK (lifecycle_generation > 0),
    assignment_generation INTEGER NOT NULL CHECK (assignment_generation > 0),
    remote_library_id TEXT NOT NULL,
    remote_item_id TEXT NOT NULL,
    remote_file_id TEXT NOT NULL,
    remote_revision TEXT NOT NULL,
    source_request_id TEXT NOT NULL,
    source_session_id TEXT,
    source_incarnation_id TEXT,
    endpoint_revision INTEGER NOT NULL CHECK (endpoint_revision > 0),
    capability_envelope TEXT,
    source_position_ms INTEGER NOT NULL CHECK (source_position_ms >= 0),
    -- Durable Source Start dispatch record: activation writes none; a sealed
    -- Upstream capsule replaces it before the first Start byte; NULL is unknown.
    dispatch_envelope TEXT
) STRICT;
CREATE TABLE sharing_delivery_grants (
    token_hash TEXT PRIMARY KEY,
    incarnation_id TEXT NOT NULL REFERENCES media_sessions(incarnation_id) ON DELETE CASCADE,
    source_token_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active','revoked')),
    deadline_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX sharing_exports_state ON sharing_exports(state);
CREATE INDEX sharing_invitations_expiry ON sharing_invitations(state, expires_at_ms);
CREATE INDEX sharing_assignments_user ON sharing_assignments(user_id, import_id, enabled);
