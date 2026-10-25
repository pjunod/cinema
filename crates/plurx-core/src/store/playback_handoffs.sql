-- Playback handoff: a signed-in client mints a one-time code so another local
-- app (the Linux player) can play one title without holding the account
-- bearer. `playback_handoffs` holds the code, hashed, single use and short
-- lived. `playback_handoffs_tokens` holds the token a code redeemed into: its
-- row in `tokens` is what the account's sessions list shows and what
-- revocation deletes, and this row is the audience marker that keeps it off
-- every native route. The marker goes only with its token row (the cascade),
-- never on its own, because a token row without it would authenticate as a
-- native account bearer. An independently recognized extension: it creates
-- only its own objects.
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
    token_hash TEXT PRIMARY KEY REFERENCES tokens(token_hash) ON DELETE CASCADE,
    handoff_id TEXT NOT NULL UNIQUE,
    user_id INTEGER NOT NULL,
    file_id INTEGER NOT NULL,
    source_token_hash TEXT NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS playback_handoffs_tokens_source
    ON playback_handoffs_tokens(source_token_hash);
-- next statement
INSERT OR IGNORE INTO playback_handoffs_schema (singleton, version) VALUES (1, 1);
