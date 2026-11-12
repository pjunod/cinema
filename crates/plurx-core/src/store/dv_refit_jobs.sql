-- F+ curve fits (FEL-RPU-REFIT-BUILD.md §3.3–3.4): one title row per file and
-- its scenes, written only by a running `dv_refit` job's fenced publication.
-- An independently recognized extension: it creates only its own objects, and
-- an older binary that does not know it neither writes nor reads it. Named
-- under background_* because only a background job writes it.
CREATE TABLE IF NOT EXISTS background_dv_refit_schema (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    version INTEGER NOT NULL CHECK (version = 1)
) STRICT;
-- next statement
-- The published fit of one source. `claim_id` names the attempt whose staged
-- scenes it adopted. One row per file: a newer fit replaces the older one in
-- the publication transaction, so serving never sees a mixed set.
CREATE TABLE IF NOT EXISTS background_dv_refit_titles (
    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    claim_id TEXT NOT NULL UNIQUE CHECK (length(CAST(claim_id AS BLOB)) BETWEEN 1 AND 64),
    job_id TEXT NOT NULL CHECK (length(CAST(job_id AS BLOB)) BETWEEN 1 AND 64),
    source_size INTEGER NOT NULL CHECK (source_size >= 0),
    source_mtime INTEGER NOT NULL CHECK (source_mtime >= 0),
    probe_revision INTEGER NOT NULL CHECK (probe_revision > 0),
    fit_version TEXT NOT NULL CHECK (length(CAST(fit_version AS BLOB)) BETWEEN 1 AND 64),
    helper_digest TEXT NOT NULL CHECK (length(helper_digest) = 64 AND helper_digest NOT GLOB '*[^0-9a-f]*'),
    curve_digest TEXT NOT NULL CHECK (length(curve_digest) = 64 AND curve_digest NOT GLOB '*[^0-9a-f]*'),
    verdict TEXT NOT NULL CHECK (verdict IN ('fitted', 'identity', 'refused')),
    reason TEXT CHECK (reason IS NULL OR length(CAST(reason AS BLOB)) BETWEEN 1 AND 1024),
    gain REAL,
    scene_count INTEGER NOT NULL CHECK (scene_count >= 0),
    frames INTEGER NOT NULL CHECK (frames >= 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    CHECK ((verdict = 'fitted') = (reason IS NULL))
) STRICT;
-- next statement
-- Scenes in presentation order. Both statistics sets are kept, so a later
-- fit version can re-solve without decoding the title again. Rows whose
-- claim has no title row are an attempt's staging and are never served.
CREATE TABLE IF NOT EXISTS background_dv_refit_scenes (
    claim_id TEXT NOT NULL CHECK (length(CAST(claim_id AS BLOB)) BETWEEN 1 AND 64),
    scene_index INTEGER NOT NULL CHECK (scene_index >= 0),
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    first_frame INTEGER NOT NULL CHECK (first_frame >= 0),
    frames INTEGER NOT NULL CHECK (frames > 0),
    first_pts INTEGER NOT NULL,
    end_pts INTEGER NOT NULL,
    time_base TEXT NOT NULL CHECK (length(CAST(time_base AS BLOB)) BETWEEN 3 AND 41 AND time_base GLOB '[1-9]*/[1-9]*' AND time_base NOT GLOB '*[^0-9/]*'),
    fit_json TEXT NOT NULL CHECK (json_valid(fit_json)),
    held_out_json TEXT NOT NULL CHECK (json_valid(held_out_json)),
    curves_json TEXT CHECK (curves_json IS NULL OR json_valid(curves_json)),
    reason TEXT CHECK (reason IS NULL OR length(CAST(reason AS BLOB)) BETWEEN 1 AND 1024),
    verdict_json TEXT CHECK (verdict_json IS NULL OR json_valid(verdict_json)),
    PRIMARY KEY (claim_id, scene_index),
    CHECK (end_pts > first_pts),
    CHECK ((curves_json IS NULL) = (reason IS NOT NULL))
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS background_dv_refit_scenes_file ON background_dv_refit_scenes(file_id, claim_id);
-- next statement
-- An attempt's staged scenes go with the attempt: a cancel, a yield, a lost
-- lease or a failure leaves no partial rows. A published claim's scenes stay,
-- because the title row that adopts them is written before the job settles.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_attempt_end
AFTER UPDATE OF state, claim_id ON background_jobs
WHEN OLD.kind = 'dv_refit' AND OLD.claim_id IS NOT NULL
 AND (NEW.claim_id IS NOT OLD.claim_id OR NEW.state IS NOT 'running')
BEGIN
 DELETE FROM background_dv_refit_scenes WHERE claim_id = OLD.claim_id
  AND NOT EXISTS (SELECT 1 FROM background_dv_refit_titles t WHERE t.claim_id = OLD.claim_id);
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_dv_refit_job_delete
AFTER DELETE ON background_jobs
WHEN OLD.kind = 'dv_refit' AND OLD.claim_id IS NOT NULL
BEGIN
 DELETE FROM background_dv_refit_scenes WHERE claim_id = OLD.claim_id
  AND NOT EXISTS (SELECT 1 FROM background_dv_refit_titles t WHERE t.claim_id = OLD.claim_id);
END;
-- next statement
-- A fit describes one source's bits. A changed source keeps nothing.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_source_change
AFTER UPDATE OF path, size, mtime ON files
WHEN NEW.path IS NOT OLD.path OR NEW.size IS NOT OLD.size OR NEW.mtime IS NOT OLD.mtime
BEGIN
 DELETE FROM background_dv_refit_scenes WHERE file_id = NEW.id;
 DELETE FROM background_dv_refit_titles WHERE file_id = NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_dv_refit_file_delete
BEFORE DELETE ON files
BEGIN
 DELETE FROM background_dv_refit_scenes WHERE file_id = OLD.id;
 DELETE FROM background_dv_refit_titles WHERE file_id = OLD.id;
END;
-- next statement
-- One staging page from the running attempt. The command verdict was decided
-- against the attempt's token in the same statement that inserts it.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_stage
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'stage_dv_refit'
BEGIN
 INSERT OR REPLACE INTO background_dv_refit_scenes(claim_id, scene_index, file_id, first_frame,
  frames, first_pts, end_pts, time_base, fit_json, held_out_json, curves_json, reason, verdict_json)
 SELECT json_extract(NEW.request_json, '$.token.claim_id'),
  json_extract(NEW.request_json, '$.first_index') + CAST(scene.key AS INTEGER),
  json_extract(NEW.request_json, '$.file_id'),
  json_extract(scene.value, '$.first_frame'), json_extract(scene.value, '$.frames'),
  json_extract(scene.value, '$.first_pts'), json_extract(scene.value, '$.end_pts'),
  json_extract(scene.value, '$.time_base'),
  json_extract(scene.value, '$.fit_json'), json_extract(scene.value, '$.held_out_json'),
  json_extract(scene.value, '$.curves_json'), json_extract(scene.value, '$.reason'),
  json_extract(scene.value, '$.verdict_json')
 FROM json_each(NEW.request_json, '$.scenes') scene
 WHERE json_extract(NEW.result_json, '$.outcome') = 'staged';
 DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
-- Publication adopts the attempt's staged scenes, replaces the file's older
-- fit and settles the job, in one transaction.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_publish
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_dv_refit'
BEGIN
 DELETE FROM background_dv_refit_scenes
 WHERE file_id = json_extract(NEW.request_json, '$.title.file_id')
  AND claim_id IS NOT json_extract(NEW.request_json, '$.token.claim_id')
  AND json_extract(NEW.result_json, '$.outcome') = 'published';
 INSERT OR REPLACE INTO background_dv_refit_titles(file_id, claim_id, job_id, source_size,
  source_mtime, probe_revision, fit_version, helper_digest, curve_digest, verdict, reason, gain,
  scene_count, frames, created_at_ms)
 SELECT json_extract(NEW.request_json, '$.title.file_id'),
  json_extract(NEW.request_json, '$.token.claim_id'), json_extract(NEW.request_json, '$.token.job_id'),
  json_extract(NEW.request_json, '$.title.source_size'), json_extract(NEW.request_json, '$.title.source_mtime'),
  json_extract(NEW.request_json, '$.title.probe_revision'), json_extract(NEW.request_json, '$.title.fit_version'),
  json_extract(NEW.request_json, '$.title.helper_digest'), json_extract(NEW.request_json, '$.title.curve_digest'),
  json_extract(NEW.request_json, '$.title.verdict'), json_extract(NEW.request_json, '$.title.reason'),
  json_extract(NEW.request_json, '$.title.gain'), json_extract(NEW.request_json, '$.title.scene_count'),
  json_extract(NEW.request_json, '$.title.frames'), json_extract(NEW.request_json, '$.now_ms')
 WHERE json_extract(NEW.result_json, '$.outcome') = 'published';
 UPDATE background_jobs SET state = 'succeeded', result_ref = json_extract(NEW.result_json, '$.result_ref'),
  owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
  revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
 WHERE id = json_extract(NEW.result_json, '$.job_id')
  AND json_extract(NEW.result_json, '$.outcome') = 'published';
 UPDATE background_job_waiters SET state = 'succeeded', result_ref = 'dv-refit:' || job_id,
  updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
 WHERE job_id = json_extract(NEW.result_json, '$.job_id') AND state = 'pending'
  AND json_extract(NEW.result_json, '$.outcome') = 'published';
 DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
INSERT INTO background_dv_refit_schema VALUES (1, 1) ON CONFLICT (singleton) DO NOTHING;
