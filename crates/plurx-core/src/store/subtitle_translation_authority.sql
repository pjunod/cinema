-- AI subtitles: a demand captures continuity when its user is an admin or
-- subtitles.ai.v1 says who='everyone', the same user rule as admission.
DROP TRIGGER subtitle_translation_capture_continuity;
-- next statement
CREATE TRIGGER subtitle_translation_capture_continuity AFTER UPDATE ON background_jobs
WHEN NEW.kind='subtitle_translation_range' AND NEW.state='running' AND NEW.fence>OLD.fence
BEGIN
 DELETE FROM subtitle_translation_range_continuities WHERE job_id=NEW.id;
 INSERT INTO subtitle_translation_range_continuities(job_id,fence,demand_id,continuity_id)
 SELECT NEW.id,NEW.fence,d.id,d.continuity_id FROM subtitle_translation_demands d JOIN users u ON u.id=d.user_id
 WHERE d.operation_id=json_extract(NEW.payload_json,'$.recipe.operation_id')
 AND d.operation_epoch=json_extract(NEW.payload_json,'$.recipe.operation_epoch')
 AND d.released=0 AND d.expires_at_ms>NEW.updated_at_ms AND (u.is_admin=1 OR EXISTS(SELECT 1 FROM settings ai_who WHERE ai_who.key='subtitles.ai.v1' AND CASE WHEN json_valid(ai_who.value) THEN json_extract(ai_who.value,'$.version')=1 AND json_extract(ai_who.value,'$.who')='everyone' ELSE 0 END)) AND d.continuity_id IS NOT NULL
 AND json_extract(NEW.payload_json,'$.recipe.core_index') BETWEEN CAST(d.position_ms/json_extract(NEW.payload_json,'$.recipe.profile.core_ms') AS INTEGER)
 AND CAST(d.position_ms/json_extract(NEW.payload_json,'$.recipe.profile.core_ms') AS INTEGER)+min(2,CAST(d.playback_rate AS INTEGER)+1)
 ORDER BY d.user_id,d.id LIMIT 16;
END;
