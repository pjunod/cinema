-- Recognize the existing finite FEL quality waiter in library dispatch turns.
-- The claim still applies source admission, authority and demand checks.
DROP TRIGGER dispatch_turn_job;
-- next statement
DROP VIEW dispatch_turn_protected_jobs;
-- next statement
-- Only live consumer authority puts a job in the protected source slot.
-- Translation authority uses the claim's exact operation/demand predicate.
CREATE VIEW dispatch_turn_protected_jobs AS
SELECT w.job_id FROM background_job_waiters w CROSS JOIN dispatch_turn_attempt a
WHERE w.state IN ('pending','awaiting_hydration') AND (w.deadline_ms IS NULL OR w.deadline_ms>a.now_ms)
AND (w.consumer_kind='playback_fragment'
    OR (w.consumer_kind='playback_qualification' AND w.deadline_ms IS NOT NULL
        AND EXISTS(SELECT 1 FROM background_jobs j WHERE j.id=w.job_id AND j.kind='dv_title_scan')) OR (w.consumer_kind='background_delivery' AND EXISTS (
        SELECT 1 FROM background_job_waiters viewer WHERE viewer.job_id=w.consumer_ref
          AND viewer.target_node_id=w.target_node_id AND viewer.consumer_kind='playback_fragment'
          AND viewer.state IN ('pending','awaiting_hydration') AND viewer.deadline_ms>a.now_ms))
    OR (w.consumer_kind='subtitle_source' AND EXISTS(SELECT 1 FROM analysis_requests v WHERE v.request_id=w.consumer_ref
          AND v.component='subtitle_source' AND v.trigger='playback' AND v.state IN ('queued','running')))
    OR (w.consumer_kind IN ('subtitle_translation','subtitle_translation_range') AND EXISTS(
         SELECT 1 FROM dispatch_turn_translation_viewer v WHERE v.job_id=w.job_id AND
           ((v.kind='subtitle_translation_prepare' AND w.consumer_kind='subtitle_translation')
             OR (v.kind='subtitle_translation_range' AND w.consumer_kind='subtitle_translation_range')))));
-- next statement
CREATE TRIGGER dispatch_turn_job BEFORE UPDATE OF state,fence ON background_jobs
WHEN NEW.state='running' AND NEW.fence>OLD.fence AND EXISTS(SELECT 1 FROM dispatch_turn_ready WHERE expires_at_ms>NEW.updated_at_ms)
AND NOT EXISTS(SELECT 1 FROM background_job_waiters w WHERE w.job_id=NEW.id AND w.state IN ('pending','awaiting_hydration')
    AND (w.deadline_ms IS NULL OR w.deadline_ms>NEW.updated_at_ms)
    AND (w.consumer_kind='playback_fragment'
      OR (NEW.kind='dv_title_scan' AND w.consumer_kind='playback_qualification' AND w.deadline_ms IS NOT NULL) OR (w.consumer_kind='background_delivery' AND EXISTS(
        SELECT 1 FROM background_job_waiters v WHERE v.job_id=w.consumer_ref AND v.target_node_id=w.target_node_id
          AND v.consumer_kind='playback_fragment' AND v.state IN ('pending','awaiting_hydration') AND v.deadline_ms>NEW.updated_at_ms))
      OR (w.consumer_kind='subtitle_source' AND EXISTS(SELECT 1 FROM analysis_requests v WHERE v.request_id=w.consumer_ref
          AND v.component='subtitle_source' AND v.trigger='playback' AND v.state IN ('queued','running')))
      -- The fenced claim already requires active translation demand on this exact job.
      OR (w.consumer_kind='subtitle_translation' AND NEW.kind='subtitle_translation_prepare')
      OR (w.consumer_kind='subtitle_translation_range' AND NEW.kind='subtitle_translation_range')))
BEGIN
    DELETE FROM dispatch_turn_attempt;
    INSERT INTO dispatch_turn_attempt(singleton,owner,work,keys_json,now_ms,queued_ms,allowed)
    SELECT 1,'job',NEW.id,json_group_array(resource_key),NEW.updated_at_ms,OLD.created_at_ms,0
      FROM background_job_required_resources WHERE job_id=NEW.id AND resource_key LIKE 'source_io%' HAVING count(*)>0;
    SELECT CASE WHEN (SELECT allowed FROM dispatch_turn_attempt)=0 THEN RAISE(IGNORE) END;
END;
