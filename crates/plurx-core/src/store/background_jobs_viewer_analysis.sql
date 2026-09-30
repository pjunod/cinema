-- A viewer owns a bounded interest in the exact analysis generation. The
-- waiter starts before a fragment job exists and moves to that job in the
-- same transaction that submits the analysis result.
CREATE TRIGGER IF NOT EXISTS background_analysis_viewer_join
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'join_analysis_viewer'
BEGIN
    INSERT INTO background_job_waiters (
        request_scope, request_id, request_digest, job_id, consumer_kind,
        consumer_ref, priority, state, target_node_id, deadline_ms,
        receipt_expires_ms, created_at_ms, updated_at_ms)
    SELECT 'playback-analysis', json_extract(NEW.request_json, '$.consumer_id'),
        request.requested_generation,
        CASE WHEN request.state = 'submitted' THEN
            (SELECT job_id FROM background_job_waiters
                WHERE request_scope = 'analysis' AND request_id = request.request_id)
            ELSE request.request_id END,
        CASE WHEN request.state = 'submitted' THEN 'playback_fragment'
            ELSE 'playback_analysis' END,
        json_object('user_id', json_extract(NEW.request_json, '$.user_id'),
            'playback_id', json_extract(NEW.request_json, '$.playback_id'),
            'file_id', request.file_id, 'analysis_request_id', request.request_id),
        3, 'pending', request.target_node_id,
        json_extract(NEW.request_json, '$.now_ms') + 120000,
        json_extract(NEW.request_json, '$.now_ms') + 604800000,
        json_extract(NEW.request_json, '$.now_ms'), json_extract(NEW.request_json, '$.now_ms')
    FROM analysis_requests request JOIN files file ON file.id = request.file_id
        AND file.size = request.source_size AND file.mtime = request.source_mtime
    WHERE request.request_id = json_extract(NEW.request_json, '$.analysis_request_id')
        AND request.component = 'fragment_index'
        AND request.pipeline_version = json_extract(NEW.request_json, '$.pipeline_version')
        AND request.video_identity = json_extract(NEW.request_json, '$.video_identity')
        AND request.requested_generation = json_extract(NEW.request_json, '$.requested_generation')
        AND request.target_node_id = json_extract(NEW.request_json, '$.target_node_id')
        AND request.state IN ('queued','running','submitted')
        AND (request.state != 'submitted' OR EXISTS (
            SELECT 1 FROM background_job_waiters
            WHERE request_scope = 'analysis' AND request_id = request.request_id))
    ON CONFLICT(request_scope, request_id) DO UPDATE SET
        state = CASE WHEN background_job_waiters.state = 'cancelled'
            THEN 'pending' ELSE background_job_waiters.state END,
        job_id = CASE WHEN background_job_waiters.state = 'cancelled'
            THEN excluded.job_id ELSE background_job_waiters.job_id END,
        consumer_kind = CASE WHEN background_job_waiters.state = 'cancelled'
            THEN excluded.consumer_kind ELSE background_job_waiters.consumer_kind END,
        deadline_ms = CASE WHEN background_job_waiters.updated_at_ms <=
            json_extract(NEW.request_json, '$.now_ms') - 30000
            AND background_job_waiters.state IN ('pending','awaiting_hydration','cancelled')
            OR background_job_waiters.state = 'cancelled'
            THEN json_extract(NEW.request_json, '$.now_ms') + 120000
            ELSE background_job_waiters.deadline_ms END,
        updated_at_ms = CASE WHEN background_job_waiters.updated_at_ms <=
            json_extract(NEW.request_json, '$.now_ms') - 30000
            AND background_job_waiters.state IN ('pending','awaiting_hydration','cancelled')
            OR background_job_waiters.state = 'cancelled'
            THEN json_extract(NEW.request_json, '$.now_ms')
            ELSE background_job_waiters.updated_at_ms END;

    UPDATE analysis_requests SET priority = 'foreground', trigger = 'playback',
        updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
    WHERE request_id = json_extract(NEW.request_json, '$.analysis_request_id')
        AND state IN ('queued','running') AND force_rebuild = 0
        AND EXISTS (SELECT 1 FROM background_job_waiters waiter
            WHERE waiter.request_scope = 'playback-analysis'
              AND waiter.request_id = json_extract(NEW.request_json, '$.consumer_id')
              AND waiter.job_id = analysis_requests.request_id
              AND waiter.state = 'pending'
              AND waiter.deadline_ms > json_extract(NEW.request_json, '$.now_ms'));

    UPDATE background_jobs SET priority = 3
    WHERE id = (SELECT job_id FROM background_job_waiters
        WHERE request_scope = 'playback-analysis'
          AND request_id = json_extract(NEW.request_json, '$.consumer_id'))
      AND kind = 'fragment_index_build' AND state IN ('queued','running')
      AND EXISTS (SELECT 1 FROM background_job_waiters waiter
          WHERE waiter.job_id = background_jobs.id
            AND waiter.consumer_kind = 'playback_fragment'
            AND waiter.state = 'pending'
            AND waiter.deadline_ms > json_extract(NEW.request_json, '$.now_ms'));

    UPDATE background_jobs SET priority = 3
    WHERE kind = 'artifact_hydrate' AND state IN ('queued','running')
      AND EXISTS (SELECT 1 FROM background_job_waiters delivery
          JOIN background_job_waiters viewer ON viewer.job_id = delivery.consumer_ref
            AND viewer.target_node_id = delivery.target_node_id
          WHERE delivery.job_id = background_jobs.id
            AND delivery.consumer_kind = 'background_delivery'
            AND viewer.request_scope = 'playback-analysis'
            AND viewer.request_id = json_extract(NEW.request_json, '$.consumer_id')
            AND viewer.state IN ('pending','awaiting_hydration')
            AND viewer.deadline_ms > json_extract(NEW.request_json, '$.now_ms'));

    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_artifact_viewer_join
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'join_artifact_viewer'
BEGIN
    INSERT INTO background_job_waiters (
        request_scope, request_id, request_digest, job_id, consumer_kind,
        consumer_ref, priority, state, target_node_id, deadline_ms,
        receipt_expires_ms, created_at_ms, updated_at_ms)
    SELECT 'playback-artifact', json_extract(NEW.request_json, '$.consumer_id'),
        json_extract(NEW.request_json, '$.cache_key'), job.id, 'playback_fragment',
        json_object('user_id', json_extract(NEW.request_json, '$.user_id'),
            'playback_id', json_extract(NEW.request_json, '$.playback_id'),
            'file_id', json_extract(NEW.request_json, '$.file_id')),
        3, 'pending', json_extract(NEW.request_json, '$.target_node_id'),
        json_extract(NEW.request_json, '$.now_ms') + 120000,
        json_extract(NEW.request_json, '$.now_ms') + 604800000,
        json_extract(NEW.request_json, '$.now_ms'), json_extract(NEW.request_json, '$.now_ms')
    FROM background_jobs job JOIN background_job_waiters target
        ON target.job_id = job.id
        AND target.target_node_id = json_extract(NEW.request_json, '$.target_node_id')
        AND target.state IN ('pending','awaiting_hydration')
    WHERE job.kind = 'fragment_index_build'
        AND job.dedupe_key = 'fragment:' || json_extract(NEW.request_json, '$.cache_key')
        AND json_extract(job.payload_json, '$.file_id') = json_extract(NEW.request_json, '$.file_id')
        AND job.state IN ('queued','running')
    LIMIT 1
    ON CONFLICT(request_scope, request_id) DO UPDATE SET
        state = CASE WHEN background_job_waiters.state = 'cancelled'
            OR background_job_waiters.job_id != excluded.job_id
            THEN 'pending' ELSE background_job_waiters.state END,
        job_id = CASE WHEN background_job_waiters.state = 'cancelled'
            OR background_job_waiters.job_id != excluded.job_id
            THEN excluded.job_id ELSE background_job_waiters.job_id END,
        deadline_ms = CASE WHEN background_job_waiters.updated_at_ms <=
            json_extract(NEW.request_json, '$.now_ms') - 30000
            OR background_job_waiters.state = 'cancelled'
            OR background_job_waiters.job_id != excluded.job_id
            THEN json_extract(NEW.request_json, '$.now_ms') + 120000
            ELSE background_job_waiters.deadline_ms END,
        updated_at_ms = CASE WHEN background_job_waiters.updated_at_ms <=
            json_extract(NEW.request_json, '$.now_ms') - 30000
            OR background_job_waiters.state = 'cancelled'
            OR background_job_waiters.job_id != excluded.job_id
            THEN json_extract(NEW.request_json, '$.now_ms')
            ELSE background_job_waiters.updated_at_ms END;

    UPDATE background_jobs SET priority = 3
    WHERE id = (SELECT job_id FROM background_job_waiters
        WHERE request_scope = 'playback-artifact'
          AND request_id = json_extract(NEW.request_json, '$.consumer_id'))
      AND kind = 'fragment_index_build' AND state IN ('queued','running');
    UPDATE background_jobs SET priority = 3
    WHERE kind = 'artifact_hydrate' AND state IN ('queued','running')
      AND EXISTS (SELECT 1 FROM background_job_waiters delivery
          JOIN background_job_waiters viewer ON viewer.job_id = delivery.consumer_ref
            AND viewer.target_node_id = delivery.target_node_id
          WHERE delivery.job_id = background_jobs.id
            AND delivery.consumer_kind = 'background_delivery'
            AND viewer.request_scope = 'playback-artifact'
            AND viewer.request_id = json_extract(NEW.request_json, '$.consumer_id'));
    DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_viewers_submitted
AFTER UPDATE OF state ON analysis_requests
WHEN NEW.component = 'fragment_index' AND NEW.state = 'submitted' AND OLD.state != 'submitted'
BEGIN
    UPDATE background_job_waiters SET
        job_id = (SELECT job_id FROM background_job_waiters
            WHERE request_scope = 'analysis' AND request_id = NEW.request_id),
        consumer_kind = 'playback_fragment', updated_at_ms = NEW.updated_at_ms
    WHERE request_scope = 'playback-analysis' AND job_id = NEW.request_id
        AND state = 'pending' AND deadline_ms > NEW.updated_at_ms
        AND EXISTS (SELECT 1 FROM background_job_waiters
            WHERE request_scope = 'analysis' AND request_id = NEW.request_id);

    UPDATE background_jobs SET priority = 3
    WHERE id = (SELECT job_id FROM background_job_waiters
        WHERE request_scope = 'analysis' AND request_id = NEW.request_id)
      AND state IN ('queued','running')
      AND EXISTS (SELECT 1 FROM background_job_waiters waiter
          WHERE waiter.job_id = background_jobs.id
            AND waiter.consumer_kind = 'playback_fragment'
            AND waiter.state = 'pending' AND waiter.deadline_ms > NEW.updated_at_ms);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_viewer_retired
AFTER UPDATE OF state ON background_job_waiters
WHEN NEW.request_scope IN ('playback-analysis','playback-artifact')
    AND OLD.state IN ('pending','awaiting_hydration')
    AND NEW.state NOT IN ('pending','awaiting_hydration')
BEGIN
    UPDATE analysis_requests SET priority = 'normal', trigger = 'background',
        updated_at_ms = MAX(updated_at_ms, NEW.updated_at_ms)
    WHERE request_id = json_extract(NEW.consumer_ref, '$.analysis_request_id')
        AND component = 'fragment_index' AND force_rebuild = 0
        AND state IN ('queued','running')
        AND NOT EXISTS (SELECT 1 FROM background_job_waiters waiter
            WHERE waiter.request_scope = 'playback-analysis'
              AND json_extract(waiter.consumer_ref, '$.analysis_request_id') = analysis_requests.request_id
              AND waiter.state IN ('pending','awaiting_hydration')
              AND waiter.deadline_ms > NEW.updated_at_ms);
    UPDATE background_jobs SET priority = COALESCE((SELECT MAX(priority)
        FROM background_job_waiters waiter WHERE waiter.job_id = background_jobs.id
          AND waiter.state IN ('pending','awaiting_hydration')
          AND (waiter.deadline_ms IS NULL OR waiter.deadline_ms > NEW.updated_at_ms)), 0)
    WHERE id = NEW.job_id AND kind = 'fragment_index_build' AND state IN ('queued','running');
    UPDATE background_jobs SET priority = COALESCE((SELECT MAX(delivery.priority)
        FROM background_job_waiters delivery
        WHERE delivery.job_id = background_jobs.id
          AND delivery.state IN ('pending','awaiting_hydration')
          AND delivery.consumer_kind = 'background_delivery'
          AND EXISTS (SELECT 1 FROM background_job_waiters viewer
              WHERE viewer.job_id = delivery.consumer_ref
                AND viewer.target_node_id = delivery.target_node_id
                AND viewer.consumer_kind = 'playback_fragment'
                AND viewer.state IN ('pending','awaiting_hydration')
                AND viewer.deadline_ms > NEW.updated_at_ms)), 1)
    WHERE kind = 'artifact_hydrate' AND state IN ('queued','running')
      AND EXISTS (SELECT 1 FROM background_job_waiters delivery
          WHERE delivery.job_id = background_jobs.id
            AND delivery.consumer_ref = NEW.job_id
            AND delivery.consumer_kind = 'background_delivery');
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
          JOIN media_sessions session ON session.user_id = json_extract(waiter.consumer_ref, '$.user_id')
            AND session.playback_id = json_extract(waiter.consumer_ref, '$.playback_id')
            AND json_extract(session.recipe_json, '$.request.file_id') = json_extract(waiter.consumer_ref, '$.file_id')
          JOIN media_playback_pointers pointer ON pointer.user_id = session.user_id
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
      AND json_extract(consumer_ref, '$.user_id') = NEW.user_id
      AND json_extract(consumer_ref, '$.playback_id') = NEW.playback_id
      AND json_extract(consumer_ref, '$.file_id') = json_extract(NEW.recipe_json, '$.request.file_id')
      AND NOT EXISTS (SELECT 1 FROM media_sessions live
          WHERE live.user_id = NEW.user_id AND live.playback_id = NEW.playback_id
            AND live.incarnation_id != NEW.incarnation_id
            AND live.state = 'active'
            AND live.lease_expires_at_ms > NEW.updated_at_ms
            AND json_extract(live.recipe_json, '$.request.file_id') = json_extract(NEW.recipe_json, '$.request.file_id'));
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_viewer_request_retired
AFTER UPDATE OF state ON analysis_requests
WHEN NEW.component = 'fragment_index' AND NEW.state IN ('failed','cancelled')
    AND OLD.state NOT IN ('failed','cancelled')
BEGIN
    UPDATE background_job_waiters SET state = 'cancelled', updated_at_ms = NEW.updated_at_ms
    WHERE request_scope = 'playback-analysis'
      AND json_extract(consumer_ref, '$.analysis_request_id') = NEW.request_id
      AND state IN ('pending','awaiting_hydration');
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_analysis_viewer_request_removed
AFTER DELETE ON analysis_requests
BEGIN
    UPDATE background_job_waiters SET state = 'cancelled', updated_at_ms = MAX(updated_at_ms, OLD.updated_at_ms)
    WHERE request_scope = 'playback-analysis'
      AND json_extract(consumer_ref, '$.analysis_request_id') = OLD.request_id
      AND state IN ('pending','awaiting_hydration');
END;
-- next statement
-- Analysis hashes the source before it can submit a common fragment job.
-- Account that read in the same storage domain as durable job reservations.
CREATE VIEW IF NOT EXISTS analysis_required_resources AS
SELECT DISTINCT request.request_id,
    COALESCE('source_io:' || mapping.domain_id, 'source_io') AS resource_key
FROM analysis_requests request JOIN files file ON file.id = request.file_id
JOIN items item ON item.id = file.item_id
LEFT JOIN libraries library ON library.id = item.library_id
LEFT JOIN json_each(CASE WHEN json_valid(library.paths) THEN library.paths ELSE '[]' END) root
LEFT JOIN background_storage_domains mapping ON mapping.library_id = item.library_id
    AND mapping.root_path = root.value
WHERE request.component IN ('fragment_index','skip_markers');
-- next statement
CREATE TABLE IF NOT EXISTS analysis_source_reservations (
    request_id TEXT NOT NULL,
    resource_key TEXT NOT NULL,
    fence INTEGER NOT NULL CHECK (fence > 0),
    expires_at_ms INTEGER NOT NULL,
    PRIMARY KEY (request_id, resource_key)
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS analysis_source_reservations_resource
ON analysis_source_reservations(resource_key, expires_at_ms);
-- next statement
INSERT INTO analysis_source_reservations(request_id, resource_key, fence, expires_at_ms)
SELECT request.request_id, required.resource_key, request.fence, request.lease_expires_ms
FROM analysis_requests request JOIN analysis_required_resources required
  ON required.request_id = request.request_id
WHERE request.state = 'running' AND request.fence > 0 AND request.lease_expires_ms IS NOT NULL
ON CONFLICT(request_id, resource_key) DO NOTHING;
-- next statement
CREATE TRIGGER IF NOT EXISTS analysis_source_reserve_on_claim
AFTER UPDATE OF state, fence ON analysis_requests
WHEN NEW.state = 'running' AND NEW.fence > OLD.fence
    AND NEW.component IN ('fragment_index','skip_markers')
BEGIN
    DELETE FROM analysis_source_reservations WHERE request_id = NEW.request_id
        OR expires_at_ms <= NEW.updated_at_ms;
    INSERT INTO analysis_source_reservations(request_id, resource_key, fence, expires_at_ms)
    SELECT NEW.request_id, resource_key, NEW.fence, NEW.lease_expires_ms
    FROM analysis_required_resources WHERE request_id = NEW.request_id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS analysis_source_renew
AFTER UPDATE OF lease_expires_ms ON analysis_requests
WHEN NEW.state = 'running' AND NEW.fence = OLD.fence
    AND NEW.lease_expires_ms > OLD.lease_expires_ms
BEGIN
    UPDATE analysis_source_reservations SET expires_at_ms = NEW.lease_expires_ms
    WHERE request_id = NEW.request_id AND fence = NEW.fence;
END;
-- next statement
-- A terminal row can precede physical read cancellation. Keep the fenced
-- reservation until its lease expiry; the next claim prunes expired rows.
