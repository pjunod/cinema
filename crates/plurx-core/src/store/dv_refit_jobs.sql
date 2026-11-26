-- F+ curve fits (FEL-RPU-REFIT-BUILD.md §3.3–3.5): a file's curve tables,
-- each named by its curve digest, and the terminal outcome of each fit
-- identity. Written only by a running `dv_refit` job's fenced publication
-- (one replicated write per title) and by the triggers below. Statistics are
-- never replicated: they stay in the node-local spool the job keeps. An
-- independently recognized extension: it creates only its own objects, and
-- an older binary that does not know it neither writes nor reads it. Named
-- under background_* because only a background job writes it.
CREATE TABLE IF NOT EXISTS background_dv_refit_schema (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    version INTEGER NOT NULL CHECK (version = 1)
) STRICT;
-- next statement
-- One curve table of a file. The current one has no supersession time; a
-- superseded one is kept seven days, so a session that pinned its digest
-- keeps the curves it started with on any node.
CREATE TABLE IF NOT EXISTS background_dv_refit_titles (
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    curve_digest TEXT NOT NULL CHECK (length(curve_digest) = 64 AND curve_digest NOT GLOB '*[^0-9a-f]*'),
    job_id TEXT NOT NULL CHECK (length(CAST(job_id AS BLOB)) BETWEEN 1 AND 64),
    source_size INTEGER NOT NULL CHECK (source_size >= 0),
    source_mtime INTEGER NOT NULL CHECK (source_mtime >= 0),
    video_index INTEGER NOT NULL CHECK (video_index BETWEEN 0 AND 255),
    probe_revision INTEGER NOT NULL CHECK (probe_revision > 0),
    helper_identity TEXT NOT NULL CHECK (length(helper_identity) = 64 AND helper_identity NOT GLOB '*[^0-9a-f]*'),
    fit_version TEXT NOT NULL CHECK (length(CAST(fit_version AS BLOB)) BETWEEN 1 AND 64),
    verdict TEXT NOT NULL CHECK (verdict IN ('fitted', 'identity', 'refused')),
    reason TEXT CHECK (reason IS NULL OR length(CAST(reason AS BLOB)) BETWEEN 1 AND 1024),
    gain TEXT CHECK (gain IS NULL OR length(CAST(gain AS BLOB)) BETWEEN 1 AND 64),
    scene_count INTEGER NOT NULL CHECK (scene_count >= 0),
    frames INTEGER NOT NULL CHECK (frames >= 0),
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    superseded_at_ms INTEGER CHECK (superseded_at_ms IS NULL OR superseded_at_ms >= 0),
    PRIMARY KEY (file_id, curve_digest),
    CHECK ((verdict = 'fitted') = (reason IS NULL))
) STRICT;
-- next statement
CREATE UNIQUE INDEX IF NOT EXISTS background_dv_refit_titles_current
    ON background_dv_refit_titles(file_id) WHERE superseded_at_ms IS NULL;
-- next statement
-- A table's scenes in presentation order: where each presents, the served
-- coefficients as the numerators over 2^23 that apply_to_rpu writes (NULL:
-- identity), why a scene keeps identity, and its held-out error numbers as
-- the decimal text Rust writes and parses back exactly.
CREATE TABLE IF NOT EXISTS background_dv_refit_scenes (
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    curve_digest TEXT NOT NULL,
    scene_index INTEGER NOT NULL CHECK (scene_index >= 0),
    first_frame INTEGER NOT NULL CHECK (first_frame >= 0),
    frames INTEGER NOT NULL CHECK (frames > 0),
    first_pts INTEGER NOT NULL,
    end_pts INTEGER NOT NULL,
    time_base TEXT NOT NULL CHECK (length(CAST(time_base AS BLOB)) BETWEEN 3 AND 41 AND time_base GLOB '[1-9]*/[1-9]*' AND time_base NOT GLOB '*[^0-9/]*'),
    coefficients TEXT CHECK (coefficients IS NULL OR (json_valid(coefficients) AND length(CAST(coefficients AS BLOB)) <= 4096)),
    reason TEXT CHECK (reason IS NULL OR length(CAST(reason AS BLOB)) BETWEEN 1 AND 1024),
    identity_error TEXT NOT NULL CHECK (length(CAST(identity_error AS BLOB)) BETWEEN 1 AND 64),
    fitted_error TEXT CHECK (fitted_error IS NULL OR length(CAST(fitted_error AS BLOB)) BETWEEN 1 AND 64),
    gain TEXT CHECK (gain IS NULL OR length(CAST(gain AS BLOB)) BETWEEN 1 AND 64),
    PRIMARY KEY (file_id, curve_digest, scene_index),
    CHECK (end_pts > first_pts),
    CHECK (coefficients IS NULL OR reason IS NULL)
) STRICT;
-- next statement
-- The terminal outcome of one fit identity (its job's dedupe key): a failure
-- after its retries, or a cancellation. It outlives the job row, so pruning
-- job history does not demand the title again; an administrator's retry (a
-- new job for the same identity) clears it.
CREATE TABLE IF NOT EXISTS background_dv_refit_outcomes (
    identity TEXT PRIMARY KEY CHECK (length(CAST(identity AS BLOB)) BETWEEN 1 AND 128),
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    source_size INTEGER NOT NULL,
    source_mtime INTEGER NOT NULL,
    video_index INTEGER NOT NULL,
    probe_revision INTEGER NOT NULL,
    helper_identity TEXT NOT NULL,
    fit_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('failed', 'cancelled')),
    error_code TEXT,
    job_id TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
-- next statement
CREATE INDEX IF NOT EXISTS background_dv_refit_outcomes_file ON background_dv_refit_outcomes(file_id);
-- next statement
CREATE TRIGGER IF NOT EXISTS background_dv_refit_terminal
AFTER UPDATE OF state ON background_jobs
WHEN NEW.kind = 'dv_refit' AND NEW.state IN ('cancelling', 'cancelled', 'failed')
 AND OLD.state IS NOT NEW.state AND json_valid(NEW.payload_json)
BEGIN
 INSERT INTO background_dv_refit_outcomes(identity, file_id, source_size, source_mtime, video_index,
  probe_revision, helper_identity, fit_version, state, error_code, job_id, updated_at_ms)
 SELECT NEW.dedupe_key, json_extract(NEW.payload_json, '$.file_id'),
  json_extract(NEW.payload_json, '$.source_size'), json_extract(NEW.payload_json, '$.source_mtime'),
  json_extract(NEW.payload_json, '$.video_index'), json_extract(NEW.payload_json, '$.probe_revision'),
  json_extract(NEW.payload_json, '$.helper_identity'), json_extract(NEW.payload_json, '$.fit_version'),
  CASE WHEN NEW.state = 'failed' THEN 'failed' ELSE 'cancelled' END, NEW.last_error_code, NEW.id,
  COALESCE(NEW.updated_at_ms, 0)
 WHERE EXISTS (SELECT 1 FROM files f WHERE f.id = json_extract(NEW.payload_json, '$.file_id'))
 ON CONFLICT (identity) DO UPDATE SET state = excluded.state, error_code = excluded.error_code,
  job_id = excluded.job_id, updated_at_ms = excluded.updated_at_ms;
END;
-- next statement
-- A new job for an identity (an administrator's retry) lifts its outcome.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_requeued
AFTER INSERT ON background_jobs WHEN NEW.kind = 'dv_refit'
BEGIN
 DELETE FROM background_dv_refit_outcomes WHERE identity = NEW.dedupe_key;
END;
-- next statement
-- A fit describes one source's bits. A changed source supersedes its jobs
-- (their tokens stop publishing) and keeps none of its tables or outcomes.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_source_change
AFTER UPDATE OF size, mtime ON files
WHEN NEW.size IS NOT OLD.size OR NEW.mtime IS NOT OLD.mtime
BEGIN
 UPDATE background_jobs SET state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
  last_error_code = 'source_changed', revision = revision + 1
 WHERE kind = 'dv_refit' AND state IN ('queued', 'running')
  AND json_extract(payload_json, '$.file_id') = OLD.id AND revision < 9223372036854775807;
 DELETE FROM background_dv_refit_scenes WHERE file_id = NEW.id;
 DELETE FROM background_dv_refit_titles WHERE file_id = NEW.id;
 DELETE FROM background_dv_refit_outcomes WHERE file_id = NEW.id;
END;
-- next statement
CREATE TRIGGER IF NOT EXISTS background_dv_refit_file_delete
BEFORE DELETE ON files
BEGIN
 UPDATE background_jobs SET state = CASE WHEN state = 'queued' THEN 'cancelled' ELSE 'cancelling' END,
  last_error_code = 'source_changed', revision = revision + 1
 WHERE kind = 'dv_refit' AND state IN ('queued', 'running')
  AND json_extract(payload_json, '$.file_id') = OLD.id AND revision < 9223372036854775807;
 DELETE FROM background_dv_refit_scenes WHERE file_id = OLD.id;
 DELETE FROM background_dv_refit_titles WHERE file_id = OLD.id;
 DELETE FROM background_dv_refit_outcomes WHERE file_id = OLD.id;
END;
-- next statement
-- Publication, in one transaction: the file's current table becomes
-- superseded, the new one (with all its scenes) becomes current, tables
-- superseded more than seven days ago go, and the job and its waiters
-- settle. Republishing a digest the file already holds replaces that table.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_publish
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_dv_refit'
 AND json_extract(NEW.result_json, '$.outcome') = 'published'
BEGIN
 UPDATE background_dv_refit_titles SET superseded_at_ms = json_extract(NEW.request_json, '$.now_ms')
 WHERE file_id = json_extract(NEW.request_json, '$.title.file_id') AND superseded_at_ms IS NULL
  AND curve_digest IS NOT json_extract(NEW.request_json, '$.title.curve_digest');
 DELETE FROM background_dv_refit_scenes WHERE file_id = json_extract(NEW.request_json, '$.title.file_id')
  AND curve_digest = json_extract(NEW.request_json, '$.title.curve_digest');
 DELETE FROM background_dv_refit_titles WHERE file_id = json_extract(NEW.request_json, '$.title.file_id')
  AND curve_digest = json_extract(NEW.request_json, '$.title.curve_digest');
 INSERT INTO background_dv_refit_titles(file_id, curve_digest, job_id, source_size, source_mtime,
  video_index, probe_revision, helper_identity, fit_version, verdict, reason, gain, scene_count, frames,
  created_at_ms, superseded_at_ms)
 SELECT json_extract(NEW.request_json, '$.title.file_id'), json_extract(NEW.request_json, '$.title.curve_digest'),
  json_extract(NEW.request_json, '$.token.job_id'),
  json_extract(NEW.request_json, '$.title.source_size'), json_extract(NEW.request_json, '$.title.source_mtime'),
  json_extract(NEW.request_json, '$.title.video_index'), json_extract(NEW.request_json, '$.title.probe_revision'),
  json_extract(NEW.request_json, '$.title.helper_identity'), json_extract(NEW.request_json, '$.title.fit_version'),
  json_extract(NEW.request_json, '$.title.verdict'), json_extract(NEW.request_json, '$.title.reason'),
  json_extract(NEW.request_json, '$.title.gain'), json_extract(NEW.request_json, '$.title.scene_count'),
  json_extract(NEW.request_json, '$.title.frames'), json_extract(NEW.request_json, '$.now_ms'), NULL;
 INSERT INTO background_dv_refit_scenes(file_id, curve_digest, scene_index, first_frame, frames, first_pts,
  end_pts, time_base, coefficients, reason, identity_error, fitted_error, gain)
 SELECT json_extract(NEW.request_json, '$.title.file_id'), json_extract(NEW.request_json, '$.title.curve_digest'),
  CAST(scene.key AS INTEGER), json_extract(scene.value, '$.first_frame'), json_extract(scene.value, '$.frames'),
  json_extract(scene.value, '$.first_pts'), json_extract(scene.value, '$.end_pts'),
  json_extract(scene.value, '$.time_base'),
  CASE WHEN json_type(scene.value, '$.coefficients') = 'object' THEN json_extract(scene.value, '$.coefficients') END,
  json_extract(scene.value, '$.reason'), json_extract(scene.value, '$.identity_error'),
  json_extract(scene.value, '$.fitted_error'), json_extract(scene.value, '$.gain')
 FROM json_each(NEW.request_json, '$.scenes') scene;
 DELETE FROM background_dv_refit_scenes WHERE EXISTS (SELECT 1 FROM background_dv_refit_titles t
  WHERE t.file_id = background_dv_refit_scenes.file_id AND t.curve_digest = background_dv_refit_scenes.curve_digest
   AND t.superseded_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 604800000);
 DELETE FROM background_dv_refit_titles
 WHERE superseded_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 604800000;
 UPDATE background_jobs SET state = 'succeeded', result_ref = json_extract(NEW.result_json, '$.result_ref'),
  owner_node_id = NULL, owner_boot_id = NULL, claim_id = NULL, lease_expires_ms = NULL,
  revision = revision + 1, updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
 WHERE id = json_extract(NEW.result_json, '$.job_id');
 UPDATE background_job_waiters SET state = 'succeeded', result_ref = json_extract(NEW.result_json, '$.result_ref'),
  updated_at_ms = json_extract(NEW.request_json, '$.now_ms')
 WHERE job_id = json_extract(NEW.result_json, '$.job_id') AND state = 'pending';
 DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
-- A refused or repeated publication leaves nothing behind.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_publish_refused
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'publish_dv_refit'
 AND json_extract(NEW.result_json, '$.outcome') IS NOT 'published'
BEGIN
 DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
-- Retention without a publication: tables superseded more than seven days
-- before the caller's clock go, and outcomes of sources the file no longer
-- has. The command row never outlives its statement.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_prune
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'prune_dv_refit'
BEGIN
 DELETE FROM background_dv_refit_scenes WHERE EXISTS (SELECT 1 FROM background_dv_refit_titles t
  WHERE t.file_id = background_dv_refit_scenes.file_id AND t.curve_digest = background_dv_refit_scenes.curve_digest
   AND t.superseded_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 604800000);
 DELETE FROM background_dv_refit_titles
 WHERE superseded_at_ms <= json_extract(NEW.request_json, '$.now_ms') - 604800000;
 DELETE FROM background_dv_refit_outcomes WHERE NOT EXISTS (SELECT 1 FROM files f
  WHERE f.id = background_dv_refit_outcomes.file_id AND f.size = background_dv_refit_outcomes.source_size
   AND f.mtime = background_dv_refit_outcomes.source_mtime);
 DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
-- The title-scan generation a stored table or outcome was read under moves
-- with every probe; when the selected video stream and the source did not
-- change, the table still describes the title and only its generation moves.
CREATE TRIGGER IF NOT EXISTS background_dv_refit_rebind
AFTER INSERT ON background_job_commands WHEN NEW.operation = 'rebind_dv_refit'
BEGIN
 UPDATE background_dv_refit_titles SET probe_revision = json_extract(NEW.request_json, '$.probe_revision')
 WHERE file_id = json_extract(NEW.request_json, '$.file_id') AND superseded_at_ms IS NULL
  AND source_size = json_extract(NEW.request_json, '$.source_size')
  AND source_mtime = json_extract(NEW.request_json, '$.source_mtime')
  AND video_index = json_extract(NEW.request_json, '$.video_index')
  AND helper_identity = json_extract(NEW.request_json, '$.helper_identity')
  AND fit_version = json_extract(NEW.request_json, '$.fit_version')
  AND EXISTS (SELECT 1 FROM files f WHERE f.id = json_extract(NEW.request_json, '$.file_id')
   AND f.size = json_extract(NEW.request_json, '$.source_size')
   AND f.mtime = json_extract(NEW.request_json, '$.source_mtime'));
 UPDATE background_dv_refit_outcomes SET probe_revision = json_extract(NEW.request_json, '$.probe_revision')
 WHERE identity = json_extract(NEW.request_json, '$.identity')
  AND file_id = json_extract(NEW.request_json, '$.file_id')
  AND source_size = json_extract(NEW.request_json, '$.source_size')
  AND source_mtime = json_extract(NEW.request_json, '$.source_mtime')
  AND video_index = json_extract(NEW.request_json, '$.video_index');
 DELETE FROM background_job_commands WHERE id = NEW.id;
END;
-- next statement
INSERT INTO background_dv_refit_schema VALUES (1, 1) ON CONFLICT (singleton) DO NOTHING;
