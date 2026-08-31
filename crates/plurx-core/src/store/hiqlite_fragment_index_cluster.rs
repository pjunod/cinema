//! Replicated catalog and fenced queue for content-addressed fragment indexes.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::fragment_index_cluster::{
    bounded_analysis_backoff_base_secs, bounded_analysis_backoff_max_secs,
    bounded_analysis_max_attempts,
};
use super::fragment_index_cluster::{ANALYSIS_CANONICAL_CTE, ANALYSIS_SUMMARY_CTE};
use super::hiqlite::{database_error, validate_sql, HiqliteAuthStore};
use super::{
    cluster_fragment_index_generation_key, cluster_fragment_index_key, AnalysisAttempt,
    AnalysisFileLabel, AnalysisHistoryCursor, AnalysisHistoryFilter, AnalysisHistoryPage,
    AnalysisHistoryQuery, AnalysisHistoryRow, AnalysisRequest, AnalysisStatusSummary,
    ClusterFragmentIndexArtifact, ClusterFragmentIndexJob, ClusterFragmentIndexLocation,
    ClusterFragmentIndexStore, FragmentIndexSourceObservation, NewAnalysisRequest,
    NewClusterFragmentIndexJob,
};
use crate::error::StoreError;

const MAX_ACTIVE_JOBS: i64 = 4_096;
const MAX_ERROR_CODE_BYTES: usize = 64;
const MAX_LOCAL_EXCLUSIONS: usize = MAX_ACTIVE_JOBS as usize;
const CLAIM_SCAN_LIMIT: i64 = MAX_ACTIVE_JOBS;
const QUEUE_ELIGIBILITY_MS: i64 = 6 * 60 * 60 * 1_000;
const MAX_ANALYSIS_REQUESTS: i64 = 4_096;
const MAX_LIST_ROWS: i64 = 500;
const MAX_ATTEMPT_HISTORY_PER_REQUEST: i64 = 64;

struct AnalysisSettingRow(String);

impl From<&mut Row<'_>> for AnalysisSettingRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("value"))
    }
}

struct SchemaCountRow(i64);

impl From<&mut Row<'_>> for SchemaCountRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("count"))
    }
}

async fn configured_max_attempts(store: &HiqliteAuthStore) -> Result<i64, StoreError> {
    let raw = store
        .client()
        .query_consistent_map::<AnalysisSettingRow, _>(
            "SELECT value FROM settings WHERE key = $1",
            params!(crate::store::keys::ANALYSIS_MAX_ATTEMPTS),
        )
        .await?
        .into_iter()
        .next()
        .map(|row| row.0);
    Ok(bounded_analysis_max_attempts(raw.as_deref()))
}

async fn configured_backoff_base_ms(store: &HiqliteAuthStore) -> Result<i64, StoreError> {
    let raw = store
        .client()
        .query_consistent_map::<AnalysisSettingRow, _>(
            "SELECT value FROM settings WHERE key = $1",
            params!(crate::store::keys::ANALYSIS_BACKOFF_BASE_SECS),
        )
        .await?
        .into_iter()
        .next()
        .map(|row| row.0);
    Ok(bounded_analysis_backoff_base_secs(raw.as_deref()).saturating_mul(1_000))
}

async fn configured_backoff_max_ms(store: &HiqliteAuthStore) -> Result<i64, StoreError> {
    let raw = store
        .client()
        .query_consistent_map::<AnalysisSettingRow, _>(
            "SELECT value FROM settings WHERE key = $1",
            params!(crate::store::keys::ANALYSIS_BACKOFF_MAX_SECS),
        )
        .await?
        .into_iter()
        .next()
        .map(|row| row.0);
    Ok(bounded_analysis_backoff_max_secs(raw.as_deref()).saturating_mul(1_000))
}

// These arrays are the immutable replicated-store migrations for schema v12,
// v13, and v15. Keep them as individual statements: Hiqlite's `batch` API applies
// statements independently, while `txn` rolls the complete version step back
// if any statement or the version-marker update fails.
//
// `CLUSTER_FRAGMENT_INDEX_SCHEMA` and `ANALYSIS_REQUESTS_SCHEMA` remain the
// matching SQLite migration strings. The parity test below prevents either
// representation from drifting without review.
const FRAGMENT_INDEX_SCHEMA_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS cluster_fragment_index_sources (
        node_id          TEXT NOT NULL,
        file_id          INTEGER NOT NULL,
        object_version   TEXT NOT NULL,
        source_size      INTEGER NOT NULL,
        source_mtime     INTEGER NOT NULL,
        source_sha256    TEXT NOT NULL,
        observed_at_ms   INTEGER NOT NULL,
        PRIMARY KEY (node_id, file_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS cluster_fragment_index_jobs (
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
    ) STRICT"#,
    r#"CREATE INDEX IF NOT EXISTS cluster_fragment_index_jobs_due
        ON cluster_fragment_index_jobs(state, not_before_ms, created_at_ms, cache_key)"#,
    r#"CREATE TABLE IF NOT EXISTS cluster_fragment_index_artifacts (
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
    ) STRICT"#,
    r#"CREATE INDEX IF NOT EXISTS cluster_fragment_index_artifacts_file
        ON cluster_fragment_index_artifacts(file_id, source_size, source_mtime, pipeline_sha256)"#,
    r#"CREATE TABLE IF NOT EXISTS cluster_fragment_index_locations (
        cache_key         TEXT NOT NULL,
        node_id           TEXT NOT NULL,
        bytes             INTEGER NOT NULL CHECK (bytes > 0),
        verified_at_ms    INTEGER NOT NULL,
        last_seen_at_ms   INTEGER NOT NULL,
        PRIMARY KEY (cache_key, node_id)
    ) STRICT"#,
    r#"CREATE INDEX IF NOT EXISTS cluster_fragment_index_locations_node
        ON cluster_fragment_index_locations(node_id, last_seen_at_ms, cache_key)"#,
    r#"CREATE TRIGGER IF NOT EXISTS cluster_fragment_indexes_cancel_source BEFORE DELETE ON files
    BEGIN
        DELETE FROM cluster_fragment_index_sources WHERE file_id = OLD.id;
        UPDATE cluster_fragment_index_jobs
           SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
               last_error_code = 'source_deleted'
         WHERE file_id = OLD.id AND state IN ('queued', 'running');
    END"#,
];

const ANALYSIS_REQUEST_SCHEMA_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS analysis_requests (
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
    ) STRICT"#,
    r#"CREATE INDEX IF NOT EXISTS analysis_requests_due
        ON analysis_requests(target_node_id, state, not_before_ms, created_at_ms, request_id)"#,
    r#"CREATE INDEX IF NOT EXISTS analysis_requests_status
        ON analysis_requests(state, updated_at_ms DESC, request_id)"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS analysis_requests_one_active_source
        ON analysis_requests(file_id, source_size, source_mtime, component, target_node_id)
        WHERE state IN ('queued', 'running', 'submitted')"#,
    r#"CREATE TRIGGER IF NOT EXISTS analysis_requests_cancel_source BEFORE DELETE ON files
    BEGIN
        UPDATE analysis_requests
           SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
               last_error_code = 'source_deleted'
         WHERE file_id = OLD.id AND state IN ('queued', 'running', 'submitted');
    END"#,
    r#"CREATE TRIGGER IF NOT EXISTS analysis_requests_supersede_source
    AFTER UPDATE OF size, mtime ON files
    WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime
    BEGIN
        UPDATE analysis_requests
           SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
               last_error_code = 'source_superseded'
         WHERE file_id = NEW.id AND state IN ('queued', 'running', 'submitted')
           AND (source_size <> NEW.size OR source_mtime <> NEW.mtime);
    END"#,
    r#"CREATE TRIGGER IF NOT EXISTS analysis_requests_bound_terminal_history
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
    END"#,
];

const ANALYSIS_HISTORY_INDEX_STATEMENTS: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS analysis_requests_result_history
        ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
        WHERE result_cache_key IS NOT NULL AND result_cache_key <> ''"#,
    r#"CREATE INDEX IF NOT EXISTS cluster_fragment_index_jobs_status_history
        ON cluster_fragment_index_jobs(state, updated_at_ms DESC, cache_key)"#,
];

const ANALYSIS_COMPONENT_STATEMENTS: &[&str] = &[
    "ALTER TABLE cluster_fragment_index_jobs ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('normal','forced','foreground'))",
    "ALTER TABLE cluster_fragment_index_jobs ADD COLUMN trigger TEXT NOT NULL DEFAULT 'background' CHECK (trigger IN ('admin','background','foreground'))",
    "ALTER TABLE cluster_fragment_index_jobs ADD COLUMN target_node_id TEXT NOT NULL DEFAULT ''",
    "DROP TRIGGER IF EXISTS cluster_fragment_indexes_cancel_source",
    "DROP INDEX IF EXISTS cluster_fragment_index_jobs_status_history",
    "DROP INDEX IF EXISTS cluster_fragment_index_jobs_due",
    "ALTER TABLE cluster_fragment_index_jobs RENAME TO cluster_fragment_index_jobs_v21",
    r#"CREATE TABLE cluster_fragment_index_jobs (
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
    ) STRICT"#,
    r#"INSERT INTO cluster_fragment_index_jobs
        (cache_key, file_id, source_size, source_mtime, source_sha256,
         pipeline_sha256, priority, trigger, target_node_id, state,
         owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
         last_error_code, created_at_ms, updated_at_ms)
     SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
            pipeline_sha256, priority, trigger,
            COALESCE((SELECT request.target_node_id FROM analysis_requests request
                       WHERE request.result_cache_key = cluster_fragment_index_jobs_v21.cache_key
                       ORDER BY request.updated_at_ms DESC, request.request_id DESC LIMIT 1), target_node_id), state,
            owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
            last_error_code, created_at_ms, updated_at_ms
       FROM cluster_fragment_index_jobs_v21"#,
    "DROP TABLE cluster_fragment_index_jobs_v21",
    r#"CREATE INDEX cluster_fragment_index_jobs_due
        ON cluster_fragment_index_jobs(state, not_before_ms, created_at_ms, cache_key, target_node_id)"#,
    r#"CREATE INDEX cluster_fragment_index_jobs_status_history
        ON cluster_fragment_index_jobs(state, updated_at_ms DESC, cache_key, target_node_id)"#,
    r#"CREATE TRIGGER cluster_fragment_indexes_cancel_source BEFORE DELETE ON files
    BEGIN
        DELETE FROM cluster_fragment_index_sources WHERE file_id = OLD.id;
        UPDATE cluster_fragment_index_jobs
           SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
               fence = fence + 1, last_error_code = 'source_deleted',
               updated_at_ms = MAX(updated_at_ms, OLD.scanned_at * 1000)
         WHERE file_id = OLD.id AND state IN ('queued', 'running');
    END"#,
    "ALTER TABLE timeline_annotation_sets ADD COLUMN publication_priority TEXT NOT NULL DEFAULT 'normal' CHECK (publication_priority IN ('normal','forced'))",
    "DROP TABLE IF EXISTS analysis_attempts",
    "DROP TABLE IF EXISTS cluster_fragment_index_heads",
    "DROP TRIGGER IF EXISTS analysis_requests_bound_terminal_history",
    "DROP TRIGGER IF EXISTS analysis_requests_supersede_source",
    "DROP TRIGGER IF EXISTS analysis_requests_cancel_source",
    "DROP INDEX IF EXISTS analysis_requests_result_history",
    "DROP INDEX IF EXISTS analysis_requests_one_active_forced_successor",
    "DROP INDEX IF EXISTS analysis_requests_one_active_source",
    "DROP INDEX IF EXISTS analysis_requests_status",
    "DROP INDEX IF EXISTS analysis_requests_due",
    "ALTER TABLE analysis_requests RENAME TO analysis_requests_v21",
    r#"CREATE TABLE analysis_requests (
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
    ) STRICT"#,
    r#"INSERT INTO analysis_requests
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
       FROM analysis_requests_v21"#,
    "DROP TABLE analysis_requests_v21",
    r#"CREATE INDEX analysis_requests_due
        ON analysis_requests(target_node_id, state, not_before_ms, created_at_ms, request_id)"#,
    r#"CREATE INDEX analysis_requests_status
        ON analysis_requests(state, updated_at_ms DESC, request_id)"#,
    r#"CREATE UNIQUE INDEX analysis_requests_one_active_source
        ON analysis_requests(file_id, source_size, source_mtime, component,
                             pipeline_version, requested_generation, target_node_id)
        WHERE state IN ('queued', 'running', 'submitted')"#,
    r#"CREATE UNIQUE INDEX analysis_requests_one_active_forced_successor
        ON analysis_requests(file_id, source_size, source_mtime, component)
        WHERE force_rebuild = 1 AND state IN ('queued', 'running', 'submitted')"#,
    r#"CREATE INDEX analysis_requests_result_history
        ON analysis_requests(result_cache_key, updated_at_ms DESC, request_id DESC)
        WHERE result_cache_key IS NOT NULL AND result_cache_key <> ''"#,
    r#"CREATE TRIGGER analysis_requests_cancel_source BEFORE DELETE ON files
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
    END"#,
    r#"CREATE TRIGGER analysis_requests_supersede_source
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
    END"#,
    r#"CREATE TRIGGER analysis_requests_bound_terminal_history
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
    END"#,
    r#"CREATE TABLE analysis_attempts (
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
    ) STRICT"#,
    r#"CREATE INDEX analysis_attempts_recent
        ON analysis_attempts(request_id, claim_epoch DESC)"#,
    r#"CREATE TABLE cluster_fragment_index_heads (
        logical_cache_key    TEXT PRIMARY KEY,
        generation_cache_key TEXT NOT NULL,
        request_id           TEXT NOT NULL,
        updated_at_ms        INTEGER NOT NULL
    ) STRICT"#,
    r#"CREATE TABLE analysis_lifecycle_counters (
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
    ) STRICT"#,
    r#"CREATE TRIGGER analysis_requests_lifecycle_counters
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
    END"#,
    r#"CREATE TRIGGER cluster_fragment_index_lifecycle_counters
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
    END"#,
];

fn migration_statements(
    statements: &'static [&'static str],
) -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    statements
        .iter()
        .map(|sql| {
            validate_sql(sql)?;
            Ok(((*sql).to_owned(), params!()))
        })
        .collect()
}

pub(super) fn fragment_index_schema_migration_statements(
) -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    migration_statements(FRAGMENT_INDEX_SCHEMA_STATEMENTS)
}

pub(super) fn analysis_request_schema_migration_statements(
) -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    migration_statements(ANALYSIS_REQUEST_SCHEMA_STATEMENTS)
}

pub(super) fn analysis_history_index_migration_statements(
) -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    migration_statements(ANALYSIS_HISTORY_INDEX_STATEMENTS)
}

pub(super) fn analysis_component_migration_statements(
) -> Result<Vec<(String, hiqlite::Params)>, StoreError> {
    migration_statements(ANALYSIS_COMPONENT_STATEMENTS)
}

/// Whether the complete v22 analysis shape is already installed.
///
/// The bootstrap schema transaction lands before `cluster_meta` is seeded. A
/// process crash in that narrow gap, or an acknowledged bootstrap whose reply
/// is lost, must therefore be safely retryable even though v22 contains table
/// rebuilds and `ALTER TABLE ADD COLUMN` statements that are not independently
/// idempotent. The same predicate lets a schema coordinator settle a v21
/// marker after observing that another coordinator already installed v22.
pub(super) const ANALYSIS_COMPONENT_SCHEMA_CURRENT_SQL: &str = r#"
SELECT
    (SELECT COUNT(*) FROM pragma_table_info('cluster_fragment_index_jobs')
      WHERE name IN ('priority','trigger','target_node_id'))
  + (SELECT COUNT(*) FROM pragma_table_info('cluster_fragment_index_jobs')
      WHERE (name = 'cache_key' AND pk = 1)
         OR (name = 'target_node_id' AND pk = 2))
  + (SELECT COUNT(*) FROM pragma_table_info('timeline_annotation_sets')
      WHERE name = 'publication_priority')
  + (SELECT COUNT(*) FROM pragma_table_info('analysis_requests')
      WHERE name IN ('pipeline_version','requested_generation',
                     'expected_predecessor_generation','priority','trigger',
                     'cancel_requested'))
  + (SELECT COUNT(*) FROM sqlite_master
      WHERE type = 'table' AND name IN
        ('analysis_attempts','cluster_fragment_index_heads',
         'analysis_lifecycle_counters')) AS count
"#;

pub(super) async fn analysis_component_schema_is_current(
    client: &hiqlite::Client,
) -> Result<bool, StoreError> {
    validate_sql(ANALYSIS_COMPONENT_SCHEMA_CURRENT_SQL)?;
    let rows = client
        .query_consistent_map::<SchemaCountRow, _>(ANALYSIS_COMPONENT_SCHEMA_CURRENT_SQL, params!())
        .await
        .map_err(database_error)?;
    Ok(rows.len() == 1 && rows[0].0 == 15)
}

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    if analysis_component_schema_is_current(client).await? {
        return Ok(());
    }
    let mut statements = fragment_index_schema_migration_statements()?;
    statements.extend(analysis_request_schema_migration_statements()?);
    statements.extend(analysis_history_index_migration_statements()?);
    statements.extend(analysis_component_migration_statements()?);
    client
        .txn(statements)
        .await
        .map_err(database_error)?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    Ok(())
}

const JOB_COLS: &str = "cache_key, file_id, source_size, source_mtime, source_sha256,
    pipeline_sha256, priority, trigger, target_node_id,
    state, COALESCE(owner_node_id, '') AS owner_node_id, fence,
    COALESCE(lease_expires_ms, 0) AS lease_expires_ms, attempts, not_before_ms,
    created_at_ms, updated_at_ms, COALESCE(last_error_code, '') AS last_error_code";

struct JobRow(ClusterFragmentIndexJob);

impl From<&mut Row<'_>> for JobRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(ClusterFragmentIndexJob {
            cache_key: row.get("cache_key"),
            file_id: row.get("file_id"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            source_sha256: row.get("source_sha256"),
            pipeline_sha256: row.get("pipeline_sha256"),
            priority: row.get("priority"),
            trigger: row.get("trigger"),
            target_node_id: row.get("target_node_id"),
            state: row.get("state"),
            owner_node_id: row.get("owner_node_id"),
            fence: row.get("fence"),
            lease_expires_ms: row.get("lease_expires_ms"),
            attempts: row.get("attempts"),
            not_before_ms: row.get("not_before_ms"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
            last_error_code: row.get("last_error_code"),
        })
    }
}

const REQUEST_COLS: &str = "request_id, file_id, source_size, source_mtime, component,
    pipeline_version, requested_generation, expected_predecessor_generation,
    priority, trigger, force_rebuild,
    target_node_id, state, COALESCE(owner_node_id, '') AS owner_node_id,
    fence, COALESCE(lease_expires_ms, 0) AS lease_expires_ms, attempts, not_before_ms,
    COALESCE(result_cache_key, '') AS result_cache_key,
    COALESCE(last_error_code, '') AS last_error_code, cancel_requested,
    created_at_ms, updated_at_ms";

struct RequestRow(AnalysisRequest);

impl From<&mut Row<'_>> for RequestRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(AnalysisRequest {
            request_id: row.get("request_id"),
            file_id: row.get("file_id"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            component: row.get("component"),
            pipeline_version: row.get("pipeline_version"),
            requested_generation: row.get("requested_generation"),
            expected_predecessor_generation: row.get("expected_predecessor_generation"),
            priority: row.get("priority"),
            trigger: row.get("trigger"),
            force_rebuild: row.get::<i64>("force_rebuild") != 0,
            target_node_id: row.get("target_node_id"),
            state: row.get("state"),
            owner_node_id: row.get("owner_node_id"),
            fence: row.get("fence"),
            lease_expires_ms: row.get("lease_expires_ms"),
            attempts: row.get("attempts"),
            not_before_ms: row.get("not_before_ms"),
            result_cache_key: row.get("result_cache_key"),
            last_error_code: row.get("last_error_code"),
            cancel_requested: row.get::<i64>("cancel_requested") != 0,
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        })
    }
}

struct AttemptRow(AnalysisAttempt);

impl From<&mut Row<'_>> for AttemptRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(AnalysisAttempt {
            request_id: row.get("request_id"),
            attempt: row.get("attempt"),
            claim_node_id: row.get("claim_node_id"),
            claim_epoch: row.get("claim_epoch"),
            claim_expires_at_ms: row.get("claim_expires_at_ms"),
            phase: row.get("phase"),
            started_at_ms: row.get("started_at_ms"),
            phase_updated_at_ms: row.get("phase_updated_at_ms"),
            terminal_code: row.get("terminal_code"),
        })
    }
}

fn valid_analysis_phase(phase: &str) -> bool {
    matches!(
        phase,
        "claimed"
            | "source_probe"
            | "hashing"
            | "staged"
            | "publishing"
            | "retry_wait"
            | "published"
            | "failed"
            | "canceled"
            | "stale"
    )
}

struct LabelRow(AnalysisFileLabel);

impl From<&mut Row<'_>> for LabelRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(AnalysisFileLabel {
            file_id: row.get("file_id"),
            item_id: row.get("item_id"),
            title: row.get("title"),
        })
    }
}

const HISTORY_COLS: &str = "row_key, request_id, job_id, file_id, item_id, title,
    component, force_rebuild, target_node_id, request_state, job_state, state, disposition,
    action, owner_node_id, claim_epoch, lease_expires_ms, attempts, not_before_ms, request_error_code,
    job_error_code, created_at_ms, updated_at_ms, pipeline_version, requested_generation,
    priority, trigger, cancel_requested, phase, source_size";

const HISTORY_PAGE_COLS: &str = "COALESCE(page.row_key, '') AS row_key,
    COALESCE(page.request_id, '') AS request_id, COALESCE(page.job_id, '') AS job_id,
    COALESCE(page.file_id, 0) AS file_id, COALESCE(page.item_id, 0) AS item_id,
    COALESCE(page.title, '') AS title, COALESCE(page.component, '') AS component,
    COALESCE(page.force_rebuild, 0) AS force_rebuild,
    COALESCE(page.target_node_id, '') AS target_node_id,
    COALESCE(page.request_state, '') AS request_state,
    COALESCE(page.job_state, '') AS job_state, COALESCE(page.state, '') AS state,
    COALESCE(page.disposition, '') AS disposition, COALESCE(page.action, 'none') AS action,
    COALESCE(page.owner_node_id, '') AS owner_node_id,
    COALESCE(page.claim_epoch, 0) AS claim_epoch,
    COALESCE(page.lease_expires_ms, 0) AS lease_expires_ms,
    COALESCE(page.attempts, 0) AS attempts, COALESCE(page.not_before_ms, 0) AS not_before_ms,
    COALESCE(page.request_error_code, '') AS request_error_code,
    COALESCE(page.job_error_code, '') AS job_error_code,
    COALESCE(page.created_at_ms, 0) AS created_at_ms,
    COALESCE(page.updated_at_ms, 0) AS updated_at_ms,
    COALESCE(page.pipeline_version, '') AS pipeline_version,
    COALESCE(page.requested_generation, '') AS requested_generation,
    COALESCE(page.priority, '') AS priority, COALESCE(page.trigger, '') AS trigger,
    COALESCE(page.cancel_requested, 0) AS cancel_requested,
    COALESCE(page.phase, '') AS phase,
    COALESCE(page.source_size, 0) AS source_size";

struct HistoryRow(AnalysisHistoryRow, AnalysisHistoryCursor, i64);

impl From<&mut Row<'_>> for HistoryRow {
    fn from(row: &mut Row<'_>) -> Self {
        let history = AnalysisHistoryRow {
            row_key: row.get("row_key"),
            request_id: row.get("request_id"),
            job_id: row.get("job_id"),
            file_id: row.get("file_id"),
            item_id: row.get("item_id"),
            title: row.get("title"),
            component: row.get("component"),
            force_rebuild: row.get::<i64>("force_rebuild") != 0,
            target_node_id: row.get("target_node_id"),
            request_state: row.get("request_state"),
            job_state: row.get("job_state"),
            state: row.get("state"),
            disposition: row.get("disposition"),
            action: row.get("action"),
            owner_node_id: row.get("owner_node_id"),
            claim_epoch: row.get("claim_epoch"),
            lease_expires_ms: row.get("lease_expires_ms"),
            attempts: row.get("attempts"),
            not_before_ms: row.get("not_before_ms"),
            request_error_code: row.get("request_error_code"),
            job_error_code: row.get("job_error_code"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
            pipeline_version: row.get("pipeline_version"),
            requested_generation: row.get("requested_generation"),
            priority: row.get("priority"),
            trigger: row.get("trigger"),
            cancel_requested: row.get::<i64>("cancel_requested") != 0,
            phase: row.get("phase"),
            source_size: row.get("source_size"),
        };
        let cursor = AnalysisHistoryCursor {
            sort_rank: row.get("sort_rank"),
            updated_at_ms: history.updated_at_ms,
            row_key: history.row_key.clone(),
        };
        Self(history, cursor, row.get("filtered_total"))
    }
}

struct StatusSummaryRow(AnalysisStatusSummary);

impl From<&mut Row<'_>> for StatusSummaryRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(AnalysisStatusSummary {
            total: row.get("total"),
            working: row.get("working"),
            queued: row.get("queued"),
            running: row.get("running"),
            submitted: row.get("submitted"),
            attention: row.get("attention"),
            expected: row.get("expected"),
            ready: row.get("ready"),
            latest_error_code: row.get("latest_error_code"),
            latest_error_file_id: row.get("latest_error_file_id"),
            latest_error_updated_at_ms: row.get("latest_error_updated_at_ms"),
        })
    }
}

fn analysis_filter_code(filter: AnalysisHistoryFilter) -> i64 {
    match filter {
        AnalysisHistoryFilter::All => 0,
        AnalysisHistoryFilter::Working => 1,
        AnalysisHistoryFilter::Attention => 2,
        AnalysisHistoryFilter::Ready => 3,
        AnalysisHistoryFilter::Expected => 4,
    }
}

fn analysis_search_pattern(search: &str) -> String {
    let escaped = search
        .trim()
        .to_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    if escaped.is_empty() {
        String::new()
    } else {
        format!("%{escaped}%")
    }
}

fn analysis_state_pattern(states: &[String]) -> String {
    if states.is_empty() {
        String::new()
    } else {
        format!(",{},", states.join(","))
    }
}

struct SourceRow(FragmentIndexSourceObservation);

impl From<&mut Row<'_>> for SourceRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(FragmentIndexSourceObservation {
            node_id: row.get("node_id"),
            file_id: row.get("file_id"),
            object_version: row.get("object_version"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            source_sha256: row.get("source_sha256"),
            observed_at_ms: row.get("observed_at_ms"),
        })
    }
}

struct ArtifactRow(ClusterFragmentIndexArtifact);

impl From<&mut Row<'_>> for ArtifactRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(ClusterFragmentIndexArtifact {
            cache_key: row.get("cache_key"),
            file_id: row.get("file_id"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            source_sha256: row.get("source_sha256"),
            pipeline_sha256: row.get("pipeline_sha256"),
            blob_sha256: row.get("blob_sha256"),
            bytes: row.get("bytes"),
            built_by_node_id: row.get("built_by_node_id"),
            built_at_ms: row.get("built_at_ms"),
        })
    }
}

struct LocationRow(ClusterFragmentIndexLocation);

impl From<&mut Row<'_>> for LocationRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(ClusterFragmentIndexLocation {
            cache_key: row.get("cache_key"),
            node_id: row.get("node_id"),
            bytes: row.get("bytes"),
            verified_at_ms: row.get("verified_at_ms"),
            last_seen_at_ms: row.get("last_seen_at_ms"),
        })
    }
}

struct CacheKeyRow(String);

impl From<&mut Row<'_>> for CacheKeyRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("cache_key"))
    }
}

fn valid_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_job(job: &NewClusterFragmentIndexJob) -> bool {
    job.cache_key.len() == 64
        && valid_hex_digest(&job.cache_key)
        && job.file_id > 0
        && job.source_size >= 0
        && valid_hex_digest(&job.source_sha256)
        && valid_hex_digest(&job.pipeline_sha256)
        && matches!(job.priority.as_str(), "normal" | "forced" | "foreground")
        && matches!(job.trigger.as_str(), "admin" | "background" | "foreground")
        && job.target_node_id.len() <= 128
}

fn valid_request(request: &NewAnalysisRequest) -> bool {
    !request.request_id.is_empty()
        && request.request_id.len() <= 64
        && request.file_id > 0
        && request.source_size >= 0
        && matches!(
            request.component.as_str(),
            "fragment_index" | "skip_markers"
        )
        && !request.pipeline_version.is_empty()
        && request.pipeline_version.len() <= 128
        && !request.requested_generation.is_empty()
        && request.requested_generation.len() <= 128
        && matches!(request.priority.as_str(), "normal" | "forced")
        && matches!(request.trigger.as_str(), "admin" | "background")
        && (request.force_rebuild == (request.priority == "forced"))
        && request.target_node_id.len() <= 128
        && ((request.component == "skip_markers" && request.target_node_id.is_empty())
            || (request.component == "fragment_index" && !request.target_node_id.is_empty()))
}

#[async_trait]
impl ClusterFragmentIndexStore for HiqliteAuthStore {
    async fn enqueue_analysis_request(
        &self,
        request: &NewAnalysisRequest,
    ) -> Result<AnalysisRequest, StoreError> {
        if !valid_request(request) {
            return Err(StoreError::Task("invalid analysis request".to_owned()));
        }
        let semantic_fingerprint = crate::segplan::argv_fingerprint(&[
            "timeline-annotations".to_owned(),
            request.pipeline_version.clone(),
        ]);
        self.client()
            .txn(vec![
                (
                    "INSERT INTO analysis_requests
                (request_id, file_id, source_size, source_mtime, component,
                 pipeline_version, requested_generation, expected_predecessor_generation,
                 priority, trigger,
                 force_rebuild, target_node_id, state, owner_node_id, fence,
                 lease_expires_ms, attempts, not_before_ms, result_cache_key,
                 last_error_code, cancel_requested, created_at_ms, updated_at_ms)
             SELECT $1, $2, $3, $4, $5, $6, $7,
                    CASE WHEN $5 = 'skip_markers' THEN COALESCE((
                      SELECT generation_id FROM timeline_annotation_sets
                       WHERE file_id = $2 AND source_size = $3 AND source_mtime = $4
                    ), '') ELSE '' END,
                    $8, $9, $10, $11,
                    CASE WHEN $10 = 0 AND $5 = 'skip_markers' AND EXISTS (
                      SELECT 1 FROM timeline_annotation_sets
                       WHERE file_id = $2 AND source_size = $3 AND source_mtime = $4
                         AND argv_fingerprint = $12 AND publication_priority = 'forced'
                    ) THEN 'ready' ELSE 'queued' END,
                    NULL, 0, NULL, 0, $13,
                    CASE WHEN $10 = 0 AND $5 = 'skip_markers' THEN (
                      SELECT generation_id FROM timeline_annotation_sets
                       WHERE file_id = $2 AND source_size = $3 AND source_mtime = $4
                         AND argv_fingerprint = $12 AND publication_priority = 'forced'
                    ) ELSE NULL END,
                    NULL, 0, $14, $14
              WHERE EXISTS (SELECT 1 FROM files WHERE id = $2 AND size = $3 AND mtime = $4)
                AND (SELECT COUNT(*) FROM analysis_requests
                      WHERE state IN ('queued', 'running', 'submitted')) < $15
                AND ($10 = 1 OR (
                  NOT EXISTS (SELECT 1 FROM analysis_requests
                    WHERE file_id = $2 AND source_size = $3 AND source_mtime = $4
                      AND component = $5 AND force_rebuild = 1
                      AND target_node_id = $11
                      AND state IN ('queued', 'running', 'submitted', 'ready'))
                  AND NOT EXISTS (SELECT 1 FROM analysis_requests
                    WHERE file_id = $2 AND source_size = $3 AND source_mtime = $4
                      AND component = $5 AND pipeline_version = $6
                      AND requested_generation = $7 AND target_node_id = $11)
                ))
             ON CONFLICT DO NOTHING"
                        .to_owned(),
                    params!(
                        &request.request_id,
                        request.file_id,
                        request.source_size,
                        request.source_mtime,
                        &request.component,
                        &request.pipeline_version,
                        &request.requested_generation,
                        &request.priority,
                        &request.trigger,
                        if request.force_rebuild { 1_i64 } else { 0_i64 },
                        &request.target_node_id,
                        semantic_fingerprint,
                        request.not_before_ms,
                        request.created_at_ms,
                        MAX_ANALYSIS_REQUESTS
                    ),
                ),
                (
                    "UPDATE analysis_attempts
                        SET phase = 'canceled', phase_updated_at_ms = $1,
                            terminal_code = 'publication_superseded'
                      WHERE $2 = 1 AND (request_id, claim_epoch) IN (
                        SELECT older.request_id, older.fence FROM analysis_requests older
                         WHERE older.file_id = $3 AND older.source_size = $4
                           AND older.source_mtime = $5 AND older.component = $6
                           AND older.pipeline_version = $7 AND older.force_rebuild = 0
                           AND older.state IN ('queued','running','submitted')
                           AND EXISTS (SELECT 1 FROM analysis_requests successor
                             WHERE successor.request_id = $8 AND successor.force_rebuild = 1))"
                        .to_owned(),
                    params!(
                        request.created_at_ms,
                        if request.force_rebuild { 1_i64 } else { 0_i64 },
                        request.file_id,
                        request.source_size,
                        request.source_mtime,
                        &request.component,
                        &request.pipeline_version,
                        &request.request_id
                    ),
                ),
                (
                    "UPDATE analysis_requests
                        SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                            fence = fence + 1, cancel_requested = 1,
                            last_error_code = 'publication_superseded', updated_at_ms = $1
                      WHERE $2 = 1 AND file_id = $3 AND source_size = $4
                        AND source_mtime = $5 AND component = $6 AND pipeline_version = $7
                        AND force_rebuild = 0 AND state IN ('queued','running','submitted')
                        AND EXISTS (SELECT 1 FROM analysis_requests successor
                          WHERE successor.request_id = $8 AND successor.force_rebuild = 1)"
                        .to_owned(),
                    params!(
                        request.created_at_ms,
                        if request.force_rebuild { 1_i64 } else { 0_i64 },
                        request.file_id,
                        request.source_size,
                        request.source_mtime,
                        &request.component,
                        &request.pipeline_version,
                        &request.request_id
                    ),
                ),
                (
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'cancelled', owner_node_id = NULL,
                            lease_expires_ms = NULL, fence = fence + 1,
                            last_error_code = 'publication_superseded', updated_at_ms = $1
                      WHERE state IN ('queued','running')
                        AND (cache_key, target_node_id) IN (
                          SELECT older.result_cache_key, older.target_node_id
                            FROM analysis_requests older
                           WHERE older.file_id = $2 AND older.source_size = $3
                             AND older.source_mtime = $4 AND older.component = $5
                             AND older.pipeline_version = $6 AND older.force_rebuild = 0
                             AND older.state = 'cancelled'
                             AND older.last_error_code = 'publication_superseded'
                             AND older.updated_at_ms = $1)
                        AND NOT EXISTS (SELECT 1 FROM analysis_requests active
                          WHERE active.result_cache_key = cluster_fragment_index_jobs.cache_key
                            AND active.target_node_id = cluster_fragment_index_jobs.target_node_id
                            AND active.state IN ('queued','running','submitted'))"
                        .to_owned(),
                    params!(
                        request.created_at_ms,
                        request.file_id,
                        request.source_size,
                        request.source_mtime,
                        &request.component,
                        &request.pipeline_version
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        self.client()
            .query_consistent_map::<RequestRow, _>(
                format!(
                    "SELECT {REQUEST_COLS} FROM analysis_requests
                      WHERE file_id = $1 AND source_size = $2 AND source_mtime = $3
                        AND component = $4
                        AND ((pipeline_version = $5 AND requested_generation = $6
                              AND target_node_id = $7)
                          OR (force_rebuild = 1 AND pipeline_version = $5
                            AND target_node_id = $7
                            AND state IN ('queued', 'running', 'submitted', 'ready')))
                        AND (request_id = $8
                          OR state IN ('queued', 'running', 'submitted')
                          OR ($9 = 0 AND requested_generation = $6)
                          OR ($9 = 0 AND force_rebuild = 1 AND state = 'ready'
                            AND pipeline_version = $5 AND target_node_id = $7))
                      ORDER BY CASE WHEN request_id = $8 THEN 0
                          WHEN force_rebuild = 1
                            AND state IN ('queued', 'running', 'submitted') THEN 1
                          WHEN state IN ('queued', 'running', 'submitted') THEN 2
                          ELSE 3 END,
                        created_at_ms DESC, request_id DESC LIMIT 1"
                ),
                params!(
                    request.file_id,
                    request.source_size,
                    request.source_mtime,
                    &request.component,
                    &request.pipeline_version,
                    &request.requested_generation,
                    &request.target_node_id,
                    &request.request_id,
                    if request.force_rebuild { 1_i64 } else { 0_i64 }
                ),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0)
            .ok_or_else(|| {
                StoreError::Task(
                    "analysis source changed or the active request queue is full".to_owned(),
                )
            })
    }

    async fn claim_analysis_request(
        &self,
        node_id: &str,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        if node_id.is_empty() || node_id.len() > 128 || lease_expires_ms <= now_ms {
            return Err(StoreError::Task("invalid analysis claim".to_owned()));
        }
        let max_attempts = configured_max_attempts(self).await?;
        let backoff_base_ms = configured_backoff_base_ms(self).await?;
        let backoff_max_ms = configured_backoff_max_ms(self).await?.max(backoff_base_ms);
        self.client()
            .txn(vec![
                (
                    "UPDATE analysis_attempts
                        SET phase = 'retry_wait', phase_updated_at_ms = $1,
                            terminal_code = 'lease_expired'
                      WHERE (request_id, claim_epoch) IN (
                        SELECT request_id, fence FROM analysis_requests
                         WHERE state = 'running'
                           AND COALESCE(lease_expires_ms, 0) <= $1)"
                        .to_owned(),
                    params!(now_ms),
                ),
                (
                    "UPDATE analysis_requests
                        SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                            not_before_ms = $1 + MIN(MAX(1000,
                              ($2 * (1 << MIN(MAX(attempts - 1, 0), 30)) *
                               (75 + (ABS(length(request_id) * 17
                                 + unicode(substr(request_id, 1, 1)) * 31
                                 + unicode(substr(request_id, -1, 1)) * 13
                                 + attempts * 7) % 51))) / 100), $3),
                            last_error_code = 'lease_expired',
                            updated_at_ms = $1
                      WHERE state = 'running' AND COALESCE(lease_expires_ms, 0) <= $1"
                        .to_owned(),
                    params!(now_ms, backoff_base_ms, backoff_max_ms),
                ),
                (
                    "UPDATE analysis_attempts
                        SET phase = 'failed', phase_updated_at_ms = $1,
                            terminal_code = 'attempt_limit'
                      WHERE (request_id, claim_epoch) IN (
                        SELECT request_id, fence FROM analysis_requests
                         WHERE attempts >= $2 AND state = 'queued' AND not_before_ms <= $1)"
                        .to_owned(),
                    params!(now_ms, max_attempts),
                ),
                (
                    "UPDATE analysis_requests
                        SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                            last_error_code = 'attempt_limit', updated_at_ms = $1
                      WHERE attempts >= $2 AND state = 'queued' AND not_before_ms <= $1"
                        .to_owned(),
                    params!(now_ms, max_attempts),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        for _ in 0..8 {
            let candidate = self
                .client()
                .query_consistent_map::<RequestRow, _>(
                    format!(
                        "SELECT {REQUEST_COLS} FROM analysis_requests
                          WHERE (target_node_id = $1
                              OR (component = 'skip_markers' AND target_node_id = ''))
                            AND attempts < $2
                            AND state = 'queued' AND not_before_ms <= $3
                          ORDER BY created_at_ms - CASE WHEN priority = 'forced'
                                     THEN $4 ELSE 0 END,
                                   created_at_ms, request_id LIMIT 1"
                    ),
                    params!(
                        node_id,
                        max_attempts,
                        now_ms,
                        super::fragment_index_cluster::ANALYSIS_FORCED_PRIORITY_BOOST_MS
                    ),
                )
                .await?
                .into_iter()
                .next()
                .map(|row| row.0);
            let Some(mut candidate) = candidate else {
                return Ok(None);
            };
            let results = self
                .client()
                .txn(vec![
                    (
                        "UPDATE analysis_requests
                        SET state = 'running', owner_node_id = $1, fence = fence + 1,
                            lease_expires_ms = $2, attempts = attempts + 1,
                            last_error_code = NULL, updated_at_ms = $3
                      WHERE request_id = $4 AND fence = $5
                        AND state = 'queued' AND not_before_ms <= $3"
                            .to_owned(),
                        params!(
                            node_id,
                            lease_expires_ms,
                            now_ms,
                            &candidate.request_id,
                            candidate.fence
                        ),
                    ),
                    (
                        "INSERT INTO analysis_attempts
                            (request_id, attempt, claim_node_id, claim_epoch,
                             claim_expires_at_ms, phase, started_at_ms,
                             phase_updated_at_ms, terminal_code)
                         SELECT $1, attempts, $2, fence, $3, 'claimed', $4, $4, NULL
                           FROM analysis_requests
                          WHERE request_id = $1 AND state = 'running'
                            AND owner_node_id = $2 AND fence = $5 AND updated_at_ms = $4"
                            .to_owned(),
                        params!(
                            &candidate.request_id,
                            node_id,
                            lease_expires_ms,
                            now_ms,
                            candidate.fence + 1
                        ),
                    ),
                    (
                        "DELETE FROM analysis_attempts
                          WHERE request_id = $1 AND claim_epoch NOT IN (
                            SELECT claim_epoch FROM analysis_attempts
                             WHERE request_id = $1 ORDER BY claim_epoch DESC LIMIT $2)"
                            .to_owned(),
                        params!(&candidate.request_id, MAX_ATTEMPT_HISTORY_PER_REQUEST),
                    ),
                ])
                .await?
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .map_err(database_error)?;
            if results.first() == Some(&1) && results.get(1) == Some(&1) {
                candidate.state = "running".to_owned();
                candidate.owner_node_id = node_id.to_owned();
                candidate.fence += 1;
                candidate.lease_expires_ms = lease_expires_ms;
                candidate.attempts += 1;
                candidate.updated_at_ms = now_ms;
                candidate.last_error_code.clear();
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    async fn renew_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError> {
        let results = self
            .client()
            .txn(vec![
                (
                    "UPDATE analysis_requests SET lease_expires_ms = $1, updated_at_ms = $2
                  WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2 AND $1 > $2"
                        .to_owned(),
                    params!(lease_expires_ms, now_ms, request_id, node_id, fence),
                ),
                (
                    "UPDATE analysis_attempts
                        SET claim_expires_at_ms = $1, phase_updated_at_ms = $2
                      WHERE request_id = $3 AND claim_epoch = $4
                        AND EXISTS (SELECT 1 FROM analysis_requests
                              WHERE request_id = $3 AND state = 'running'
                                AND owner_node_id = $5 AND fence = $4
                                AND lease_expires_ms = $1 AND updated_at_ms = $2)"
                        .to_owned(),
                    params!(lease_expires_ms, now_ms, request_id, fence, node_id),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.first() == Some(&1))
    }

    async fn record_analysis_request_phase(
        &self,
        request: &AnalysisRequest,
        phase: &str,
        terminal_code: Option<&str>,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if !valid_analysis_phase(phase)
            || terminal_code
                .is_some_and(|code| code.is_empty() || code.len() > MAX_ERROR_CODE_BYTES)
        {
            return Err(StoreError::Task("invalid analysis phase".to_owned()));
        }
        Ok(self
            .execute(
                "UPDATE analysis_attempts
                    SET phase = $1, phase_updated_at_ms = $2, terminal_code = $3
                  WHERE request_id = $4 AND attempt = $5 AND claim_epoch = $6
                    AND claim_node_id = $7
                    AND EXISTS (SELECT 1 FROM analysis_requests
                          WHERE request_id = $4 AND state = 'running'
                            AND owner_node_id = $7 AND fence = $6
                            AND attempts = $5 AND lease_expires_ms > $2)",
                params!(
                    phase,
                    now_ms,
                    terminal_code,
                    &request.request_id,
                    request.attempts,
                    request.fence,
                    &request.owner_node_id
                ),
            )
            .await?
            == 1)
    }

    async fn analysis_attempts(
        &self,
        request_id: &str,
        limit: i64,
    ) -> Result<Vec<AnalysisAttempt>, StoreError> {
        let limit = limit.clamp(1, 100);
        Ok(self
            .client()
            .query_consistent_map::<AttemptRow, _>(
                "SELECT request_id, attempt, claim_node_id, claim_epoch,
                        claim_expires_at_ms, phase, started_at_ms,
                        phase_updated_at_ms, COALESCE(terminal_code, '') AS terminal_code
                   FROM analysis_attempts WHERE request_id = $1
                  ORDER BY claim_epoch DESC LIMIT $2",
                params!(request_id, limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn submit_fragment_index_analysis(
        &self,
        request: &AnalysisRequest,
        job: &NewClusterFragmentIndexJob,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if !valid_job(job)
            || request.state != "running"
            || request.owner_node_id.is_empty()
            || request.file_id != job.file_id
            || request.source_size != job.source_size
            || request.source_mtime != job.source_mtime
            || request.priority != job.priority
            || request.trigger != job.trigger
            || request.target_node_id != job.target_node_id
        {
            return Err(StoreError::Task("invalid analysis submission".to_owned()));
        }
        let logical_cache_key = cluster_fragment_index_key(
            job.file_id,
            job.source_size,
            job.source_mtime,
            &job.source_sha256,
            &job.pipeline_sha256,
        )
        .ok_or_else(|| StoreError::Task("invalid analysis logical cache key".to_owned()))?;
        if request.force_rebuild {
            let expected_cache_key = cluster_fragment_index_generation_key(
                job.file_id,
                job.source_size,
                job.source_mtime,
                &job.source_sha256,
                &job.pipeline_sha256,
                &request.requested_generation,
            )
            .ok_or_else(|| StoreError::Task("invalid analysis generation key".to_owned()))?;
            if job.cache_key != expected_cache_key {
                return Err(StoreError::Task(
                    "analysis submission cache identity mismatch".to_owned(),
                ));
            }
        }
        let max_attempts = configured_max_attempts(self).await?;
        let results = self
            .client()
            .txn(vec![
                (
                    "UPDATE analysis_requests
                        SET state = 'submitted', lease_expires_ms = NULL,
                            result_cache_key = $1,
                            expected_predecessor_generation = COALESCE((
                              SELECT generation_cache_key FROM cluster_fragment_index_heads
                               WHERE logical_cache_key = $2
                            ), ''),
                            last_error_code = NULL, updated_at_ms = $3
                      WHERE request_id = $4 AND state = 'running' AND owner_node_id = $5
                        AND fence = $6 AND lease_expires_ms > $3
                        AND file_id = $7 AND source_size = $8 AND source_mtime = $9
                        AND EXISTS (SELECT 1 FROM files
                              WHERE id = $7 AND size = $8 AND mtime = $9)
                        AND ($10 = 1 OR $1 = COALESCE((
                          SELECT generation_cache_key FROM cluster_fragment_index_heads
                           WHERE logical_cache_key = $2), $2))
                        AND (
                          ($10 = 1
                            AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                                  WHERE state IN ('queued', 'running')) < $11
                            AND (NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                              WHERE cache_key = $1 AND target_node_id = $12)
                              OR EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                          WHERE cache_key = $1 AND target_node_id = $12
                                            AND state IN ('ready', 'failed', 'cancelled'))))
                          OR
                          ($10 = 0 AND (
                            EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                     WHERE cache_key = $1 AND target_node_id = $12
                                       AND state IN ('queued', 'running', 'ready'))
                            OR ((SELECT COUNT(*) FROM cluster_fragment_index_jobs
                                    WHERE state IN ('queued', 'running')) < $11
                              AND (NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                                WHERE cache_key = $1 AND target_node_id = $12)
                                OR EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                            WHERE cache_key = $1 AND target_node_id = $12 AND (
                                              state = 'cancelled'
                                              OR (state = 'failed'
                                                AND last_error_code = 'queue_expired'
                                                AND attempts <= $13))))))))"
                        .to_owned(),
                    params!(
                        &job.cache_key,
                        &logical_cache_key,
                        now_ms,
                        &request.request_id,
                        &request.owner_node_id,
                        request.fence,
                        job.file_id,
                        job.source_size,
                        job.source_mtime,
                        if request.force_rebuild { 1_i64 } else { 0_i64 },
                        MAX_ACTIVE_JOBS,
                        &job.target_node_id,
                        max_attempts
                    ),
                ),
                (
                    "INSERT INTO cluster_fragment_index_jobs
                        (cache_key, file_id, source_size, source_mtime, source_sha256,
                         pipeline_sha256, priority, trigger, target_node_id,
                         state, owner_node_id, fence, lease_expires_ms,
                         attempts, not_before_ms, created_at_ms, updated_at_ms, last_error_code)
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9,
                            'queued', NULL, 0, NULL, 0, $10, $11, $11, NULL
                      WHERE EXISTS (SELECT 1 FROM analysis_requests
                        WHERE request_id = $12 AND state = 'submitted' AND fence = $13
                          AND file_id = $2 AND source_size = $3 AND source_mtime = $4
                          AND result_cache_key = $1 AND updated_at_ms = $11
                          AND owner_node_id = $14 AND target_node_id = $9)
                     ON CONFLICT(cache_key, target_node_id) DO UPDATE SET
                        file_id = excluded.file_id, source_size = excluded.source_size,
                        source_mtime = excluded.source_mtime,
                        source_sha256 = excluded.source_sha256,
                        pipeline_sha256 = excluded.pipeline_sha256,
                        priority = excluded.priority, trigger = excluded.trigger,
                        state = 'queued',
                        owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = CASE WHEN $15 = 1 THEN 0
                          WHEN cluster_fragment_index_jobs.state = 'cancelled'
                            OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                            OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN 0
                          ELSE cluster_fragment_index_jobs.attempts END,
                        not_before_ms = excluded.not_before_ms,
                        created_at_ms = excluded.created_at_ms,
                        updated_at_ms = excluded.updated_at_ms, last_error_code = NULL
                      WHERE ($15 = 1 AND cluster_fragment_index_jobs.state
                                          IN ('ready', 'failed', 'cancelled'))
                         OR ($15 = 0 AND (
                           cluster_fragment_index_jobs.state = 'cancelled'
                           OR (cluster_fragment_index_jobs.state = 'failed'
                             AND cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                             AND cluster_fragment_index_jobs.attempts <= $16)))"
                        .to_owned(),
                    params!(
                        &job.cache_key,
                        job.file_id,
                        job.source_size,
                        job.source_mtime,
                        &job.source_sha256,
                        &job.pipeline_sha256,
                        &job.priority,
                        &job.trigger,
                        &job.target_node_id,
                        job.not_before_ms,
                        now_ms,
                        &request.request_id,
                        request.fence,
                        &request.owner_node_id,
                        if request.force_rebuild { 1_i64 } else { 0_i64 },
                        max_attempts
                    ),
                ),
                (
                    "INSERT INTO cluster_fragment_index_heads
                        (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                     SELECT $1, $2, $3, $4
                      WHERE $1 = $2
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                              WHERE cache_key = $2 AND target_node_id = $5 AND state = 'ready')
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                              WHERE cache_key = $2)
                     ON CONFLICT(logical_cache_key) DO NOTHING"
                        .to_owned(),
                    params!(
                        &logical_cache_key,
                        &job.cache_key,
                        &request.request_id,
                        now_ms,
                        &job.target_node_id
                    ),
                ),
                (
                    "UPDATE analysis_attempts
                        SET phase = 'staged', phase_updated_at_ms = $1,
                            terminal_code = NULL
                      WHERE request_id = $2 AND claim_epoch = $3
                        AND EXISTS (SELECT 1 FROM analysis_requests
                              WHERE request_id = $2 AND state = 'submitted'
                                AND fence = $3 AND owner_node_id = $4
                                AND updated_at_ms = $1)"
                        .to_owned(),
                    params!(
                        now_ms,
                        &request.request_id,
                        request.fence,
                        &request.owner_node_id
                    ),
                ),
                (
                    "UPDATE analysis_requests SET owner_node_id = NULL
                      WHERE request_id = $1 AND state = 'submitted'
                        AND owner_node_id = $2 AND fence = $3
                        AND file_id = $4 AND source_size = $5 AND source_mtime = $6
                        AND result_cache_key = $7 AND updated_at_ms = $8"
                        .to_owned(),
                    params!(
                        &request.request_id,
                        &request.owner_node_id,
                        request.fence,
                        job.file_id,
                        job.source_size,
                        job.source_mtime,
                        &job.cache_key,
                        now_ms
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        match (
            results.first().copied(),
            results.get(3).copied(),
            results.get(4).copied(),
        ) {
            (Some(1), Some(1), Some(1)) => Ok(true),
            (Some(0), Some(0), Some(0)) => Ok(false),
            _ => Err(StoreError::Task(
                "analysis handoff token was not consumed atomically".to_owned(),
            )),
        }
    }

    async fn retry_analysis_request(
        &self,
        request: &AnalysisRequest,
        error_code: &str,
        now_ms: i64,
        retry_at_ms: i64,
        charge_attempt: bool,
    ) -> Result<bool, StoreError> {
        if request.state != "running"
            || request.owner_node_id.is_empty()
            || error_code.is_empty()
            || error_code.len() > MAX_ERROR_CODE_BYTES
            || retry_at_ms <= now_ms
        {
            return Err(StoreError::Task("invalid analysis retry".to_owned()));
        }
        let results = self
            .client()
            .txn(vec![
                (
                    "UPDATE analysis_requests
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = CASE WHEN $1 = 0 AND attempts > 0
                          THEN attempts - 1 ELSE attempts END,
                        not_before_ms = $2, last_error_code = $3, updated_at_ms = $4
                  WHERE request_id = $5 AND state = 'running' AND owner_node_id = $6
                    AND fence = $7 AND lease_expires_ms > $4"
                        .to_owned(),
                    params!(
                        if charge_attempt { 1_i64 } else { 0_i64 },
                        retry_at_ms,
                        error_code,
                        now_ms,
                        &request.request_id,
                        &request.owner_node_id,
                        request.fence
                    ),
                ),
                (
                    "UPDATE analysis_attempts
                        SET phase = 'retry_wait', phase_updated_at_ms = $1,
                            terminal_code = $2
                      WHERE request_id = $3 AND claim_epoch = $4
                        AND EXISTS (SELECT 1 FROM analysis_requests
                              WHERE request_id = $3 AND state = 'queued'
                                AND fence = $4 AND updated_at_ms = $1)"
                        .to_owned(),
                    params!(now_ms, error_code, &request.request_id, request.fence),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.first() == Some(&1))
    }

    async fn fail_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        error_code: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if error_code.is_empty() || error_code.len() > MAX_ERROR_CODE_BYTES {
            return Err(StoreError::Task("invalid analysis failure code".to_owned()));
        }
        let results = self
            .client()
            .txn(vec![
                (
                    "UPDATE analysis_requests
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = $1, updated_at_ms = $2
                  WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2"
                        .to_owned(),
                    params!(error_code, now_ms, request_id, node_id, fence),
                ),
                (
                    "UPDATE analysis_attempts
                        SET phase = 'failed', phase_updated_at_ms = $1,
                            terminal_code = $2
                      WHERE request_id = $3 AND claim_epoch = $4
                        AND EXISTS (SELECT 1 FROM analysis_requests
                              WHERE request_id = $3 AND state = 'failed'
                                AND fence = $4 AND updated_at_ms = $1)"
                        .to_owned(),
                    params!(now_ms, error_code, request_id, fence),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.first() == Some(&1))
    }

    async fn settle_analysis_requests(&self, now_ms: i64) -> Result<u64, StoreError> {
        let results = self
            .client()
            .txn(vec![
                (
                    "UPDATE analysis_attempts
                        SET phase = COALESCE((SELECT CASE job.state
                                WHEN 'ready' THEN CASE WHEN EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_heads head
                                   WHERE head.generation_cache_key = request.result_cache_key
                                ) AND EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_locations location
                                   WHERE location.cache_key = request.result_cache_key
                                     AND location.node_id = request.target_node_id
                                ) THEN 'published' WHEN NOT EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_heads head
                                   WHERE head.generation_cache_key = request.result_cache_key
                                ) THEN 'failed' ELSE 'retry_wait' END
                                WHEN 'failed' THEN 'failed'
                                WHEN 'cancelled' THEN 'canceled' END
                              FROM analysis_requests request
                              JOIN cluster_fragment_index_jobs job
                                ON job.cache_key = request.result_cache_key
                               AND job.target_node_id = request.target_node_id
                             WHERE request.request_id = analysis_attempts.request_id), phase),
                            terminal_code = (SELECT CASE WHEN job.state = 'ready' AND NOT EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_heads head
                                   WHERE head.generation_cache_key = request.result_cache_key
                                ) THEN 'publication_superseded' WHEN job.state = 'ready' AND NOT EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_locations location
                                   WHERE location.cache_key = request.result_cache_key
                                     AND location.node_id = request.target_node_id
                                ) THEN 'target_coverage_missing' ELSE job.last_error_code END
                              FROM analysis_requests request
                              JOIN cluster_fragment_index_jobs job
                                ON job.cache_key = request.result_cache_key
                               AND job.target_node_id = request.target_node_id
                             WHERE request.request_id = analysis_attempts.request_id),
                            phase_updated_at_ms = $1
                      WHERE (request_id, claim_epoch) IN (
                        SELECT request.request_id, request.fence
                          FROM analysis_requests request
                          JOIN cluster_fragment_index_jobs job
                            ON job.cache_key = request.result_cache_key
                           AND job.target_node_id = request.target_node_id
                         WHERE request.state = 'submitted'
                           AND job.state IN ('ready', 'failed', 'cancelled'))"
                        .to_owned(),
                    params!(now_ms),
                ),
                (
                    "UPDATE analysis_requests
                        SET state = COALESCE((SELECT CASE job.state
                                WHEN 'ready' THEN CASE WHEN EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_heads head
                                   WHERE head.generation_cache_key = analysis_requests.result_cache_key
                                ) AND EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_locations location
                                   WHERE location.cache_key = analysis_requests.result_cache_key
                                     AND location.node_id = analysis_requests.target_node_id
                                ) THEN 'ready' WHEN NOT EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_heads head
                                   WHERE head.generation_cache_key = analysis_requests.result_cache_key
                                ) THEN 'failed' ELSE 'queued' END
                                WHEN 'failed' THEN 'failed'
                                WHEN 'cancelled' THEN 'cancelled' ELSE analysis_requests.state END
                              FROM cluster_fragment_index_jobs job
                             WHERE job.cache_key = analysis_requests.result_cache_key
                               AND job.target_node_id = analysis_requests.target_node_id), state),
                            owner_node_id = CASE WHEN EXISTS (SELECT 1 FROM cluster_fragment_index_jobs job
                              WHERE job.cache_key = analysis_requests.result_cache_key
                                AND job.target_node_id = analysis_requests.target_node_id
                                AND job.state = 'ready')
                              THEN NULL ELSE owner_node_id END,
                            lease_expires_ms = CASE WHEN EXISTS (SELECT 1 FROM cluster_fragment_index_jobs job
                              WHERE job.cache_key = analysis_requests.result_cache_key
                                AND job.target_node_id = analysis_requests.target_node_id
                                AND job.state = 'ready')
                              THEN NULL ELSE lease_expires_ms END,
                            not_before_ms = CASE WHEN EXISTS (SELECT 1 FROM cluster_fragment_index_jobs job
                              WHERE job.cache_key = analysis_requests.result_cache_key
                                AND job.target_node_id = analysis_requests.target_node_id
                                AND job.state = 'ready')
                              THEN $1 + 1000 ELSE not_before_ms END,
                            last_error_code = (SELECT CASE WHEN job.state = 'ready' AND NOT EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_heads head
                                   WHERE head.generation_cache_key = analysis_requests.result_cache_key
                                ) THEN 'publication_superseded' WHEN job.state = 'ready' AND NOT EXISTS (
                                  SELECT 1 FROM cluster_fragment_index_locations location
                                   WHERE location.cache_key = analysis_requests.result_cache_key
                                     AND location.node_id = analysis_requests.target_node_id
                                ) THEN 'target_coverage_missing' ELSE job.last_error_code END
                              FROM cluster_fragment_index_jobs job
                             WHERE job.cache_key = analysis_requests.result_cache_key
                               AND job.target_node_id = analysis_requests.target_node_id),
                            updated_at_ms = $1
                      WHERE state = 'submitted'
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs job
                             WHERE job.cache_key = analysis_requests.result_cache_key
                               AND job.target_node_id = analysis_requests.target_node_id
                               AND job.state IN ('ready', 'failed', 'cancelled'))"
                        .to_owned(),
                    params!(now_ms),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.get(1).copied().unwrap_or_default() as u64)
    }

    async fn prune_analysis_requests(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<u64, StoreError> {
        let limit = limit.clamp(1, MAX_ANALYSIS_REQUESTS);
        Ok(self
            .execute(
                "WITH ranked AS (
                   SELECT request_id, updated_at_ms, file_id, source_size, source_mtime,
                          force_rebuild,
                          ROW_NUMBER() OVER (
                            PARTITION BY file_id, component, target_node_id
                            ORDER BY updated_at_ms DESC, request_id DESC) AS generation_rank,
                          ROW_NUMBER() OVER (
                            PARTITION BY file_id, component, target_node_id, force_rebuild
                            ORDER BY updated_at_ms DESC, request_id DESC) AS priority_generation_rank,
                          ROW_NUMBER() OVER (
                            PARTITION BY file_id, source_size, source_mtime, component,
                                         pipeline_version, requested_generation,
                                         target_node_id, force_rebuild
                            ORDER BY updated_at_ms DESC, request_id DESC) AS identity_rank
                     FROM analysis_requests
                    WHERE state IN ('ready', 'failed', 'cancelled')
                 )
                 DELETE FROM analysis_requests WHERE request_id IN (
                   SELECT request_id FROM ranked
                    WHERE (updated_at_ms < $1 OR generation_rank > 20)
                      AND (force_rebuild = 1 OR identity_rank > 1
                        OR priority_generation_rank > 1
                        OR NOT EXISTS (SELECT 1 FROM files
                             WHERE files.id = ranked.file_id
                               AND files.size = ranked.source_size
                               AND files.mtime = ranked.source_mtime))
                    ORDER BY updated_at_ms, request_id LIMIT $2)",
                params!(older_than_ms, limit),
            )
            .await? as u64)
    }

    async fn analysis_requests(&self, limit: i64) -> Result<Vec<AnalysisRequest>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        Ok(self
            .client()
            .query_consistent_map::<RequestRow, _>(
                format!(
                    "WITH visible AS (
                       SELECT * FROM (
                         SELECT * FROM analysis_requests
                          WHERE state IN ('queued', 'running', 'submitted')
                          ORDER BY updated_at_ms DESC, request_id LIMIT $1)
                       UNION ALL
                       SELECT * FROM (
                         SELECT * FROM analysis_requests
                          WHERE state IN ('ready', 'failed', 'cancelled')
                          ORDER BY updated_at_ms DESC, request_id LIMIT $1)
                     )
                     SELECT {REQUEST_COLS} FROM visible
                      ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1
                        WHEN 'submitted' THEN 2 WHEN 'failed' THEN 3 ELSE 4 END,
                        updated_at_ms DESC, request_id LIMIT $1"
                ),
                params!(limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn analysis_request(
        &self,
        request_id: &str,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<RequestRow, _>(
                format!("SELECT {REQUEST_COLS} FROM analysis_requests WHERE request_id = $1"),
                params!(request_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn retry_analysis_request_admin(
        &self,
        request_id: &str,
        requested_generation: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        if requested_generation.is_empty() || requested_generation.len() > 128 {
            return Err(StoreError::Task(
                "invalid requested analysis generation".to_owned(),
            ));
        }
        self.client()
            .txn(vec![(
                "INSERT INTO analysis_requests
                        (request_id, file_id, source_size, source_mtime, component,
                         pipeline_version, requested_generation,
                         expected_predecessor_generation, priority, trigger,
                         force_rebuild, target_node_id, state, owner_node_id, fence,
                         lease_expires_ms, attempts, not_before_ms, result_cache_key,
                         last_error_code, cancel_requested, created_at_ms, updated_at_ms)
                     SELECT $1, original.file_id, original.source_size, original.source_mtime,
                            original.component, original.pipeline_version, $1,
                            CASE WHEN original.component = 'skip_markers' THEN COALESCE((
                              SELECT generation_id FROM timeline_annotation_sets
                               WHERE file_id = original.file_id
                                 AND source_size = original.source_size
                                 AND source_mtime = original.source_mtime
                            ), '') ELSE original.expected_predecessor_generation END,
                            'forced', 'admin', 1,
                            original.target_node_id, 'queued', NULL, 0, NULL, 0,
                            $2, NULL, NULL, 0, $2, $2
                       FROM analysis_requests original
                      WHERE original.request_id = $3
                        AND original.state IN ('failed', 'cancelled')
                        AND EXISTS (SELECT 1 FROM files
                              WHERE id = original.file_id
                                AND size = original.source_size
                                AND mtime = original.source_mtime)
                     ON CONFLICT DO NOTHING"
                    .to_owned(),
                params!(requested_generation, now_ms, request_id),
            )])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        self.analysis_request(requested_generation).await
    }

    async fn cancel_analysis_request_admin(
        &self,
        request_id: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        self.client()
            .txn(vec![
                (
                    "UPDATE analysis_attempts
                        SET phase = 'canceled', phase_updated_at_ms = $1,
                            terminal_code = 'admin_cancelled'
                      WHERE (request_id, claim_epoch) = (
                        SELECT request_id, fence FROM analysis_requests
                         WHERE request_id = $2 AND state IN ('running', 'submitted'))"
                        .to_owned(),
                    params!(now_ms, request_id),
                ),
                (
                    "UPDATE analysis_requests
                        SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                            fence = fence + 1, cancel_requested = 1,
                            last_error_code = 'admin_cancelled', updated_at_ms = $1
                      WHERE request_id = $2
                        AND state IN ('queued', 'running', 'submitted')"
                        .to_owned(),
                    params!(now_ms, request_id),
                ),
                (
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                            fence = fence + 1, last_error_code = 'admin_cancelled',
                            updated_at_ms = $1
                      WHERE state IN ('queued', 'running')
                        AND cache_key = (SELECT result_cache_key FROM analysis_requests
                                          WHERE request_id = $2)
                        AND target_node_id = (SELECT target_node_id FROM analysis_requests
                                              WHERE request_id = $2)
                        AND NOT EXISTS (
                          SELECT 1 FROM analysis_requests other
                           WHERE other.request_id <> $2
                             AND other.result_cache_key = cluster_fragment_index_jobs.cache_key
                             AND other.target_node_id = cluster_fragment_index_jobs.target_node_id
                             AND other.state IN ('queued', 'running', 'submitted'))"
                        .to_owned(),
                    params!(now_ms, request_id),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        self.analysis_request(request_id).await
    }

    async fn complete_analysis_request(
        &self,
        request: &AnalysisRequest,
        result_key: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if request.state != "running"
            || request.owner_node_id.is_empty()
            || result_key.is_empty()
            || result_key.len() > 160
        {
            return Err(StoreError::Task(
                "invalid direct analysis completion".to_owned(),
            ));
        }
        let results = self
            .client()
            .txn(vec![
                (
                    "UPDATE analysis_requests
                    SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                        result_cache_key = $1, last_error_code = NULL, updated_at_ms = $2
                  WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2
                    AND file_id = $6 AND source_size = $7 AND source_mtime = $8
                    AND EXISTS (SELECT 1 FROM files
                          WHERE id = $6 AND size = $7 AND mtime = $8)"
                        .to_owned(),
                    params!(
                        result_key,
                        now_ms,
                        &request.request_id,
                        &request.owner_node_id,
                        request.fence,
                        request.file_id,
                        request.source_size,
                        request.source_mtime
                    ),
                ),
                (
                    "UPDATE analysis_attempts
                        SET phase = 'published', phase_updated_at_ms = $1,
                            terminal_code = NULL
                      WHERE request_id = $2 AND claim_epoch = $3
                        AND EXISTS (SELECT 1 FROM analysis_requests
                              WHERE request_id = $2 AND state = 'ready'
                                AND fence = $3 AND updated_at_ms = $1)"
                        .to_owned(),
                    params!(now_ms, &request.request_id, request.fence),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.first() == Some(&1))
    }

    async fn publish_timeline_annotation_set_for_request(
        &self,
        request: &AnalysisRequest,
        duration_ms: i64,
        set: &crate::segplan::TimelineAnnotationSet,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if request.state != "running" || request.owner_node_id.is_empty() {
            return Err(StoreError::Task(
                "invalid timeline analysis publication".to_owned(),
            ));
        }
        let set = crate::store::timeline_annotations::validated(set, duration_ms)?;
        if set.source_identity.size != u64::try_from(request.source_size).unwrap_or(u64::MAX)
            || set.source_identity.mtime_ms != request.source_mtime
            || set.generation_id != request.requested_generation
        {
            return Err(StoreError::Task(
                "timeline analysis source identity does not match its request".to_owned(),
            ));
        }
        let annotations_json = serde_json::to_string(&set.annotations).map_err(|error| {
            StoreError::Database(format!("encode timeline annotations: {error}"))
        })?;
        let results = self
            .client()
            .txn(vec![
                (
                    "UPDATE analysis_requests
                        SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                            result_cache_key = $1, last_error_code = NULL, updated_at_ms = $2
                      WHERE request_id = $3 AND component = 'skip_markers'
                        AND state = 'running' AND owner_node_id = $4
                        AND fence = $5 AND lease_expires_ms > $2
                        AND file_id = $6 AND source_size = $7 AND source_mtime = $8
                        AND requested_generation = $1
                        AND expected_predecessor_generation = COALESCE((
                          SELECT generation_id FROM timeline_annotation_sets
                           WHERE file_id = $6 AND source_size = $7 AND source_mtime = $8
                        ), '')
                        AND (force_rebuild = 1 OR COALESCE((
                          SELECT publication_priority FROM timeline_annotation_sets
                           WHERE file_id = $6 AND source_size = $7 AND source_mtime = $8
                        ), 'normal') <> 'forced')
                        AND NOT EXISTS (
                          SELECT 1 FROM analysis_requests newer
                           WHERE newer.file_id = $6 AND newer.source_size = $7
                             AND newer.source_mtime = $8 AND newer.component = 'skip_markers'
                             AND (newer.created_at_ms > analysis_requests.created_at_ms
                               OR (newer.created_at_ms = analysis_requests.created_at_ms
                                 AND newer.request_id > analysis_requests.request_id))
                             AND newer.state IN ('queued','running','submitted','ready'))
                        AND EXISTS (SELECT 1 FROM files
                              WHERE id = $6 AND size = $7 AND mtime = $8)"
                        .to_owned(),
                    params!(
                        &set.generation_id,
                        now_ms,
                        &request.request_id,
                        &request.owner_node_id,
                        request.fence,
                        request.file_id,
                        request.source_size,
                        request.source_mtime
                    ),
                ),
                (
                    "INSERT INTO timeline_annotation_sets
                        (file_id, source_size, source_mtime, argv_fingerprint,
                         generation_id, version, annotations_json, updated_at_ms,
                         publication_priority)
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9
                      WHERE EXISTS (SELECT 1 FROM analysis_requests
                        WHERE request_id = $10 AND component = 'skip_markers'
                          AND state = 'ready' AND fence = $11
                          AND file_id = $1 AND source_size = $2 AND source_mtime = $3
                          AND result_cache_key = $5 AND updated_at_ms = $8)
                     ON CONFLICT(file_id) DO UPDATE SET
                        source_size = excluded.source_size,
                        source_mtime = excluded.source_mtime,
                        argv_fingerprint = excluded.argv_fingerprint,
                        generation_id = excluded.generation_id,
                        version = excluded.version,
                        annotations_json = excluded.annotations_json,
                        updated_at_ms = excluded.updated_at_ms,
                        publication_priority = excluded.publication_priority"
                        .to_owned(),
                    params!(
                        request.file_id,
                        request.source_size,
                        set.source_identity.mtime_ms,
                        &set.source_identity.argv_fingerprint,
                        &set.generation_id,
                        i64::from(set.version),
                        annotations_json,
                        now_ms,
                        if request.force_rebuild {
                            "forced"
                        } else {
                            "normal"
                        },
                        &request.request_id,
                        request.fence
                    ),
                ),
                (
                    "UPDATE analysis_attempts
                        SET phase = 'published', phase_updated_at_ms = $1,
                            terminal_code = NULL
                      WHERE request_id = $2 AND claim_epoch = $3
                        AND EXISTS (SELECT 1 FROM analysis_requests
                              WHERE request_id = $2 AND state = 'ready'
                                AND fence = $3 AND updated_at_ms = $1)"
                        .to_owned(),
                    params!(now_ms, &request.request_id, request.fence),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        match (
            results.first().copied(),
            results.get(1).copied(),
            results.get(2).copied(),
        ) {
            (Some(1), Some(1), Some(1)) => Ok(true),
            (Some(0), Some(0), Some(0)) => Ok(false),
            _ => Err(StoreError::Task(
                "timeline analysis publication was not atomic".to_owned(),
            )),
        }
    }

    async fn analysis_history(
        &self,
        query: &AnalysisHistoryQuery,
    ) -> Result<AnalysisHistoryPage, StoreError> {
        let limit = query.limit.clamp(10, 100);
        let filter = analysis_filter_code(query.filter);
        let search = analysis_search_pattern(&query.search);
        let states = analysis_state_pattern(&query.states);
        let now_ms = query.now_ms;
        let cursor = query.cursor.clone().unwrap_or(AnalysisHistoryCursor {
            sort_rank: -1,
            updated_at_ms: 0,
            row_key: String::new(),
        });
        let sql = format!(
            "{ANALYSIS_CANONICAL_CTE}, matching AS (
               SELECT * FROM classified
                WHERE ($1 = 0
                  OR ($1 = 1 AND disposition IN ('working', 'automatic'))
                  OR ($1 = 2 AND disposition = 'attention')
                  OR ($1 = 3 AND disposition = 'ready')
                  OR ($1 = 4 AND disposition IN ('expected', 'unsupported')))
                  AND ($2 = '' OR LOWER(title) LIKE $2 ESCAPE '\\'
                    OR CAST(file_id AS TEXT) LIKE $2 ESCAPE '\\'
                    OR LOWER(row_key) LIKE $2 ESCAPE '\\'
                    OR LOWER(owner_node_id) LIKE $2 ESCAPE '\\'
                    OR LOWER(target_node_id) LIKE $2 ESCAPE '\\'
                    OR LOWER(request_error_code) LIKE $2 ESCAPE '\\'
                    OR LOWER(job_error_code) LIKE $2 ESCAPE '\\'
                    OR LOWER(state) LIKE $2 ESCAPE '\\')
                  AND ($3 = '' OR INSTR($3, ',' || CASE
                    WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code) = 'source_superseded'
                      THEN 'stale'
                    WHEN state = 'queued' AND not_before_ms > $4 THEN 'retry_wait'
                    WHEN state = 'running' THEN 'claimed'
                    WHEN state = 'submitted' THEN 'staged'
                    WHEN state = 'ready' THEN 'published'
                    WHEN state = 'cancelled' THEN 'canceled'
                    ELSE state END || ',') > 0)
             ), page AS (
               SELECT {HISTORY_COLS}, sort_rank FROM matching
                WHERE $5 < 0 OR sort_rank > $5
                   OR (sort_rank = $5 AND updated_at_ms < $6)
                   OR (sort_rank = $5 AND updated_at_ms = $6 AND row_key > $7)
                ORDER BY sort_rank, updated_at_ms DESC, row_key
                LIMIT $8
             ), totals AS (
               SELECT COUNT(*) AS filtered_total FROM matching
             )
             SELECT {HISTORY_PAGE_COLS}, COALESCE(page.sort_rank, -1) AS sort_rank,
                    totals.filtered_total
               FROM totals LEFT JOIN page ON 1 = 1
              ORDER BY page.sort_rank, page.updated_at_ms DESC, page.row_key"
        );
        let mut rows = self
            .client()
            .query_consistent_map::<HistoryRow, _>(
                sql,
                params!(
                    filter,
                    search,
                    states,
                    now_ms,
                    cursor.sort_rank,
                    cursor.updated_at_ms,
                    cursor.row_key,
                    limit + 1
                ),
            )
            .await?;
        let filtered_total = rows.first().map_or(0, |row| row.2);
        rows.retain(|row| !row.0.row_key.is_empty());
        let has_more = rows.len() > limit as usize;
        if has_more {
            rows.pop();
        }
        let next_cursor = has_more.then(|| rows.last().expect("non-empty history page").1.clone());
        Ok(AnalysisHistoryPage {
            rows: rows.into_iter().map(|row| row.0).collect(),
            filtered_total,
            next_cursor,
        })
    }

    async fn analysis_status_summary(&self) -> Result<AnalysisStatusSummary, StoreError> {
        let sql = format!(
            "{ANALYSIS_SUMMARY_CTE}
             SELECT COUNT(*) AS total,
                    COALESCE(SUM(CASE WHEN disposition IN ('working', 'automatic') THEN 1 ELSE 0 END), 0) AS working,
                    COALESCE(SUM(CASE WHEN state = 'queued' THEN 1 ELSE 0 END), 0) AS queued,
                    COALESCE(SUM(CASE WHEN state = 'running' THEN 1 ELSE 0 END), 0) AS running,
                    COALESCE(SUM(CASE WHEN state = 'submitted' THEN 1 ELSE 0 END), 0) AS submitted,
                    COALESCE(SUM(CASE WHEN disposition = 'attention' THEN 1 ELSE 0 END), 0) AS attention,
                    COALESCE(SUM(CASE WHEN disposition IN ('expected', 'unsupported') THEN 1 ELSE 0 END), 0) AS expected,
                    COALESCE(SUM(CASE WHEN disposition = 'ready' THEN 1 ELSE 0 END), 0) AS ready,
                    COALESCE((SELECT COALESCE(NULLIF(job_error_code, ''), request_error_code)
                      FROM summary_classified WHERE disposition = 'attention'
                      ORDER BY updated_at_ms DESC, row_key LIMIT 1), '') AS latest_error_code,
                    COALESCE((SELECT file_id FROM summary_classified WHERE disposition = 'attention'
                      ORDER BY updated_at_ms DESC, row_key LIMIT 1), 0) AS latest_error_file_id,
                    COALESCE((SELECT updated_at_ms FROM summary_classified WHERE disposition = 'attention'
                      ORDER BY updated_at_ms DESC, row_key LIMIT 1), 0) AS latest_error_updated_at_ms
               FROM summary_classified"
        );
        self.client()
            .query_consistent_map::<StatusSummaryRow, _>(sql, params!())
            .await?
            .into_iter()
            .next()
            .map(|row| row.0)
            .ok_or_else(|| StoreError::Task("analysis summary returned no row".to_owned()))
    }

    async fn analysis_file_labels(&self, limit: i64) -> Result<Vec<AnalysisFileLabel>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        Ok(self
            .client()
            .query_consistent_map::<LabelRow, _>(
                "SELECT files.id AS file_id, files.item_id AS item_id, items.title AS title
                   FROM files JOIN items ON items.id = files.item_id
                  WHERE files.id IN (
                    SELECT file_id FROM (
                      SELECT file_id FROM analysis_requests
                       ORDER BY updated_at_ms DESC, request_id LIMIT $1)
                    UNION SELECT file_id FROM (
                      SELECT file_id FROM cluster_fragment_index_jobs
                       ORDER BY updated_at_ms DESC, cache_key LIMIT $1))
                  ORDER BY files.id LIMIT $1",
                params!(limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn cluster_fragment_index_jobs(
        &self,
        limit: i64,
    ) -> Result<Vec<ClusterFragmentIndexJob>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        Ok(self
            .client()
            .query_consistent_map::<JobRow, _>(
                format!(
                    "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs
                      ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1
                        WHEN 'failed' THEN 2 ELSE 3 END, updated_at_ms DESC, cache_key
                      LIMIT $1"
                ),
                params!(limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn cluster_fragment_index_job(
        &self,
        cache_key: &str,
        target_node_id: &str,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<JobRow, _>(
                format!("SELECT {JOB_COLS} FROM cluster_fragment_index_jobs WHERE cache_key = $1 AND target_node_id = $2"),
                params!(cache_key, target_node_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn cluster_fragment_index_current_generation(
        &self,
        logical_cache_key: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<CacheKeyRow, _>(
                "SELECT generation_cache_key AS cache_key
                   FROM cluster_fragment_index_heads WHERE logical_cache_key = $1",
                params!(logical_cache_key),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn record_fragment_index_source(
        &self,
        observation: &FragmentIndexSourceObservation,
    ) -> Result<(), StoreError> {
        if observation.node_id.is_empty()
            || observation.node_id.len() > 128
            || observation.file_id <= 0
            || observation.object_version.is_empty()
            || observation.object_version.len() > 512
            || observation.source_size < 0
            || !valid_hex_digest(&observation.source_sha256)
        {
            return Err(StoreError::Task(
                "invalid fragment-index source observation".to_owned(),
            ));
        }
        self.execute(
            "INSERT INTO cluster_fragment_index_sources
                (node_id, file_id, object_version, source_size, source_mtime,
                 source_sha256, observed_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT(node_id, file_id) DO UPDATE SET
                object_version = excluded.object_version,
                source_size = excluded.source_size,
                source_mtime = excluded.source_mtime,
                source_sha256 = excluded.source_sha256,
                observed_at_ms = excluded.observed_at_ms",
            params!(
                &observation.node_id,
                observation.file_id,
                &observation.object_version,
                observation.source_size,
                observation.source_mtime,
                &observation.source_sha256,
                observation.observed_at_ms
            ),
        )
        .await?;
        Ok(())
    }

    async fn fragment_index_source(
        &self,
        node_id: &str,
        file_id: i64,
        object_version: &str,
    ) -> Result<Option<FragmentIndexSourceObservation>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<SourceRow, _>(
                "SELECT node_id, file_id, object_version, source_size, source_mtime,
                        source_sha256, observed_at_ms
                   FROM cluster_fragment_index_sources
                  WHERE node_id = $1 AND file_id = $2 AND object_version = $3",
                params!(node_id, file_id, object_version),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn cluster_fragment_index_artifact(
        &self,
        cache_key: &str,
    ) -> Result<Option<ClusterFragmentIndexArtifact>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<ArtifactRow, _>(
                "SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
                        pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms
                   FROM cluster_fragment_index_artifacts WHERE cache_key = COALESCE((
                     SELECT generation_cache_key FROM cluster_fragment_index_heads
                      WHERE logical_cache_key = $1
                   ), $1)",
                params!(cache_key),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.0))
    }

    async fn enqueue_cluster_fragment_index(
        &self,
        job: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError> {
        if !valid_job(job) {
            return Err(StoreError::Task(
                "invalid cluster fragment-index job".to_owned(),
            ));
        }
        let max_attempts = configured_max_attempts(self).await?;
        let eligibility_cutoff = job.created_at_ms.saturating_sub(QUEUE_ELIGIBILITY_MS);
        self.execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = $1, updated_at_ms = $1,
                    last_error_code = CASE WHEN state = 'running'
                      THEN 'attempt_limit' ELSE 'queue_expired' END
              WHERE (state = 'queued' AND created_at_ms < $2)
                 OR (state = 'running' AND attempts >= $3
                   AND COALESCE(lease_expires_ms, 0) <= $1)",
            params!(job.created_at_ms, eligibility_cutoff, max_attempts),
        )
        .await?;
        Ok(self
            .execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, priority, trigger, target_node_id,
                     state, owner_node_id, fence, lease_expires_ms,
                     attempts, not_before_ms, created_at_ms, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9,
                        'queued', NULL, 0, NULL, 0, $10, $11, $11
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = $2 AND size = $3 AND mtime = $4)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = $1)
                    AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < $12
                 ON CONFLICT(cache_key, target_node_id) DO UPDATE SET
                    file_id = excluded.file_id, source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    pipeline_sha256 = excluded.pipeline_sha256,
                    priority = CASE
                        WHEN cluster_fragment_index_jobs.priority IN ('forced','foreground')
                        THEN cluster_fragment_index_jobs.priority
                        ELSE excluded.priority END,
                    trigger = CASE
                        WHEN excluded.priority = 'foreground' THEN 'foreground'
                        ELSE excluded.trigger END,
                    state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                    attempts = CASE
                        WHEN cluster_fragment_index_jobs.state = 'cancelled'
                          OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                          OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN 0
                        ELSE cluster_fragment_index_jobs.attempts END,
                    not_before_ms = CASE
                        WHEN cluster_fragment_index_jobs.state = 'queued'
                        THEN MIN(cluster_fragment_index_jobs.not_before_ms, excluded.not_before_ms)
                        ELSE excluded.not_before_ms END,
                    created_at_ms = CASE
                        WHEN cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                        THEN excluded.created_at_ms
                        ELSE cluster_fragment_index_jobs.created_at_ms END,
                    updated_at_ms = excluded.updated_at_ms,
                    last_error_code = NULL
                  WHERE cluster_fragment_index_jobs.state = 'cancelled'
                     OR (cluster_fragment_index_jobs.state = 'failed'
                       AND cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                       AND cluster_fragment_index_jobs.attempts <= $13)
                     OR (cluster_fragment_index_jobs.state = 'queued'
                       AND cluster_fragment_index_jobs.priority = 'normal'
                       AND excluded.priority = 'foreground')",
                params!(
                    &job.cache_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    &job.source_sha256,
                    &job.pipeline_sha256,
                    &job.priority,
                    &job.trigger,
                    &job.target_node_id,
                    job.not_before_ms,
                    job.created_at_ms,
                    MAX_ACTIVE_JOBS,
                    max_attempts
                ),
            )
            .await?
            == 1)
    }

    async fn claim_cluster_fragment_index(
        &self,
        node_id: &str,
        excluded_cache_keys: &[String],
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError> {
        if node_id.is_empty()
            || node_id.len() > 128
            || excluded_cache_keys.len() > MAX_LOCAL_EXCLUSIONS
            || excluded_cache_keys.iter().any(|key| !valid_hex_digest(key))
            || lease_expires_ms <= now_ms
        {
            return Err(StoreError::Task(
                "invalid cluster fragment-index claim".to_owned(),
            ));
        }
        let excluded_cache_keys = excluded_cache_keys
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let max_attempts = configured_max_attempts(self).await?;
        let backoff_base_ms = configured_backoff_base_ms(self).await?;
        let backoff_max_ms = configured_backoff_max_ms(self).await?.max(backoff_base_ms);
        self.client()
            .txn(vec![
                (
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                            not_before_ms = $1, updated_at_ms = $1,
                            last_error_code = CASE WHEN state = 'running'
                              THEN 'attempt_limit' ELSE 'queue_expired' END
                      WHERE (state = 'queued' AND created_at_ms < $2)
                         OR (state = 'running' AND attempts >= $3
                           AND COALESCE(lease_expires_ms, 0) <= $1)"
                        .to_owned(),
                    params!(
                        now_ms,
                        now_ms.saturating_sub(QUEUE_ELIGIBILITY_MS),
                        max_attempts
                    ),
                ),
                (
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                            not_before_ms = $1 + MIN(MAX(1000,
                              ($2 * (1 << MIN(MAX(attempts - 1, 0), 30)) *
                               (75 + (ABS(length(cache_key || ':' || target_node_id) * 17
                                 + unicode(substr(cache_key || ':' || target_node_id, 1, 1)) * 31
                                 + unicode(substr(cache_key || ':' || target_node_id, -1, 1)) * 13
                                 + attempts * 7) % 51))) / 100), $3),
                            last_error_code = 'lease_expired', updated_at_ms = $1
                      WHERE state = 'running' AND COALESCE(lease_expires_ms, 0) <= $1
                        AND attempts < $4"
                        .to_owned(),
                    params!(now_ms, backoff_base_ms, backoff_max_ms, max_attempts),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        for _ in 0..8 {
            let sql = format!(
                "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs job
                  WHERE state = 'queued' AND not_before_ms <= $1
                    AND attempts < $2 AND fence < 9223372036854775807
                    AND (target_node_id = '' OR target_node_id = $3)
                  ORDER BY job.created_at_ms - CASE
                      WHEN job.priority IN ('forced','foreground') THEN $4 ELSE 0 END,
                    job.created_at_ms, job.cache_key, job.target_node_id LIMIT $5"
            );
            let candidates = self
                .client()
                .query_consistent_map::<JobRow, _>(
                    sql,
                    params!(
                        now_ms,
                        max_attempts,
                        node_id,
                        super::fragment_index_cluster::ANALYSIS_FORCED_PRIORITY_BOOST_MS,
                        CLAIM_SCAN_LIMIT
                    ),
                )
                .await?
                .into_iter()
                .map(|row| row.0)
                .collect::<Vec<_>>();
            let Some(mut candidate) = candidates
                .into_iter()
                .find(|candidate| !excluded_cache_keys.contains(&candidate.cache_key))
            else {
                return Ok(None);
            };
            let results = self
                .client()
                .txn(vec![(
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'running', owner_node_id = $1, fence = fence + 1,
                            lease_expires_ms = $2, attempts = attempts + 1,
                            last_error_code = NULL, updated_at_ms = $3
                      WHERE cache_key = $4 AND fence = $5 AND target_node_id = $6
                        AND state = 'queued' AND not_before_ms <= $3
                        AND (target_node_id = '' OR target_node_id = $1)"
                        .to_owned(),
                    params!(
                        node_id,
                        lease_expires_ms,
                        now_ms,
                        &candidate.cache_key,
                        candidate.fence,
                        &candidate.target_node_id
                    ),
                )])
                .await?
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .map_err(database_error)?;
            let changed = results.first().copied().unwrap_or_default();
            if changed == 1 {
                candidate.state = "running".to_owned();
                candidate.owner_node_id = node_id.to_owned();
                candidate.fence += 1;
                candidate.lease_expires_ms = lease_expires_ms;
                candidate.attempts += 1;
                candidate.updated_at_ms = now_ms;
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    async fn requeue_cluster_fragment_index(
        &self,
        replacement: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError> {
        if !valid_job(replacement) {
            return Err(StoreError::Task(
                "invalid cluster fragment-index repair job".to_owned(),
            ));
        }
        let max_attempts = configured_max_attempts(self).await?;
        let eligibility_cutoff = replacement
            .created_at_ms
            .saturating_sub(QUEUE_ELIGIBILITY_MS);
        self.execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = $1, updated_at_ms = $1,
                    last_error_code = CASE WHEN state = 'running'
                      THEN 'attempt_limit' ELSE 'queue_expired' END
              WHERE (state = 'queued' AND created_at_ms < $2)
                 OR (state = 'running' AND attempts >= $3
                   AND COALESCE(lease_expires_ms, 0) <= $1)",
            params!(replacement.created_at_ms, eligibility_cutoff, max_attempts),
        )
        .await?;
        Ok(self
            .execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, priority, trigger, target_node_id, state,
                     owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
                     created_at_ms, updated_at_ms, last_error_code)
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, 'queued',
                        NULL, 0, NULL, 0, $10, $11, $11, 'holders_unavailable'
                  WHERE (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < $12
                    AND EXISTS (SELECT 1 FROM files
                      WHERE id = $2 AND size = $3 AND mtime = $4)
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                      WHERE cache_key = $1 AND source_size = $3
                        AND source_sha256 = $5 AND pipeline_sha256 = $6)
                 ON CONFLICT(cache_key, target_node_id) DO UPDATE SET
                    file_id = excluded.file_id, source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    pipeline_sha256 = excluded.pipeline_sha256,
                    priority = excluded.priority, trigger = excluded.trigger,
                    state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                    attempts = CASE
                      WHEN cluster_fragment_index_jobs.state = 'ready'
                        OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                        OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN 0
                      ELSE cluster_fragment_index_jobs.attempts END,
                    not_before_ms = excluded.not_before_ms,
                    created_at_ms = excluded.created_at_ms,
                    updated_at_ms = excluded.updated_at_ms,
                    last_error_code = 'holders_unavailable'
                  WHERE cluster_fragment_index_jobs.state = 'ready'
                     OR (cluster_fragment_index_jobs.state = 'failed'
                       AND cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                       AND cluster_fragment_index_jobs.attempts <= $13)",
                params!(
                    &replacement.cache_key,
                    replacement.file_id,
                    replacement.source_size,
                    replacement.source_mtime,
                    &replacement.source_sha256,
                    &replacement.pipeline_sha256,
                    &replacement.priority,
                    &replacement.trigger,
                    &replacement.target_node_id,
                    replacement.not_before_ms,
                    replacement.created_at_ms,
                    MAX_ACTIVE_JOBS,
                    max_attempts
                ),
            )
            .await?
            == 1)
    }

    async fn renew_cluster_fragment_index(
        &self,
        cache_key: &str,
        target_node_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET lease_expires_ms = $1, updated_at_ms = $2
                  WHERE cache_key = $3 AND target_node_id = $6
                    AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2 AND $1 > $2",
                params!(
                    lease_expires_ms,
                    now_ms,
                    cache_key,
                    node_id,
                    fence,
                    target_node_id
                ),
            )
            .await?
            == 1)
    }

    async fn yield_cluster_fragment_index(
        &self,
        cache_key: &str,
        target_node_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = MAX(attempts - 1, 0), not_before_ms = $1,
                        last_error_code = 'node_local_refusal', updated_at_ms = $2
                  WHERE cache_key = $3 AND target_node_id = $6
                    AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2",
                params!(
                    retry_at_ms,
                    now_ms,
                    cache_key,
                    node_id,
                    fence,
                    target_node_id
                ),
            )
            .await?
            == 1)
    }

    async fn complete_cluster_fragment_index(
        &self,
        job: &ClusterFragmentIndexJob,
        artifact: &ClusterFragmentIndexArtifact,
        location: &ClusterFragmentIndexLocation,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if artifact.cache_key != job.cache_key
            || artifact.file_id != job.file_id
            || artifact.source_size != job.source_size
            || artifact.source_mtime != job.source_mtime
            || artifact.source_sha256 != job.source_sha256
            || artifact.pipeline_sha256 != job.pipeline_sha256
            || location.cache_key != job.cache_key
            || artifact.built_by_node_id != job.owner_node_id
            || location.node_id != job.owner_node_id
            || artifact.bytes <= 0
            || location.bytes != artifact.bytes
            || !valid_hex_digest(&artifact.blob_sha256)
        {
            return Err(StoreError::Task(
                "invalid cluster fragment-index completion".to_owned(),
            ));
        }
        let logical_cache_key = cluster_fragment_index_key(
            artifact.file_id,
            artifact.source_size,
            artifact.source_mtime,
            &artifact.source_sha256,
            &artifact.pipeline_sha256,
        )
        .ok_or_else(|| StoreError::Task("invalid fragment-index logical key".to_owned()))?;
        let results = self
            .client()
            .txn(vec![
                (
                    "INSERT INTO cluster_fragment_index_artifacts
                        (cache_key, file_id, source_size, source_mtime, source_sha256,
                         pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
                      WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                        WHERE cache_key = $1 AND target_node_id = $11
                          AND state = 'running' AND owner_node_id = $9
                          AND fence = $12 AND lease_expires_ms > $13
                          AND file_id = $2 AND source_size = $3 AND source_mtime = $4
                          AND source_sha256 = $5 AND pipeline_sha256 = $6
                          AND EXISTS (SELECT 1 FROM files current_file
                            WHERE current_file.id = $2 AND current_file.size = $3
                              AND current_file.mtime = $4)
                          AND ($14 = $1 OR EXISTS (
                            SELECT 1 FROM analysis_requests
                             WHERE component = 'fragment_index' AND state = 'submitted'
                               AND result_cache_key = $1 AND target_node_id = $11) OR EXISTS (
                            SELECT 1 FROM cluster_fragment_index_heads
                             WHERE logical_cache_key = $14 AND generation_cache_key = $1)))
                     ON CONFLICT(cache_key) DO NOTHING"
                        .to_owned(),
                    params!(
                        &artifact.cache_key,
                        artifact.file_id,
                        artifact.source_size,
                        artifact.source_mtime,
                        &artifact.source_sha256,
                        &artifact.pipeline_sha256,
                        &artifact.blob_sha256,
                        artifact.bytes,
                        &artifact.built_by_node_id,
                        artifact.built_at_ms,
                        &job.target_node_id,
                        job.fence,
                        now_ms,
                        &logical_cache_key
                    ),
                ),
                (
                    "INSERT INTO cluster_fragment_index_locations
                        (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                     SELECT $1, $2, $3, $4, $5
                      WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                        WHERE cache_key = $1 AND target_node_id = $6
                          AND state = 'running' AND owner_node_id = $2
                          AND fence = $7 AND lease_expires_ms > $8)
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                          WHERE cache_key = $1 AND source_sha256 = $9
                            AND pipeline_sha256 = $10 AND blob_sha256 = $11 AND bytes = $3)
                     ON CONFLICT(cache_key, node_id) DO UPDATE SET
                        bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                        last_seen_at_ms = excluded.last_seen_at_ms"
                        .to_owned(),
                    params!(
                        &location.cache_key,
                        &location.node_id,
                        location.bytes,
                        location.verified_at_ms,
                        location.last_seen_at_ms,
                        &job.target_node_id,
                        job.fence,
                        now_ms,
                        &artifact.source_sha256,
                        &artifact.pipeline_sha256,
                        &artifact.blob_sha256
                    ),
                ),
                (
                    "INSERT INTO cluster_fragment_index_heads
                        (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                     SELECT $1, $2, request_id, $3 FROM analysis_requests
                      WHERE component = 'fragment_index' AND state = 'submitted'
                        AND result_cache_key = $2
                        AND target_node_id = $4
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs current_job
                          WHERE current_job.cache_key = $2
                            AND current_job.target_node_id = $4
                            AND current_job.state = 'running'
                            AND current_job.owner_node_id = $5 AND current_job.fence = $6
                            AND current_job.lease_expires_ms > $3
                            AND current_job.file_id = $7
                            AND current_job.source_size = $8
                            AND current_job.source_mtime = $9
                            AND current_job.source_sha256 = $10
                            AND current_job.pipeline_sha256 = $11)
                        AND EXISTS (SELECT 1 FROM files current_file
                          WHERE current_file.id = $7 AND current_file.size = $8
                            AND current_file.mtime = $9)
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts artifact
                          WHERE artifact.cache_key = $2 AND artifact.file_id = $7
                            AND artifact.source_size = $8 AND artifact.source_mtime = $9
                            AND artifact.source_sha256 = $10
                            AND artifact.pipeline_sha256 = $11
                            AND artifact.blob_sha256 = $12 AND artifact.bytes = $13)
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                          WHERE location.cache_key = $2 AND location.node_id = $5
                            AND location.bytes = $13)
                        AND (force_rebuild = 1 OR expected_predecessor_generation = '')
                        AND expected_predecessor_generation = COALESCE((
                          SELECT generation_cache_key FROM cluster_fragment_index_heads
                           WHERE logical_cache_key = $1
                        ), '')
                        AND NOT EXISTS (
                          SELECT 1 FROM analysis_requests newer
                           WHERE newer.file_id = analysis_requests.file_id
                             AND newer.source_size = analysis_requests.source_size
                             AND newer.source_mtime = analysis_requests.source_mtime
                             AND newer.component = 'fragment_index'
                             AND newer.target_node_id = analysis_requests.target_node_id
                             AND (newer.created_at_ms > analysis_requests.created_at_ms
                               OR (newer.created_at_ms = analysis_requests.created_at_ms
                                 AND newer.request_id > analysis_requests.request_id))
                             AND newer.state IN ('queued','running','submitted','ready'))
                     ON CONFLICT(logical_cache_key) DO UPDATE SET
                        generation_cache_key = excluded.generation_cache_key,
                        request_id = excluded.request_id,
                        updated_at_ms = excluded.updated_at_ms
                      WHERE cluster_fragment_index_heads.generation_cache_key = (
                        SELECT expected_predecessor_generation FROM analysis_requests
                         WHERE request_id = excluded.request_id)"
                        .to_owned(),
                    params!(
                        &logical_cache_key,
                        &job.cache_key,
                        now_ms,
                        &job.target_node_id,
                        &job.owner_node_id,
                        job.fence,
                        job.file_id,
                        job.source_size,
                        job.source_mtime,
                        &job.source_sha256,
                        &job.pipeline_sha256,
                        &artifact.blob_sha256,
                        artifact.bytes
                    ),
                ),
                (
                    "INSERT INTO cluster_fragment_index_heads
                        (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                     SELECT $1, $2, '', $3 WHERE $1 = $2
                       AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs current_job
                         WHERE current_job.cache_key = $2
                           AND current_job.target_node_id = $4
                           AND current_job.state = 'running'
                           AND current_job.owner_node_id = $5 AND current_job.fence = $6
                           AND current_job.lease_expires_ms > $3
                           AND current_job.file_id = $7
                           AND current_job.source_size = $8
                           AND current_job.source_mtime = $9
                           AND current_job.source_sha256 = $10
                           AND current_job.pipeline_sha256 = $11)
                       AND EXISTS (SELECT 1 FROM files current_file
                         WHERE current_file.id = $7 AND current_file.size = $8
                           AND current_file.mtime = $9)
                       AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts artifact
                         WHERE artifact.cache_key = $2 AND artifact.file_id = $7
                           AND artifact.source_size = $8 AND artifact.source_mtime = $9
                           AND artifact.source_sha256 = $10
                           AND artifact.pipeline_sha256 = $11
                           AND artifact.blob_sha256 = $12 AND artifact.bytes = $13)
                       AND EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                         WHERE location.cache_key = $2 AND location.node_id = $5
                           AND location.bytes = $13)
                     ON CONFLICT(logical_cache_key) DO NOTHING"
                        .to_owned(),
                    params!(
                        &logical_cache_key,
                        &job.cache_key,
                        now_ms,
                        &job.target_node_id,
                        &job.owner_node_id,
                        job.fence,
                        job.file_id,
                        job.source_size,
                        job.source_mtime,
                        &job.source_sha256,
                        &job.pipeline_sha256,
                        &artifact.blob_sha256,
                        artifact.bytes
                    ),
                ),
                (
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                            last_error_code = NULL, updated_at_ms = $1
                      WHERE cache_key = $2 AND target_node_id = $3
                        AND state = 'running' AND owner_node_id = $4
                        AND fence = $5 AND lease_expires_ms > $1
                        AND EXISTS (SELECT 1 FROM files current_file
                          WHERE current_file.id = cluster_fragment_index_jobs.file_id
                            AND current_file.size = cluster_fragment_index_jobs.source_size
                            AND current_file.mtime = cluster_fragment_index_jobs.source_mtime)
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                          WHERE cache_key = $2 AND source_sha256 = $6
                            AND pipeline_sha256 = $7 AND blob_sha256 = $8 AND bytes = $9)"
                        .to_owned(),
                    params!(
                        now_ms,
                        &job.cache_key,
                        &job.target_node_id,
                        &job.owner_node_id,
                        job.fence,
                        &artifact.source_sha256,
                        &artifact.pipeline_sha256,
                        &artifact.blob_sha256,
                        artifact.bytes
                    ),
                ),
            ])
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.last().copied() == Some(1))
    }

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
    ) -> Result<bool, StoreError> {
        if error_code.is_empty()
            || error_code.len() > MAX_ERROR_CODE_BYTES
            || (retryable && retry_at_ms <= now_ms)
        {
            return Err(StoreError::Task(
                "invalid fragment-index failure code".to_owned(),
            ));
        }
        let max_attempts = configured_max_attempts(self).await?;
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = CASE WHEN $1 = 1 AND attempts < $2
                              THEN 'queued' ELSE 'failed' END,
                        owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = CASE WHEN $1 = 1 AND attempts >= $2
                              THEN 'attempt_limit' ELSE $3 END,
                        not_before_ms = $4, updated_at_ms = $5
                  WHERE cache_key = $6 AND target_node_id = $7
                    AND state = 'running' AND owner_node_id = $8
                    AND fence = $9 AND lease_expires_ms > $5",
                params!(
                    if retryable { 1_i64 } else { 0_i64 },
                    max_attempts,
                    error_code,
                    retry_at_ms,
                    now_ms,
                    cache_key,
                    target_node_id,
                    node_id,
                    fence
                ),
            )
            .await?
            == 1)
    }

    async fn put_cluster_fragment_index_location(
        &self,
        location: &ClusterFragmentIndexLocation,
    ) -> Result<(), StoreError> {
        if location.cache_key.len() != 64
            || location.node_id.is_empty()
            || location.node_id.len() > 128
            || location.bytes <= 0
        {
            return Err(StoreError::Task(
                "invalid fragment-index location".to_owned(),
            ));
        }
        self.execute(
            "INSERT INTO cluster_fragment_index_locations
                (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
             SELECT $1, $2, $3, $4, $5
              WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                             WHERE cache_key = $1 AND bytes = $3)
             ON CONFLICT(cache_key, node_id) DO UPDATE SET
                bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                last_seen_at_ms = excluded.last_seen_at_ms",
            params!(
                &location.cache_key,
                &location.node_id,
                location.bytes,
                location.verified_at_ms,
                location.last_seen_at_ms
            ),
        )
        .await?;
        Ok(())
    }

    async fn cluster_fragment_index_locations(
        &self,
        cache_key: &str,
    ) -> Result<Vec<ClusterFragmentIndexLocation>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<LocationRow, _>(
                "SELECT cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms
                   FROM cluster_fragment_index_locations
                  WHERE cache_key = $1 ORDER BY last_seen_at_ms DESC, node_id LIMIT 128",
                params!(cache_key),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect())
    }

    async fn forget_cluster_fragment_index_location(
        &self,
        cache_key: &str,
        node_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "DELETE FROM cluster_fragment_index_locations
                  WHERE cache_key = $1 AND node_id = $2",
                params!(cache_key, node_id),
            )
            .await?
            == 1)
    }

    async fn prune_cluster_fragment_indexes(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<Vec<String>, StoreError> {
        if !(1..=512).contains(&limit) {
            return Err(StoreError::Task(
                "invalid cluster fragment-index prune bound".to_owned(),
            ));
        }
        self.execute(
            "DELETE FROM cluster_fragment_index_locations
              WHERE (cache_key, node_id) IN (
                SELECT cache_key, node_id
                  FROM cluster_fragment_index_locations
                 WHERE last_seen_at_ms < $1
                 ORDER BY last_seen_at_ms, cache_key, node_id
                 LIMIT $2
              )",
            params!(older_than_ms, limit),
        )
        .await?;
        let candidates = self
            .client()
            .query_consistent_map::<CacheKeyRow, _>(
                "SELECT j.cache_key AS cache_key
                   FROM cluster_fragment_index_jobs j
                  WHERE (j.state IN ('ready', 'cancelled') OR (
                    j.state = 'failed' AND (
                      NOT EXISTS (SELECT 1 FROM files current_file
                        WHERE current_file.id = j.file_id
                          AND current_file.size = j.source_size
                          AND current_file.mtime = j.source_mtime)
                      OR EXISTS (SELECT 1 FROM analysis_requests request
                        WHERE request.result_cache_key = j.cache_key
                          AND request.target_node_id = j.target_node_id
                          AND request.force_rebuild = 1))))
                    AND j.updated_at_ms < $1
                    AND NOT EXISTS (
                      SELECT 1 FROM cluster_fragment_index_jobs active_job
                       WHERE active_job.cache_key = j.cache_key
                         AND (active_job.state IN ('queued', 'running')
                           OR active_job.updated_at_ms >= $1))
                    AND NOT EXISTS (
                      SELECT 1 FROM analysis_requests active_request
                       WHERE active_request.result_cache_key = j.cache_key
                         AND active_request.state IN ('queued', 'running', 'submitted'))
                    AND NOT EXISTS (
                      SELECT 1 FROM cluster_fragment_index_locations l
                       WHERE l.cache_key = j.cache_key)
                  GROUP BY j.cache_key
                  ORDER BY MIN(j.updated_at_ms), j.cache_key LIMIT $2",
                params!(older_than_ms, limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>();
        let mut statements = Vec::with_capacity(candidates.len() * 2 + 1);
        for cache_key in &candidates {
            statements.push((
                "DELETE FROM cluster_fragment_index_heads
                  WHERE generation_cache_key = $1
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                      WHERE cache_key = $1 AND updated_at_ms < $2)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                      WHERE cache_key = $1 AND (state IN ('queued', 'running')
                        OR updated_at_ms >= $2))
                    AND NOT EXISTS (SELECT 1 FROM analysis_requests
                      WHERE result_cache_key = $1
                        AND state IN ('queued', 'running', 'submitted'))
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                      WHERE cache_key = $1)"
                    .to_owned(),
                params!(cache_key, older_than_ms),
            ));
            statements.push((
                "DELETE FROM cluster_fragment_index_artifacts
                  WHERE cache_key = $1
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                      WHERE cache_key = $1
                        AND state IN ('ready', 'failed', 'cancelled')
                        AND updated_at_ms < $2)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                      WHERE cache_key = $1 AND (state IN ('queued', 'running')
                        OR updated_at_ms >= $2))
                    AND NOT EXISTS (SELECT 1 FROM analysis_requests
                      WHERE result_cache_key = $1
                        AND state IN ('queued', 'running', 'submitted'))
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                      WHERE cache_key = $1)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_heads
                      WHERE generation_cache_key = $1)"
                    .to_owned(),
                params!(cache_key, older_than_ms),
            ));
        }
        statements.push((
            "DELETE FROM cluster_fragment_index_jobs
              WHERE (cache_key, target_node_id) IN (
                SELECT terminal_job.cache_key, terminal_job.target_node_id
                  FROM cluster_fragment_index_jobs terminal_job
                 WHERE (terminal_job.state IN ('ready', 'cancelled') OR (
                   terminal_job.state = 'failed' AND (
                     NOT EXISTS (SELECT 1 FROM files current_file
                       WHERE current_file.id = terminal_job.file_id
                         AND current_file.size = terminal_job.source_size
                         AND current_file.mtime = terminal_job.source_mtime)
                     OR EXISTS (SELECT 1 FROM analysis_requests request
                       WHERE request.result_cache_key = terminal_job.cache_key
                         AND request.target_node_id = terminal_job.target_node_id
                         AND request.force_rebuild = 1))))
                   AND terminal_job.updated_at_ms < $1
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts artifact
                     WHERE artifact.cache_key = terminal_job.cache_key)
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_heads head
                     WHERE head.generation_cache_key = terminal_job.cache_key)
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                     WHERE location.cache_key = terminal_job.cache_key)
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs active_job
                     WHERE active_job.cache_key = terminal_job.cache_key
                       AND (active_job.state IN ('queued', 'running')
                         OR active_job.updated_at_ms >= $1))
                   AND NOT EXISTS (SELECT 1 FROM analysis_requests active_request
                     WHERE active_request.result_cache_key = terminal_job.cache_key
                       AND active_request.state IN ('queued', 'running', 'submitted'))
                 ORDER BY terminal_job.updated_at_ms, terminal_job.cache_key,
                          terminal_job.target_node_id
                 LIMIT $2
              )"
            .to_owned(),
            params!(older_than_ms, limit),
        ));
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(candidates
            .into_iter()
            .zip(results.chunks_exact(2))
            .filter_map(|(key, result)| (result[1] == 1).then_some(key))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{
        ANALYSIS_HISTORY_INDEX_STATEMENTS, ANALYSIS_REQUEST_SCHEMA_STATEMENTS,
        FRAGMENT_INDEX_SCHEMA_STATEMENTS,
    };
    use crate::store::fragment_index_cluster::{
        ANALYSIS_HISTORY_INDEX_SCHEMA, ANALYSIS_REQUESTS_SCHEMA, CLUSTER_FRAGMENT_INDEX_SCHEMA,
    };

    fn fixture() -> Connection {
        let connection = Connection::open_in_memory().expect("in-memory schema fixture");
        connection
            .execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY,
                    size INTEGER NOT NULL,
                    mtime INTEGER NOT NULL
                 ) STRICT;",
            )
            .expect("files prerequisite");
        connection
    }

    fn schema_objects(connection: &Connection) -> Vec<(String, String, String, String)> {
        let mut statement = connection
            .prepare(
                "SELECT type, name, tbl_name, COALESCE(sql, '')
                   FROM sqlite_master
                  WHERE name LIKE 'cluster_fragment_index_%'
                     OR name LIKE 'analysis_requests%'
                  ORDER BY type, name",
            )
            .expect("schema object query");
        statement
            .query_map([], |row| {
                let sql: String = row.get(3)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    sql.split_whitespace().collect::<Vec<_>>().join(" "),
                ))
            })
            .expect("schema object rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("schema object values")
    }

    #[test]
    fn replicated_migration_statements_match_sqlite_schema_versions() {
        assert_eq!(FRAGMENT_INDEX_SCHEMA_STATEMENTS.len(), 8);
        assert_eq!(ANALYSIS_REQUEST_SCHEMA_STATEMENTS.len(), 7);
        assert_eq!(ANALYSIS_HISTORY_INDEX_STATEMENTS.len(), 2);

        let sqlite = fixture();
        sqlite
            .execute_batch(CLUSTER_FRAGMENT_INDEX_SCHEMA)
            .expect("SQLite v30 fragment-index schema");
        sqlite
            .execute_batch(ANALYSIS_REQUESTS_SCHEMA)
            .expect("SQLite v31 analysis-request schema");
        sqlite
            .execute_batch(ANALYSIS_HISTORY_INDEX_SCHEMA)
            .expect("SQLite v33 analysis-history indexes");

        let replicated = fixture();
        for sql in FRAGMENT_INDEX_SCHEMA_STATEMENTS
            .iter()
            .chain(ANALYSIS_REQUEST_SCHEMA_STATEMENTS)
            .chain(ANALYSIS_HISTORY_INDEX_STATEMENTS)
        {
            replicated
                .execute_batch(sql)
                .expect("replicated migration statement");
        }

        assert_eq!(schema_objects(&replicated), schema_objects(&sqlite));
    }
}
