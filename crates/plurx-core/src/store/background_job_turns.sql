-- A worker has checked the library paths locally. This is demand, never
-- permission to read; the ordinary physical custody and claim rules still run.
CREATE TABLE dispatch_turn_ready (
    job_id TEXT NOT NULL REFERENCES background_jobs(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL, revision INTEGER NOT NULL, expires_at_ms INTEGER NOT NULL,
    roots TEXT NOT NULL, PRIMARY KEY(job_id,node_id)
) STRICT;
-- next statement
CREATE VIEW dispatch_turn_interest AS
SELECT r.job_id,w.deadline_ms,w.created_at_ms FROM background_library_requests r
JOIN background_job_waiters w ON w.request_scope='library' AND w.request_id=r.request_id AND w.job_id=r.job_id
WHERE w.state='pending' AND w.consumer_kind='library_work' AND r.result_json IS NULL
AND (json_extract(r.input_json,'$.kind')='targeted' OR
    (json_extract(r.input_json,'$.kind')='full' AND json_extract(r.input_json,'$.trigger')='manual'));
-- next statement
CREATE TRIGGER dispatch_turn_retire AFTER UPDATE OF state,revision ON background_jobs
WHEN NEW.state!='queued' OR NEW.revision!=OLD.revision BEGIN
    DELETE FROM dispatch_turn_ready WHERE job_id=NEW.id;
END;
-- next statement
-- One scratch verdict per replicated writer. Each admission overwrites it and
-- consumes the verdict in the SAME SQL statement. No lease or partial grant.
CREATE TABLE dispatch_turn_attempt (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1), owner TEXT NOT NULL, work TEXT NOT NULL,
    keys_json TEXT NOT NULL, now_ms INTEGER NOT NULL, queued_ms INTEGER NOT NULL, allowed INTEGER NOT NULL,
    winner TEXT, interactive INTEGER NOT NULL DEFAULT 0
) STRICT;
-- next statement
-- Actual otherwise-admissible maintenance claims earn one credit per
-- overlapping interactive grant. Only a fresh retry can reserve a fair turn;
-- a disappeared maintenance worker must never leave the queue idle.
CREATE TABLE dispatch_turn_wait (
    owner TEXT NOT NULL, work TEXT NOT NULL, keys_json TEXT NOT NULL, queued_ms INTEGER NOT NULL,
    last_seen_ms INTEGER NOT NULL, credits INTEGER NOT NULL, PRIMARY KEY(owner,work)
) STRICT;
-- next statement
-- These are the same source domains and provider slots used by a library
-- claim, derived from that worker's observed roots. No job history is read.
CREATE VIEW dispatch_turn_library_resources AS
SELECT r.job_id,r.node_id,coalesce('source_io:'||m.domain_id,'source_io') AS resource_key
FROM dispatch_turn_ready r JOIN background_jobs j ON j.id=r.job_id JOIN json_each(r.roots) root
LEFT JOIN background_storage_domains m ON m.library_id=json_extract(j.payload_json,'$.library_id') AND m.root_path=root.value
UNION ALL SELECT job_id,node_id,'provider:tmdb' FROM dispatch_turn_ready
UNION ALL SELECT job_id,node_id,'provider:anilist' FROM dispatch_turn_ready;
-- next statement
-- Only live consumer authority puts a job in the protected source slot.
-- Translation authority uses the claim's exact operation/demand predicate.
CREATE VIEW dispatch_turn_protected_jobs AS
SELECT w.job_id FROM background_job_waiters w CROSS JOIN dispatch_turn_attempt a
WHERE w.state IN ('pending','awaiting_hydration') AND (w.deadline_ms IS NULL OR w.deadline_ms>a.now_ms)
AND (w.consumer_kind='playback_fragment' OR (w.consumer_kind='background_delivery' AND EXISTS (
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
CREATE VIEW dispatch_turn_ordinary_occupancy AS
SELECT h.resource_key,h.job_id AS work_id FROM background_job_reservations h CROSS JOIN dispatch_turn_attempt a
WHERE h.expires_at_ms>a.now_ms AND NOT EXISTS(SELECT 1 FROM dispatch_turn_protected_jobs p WHERE p.job_id=h.job_id)
UNION ALL
SELECT h.resource_key,h.request_id FROM analysis_source_reservations h CROSS JOIN dispatch_turn_attempt a
WHERE h.expires_at_ms>a.now_ms AND NOT EXISTS(
    SELECT 1 FROM background_job_waiters viewer WHERE viewer.job_id=h.request_id AND viewer.request_scope='playback-analysis'
      AND viewer.state='pending' AND viewer.deadline_ms>a.now_ms)
UNION ALL
SELECT h.resource_key,h.work_id FROM source_io_occupancy h CROSS JOIN dispatch_turn_attempt a
WHERE h.owner_kind IN ('lease','offline') AND h.active_occupancy=1 AND (h.expires_at_ms>a.now_ms OR h.custody_id IS NOT NULL);
-- next statement
CREATE VIEW dispatch_turn_contenders AS
SELECT r.job_id,min(i.created_at_ms) AS queued_ms
FROM dispatch_turn_attempt a JOIN dispatch_turn_ready r ON r.expires_at_ms>a.now_ms
JOIN background_jobs j ON j.id=r.job_id AND j.state='queued' AND j.revision=r.revision
JOIN libraries l ON l.id=json_extract(j.payload_json,'$.library_id') AND l.paths=r.roots
JOIN dispatch_turn_interest i ON i.job_id=j.id AND (i.deadline_ms IS NULL OR i.deadline_ms>a.now_ms)
WHERE (j.retry_deadline_ms=0 OR j.retry_deadline_ms>a.now_ms)
AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||r.node_id)
-- Coordination's acquire_lease cannot reuse any live domain lease, even on
-- the same node. Owner removal alone does not bypass that lease's expiry.
AND NOT EXISTS(SELECT 1 FROM job_leases lease WHERE lease.resource='scan:library:'||l.id
    AND (lease.expires_at_ms>a.now_ms OR lease.fence=9223372036854775807 OR lease.revision=9223372036854775807))
AND EXISTS (SELECT 1 FROM json_each(r.roots) root
    LEFT JOIN background_storage_domains m ON m.library_id=l.id AND m.root_path=root.value
    JOIN json_each(a.keys_json) wanted ON m.domain_id IS NULL OR wanted.value='source_io'
        OR wanted.value='source_io:'||m.domain_id)
-- A ready scan can only displace other work if its complete bundle can be
-- granted at this instant. The unmapped source key overlaps every disk.
AND NOT EXISTS (
    SELECT 1 FROM dispatch_turn_library_resources k WHERE k.job_id=r.job_id AND k.node_id=r.node_id
    AND (EXISTS (
        SELECT 1 FROM source_io_occupancy held
        WHERE (held.resource_key=k.resource_key OR (held.resource_key LIKE 'source_io%' AND k.resource_key LIKE 'source_io%'
          AND (held.resource_key='source_io' OR k.resource_key='source_io')))
        AND held.active_occupancy=1 AND held.custody_id IS NOT NULL AND held.expires_at_ms<=a.now_ms)
      OR (SELECT count(DISTINCT json_array(held.owner_kind,held.work_id,held.fence)) FROM source_io_occupancy held
        WHERE (held.resource_key=k.resource_key OR (held.resource_key LIKE 'source_io%' AND k.resource_key LIKE 'source_io%'
          AND (held.resource_key='source_io' OR k.resource_key='source_io')))
        AND held.active_occupancy=1 AND (held.expires_at_ms>a.now_ms OR held.custody_id IS NOT NULL)
        AND (held.work_id!=r.job_id OR held.custody_id IS NOT NULL))>=2
      OR (k.resource_key LIKE 'source_io%' AND (EXISTS (
          SELECT 1 FROM foreground_source_permissions fg
          WHERE (fg.resource_key=k.resource_key OR fg.resource_key='source_io' OR k.resource_key='source_io')
          AND (fg.lease_expires_at_ms>a.now_ms OR fg.custody_held=1)
          AND (fg.lease_expires_at_ms<=a.now_ms OR coalesce(fg.permits_through_ms,0)<=a.now_ms))
        OR EXISTS (SELECT 1 FROM dispatch_turn_ordinary_occupancy held
          WHERE held.resource_key LIKE 'source_io%' AND held.work_id!=r.job_id
          AND (held.resource_key=k.resource_key OR held.resource_key='source_io' OR k.resource_key='source_io'))))))
GROUP BY r.job_id;
-- next statement
CREATE VIEW dispatch_turn_fair AS
SELECT w.owner,w.work,w.queued_ms FROM dispatch_turn_wait w JOIN dispatch_turn_attempt a
WHERE w.credits>=8 AND w.queued_ms<=a.now_ms-60000 AND w.last_seen_ms>a.now_ms-7000
AND EXISTS(SELECT 1 FROM json_each(w.keys_json) x JOIN json_each(a.keys_json) y
    ON x.value=y.value OR x.value='source_io' OR y.value='source_io');
-- next statement
CREATE TRIGGER dispatch_turn_decide AFTER INSERT ON dispatch_turn_attempt BEGIN
    DELETE FROM dispatch_turn_wait WHERE last_seen_ms<NEW.now_ms-86400000;
    DELETE FROM dispatch_turn_ready WHERE expires_at_ms<=NEW.now_ms;
    -- Resolve the candidate bundles once under the writer. Reusing that
    -- decision also keeps SQLite from expanding the occupancy views repeatedly.
    UPDATE dispatch_turn_attempt SET (winner,interactive)=(
        WITH candidates AS MATERIALIZED (SELECT job_id,queued_ms FROM dispatch_turn_contenders)
        SELECT (SELECT job_id FROM candidates ORDER BY queued_ms,job_id LIMIT 1),
          NEW.owner='job' AND EXISTS(SELECT 1 FROM candidates WHERE job_id=NEW.work)
    ) WHERE singleton=1;
    INSERT INTO dispatch_turn_wait(owner,work,keys_json,queued_ms,last_seen_ms,credits)
      SELECT NEW.owner,NEW.work,NEW.keys_json,NEW.queued_ms,NEW.now_ms,0
      FROM dispatch_turn_attempt WHERE winner IS NOT NULL AND interactive=0
      ON CONFLICT(owner,work) DO UPDATE SET last_seen_ms=excluded.last_seen_ms,keys_json=excluded.keys_json;
    UPDATE dispatch_turn_attempt SET allowed=CASE
      WHEN winner IS NULL THEN 1
      WHEN EXISTS(SELECT 1 FROM dispatch_turn_fair) THEN EXISTS(
        SELECT 1 FROM (SELECT owner,work FROM dispatch_turn_fair ORDER BY queued_ms,owner,work LIMIT 1) fair
        WHERE fair.owner=NEW.owner AND fair.work=NEW.work)
      ELSE NEW.owner='job' AND NEW.work=winner
    END WHERE singleton=1;
    -- One overlapping maintenance grant serves the whole burst. Retain the
    -- other contenders and their ordering for the next burst of manual work.
    UPDATE dispatch_turn_wait SET credits=CASE (SELECT interactive FROM dispatch_turn_attempt)
        WHEN 1 THEN min(8,credits+1) ELSE 0 END
      WHERE (SELECT allowed FROM dispatch_turn_attempt)=1
        AND EXISTS(SELECT 1 FROM json_each(keys_json) x JOIN json_each(NEW.keys_json) y
          ON x.value=y.value OR x.value='source_io' OR y.value='source_io');
    DELETE FROM dispatch_turn_wait WHERE owner=NEW.owner AND work=NEW.work AND (SELECT allowed FROM dispatch_turn_attempt)=1;
END;
-- next statement
CREATE TRIGGER dispatch_turn_job BEFORE UPDATE OF state,fence ON background_jobs
WHEN NEW.state='running' AND NEW.fence>OLD.fence AND EXISTS(SELECT 1 FROM dispatch_turn_ready WHERE expires_at_ms>NEW.updated_at_ms)
AND NOT EXISTS(SELECT 1 FROM background_job_waiters w WHERE w.job_id=NEW.id AND w.state IN ('pending','awaiting_hydration')
    AND (w.deadline_ms IS NULL OR w.deadline_ms>NEW.updated_at_ms)
    AND (w.consumer_kind='playback_fragment' OR (w.consumer_kind='background_delivery' AND EXISTS(
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
-- next statement
CREATE TRIGGER dispatch_turn_analysis BEFORE UPDATE OF state,fence ON analysis_requests
WHEN NEW.state='running' AND NEW.fence>OLD.fence AND NEW.component IN ('fragment_index','skip_markers')
AND EXISTS(SELECT 1 FROM dispatch_turn_ready WHERE expires_at_ms>NEW.updated_at_ms)
AND NOT EXISTS(SELECT 1 FROM background_job_waiters v WHERE v.job_id=NEW.request_id AND v.request_scope='playback-analysis'
    AND v.state='pending' AND v.deadline_ms>NEW.updated_at_ms)
BEGIN
    DELETE FROM dispatch_turn_attempt;
    INSERT INTO dispatch_turn_attempt(singleton,owner,work,keys_json,now_ms,queued_ms,allowed)
    SELECT 1,'analysis',NEW.request_id,json_group_array(resource_key),NEW.updated_at_ms,OLD.created_at_ms,0
      FROM analysis_required_resources WHERE request_id=NEW.request_id HAVING count(*)>0;
    SELECT CASE WHEN (SELECT allowed FROM dispatch_turn_attempt)=0 THEN RAISE(IGNORE) END;
END;
-- next statement
CREATE TRIGGER dispatch_turn_independent BEFORE INSERT ON background_source_custody
WHEN NEW.owner_kind IN ('lease','offline') AND EXISTS(SELECT 1 FROM dispatch_turn_ready WHERE expires_at_ms>NEW.started_at_ms)
AND NOT EXISTS(SELECT 1 FROM background_source_custody WHERE owner_key=NEW.owner_key)
BEGIN
    DELETE FROM dispatch_turn_attempt;
    INSERT INTO dispatch_turn_attempt(singleton,owner,work,keys_json,now_ms,queued_ms,allowed)
    VALUES(1,NEW.owner_kind,NEW.work_id,NEW.resource_keys_json,NEW.started_at_ms,NEW.started_at_ms,0);
    SELECT CASE WHEN (SELECT allowed FROM dispatch_turn_attempt)=0 THEN RAISE(IGNORE) END;
END;
-- next statement
-- Immutable v124 snapshot of translation_admission! for a held job's fence.
-- Later changes to translation authority must migrate this catalog definition
-- explicitly; changing the old text would strand already-upgraded clusters.
CREATE VIEW dispatch_turn_translation_viewer AS
SELECT j.id AS job_id,j.kind FROM background_jobs j CROSS JOIN dispatch_turn_attempt a
WHERE j.kind IN ('subtitle_translation_prepare','subtitle_translation_range') AND EXISTS (SELECT 1 FROM subtitle_translation_operations op JOIN files f ON f.id=op.file_id
      JOIN settings config ON config.key='subtitles.translation.config.v1'
      WHERE op.id=COALESCE(json_extract(j.payload_json,'$.operation_id'),json_extract(j.payload_json,'$.recipe.operation_id'))
      AND op.operation_epoch=COALESCE(json_extract(j.payload_json,'$.operation_epoch'),json_extract(j.payload_json,'$.recipe.operation_epoch'))
      AND op.stopped=0 AND op.state NOT IN ('failed','stopped','complete')
      AND op.target_node_id=json_extract(j.payload_json,'$.target_node_id')
      AND ((json_extract(j.payload_json,'$.kind')='subtitle_translation_prepare' AND op.preparation_job_id=j.id)
        OR (json_extract(j.payload_json,'$.kind')='subtitle_translation_range'
          AND op.preparation_job_id=json_extract(j.payload_json,'$.producer_preparation_job_id')))
      AND f.id=json_extract(j.payload_json,'$.file_id') AND f.probe_json IS NOT NULL
      AND f.size=json_extract(op.source_json,'$.source_size') AND f.mtime=json_extract(op.source_json,'$.source_mtime')
      AND CASE WHEN json_valid(config.value) THEN json_extract(config.value,'$.version')=1
        AND json_type(config.value,'$.enabled')='true' ELSE 0 END
      AND EXISTS (SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id
        WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch AND d.released=0
        AND d.expires_at_ms>a.now_ms AND u.is_admin=1
        AND (json_extract(j.payload_json,'$.kind')='subtitle_translation_prepare' OR
        json_extract(j.payload_json,'$.recipe.core_index') BETWEEN CAST(d.position_ms/json_extract(op.profile_json,'$.core_ms') AS INTEGER)
        AND CAST(d.position_ms/json_extract(op.profile_json,'$.core_ms') AS INTEGER)+min(2,CAST(d.playback_rate AS INTEGER)+1)
        OR EXISTS(SELECT 1 FROM subtitle_translation_range_continuities continuity
          WHERE continuity.job_id=j.id AND continuity.fence=j.fence
          AND continuity.demand_id=d.id AND continuity.continuity_id=d.continuity_id)))
      AND (json_extract(j.payload_json,'$.kind')='subtitle_translation_prepare' OR
        (op.asset_id=json_extract(j.payload_json,'$.recipe.asset_id') AND EXISTS (
          SELECT 1 FROM subtitle_translation_assets a WHERE a.id=op.asset_id AND a.retired_at_ms IS NULL
          AND a.source_json=op.source_json AND a.profile_json=op.profile_json))));
