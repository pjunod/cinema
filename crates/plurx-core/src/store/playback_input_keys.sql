-- D3t: the planning binding covers every planning key. `playback_input_generation`
-- already moves on `playback.*` and `transcode.*` (PLAYBACK_INPUT_SCHEMA, left
-- untouched). These triggers add the planning keys outside those globs, under
-- new revision-tagged names, with their own marker. Additive: no schema version
-- bump on either store; a binary that predates them ignores the unknown names
-- and they keep firing. A later list change is a new revision (`_r2_*` triggers,
-- marker version 2), never an edit of these objects, so a binary that knows only
-- r1 still opens a store that carries r2.
CREATE TABLE IF NOT EXISTS playback_input_keys_schema (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    version INTEGER NOT NULL CHECK(version>=1)
) STRICT;
-- next statement
INSERT INTO playback_input_keys_schema VALUES(1,1) ON CONFLICT DO NOTHING;
-- next statement
CREATE TRIGGER IF NOT EXISTS playback_input_keys_r1_insert AFTER INSERT ON settings
WHEN NEW.key IN ('cache.max_gb','cluster.media_pool_enabled','cluster.session_takeover_enabled','subtitles.stored_sources','vod.output_preparation','vod.rolling_retention')
BEGIN UPDATE playback_input_generation SET generation = generation + 1 WHERE singleton = 1; END;
-- next statement
CREATE TRIGGER IF NOT EXISTS playback_input_keys_r1_update AFTER UPDATE ON settings
WHEN NEW.key IN ('cache.max_gb','cluster.media_pool_enabled','cluster.session_takeover_enabled','subtitles.stored_sources','vod.output_preparation','vod.rolling_retention') OR OLD.key IN ('cache.max_gb','cluster.media_pool_enabled','cluster.session_takeover_enabled','subtitles.stored_sources','vod.output_preparation','vod.rolling_retention')
BEGIN UPDATE playback_input_generation SET generation = generation + 1 WHERE singleton = 1; END;
-- next statement
CREATE TRIGGER IF NOT EXISTS playback_input_keys_r1_delete AFTER DELETE ON settings
WHEN OLD.key IN ('cache.max_gb','cluster.media_pool_enabled','cluster.session_takeover_enabled','subtitles.stored_sources','vod.output_preparation','vod.rolling_retention')
BEGIN UPDATE playback_input_generation SET generation = generation + 1 WHERE singleton = 1; END;
