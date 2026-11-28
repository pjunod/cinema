DROP VIEW IF EXISTS dispatch_turn_contenders;
-- next statement
CREATE VIEW dispatch_turn_contenders AS SELECT r.job_id,min(i.created_at_ms) AS queued_ms FROM dispatch_turn_attempt a JOIN dispatch_turn_ready r ON r.expires_at_ms>a.now_ms JOIN background_jobs j ON j.id=r.job_id AND j.state='queued' AND j.revision=r.revision JOIN libraries l ON l.id=json_extract(j.payload_json,'$.library_id') AND l.paths=r.roots JOIN dispatch_turn_interest i ON i.job_id=j.id AND (i.deadline_ms IS NULL OR i.deadline_ms>a.now_ms) WHERE (j.retry_deadline_ms=0 OR j.retry_deadline_ms>a.now_ms) AND NOT EXISTS(SELECT 1 FROM settings WHERE key='internal.cluster_job_owner_removed.'||r.node_id) AND NOT EXISTS(SELECT 1 FROM job_leases lease WHERE lease.resource='scan:library:'||l.id AND (lease.expires_at_ms>a.now_ms OR lease.fence=9223372036854775807 OR lease.revision=9223372036854775807)) AND EXISTS(SELECT 1 FROM dispatch_turn_library_resources overlap JOIN json_each(a.keys_json) wanted ON overlap.resource_key=wanted.value OR overlap.resource_key='source_io' OR wanted.value='source_io' WHERE overlap.job_id=r.job_id AND overlap.node_id=r.node_id AND overlap.resource_key LIKE 'source_io%') AND NOT EXISTS(SELECT 1 FROM dispatch_turn_library_resources k WHERE k.job_id=r.job_id AND k.node_id=r.node_id AND k.resource_key LIKE 'source_io%' AND (EXISTS(SELECT 1 FROM source_lease_permissions fg WHERE (fg.resource_key=k.resource_key OR fg.resource_key='source_io' OR k.resource_key='source_io') AND ((fg.expires_at_ms>a.now_ms AND COALESCE(fg.permits_through_ms,0)<=a.now_ms) OR (fg.expires_at_ms<=a.now_ms AND fg.expires_at_ms>a.now_ms-60000 AND fg.source_quiescent=0))) OR ((k.resource_key='source_io' AND (SELECT COUNT(*) FROM source_leases held WHERE held.node_id=r.node_id AND (held.owner_kind!='playback' AND held.phase!='paused' AND held.expires_at_ms>a.now_ms-60000))>=(SELECT readers FROM source_lease_limits WHERE resource_key=k.resource_key)+1) OR (k.resource_key!='source_io' AND (SELECT COUNT(*) FROM source_leases held WHERE EXISTS(SELECT 1 FROM source_lease_effective_resources held_key WHERE held_key.lease_id=held.lease_id AND (held_key.resource_key=k.resource_key OR (substr(k.resource_key,1,15)='source_io:root/' AND NOT EXISTS(SELECT 1 FROM source_lease_effective_resources rooted WHERE rooted.lease_id=held.lease_id AND substr(rooted.resource_key,1,15)='source_io:root/') AND (held_key.resource_key='source_io' OR held_key.resource_key IN(SELECT resource_key FROM source_lease_root_resources WHERE root_key=k.resource_key))))) AND (held.owner_kind!='playback' AND held.phase!='paused' AND held.expires_at_ms>a.now_ms-60000))>=(SELECT readers FROM source_lease_limits WHERE resource_key=k.resource_key)+1)) OR ((k.resource_key='source_io' AND (SELECT COUNT(*) FROM source_leases held WHERE held.node_id=r.node_id AND (held.owner_kind!='playback' AND held.phase!='paused' AND held.expires_at_ms>a.now_ms-60000) AND NOT ((held.owner_kind='job' AND EXISTS (SELECT 1 FROM background_job_waiters interest
          WHERE interest.job_id = held.work_id
            AND interest.state IN ('pending','awaiting_hydration')
            AND (interest.deadline_ms IS NULL OR interest.deadline_ms > a.now_ms)
            AND (interest.consumer_kind IN ('playback_fragment','playback_qualification')
              OR (interest.consumer_kind = 'background_delivery' AND EXISTS (
                  SELECT 1 FROM background_job_waiters viewer
                  WHERE viewer.job_id = interest.consumer_ref
                    AND viewer.target_node_id = interest.target_node_id
                    AND viewer.consumer_kind = 'playback_fragment'
                    AND viewer.state IN ('pending','awaiting_hydration')
                    AND viewer.deadline_ms > a.now_ms))
              OR (interest.consumer_kind IN ('subtitle_translation','subtitle_translation_range') AND EXISTS (
                  SELECT 1 FROM background_jobs live_range WHERE live_range.id=interest.job_id
                    AND ((interest.consumer_kind='subtitle_translation_range' AND live_range.kind='subtitle_translation_range')
                      OR (interest.consumer_kind='subtitle_translation' AND live_range.kind='subtitle_translation_prepare')) AND (EXISTS (SELECT 1 FROM subtitle_translation_operations op JOIN files f ON f.id=op.file_id
      JOIN settings config ON config.key='subtitles.translation.config.v1'
      WHERE op.id=COALESCE(json_extract(live_range.payload_json,'$.operation_id'),json_extract(live_range.payload_json,'$.recipe.operation_id'))
      AND op.operation_epoch=COALESCE(json_extract(live_range.payload_json,'$.operation_epoch'),json_extract(live_range.payload_json,'$.recipe.operation_epoch'))
      AND op.stopped=0 AND op.state NOT IN ('failed','stopped','complete')
      AND op.target_node_id=json_extract(live_range.payload_json,'$.target_node_id')
      AND ((json_extract(live_range.payload_json,'$.kind')='subtitle_translation_prepare' AND op.preparation_job_id=live_range.id)
        OR (json_extract(live_range.payload_json,'$.kind')='subtitle_translation_range'
          AND op.preparation_job_id=json_extract(live_range.payload_json,'$.producer_preparation_job_id')))
      AND f.id=json_extract(live_range.payload_json,'$.file_id') AND f.probe_json IS NOT NULL
      AND f.size=json_extract(op.source_json,'$.source_size') AND f.mtime=json_extract(op.source_json,'$.source_mtime')
      AND CASE WHEN json_valid(config.value) THEN json_extract(config.value,'$.version')=1
        AND json_type(config.value,'$.enabled')='true' ELSE 0 END
      AND EXISTS (SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id
        WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch AND d.released=0
        AND d.expires_at_ms>a.now_ms AND u.is_admin=1
        AND (json_extract(live_range.payload_json,'$.kind')='subtitle_translation_prepare' OR
        json_extract(live_range.payload_json,'$.recipe.core_index') BETWEEN CAST(d.position_ms/json_extract(op.profile_json,'$.core_ms') AS INTEGER)
        AND CAST(d.position_ms/json_extract(op.profile_json,'$.core_ms') AS INTEGER)+min(2,CAST(d.playback_rate AS INTEGER)+1)
        OR EXISTS(SELECT 1 FROM subtitle_translation_range_continuities continuity
          WHERE continuity.job_id=live_range.id AND continuity.fence=live_range.fence
          AND continuity.demand_id=d.id AND continuity.continuity_id=d.continuity_id)))
      AND (json_extract(live_range.payload_json,'$.kind')='subtitle_translation_prepare' OR
        (op.asset_id=json_extract(live_range.payload_json,'$.recipe.asset_id') AND EXISTS (
          SELECT 1 FROM subtitle_translation_assets a WHERE a.id=op.asset_id AND a.retired_at_ms IS NULL
          AND a.source_json=op.source_json AND a.profile_json=op.profile_json)))))))
              OR (interest.consumer_kind = 'subtitle_source' AND EXISTS (
                  SELECT 1 FROM analysis_requests request
                  WHERE request.request_id = interest.consumer_ref
                    AND request.component = 'subtitle_source'
                    AND request.trigger = 'playback'
                    AND request.state IN ('queued','running')))))) OR (held.owner_kind='analysis' AND EXISTS (SELECT 1 FROM background_job_waiters viewer WHERE viewer.request_scope='playback-analysis' AND viewer.job_id=held.work_id AND viewer.state='pending' AND viewer.deadline_ms>a.now_ms))))>=(SELECT readers FROM source_lease_limits WHERE resource_key=k.resource_key)) OR (k.resource_key!='source_io' AND (SELECT COUNT(*) FROM source_leases held WHERE EXISTS(SELECT 1 FROM source_lease_effective_resources held_key WHERE held_key.lease_id=held.lease_id AND (held_key.resource_key=k.resource_key OR (substr(k.resource_key,1,15)='source_io:root/' AND NOT EXISTS(SELECT 1 FROM source_lease_effective_resources rooted WHERE rooted.lease_id=held.lease_id AND substr(rooted.resource_key,1,15)='source_io:root/') AND (held_key.resource_key='source_io' OR held_key.resource_key IN(SELECT resource_key FROM source_lease_root_resources WHERE root_key=k.resource_key))))) AND (held.owner_kind!='playback' AND held.phase!='paused' AND held.expires_at_ms>a.now_ms-60000) AND NOT ((held.owner_kind='job' AND EXISTS (SELECT 1 FROM background_job_waiters interest
          WHERE interest.job_id = held.work_id
            AND interest.state IN ('pending','awaiting_hydration')
            AND (interest.deadline_ms IS NULL OR interest.deadline_ms > a.now_ms)
            AND (interest.consumer_kind IN ('playback_fragment','playback_qualification')
              OR (interest.consumer_kind = 'background_delivery' AND EXISTS (
                  SELECT 1 FROM background_job_waiters viewer
                  WHERE viewer.job_id = interest.consumer_ref
                    AND viewer.target_node_id = interest.target_node_id
                    AND viewer.consumer_kind = 'playback_fragment'
                    AND viewer.state IN ('pending','awaiting_hydration')
                    AND viewer.deadline_ms > a.now_ms))
              OR (interest.consumer_kind IN ('subtitle_translation','subtitle_translation_range') AND EXISTS (
                  SELECT 1 FROM background_jobs live_range WHERE live_range.id=interest.job_id
                    AND ((interest.consumer_kind='subtitle_translation_range' AND live_range.kind='subtitle_translation_range')
                      OR (interest.consumer_kind='subtitle_translation' AND live_range.kind='subtitle_translation_prepare')) AND (EXISTS (SELECT 1 FROM subtitle_translation_operations op JOIN files f ON f.id=op.file_id
      JOIN settings config ON config.key='subtitles.translation.config.v1'
      WHERE op.id=COALESCE(json_extract(live_range.payload_json,'$.operation_id'),json_extract(live_range.payload_json,'$.recipe.operation_id'))
      AND op.operation_epoch=COALESCE(json_extract(live_range.payload_json,'$.operation_epoch'),json_extract(live_range.payload_json,'$.recipe.operation_epoch'))
      AND op.stopped=0 AND op.state NOT IN ('failed','stopped','complete')
      AND op.target_node_id=json_extract(live_range.payload_json,'$.target_node_id')
      AND ((json_extract(live_range.payload_json,'$.kind')='subtitle_translation_prepare' AND op.preparation_job_id=live_range.id)
        OR (json_extract(live_range.payload_json,'$.kind')='subtitle_translation_range'
          AND op.preparation_job_id=json_extract(live_range.payload_json,'$.producer_preparation_job_id')))
      AND f.id=json_extract(live_range.payload_json,'$.file_id') AND f.probe_json IS NOT NULL
      AND f.size=json_extract(op.source_json,'$.source_size') AND f.mtime=json_extract(op.source_json,'$.source_mtime')
      AND CASE WHEN json_valid(config.value) THEN json_extract(config.value,'$.version')=1
        AND json_type(config.value,'$.enabled')='true' ELSE 0 END
      AND EXISTS (SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id
        WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch AND d.released=0
        AND d.expires_at_ms>a.now_ms AND u.is_admin=1
        AND (json_extract(live_range.payload_json,'$.kind')='subtitle_translation_prepare' OR
        json_extract(live_range.payload_json,'$.recipe.core_index') BETWEEN CAST(d.position_ms/json_extract(op.profile_json,'$.core_ms') AS INTEGER)
        AND CAST(d.position_ms/json_extract(op.profile_json,'$.core_ms') AS INTEGER)+min(2,CAST(d.playback_rate AS INTEGER)+1)
        OR EXISTS(SELECT 1 FROM subtitle_translation_range_continuities continuity
          WHERE continuity.job_id=live_range.id AND continuity.fence=live_range.fence
          AND continuity.demand_id=d.id AND continuity.continuity_id=d.continuity_id)))
      AND (json_extract(live_range.payload_json,'$.kind')='subtitle_translation_prepare' OR
        (op.asset_id=json_extract(live_range.payload_json,'$.recipe.asset_id') AND EXISTS (
          SELECT 1 FROM subtitle_translation_assets a WHERE a.id=op.asset_id AND a.retired_at_ms IS NULL
          AND a.source_json=op.source_json AND a.profile_json=op.profile_json)))))))
              OR (interest.consumer_kind = 'subtitle_source' AND EXISTS (
                  SELECT 1 FROM analysis_requests request
                  WHERE request.request_id = interest.consumer_ref
                    AND request.component = 'subtitle_source'
                    AND request.trigger = 'playback'
                    AND request.state IN ('queued','running')))))) OR (held.owner_kind='analysis' AND EXISTS (SELECT 1 FROM background_job_waiters viewer WHERE viewer.request_scope='playback-analysis' AND viewer.job_id=held.work_id AND viewer.state='pending' AND viewer.deadline_ms>a.now_ms))))>=(SELECT readers FROM source_lease_limits WHERE resource_key=k.resource_key))))) GROUP BY r.job_id;