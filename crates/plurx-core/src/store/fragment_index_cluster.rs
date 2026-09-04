//! Cluster coordination for content-addressed VOD fragment indexes.
//!
//! Only small ownership and queue facts belong in the replicated store.  The
//! encoded index itself is deliberately returned to the daemon as an opaque,
//! checksummed blob and lives in a node-local or explicitly shared cache.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::fmp4::{CutClass, PromotionInputs};
use crate::segplan::{FragmentIndex, IndexRow, SourceIdentity, SEGPLAN_VERSION};

pub const CLUSTER_FRAGMENT_INDEX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cluster_fragment_index_sources (
    node_id          TEXT NOT NULL,
    file_id          INTEGER NOT NULL,
    object_version   TEXT NOT NULL,
    source_size      INTEGER NOT NULL,
    source_mtime     INTEGER NOT NULL,
    source_sha256    TEXT NOT NULL,
    observed_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (node_id, file_id)
) STRICT;

CREATE TABLE IF NOT EXISTS cluster_fragment_index_jobs (
    cache_key         TEXT PRIMARY KEY,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    source_sha256     TEXT NOT NULL,
    pipeline_sha256   TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
    owner_node_id     TEXT,
    fence             INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms  INTEGER,
    attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms     INTEGER NOT NULL,
    last_error_code   TEXT,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_jobs_due
    ON cluster_fragment_index_jobs(state, not_before_ms, created_at_ms, cache_key);

CREATE TABLE IF NOT EXISTS cluster_fragment_index_artifacts (
    cache_key         TEXT PRIMARY KEY,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    source_sha256     TEXT NOT NULL,
    pipeline_sha256   TEXT NOT NULL,
    blob_sha256       TEXT NOT NULL,
    bytes             INTEGER NOT NULL CHECK (bytes > 0),
    built_by_node_id  TEXT NOT NULL,
    built_at_ms       INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_artifacts_file
    ON cluster_fragment_index_artifacts(file_id, source_size, source_mtime, pipeline_sha256);

CREATE TABLE IF NOT EXISTS cluster_fragment_index_locations (
    cache_key         TEXT NOT NULL,
    node_id           TEXT NOT NULL,
    bytes             INTEGER NOT NULL CHECK (bytes > 0),
    verified_at_ms    INTEGER NOT NULL,
    last_seen_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (cache_key, node_id)
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_fragment_index_locations_node
    ON cluster_fragment_index_locations(node_id, last_seen_at_ms, cache_key);

CREATE TRIGGER IF NOT EXISTS cluster_fragment_indexes_cancel_source BEFORE DELETE ON files
BEGIN
    DELETE FROM cluster_fragment_index_sources WHERE file_id = OLD.id;
    UPDATE cluster_fragment_index_jobs
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           last_error_code = 'source_deleted'
     WHERE file_id = OLD.id AND state IN ('queued', 'running');
END;
"#;

/// v31/v13 schema for durable operator requests before content addressing.
/// Kept separate from [`CLUSTER_FRAGMENT_INDEX_SCHEMA`] so migration fixtures
/// and deployed v30/v12 databases retain their historical shape.
pub const ANALYSIS_REQUESTS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS analysis_requests (
    request_id         TEXT PRIMARY KEY,
    file_id            INTEGER NOT NULL,
    source_size        INTEGER NOT NULL,
    source_mtime       INTEGER NOT NULL,
    component          TEXT NOT NULL CHECK (component IN ('fragment_index')),
    force_rebuild      INTEGER NOT NULL CHECK (force_rebuild IN (0, 1)),
    target_node_id     TEXT NOT NULL,
    state              TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'submitted', 'ready', 'failed', 'cancelled')),
    owner_node_id      TEXT,
    fence              INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms   INTEGER,
    attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms      INTEGER NOT NULL,
    result_cache_key   TEXT,
    last_error_code    TEXT,
    created_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS analysis_requests_due
    ON analysis_requests(target_node_id, state, not_before_ms, created_at_ms, request_id);
CREATE INDEX IF NOT EXISTS analysis_requests_status
    ON analysis_requests(state, updated_at_ms DESC, request_id);
CREATE UNIQUE INDEX IF NOT EXISTS analysis_requests_one_active_source
    ON analysis_requests(file_id, source_size, source_mtime, component, target_node_id)
    WHERE state IN ('queued', 'running', 'submitted');

CREATE TRIGGER IF NOT EXISTS analysis_requests_cancel_source BEFORE DELETE ON files
BEGIN
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           last_error_code = 'source_deleted'
     WHERE file_id = OLD.id AND state IN ('queued', 'running', 'submitted');
END;
CREATE TRIGGER IF NOT EXISTS analysis_requests_supersede_source
AFTER UPDATE OF size, mtime ON files
WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime
BEGIN
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           last_error_code = 'source_superseded'
     WHERE file_id = NEW.id AND state IN ('queued', 'running', 'submitted')
       AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
END;
CREATE TRIGGER IF NOT EXISTS analysis_requests_bound_terminal_history
AFTER UPDATE OF state ON analysis_requests
WHEN NEW.state IN ('ready', 'failed', 'cancelled')
BEGIN
    DELETE FROM analysis_requests
     WHERE request_id IN (
       SELECT request_id FROM analysis_requests
        WHERE state IN ('ready', 'failed', 'cancelled')
          AND request_id <> NEW.request_id
        ORDER BY updated_at_ms, request_id
        LIMIT MAX((SELECT COUNT(*) FROM analysis_requests
                    WHERE state IN ('ready', 'failed', 'cancelled')) - 8192, 0));
END;
"#;

/// v33/v15 query indexes for the paginated operator history and its compact
/// polling projection. This is a separate migration from the v31/v13 table so
/// deployed databases receive it instead of only fresh installations.
pub const ANALYSIS_HISTORY_INDEX_SCHEMA: &str = r#"
CREATE INDEX IF NOT EXISTS analysis_requests_result_history
    ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
    WHERE result_cache_key IS NOT NULL AND result_cache_key <> '';
CREATE INDEX IF NOT EXISTS cluster_fragment_index_jobs_status_history
    ON cluster_fragment_index_jobs(state, updated_at_ms DESC, cache_key);
"#;

/// v41/v22 widens the already-durable request identity to the replicated
/// semantic component. A table rebuild is required because SQLite cannot
/// alter a CHECK constraint in place.
pub const ANALYSIS_COMPONENTS_SCHEMA: &str = r#"
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal'
      CHECK (priority IN ('normal','forced','foreground'));
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN trigger TEXT NOT NULL DEFAULT 'background'
      CHECK (trigger IN ('admin','background','foreground'));
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN target_node_id TEXT NOT NULL DEFAULT '';
DROP TRIGGER IF EXISTS cluster_fragment_indexes_cancel_source;
DROP INDEX IF EXISTS cluster_fragment_index_jobs_status_history;
DROP INDEX IF EXISTS cluster_fragment_index_jobs_due;
ALTER TABLE cluster_fragment_index_jobs RENAME TO cluster_fragment_index_jobs_v40;
CREATE TABLE cluster_fragment_index_jobs (
    cache_key         TEXT NOT NULL,
    file_id           INTEGER NOT NULL,
    source_size       INTEGER NOT NULL,
    source_mtime      INTEGER NOT NULL,
    source_sha256     TEXT NOT NULL,
    pipeline_sha256   TEXT NOT NULL,
    priority          TEXT NOT NULL DEFAULT 'normal'
      CHECK (priority IN ('normal','forced','foreground')),
    trigger           TEXT NOT NULL DEFAULT 'background'
      CHECK (trigger IN ('admin','background','foreground')),
    target_node_id    TEXT NOT NULL DEFAULT '',
    state             TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'ready', 'failed', 'cancelled')),
    owner_node_id     TEXT,
    fence             INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms  INTEGER,
    attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms     INTEGER NOT NULL,
    last_error_code   TEXT,
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    PRIMARY KEY (cache_key, target_node_id)
) STRICT;
INSERT INTO cluster_fragment_index_jobs
    (cache_key, file_id, source_size, source_mtime, source_sha256,
     pipeline_sha256, priority, trigger, target_node_id, state,
     owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
     last_error_code, created_at_ms, updated_at_ms)
SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
       pipeline_sha256, priority, trigger,
       COALESCE((SELECT request.target_node_id FROM analysis_requests request
                  WHERE request.result_cache_key = cluster_fragment_index_jobs_v40.cache_key
                  ORDER BY request.updated_at_ms DESC, request.request_id DESC LIMIT 1), target_node_id), state,
       owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
       last_error_code, created_at_ms, updated_at_ms
  FROM cluster_fragment_index_jobs_v40;
DROP TABLE cluster_fragment_index_jobs_v40;
CREATE INDEX cluster_fragment_index_jobs_due
    ON cluster_fragment_index_jobs(state, not_before_ms, created_at_ms, cache_key, target_node_id);
CREATE INDEX cluster_fragment_index_jobs_status_history
    ON cluster_fragment_index_jobs(state, updated_at_ms DESC, cache_key, target_node_id);
CREATE TRIGGER cluster_fragment_indexes_cancel_source BEFORE DELETE ON files
BEGIN
    DELETE FROM cluster_fragment_index_sources WHERE file_id = OLD.id;
    UPDATE cluster_fragment_index_jobs
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, last_error_code = 'source_deleted',
           updated_at_ms = MAX(updated_at_ms, OLD.scanned_at * 1000)
     WHERE file_id = OLD.id AND state IN ('queued', 'running');
END;
ALTER TABLE timeline_annotation_sets
    ADD COLUMN publication_priority TEXT NOT NULL DEFAULT 'normal'
      CHECK (publication_priority IN ('normal','forced'));
DROP TABLE IF EXISTS analysis_attempts;
DROP TABLE IF EXISTS cluster_fragment_index_heads;
DROP TRIGGER IF EXISTS analysis_requests_bound_terminal_history;
DROP TRIGGER IF EXISTS analysis_requests_supersede_source;
DROP TRIGGER IF EXISTS analysis_requests_cancel_source;
DROP INDEX IF EXISTS analysis_requests_result_history;
DROP INDEX IF EXISTS analysis_requests_one_active_forced_successor;
DROP INDEX IF EXISTS analysis_requests_one_active_source;
DROP INDEX IF EXISTS analysis_requests_status;
DROP INDEX IF EXISTS analysis_requests_due;
ALTER TABLE analysis_requests RENAME TO analysis_requests_v40;
CREATE TABLE analysis_requests (
    request_id         TEXT PRIMARY KEY,
    file_id            INTEGER NOT NULL,
    source_size        INTEGER NOT NULL,
    source_mtime       INTEGER NOT NULL,
    component          TEXT NOT NULL CHECK (component IN ('fragment_index','skip_markers')),
    pipeline_version   TEXT NOT NULL DEFAULT 'legacy-fragment-index',
    requested_generation TEXT NOT NULL DEFAULT 'legacy-generation',
    expected_predecessor_generation TEXT NOT NULL DEFAULT '',
    priority           TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('normal','forced')),
    trigger            TEXT NOT NULL DEFAULT 'admin' CHECK (trigger IN ('admin','background')),
    force_rebuild      INTEGER NOT NULL CHECK (force_rebuild IN (0, 1)),
    target_node_id     TEXT NOT NULL,
    state              TEXT NOT NULL CHECK (
        state IN ('queued', 'running', 'submitted', 'ready', 'failed', 'cancelled')),
    owner_node_id      TEXT,
    fence              INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_ms   INTEGER,
    attempts           INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_ms      INTEGER NOT NULL,
    result_cache_key   TEXT,
    last_error_code    TEXT,
    cancel_requested   INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    created_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL
) STRICT;
INSERT INTO analysis_requests
    (request_id, file_id, source_size, source_mtime, component,
     pipeline_version, requested_generation, expected_predecessor_generation, priority, trigger,
     force_rebuild, target_node_id, state, owner_node_id, fence,
     lease_expires_ms, attempts, not_before_ms, result_cache_key,
     last_error_code, cancel_requested, created_at_ms, updated_at_ms)
SELECT request_id, file_id, source_size, source_mtime, component,
       CASE component WHEN 'fragment_index' THEN 'legacy-fragment-index'
                      ELSE 'chapter-classifier-v1' END,
       request_id, '', CASE force_rebuild WHEN 1 THEN 'forced' ELSE 'normal' END,
       'admin', force_rebuild, target_node_id, state, owner_node_id, fence,
       lease_expires_ms, attempts, not_before_ms, result_cache_key,
       last_error_code, 0, created_at_ms, updated_at_ms
  FROM analysis_requests_v40;
DROP TABLE analysis_requests_v40;
CREATE INDEX analysis_requests_due
    ON analysis_requests(target_node_id, state, not_before_ms, created_at_ms, request_id);
CREATE INDEX analysis_requests_status
    ON analysis_requests(state, updated_at_ms DESC, request_id);
CREATE UNIQUE INDEX analysis_requests_one_active_source
    ON analysis_requests(file_id, source_size, source_mtime, component,
                         pipeline_version, requested_generation, target_node_id)
    WHERE state IN ('queued', 'running', 'submitted');
CREATE UNIQUE INDEX analysis_requests_one_active_forced_successor
    ON analysis_requests(file_id, source_size, source_mtime, component)
    WHERE force_rebuild = 1 AND state IN ('queued', 'running', 'submitted');
CREATE INDEX analysis_requests_result_history
    ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
    WHERE result_cache_key IS NOT NULL AND result_cache_key <> '';
CREATE TRIGGER analysis_requests_cancel_source BEFORE DELETE ON files
BEGIN
    DELETE FROM cluster_fragment_index_heads
     WHERE generation_cache_key IN (
       SELECT cache_key FROM cluster_fragment_index_jobs WHERE file_id = OLD.id);
    UPDATE analysis_attempts
       SET phase = 'canceled', phase_updated_at_ms = MAX(phase_updated_at_ms, OLD.scanned_at * 1000),
           terminal_code = 'source_deleted'
     WHERE (request_id, claim_epoch) IN (
       SELECT request_id, fence FROM analysis_requests
        WHERE file_id = OLD.id AND state IN ('running', 'submitted'));
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, cancel_requested = 1,
           last_error_code = 'source_deleted', updated_at_ms = MAX(updated_at_ms, OLD.scanned_at * 1000)
     WHERE file_id = OLD.id AND state IN ('queued', 'running', 'submitted');
END;
CREATE TRIGGER analysis_requests_supersede_source
AFTER UPDATE OF size, mtime ON files
WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime
BEGIN
    DELETE FROM cluster_fragment_index_heads
     WHERE generation_cache_key IN (
       SELECT cache_key FROM cluster_fragment_index_jobs
        WHERE file_id = NEW.id
          AND (source_size <> NEW.size OR source_mtime <> NEW.mtime));
    UPDATE cluster_fragment_index_jobs
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, last_error_code = 'source_superseded',
           updated_at_ms = MAX(updated_at_ms, NEW.scanned_at * 1000)
     WHERE file_id = NEW.id AND state IN ('queued', 'running')
       AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
    UPDATE analysis_attempts
       SET phase = 'stale', phase_updated_at_ms = MAX(phase_updated_at_ms, NEW.scanned_at * 1000),
           terminal_code = 'source_superseded'
     WHERE (request_id, claim_epoch) IN (
       SELECT request_id, fence FROM analysis_requests
        WHERE file_id = NEW.id AND state IN ('running', 'submitted')
          AND (source_size <> NEW.size OR source_mtime <> NEW.mtime));
    UPDATE analysis_requests
       SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
           fence = fence + 1, cancel_requested = 1,
           last_error_code = 'source_superseded', updated_at_ms = MAX(updated_at_ms, NEW.scanned_at * 1000)
     WHERE file_id = NEW.id AND state IN ('queued', 'running', 'submitted')
       AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
END;
CREATE TRIGGER analysis_requests_bound_terminal_history
AFTER UPDATE OF state ON analysis_requests
WHEN NEW.state IN ('ready', 'failed', 'cancelled')
BEGIN
    DELETE FROM analysis_requests
     WHERE request_id IN (
       SELECT candidate.request_id FROM analysis_requests candidate
        WHERE candidate.state IN ('ready', 'failed', 'cancelled')
          AND candidate.request_id <> NEW.request_id
          AND (candidate.force_rebuild = 1
            OR NOT EXISTS (SELECT 1 FROM files
                 WHERE files.id = candidate.file_id
                   AND files.size = candidate.source_size
                   AND files.mtime = candidate.source_mtime)
            OR EXISTS (SELECT 1 FROM analysis_requests newer
                 WHERE newer.state IN ('ready', 'failed', 'cancelled')
                   AND newer.force_rebuild = 0
                   AND newer.file_id = candidate.file_id
                   AND newer.source_size = candidate.source_size
                   AND newer.source_mtime = candidate.source_mtime
                   AND newer.component = candidate.component
                   AND newer.pipeline_version = candidate.pipeline_version
                   AND newer.requested_generation = candidate.requested_generation
                   AND newer.target_node_id = candidate.target_node_id
                   AND (newer.updated_at_ms > candidate.updated_at_ms
                     OR (newer.updated_at_ms = candidate.updated_at_ms
                       AND newer.request_id > candidate.request_id))))
        ORDER BY candidate.updated_at_ms, candidate.request_id
        LIMIT MAX((SELECT COUNT(*) FROM analysis_requests
                    WHERE state IN ('ready', 'failed', 'cancelled')) - 8192, 0));
END;
CREATE TABLE analysis_attempts (
    request_id          TEXT NOT NULL REFERENCES analysis_requests(request_id) ON DELETE CASCADE,
    attempt             INTEGER NOT NULL CHECK (attempt > 0),
    claim_node_id       TEXT NOT NULL,
    claim_epoch         INTEGER NOT NULL CHECK (claim_epoch > 0),
    claim_expires_at_ms INTEGER NOT NULL,
    phase               TEXT NOT NULL CHECK (
        phase IN ('claimed','source_probe','hashing','staged','running','publishing','retry_wait','published','failed','canceled','stale')),
    started_at_ms       INTEGER NOT NULL,
    phase_updated_at_ms INTEGER NOT NULL,
    terminal_code       TEXT,
    PRIMARY KEY (request_id, claim_epoch)
) STRICT;
CREATE INDEX analysis_attempts_recent
    ON analysis_attempts(request_id, claim_epoch DESC);
CREATE TABLE cluster_fragment_index_heads (
    logical_cache_key   TEXT PRIMARY KEY,
    generation_cache_key TEXT NOT NULL,
    request_id          TEXT NOT NULL,
    updated_at_ms       INTEGER NOT NULL
) STRICT;
CREATE TABLE analysis_lifecycle_counters (
    event  TEXT NOT NULL CHECK (event IN ('claim','lease_loss','retry','cancel','stale','failure','publication')),
    reason TEXT NOT NULL CHECK (reason IN (
      'all','lease_expired','source_catalog_read_failed','source_probe_timeout',
      'source_unavailable','source_attestation_failed','foreground_preempted',
      'source_attestation_timeout','source_record_failed','queue_write_failed',
      'queue_full_or_busy','pipeline_version_unavailable','admin_cancelled',
      'source_deleted','source_identity_changed','attempt_limit','stored_probe_invalid',
      'source_duration_missing','invalid_cache_identity','unsupported','other','validated')),
    count INTEGER NOT NULL DEFAULT 0 CHECK (count >= 0),
    PRIMARY KEY (event, reason)
) STRICT;
CREATE TRIGGER analysis_requests_lifecycle_counters
AFTER UPDATE OF state ON analysis_requests
WHEN OLD.state <> NEW.state
BEGIN
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'claim', 'all', 1 WHERE NEW.state = 'running'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'lease_loss', 'lease_expired', 1
       WHERE OLD.state = 'running'
         AND ((NEW.state = 'queued' AND NEW.last_error_code = 'lease_expired')
           OR (NEW.state = 'failed'
             AND OLD.lease_expires_ms IS NOT NULL
             AND OLD.lease_expires_ms <= NEW.updated_at_ms))
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'retry', CASE NEW.last_error_code
        WHEN 'lease_expired' THEN 'lease_expired'
        WHEN 'source_catalog_read_failed' THEN 'source_catalog_read_failed'
        WHEN 'source_probe_timeout' THEN 'source_probe_timeout'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        WHEN 'source_attestation_failed' THEN 'source_attestation_failed'
        WHEN 'foreground_preempted' THEN 'foreground_preempted'
        WHEN 'source_attestation_timeout' THEN 'source_attestation_timeout'
        WHEN 'source_record_failed' THEN 'source_record_failed'
        WHEN 'queue_write_failed' THEN 'queue_write_failed'
        WHEN 'queue_full_or_busy' THEN 'queue_full_or_busy'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        ELSE 'other' END, 1
       WHERE OLD.state = 'running' AND NEW.state = 'queued'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'cancel', CASE NEW.last_error_code
        WHEN 'admin_cancelled' THEN 'admin_cancelled'
        WHEN 'source_deleted' THEN 'source_deleted'
        ELSE 'other' END, 1
       WHERE NEW.state = 'cancelled'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'stale', 'source_identity_changed', 1
       WHERE NEW.state IN ('failed','cancelled')
         AND NEW.last_error_code IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'failure', CASE NEW.last_error_code
        WHEN 'attempt_limit' THEN 'attempt_limit'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        WHEN 'stored_probe_invalid' THEN 'stored_probe_invalid'
        WHEN 'source_duration_missing' THEN 'source_duration_missing'
        WHEN 'invalid_cache_identity' THEN 'invalid_cache_identity'
        WHEN 'unsupported' THEN 'unsupported'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        ELSE 'other' END, 1
       WHERE NEW.state = 'failed'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'publication', 'validated', 1
       WHERE NEW.state = 'ready' AND NEW.component = 'skip_markers'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
END;
CREATE TRIGGER cluster_fragment_index_lifecycle_counters
AFTER UPDATE OF state ON cluster_fragment_index_jobs
WHEN OLD.state <> NEW.state
BEGIN
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'claim', 'all', 1 WHERE NEW.state = 'running'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'lease_loss', 'lease_expired', 1
       WHERE OLD.state = 'running'
         AND ((NEW.state = 'queued' AND NEW.last_error_code = 'lease_expired')
           OR (NEW.state = 'failed'
             AND OLD.lease_expires_ms IS NOT NULL
             AND OLD.lease_expires_ms <= NEW.updated_at_ms))
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'retry', CASE NEW.last_error_code
        WHEN 'lease_expired' THEN 'lease_expired'
        WHEN 'source_catalog_read_failed' THEN 'source_catalog_read_failed'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        WHEN 'source_attestation_failed' THEN 'source_attestation_failed'
        WHEN 'foreground_preempted' THEN 'foreground_preempted'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        ELSE 'other' END, 1
       WHERE OLD.state = 'running' AND NEW.state = 'queued'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'cancel', CASE NEW.last_error_code
        WHEN 'source_deleted' THEN 'source_deleted' ELSE 'other' END, 1
       WHERE NEW.state = 'cancelled'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'stale', 'source_identity_changed', 1
       WHERE NEW.state IN ('failed','cancelled')
         AND NEW.last_error_code IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'failure', CASE NEW.last_error_code
        WHEN 'attempt_limit' THEN 'attempt_limit'
        WHEN 'pipeline_version_unavailable' THEN 'pipeline_version_unavailable'
        WHEN 'unsupported' THEN 'unsupported'
        WHEN 'source_unavailable' THEN 'source_unavailable'
        ELSE 'other' END, 1
       WHERE NEW.state = 'failed'
         AND COALESCE(NEW.last_error_code, '') NOT IN ('source_changed','source_superseded')
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
    INSERT INTO analysis_lifecycle_counters(event, reason, count)
      SELECT 'publication', 'validated', 1 WHERE NEW.state = 'ready'
      ON CONFLICT(event, reason) DO UPDATE SET count = count + 1;
END;
"#;

/// SQLite v46 / replicated v26: the code each *charged* attempt ended with.
///
/// A job row keeps one `last_error_code`, wiped on every claim and overwritten
/// by the terminal `attempt_limit`, so the row that dies carries no trace of
/// why. 1,933 rows died that way in three days and the cause had to be
/// recovered from the aggregate lifecycle counters. This is a column rather
/// than rows in `analysis_attempts` because a job can exist without a request
/// — playback's foreground enqueue makes one — and that table is keyed by
/// request fence.
///
/// Bounded by `MAX_ANALYSIS_MAX_ATTEMPTS` entries of `MAX_ERROR_CODE_BYTES`:
/// only charged attempts append, so an uncharged yield cannot grow it without
/// limit, and every reopen that resets `attempts` resets the history with it.
pub const ANALYSIS_ATTEMPT_ERRORS_SCHEMA: &str = r#"
ALTER TABLE cluster_fragment_index_jobs
    ADD COLUMN attempt_errors TEXT NOT NULL DEFAULT '';
"#;

/// Which copy-video identity a fragment-index request is for.
///
/// A file has one to three of them — stripped, preserved, converting — and
/// before this the queue could only ever ask for one. A request resolved
/// whichever identity lacked an index first, and its row then became the dedup
/// tombstone that stopped background discovery asking for the others. So a
/// Dolby Vision Profile 7 title got its stripped identity and never its
/// converting one, which is why no converting fragment index existed anywhere
/// on the fleet: only a play attempt or an admin request could reach it.
///
/// Empty for `skip_markers`, which has no copy-video identity, and empty on
/// every row written before this column existed. Empty therefore means
/// "whichever identity is next", which is exactly the old behaviour, so a
/// request in flight across the upgrade resolves the way it always did.
///
/// The forced-successor cancellation is why this has to be a column rather
/// than something folded into an existing one. Forcing a rebuild of one
/// identity must cancel the pending background request for *that* identity and
/// leave its siblings alone; `cancelled` is terminal, so cancelling all three
/// would refuse their generations for good. Nothing else on the row can carry
/// it: `requested_generation` is a fresh UUID on a forced request by design,
/// `pipeline_version` is the engine digest the resolver compares directly and
/// the store dedups and prunes on, and `expected_predecessor_generation` is
/// the heads CAS token.
pub const ANALYSIS_REQUEST_IDENTITY_SCHEMA: &str = r#"
ALTER TABLE analysis_requests
    ADD COLUMN video_identity TEXT NOT NULL DEFAULT '';
"#;

pub const MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_ANALYSIS_MAX_ATTEMPTS: i64 = 5;
pub const MAX_ANALYSIS_MAX_ATTEMPTS: i64 = 20;
pub const DEFAULT_ANALYSIS_LEASE_SECS: i64 = 60;
pub const MAX_ANALYSIS_LEASE_SECS: i64 = 600;
pub const DEFAULT_ANALYSIS_BACKOFF_BASE_SECS: i64 = 5;
pub const MAX_ANALYSIS_BACKOFF_BASE_SECS: i64 = 300;
pub const DEFAULT_ANALYSIS_BACKOFF_MAX_SECS: i64 = 300;
pub const MAX_ANALYSIS_BACKOFF_MAX_SECS: i64 = 3_600;
/// Forward subtitle materialization span. Kept with the other bounded server
/// settings so every consumer resolves the same default and clamp.
pub const DEFAULT_SUBTITLE_WINDOW_SECS: i64 = 200;
pub const MIN_SUBTITLE_WINDOW_SECS: i64 = 30;
pub const MAX_SUBTITLE_WINDOW_SECS: i64 = 900;
/// Virtual head start for forced work. Ordering by the adjusted creation time
/// lets an operator request pass recent background work while guaranteeing
/// that background work older than this window cannot be starved by a stream
/// of newer forced requests.
pub const ANALYSIS_FORCED_PRIORITY_BOOST_MS: i64 = 5 * 60 * 1_000;

pub fn bounded_analysis_max_attempts(value: Option<&str>) -> i64 {
    value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_ANALYSIS_MAX_ATTEMPTS)
        .clamp(1, MAX_ANALYSIS_MAX_ATTEMPTS)
}

fn bounded_analysis_seconds(value: Option<&str>, default: i64, min: i64, max: i64) -> i64 {
    value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

pub fn bounded_analysis_lease_secs(value: Option<&str>) -> i64 {
    bounded_analysis_seconds(
        value,
        DEFAULT_ANALYSIS_LEASE_SECS,
        15,
        MAX_ANALYSIS_LEASE_SECS,
    )
}

pub fn bounded_analysis_backoff_base_secs(value: Option<&str>) -> i64 {
    bounded_analysis_seconds(
        value,
        DEFAULT_ANALYSIS_BACKOFF_BASE_SECS,
        1,
        MAX_ANALYSIS_BACKOFF_BASE_SECS,
    )
}

pub fn bounded_analysis_backoff_max_secs(value: Option<&str>) -> i64 {
    bounded_analysis_seconds(
        value,
        DEFAULT_ANALYSIS_BACKOFF_MAX_SECS,
        1,
        MAX_ANALYSIS_BACKOFF_MAX_SECS,
    )
}

pub fn bounded_subtitle_window_seconds(value: Option<&str>) -> i64 {
    bounded_analysis_seconds(
        value,
        DEFAULT_SUBTITLE_WINDOW_SECS,
        MIN_SUBTITLE_WINDOW_SECS,
        MAX_SUBTITLE_WINDOW_SECS,
    )
}

/// Stable capped exponential jitter shared by request workers and both store
/// backends when they recover an expired lease. Queue identities are ASCII
/// (UUID request ids or hexadecimal content keys), which keeps this formula
/// identical to the SQLite expression used by replicated and local stores.
pub fn analysis_backoff_ms(identity: &str, attempt: i64, base_ms: i64, max_ms: i64) -> i64 {
    let attempt = attempt.max(1);
    let shift = u32::try_from(attempt.saturating_sub(1).min(30)).unwrap_or(0);
    let exponential = base_ms
        .max(1)
        .saturating_mul(1_i64.checked_shl(shift).unwrap_or(i64::MAX));
    let mut chars = identity.chars();
    let first = i64::from(chars.next().map(u32::from).unwrap_or_default());
    let last = i64::from(
        identity
            .chars()
            .next_back()
            .map(u32::from)
            .unwrap_or_default(),
    );
    let seed = i64::try_from(identity.chars().count())
        .unwrap_or(i64::MAX)
        .saturating_mul(17)
        .saturating_add(first.saturating_mul(31))
        .saturating_add(last.saturating_mul(13))
        .saturating_add(attempt.saturating_mul(7));
    let jitter_percent = 75_i64.saturating_add(seed.rem_euclid(51));
    exponential
        .saturating_mul(jitter_percent)
        .saturating_div(100)
        .clamp(1_000, max_ms.max(1_000))
}

/// Portable SQLite/Postgres projection used by both store backends. Retained
/// request generations are authoritative history. A mutable worker job is
/// attached only to the newest request that references its cache key; jobs
/// created solely by background analysis remain standalone rows.
pub(super) const ANALYSIS_CANONICAL_CTE: &str = r#"WITH request_ranked AS (
  SELECT request.*, ROW_NUMBER() OVER (
    PARTITION BY COALESCE(NULLIF(result_cache_key, ''), request_id), target_node_id
    ORDER BY updated_at_ms DESC, request_id DESC) AS cache_rank
    FROM analysis_requests request
), canonical AS (
  SELECT 'request:' || request.request_id AS row_key,
         request.request_id AS request_id,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.cache_key, '') ELSE '' END AS job_id,
         request.file_id AS file_id,
         CASE WHEN files.id IS NULL THEN 0 ELSE 1 END AS file_available,
         COALESCE(files.item_id, 0) AS item_id,
         COALESCE(items.title, '') AS title,
         request.component AS component,
         request.force_rebuild AS force_rebuild,
         request.target_node_id AS target_node_id,
         request.state AS request_state,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.state, '') ELSE '' END AS job_state,
         CASE
           WHEN request.cache_rank = 1 AND job.state = 'ready'
             AND request.component = 'fragment_index'
             AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                  WHERE location.cache_key = job.cache_key
                    AND location.node_id = request.target_node_id)
             THEN request.state
           WHEN request.cache_rank = 1 THEN COALESCE(job.state, request.state)
           ELSE request.state
         END AS state,
         CASE WHEN request.cache_rank = 1 AND COALESCE(job.owner_node_id, '') <> ''
              THEN job.owner_node_id ELSE COALESCE(request.owner_node_id, '') END AS owner_node_id,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.fence ELSE request.fence END AS claim_epoch,
         CASE WHEN request.cache_rank = 1 AND COALESCE(job.lease_expires_ms, 0) > 0
              THEN job.lease_expires_ms ELSE COALESCE(request.lease_expires_ms, 0) END AS lease_expires_ms,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.attempts ELSE request.attempts END AS attempts,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.not_before_ms ELSE request.not_before_ms END AS not_before_ms,
         COALESCE(request.last_error_code, '') AS request_error_code,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.last_error_code, '') ELSE '' END AS job_error_code,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.attempt_errors, '') ELSE '' END AS job_attempt_errors,
         request.created_at_ms AS created_at_ms,
         CASE WHEN request.cache_rank = 1 AND COALESCE(job.updated_at_ms, 0) > request.updated_at_ms
              THEN job.updated_at_ms ELSE request.updated_at_ms END AS updated_at_ms,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN SUBSTR(job.pipeline_sha256, 1, 12)
              ELSE request.pipeline_version END AS pipeline_version,
         request.requested_generation AS requested_generation,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.priority ELSE request.priority END AS priority,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.trigger ELSE request.trigger END AS trigger,
         request.cancel_requested AS cancel_requested,
         COALESCE((SELECT attempt.phase FROM analysis_attempts attempt
                    WHERE attempt.request_id = request.request_id
                    ORDER BY attempt.claim_epoch DESC LIMIT 1), '') AS phase,
         CASE WHEN request.cache_rank = 1 AND job.cache_key IS NOT NULL
              THEN job.source_size ELSE request.source_size END AS source_size
    FROM request_ranked request
    LEFT JOIN cluster_fragment_index_jobs job
      ON request.cache_rank = 1 AND job.cache_key = request.result_cache_key
     AND job.target_node_id = request.target_node_id
    LEFT JOIN files ON files.id = request.file_id
    LEFT JOIN items ON items.id = files.item_id
  UNION ALL
  SELECT 'job:' || job.cache_key || ':' || job.target_node_id AS row_key,
         '' AS request_id, job.cache_key AS job_id, job.file_id AS file_id,
         CASE WHEN files.id IS NULL THEN 0 ELSE 1 END AS file_available,
         COALESCE(files.item_id, 0) AS item_id,
         COALESCE(items.title, '') AS title,
         'fragment_index' AS component, 0 AS force_rebuild,
         job.target_node_id AS target_node_id, '' AS request_state, job.state AS job_state,
         job.state AS state, COALESCE(job.owner_node_id, '') AS owner_node_id,
         job.fence AS claim_epoch,
         COALESCE(job.lease_expires_ms, 0) AS lease_expires_ms,
         job.attempts AS attempts, job.not_before_ms AS not_before_ms,
         '' AS request_error_code, COALESCE(job.last_error_code, '') AS job_error_code,
         COALESCE(job.attempt_errors, '') AS job_attempt_errors,
         job.created_at_ms AS created_at_ms, job.updated_at_ms AS updated_at_ms,
         SUBSTR(job.pipeline_sha256, 1, 12) AS pipeline_version,
         job.cache_key AS requested_generation,
         job.priority AS priority, job.trigger AS trigger, 0 AS cancel_requested,
         CASE job.state WHEN 'running' THEN 'hashing' WHEN 'queued' THEN 'claimed'
           WHEN 'ready' THEN 'published' WHEN 'failed' THEN 'failed'
           WHEN 'cancelled' THEN 'canceled' ELSE '' END AS phase,
         job.source_size AS source_size
    FROM cluster_fragment_index_jobs job
    LEFT JOIN files ON files.id = job.file_id
    LEFT JOIN items ON items.id = files.item_id
   WHERE NOT EXISTS (
     SELECT 1 FROM analysis_requests request
      WHERE request.result_cache_key = job.cache_key
        AND request.target_node_id = job.target_node_id)
), classified_base AS (
  SELECT canonical.*,
         CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1
           WHEN 'submitted' THEN 2 WHEN 'failed' THEN 3
           WHEN 'cancelled' THEN 4 WHEN 'ready' THEN 5 ELSE 6 END AS sort_rank,
         CASE
           WHEN state IN ('queued', 'running', 'submitted')
             AND COALESCE(NULLIF(job_error_code, ''), request_error_code) <> '' THEN 'automatic'
           WHEN state IN ('queued', 'running', 'submitted') THEN 'working'
           WHEN state = 'ready' THEN 'ready'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code) = 'unsupported' THEN 'unsupported'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code)
             IN ('source_deleted', 'source_superseded') THEN 'expected'
           ELSE 'attention'
         END AS disposition
    FROM canonical
), classified AS (
  SELECT classified_base.*,
         CASE
           WHEN file_available = 0 THEN 'none'
           WHEN state = 'ready' THEN 'rebuild'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code) = 'source_superseded'
             THEN 'analyze_current'
           WHEN disposition = 'attention' THEN 'retry'
           ELSE 'none'
         END AS action
    FROM classified_base
)"#;

/// Bounded, join-free projection for the frequently polled status cards.
/// Operator requests already have a hard retained-history cap. Standalone
/// terminal worker rows are restricted to the newest retained window while
/// active and request-referenced jobs are always included.
pub(super) const ANALYSIS_SUMMARY_CTE: &str = r#"WITH request_ranked AS (
  SELECT request.*, ROW_NUMBER() OVER (
    PARTITION BY COALESCE(NULLIF(result_cache_key, ''), request_id), target_node_id
    ORDER BY updated_at_ms DESC, request_id DESC) AS cache_rank
    FROM analysis_requests request
), summary_job_keys AS (
  SELECT result_cache_key AS cache_key, target_node_id FROM analysis_requests
   WHERE result_cache_key IS NOT NULL AND result_cache_key <> ''
  UNION
  SELECT cache_key, target_node_id FROM cluster_fragment_index_jobs
   WHERE state IN ('queued', 'running')
  UNION
  SELECT cache_key, target_node_id FROM (
    SELECT cache_key, target_node_id, updated_at_ms FROM (
      SELECT cache_key, target_node_id, updated_at_ms FROM cluster_fragment_index_jobs
       WHERE state = 'ready'
       ORDER BY updated_at_ms DESC, cache_key, target_node_id LIMIT 8192)
    UNION ALL
    SELECT cache_key, target_node_id, updated_at_ms FROM (
      SELECT cache_key, target_node_id, updated_at_ms FROM cluster_fragment_index_jobs
       WHERE state = 'failed'
       ORDER BY updated_at_ms DESC, cache_key, target_node_id LIMIT 8192)
    UNION ALL
    SELECT cache_key, target_node_id, updated_at_ms FROM (
      SELECT cache_key, target_node_id, updated_at_ms FROM cluster_fragment_index_jobs
       WHERE state = 'cancelled'
       ORDER BY updated_at_ms DESC, cache_key, target_node_id LIMIT 8192)
    ORDER BY updated_at_ms DESC, cache_key, target_node_id LIMIT 8192)
), summary_jobs AS (
  SELECT job.* FROM cluster_fragment_index_jobs job
  JOIN summary_job_keys keys ON keys.cache_key = job.cache_key
   AND keys.target_node_id = job.target_node_id
), summary_canonical AS (
  SELECT 'request:' || request.request_id AS row_key,
         request.file_id AS file_id,
         CASE
           WHEN request.cache_rank = 1 AND job.state = 'ready'
             AND request.component = 'fragment_index'
             AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                  WHERE location.cache_key = job.cache_key
                    AND location.node_id = request.target_node_id)
             THEN request.state
           WHEN request.cache_rank = 1 THEN COALESCE(job.state, request.state)
           ELSE request.state
         END AS state,
         COALESCE(request.last_error_code, '') AS request_error_code,
         CASE WHEN request.cache_rank = 1 THEN COALESCE(job.last_error_code, '') ELSE '' END AS job_error_code,
         CASE WHEN request.cache_rank = 1 AND COALESCE(job.updated_at_ms, 0) > request.updated_at_ms
              THEN job.updated_at_ms ELSE request.updated_at_ms END AS updated_at_ms
    FROM request_ranked request
    LEFT JOIN summary_jobs job
      ON request.cache_rank = 1 AND job.cache_key = request.result_cache_key
     AND job.target_node_id = request.target_node_id
  UNION ALL
  SELECT 'job:' || job.cache_key || ':' || job.target_node_id AS row_key, job.file_id AS file_id,
         job.state AS state, '' AS request_error_code,
         COALESCE(job.last_error_code, '') AS job_error_code,
         job.updated_at_ms AS updated_at_ms
    FROM summary_jobs job
   WHERE NOT EXISTS (
     SELECT 1 FROM analysis_requests request
      WHERE request.result_cache_key = job.cache_key
        AND request.target_node_id = job.target_node_id)
), summary_classified AS (
  SELECT summary_canonical.*,
         CASE
           WHEN state IN ('queued', 'running', 'submitted')
             AND COALESCE(NULLIF(job_error_code, ''), request_error_code) <> '' THEN 'automatic'
           WHEN state IN ('queued', 'running', 'submitted') THEN 'working'
           WHEN state = 'ready' THEN 'ready'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code) = 'unsupported' THEN 'unsupported'
           WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code)
             IN ('source_deleted', 'source_superseded') THEN 'expected'
           ELSE 'attention'
         END AS disposition
    FROM summary_canonical
)"#;
const BLOB_MAGIC: &[u8; 8] = b"PLRXIDX2";
const BLOB_FORMAT_VERSION: u16 = 2;
const ROW_BYTES: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragmentIndexSourceObservation {
    pub node_id: String,
    pub file_id: i64,
    pub object_version: String,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewClusterFragmentIndexJob {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub priority: String,
    pub trigger: String,
    pub target_node_id: String,
    pub not_before_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexJob {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub priority: String,
    pub trigger: String,
    pub target_node_id: String,
    pub state: String,
    pub owner_node_id: String,
    pub fence: i64,
    pub lease_expires_ms: i64,
    pub attempts: i64,
    pub not_before_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_error_code: String,
    /// The code each *charged* attempt ended with, oldest first, comma
    /// separated. Never cleared by a claim; reset only where the retry budget
    /// itself resets. `last_error_code` is the terminal code the UI and the
    /// lifecycle triggers key on — this is the history behind it.
    pub attempt_errors: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAnalysisRequest {
    pub request_id: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub component: String,
    pub pipeline_version: String,
    /// The copy-video identity this request is for, as an argv fingerprint.
    /// Empty means "whichever identity is next", which is what every request
    /// written before the column existed meant, and what `skip_markers` — which
    /// has no copy-video identity at all — always means.
    pub video_identity: String,
    pub requested_generation: String,
    pub priority: String,
    pub trigger: String,
    pub force_rebuild: bool,
    pub target_node_id: String,
    pub not_before_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisRequest {
    pub request_id: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub component: String,
    pub pipeline_version: String,
    /// The copy-video identity this request is for, as an argv fingerprint.
    /// Empty means "whichever identity is next", which is what every request
    /// written before the column existed meant, and what `skip_markers` — which
    /// has no copy-video identity at all — always means.
    pub video_identity: String,
    pub requested_generation: String,
    pub expected_predecessor_generation: String,
    pub priority: String,
    pub trigger: String,
    pub force_rebuild: bool,
    pub target_node_id: String,
    pub state: String,
    pub owner_node_id: String,
    pub fence: i64,
    pub lease_expires_ms: i64,
    pub attempts: i64,
    pub not_before_ms: i64,
    pub result_cache_key: String,
    pub last_error_code: String,
    pub cancel_requested: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisAttempt {
    pub request_id: String,
    pub attempt: i64,
    pub claim_node_id: String,
    pub claim_epoch: i64,
    pub claim_expires_at_ms: i64,
    pub phase: String,
    pub started_at_ms: i64,
    pub phase_updated_at_ms: i64,
    pub terminal_code: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisFileLabel {
    pub file_id: i64,
    pub item_id: i64,
    pub title: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnalysisHistoryFilter {
    #[default]
    All,
    Working,
    Attention,
    Ready,
    Expected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisHistoryCursor {
    pub sort_rank: i64,
    pub updated_at_ms: i64,
    pub row_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisHistoryQuery {
    pub limit: i64,
    pub cursor: Option<AnalysisHistoryCursor>,
    pub filter: AnalysisHistoryFilter,
    pub search: String,
    /// Canonical durable states (`queued`, `claimed`, `retry_wait`, ...).
    /// Empty means all states. Values are validated at the HTTP boundary.
    pub states: Vec<String>,
    /// Server-stamped time used to distinguish queued work from retry wait.
    pub now_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisHistoryRow {
    pub row_key: String,
    pub request_id: String,
    pub job_id: String,
    pub file_id: i64,
    pub item_id: i64,
    pub title: String,
    pub component: String,
    pub force_rebuild: bool,
    pub target_node_id: String,
    pub request_state: String,
    pub job_state: String,
    pub state: String,
    pub disposition: String,
    pub action: String,
    pub owner_node_id: String,
    pub claim_epoch: i64,
    pub lease_expires_ms: i64,
    pub attempts: i64,
    pub not_before_ms: i64,
    pub request_error_code: String,
    pub job_error_code: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub pipeline_version: String,
    pub requested_generation: String,
    pub priority: String,
    pub trigger: String,
    pub cancel_requested: bool,
    pub phase: String,
    pub source_size: i64,
    /// The code each charged attempt of the joined job ended with, oldest
    /// first. Empty for a request with no job, and for the non-current
    /// generations of one.
    pub job_attempt_errors: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisHistoryPage {
    pub rows: Vec<AnalysisHistoryRow>,
    pub filtered_total: i64,
    pub next_cursor: Option<AnalysisHistoryCursor>,
}

/// The ceiling `enqueue_analysis_request` refuses at. Exposed so a bulk
/// operator action can leave headroom rather than filling the table every
/// other producer needs to be able to enqueue into at all.
pub const MAX_ACTIVE_ANALYSIS_REQUESTS: i64 = 4_096;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisStatusSummary {
    pub total: i64,
    pub working: i64,
    pub queued: i64,
    pub running: i64,
    pub submitted: i64,
    pub attention: i64,
    pub expected: i64,
    pub ready: i64,
    pub latest_error_code: String,
    pub latest_error_file_id: i64,
    pub latest_error_updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexArtifact {
    pub cache_key: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub source_sha256: String,
    pub pipeline_sha256: String,
    pub blob_sha256: String,
    pub bytes: i64,
    pub built_by_node_id: String,
    pub built_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFragmentIndexLocation {
    pub cache_key: String,
    pub node_id: String,
    pub bytes: i64,
    pub verified_at_ms: i64,
    pub last_seen_at_ms: i64,
}

#[async_trait]
pub trait ClusterFragmentIndexStore: Send + Sync + 'static {
    /// Persist an operator request before the source digest (and therefore the
    /// content-addressed worker key) is known. Concurrent duplicate requests
    /// join the one active request for the same file/component/node.
    async fn enqueue_analysis_request(
        &self,
        request: &NewAnalysisRequest,
    ) -> Result<AnalysisRequest, StoreError>;

    async fn claim_analysis_request(
        &self,
        node_id: &str,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    async fn renew_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Advance the current fenced attempt's durable phase. The update is
    /// rejected after cancellation or ownership has moved to another fence.
    async fn record_analysis_request_phase(
        &self,
        request: &AnalysisRequest,
        phase: &str,
        terminal_code: Option<&str>,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn analysis_attempts(
        &self,
        request_id: &str,
        limit: i64,
    ) -> Result<Vec<AnalysisAttempt>, StoreError>;

    /// Atomically resolve a current request into its content-addressed worker
    /// row and transition the request to submitted. No stale request owner may
    /// enqueue or reopen work without also committing the fenced handoff.
    async fn submit_fragment_index_analysis(
        &self,
        request: &AnalysisRequest,
        job: &NewClusterFragmentIndexJob,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn retry_analysis_request(
        &self,
        request: &AnalysisRequest,
        error_code: &str,
        now_ms: i64,
        retry_at_ms: i64,
        charge_attempt: bool,
    ) -> Result<bool, StoreError>;

    async fn fail_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        error_code: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Fold terminal fragment-index results into submitted operator requests.
    async fn settle_analysis_requests(&self, now_ms: i64) -> Result<u64, StoreError>;

    async fn prune_analysis_requests(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<u64, StoreError>;

    async fn analysis_requests(&self, limit: i64) -> Result<Vec<AnalysisRequest>, StoreError>;

    /// Terminal requests that a bulk reopen should actually act on.
    ///
    /// Narrower than "everything failed", and the difference is what makes a
    /// bulk reopen safe to press twice. A row is reopenable only when no
    /// other request for the same source identity, component and target is
    /// `queued`, `running`, `submitted` or `ready` — so a file that a previous
    /// reopen already repaired, or whose successor is still in flight, is not
    /// offered again. Without that predicate the failed row stays in the
    /// operator's attention list forever after its successor succeeds, and
    /// every later press re-forces a library that is already indexed.
    ///
    /// Forced requests are excluded: a forced generation is somebody's
    /// deliberate one-off, not a queue fault to repair.
    ///
    /// Rows are ordered oldest-terminal first, so paging through a backlog
    /// makes progress rather than re-offering the same head.
    async fn reopenable_analysis_requests(
        &self,
        component: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AnalysisRequest>, StoreError>;

    async fn analysis_request(
        &self,
        request_id: &str,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    /// Explicit administrator retry. This creates a new request generation so
    /// the deterministic terminal identity remains as a discovery tombstone.
    async fn retry_analysis_request_admin(
        &self,
        request_id: &str,
        requested_generation: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    /// Cancel an operator request and revoke any live request fence. A shared
    /// fragment artifact already published by another identity is untouched.
    async fn cancel_analysis_request_admin(
        &self,
        request_id: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError>;

    /// Publish a non-fragment component directly under the exact live request
    /// fence after its replicated payload has been validated.
    async fn complete_analysis_request(
        &self,
        request: &AnalysisRequest,
        result_key: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Atomically publish a validated semantic set and consume the exact live
    /// request fence. A stale worker can do neither half.
    async fn publish_timeline_annotation_set_for_request(
        &self,
        request: &AnalysisRequest,
        duration_ms: i64,
        set: &crate::segplan::TimelineAnnotationSet,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Stable keyset page over operator requests plus background-only jobs.
    /// A worker job is attached only to the newest request that references its
    /// cache key, so retained request generations remain distinct history.
    async fn analysis_history(
        &self,
        query: &AnalysisHistoryQuery,
    ) -> Result<AnalysisHistoryPage, StoreError>;

    /// Compact aggregate used by frequently-polled operator surfaces.
    async fn analysis_status_summary(&self) -> Result<AnalysisStatusSummary, StoreError>;

    async fn analysis_file_labels(&self, limit: i64) -> Result<Vec<AnalysisFileLabel>, StoreError>;

    async fn cluster_fragment_index_jobs(
        &self,
        limit: i64,
    ) -> Result<Vec<ClusterFragmentIndexJob>, StoreError>;

    async fn cluster_fragment_index_job(
        &self,
        cache_key: &str,
        target_node_id: &str,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError>;

    /// Resolve the immutable generation currently published for a logical
    /// file/source/pipeline identity. Coverage work on another node must build
    /// this generation rather than creating an unused normal generation.
    async fn cluster_fragment_index_current_generation(
        &self,
        logical_cache_key: &str,
    ) -> Result<Option<String>, StoreError>;

    async fn record_fragment_index_source(
        &self,
        observation: &FragmentIndexSourceObservation,
    ) -> Result<(), StoreError>;

    async fn fragment_index_source(
        &self,
        node_id: &str,
        file_id: i64,
        object_version: &str,
    ) -> Result<Option<FragmentIndexSourceObservation>, StoreError>;

    async fn cluster_fragment_index_artifact(
        &self,
        cache_key: &str,
    ) -> Result<Option<ClusterFragmentIndexArtifact>, StoreError>;

    async fn enqueue_cluster_fragment_index(
        &self,
        job: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError>;

    /// Reopen an existing catalog artifact whose physical holders could not
    /// supply a valid blob. The immutable artifact remains the expected
    /// identity while a fenced worker deterministically reconstructs it.
    async fn requeue_cluster_fragment_index(
        &self,
        replacement: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError>;

    async fn claim_cluster_fragment_index(
        &self,
        node_id: &str,
        excluded_cache_keys: &[String],
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError>;

    async fn renew_cluster_fragment_index(
        &self,
        cache_key: &str,
        target_node_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Return a claim this node cannot execute without charging the shared
    /// retry budget. Used for node-local path/mount refusals; the worker stops
    /// its pass after yielding so it cannot immediately reclaim the same row.
    async fn yield_cluster_fragment_index(
        &self,
        cache_key: &str,
        target_node_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn complete_cluster_fragment_index(
        &self,
        job: &ClusterFragmentIndexJob,
        artifact: &ClusterFragmentIndexArtifact,
        location: &ClusterFragmentIndexLocation,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Settle a claimed job from an artifact another node already built.
    ///
    /// Discovery on each voter targets itself, so four voters queue four jobs
    /// for one `cache_key` and, on a healthy queue, four full passes over the
    /// same file produce one artifact. Once any of them has published it, the
    /// rest need only the bytes and a location row.
    ///
    /// This exists rather than reusing [`ClusterFragmentIndexStore::complete_cluster_fragment_index`]
    /// because that one requires `artifact.built_by_node_id == job.owner_node_id`
    /// — correct, for a build. Relabelling a hydrated artifact to get past it
    /// would appear to work only because the artifact insert is
    /// `ON CONFLICT DO NOTHING`, which is a trick and not a contract: the
    /// stored row would keep its real builder while the caller lied about it.
    ///
    /// So the artifact here is required to already exist, byte for byte, and
    /// is never written. Every other guard is the build path's: the same
    /// running/owner/fence/lease check, the same source-identity check, the
    /// same head advance. Settling from inside the claimed job is what keeps
    /// the request fence intact — the reason the older comment gave for
    /// submitting an ordinary worker instead of hydrating.
    async fn complete_cluster_fragment_index_by_hydration(
        &self,
        job: &ClusterFragmentIndexJob,
        artifact: &ClusterFragmentIndexArtifact,
        location: &ClusterFragmentIndexLocation,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    #[allow(clippy::too_many_arguments)]
    async fn fail_cluster_fragment_index(
        &self,
        cache_key: &str,
        target_node_id: &str,
        node_id: &str,
        fence: i64,
        error_code: &str,
        retryable: bool,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn put_cluster_fragment_index_location(
        &self,
        location: &ClusterFragmentIndexLocation,
    ) -> Result<(), StoreError>;

    async fn cluster_fragment_index_locations(
        &self,
        cache_key: &str,
    ) -> Result<Vec<ClusterFragmentIndexLocation>, StoreError>;

    async fn forget_cluster_fragment_index_location(
        &self,
        cache_key: &str,
        node_id: &str,
    ) -> Result<bool, StoreError>;

    /// Remove a bounded set of terminal catalog generations that have not
    /// had a verified holder within the retention window. Returns artifact
    /// keys whose node-local bytes may now be removed.
    async fn prune_cluster_fragment_indexes(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<Vec<String>, StoreError>;
}

/// Canonical structural work and artifact identity. Every component is
/// length-delimited. The file and source generation are intentionally part of
/// the key: byte-identical files remain independent work/coverage identities,
/// and replacing a file cannot attach its request to an older generation.
pub fn cluster_fragment_index_key(
    file_id: i64,
    source_size: i64,
    source_mtime: i64,
    source_sha256: &str,
    pipeline_sha256: &str,
) -> Option<String> {
    if file_id <= 0 || source_size < 0 || !is_sha256(source_sha256) || !is_sha256(pipeline_sha256) {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/cache-key\0");
    digest.update(BLOB_FORMAT_VERSION.to_be_bytes());
    digest.update(SEGPLAN_VERSION.to_be_bytes());
    digest.update(file_id.to_be_bytes());
    digest.update(source_size.to_be_bytes());
    digest.update(source_mtime.to_be_bytes());
    update_field(&mut digest, source_sha256.as_bytes());
    update_field(&mut digest, pipeline_sha256.as_bytes());
    Some(hex::encode(digest.finalize()))
}

/// Physical key for a forced successor. The logical content/pipeline key
/// remains the serving pointer; this key keeps the predecessor's immutable
/// bytes addressable until the successor wins publication CAS.
pub fn cluster_fragment_index_generation_key(
    file_id: i64,
    source_size: i64,
    source_mtime: i64,
    source_sha256: &str,
    pipeline_sha256: &str,
    generation: &str,
) -> Option<String> {
    if file_id <= 0
        || source_size < 0
        || !is_sha256(source_sha256)
        || !is_sha256(pipeline_sha256)
        || generation.trim().is_empty()
        || generation.len() > 128
    {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/generation-key\0");
    digest.update(BLOB_FORMAT_VERSION.to_be_bytes());
    digest.update(SEGPLAN_VERSION.to_be_bytes());
    digest.update(file_id.to_be_bytes());
    digest.update(source_size.to_be_bytes());
    digest.update(source_mtime.to_be_bytes());
    update_field(&mut digest, source_sha256.as_bytes());
    update_field(&mut digest, pipeline_sha256.as_bytes());
    update_field(&mut digest, generation.as_bytes());
    Some(hex::encode(digest.finalize()))
}

/// Cryptographic digest of a pipeline vector. The daemon passes the exact
/// video-deciding argv plus its engine digest; source path is intentionally
/// excluded because the complete source digest already names the bytes.
pub fn cluster_fragment_index_pipeline_digest(
    engine_sha256: &str,
    args: &[String],
) -> Option<String> {
    if !is_sha256(engine_sha256) {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"plurx/fragment-index/pipeline\0");
    digest.update(SEGPLAN_VERSION.to_be_bytes());
    update_field(&mut digest, engine_sha256.as_bytes());
    for arg in args {
        update_field(&mut digest, arg.as_bytes());
    }
    Some(hex::encode(digest.finalize()))
}

fn update_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Serialize, Deserialize)]
struct BlobHeader {
    segplan_version: u32,
    timescale: u32,
    init_sha256: String,
    promotion: PromotionInputs,
    parameter_sets_constant: bool,
    source_sha256: String,
    pipeline_sha256: String,
    rows: usize,
}

pub fn encode_cluster_fragment_index_blob(
    index: &FragmentIndex,
    source_sha256: &str,
    pipeline_sha256: &str,
) -> Result<Vec<u8>, StoreError> {
    if index.version != SEGPLAN_VERSION
        || index.timescale == 0
        || index.rows.is_empty()
        || !is_sha256(source_sha256)
        || !is_sha256(pipeline_sha256)
    {
        return Err(StoreError::Task(
            "invalid cluster fragment-index artifact".to_owned(),
        ));
    }
    let header = serde_json::to_vec(&BlobHeader {
        segplan_version: index.version,
        timescale: index.timescale,
        init_sha256: index.init_sha256.clone(),
        promotion: index.promotion.clone(),
        parameter_sets_constant: index.parameter_sets_constant,
        source_sha256: source_sha256.to_ascii_lowercase(),
        pipeline_sha256: pipeline_sha256.to_ascii_lowercase(),
        rows: index.rows.len(),
    })
    .map_err(|error| StoreError::Task(format!("encoding fragment-index header: {error}")))?;
    let rows_bytes = index
        .rows
        .len()
        .checked_mul(ROW_BYTES)
        .ok_or_else(|| StoreError::Task("fragment-index row size overflow".to_owned()))?;
    let capacity = 8_usize
        .checked_add(2 + 4)
        .and_then(|bytes| bytes.checked_add(header.len()))
        .and_then(|bytes| bytes.checked_add(rows_bytes))
        .ok_or_else(|| StoreError::Task("fragment-index blob size overflow".to_owned()))?;
    if capacity > MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES {
        return Err(StoreError::Task(
            "fragment-index blob exceeds hard limit".to_owned(),
        ));
    }
    let mut blob = Vec::with_capacity(capacity);
    blob.extend_from_slice(BLOB_MAGIC);
    blob.extend_from_slice(&BLOB_FORMAT_VERSION.to_be_bytes());
    blob.extend_from_slice(
        &u32::try_from(header.len())
            .map_err(|_| StoreError::Task("fragment-index header is too large".to_owned()))?
            .to_be_bytes(),
    );
    blob.extend_from_slice(&header);
    for row in &index.rows {
        blob.extend_from_slice(&row.dts.to_le_bytes());
        blob.extend_from_slice(
            &u32::try_from(row.duration)
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        blob.extend_from_slice(&row.bytes.to_le_bytes());
        blob.extend_from_slice(&row.video_bytes.to_le_bytes());
        blob.push(class_code(row.class));
        blob.extend_from_slice(&[0, 0, 0]);
    }
    Ok(blob)
}

pub fn decode_cluster_fragment_index_blob(
    blob: &[u8],
    expected_source_sha256: &str,
    expected_pipeline_sha256: &str,
) -> Result<FragmentIndex, StoreError> {
    if blob.len() > MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES || blob.len() < 14 {
        return Err(StoreError::Task(
            "invalid fragment-index blob size".to_owned(),
        ));
    }
    if &blob[..8] != BLOB_MAGIC {
        return Err(StoreError::Task(
            "invalid fragment-index blob magic".to_owned(),
        ));
    }
    let version = u16::from_be_bytes(blob[8..10].try_into().expect("two bytes"));
    if version != BLOB_FORMAT_VERSION {
        return Err(StoreError::Task(format!(
            "unsupported fragment-index blob version {version}"
        )));
    }
    let header_len = u32::from_be_bytes(blob[10..14].try_into().expect("four bytes")) as usize;
    let rows_offset = 14_usize
        .checked_add(header_len)
        .filter(|offset| *offset <= blob.len())
        .ok_or_else(|| StoreError::Task("truncated fragment-index blob header".to_owned()))?;
    let header: BlobHeader = serde_json::from_slice(&blob[14..rows_offset])
        .map_err(|error| StoreError::Task(format!("decoding fragment-index header: {error}")))?;
    if header.segplan_version != SEGPLAN_VERSION
        || header.timescale == 0
        || header.rows == 0
        || !header
            .source_sha256
            .eq_ignore_ascii_case(expected_source_sha256)
        || !header
            .pipeline_sha256
            .eq_ignore_ascii_case(expected_pipeline_sha256)
    {
        return Err(StoreError::Task(
            "fragment-index blob identity mismatch".to_owned(),
        ));
    }
    let packed = &blob[rows_offset..];
    if packed.len() != header.rows.saturating_mul(ROW_BYTES) {
        return Err(StoreError::Task(
            "fragment-index blob row count mismatch".to_owned(),
        ));
    }
    let mut rows = Vec::with_capacity(header.rows);
    for chunk in packed.chunks_exact(ROW_BYTES) {
        let class = class_from_code(chunk[20]).ok_or_else(|| {
            StoreError::Task(format!("fragment-index blob has cut class {}", chunk[20]))
        })?;
        rows.push(IndexRow {
            dts: u64::from_le_bytes(chunk[0..8].try_into().expect("eight bytes")),
            duration: u64::from(u32::from_le_bytes(
                chunk[8..12].try_into().expect("four bytes"),
            )),
            bytes: u32::from_le_bytes(chunk[12..16].try_into().expect("four bytes")),
            video_bytes: u32::from_le_bytes(chunk[16..20].try_into().expect("four bytes")),
            class,
        });
    }
    // The cluster artifact is named by source *content* and pipeline content.
    // Legacy SourceIdentity contains scanner mtime and a non-cryptographic argv
    // fingerprint, so serializing it would make duplicate files produce
    // different bytes for the same cache key. Consumers that bridge this v2
    // artifact into the file-keyed v1 store replace this neutral identity with
    // that file's current local identity.
    let canonical_source = SourceIdentity::new(0, 0, header.pipeline_sha256.clone());
    let mut index =
        FragmentIndex::new(header.timescale, rows, header.init_sha256, canonical_source);
    index.promotion = header.promotion;
    index.parameter_sets_constant = header.parameter_sets_constant;
    Ok(index)
}

pub fn cluster_fragment_index_blob_sha256(blob: &[u8]) -> String {
    hex::encode(Sha256::digest(blob))
}

fn class_code(class: CutClass) -> u8 {
    match class {
        CutClass::CleanIdr => 1,
        CutClass::CleanCra => 2,
        CutClass::Dirty => 3,
        CutClass::Unparseable => 4,
    }
}

fn class_from_code(code: u8) -> Option<CutClass> {
    match code {
        1 => Some(CutClass::CleanIdr),
        2 => Some(CutClass::CleanCra),
        3 => Some(CutClass::Dirty),
        4 => Some(CutClass::Unparseable),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    #[test]
    fn cache_key_is_domain_separated_and_exact() {
        let a =
            cluster_fragment_index_key(1, 100, 10, &digest('a'), &digest('b')).expect("valid key");
        let b =
            cluster_fragment_index_key(1, 100, 10, &digest('b'), &digest('a')).expect("valid key");
        let other_file = cluster_fragment_index_key(2, 100, 10, &digest('a'), &digest('b'))
            .expect("other file key");
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_ne!(a, other_file);
        assert!(cluster_fragment_index_key(1, 100, 10, "not-a-digest", &digest('b')).is_none());
    }

    #[test]
    fn generation_keys_are_immutable_successors_of_one_logical_key() {
        let source = digest('a');
        let pipeline = digest('b');
        let logical =
            cluster_fragment_index_key(1, 100, 10, &source, &pipeline).expect("logical key");
        let first =
            cluster_fragment_index_generation_key(1, 100, 10, &source, &pipeline, "generation-1")
                .expect("first generation");
        let second =
            cluster_fragment_index_generation_key(1, 100, 10, &source, &pipeline, "generation-2")
                .expect("second generation");
        assert_ne!(logical, first);
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn analysis_retry_settings_have_safe_defaults_and_hard_maximums() {
        assert_eq!(
            bounded_analysis_max_attempts(None),
            DEFAULT_ANALYSIS_MAX_ATTEMPTS
        );
        assert_eq!(
            bounded_analysis_max_attempts(Some("999")),
            MAX_ANALYSIS_MAX_ATTEMPTS
        );
        assert_eq!(
            bounded_analysis_lease_secs(Some("0")),
            15,
            "a lease shorter than the renewal floor is refused"
        );
        assert_eq!(
            bounded_analysis_lease_secs(Some("99999")),
            MAX_ANALYSIS_LEASE_SECS
        );
        assert_eq!(
            bounded_analysis_backoff_base_secs(Some("99999")),
            MAX_ANALYSIS_BACKOFF_BASE_SECS
        );
        assert_eq!(
            bounded_analysis_backoff_max_secs(Some("99999")),
            MAX_ANALYSIS_BACKOFF_MAX_SECS
        );
    }

    #[test]
    fn subtitle_window_setting_has_the_documented_default_and_bounds() {
        assert_eq!(
            bounded_subtitle_window_seconds(None),
            DEFAULT_SUBTITLE_WINDOW_SECS
        );
        assert_eq!(
            bounded_subtitle_window_seconds(Some("not-a-number")),
            DEFAULT_SUBTITLE_WINDOW_SECS
        );
        assert_eq!(
            bounded_subtitle_window_seconds(Some("0")),
            MIN_SUBTITLE_WINDOW_SECS
        );
        assert_eq!(
            bounded_subtitle_window_seconds(Some("99999")),
            MAX_SUBTITLE_WINDOW_SECS
        );
    }

    #[test]
    fn analysis_backoff_is_stable_exponential_and_capped() {
        let first = analysis_backoff_ms("request-a", 1, 5_000, 300_000);
        assert_eq!(first, analysis_backoff_ms("request-a", 1, 5_000, 300_000));
        let second = analysis_backoff_ms("request-a", 2, 5_000, 300_000);
        assert!(second > first);
        assert!(analysis_backoff_ms("request-b", 1, 5_000, 300_000) != first);
        assert_eq!(
            analysis_backoff_ms("request-a", 60, 5_000, 300_000),
            300_000
        );
    }

    #[test]
    fn blob_round_trips_and_rejects_identity_aliases() {
        let mut index = FragmentIndex::new(
            90_000,
            vec![IndexRow {
                dts: 12,
                duration: 3_003,
                bytes: 45,
                video_bytes: 31,
                class: CutClass::CleanIdr,
            }],
            digest('c'),
            SourceIdentity::new(123, 456, "argv"),
        );
        index.parameter_sets_constant = true;
        let source = digest('a');
        let pipeline = digest('b');
        let blob = encode_cluster_fragment_index_blob(&index, &source, &pipeline).expect("encode");
        let decoded =
            decode_cluster_fragment_index_blob(&blob, &source, &pipeline).expect("decode");
        assert_eq!(decoded.rows, index.rows);
        assert_eq!(decoded.init_sha256, index.init_sha256);
        assert_eq!(decoded.promotion, index.promotion);
        assert_eq!(
            decoded.parameter_sets_constant,
            index.parameter_sets_constant
        );
        assert_eq!(decoded.source, SourceIdentity::new(0, 0, pipeline.clone()));
        assert!(decode_cluster_fragment_index_blob(&blob, &digest('d'), &pipeline).is_err());
    }

    #[test]
    fn blob_bytes_ignore_file_specific_legacy_identity() {
        let row = IndexRow {
            dts: 12,
            duration: 3_003,
            bytes: 45,
            video_bytes: 31,
            class: CutClass::CleanIdr,
        };
        let first = FragmentIndex::new(
            90_000,
            vec![row.clone()],
            digest('c'),
            SourceIdentity::new(123, 456, "argv-a"),
        );
        let second = FragmentIndex::new(
            90_000,
            vec![row],
            digest('c'),
            SourceIdentity::new(123, 999, "argv-b"),
        );
        let source = digest('a');
        let pipeline = digest('b');
        assert_eq!(
            encode_cluster_fragment_index_blob(&first, &source, &pipeline).expect("first"),
            encode_cluster_fragment_index_blob(&second, &source, &pipeline).expect("second")
        );
    }
}
