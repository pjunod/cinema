-- Frozen candidate rebuild from SQLite v92 / replicated v70.
-- Install atomically only after compatible writers have drained.
-- No foreign keys: retained terminal rows do not acquire authority.

DROP TRIGGER IF EXISTS background_analysis_viewer_refresh;

-- next statement
DROP TRIGGER IF EXISTS background_analysis_viewer_session_ended;

-- next statement
DROP TRIGGER IF EXISTS media_playback_pointers_desired_fence_ai;

-- next statement
DROP TRIGGER IF EXISTS media_playback_pointers_desired_fence_au;

-- next statement
DROP TRIGGER IF EXISTS media_sessions_drain_ownership_fence_au;

-- next statement
DROP TRIGGER IF EXISTS media_session_publication_claim_au;

-- next statement
DROP TRIGGER IF EXISTS library_channel_session_recipes_request_delete;

-- next statement
CREATE TABLE media_session_requests_principal_new (
    owner_key TEXT NOT NULL,
    principal_kind TEXT NOT NULL DEFAULT 'local' CHECK (principal_kind IN ('local', 'sharing')),
    user_id INTEGER,
    share_grant_id TEXT,
    share_viewer_key TEXT,
    request_id          TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
    request_fingerprint TEXT NOT NULL,
    playback_id         TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
    state               TEXT NOT NULL CHECK (state IN ('starting', 'resolved', 'failed')),
    claim_expires_at_ms INTEGER NOT NULL,
    incarnation_id      TEXT NOT NULL,
    owner_node_id       TEXT,
    response_json       TEXT,
    updated_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (owner_key, request_id),
    CHECK (
      (principal_kind = 'local' AND user_id IS NOT NULL AND user_id > 0
       AND share_grant_id IS NULL AND share_viewer_key IS NULL
       AND owner_key = 'local:' || CAST(user_id AS TEXT))
      OR (principal_kind = 'sharing' AND user_id IS NULL
       AND share_grant_id IS NOT NULL AND share_viewer_key IS NOT NULL
       AND length(share_grant_id) = 36
       AND substr(share_grant_id,9,1) = '-' AND substr(share_grant_id,14,1) = '-'
       AND substr(share_grant_id,19,1) = '-' AND substr(share_grant_id,24,1) = '-'
       AND length(replace(share_grant_id,'-','')) = 32
       AND replace(share_grant_id,'-','') NOT GLOB '*[^0-9a-f]*'
       AND length(share_viewer_key) = 64 AND share_viewer_key NOT GLOB '*[^0-9a-f]*'
       AND owner_key = 'share:' || share_grant_id || ':' || share_viewer_key)
    )
) STRICT;

-- next statement
INSERT INTO media_session_requests_principal_new (owner_key, principal_kind, share_grant_id, share_viewer_key, user_id, request_id, request_fingerprint, playback_id, state, claim_expires_at_ms, incarnation_id, owner_node_id, response_json, updated_at_ms)
SELECT 'local:' || CAST(user_id AS TEXT), 'local', NULL, NULL, user_id, request_id, request_fingerprint, playback_id, state, claim_expires_at_ms, incarnation_id, owner_node_id, response_json, updated_at_ms FROM media_session_requests;

-- next statement
DROP TABLE media_session_requests;

-- next statement
ALTER TABLE media_session_requests_principal_new RENAME TO media_session_requests;

-- next statement
CREATE TABLE media_playback_pointers_principal_new (
    owner_key TEXT NOT NULL,
    principal_kind TEXT NOT NULL DEFAULT 'local' CHECK (principal_kind IN ('local', 'sharing')),
    user_id INTEGER,
    share_grant_id TEXT,
    share_viewer_key TEXT,
    playback_id            TEXT NOT NULL CHECK (length(playback_id) BETWEEN 1 AND 128),
    current_incarnation_id TEXT NOT NULL UNIQUE,
    updated_at_ms          INTEGER NOT NULL,
    -- The ask this pointer was written against. Nullable, and the null means
    -- the playback had no recorded ask -- see the fence triggers, which read a
    -- null on a playback that *does* have one as a writer from before this
    -- column. A fresh install declares it here; an upgrade adds it by `ALTER`,
    -- and the two have to end in the same shape.
    desired_revision       INTEGER,
    PRIMARY KEY (owner_key, playback_id),
    CHECK (
      (principal_kind = 'local' AND user_id IS NOT NULL AND user_id > 0
       AND share_grant_id IS NULL AND share_viewer_key IS NULL
       AND owner_key = 'local:' || CAST(user_id AS TEXT))
      OR (principal_kind = 'sharing' AND user_id IS NULL
       AND share_grant_id IS NOT NULL AND share_viewer_key IS NOT NULL
       AND length(share_grant_id) = 36
       AND substr(share_grant_id,9,1) = '-' AND substr(share_grant_id,14,1) = '-'
       AND substr(share_grant_id,19,1) = '-' AND substr(share_grant_id,24,1) = '-'
       AND length(replace(share_grant_id,'-','')) = 32
       AND replace(share_grant_id,'-','') NOT GLOB '*[^0-9a-f]*'
       AND length(share_viewer_key) = 64 AND share_viewer_key NOT GLOB '*[^0-9a-f]*'
       AND owner_key = 'share:' || share_grant_id || ':' || share_viewer_key)
    )
) STRICT;

-- next statement
INSERT INTO media_playback_pointers_principal_new (owner_key, principal_kind, share_grant_id, share_viewer_key, user_id, playback_id, current_incarnation_id, updated_at_ms, desired_revision)
SELECT 'local:' || CAST(user_id AS TEXT), 'local', NULL, NULL, user_id, playback_id, current_incarnation_id, updated_at_ms, desired_revision FROM media_playback_pointers;

-- next statement
DROP TABLE media_playback_pointers;

-- next statement
ALTER TABLE media_playback_pointers_principal_new RENAME TO media_playback_pointers;

-- next statement
CREATE TABLE media_sessions_principal_new (
    incarnation_id                TEXT PRIMARY KEY,
    session_id                    TEXT NOT NULL UNIQUE,
    owner_key TEXT NOT NULL,
    principal_kind TEXT NOT NULL DEFAULT 'local' CHECK (principal_kind IN ('local', 'sharing')),
    user_id INTEGER,
    share_grant_id TEXT,
    share_viewer_key TEXT,
    playback_id                   TEXT NOT NULL,
    request_fingerprint           TEXT NOT NULL,
    owner_node_id                 TEXT NOT NULL,
    owner_epoch                   INTEGER NOT NULL CHECK (owner_epoch > 0),
    lease_expires_at_ms           INTEGER NOT NULL,
    state                         TEXT NOT NULL CHECK (state IN ('starting', 'active', 'ended')),
    terminal_reason               TEXT CHECK (terminal_reason IN
                                      ('deleted', 'superseded', 'admin_stop', 'revoked', 'replaced')),
    publication_ready_at_ms       INTEGER NOT NULL DEFAULT 0 CHECK (publication_ready_at_ms >= 0),
    recipe_json                   TEXT NOT NULL,
    response_json                 TEXT NOT NULL,
    produced_playable_through_ms  INTEGER NOT NULL DEFAULT 0,
    fetched_through_ms            INTEGER NOT NULL DEFAULT 0,
    media_origin_ms               INTEGER NOT NULL DEFAULT 0,
    media_sequence                INTEGER NOT NULL DEFAULT 0,
    discontinuity_sequence        INTEGER NOT NULL DEFAULT 0,
    updated_at_ms                 INTEGER NOT NULL,
    recovery_epoch                TEXT NOT NULL DEFAULT '',
    -- When a predecessor being drained on purpose stops being kept. Null means
    -- not draining, which is what every session starts as. A fresh cluster
    -- declares it here; an upgrade adds it by `ALTER`, and the migration has
    -- to ask which of the two it is looking at before it tries.
    drain_deadline_ms             INTEGER
,
    CHECK (
      (principal_kind = 'local' AND user_id IS NOT NULL AND user_id > 0
       AND share_grant_id IS NULL AND share_viewer_key IS NULL
       AND owner_key = 'local:' || CAST(user_id AS TEXT))
      OR (principal_kind = 'sharing' AND user_id IS NULL
       AND share_grant_id IS NOT NULL AND share_viewer_key IS NOT NULL
       AND length(share_grant_id) = 36
       AND substr(share_grant_id,9,1) = '-' AND substr(share_grant_id,14,1) = '-'
       AND substr(share_grant_id,19,1) = '-' AND substr(share_grant_id,24,1) = '-'
       AND length(replace(share_grant_id,'-','')) = 32
       AND replace(share_grant_id,'-','') NOT GLOB '*[^0-9a-f]*'
       AND length(share_viewer_key) = 64 AND share_viewer_key NOT GLOB '*[^0-9a-f]*'
       AND owner_key = 'share:' || share_grant_id || ':' || share_viewer_key)
    )
) STRICT;

-- next statement
INSERT INTO media_sessions_principal_new (owner_key, principal_kind, share_grant_id, share_viewer_key, incarnation_id, session_id, user_id, playback_id, request_fingerprint, owner_node_id, owner_epoch, lease_expires_at_ms, state, terminal_reason, publication_ready_at_ms, recipe_json, response_json, produced_playable_through_ms, fetched_through_ms, media_origin_ms, media_sequence, discontinuity_sequence, updated_at_ms, recovery_epoch, drain_deadline_ms)
SELECT 'local:' || CAST(user_id AS TEXT), 'local', NULL, NULL, incarnation_id, session_id, user_id, playback_id, request_fingerprint, owner_node_id, owner_epoch, lease_expires_at_ms, state, terminal_reason, publication_ready_at_ms, recipe_json, response_json, produced_playable_through_ms, fetched_through_ms, media_origin_ms, media_sequence, discontinuity_sequence, updated_at_ms, recovery_epoch, drain_deadline_ms FROM media_sessions;

-- next statement
-- Preserve the child rows while replacing their parent. The replicated writer
-- enforces foreign keys; SQLite's migration runner temporarily turns them off.
-- Explicitly evacuating both children handles either setting in one atomic
-- transaction and avoids DROP TABLE's ON DELETE CASCADE losing relay authority.
CREATE TABLE media_session_principal_relay_backup AS SELECT * FROM sharing_relay_upstream;

-- next statement
CREATE TABLE media_session_principal_delivery_backup AS SELECT * FROM sharing_delivery_grants;

-- next statement
DELETE FROM sharing_delivery_grants;

-- next statement
DELETE FROM sharing_relay_upstream;

-- next statement
DROP TABLE media_sessions;

-- next statement
ALTER TABLE media_sessions_principal_new RENAME TO media_sessions;

-- next statement
INSERT INTO sharing_relay_upstream SELECT * FROM media_session_principal_relay_backup;

-- next statement
INSERT INTO sharing_delivery_grants SELECT * FROM media_session_principal_delivery_backup;

-- next statement
DROP TABLE media_session_principal_delivery_backup;

-- next statement
DROP TABLE media_session_principal_relay_backup;

-- next statement
CREATE TABLE media_session_preparations_principal_new (
        owner_key TEXT NOT NULL,
    principal_kind TEXT NOT NULL DEFAULT 'local' CHECK (principal_kind IN ('local', 'sharing')),
    user_id INTEGER,
    share_grant_id TEXT,
    share_viewer_key TEXT,
        playback_id                        TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        staged_incarnation_id              TEXT NOT NULL UNIQUE,
        expected_predecessor_incarnation_id TEXT NOT NULL
            CHECK (length(expected_predecessor_incarnation_id) BETWEEN 1 AND 128),
        deadline_ms                        INTEGER NOT NULL,
        created_at_ms                      INTEGER NOT NULL,
        updated_at_ms                      INTEGER NOT NULL,
        PRIMARY KEY (owner_key, playback_id)
    ,
    CHECK (
      (principal_kind = 'local' AND user_id IS NOT NULL AND user_id > 0
       AND share_grant_id IS NULL AND share_viewer_key IS NULL
       AND owner_key = 'local:' || CAST(user_id AS TEXT))
      OR (principal_kind = 'sharing' AND user_id IS NULL
       AND share_grant_id IS NOT NULL AND share_viewer_key IS NOT NULL
       AND length(share_grant_id) = 36
       AND substr(share_grant_id,9,1) = '-' AND substr(share_grant_id,14,1) = '-'
       AND substr(share_grant_id,19,1) = '-' AND substr(share_grant_id,24,1) = '-'
       AND length(replace(share_grant_id,'-','')) = 32
       AND replace(share_grant_id,'-','') NOT GLOB '*[^0-9a-f]*'
       AND length(share_viewer_key) = 64 AND share_viewer_key NOT GLOB '*[^0-9a-f]*'
       AND owner_key = 'share:' || share_grant_id || ':' || share_viewer_key)
    )
) STRICT;

-- next statement
INSERT INTO media_session_preparations_principal_new (owner_key, principal_kind, share_grant_id, share_viewer_key, user_id, playback_id, staged_incarnation_id, expected_predecessor_incarnation_id, deadline_ms, created_at_ms, updated_at_ms)
SELECT 'local:' || CAST(user_id AS TEXT), 'local', NULL, NULL, user_id, playback_id, staged_incarnation_id, expected_predecessor_incarnation_id, deadline_ms, created_at_ms, updated_at_ms FROM media_session_preparations;

-- next statement
DROP TABLE media_session_preparations;

-- next statement
ALTER TABLE media_session_preparations_principal_new RENAME TO media_session_preparations;

-- next statement
CREATE TABLE media_playback_desired_principal_new (
        owner_key TEXT NOT NULL,
    principal_kind TEXT NOT NULL DEFAULT 'local' CHECK (principal_kind IN ('local', 'sharing')),
    user_id INTEGER,
    share_grant_id TEXT,
    share_viewer_key TEXT,
        playback_id    TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        revision       INTEGER NOT NULL CHECK (revision > 0),
        digest         TEXT NOT NULL CHECK (length(digest) = 64),
        canonical_form TEXT NOT NULL
            CHECK (length(canonical_form) BETWEEN 1 AND 512),
        updated_at_ms  INTEGER NOT NULL,
        PRIMARY KEY (owner_key, playback_id)
    ,
    CHECK (
      (principal_kind = 'local' AND user_id IS NOT NULL AND user_id > 0
       AND share_grant_id IS NULL AND share_viewer_key IS NULL
       AND owner_key = 'local:' || CAST(user_id AS TEXT))
      OR (principal_kind = 'sharing' AND user_id IS NULL
       AND share_grant_id IS NOT NULL AND share_viewer_key IS NOT NULL
       AND length(share_grant_id) = 36
       AND substr(share_grant_id,9,1) = '-' AND substr(share_grant_id,14,1) = '-'
       AND substr(share_grant_id,19,1) = '-' AND substr(share_grant_id,24,1) = '-'
       AND length(replace(share_grant_id,'-','')) = 32
       AND replace(share_grant_id,'-','') NOT GLOB '*[^0-9a-f]*'
       AND length(share_viewer_key) = 64 AND share_viewer_key NOT GLOB '*[^0-9a-f]*'
       AND owner_key = 'share:' || share_grant_id || ':' || share_viewer_key)
    )
) STRICT;

-- next statement
INSERT INTO media_playback_desired_principal_new (owner_key, principal_kind, share_grant_id, share_viewer_key, user_id, playback_id, revision, digest, canonical_form, updated_at_ms)
SELECT 'local:' || CAST(user_id AS TEXT), 'local', NULL, NULL, user_id, playback_id, revision, digest, canonical_form, updated_at_ms FROM media_playback_desired;

-- next statement
DROP TABLE media_playback_desired;

-- next statement
ALTER TABLE media_playback_desired_principal_new RENAME TO media_playback_desired;

-- next statement
CREATE TABLE media_session_producer_recovery_principal_new (
        owner_key TEXT NOT NULL,
    principal_kind TEXT NOT NULL DEFAULT 'local' CHECK (principal_kind IN ('local', 'sharing')),
    user_id INTEGER,
    share_grant_id TEXT,
    share_viewer_key TEXT,
        playback_id             TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        recovery_epoch          TEXT NOT NULL
            CHECK (length(recovery_epoch) BETWEEN 1 AND 128),
        failed_incarnation_id   TEXT NOT NULL,
        failed_producer_attempt INTEGER NOT NULL CHECK (failed_producer_attempt > 0),
        decision_sequence       INTEGER NOT NULL CHECK (decision_sequence > 0),
        failed_plan_digest      TEXT NOT NULL CHECK (length(failed_plan_digest) = 64),
        alternate_plan_digest   TEXT NOT NULL CHECK (length(alternate_plan_digest) = 64),
        decode_restriction      TEXT
            CHECK (decode_restriction IS NULL OR
                   length(CAST(decode_restriction AS BLOB)) <= 4096),
        state                   TEXT NOT NULL
            CHECK (state IN ('reserved', 'installed', 'exhausted')),
        created_at_ms           INTEGER NOT NULL,
        updated_at_ms           INTEGER NOT NULL,
        PRIMARY KEY (owner_key, playback_id, recovery_epoch)
    ,
    CHECK (
      (principal_kind = 'local' AND user_id IS NOT NULL AND user_id > 0
       AND share_grant_id IS NULL AND share_viewer_key IS NULL
       AND owner_key = 'local:' || CAST(user_id AS TEXT))
      OR (principal_kind = 'sharing' AND user_id IS NULL
       AND share_grant_id IS NOT NULL AND share_viewer_key IS NOT NULL
       AND length(share_grant_id) = 36
       AND substr(share_grant_id,9,1) = '-' AND substr(share_grant_id,14,1) = '-'
       AND substr(share_grant_id,19,1) = '-' AND substr(share_grant_id,24,1) = '-'
       AND length(replace(share_grant_id,'-','')) = 32
       AND replace(share_grant_id,'-','') NOT GLOB '*[^0-9a-f]*'
       AND length(share_viewer_key) = 64 AND share_viewer_key NOT GLOB '*[^0-9a-f]*'
       AND owner_key = 'share:' || share_grant_id || ':' || share_viewer_key)
    )
) STRICT;

-- next statement
INSERT INTO media_session_producer_recovery_principal_new (owner_key, principal_kind, share_grant_id, share_viewer_key, user_id, playback_id, recovery_epoch, failed_incarnation_id, failed_producer_attempt, decision_sequence, failed_plan_digest, alternate_plan_digest, decode_restriction, state, created_at_ms, updated_at_ms)
SELECT 'local:' || CAST(user_id AS TEXT), 'local', NULL, NULL, user_id, playback_id, recovery_epoch, failed_incarnation_id, failed_producer_attempt, decision_sequence, failed_plan_digest, alternate_plan_digest, decode_restriction, state, created_at_ms, updated_at_ms FROM media_session_producer_recovery;

-- next statement
DROP TABLE media_session_producer_recovery;

-- next statement
ALTER TABLE media_session_producer_recovery_principal_new RENAME TO media_session_producer_recovery;

-- next statement
CREATE TABLE library_channel_session_recipes_principal_new (
    owner_key TEXT NOT NULL,
    principal_kind TEXT NOT NULL DEFAULT 'local' CHECK (principal_kind IN ('local', 'sharing')),
    user_id INTEGER,
    share_grant_id TEXT,
    share_viewer_key TEXT,
    request_id      TEXT NOT NULL,
    incarnation_id  TEXT NOT NULL UNIQUE,
    recipe_json     TEXT NOT NULL CHECK (length(recipe_json) BETWEEN 2 AND 32768),
    created_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (owner_key, request_id),
    CHECK (
      (principal_kind = 'local' AND user_id IS NOT NULL AND user_id > 0
       AND share_grant_id IS NULL AND share_viewer_key IS NULL
       AND owner_key = 'local:' || CAST(user_id AS TEXT))
      OR (principal_kind = 'sharing' AND user_id IS NULL
       AND share_grant_id IS NOT NULL AND share_viewer_key IS NOT NULL
       AND length(share_grant_id) = 36
       AND substr(share_grant_id,9,1) = '-' AND substr(share_grant_id,14,1) = '-'
       AND substr(share_grant_id,19,1) = '-' AND substr(share_grant_id,24,1) = '-'
       AND length(replace(share_grant_id,'-','')) = 32
       AND replace(share_grant_id,'-','') NOT GLOB '*[^0-9a-f]*'
       AND length(share_viewer_key) = 64 AND share_viewer_key NOT GLOB '*[^0-9a-f]*'
       AND owner_key = 'share:' || share_grant_id || ':' || share_viewer_key)
    )
) STRICT;

-- next statement
INSERT INTO library_channel_session_recipes_principal_new (owner_key, principal_kind, share_grant_id, share_viewer_key, user_id, request_id, incarnation_id, recipe_json, created_at_ms)
SELECT 'local:' || CAST(user_id AS TEXT), 'local', NULL, NULL, user_id, request_id, incarnation_id, recipe_json, created_at_ms FROM library_channel_session_recipes;

-- next statement
DROP TABLE library_channel_session_recipes;

-- next statement
ALTER TABLE library_channel_session_recipes_principal_new RENAME TO library_channel_session_recipes;

-- next statement
CREATE INDEX IF NOT EXISTS media_session_requests_expiry
        ON media_session_requests(state, claim_expires_at_ms);

-- next statement
CREATE INDEX IF NOT EXISTS media_sessions_owner
        ON media_sessions(owner_node_id, state, lease_expires_at_ms);

-- next statement
CREATE INDEX media_sessions_user
    ON media_sessions(user_id, state, lease_expires_at_ms) WHERE principal_kind = 'local';

-- next statement
CREATE INDEX IF NOT EXISTS media_sessions_principal
        ON media_sessions(owner_key, state, lease_expires_at_ms);

-- next statement
CREATE INDEX IF NOT EXISTS media_sessions_expiry
        ON media_sessions(state, lease_expires_at_ms, incarnation_id);

-- next statement
CREATE INDEX IF NOT EXISTS media_sessions_retention
        ON media_sessions(state, updated_at_ms, incarnation_id);

-- next statement
CREATE TRIGGER IF NOT EXISTS media_playback_pointers_desired_fence_ai
    BEFORE INSERT ON media_playback_pointers
    WHEN EXISTS (SELECT 1 FROM media_playback_desired
                  WHERE owner_key = NEW.owner_key AND playback_id = NEW.playback_id
                    AND revision IS NOT NEW.desired_revision)
    BEGIN
      SELECT RAISE(ABORT, 'playback pointer written against an ask that is not current');
    END;

-- next statement
CREATE TRIGGER IF NOT EXISTS media_playback_pointers_desired_fence_au
    BEFORE UPDATE ON media_playback_pointers
    WHEN EXISTS (SELECT 1 FROM media_playback_desired
                  WHERE owner_key = NEW.owner_key AND playback_id = NEW.playback_id
                    AND revision IS NOT NEW.desired_revision)
    BEGIN
      SELECT RAISE(ABORT, 'playback pointer written against an ask that is not current');
    END;

-- next statement
CREATE TRIGGER IF NOT EXISTS media_sessions_drain_ownership_fence_au
    BEFORE UPDATE OF owner_node_id, owner_epoch ON media_sessions
    WHEN OLD.drain_deadline_ms IS NOT NULL
     AND (NEW.owner_node_id IS NOT OLD.owner_node_id
          OR NEW.owner_epoch IS NOT OLD.owner_epoch)
    BEGIN
      SELECT RAISE(IGNORE);
    END;

-- next statement
CREATE TRIGGER IF NOT EXISTS media_session_publication_claim_au
    AFTER UPDATE OF publication_ready_at_ms ON media_sessions
    WHEN NEW.state = 'active' AND (
      (OLD.publication_ready_at_ms = 9223372036854775807
        AND NEW.publication_ready_at_ms != 9223372036854775807)
      OR (OLD.publication_ready_at_ms > 0
        AND OLD.publication_ready_at_ms < 9223372036854775807
        AND NEW.publication_ready_at_ms = 0)
    ) BEGIN
      UPDATE media_session_requests
         SET claim_expires_at_ms = CASE
               WHEN OLD.publication_ready_at_ms = 9223372036854775807
                    AND NEW.publication_ready_at_ms = 0
                 THEN NEW.lease_expires_at_ms
               WHEN OLD.publication_ready_at_ms = 9223372036854775807
                 THEN MIN(9223372036854775806,
                   NEW.publication_ready_at_ms
                     + (NEW.lease_expires_at_ms - OLD.updated_at_ms) + 1)
               ELSE MIN(9223372036854775806, NEW.lease_expires_at_ms + 1)
             END,
             updated_at_ms = NEW.updated_at_ms
       WHERE incarnation_id = NEW.incarnation_id
         AND owner_key = NEW.owner_key
         AND request_fingerprint = NEW.request_fingerprint
         AND playback_id = NEW.playback_id
         AND owner_node_id = NEW.owner_node_id
         AND state = 'starting';
    END;

-- next statement
CREATE TRIGGER library_channel_session_recipes_request_delete
AFTER DELETE ON media_session_requests BEGIN
    DELETE FROM library_channel_session_recipes
    WHERE owner_key = OLD.owner_key AND request_id = OLD.request_id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_viewer_refresh
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'refresh_analysis_viewers'
BEGIN
    UPDATE background_job_waiters SET
        deadline_ms = json_extract(NEW.request_json, '$.now_ms') + 120000,
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE request_scope IN ('playback-analysis','playback-artifact')
      AND state IN ('pending','awaiting_hydration')
      AND updated_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 30000
      AND deadline_ms > json_extract(NEW.request_json, '$.now_ms')
      AND (request_scope, request_id) IN (
          SELECT waiter.request_scope, waiter.request_id
          FROM background_job_waiters waiter
          JOIN media_sessions session ON ((session.principal_kind = 'local'
                AND session.user_id = json_extract(waiter.consumer_ref, '$.user_id'))
             OR (session.principal_kind = 'sharing'
                AND session.owner_key = json_extract(waiter.consumer_ref, '$.owner_key')))
            AND session.playback_id = json_extract(waiter.consumer_ref, '$.playback_id')
            AND json_extract(session.recipe_json, '$.request.file_id') = json_extract(waiter.consumer_ref, '$.file_id')
          JOIN media_playback_pointers pointer ON pointer.owner_key = session.owner_key
            AND pointer.playback_id = session.playback_id
            AND pointer.current_incarnation_id = session.incarnation_id
          WHERE waiter.request_scope IN ('playback-analysis','playback-artifact')
            AND waiter.state IN ('pending','awaiting_hydration')
            AND session.state = 'active'
            AND session.lease_expires_at_ms > json_extract(NEW.request_json, '$.now_ms')
          ORDER BY waiter.deadline_ms, waiter.request_id LIMIT 128);
    UPDATE background_jobs SET priority = 3
    WHERE kind = 'artifact_hydrate' AND state IN ('queued','running')
      AND EXISTS (SELECT 1 FROM background_job_waiters delivery
          JOIN background_job_waiters viewer ON viewer.job_id = delivery.consumer_ref
            AND viewer.target_node_id = delivery.target_node_id
          WHERE delivery.job_id = background_jobs.id
            AND delivery.consumer_kind = 'background_delivery'
            AND viewer.consumer_kind = 'playback_fragment'
            AND viewer.state IN ('pending','awaiting_hydration')
            AND viewer.deadline_ms > json_extract(NEW.request_json, '$.now_ms'));
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;

-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_viewer_session_ended
AFTER UPDATE OF state ON media_sessions WHEN NEW.state = 'ended' AND OLD.state != 'ended'
BEGIN
    UPDATE background_job_waiters SET state = 'cancelled', updated_at_ms = NEW.updated_at_ms
    WHERE request_scope IN ('playback-analysis','playback-artifact')
      AND state IN ('pending','awaiting_hydration')
      AND ((NEW.principal_kind = 'local'
           AND json_extract(consumer_ref, '$.user_id') = NEW.user_id)
        OR (NEW.principal_kind = 'sharing'
           AND json_extract(consumer_ref, '$.owner_key') = NEW.owner_key))
      AND json_extract(consumer_ref, '$.playback_id') = NEW.playback_id
      AND json_extract(consumer_ref, '$.file_id') = json_extract(NEW.recipe_json, '$.request.file_id')
      AND NOT EXISTS (SELECT 1 FROM media_sessions live
          WHERE live.owner_key = NEW.owner_key AND live.playback_id = NEW.playback_id
            AND live.incarnation_id != NEW.incarnation_id
            AND live.state = 'active'
            AND live.lease_expires_at_ms > NEW.updated_at_ms
            AND json_extract(live.recipe_json, '$.request.file_id') = json_extract(NEW.recipe_json, '$.request.file_id'));
END;

-- next statement
CREATE TRIGGER media_sessions_local_owner_delete
    BEFORE DELETE ON users
    
    BEGIN
      UPDATE sharing_delivery_grants SET state = 'revoked'
       WHERE incarnation_id IN (SELECT incarnation_id FROM media_sessions WHERE principal_kind = 'local' AND user_id = OLD.id);
      UPDATE media_sessions SET state = 'ended', terminal_reason = 'deleted', lease_expires_at_ms = 0
       WHERE principal_kind = 'local' AND user_id = OLD.id AND state != 'ended';
      UPDATE job_leases SET expires_at_ms = 0
       WHERE resource IN (SELECT 'session:' || incarnation_id FROM media_sessions WHERE principal_kind = 'local' AND user_id = OLD.id);
      UPDATE media_session_requests SET state = 'failed', claim_expires_at_ms = 0, response_json = NULL
       WHERE principal_kind = 'local' AND user_id = OLD.id;
      DELETE FROM media_playback_pointers WHERE principal_kind = 'local' AND user_id = OLD.id;
      DELETE FROM media_session_preparations WHERE principal_kind = 'local' AND user_id = OLD.id;
      DELETE FROM media_playback_desired WHERE principal_kind = 'local' AND user_id = OLD.id;
    END;

-- next statement
CREATE TRIGGER media_sessions_sharing_owner_revoke
    AFTER UPDATE OF state ON sharing_exports
    WHEN NEW.state = 'revoked' AND OLD.state != 'revoked'
    BEGIN
      UPDATE sharing_delivery_grants SET state = 'revoked'
       WHERE incarnation_id IN (SELECT incarnation_id FROM media_sessions WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id);
      UPDATE media_sessions SET state = 'ended', terminal_reason = 'revoked', lease_expires_at_ms = 0
       WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id AND state != 'ended';
      UPDATE job_leases SET expires_at_ms = 0
       WHERE resource IN (SELECT 'session:' || incarnation_id FROM media_sessions WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id);
      UPDATE media_session_requests SET state = 'failed', claim_expires_at_ms = 0, response_json = NULL
       WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id;
      DELETE FROM media_playback_pointers WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id;
      DELETE FROM media_session_preparations WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id;
      DELETE FROM media_playback_desired WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id;
    END;

-- next statement
CREATE TRIGGER media_sessions_sharing_owner_delete
    BEFORE DELETE ON sharing_exports
    
    BEGIN
      UPDATE sharing_delivery_grants SET state = 'revoked'
       WHERE incarnation_id IN (SELECT incarnation_id FROM media_sessions WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id);
      UPDATE media_sessions SET state = 'ended', terminal_reason = 'revoked', lease_expires_at_ms = 0
       WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id AND state != 'ended';
      UPDATE job_leases SET expires_at_ms = 0
       WHERE resource IN (SELECT 'session:' || incarnation_id FROM media_sessions WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id);
      UPDATE media_session_requests SET state = 'failed', claim_expires_at_ms = 0, response_json = NULL
       WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id;
      DELETE FROM media_playback_pointers WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id;
      DELETE FROM media_session_preparations WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id;
      DELETE FROM media_playback_desired WHERE principal_kind = 'sharing' AND share_grant_id = OLD.id;
    END;
