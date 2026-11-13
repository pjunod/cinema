-- Metadata coordination is an explicit protocol, not a physical reader class.
-- Legacy analysis rows have no role row and retain their original admission.
CREATE TABLE IF NOT EXISTS analysis_coordination_schema (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    version INTEGER NOT NULL CHECK(version=1)
) STRICT;
-- next statement
INSERT INTO analysis_coordination_schema VALUES(1,1) ON CONFLICT DO NOTHING;
-- next statement
CREATE TABLE IF NOT EXISTS analysis_coordination_roles (
    request_id TEXT PRIMARY KEY REFERENCES analysis_requests(request_id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL CHECK(file_id>0),
    source_size INTEGER NOT NULL CHECK(source_size>=0),
    source_mtime INTEGER NOT NULL,
    origin_node_id TEXT NOT NULL,
    object_version TEXT NOT NULL,
    video_identity TEXT NOT NULL,
    contract_version INTEGER NOT NULL CHECK(contract_version>0),
    requested_generation TEXT NOT NULL,
    phase_sequence INTEGER NOT NULL DEFAULT 0 CHECK(phase_sequence>=0),
    active_custody_id TEXT,
    phase_registered INTEGER NOT NULL DEFAULT 0 CHECK(phase_registered IN (0,1)),
    physical_node_id TEXT,
    physical_fence INTEGER,
    claim_permit_node TEXT,
    claim_permit_fence INTEGER,
    UNIQUE(file_id,source_size,source_mtime,origin_node_id,object_version,video_identity,contract_version,requested_generation),
    CHECK((active_custody_id IS NULL AND physical_node_id IS NULL AND physical_fence IS NULL)
       OR (active_custody_id IS NOT NULL AND physical_node_id IS NOT NULL AND physical_fence>0))
) STRICT;
-- next statement
-- Reconciliation in old binaries copies video_identity but substitutes its
-- engine pipeline. Reject only this invalid protocol pairing, never an
-- unrelated legacy recipe or source generation.
CREATE TRIGGER IF NOT EXISTS analysis_coordination_no_legacy_replacement
BEFORE INSERT ON analysis_requests
WHEN NEW.video_identity LIKE 'coordination-video:%'
 AND NEW.pipeline_version NOT LIKE 'coordination-contract:%'
BEGIN SELECT RAISE(IGNORE); END;
-- next statement
CREATE TRIGGER IF NOT EXISTS analysis_coordination_no_legacy_reconcile
BEFORE UPDATE ON analysis_requests
WHEN OLD.pipeline_version LIKE 'coordination-contract:%'
 AND NEW.last_error_code='request_reconciled'
BEGIN SELECT RAISE(IGNORE); END;
-- next statement
-- A legacy claim with pipeline=None also cannot acquire a coordinator.
-- The new protocol installs this one-use permit in the same transaction.
CREATE TRIGGER IF NOT EXISTS analysis_coordination_claim_protocol
BEFORE UPDATE OF state,fence ON analysis_requests
WHEN NEW.pipeline_version LIKE 'coordination-contract:%'
 AND NEW.state='running' AND NEW.fence>OLD.fence
 AND NOT EXISTS(SELECT 1 FROM analysis_coordination_roles role
    WHERE role.request_id=NEW.request_id AND role.claim_permit_node=NEW.owner_node_id
      AND role.claim_permit_fence=OLD.fence AND NEW.fence=OLD.fence+1
      AND NEW.pipeline_version='coordination-contract:'||role.contract_version
      AND NEW.video_identity='coordination-video:'||role.contract_version||':'||role.video_identity)
BEGIN SELECT RAISE(IGNORE); END;
-- next statement
CREATE TRIGGER IF NOT EXISTS analysis_coordination_consume_claim_permit
AFTER UPDATE OF state,fence ON analysis_requests
WHEN NEW.pipeline_version LIKE 'coordination-contract:%'
 AND NEW.state='running' AND NEW.fence>OLD.fence
BEGIN
 UPDATE analysis_coordination_roles SET claim_permit_node=NULL,claim_permit_fence=NULL
 WHERE request_id=NEW.request_id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS analysis_coordination_preserve_live_role
BEFORE DELETE ON analysis_requests
WHEN OLD.pipeline_version LIKE 'coordination-contract:%'
 AND (OLD.state IN ('queued','running','submitted') OR EXISTS(
   SELECT 1 FROM analysis_coordination_roles role WHERE role.request_id=OLD.request_id
     AND role.active_custody_id IS NOT NULL))
BEGIN SELECT RAISE(IGNORE); END;
-- next statement
DROP TRIGGER IF EXISTS analysis_source_reserve_on_claim;
-- next statement
CREATE TRIGGER analysis_source_reserve_on_claim
AFTER UPDATE OF state,fence ON analysis_requests
WHEN NEW.state='running' AND NEW.fence>OLD.fence
 AND NEW.component IN ('fragment_index','skip_markers')
 AND NEW.pipeline_version NOT LIKE 'coordination-contract:%'
BEGIN
 DELETE FROM analysis_source_reservations WHERE request_id=NEW.request_id
   OR expires_at_ms<=NEW.updated_at_ms;
 INSERT INTO analysis_source_reservations(request_id,resource_key,fence,expires_at_ms)
 SELECT NEW.request_id,resource_key,NEW.fence,NEW.lease_expires_ms
 FROM analysis_required_resources WHERE request_id=NEW.request_id;
END;
-- next statement
-- Old startup backfill must not charge an idle metadata claim as a reader.
CREATE TRIGGER IF NOT EXISTS analysis_coordination_reservation_protocol
BEFORE INSERT ON analysis_source_reservations
WHEN EXISTS(SELECT 1 FROM analysis_requests request WHERE request.request_id=NEW.request_id
  AND request.pipeline_version LIKE 'coordination-contract:%')
 AND NOT EXISTS(SELECT 1 FROM analysis_coordination_roles role WHERE role.request_id=NEW.request_id
  AND role.active_custody_id IS NOT NULL AND role.physical_fence=NEW.fence)
BEGIN SELECT RAISE(IGNORE); END;
-- next statement
-- An association is metadata demand only; it never grants source I/O or media
-- serving authority. Its typed admission transaction checks the Source gate.
CREATE TABLE IF NOT EXISTS analysis_coordination_source_interests (
    analysis_request_id TEXT NOT NULL REFERENCES analysis_coordination_roles(request_id) ON DELETE CASCADE,
    source_incarnation_id TEXT NOT NULL,
    association_json TEXT NOT NULL CHECK(json_valid(association_json)),
    authority_phase TEXT NOT NULL CHECK(authority_phase IN ('preparing','active')),
    session_id TEXT,
    lease_revision INTEGER,
    expires_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(analysis_request_id,source_incarnation_id),
    CHECK((authority_phase='preparing' AND session_id IS NULL AND lease_revision IS NULL)
       OR (authority_phase='active' AND session_id IS NOT NULL AND lease_revision>0))
) STRICT;
-- next statement
-- File-level Source placement metadata, anchored by the existing Source claim.
-- It owns no independent lease and never authorizes media or source reads.
CREATE TABLE IF NOT EXISTS copy_source_owner_designations (
    server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    file_id TEXT NOT NULL,
    file_revision TEXT NOT NULL,
    owner_node_id TEXT NOT NULL,
    anchor_owner_key TEXT NOT NULL,
    anchor_request_id TEXT NOT NULL,
    anchor_incarnation_id TEXT NOT NULL,
    designation_epoch INTEGER NOT NULL DEFAULT 1 CHECK(designation_epoch>0),
    PRIMARY KEY(server_id,catalogue_epoch,file_id,file_revision)
) STRICT;

-- next statement
-- Placement fencing is immutable per Source invocation, not a physical lease.
CREATE TABLE IF NOT EXISTS copy_source_designation_bindings (
    incarnation_id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    file_id TEXT NOT NULL,
    file_revision TEXT NOT NULL,
    owner_node_id TEXT NOT NULL,
    designation_epoch INTEGER NOT NULL CHECK(designation_epoch>0)
) STRICT;
-- next statement
CREATE TRIGGER IF NOT EXISTS copy_source_designation_binding_immutable
BEFORE UPDATE ON copy_source_designation_bindings
BEGIN SELECT RAISE(ABORT,'immutable Source designation binding'); END;
-- next statement
CREATE TRIGGER IF NOT EXISTS copy_source_designation_binding_no_delete
BEFORE DELETE ON copy_source_designation_bindings
BEGIN SELECT RAISE(ABORT,'immutable Source designation binding'); END;
