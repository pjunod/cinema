-- Suspending a fallback viewer changes only that exact viewer's priority.
-- The queued computation, other viewers and physical source custody survive.
CREATE TABLE IF NOT EXISTS playback_analysis_suspensions (
    receipt_id TEXT NOT NULL,
    consumer_id TEXT NOT NULL,
    analysis_request_id TEXT NOT NULL,
    requested_generation TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    playback_id TEXT NOT NULL,
    incarnation_id TEXT NOT NULL,
    file_id INTEGER NOT NULL,
    source_size INTEGER NOT NULL,
    source_mtime INTEGER NOT NULL,
    prior_deadline_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('suspended','restoring')),
    PRIMARY KEY(receipt_id,consumer_id)
) STRICT;
-- next statement
CREATE TRIGGER IF NOT EXISTS playback_analysis_suspend_effects
AFTER INSERT ON background_job_commands WHEN NEW.operation='source_suspend_interest' AND NEW.result_json='true'
BEGIN
    INSERT INTO playback_analysis_suspensions
    SELECT json_extract(NEW.request_json,'$.receipt_id'),waiter.request_id,request.request_id,request.requested_generation,
        json_extract(NEW.request_json,'$.user_id'),json_extract(NEW.request_json,'$.playback_id'),json_extract(NEW.request_json,'$.incarnation_id'),
        request.file_id,request.source_size,request.source_mtime,waiter.deadline_ms,waiter.deadline_ms,'suspended'
    FROM background_job_waiters waiter JOIN analysis_requests request
      ON request.request_id=json_extract(waiter.consumer_ref,'$.analysis_request_id')
      AND request.requested_generation=waiter.request_digest
    JOIN files file ON file.id=request.file_id AND file.size=request.source_size AND file.mtime=request.source_mtime
    WHERE waiter.request_scope='playback-analysis' AND (waiter.state IN ('pending','awaiting_hydration') OR (waiter.state='cancelled' AND EXISTS(SELECT 1 FROM playback_analysis_suspensions prior WHERE prior.consumer_id=waiter.request_id AND prior.phase='suspended' AND prior.expires_at_ms>json_extract(NEW.request_json,'$.now_ms'))))
      AND waiter.deadline_ms>json_extract(NEW.request_json,'$.now_ms') AND request.state IN ('queued','running','submitted')
      AND json_extract(waiter.consumer_ref,'$.user_id')=json_extract(NEW.request_json,'$.user_id')
      AND json_extract(waiter.consumer_ref,'$.playback_id')=json_extract(NEW.request_json,'$.playback_id')
      AND request.file_id=json_extract(NEW.request_json,'$.file_id')
      AND request.source_size=json_extract(NEW.request_json,'$.source_size') AND request.source_mtime=json_extract(NEW.request_json,'$.source_mtime');
    UPDATE background_job_waiters SET state='cancelled',updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE request_scope='playback-analysis' AND request_id IN
      (SELECT consumer_id FROM playback_analysis_suspensions WHERE receipt_id=json_extract(NEW.request_json,'$.receipt_id'));
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS playback_analysis_suspended_renewal
AFTER UPDATE OF state ON background_job_waiters
WHEN NEW.request_scope='playback-analysis' AND NEW.state IN ('pending','awaiting_hydration')
  AND EXISTS(SELECT 1 FROM playback_analysis_suspensions held WHERE held.consumer_id=NEW.request_id
      AND held.phase='suspended' AND held.expires_at_ms>NEW.updated_at_ms)
BEGIN
    UPDATE background_job_waiters SET state='cancelled'
      WHERE request_scope=NEW.request_scope AND request_id=NEW.request_id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS playback_analysis_restore_effects
AFTER INSERT ON background_job_commands WHEN NEW.operation='source_restore_interest' AND NEW.result_json='true'
BEGIN
    UPDATE playback_analysis_suspensions SET phase='restoring' WHERE receipt_id=json_extract(NEW.request_json,'$.receipt_id');
    UPDATE background_job_waiters SET state='pending',updated_at_ms=json_extract(NEW.request_json,'$.now_ms'),
      deadline_ms=MIN(json_extract(NEW.request_json,'$.now_ms')+120000,(SELECT prior_deadline_ms FROM playback_analysis_suspensions held
        WHERE held.receipt_id=json_extract(NEW.request_json,'$.receipt_id') AND held.consumer_id=background_job_waiters.request_id)),
      job_id=COALESCE((SELECT CASE WHEN request.state='submitted' THEN
        (SELECT job_id FROM background_job_waiters submitted WHERE submitted.request_scope='analysis' AND submitted.request_id=request.request_id)
        ELSE request.request_id END FROM analysis_requests request JOIN playback_analysis_suspensions held ON held.analysis_request_id=request.request_id
          AND held.requested_generation=request.requested_generation WHERE held.receipt_id=json_extract(NEW.request_json,'$.receipt_id')
          AND held.consumer_id=background_job_waiters.request_id),job_id),
      consumer_kind=CASE WHEN EXISTS(SELECT 1 FROM analysis_requests request JOIN playback_analysis_suspensions held
          ON held.analysis_request_id=request.request_id AND held.requested_generation=request.requested_generation
          WHERE held.receipt_id=json_extract(NEW.request_json,'$.receipt_id') AND held.consumer_id=background_job_waiters.request_id
          AND request.state='submitted') THEN 'playback_fragment' ELSE 'playback_analysis' END
    WHERE request_scope='playback-analysis' AND state='cancelled'
      AND NOT EXISTS(SELECT 1 FROM playback_analysis_suspensions other WHERE other.consumer_id=background_job_waiters.request_id
        AND other.phase='suspended' AND other.expires_at_ms>json_extract(NEW.request_json,'$.now_ms'))
      AND request_id IN (SELECT held.consumer_id FROM playback_analysis_suspensions held JOIN analysis_requests request
        ON request.request_id=held.analysis_request_id AND request.requested_generation=held.requested_generation
        JOIN files file ON file.id=held.file_id AND file.size=held.source_size AND file.mtime=held.source_mtime
        WHERE held.receipt_id=json_extract(NEW.request_json,'$.receipt_id') AND held.prior_deadline_ms>json_extract(NEW.request_json,'$.now_ms')
          AND request.state IN ('queued','running','submitted') AND request.cancel_requested=0
          AND (EXISTS(SELECT 1 FROM media_session_requests demand WHERE demand.user_id=held.user_id
              AND demand.playback_id=held.playback_id AND demand.incarnation_id=held.incarnation_id AND demand.state='starting'
              AND demand.claim_expires_at_ms>json_extract(NEW.request_json,'$.now_ms'))
            OR EXISTS(SELECT 1 FROM media_sessions session JOIN media_playback_pointers pointer
              ON pointer.user_id=session.user_id AND pointer.playback_id=session.playback_id AND pointer.current_incarnation_id=session.incarnation_id
              WHERE session.user_id=held.user_id AND session.playback_id=held.playback_id AND session.incarnation_id=held.incarnation_id
                AND session.state='active' AND session.lease_expires_at_ms>json_extract(NEW.request_json,'$.now_ms')
                AND json_extract(session.recipe_json,'$.request.file_id')=held.file_id)));
    UPDATE analysis_requests SET priority='foreground',trigger='playback',updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
    WHERE force_rebuild=0 AND state IN ('queued','running') AND EXISTS(SELECT 1 FROM background_job_waiters waiter
      WHERE waiter.request_scope='playback-analysis' AND json_extract(waiter.consumer_ref,'$.analysis_request_id')=analysis_requests.request_id
        AND waiter.state='pending' AND waiter.deadline_ms>json_extract(NEW.request_json,'$.now_ms'));
    UPDATE background_jobs SET priority=COALESCE((SELECT MAX(priority) FROM background_job_waiters viewer
      WHERE viewer.job_id=background_jobs.id AND viewer.state IN ('pending','awaiting_hydration')
        AND (viewer.deadline_ms IS NULL OR viewer.deadline_ms>json_extract(NEW.request_json,'$.now_ms'))),0)
    WHERE kind='fragment_index_build' AND state IN ('queued','running') AND id IN
      (SELECT waiter.job_id FROM background_job_waiters waiter JOIN playback_analysis_suspensions held
        ON held.consumer_id=waiter.request_id WHERE held.receipt_id=json_extract(NEW.request_json,'$.receipt_id')
          AND waiter.request_scope='playback-analysis' AND waiter.state='pending');
    DELETE FROM playback_analysis_suspensions WHERE receipt_id=json_extract(NEW.request_json,'$.receipt_id');
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS playback_analysis_suspension_command_cleanup
AFTER INSERT ON background_job_commands WHEN NEW.operation IN ('source_suspend_interest','source_restore_interest') AND NEW.result_json='false'
BEGIN
    DELETE FROM background_job_commands WHERE id=NEW.id;
END;
