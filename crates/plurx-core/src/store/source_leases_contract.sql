-- Source leases v3 (contract): the v1 source-owner objects go. On the
-- replicated store this runs only once every active node's binary has
-- published the source_leases_v2 capability.
DROP TRIGGER source_lease_bridge_insert;
-- next statement
DROP TRIGGER source_lease_bridge_update;
-- next statement
DROP TRIGGER source_lease_bridge_delete;
-- next statement
DROP VIEW source_io_occupancy;
-- next statement
DROP VIEW background_source_current_owners;
-- next statement
DROP VIEW foreground_source_permissions;
-- next statement
DROP VIEW foreground_source_required_resources;
-- next statement
DROP TABLE background_source_custody;
-- next statement
DROP TABLE foreground_source_owners;
-- next statement
DROP TABLE background_source_schema;
-- next statement
UPDATE source_lease_schema SET version=3 WHERE singleton=1;
