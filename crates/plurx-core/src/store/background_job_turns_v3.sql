-- AI subtitles: the held-job snapshot of translation_admission! follows
-- subtitles.ai.v1 (from_audio, and who='everyone' or an admin demand).
DROP VIEW dispatch_turn_translation_viewer;
-- next statement
-- Immutable v142 snapshot of translation_admission! for a held job's fence.
-- Later changes to translation authority must migrate this catalog definition
-- explicitly; changing the old text would strand already-upgraded clusters.
CREATE VIEW dispatch_turn_translation_viewer AS
SELECT j.id AS job_id,j.kind FROM background_jobs j CROSS JOIN dispatch_turn_attempt a
WHERE j.kind IN ('subtitle_translation_prepare','subtitle_translation_range') AND EXISTS (SELECT 1 FROM subtitle_translation_operations op JOIN files f ON f.id=op.file_id
      JOIN settings config ON config.key='subtitles.ai.v1'
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
        AND json_type(config.value,'$.from_audio')='true' ELSE 0 END
      AND EXISTS (SELECT 1 FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id
        WHERE d.operation_id=op.id AND d.operation_epoch=op.operation_epoch AND d.released=0
        AND d.expires_at_ms>a.now_ms AND (u.is_admin=1 OR EXISTS(SELECT 1 FROM settings ai_who WHERE ai_who.key='subtitles.ai.v1' AND CASE WHEN json_valid(ai_who.value) THEN json_extract(ai_who.value,'$.version')=1 AND json_extract(ai_who.value,'$.who')='everyone' ELSE 0 END))
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
