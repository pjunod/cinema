-- A read-retention principal is a Source grant plus its exact signed file target,
-- never a source user account or a work-producing demand.
CREATE TABLE shared_caption_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=1)) STRICT;
-- next statement
INSERT INTO shared_caption_schema VALUES(1,1);
-- next statement
CREATE TABLE shared_caption_read_leases(
 id TEXT PRIMARY KEY,
 grant_id TEXT NOT NULL CHECK(length(grant_id)=36),
 principal TEXT NOT NULL CHECK(length(principal)=64),
 file_id INTEGER NOT NULL,
 asset_id TEXT NOT NULL REFERENCES subtitle_translation_assets(id),
 manifest_revision INTEGER NOT NULL CHECK(manifest_revision>=0),
 expires_at_ms INTEGER NOT NULL,
 created_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX shared_caption_read_expiry ON shared_caption_read_leases(asset_id,expires_at_ms);
-- next statement
CREATE INDEX shared_caption_read_principal ON shared_caption_read_leases(principal,expires_at_ms);
