-- Recomputable, centrally fenced partial text output; no worker-local paths.
CREATE TABLE subtitle_text_translation_schema(singleton INTEGER PRIMARY KEY CHECK(singleton=1),version INTEGER NOT NULL CHECK(version=1)) STRICT;
-- next statement
INSERT INTO subtitle_text_translation_schema VALUES(1,1);
-- next statement
CREATE TABLE subtitle_text_translation_cues(
 job_id TEXT NOT NULL REFERENCES background_jobs(id) ON DELETE CASCADE,
 recipe_digest TEXT NOT NULL CHECK(length(recipe_digest)=64),
 cue_index INTEGER NOT NULL CHECK(cue_index>=0 AND cue_index<20000),
 input_sha256 TEXT NOT NULL CHECK(length(input_sha256)=64),
 start_ms INTEGER NOT NULL CHECK(start_ms>=0),
 end_ms INTEGER NOT NULL CHECK(end_ms>start_ms AND end_ms<=86400000),
 body TEXT NOT NULL CHECK(json_valid(body) AND length(CAST(body AS BLOB))<=32768),
 created_at_ms INTEGER NOT NULL,
 PRIMARY KEY(job_id,recipe_digest,cue_index)
) STRICT;
-- next statement
CREATE INDEX subtitle_text_translation_recipe ON subtitle_text_translation_cues(recipe_digest,cue_index,created_at_ms);
-- next statement
CREATE INDEX subtitle_text_translation_retirement ON subtitle_text_translation_cues(created_at_ms,job_id);
