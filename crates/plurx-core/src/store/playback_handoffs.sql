-- Playback handoff: a signed-in client mints a one-time code so another local
-- app (the Linux player) can play one title without holding the account
-- bearer. `playback_handoffs` holds the code, hashed, single use and short
-- lived. `playback_handoffs_tokens` holds the token a code redeemed into. It
-- is deliberately not a row in `tokens`: every native lookup, including one
-- by a binary older than this table during a rolling upgrade, reads `tokens`
-- and so can never find a handoff token, let alone accept it as an account
-- bearer. An independently recognized extension: it creates only its own
-- objects.
CREATE TABLE IF NOT EXISTS playback_handoffs_schema (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    version INTEGER NOT NULL CHECK (version = 1)
) STRICT;
-- next statement
CREATE TABLE IF NOT EXISTS playback_handoffs (
    id TEXT PRIMARY KEY,
    code_hash TEXT NOT NULL UNIQUE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL,
    source_token_hash TEXT NOT NULL,
    start_ms INTEGER,
    audio INTEGER,
    subtitle INTEGER,
    origin TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    redeemed_at INTEGER
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS playback_handoffs_expiry ON playback_handoffs(expires_at);
-- next statement
CREATE TABLE IF NOT EXISTS playback_handoffs_tokens (
    token_hash TEXT PRIMARY KEY,
    handoff_id TEXT NOT NULL UNIQUE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL,
    source_token_hash TEXT NOT NULL,
    device TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS playback_handoffs_tokens_user
    ON playback_handoffs_tokens(user_id);
-- next statement
CREATE INDEX IF NOT EXISTS playback_handoffs_tokens_source
    ON playback_handoffs_tokens(source_token_hash);
-- next statement
INSERT OR IGNORE INTO playback_handoffs_schema (singleton, version) VALUES (1, 1);
