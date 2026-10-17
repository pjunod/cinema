-- Independently recognized extension: no historical object/view replacement.
CREATE TABLE subtitle_translation_schema(
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 version INTEGER NOT NULL CHECK(version=1)
) STRICT;
-- next statement
CREATE TABLE subtitle_translation_operations(
 id TEXT PRIMARY KEY,
 recipe_key TEXT NOT NULL UNIQUE,
 file_id INTEGER NOT NULL,
 source_json TEXT NOT NULL CHECK(json_valid(source_json)),
 profile_json TEXT NOT NULL CHECK(json_valid(profile_json)),
 audio_index INTEGER NOT NULL CHECK(audio_index>=0),
 spoken_language TEXT NOT NULL,
 duration_ms INTEGER NOT NULL CHECK(duration_ms>0 AND duration_ms<=14400000),
 target_node_id TEXT NOT NULL,
 operation_epoch INTEGER NOT NULL CHECK(operation_epoch>0),
 view_revision INTEGER NOT NULL CHECK(view_revision>0),
 state TEXT NOT NULL CHECK(state IN ('checking_source','waiting_model','waiting_capacity','translating','partial','suspended','stopped','complete','failed')),
 stopped INTEGER NOT NULL CHECK(stopped IN (0,1)),
 asset_id TEXT,
 preparation_job_id TEXT,
 reason TEXT,
 retryable INTEGER NOT NULL DEFAULT 0 CHECK(retryable IN (0,1)),
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX subtitle_translation_operation_file ON subtitle_translation_operations(file_id);
-- next statement
CREATE TABLE subtitle_translation_demands(
 id TEXT PRIMARY KEY,
 operation_id TEXT NOT NULL REFERENCES subtitle_translation_operations(id),
 operation_epoch INTEGER NOT NULL CHECK(operation_epoch>0),
 user_id INTEGER NOT NULL,
 playback_id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation>0),
 position_ms INTEGER NOT NULL CHECK(position_ms>=0 AND position_ms<=14400000),
 playback_rate REAL NOT NULL CHECK(playback_rate>0 AND playback_rate<=4),
 paused INTEGER NOT NULL CHECK(paused IN (0,1)),
 released INTEGER NOT NULL DEFAULT 0 CHECK(released IN (0,1)),
 expires_at_ms INTEGER NOT NULL,
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 UNIQUE(operation_id,operation_epoch,user_id,playback_id)
) STRICT;
-- next statement
CREATE INDEX subtitle_translation_demand_expiry ON subtitle_translation_demands(operation_id,operation_epoch,expires_at_ms);
-- next statement
CREATE TABLE subtitle_translation_assets(
 id TEXT PRIMARY KEY,
 operation_id TEXT NOT NULL REFERENCES subtitle_translation_operations(id),
 file_id INTEGER NOT NULL,
 source_json TEXT NOT NULL CHECK(json_valid(source_json)),
 source_sha256 TEXT NOT NULL CHECK(length(source_sha256)=64),
 profile_json TEXT NOT NULL CHECK(json_valid(profile_json)),
 profile_id TEXT NOT NULL CHECK(length(profile_id)=64),
 audio_index INTEGER NOT NULL,
 spoken_language TEXT NOT NULL,
 duration_ms INTEGER NOT NULL CHECK(duration_ms>0 AND duration_ms<=14400000),
 core_count INTEGER NOT NULL CHECK(core_count>0 AND core_count<=2048),
 manifest_revision INTEGER NOT NULL DEFAULT 0 CHECK(manifest_revision>=0),
 accepted_bytes INTEGER NOT NULL DEFAULT 0 CHECK(accepted_bytes>=0 AND accepted_bytes<=16777216),
 created_at_ms INTEGER NOT NULL,
 last_read_at_ms INTEGER NOT NULL,
 retired_at_ms INTEGER
) STRICT;
-- next statement
CREATE TABLE subtitle_translation_source_bindings(
 operation_id TEXT NOT NULL REFERENCES subtitle_translation_operations(id),
 operation_epoch INTEGER NOT NULL,
 node_id TEXT NOT NULL,
 boot_id TEXT NOT NULL,
 local_identity TEXT NOT NULL,
 source_sha256 TEXT NOT NULL CHECK(length(source_sha256)=64),
 verified_at_ms INTEGER NOT NULL,
 PRIMARY KEY(operation_id,operation_epoch,node_id)
) STRICT;
-- next statement
CREATE TABLE subtitle_translation_ranges(
 operation_id TEXT NOT NULL REFERENCES subtitle_translation_operations(id),
 operation_epoch INTEGER NOT NULL,
 core_index INTEGER NOT NULL CHECK(core_index>=0 AND core_index<2048),
 range_revision INTEGER NOT NULL CHECK(range_revision>0),
 state TEXT NOT NULL CHECK(state IN ('queued','running','failed','cancelled','succeeded')),
 job_id TEXT NOT NULL,
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts>=0 AND attempts<=3),
 reason TEXT,
 retryable INTEGER NOT NULL DEFAULT 0 CHECK(retryable IN (0,1)),
 updated_at_ms INTEGER NOT NULL,
 PRIMARY KEY(operation_id,operation_epoch,core_index)
) STRICT;
-- next statement
CREATE TABLE subtitle_translation_segments(
 asset_id TEXT NOT NULL REFERENCES subtitle_translation_assets(id),
 core_index INTEGER NOT NULL CHECK(core_index>=0 AND core_index<2048),
 segment_id TEXT NOT NULL UNIQUE CHECK(length(segment_id)=64),
 publication_id TEXT NOT NULL UNIQUE,
 digest TEXT NOT NULL CHECK(length(digest)=64),
 body TEXT NOT NULL CHECK(json_valid(body) AND length(CAST(body AS BLOB))<=16384),
 start_ms INTEGER NOT NULL CHECK(start_ms>=0),
 end_ms INTEGER NOT NULL CHECK(end_ms>start_ms AND end_ms<=14400000),
 accepted_revision INTEGER NOT NULL CHECK(accepted_revision>0),
 created_at_ms INTEGER NOT NULL,
 PRIMARY KEY(asset_id,core_index)
) STRICT;
-- next statement
CREATE TABLE subtitle_translation_snapshots(
 asset_id TEXT NOT NULL REFERENCES subtitle_translation_assets(id),
 manifest_revision INTEGER NOT NULL,
 created_at_ms INTEGER NOT NULL,
 PRIMARY KEY(asset_id,manifest_revision)
) STRICT;
-- next statement
CREATE TABLE subtitle_translation_read_leases(
 id TEXT PRIMARY KEY,
 user_id INTEGER NOT NULL,
 asset_id TEXT NOT NULL REFERENCES subtitle_translation_assets(id),
 manifest_revision INTEGER NOT NULL,
 expires_at_ms INTEGER NOT NULL,
 created_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX subtitle_translation_read_lease_expiry ON subtitle_translation_read_leases(asset_id,expires_at_ms);
-- next statement
CREATE TABLE subtitle_translation_requests(
 user_id INTEGER NOT NULL,
 request_id TEXT NOT NULL,
 body_digest TEXT NOT NULL CHECK(length(body_digest)=64),
 outcome_json TEXT NOT NULL CHECK(json_valid(outcome_json) AND length(CAST(outcome_json AS BLOB))<=2048),
 created_at_ms INTEGER NOT NULL,
 expires_at_ms INTEGER NOT NULL,
 PRIMARY KEY(user_id,request_id)
) STRICT;
-- next statement
CREATE TABLE subtitle_translation_capabilities(
 node_id TEXT NOT NULL,
 model_id TEXT NOT NULL,
 profile_id TEXT NOT NULL CHECK(length(profile_id)=64),
 receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
 PRIMARY KEY(node_id,model_id,profile_id)
) STRICT;
-- next statement
CREATE TABLE subtitle_translation_commands(
 id TEXT PRIMARY KEY,
 operation TEXT NOT NULL,
 request_json TEXT NOT NULL CHECK(json_valid(request_json)),
 result_json TEXT NOT NULL CHECK(json_valid(result_json))
) STRICT;
-- next statement
CREATE TRIGGER subtitle_translation_source_changed AFTER UPDATE OF size,mtime ON files
WHEN NEW.size!=OLD.size OR NEW.mtime!=OLD.mtime
BEGIN
 UPDATE subtitle_translation_operations SET
  operation_epoch=operation_epoch+1,view_revision=view_revision+1,
  state='failed',reason='source_changed',retryable=0
 WHERE file_id=OLD.id AND operation_epoch<9223372036854775807
 AND view_revision<9223372036854775807;
 UPDATE subtitle_translation_demands SET released=1
 WHERE operation_id IN (SELECT id FROM subtitle_translation_operations WHERE file_id=OLD.id);
 UPDATE background_jobs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancelling' END,
  revision=revision+1,last_error_code='source_changed'
 WHERE kind IN ('subtitle_translation_prepare','subtitle_translation_range')
  AND state IN ('queued','running') AND json_extract(payload_json,'$.file_id')=OLD.id
  AND revision<9223372036854775807;
END;
-- next statement
CREATE TRIGGER subtitle_translation_source_deleted AFTER DELETE ON files
BEGIN
 UPDATE subtitle_translation_operations SET
  operation_epoch=operation_epoch+1,view_revision=view_revision+1,
  state='failed',reason='source_changed',retryable=0
 WHERE file_id=OLD.id AND operation_epoch<9223372036854775807
 AND view_revision<9223372036854775807;
 UPDATE subtitle_translation_demands SET released=1
 WHERE operation_id IN (SELECT id FROM subtitle_translation_operations WHERE file_id=OLD.id);
 UPDATE background_jobs SET state=CASE WHEN state='queued' THEN 'cancelled' ELSE 'cancelling' END,
  revision=revision+1,last_error_code='source_changed'
 WHERE kind IN ('subtitle_translation_prepare','subtitle_translation_range')
  AND state IN ('queued','running') AND json_extract(payload_json,'$.file_id')=OLD.id
  AND revision<9223372036854775807;
END;
-- next statement
INSERT INTO subtitle_translation_schema VALUES(1,1);
-- next statement
CREATE TRIGGER subtitle_translation_command_limit BEFORE INSERT ON subtitle_translation_commands
WHEN (SELECT count(*) FROM subtitle_translation_commands)>=1024
BEGIN SELECT RAISE(ABORT,'translation command capacity'); END;
-- next statement
CREATE TRIGGER subtitle_translation_mutate AFTER INSERT ON subtitle_translation_commands
WHEN NEW.operation='mutate' AND json_extract(NEW.result_json,'$.outcome')='accepted'
AND NOT EXISTS(SELECT 1 FROM subtitle_translation_requests WHERE user_id=json_extract(NEW.request_json,'$.user_id') AND request_id=json_extract(NEW.request_json,'$.request_id'))
BEGIN
 INSERT INTO subtitle_translation_operations(id,recipe_key,file_id,source_json,profile_json,audio_index,spoken_language,duration_ms,target_node_id,operation_epoch,view_revision,state,stopped,preparation_job_id,created_at_ms,updated_at_ms)
 SELECT json_extract(NEW.result_json,'$.operation_id'),json_extract(NEW.request_json,'$.recipe_key'),json_extract(NEW.request_json,'$.action.source.file_id'),json(json_extract(NEW.request_json,'$.action.source')),json(json_extract(NEW.request_json,'$.action.profile')),json_extract(NEW.request_json,'$.action.audio_index'),json_extract(NEW.request_json,'$.action.spoken_language'),json_extract(NEW.request_json,'$.action.duration_ms'),json_extract(NEW.request_json,'$.action.target_node_id'),1,1,'checking_source',0,json_extract(NEW.request_json,'$.allocated_job_id'),json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms')
 WHERE json_extract(NEW.request_json,'$.action.action')='create' ON CONFLICT(recipe_key) DO NOTHING;
 UPDATE subtitle_translation_operations SET preparation_job_id=json_extract(NEW.request_json,'$.allocated_job_id'),state='checking_source',updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE id=json_extract(NEW.result_json,'$.operation_id') AND json_extract(NEW.request_json,'$.action.action')='create'
 AND (state='suspended' OR NOT EXISTS(SELECT 1 FROM background_jobs j WHERE j.id=subtitle_translation_operations.preparation_job_id AND j.state IN ('queued','running')));
 UPDATE subtitle_translation_operations SET operation_epoch=operation_epoch+1,view_revision=view_revision+1,stopped=1,state='stopped',reason=NULL,retryable=0,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE id=json_extract(NEW.result_json,'$.operation_id') AND json_extract(NEW.request_json,'$.action.action')='stop';
 UPDATE subtitle_translation_demands SET released=1,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE operation_id=json_extract(NEW.result_json,'$.operation_id') AND json_extract(NEW.request_json,'$.action.action') IN ('stop','retry_operation');
 UPDATE background_jobs SET state=CASE state WHEN 'queued' THEN 'cancelled' ELSE 'cancelling' END,revision=revision+1,last_error_code='translation_stopped',updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE kind IN ('subtitle_translation_prepare','subtitle_translation_range') AND state IN ('queued','running') AND revision<9223372036854775807
 AND COALESCE(json_extract(payload_json,'$.operation_id'),json_extract(payload_json,'$.recipe.operation_id'))=json_extract(NEW.result_json,'$.operation_id')
 AND json_extract(NEW.request_json,'$.action.action') IN ('stop','retry_operation');
 UPDATE subtitle_translation_operations SET operation_epoch=operation_epoch+1,view_revision=view_revision+1,stopped=0,state='checking_source',asset_id=NULL,preparation_job_id=json_extract(NEW.request_json,'$.allocated_job_id'),reason=NULL,retryable=0,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE id=json_extract(NEW.result_json,'$.operation_id') AND json_extract(NEW.request_json,'$.action.action')='retry_operation';
 UPDATE subtitle_translation_ranges SET range_revision=range_revision+1,state='queued',job_id=json_extract(NEW.request_json,'$.allocated_job_id'),reason=NULL,retryable=0,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE operation_id=json_extract(NEW.result_json,'$.operation_id') AND operation_epoch=json_extract(NEW.request_json,'$.action.expected_operation_epoch') AND core_index=json_extract(NEW.request_json,'$.action.core_index')
 AND json_extract(NEW.request_json,'$.action.action')='retry_range';
 INSERT INTO subtitle_translation_demands(id,operation_id,operation_epoch,user_id,playback_id,generation,position_ms,playback_rate,paused,expires_at_ms,created_at_ms,updated_at_ms)
 SELECT json_extract(NEW.request_json,'$.allocated_demand_id'),op.id,op.operation_epoch,json_extract(NEW.request_json,'$.user_id'),json_extract(NEW.request_json,'$.action.playback_id'),1,json_extract(NEW.request_json,'$.action.position_ms'),json_extract(NEW.request_json,'$.action.playback_rate'),json_extract(NEW.request_json,'$.action.paused'),json_extract(NEW.request_json,'$.now_ms')+15000,json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms')
 FROM subtitle_translation_operations op WHERE op.id=json_extract(NEW.result_json,'$.operation_id') AND json_extract(NEW.request_json,'$.action.action') IN ('create','retry_operation','retry_range')
 ON CONFLICT(operation_id,operation_epoch,user_id,playback_id) DO UPDATE SET generation=generation+1,position_ms=excluded.position_ms,playback_rate=excluded.playback_rate,paused=excluded.paused,released=0,expires_at_ms=excluded.expires_at_ms,updated_at_ms=excluded.updated_at_ms;
 UPDATE subtitle_translation_demands SET generation=generation+1,position_ms=CASE WHEN json_extract(NEW.request_json,'$.action.action')='demand' THEN json_extract(NEW.request_json,'$.action.position_ms') ELSE position_ms END,
 playback_rate=CASE WHEN json_extract(NEW.request_json,'$.action.action')='demand' THEN json_extract(NEW.request_json,'$.action.playback_rate') ELSE playback_rate END,
 paused=CASE WHEN json_extract(NEW.request_json,'$.action.action')='demand' THEN json_extract(NEW.request_json,'$.action.paused') ELSE paused END,
 released=CASE WHEN json_extract(NEW.request_json,'$.action.action')='release' THEN 1 ELSE 0 END,expires_at_ms=json_extract(NEW.request_json,'$.now_ms')+15000,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE id=json_extract(NEW.request_json,'$.action.demand_id') AND json_extract(NEW.request_json,'$.action.action') IN ('demand','release');
 UPDATE subtitle_translation_operations SET view_revision=view_revision+1,updated_at_ms=json_extract(NEW.request_json,'$.now_ms')
 WHERE id=json_extract(NEW.result_json,'$.operation_id') AND json_extract(NEW.request_json,'$.action.action') IN ('create','demand','release','retry_range');
 UPDATE subtitle_translation_commands SET result_json=(SELECT json_object('outcome','accepted','operation_id',op.id,'operation_epoch',CAST(op.operation_epoch AS TEXT),'view_revision',CAST(op.view_revision AS TEXT),'state',op.state,'asset_id',op.asset_id,'job_id',CASE WHEN json_extract(NEW.request_json,'$.action.action')='retry_range' THEN r.job_id WHEN op.state='checking_source' THEN op.preparation_job_id ELSE NULL END,'core_index',r.core_index,'range_revision',CASE WHEN r.range_revision IS NULL THEN NULL ELSE CAST(r.range_revision AS TEXT) END,
 'demand_id',d.id,'demand_generation',CASE WHEN d.generation IS NULL THEN NULL ELSE CAST(d.generation AS TEXT) END,'expires_at_ms',d.expires_at_ms,'intent_expires_at_ms',json_extract(NEW.request_json,'$.intent_expires_at_ms'),'server_now_ms',json_extract(NEW.request_json,'$.now_ms'))
 FROM subtitle_translation_operations op LEFT JOIN subtitle_translation_demands d ON d.operation_id=op.id AND d.operation_epoch=op.operation_epoch AND d.user_id=json_extract(NEW.request_json,'$.user_id') AND d.playback_id=json_extract(NEW.request_json,'$.action.playback_id')
 LEFT JOIN subtitle_translation_ranges r ON r.operation_id=op.id AND r.operation_epoch=op.operation_epoch AND r.core_index=json_extract(NEW.request_json,'$.action.core_index')
 WHERE op.id=json_extract(NEW.result_json,'$.operation_id')) WHERE id=NEW.id;
 INSERT INTO subtitle_translation_requests(user_id,request_id,body_digest,outcome_json,created_at_ms,expires_at_ms)
 SELECT json_extract(NEW.request_json,'$.user_id'),json_extract(NEW.request_json,'$.request_id'),json_extract(NEW.request_json,'$.body_digest'),result_json,json_extract(NEW.request_json,'$.now_ms'),
 CASE WHEN json_extract(NEW.request_json,'$.action.action') IN ('demand','release') THEN max(json_extract(NEW.request_json,'$.intent_expires_at_ms'),json_extract(NEW.request_json,'$.now_ms'))+60000 ELSE json_extract(NEW.request_json,'$.now_ms')+604800000 END
 FROM subtitle_translation_commands WHERE id=NEW.id;
END;
-- next statement
CREATE TRIGGER subtitle_translation_settings_saved AFTER INSERT ON subtitle_translation_commands
WHEN NEW.operation='settings' AND json_extract(NEW.result_json,'$.outcome')='saved'
AND NOT EXISTS(SELECT 1 FROM subtitle_translation_requests WHERE user_id=json_extract(NEW.request_json,'$.user_id') AND request_id=json_extract(NEW.request_json,'$.request_id'))
BEGIN
 INSERT INTO settings(key,value,updated_at) VALUES('subtitles.translation.config.v1',json(json_extract(NEW.request_json,'$.settings')),json_extract(NEW.request_json,'$.now_ms')/1000)
 ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at;
 INSERT INTO subtitle_translation_requests(user_id,request_id,body_digest,outcome_json,created_at_ms,expires_at_ms)
 VALUES(json_extract(NEW.request_json,'$.user_id'),json_extract(NEW.request_json,'$.request_id'),json_extract(NEW.request_json,'$.body_digest'),NEW.result_json,json_extract(NEW.request_json,'$.now_ms'),json_extract(NEW.request_json,'$.now_ms')+604800000);
END;
-- next statement
CREATE TRIGGER subtitle_translation_range_enqueued AFTER INSERT ON background_jobs
WHEN NEW.kind='subtitle_translation_range'
BEGIN
 INSERT INTO subtitle_translation_ranges(operation_id,operation_epoch,core_index,range_revision,state,job_id,attempts,updated_at_ms)
 VALUES(json_extract(NEW.payload_json,'$.recipe.operation_id'),json_extract(NEW.payload_json,'$.recipe.operation_epoch'),json_extract(NEW.payload_json,'$.recipe.core_index'),1,'queued',NEW.id,0,NEW.updated_at_ms)
 ON CONFLICT(operation_id,operation_epoch,core_index) DO UPDATE SET range_revision=subtitle_translation_ranges.range_revision+1,state='queued',job_id=excluded.job_id,reason=NULL,retryable=0,updated_at_ms=excluded.updated_at_ms WHERE subtitle_translation_ranges.state='cancelled' AND subtitle_translation_ranges.range_revision<9223372036854775807;
END;
-- next statement
CREATE TRIGGER subtitle_translation_job_projection AFTER UPDATE OF state ON background_jobs
WHEN NEW.kind IN ('subtitle_translation_prepare','subtitle_translation_range')
BEGIN
 UPDATE subtitle_translation_ranges SET state=CASE NEW.state WHEN 'cancelling' THEN 'running' ELSE NEW.state END,
 attempts=min(3,attempts+max(0,NEW.failed_attempts-OLD.failed_attempts)),
 range_revision=range_revision+1,reason=NEW.last_error_code,retryable=CASE WHEN NEW.state='failed' AND NEW.last_error_code IN ('worker_interrupted','inference_failed','inference_timeout','worker_source_unavailable') THEN 1 ELSE 0 END,updated_at_ms=NEW.updated_at_ms
 WHERE job_id=NEW.id AND range_revision<9223372036854775807;
 UPDATE subtitle_translation_operations SET state=CASE WHEN NEW.state IN ('cancelled','cancelling') AND stopped=0 THEN 'suspended' ELSE 'failed' END,
 reason=NEW.last_error_code,retryable=CASE WHEN NEW.last_error_code IN ('worker_interrupted','worker_source_unavailable','inference_failed','inference_timeout') THEN 1 ELSE 0 END,view_revision=view_revision+1,updated_at_ms=NEW.updated_at_ms
 WHERE preparation_job_id=NEW.id AND NEW.state IN ('cancelled','cancelling','failed') AND stopped=0
 AND operation_epoch=json_extract(NEW.payload_json,'$.operation_epoch') AND view_revision<9223372036854775807;
 UPDATE subtitle_translation_operations SET view_revision=view_revision+1,updated_at_ms=NEW.updated_at_ms
 WHERE id=json_extract(NEW.payload_json,'$.recipe.operation_id') AND operation_epoch=json_extract(NEW.payload_json,'$.recipe.operation_epoch') AND NEW.kind='subtitle_translation_range' AND NEW.state!=OLD.state AND view_revision<9223372036854775807;
END;
