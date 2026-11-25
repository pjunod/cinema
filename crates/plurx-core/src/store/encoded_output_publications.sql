CREATE TABLE encoded_output_publications (
    claim_id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL,
    request_json TEXT NOT NULL,
    result_json TEXT NOT NULL
)
-- next statement
CREATE INDEX encoded_output_publications_job ON encoded_output_publications(job_id)
-- next statement
CREATE TRIGGER encoded_output_publications_record
AFTER INSERT ON background_job_commands
WHEN NEW.operation='publish_copy_output'
    AND json_extract(NEW.result_json,'$.outcome') IN ('published','already_published')
    AND EXISTS(SELECT 1 FROM background_jobs job
        WHERE job.id=json_extract(NEW.result_json,'$.job_id') AND job.kind='encoded_output_prepare')
BEGIN
    INSERT OR IGNORE INTO encoded_output_publications(claim_id,job_id,request_json,result_json)
    VALUES(NEW.id,json_extract(NEW.result_json,'$.job_id'),NEW.request_json,NEW.result_json);
END
-- next statement
CREATE TRIGGER encoded_output_publications_retire
AFTER DELETE ON background_jobs WHEN OLD.kind='encoded_output_prepare'
BEGIN
    DELETE FROM encoded_output_publications WHERE job_id=OLD.id;
END
