-- This is a second ownership fence, not a second execution queue. Keep the
-- binding after a job expires or is compacted: missing queue authority must
-- reject publication, never turn a bound lease back into an unbound one.
CREATE TABLE IF NOT EXISTS background_job_domain_leases (
    resource TEXT PRIMARY KEY,
    domain_fence INTEGER NOT NULL,
    job_id TEXT NOT NULL,
    job_fence INTEGER NOT NULL,
    node_id TEXT NOT NULL,
    boot_id TEXT NOT NULL,
    claim_id TEXT NOT NULL
) STRICT;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_job_domain_lease_removed
AFTER DELETE ON job_leases
BEGIN
    DELETE FROM background_job_domain_leases WHERE resource = OLD.resource;
END;
