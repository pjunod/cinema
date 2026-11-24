-- Each signed-in device's last playback capability document (Curator ADR
-- 0026 §5). One row per login token, owned by that token: the foreign keys
-- cascade, so logout, revoking a device, a password reset, deleting the user
-- and any other removal of the token remove its row with no handler of its
-- own. Additive: no schema version bump on either store; a binary that
-- predates it ignores the unknown names. A later shape is a new revision
-- (`_r2` objects, marker version 2), never an edit of these.
CREATE TABLE IF NOT EXISTS playback_device_caps_schema (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    version INTEGER NOT NULL CHECK(version>=1)
) STRICT;
-- next statement
INSERT INTO playback_device_caps_schema VALUES(1,1) ON CONFLICT DO NOTHING;
-- next statement
CREATE TABLE IF NOT EXISTS playback_device_caps_r1 (
    token_hash TEXT PRIMARY KEY REFERENCES tokens(token_hash) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    caps TEXT NOT NULL CHECK(length(caps) <= 16384),
    updated_at INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS playback_device_caps_r1_user ON playback_device_caps_r1(user_id);
