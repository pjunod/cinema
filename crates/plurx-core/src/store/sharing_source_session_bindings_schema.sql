-- Candidate only. Coordinated activation owns installation; reads never repair it.
-- This is immutable Source identity and capacity accounting, not media lifecycle.
CREATE TABLE sharing_source_session_bindings (
    incarnation_id TEXT NOT NULL PRIMARY KEY,
    owner_key TEXT NOT NULL,
    share_grant_id TEXT NOT NULL,
    share_viewer_key TEXT NOT NULL,
    request_id TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
    request_fingerprint TEXT NOT NULL CHECK
      (length(request_fingerprint)=64 AND request_fingerprint NOT GLOB '*[^0-9a-f]*'),
    playback_id TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
    source_server_id TEXT NOT NULL,
    catalogue_epoch TEXT NOT NULL,
    library_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    file_revision TEXT NOT NULL CHECK
      (length(file_revision)=64 AND file_revision NOT GLOB '*[^0-9a-f]*'),
    reservation_state TEXT NOT NULL CHECK (reservation_state IN ('held','released')),
    start_resolved_at_ms INTEGER,
    dispatch_generation INTEGER NOT NULL DEFAULT 0 CHECK (dispatch_generation>=0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms>0),
    released_at_ms INTEGER,
    release_fingerprint TEXT,
    UNIQUE(owner_key,request_id),
    CHECK (owner_key='share:'||share_grant_id||':'||share_viewer_key),
    CHECK (length(share_viewer_key)=64 AND share_viewer_key NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(incarnation_id)=36 AND substr(incarnation_id,9,1)='-'
      AND substr(incarnation_id,14,1)='-' AND substr(incarnation_id,19,1)='-'
      AND substr(incarnation_id,24,1)='-' AND length(replace(incarnation_id,'-',''))=32
      AND replace(incarnation_id,'-','') NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(share_grant_id)=36 AND substr(share_grant_id,9,1)='-'
      AND substr(share_grant_id,14,1)='-' AND substr(share_grant_id,19,1)='-'
      AND substr(share_grant_id,24,1)='-' AND length(replace(share_grant_id,'-',''))=32
      AND replace(share_grant_id,'-','') NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(source_server_id)=36 AND substr(source_server_id,9,1)='-'
      AND substr(source_server_id,14,1)='-' AND substr(source_server_id,19,1)='-'
      AND substr(source_server_id,24,1)='-' AND length(replace(source_server_id,'-',''))=32
      AND replace(source_server_id,'-','') NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(catalogue_epoch)=36 AND substr(catalogue_epoch,9,1)='-'
      AND substr(catalogue_epoch,14,1)='-' AND substr(catalogue_epoch,19,1)='-'
      AND substr(catalogue_epoch,24,1)='-' AND length(replace(catalogue_epoch,'-',''))=32
      AND replace(catalogue_epoch,'-','') NOT GLOB '*[^0-9a-f]*'),
    CHECK (CAST(library_id AS INTEGER)>0 AND CAST(CAST(library_id AS INTEGER) AS TEXT)=library_id),
    CHECK (CAST(item_id AS INTEGER)>0 AND CAST(CAST(item_id AS INTEGER) AS TEXT)=item_id),
    CHECK (CAST(file_id AS INTEGER)>0 AND CAST(CAST(file_id AS INTEGER) AS TEXT)=file_id),
    CHECK (start_resolved_at_ms IS NULL OR start_resolved_at_ms>=created_at_ms),
    CHECK ((reservation_state='held' AND released_at_ms IS NULL AND release_fingerprint IS NULL)
      OR (reservation_state='released' AND released_at_ms IS NOT NULL
        AND released_at_ms>=created_at_ms AND start_resolved_at_ms IS NOT NULL
        AND release_fingerprint IS NOT NULL AND length(release_fingerprint)=64
        AND release_fingerprint NOT GLOB '*[^0-9a-f]*'))
) STRICT;
-- next statement
CREATE INDEX sharing_source_binding_reservations
    ON sharing_source_session_bindings(reservation_state,share_grant_id,start_resolved_at_ms);
-- next statement
CREATE INDEX sharing_source_requests
    ON media_session_requests(principal_kind,state,share_grant_id,incarnation_id);
-- next statement
CREATE INDEX sharing_source_routes
    ON media_sessions(principal_kind,state,share_grant_id,incarnation_id);
-- next statement
CREATE INDEX sharing_source_preparations
    ON media_session_preparations(principal_kind,share_grant_id,staged_incarnation_id);
-- next statement
CREATE TRIGGER sharing_source_binding_immutable
    BEFORE UPDATE OF incarnation_id,owner_key,share_grant_id,share_viewer_key,
      request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,
      library_id,item_id,file_id,file_revision,created_at_ms
    ON sharing_source_session_bindings
    WHEN OLD.incarnation_id<>NEW.incarnation_id OR OLD.owner_key<>NEW.owner_key
      OR OLD.share_grant_id<>NEW.share_grant_id OR OLD.share_viewer_key<>NEW.share_viewer_key
      OR OLD.request_id<>NEW.request_id OR OLD.request_fingerprint<>NEW.request_fingerprint
      OR OLD.playback_id<>NEW.playback_id OR OLD.source_server_id<>NEW.source_server_id
      OR OLD.catalogue_epoch<>NEW.catalogue_epoch OR OLD.library_id<>NEW.library_id
      OR OLD.item_id<>NEW.item_id OR OLD.file_id<>NEW.file_id
      OR OLD.file_revision<>NEW.file_revision OR OLD.created_at_ms<>NEW.created_at_ms
    BEGIN SELECT RAISE(ABORT,'immutable Source session binding'); END;
-- next statement
CREATE TRIGGER sharing_source_binding_held_retention
    BEFORE DELETE ON sharing_source_session_bindings WHEN OLD.reservation_state='held'
    BEGIN SELECT RAISE(ABORT,'held Source session reservation cannot be pruned'); END;

-- next statement
CREATE TRIGGER sharing_source_binding_monotonic
    BEFORE UPDATE ON sharing_source_session_bindings
    WHEN (OLD.reservation_state='released' AND NEW.reservation_state<>'released')
      OR NEW.dispatch_generation<OLD.dispatch_generation
      OR (OLD.start_resolved_at_ms IS NOT NULL AND NEW.start_resolved_at_ms IS NOT OLD.start_resolved_at_ms)
      OR (OLD.reservation_state='released' AND
        (NEW.release_fingerprint IS NOT OLD.release_fingerprint
         OR NEW.released_at_ms IS NOT OLD.released_at_ms))
    BEGIN SELECT RAISE(ABORT,'Source reservation proof cannot move backwards'); END;
