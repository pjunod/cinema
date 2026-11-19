CREATE TABLE IF NOT EXISTS background_source_service_epochs (
    resource_key TEXT NOT NULL,epoch_id INTEGER NOT NULL,allocated_bytes INTEGER NOT NULL CHECK(allocated_bytes>=0),
    PRIMARY KEY(resource_key,epoch_id)
) STRICT;
-- next statement
CREATE TABLE IF NOT EXISTS background_source_service_grants (
    receipt_id TEXT PRIMARY KEY,owner_key TEXT NOT NULL,epoch_id INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,allocated_bytes INTEGER NOT NULL CHECK(allocated_bytes>=0),grant_json TEXT NOT NULL CHECK(json_valid(grant_json)),
    UNIQUE(owner_key,epoch_id)
) STRICT;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_source_service_effects
AFTER INSERT ON background_job_commands WHEN NEW.operation='source_service' AND json_type(NEW.result_json)='object'
BEGIN
    DELETE FROM background_source_service_grants WHERE epoch_id<json_extract(NEW.request_json,'$.epoch_id');
    DELETE FROM background_source_service_epochs WHERE epoch_id<json_extract(NEW.request_json,'$.epoch_id');
    INSERT INTO background_source_service_grants(receipt_id,owner_key,epoch_id,expires_at_ms,allocated_bytes,grant_json)
    VALUES(json_extract(NEW.result_json,'$.receipt_id'),json_extract(NEW.request_json,'$.owner_key'),
      json_extract(NEW.request_json,'$.epoch_id'),json_extract(NEW.result_json,'$.expires_at_ms'),json_extract(NEW.request_json,'$.charge_bytes'),NEW.result_json)
    ON CONFLICT(owner_key,epoch_id) DO UPDATE SET expires_at_ms=excluded.expires_at_ms,
      allocated_bytes=allocated_bytes+excluded.allocated_bytes,grant_json=excluded.grant_json;
    INSERT INTO background_source_service_epochs(resource_key,epoch_id,allocated_bytes)
    SELECT resource.value,json_extract(NEW.request_json,'$.epoch_id'),json_extract(NEW.request_json,'$.charge_bytes')
    FROM json_each(NEW.result_json,'$.resource_keys') resource WHERE json_extract(NEW.request_json,'$.charge_bytes')>0
    ON CONFLICT(resource_key,epoch_id) DO UPDATE SET allocated_bytes=allocated_bytes+excluded.allocated_bytes;
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_source_service_refusal_cleanup
AFTER INSERT ON background_job_commands WHEN NEW.operation='source_service' AND NEW.result_json='false'
BEGIN
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
