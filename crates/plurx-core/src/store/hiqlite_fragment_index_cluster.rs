//! Replicated catalog and fenced queue for content-addressed fragment indexes.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::fragment_index_cluster::{ANALYSIS_CANONICAL_CTE, ANALYSIS_SUMMARY_CTE};
use super::hiqlite::{database_error, validate_sql, HiqliteAuthStore};
use super::{
    cluster_fragment_index_key, AnalysisFileLabel, AnalysisHistoryCursor, AnalysisHistoryFilter,
    AnalysisHistoryPage, AnalysisHistoryQuery, AnalysisHistoryRow, AnalysisRequest,
    AnalysisStatusSummary, ClusterFragmentIndexArtifact, ClusterFragmentIndexJob,
    ClusterFragmentIndexLocation, ClusterFragmentIndexStore, FragmentIndexSourceObservation,
    NewAnalysisRequest, NewClusterFragmentIndexJob,
};
use crate::error::StoreError;

const MAX_ATTEMPTS: i64 = 5;
const MAX_ACTIVE_JOBS: i64 = 4_096;
const MAX_ERROR_CODE_BYTES: usize = 64;
const MAX_LOCAL_EXCLUSIONS: usize = MAX_ACTIVE_JOBS as usize;
const CLAIM_SCAN_LIMIT: i64 = MAX_ACTIVE_JOBS;
const QUEUE_ELIGIBILITY_MS: i64 = 6 * 60 * 60 * 1_000;
const MAX_ANALYSIS_REQUESTS: i64 = 4_096;
const MAX_LIST_ROWS: i64 = 500;

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

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    let mut statements = fragment_index_schema_migration_statements()?;
    statements.extend(analysis_request_schema_migration_statements()?);
    statements.extend(analysis_history_index_migration_statements()?);
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
    pipeline_sha256, state, COALESCE(owner_node_id, '') AS owner_node_id, fence,
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
    force_rebuild, target_node_id, state, COALESCE(owner_node_id, '') AS owner_node_id,
    fence, COALESCE(lease_expires_ms, 0) AS lease_expires_ms, attempts, not_before_ms,
    COALESCE(result_cache_key, '') AS result_cache_key,
    COALESCE(last_error_code, '') AS last_error_code, created_at_ms, updated_at_ms";

struct RequestRow(AnalysisRequest);

impl From<&mut Row<'_>> for RequestRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self(AnalysisRequest {
            request_id: row.get("request_id"),
            file_id: row.get("file_id"),
            source_size: row.get("source_size"),
            source_mtime: row.get("source_mtime"),
            component: row.get("component"),
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
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
        })
    }
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
    action, owner_node_id, lease_expires_ms, attempts, not_before_ms, request_error_code,
    job_error_code, created_at_ms, updated_at_ms, pipeline_version, source_size";

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
    COALESCE(page.lease_expires_ms, 0) AS lease_expires_ms,
    COALESCE(page.attempts, 0) AS attempts, COALESCE(page.not_before_ms, 0) AS not_before_ms,
    COALESCE(page.request_error_code, '') AS request_error_code,
    COALESCE(page.job_error_code, '') AS job_error_code,
    COALESCE(page.created_at_ms, 0) AS created_at_ms,
    COALESCE(page.updated_at_ms, 0) AS updated_at_ms,
    COALESCE(page.pipeline_version, '') AS pipeline_version,
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
            lease_expires_ms: row.get("lease_expires_ms"),
            attempts: row.get("attempts"),
            not_before_ms: row.get("not_before_ms"),
            request_error_code: row.get("request_error_code"),
            job_error_code: row.get("job_error_code"),
            created_at_ms: row.get("created_at_ms"),
            updated_at_ms: row.get("updated_at_ms"),
            pipeline_version: row.get("pipeline_version"),
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
}

fn valid_request(request: &NewAnalysisRequest) -> bool {
    !request.request_id.is_empty()
        && request.request_id.len() <= 64
        && request.file_id > 0
        && request.source_size >= 0
        && request.component == "fragment_index"
        && !request.target_node_id.is_empty()
        && request.target_node_id.len() <= 128
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
        self.execute(
            "INSERT INTO analysis_requests
                (request_id, file_id, source_size, source_mtime, component,
                 force_rebuild, target_node_id, state, owner_node_id, fence,
                 lease_expires_ms, attempts, not_before_ms, result_cache_key,
                 last_error_code, created_at_ms, updated_at_ms)
             SELECT $1, $2, $3, $4, $5, $6, $7, 'queued', NULL, 0,
                    NULL, 0, $8, NULL, NULL, $9, $9
              WHERE EXISTS (SELECT 1 FROM files WHERE id = $2 AND size = $3 AND mtime = $4)
                AND (SELECT COUNT(*) FROM analysis_requests
                      WHERE state IN ('queued', 'running', 'submitted')) < $10
             ON CONFLICT DO NOTHING",
            params!(
                &request.request_id,
                request.file_id,
                request.source_size,
                request.source_mtime,
                &request.component,
                if request.force_rebuild { 1_i64 } else { 0_i64 },
                &request.target_node_id,
                request.not_before_ms,
                request.created_at_ms,
                MAX_ANALYSIS_REQUESTS
            ),
        )
        .await?;
        self.client()
            .query_consistent_map::<RequestRow, _>(
                format!(
                    "SELECT {REQUEST_COLS} FROM analysis_requests
                      WHERE request_id = $1 OR (
                        file_id = $2 AND source_size = $3 AND source_mtime = $4
                        AND component = $5 AND target_node_id = $6
                        AND state IN ('queued', 'running', 'submitted'))
                      ORDER BY CASE WHEN request_id = $1 THEN 0 ELSE 1 END,
                        created_at_ms, request_id LIMIT 1"
                ),
                params!(
                    &request.request_id,
                    request.file_id,
                    request.source_size,
                    request.source_mtime,
                    &request.component,
                    &request.target_node_id
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
        self.execute(
            "UPDATE analysis_requests
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    last_error_code = 'attempt_limit', updated_at_ms = $1
              WHERE attempts >= $2 AND (
                (state = 'queued' AND not_before_ms <= $1)
                OR (state = 'running' AND COALESCE(lease_expires_ms, 0) <= $1))",
            params!(now_ms, MAX_ATTEMPTS),
        )
        .await?;
        for _ in 0..8 {
            let candidate = self
                .client()
                .query_consistent_map::<RequestRow, _>(
                    format!(
                        "SELECT {REQUEST_COLS} FROM analysis_requests
                          WHERE target_node_id = $1 AND attempts < $2
                            AND ((state = 'queued' AND not_before_ms <= $3)
                              OR (state = 'running' AND lease_expires_ms <= $3))
                          ORDER BY created_at_ms, request_id LIMIT 1"
                    ),
                    params!(node_id, MAX_ATTEMPTS, now_ms),
                )
                .await?
                .into_iter()
                .next()
                .map(|row| row.0);
            let Some(mut candidate) = candidate else {
                return Ok(None);
            };
            let changed = self
                .execute(
                    "UPDATE analysis_requests
                        SET state = 'running', owner_node_id = $1, fence = fence + 1,
                            lease_expires_ms = $2, attempts = attempts + 1,
                            last_error_code = NULL, updated_at_ms = $3
                      WHERE request_id = $4 AND fence = $5
                        AND ((state = 'queued' AND not_before_ms <= $3)
                          OR (state = 'running' AND lease_expires_ms <= $3))",
                    params!(
                        node_id,
                        lease_expires_ms,
                        now_ms,
                        &candidate.request_id,
                        candidate.fence
                    ),
                )
                .await?;
            if changed == 1 {
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
        Ok(self
            .execute(
                "UPDATE analysis_requests SET lease_expires_ms = $1, updated_at_ms = $2
                  WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2 AND $1 > $2",
                params!(lease_expires_ms, now_ms, request_id, node_id, fence),
            )
            .await?
            == 1)
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
        {
            return Err(StoreError::Task("invalid analysis submission".to_owned()));
        }
        let results = self
            .client()
            .txn(vec![
                (
                    "UPDATE analysis_requests
                        SET state = 'submitted', lease_expires_ms = NULL,
                            result_cache_key = $1, last_error_code = NULL, updated_at_ms = $2
                      WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                        AND fence = $5 AND lease_expires_ms > $2
                        AND file_id = $6 AND source_size = $7 AND source_mtime = $8
                        AND EXISTS (SELECT 1 FROM files
                              WHERE id = $6 AND size = $7 AND mtime = $8)
                        AND (
                          ($9 = 1
                            AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                                  WHERE state IN ('queued', 'running')) < $10
                            AND (NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                              WHERE cache_key = $1)
                              OR EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                          WHERE cache_key = $1
                                            AND state IN ('ready', 'failed', 'cancelled'))))
                          OR
                          ($9 = 0 AND (
                            EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                     WHERE cache_key = $1
                                       AND state IN ('queued', 'running', 'ready'))
                            OR ((SELECT COUNT(*) FROM cluster_fragment_index_jobs
                                    WHERE state IN ('queued', 'running')) < $10
                              AND (NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                                WHERE cache_key = $1)
                                OR EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                            WHERE cache_key = $1 AND (
                                              state = 'cancelled'
                                              OR (state = 'failed' AND (
                                                last_error_code = 'queue_expired'
                                                OR (attempts < $11 AND not_before_ms <= $2))))))))))"
                        .to_owned(),
                    params!(
                        &job.cache_key,
                        now_ms,
                        &request.request_id,
                        &request.owner_node_id,
                        request.fence,
                        job.file_id,
                        job.source_size,
                        job.source_mtime,
                        if request.force_rebuild { 1_i64 } else { 0_i64 },
                        MAX_ACTIVE_JOBS,
                        MAX_ATTEMPTS
                    ),
                ),
                (
                    "INSERT INTO cluster_fragment_index_jobs
                        (cache_key, file_id, source_size, source_mtime, source_sha256,
                         pipeline_sha256, state, owner_node_id, fence, lease_expires_ms,
                         attempts, not_before_ms, created_at_ms, updated_at_ms, last_error_code)
                     SELECT $1, $2, $3, $4, $5, $6, 'queued', NULL, 0, NULL, 0,
                            $7, $8, $8, NULL
                      WHERE EXISTS (SELECT 1 FROM analysis_requests
                        WHERE request_id = $9 AND state = 'submitted' AND fence = $10
                          AND file_id = $2 AND source_size = $3 AND source_mtime = $4
                          AND result_cache_key = $1 AND updated_at_ms = $8
                          AND owner_node_id = $11)
                     ON CONFLICT(cache_key) DO UPDATE SET
                        file_id = excluded.file_id, source_size = excluded.source_size,
                        source_mtime = excluded.source_mtime,
                        source_sha256 = excluded.source_sha256,
                        pipeline_sha256 = excluded.pipeline_sha256,
                        state = 'queued',
                        owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = CASE WHEN $12 = 1 THEN 0
                          WHEN cluster_fragment_index_jobs.state = 'cancelled'
                            OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                            OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN 0
                          ELSE cluster_fragment_index_jobs.attempts END,
                        not_before_ms = excluded.not_before_ms,
                        created_at_ms = excluded.created_at_ms,
                        updated_at_ms = excluded.updated_at_ms, last_error_code = NULL
                      WHERE ($12 = 1 AND cluster_fragment_index_jobs.state
                                          IN ('ready', 'failed', 'cancelled'))
                         OR ($12 = 0 AND (
                           cluster_fragment_index_jobs.state = 'cancelled'
                           OR (cluster_fragment_index_jobs.state = 'failed'
                             AND (cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                               OR (cluster_fragment_index_jobs.attempts < $13
                                 AND cluster_fragment_index_jobs.not_before_ms <= $8)))))"
                        .to_owned(),
                    params!(
                        &job.cache_key,
                        job.file_id,
                        job.source_size,
                        job.source_mtime,
                        &job.source_sha256,
                        &job.pipeline_sha256,
                        job.not_before_ms,
                        now_ms,
                        &request.request_id,
                        request.fence,
                        &request.owner_node_id,
                        if request.force_rebuild { 1_i64 } else { 0_i64 },
                        MAX_ATTEMPTS
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
        match (results.first().copied(), results.get(2).copied()) {
            (Some(1), Some(1)) => Ok(true),
            (Some(0), Some(0)) => Ok(false),
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
        Ok(self
            .execute(
                "UPDATE analysis_requests
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = CASE WHEN $1 = 0 AND attempts > 0
                          THEN attempts - 1 ELSE attempts END,
                        not_before_ms = $2, last_error_code = $3, updated_at_ms = $4
                  WHERE request_id = $5 AND state = 'running' AND owner_node_id = $6
                    AND fence = $7 AND lease_expires_ms > $4",
                params!(
                    if charge_attempt { 1_i64 } else { 0_i64 },
                    retry_at_ms,
                    error_code,
                    now_ms,
                    &request.request_id,
                    &request.owner_node_id,
                    request.fence
                ),
            )
            .await?
            == 1)
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
        Ok(self
            .execute(
                "UPDATE analysis_requests
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = $1, updated_at_ms = $2
                  WHERE request_id = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2",
                params!(error_code, now_ms, request_id, node_id, fence),
            )
            .await?
            == 1)
    }

    async fn settle_analysis_requests(&self, now_ms: i64) -> Result<u64, StoreError> {
        Ok(self
            .execute(
                "UPDATE analysis_requests
                    SET state = COALESCE((SELECT CASE job.state
                            WHEN 'ready' THEN 'ready' WHEN 'failed' THEN 'failed'
                            WHEN 'cancelled' THEN 'cancelled' ELSE analysis_requests.state END
                          FROM cluster_fragment_index_jobs job
                         WHERE job.cache_key = analysis_requests.result_cache_key), state),
                        last_error_code = (SELECT job.last_error_code
                          FROM cluster_fragment_index_jobs job
                         WHERE job.cache_key = analysis_requests.result_cache_key),
                        updated_at_ms = $1
                  WHERE state = 'submitted'
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs job
                         WHERE job.cache_key = analysis_requests.result_cache_key
                           AND job.state IN ('ready', 'failed', 'cancelled'))",
                params!(now_ms),
            )
            .await? as u64)
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
                   SELECT request_id, updated_at_ms,
                          ROW_NUMBER() OVER (
                            PARTITION BY file_id, component, target_node_id
                            ORDER BY updated_at_ms DESC, request_id DESC) AS generation_rank
                     FROM analysis_requests
                    WHERE state IN ('ready', 'failed', 'cancelled')
                 )
                 DELETE FROM analysis_requests WHERE request_id IN (
                   SELECT request_id FROM ranked
                    WHERE updated_at_ms < $1 OR generation_rank > 20
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

    async fn analysis_history(
        &self,
        query: &AnalysisHistoryQuery,
    ) -> Result<AnalysisHistoryPage, StoreError> {
        let limit = query.limit.clamp(10, 100);
        let filter = analysis_filter_code(query.filter);
        let search = analysis_search_pattern(&query.search);
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
             ), page AS (
               SELECT {HISTORY_COLS}, sort_rank FROM matching
                WHERE $3 < 0 OR sort_rank > $3
                   OR (sort_rank = $3 AND updated_at_ms < $4)
                   OR (sort_rank = $3 AND updated_at_ms = $4 AND row_key > $5)
                ORDER BY sort_rank, updated_at_ms DESC, row_key
                LIMIT $6
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
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError> {
        Ok(self
            .client()
            .query_consistent_map::<JobRow, _>(
                format!("SELECT {JOB_COLS} FROM cluster_fragment_index_jobs WHERE cache_key = $1"),
                params!(cache_key),
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
                   FROM cluster_fragment_index_artifacts WHERE cache_key = $1",
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
        let eligibility_cutoff = job.created_at_ms.saturating_sub(QUEUE_ELIGIBILITY_MS);
        self.execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = $1, updated_at_ms = $1,
                    last_error_code = 'queue_expired'
              WHERE (state = 'queued' AND created_at_ms < $2)
                 OR (state = 'running' AND attempts >= $3
                   AND COALESCE(lease_expires_ms, 0) <= $1)",
            params!(job.created_at_ms, eligibility_cutoff, MAX_ATTEMPTS),
        )
        .await?;
        Ok(self
            .execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, state, owner_node_id, fence, lease_expires_ms,
                     attempts, not_before_ms, created_at_ms, updated_at_ms)
                 SELECT $1, $2, $3, $4, $5, $6, 'queued', NULL, 0, NULL, 0, $7, $8, $8
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = $2 AND size = $3 AND mtime = $4)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = $1)
                    AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < $9
                 ON CONFLICT(cache_key) DO UPDATE SET
                    file_id = excluded.file_id, source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    pipeline_sha256 = excluded.pipeline_sha256,
                    state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                    attempts = CASE
                        WHEN cluster_fragment_index_jobs.state = 'cancelled'
                          OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                          OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN 0
                        ELSE cluster_fragment_index_jobs.attempts END,
                    not_before_ms = excluded.not_before_ms,
                    created_at_ms = CASE
                        WHEN cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                        THEN excluded.created_at_ms
                        ELSE cluster_fragment_index_jobs.created_at_ms END,
                    updated_at_ms = excluded.updated_at_ms,
                    last_error_code = NULL
                  WHERE cluster_fragment_index_jobs.state = 'cancelled'
                     OR (cluster_fragment_index_jobs.state = 'failed'
                       AND cluster_fragment_index_jobs.last_error_code = 'queue_expired')
                     OR (cluster_fragment_index_jobs.state = 'failed'
                       AND cluster_fragment_index_jobs.attempts < $10
                       AND cluster_fragment_index_jobs.not_before_ms <= excluded.created_at_ms)",
                params!(
                    &job.cache_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    &job.source_sha256,
                    &job.pipeline_sha256,
                    job.not_before_ms,
                    job.created_at_ms,
                    MAX_ACTIVE_JOBS,
                    MAX_ATTEMPTS
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
        self.execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = $1, updated_at_ms = $1,
                    last_error_code = 'queue_expired'
              WHERE (state = 'queued' AND created_at_ms < $2)
                 OR (state = 'running' AND attempts >= $3
                   AND COALESCE(lease_expires_ms, 0) <= $1)",
            params!(
                now_ms,
                now_ms.saturating_sub(QUEUE_ELIGIBILITY_MS),
                MAX_ATTEMPTS
            ),
        )
        .await?;
        for _ in 0..8 {
            let sql = format!(
                "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs
                  WHERE ((state = 'queued' AND not_before_ms <= $1)
                      OR (state = 'running' AND lease_expires_ms <= $1))
                    AND attempts < $2 AND fence < 9223372036854775807
                  ORDER BY created_at_ms, cache_key LIMIT $3"
            );
            let candidates = self
                .client()
                .query_consistent_map::<JobRow, _>(
                    sql,
                    params!(now_ms, MAX_ATTEMPTS, CLAIM_SCAN_LIMIT),
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
            let changed = self
                .execute(
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'running', owner_node_id = $1, fence = fence + 1,
                            lease_expires_ms = $2, attempts = attempts + 1,
                            last_error_code = NULL, updated_at_ms = $3
                      WHERE cache_key = $4 AND fence = $5
                        AND ((state = 'queued' AND not_before_ms <= $3)
                          OR (state = 'running' AND lease_expires_ms <= $3))",
                    params!(
                        node_id,
                        lease_expires_ms,
                        now_ms,
                        &candidate.cache_key,
                        candidate.fence
                    ),
                )
                .await?;
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
        let eligibility_cutoff = replacement
            .created_at_ms
            .saturating_sub(QUEUE_ELIGIBILITY_MS);
        self.execute(
            "UPDATE cluster_fragment_index_jobs
                SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                    not_before_ms = $1, updated_at_ms = $1,
                    last_error_code = 'queue_expired'
              WHERE (state = 'queued' AND created_at_ms < $2)
                 OR (state = 'running' AND attempts >= $3
                   AND COALESCE(lease_expires_ms, 0) <= $1)",
            params!(replacement.created_at_ms, eligibility_cutoff, MAX_ATTEMPTS),
        )
        .await?;
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET file_id = $2, source_size = $3, source_mtime = $4,
                        source_sha256 = $5, pipeline_sha256 = $6,
                        state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = CASE
                          WHEN state = 'ready' OR last_error_code = 'queue_expired'
                            OR file_id <> $2 THEN 0
                          ELSE attempts END,
                        not_before_ms = $7, created_at_ms = $8, updated_at_ms = $8,
                        last_error_code = 'holders_unavailable'
                  WHERE cache_key = $1
                    AND (state = 'ready'
                      OR (state = 'failed' AND last_error_code = 'queue_expired')
                      OR (state = 'failed'
                      AND attempts < $9 AND not_before_ms <= $8))
                    AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < $10
                    AND EXISTS (SELECT 1 FROM files
                      WHERE id = $2 AND size = $3 AND mtime = $4)
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                      WHERE cache_key = $1 AND source_size = $3
                        AND source_sha256 = $5 AND pipeline_sha256 = $6)",
                params!(
                    &replacement.cache_key,
                    replacement.file_id,
                    replacement.source_size,
                    replacement.source_mtime,
                    &replacement.source_sha256,
                    &replacement.pipeline_sha256,
                    replacement.not_before_ms,
                    replacement.created_at_ms,
                    MAX_ATTEMPTS,
                    MAX_ACTIVE_JOBS
                ),
            )
            .await?
            == 1)
    }

    async fn renew_cluster_fragment_index(
        &self,
        cache_key: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET lease_expires_ms = $1, updated_at_ms = $2
                  WHERE cache_key = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2 AND $1 > $2",
                params!(lease_expires_ms, now_ms, cache_key, node_id, fence),
            )
            .await?
            == 1)
    }

    async fn yield_cluster_fragment_index(
        &self,
        cache_key: &str,
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
                  WHERE cache_key = $3 AND state = 'running' AND owner_node_id = $4
                    AND fence = $5 AND lease_expires_ms > $2",
                params!(retry_at_ms, now_ms, cache_key, node_id, fence),
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
            || cluster_fragment_index_key(&artifact.source_sha256, &artifact.pipeline_sha256)
                .as_deref()
                != Some(job.cache_key.as_str())
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
        let results = self
            .client()
            .txn(vec![
                (
                    "INSERT INTO cluster_fragment_index_artifacts
                        (cache_key, file_id, source_size, source_mtime, source_sha256,
                         pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
                      WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                        WHERE cache_key = $1 AND state = 'running' AND owner_node_id = $9
                          AND fence = $11 AND lease_expires_ms > $12
                          AND file_id = $2 AND source_size = $3 AND source_mtime = $4
                          AND source_sha256 = $5 AND pipeline_sha256 = $6)
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
                        job.fence,
                        now_ms
                    ),
                ),
                (
                    "INSERT INTO cluster_fragment_index_locations
                        (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                     SELECT $1, $2, $3, $4, $5
                      WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                        WHERE cache_key = $1 AND state = 'running' AND owner_node_id = $2
                          AND fence = $6 AND lease_expires_ms > $7)
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                          WHERE cache_key = $1 AND source_sha256 = $8
                            AND pipeline_sha256 = $9 AND blob_sha256 = $10 AND bytes = $3)
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
                        job.fence,
                        now_ms,
                        &artifact.source_sha256,
                        &artifact.pipeline_sha256,
                        &artifact.blob_sha256
                    ),
                ),
                (
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                            last_error_code = NULL, updated_at_ms = $1
                      WHERE cache_key = $2 AND state = 'running' AND owner_node_id = $3
                        AND fence = $4 AND lease_expires_ms > $1
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                          WHERE cache_key = $2 AND source_sha256 = $5
                            AND pipeline_sha256 = $6 AND blob_sha256 = $7 AND bytes = $8)"
                        .to_owned(),
                    params!(
                        now_ms,
                        &job.cache_key,
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
        node_id: &str,
        fence: i64,
        error_code: &str,
        now_ms: i64,
        retry_at_ms: i64,
    ) -> Result<bool, StoreError> {
        if error_code.is_empty() || error_code.len() > MAX_ERROR_CODE_BYTES {
            return Err(StoreError::Task(
                "invalid fragment-index failure code".to_owned(),
            ));
        }
        Ok(self
            .execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = $1, not_before_ms = $2, updated_at_ms = $3
                  WHERE cache_key = $4 AND state = 'running' AND owner_node_id = $5
                    AND fence = $6 AND lease_expires_ms > $3",
                params!(error_code, retry_at_ms, now_ms, cache_key, node_id, fence),
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
        let candidates = self
            .client()
            .query_consistent_map::<CacheKeyRow, _>(
                "SELECT j.cache_key AS cache_key
                   FROM cluster_fragment_index_jobs j
                  WHERE j.state IN ('ready', 'failed', 'cancelled')
                    AND j.updated_at_ms < $1
                    AND NOT EXISTS (
                      SELECT 1 FROM cluster_fragment_index_locations l
                       WHERE l.cache_key = j.cache_key AND l.last_seen_at_ms >= $1)
                  ORDER BY j.updated_at_ms, j.cache_key LIMIT $2",
                params!(older_than_ms, limit),
            )
            .await?
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let mut statements = Vec::with_capacity(candidates.len() * 3);
        for cache_key in &candidates {
            statements.push((
                "DELETE FROM cluster_fragment_index_artifacts
                  WHERE cache_key = $1
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                      WHERE cache_key = $1
                        AND state IN ('ready', 'failed', 'cancelled')
                        AND updated_at_ms < $2)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                      WHERE cache_key = $1 AND last_seen_at_ms >= $2)"
                    .to_owned(),
                params!(cache_key, older_than_ms),
            ));
            statements.push((
                "DELETE FROM cluster_fragment_index_locations
                  WHERE cache_key = $1 AND last_seen_at_ms < $2
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = $1)"
                    .to_owned(),
                params!(cache_key, older_than_ms),
            ));
            statements.push((
                "DELETE FROM cluster_fragment_index_jobs
                  WHERE cache_key = $1
                    AND state IN ('ready', 'failed', 'cancelled')
                    AND updated_at_ms < $2
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = $1)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                      WHERE cache_key = $1 AND last_seen_at_ms >= $2)"
                    .to_owned(),
                params!(cache_key, older_than_ms),
            ));
        }
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(candidates
            .into_iter()
            .zip(results.chunks_exact(3))
            .filter_map(|(key, result)| (result[0] == 1).then_some(key))
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
