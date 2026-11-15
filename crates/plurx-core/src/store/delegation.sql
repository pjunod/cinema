-- Optional household login clients. Grants never enter the native token table.
CREATE TABLE IF NOT EXISTS delegation_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=1)) STRICT;
-- next statement
CREATE TABLE IF NOT EXISTS delegation_clients(id TEXT PRIMARY KEY,name TEXT NOT NULL,secret_hash TEXT NOT NULL,redirect TEXT NOT NULL,generation INTEGER NOT NULL DEFAULT 1,active INTEGER NOT NULL DEFAULT 1 CHECK(active IN(0,1))) STRICT;
-- next statement
CREATE TABLE IF NOT EXISTS delegation_codes(code_hash TEXT PRIMARY KEY,client_id TEXT NOT NULL REFERENCES delegation_clients(id),client_generation INTEGER NOT NULL,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,subject TEXT NOT NULL,source_hash TEXT NOT NULL,challenge TEXT NOT NULL,redirect TEXT NOT NULL,expires_at INTEGER NOT NULL,claim TEXT NOT NULL DEFAULT '') STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS delegation_codes_expiry ON delegation_codes(expires_at);
-- next statement
CREATE TABLE IF NOT EXISTS delegation_grants(grant_hash TEXT PRIMARY KEY,code_hash TEXT NOT NULL UNIQUE,client_id TEXT NOT NULL REFERENCES delegation_clients(id),client_generation INTEGER NOT NULL,user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,subject TEXT NOT NULL,source_hash TEXT NOT NULL,expires_at INTEGER NOT NULL) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS delegation_grants_expiry ON delegation_grants(expires_at);
-- next statement
INSERT OR IGNORE INTO delegation_schema(singleton,version) VALUES(1,1);
