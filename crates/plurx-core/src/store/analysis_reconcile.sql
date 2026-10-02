-- Both stores execute these statements in one transaction. The JSON parameters
-- are server-derived snapshots, never SQL supplied by the caller.
WITH input AS (SELECT $1 AS original, $2 AS replacement, $3 AS now_ms)
INSERT INTO analysis_requests
  (request_id, file_id, source_size, source_mtime, component, pipeline_version,
   video_identity, requested_generation, expected_predecessor_generation,
   priority, trigger, force_rebuild, target_node_id, state, fence, attempts,
   not_before_ms, cancel_requested, created_at_ms, updated_at_ms)
SELECT json_extract((SELECT replacement FROM input), '$.request_id'), old.file_id,
  json_extract((SELECT replacement FROM input), '$.source_size'), json_extract((SELECT replacement FROM input), '$.source_mtime'), old.component,
  json_extract((SELECT replacement FROM input), '$.pipeline_version'), old.video_identity,
  json_extract((SELECT replacement FROM input), '$.requested_generation'), old.expected_predecessor_generation,
  old.priority, old.trigger, old.force_rebuild, json_extract((SELECT replacement FROM input), '$.target_node_id'),
  'queued', 0, 0, (SELECT now_ms FROM input), 0, (SELECT now_ms FROM input), (SELECT now_ms FROM input)
FROM analysis_requests old
WHERE old.request_id = json_extract((SELECT original FROM input), '$.request_id')
  AND old.fence = json_extract((SELECT original FROM input), '$.fence')
  AND old.updated_at_ms = json_extract((SELECT original FROM input), '$.updated_at_ms')
  AND old.state = 'queued' AND old.component = 'fragment_index'
  AND COALESCE(old.result_cache_key, '') = '' AND old.cancel_requested = 0
  AND EXISTS (SELECT 1 FROM files WHERE id = old.file_id
    AND size = json_extract((SELECT replacement FROM input), '$.source_size')
    AND mtime = json_extract((SELECT replacement FROM input), '$.source_mtime'))
  AND NOT EXISTS (SELECT 1 FROM background_job_waiters viewer
    WHERE viewer.request_scope = 'playback-analysis' AND viewer.job_id = old.request_id
      AND viewer.state = 'pending' AND viewer.deadline_ms > (SELECT now_ms FROM input))
  AND NOT EXISTS (SELECT 1 FROM analysis_requests current
    WHERE current.request_id <> old.request_id AND current.file_id = old.file_id
      AND current.source_size = json_extract((SELECT replacement FROM input), '$.source_size')
      AND current.source_mtime = json_extract((SELECT replacement FROM input), '$.source_mtime')
      AND current.component = old.component AND current.video_identity = old.video_identity
      AND current.pipeline_version = json_extract((SELECT replacement FROM input), '$.pipeline_version')
      AND current.target_node_id = json_extract((SELECT replacement FROM input), '$.target_node_id')
      AND (current.requested_generation = json_extract((SELECT replacement FROM input), '$.requested_generation')
        OR (current.force_rebuild = 1 AND (current.state IN ('queued','running','submitted')
          OR (old.force_rebuild = 0 AND current.state = 'ready'))))
      AND current.state IN ('queued', 'running', 'submitted', 'ready'))
ON CONFLICT DO NOTHING;
-- next
WITH input AS (SELECT $1 AS original, $2 AS replacement, $3 AS now_ms)
UPDATE analysis_requests AS old
SET state = 'cancelled', fence = fence + 1, cancel_requested = 1,
    last_error_code = 'request_reconciled', updated_at_ms = (SELECT now_ms FROM input)
WHERE old.request_id = json_extract((SELECT original FROM input), '$.request_id')
  AND old.fence = json_extract((SELECT original FROM input), '$.fence')
  AND old.updated_at_ms = json_extract((SELECT original FROM input), '$.updated_at_ms')
  AND old.state = 'queued' AND old.component = 'fragment_index'
  AND COALESCE(old.result_cache_key, '') = '' AND old.cancel_requested = 0
  AND EXISTS (SELECT 1 FROM files WHERE id = old.file_id
    AND size = json_extract((SELECT replacement FROM input), '$.source_size')
    AND mtime = json_extract((SELECT replacement FROM input), '$.source_mtime'))
  AND NOT EXISTS (SELECT 1 FROM background_job_waiters viewer
    WHERE viewer.request_scope = 'playback-analysis' AND viewer.job_id = old.request_id
      AND viewer.state = 'pending' AND viewer.deadline_ms > (SELECT now_ms FROM input))
  AND EXISTS (SELECT 1 FROM analysis_requests current
    WHERE current.request_id <> old.request_id AND current.file_id = old.file_id
      AND current.source_size = json_extract((SELECT replacement FROM input), '$.source_size')
      AND current.source_mtime = json_extract((SELECT replacement FROM input), '$.source_mtime')
      AND current.component = old.component AND current.video_identity = old.video_identity
      AND current.pipeline_version = json_extract((SELECT replacement FROM input), '$.pipeline_version')
      AND current.target_node_id = json_extract((SELECT replacement FROM input), '$.target_node_id')
      AND (current.requested_generation = json_extract((SELECT replacement FROM input), '$.requested_generation')
        OR (current.force_rebuild = 1 AND (current.state IN ('queued','running','submitted')
          OR (old.force_rebuild = 0 AND current.state = 'ready'))))
      AND current.state IN ('queued', 'running', 'submitted', 'ready'));
