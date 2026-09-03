use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::fragment_index_cluster::{
    bounded_analysis_backoff_base_secs, bounded_analysis_backoff_max_secs,
    bounded_analysis_max_attempts,
};
use crate::store::fragment_index_cluster::{ANALYSIS_CANONICAL_CTE, ANALYSIS_SUMMARY_CTE};
use crate::store::{
    cluster_fragment_index_generation_key, cluster_fragment_index_key, AnalysisAttempt,
    AnalysisFileLabel, AnalysisHistoryCursor, AnalysisHistoryFilter, AnalysisHistoryPage,
    AnalysisHistoryQuery, AnalysisHistoryRow, AnalysisRequest, AnalysisStatusSummary,
    ClusterFragmentIndexArtifact, ClusterFragmentIndexJob, ClusterFragmentIndexLocation,
    ClusterFragmentIndexStore, FragmentIndexSourceObservation, NewAnalysisRequest,
    NewClusterFragmentIndexJob,
};

const MAX_ACTIVE_JOBS: i64 = 4_096;
const MAX_ERROR_CODE_BYTES: usize = 64;
const MAX_LOCAL_EXCLUSIONS: usize = MAX_ACTIVE_JOBS as usize;
const CLAIM_SCAN_LIMIT: i64 = MAX_ACTIVE_JOBS;
const QUEUE_ELIGIBILITY_MS: i64 = 6 * 60 * 60 * 1_000;
use super::super::MAX_ACTIVE_ANALYSIS_REQUESTS as MAX_ANALYSIS_REQUESTS;
const MAX_LIST_ROWS: i64 = 500;
const MAX_ATTEMPT_HISTORY_PER_REQUEST: i64 = 64;

fn configured_max_attempts(conn: &Connection) -> rusqlite::Result<i64> {
    let raw = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![crate::store::keys::ANALYSIS_MAX_ATTEMPTS],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(bounded_analysis_max_attempts(raw.as_deref()))
}

fn configured_backoff_base_ms(conn: &Connection) -> rusqlite::Result<i64> {
    let raw = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![crate::store::keys::ANALYSIS_BACKOFF_BASE_SECS],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(bounded_analysis_backoff_base_secs(raw.as_deref()).saturating_mul(1_000))
}

fn configured_backoff_max_ms(conn: &Connection) -> rusqlite::Result<i64> {
    let raw = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![crate::store::keys::ANALYSIS_BACKOFF_MAX_SECS],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(bounded_analysis_backoff_max_secs(raw.as_deref()).saturating_mul(1_000))
}

const JOB_COLS: &str = "cache_key, file_id, source_size, source_mtime, source_sha256,
    pipeline_sha256, priority, trigger, target_node_id,
    state, COALESCE(owner_node_id, ''), fence,
    COALESCE(lease_expires_ms, 0), attempts, not_before_ms, created_at_ms, updated_at_ms,
    COALESCE(last_error_code, ''), attempt_errors";

fn job_from_row(row: &Row<'_>) -> rusqlite::Result<ClusterFragmentIndexJob> {
    Ok(ClusterFragmentIndexJob {
        cache_key: row.get(0)?,
        file_id: row.get(1)?,
        source_size: row.get(2)?,
        source_mtime: row.get(3)?,
        source_sha256: row.get(4)?,
        pipeline_sha256: row.get(5)?,
        priority: row.get(6)?,
        trigger: row.get(7)?,
        target_node_id: row.get(8)?,
        state: row.get(9)?,
        owner_node_id: row.get(10)?,
        fence: row.get(11)?,
        lease_expires_ms: row.get(12)?,
        attempts: row.get(13)?,
        not_before_ms: row.get(14)?,
        created_at_ms: row.get(15)?,
        updated_at_ms: row.get(16)?,
        last_error_code: row.get(17)?,
        attempt_errors: row.get(18)?,
    })
}

const REQUEST_COLS: &str = "request_id, file_id, source_size, source_mtime, component,
    pipeline_version, requested_generation, expected_predecessor_generation,
    priority, trigger, force_rebuild,
    target_node_id, state, COALESCE(owner_node_id, ''), fence,
    COALESCE(lease_expires_ms, 0), attempts, not_before_ms,
    COALESCE(result_cache_key, ''), COALESCE(last_error_code, ''),
    cancel_requested, created_at_ms, updated_at_ms";

const HISTORY_COLS: &str = "row_key, request_id, job_id, file_id, item_id, title,
    component, force_rebuild, target_node_id, request_state, job_state, state, disposition,
    action, owner_node_id, claim_epoch, lease_expires_ms, attempts, not_before_ms, request_error_code,
    job_error_code, created_at_ms, updated_at_ms, pipeline_version, requested_generation,
    priority, trigger, cancel_requested, phase, source_size, job_attempt_errors";

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
    COALESCE(page.source_size, 0) AS source_size,
    COALESCE(page.job_attempt_errors, '') AS job_attempt_errors";

fn request_from_row(row: &Row<'_>) -> rusqlite::Result<AnalysisRequest> {
    Ok(AnalysisRequest {
        request_id: row.get(0)?,
        file_id: row.get(1)?,
        source_size: row.get(2)?,
        source_mtime: row.get(3)?,
        component: row.get(4)?,
        pipeline_version: row.get(5)?,
        requested_generation: row.get(6)?,
        expected_predecessor_generation: row.get(7)?,
        priority: row.get(8)?,
        trigger: row.get(9)?,
        force_rebuild: row.get::<_, i64>(10)? != 0,
        target_node_id: row.get(11)?,
        state: row.get(12)?,
        owner_node_id: row.get(13)?,
        fence: row.get(14)?,
        lease_expires_ms: row.get(15)?,
        attempts: row.get(16)?,
        not_before_ms: row.get(17)?,
        result_cache_key: row.get(18)?,
        last_error_code: row.get(19)?,
        cancel_requested: row.get::<_, i64>(20)? != 0,
        created_at_ms: row.get(21)?,
        updated_at_ms: row.get(22)?,
    })
}

fn attempt_from_row(row: &Row<'_>) -> rusqlite::Result<AnalysisAttempt> {
    Ok(AnalysisAttempt {
        request_id: row.get(0)?,
        attempt: row.get(1)?,
        claim_node_id: row.get(2)?,
        claim_epoch: row.get(3)?,
        claim_expires_at_ms: row.get(4)?,
        phase: row.get(5)?,
        started_at_ms: row.get(6)?,
        phase_updated_at_ms: row.get(7)?,
        terminal_code: row.get(8)?,
    })
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

fn history_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<(AnalysisHistoryRow, AnalysisHistoryCursor, i64)> {
    let history = AnalysisHistoryRow {
        row_key: row.get(0)?,
        request_id: row.get(1)?,
        job_id: row.get(2)?,
        file_id: row.get(3)?,
        item_id: row.get(4)?,
        title: row.get(5)?,
        component: row.get(6)?,
        force_rebuild: row.get::<_, i64>(7)? != 0,
        target_node_id: row.get(8)?,
        request_state: row.get(9)?,
        job_state: row.get(10)?,
        state: row.get(11)?,
        disposition: row.get(12)?,
        action: row.get(13)?,
        owner_node_id: row.get(14)?,
        claim_epoch: row.get(15)?,
        lease_expires_ms: row.get(16)?,
        attempts: row.get(17)?,
        not_before_ms: row.get(18)?,
        request_error_code: row.get(19)?,
        job_error_code: row.get(20)?,
        created_at_ms: row.get(21)?,
        updated_at_ms: row.get(22)?,
        pipeline_version: row.get(23)?,
        requested_generation: row.get(24)?,
        priority: row.get(25)?,
        trigger: row.get(26)?,
        cancel_requested: row.get::<_, i64>(27)? != 0,
        phase: row.get(28)?,
        source_size: row.get(29)?,
        job_attempt_errors: row.get(30)?,
    };
    let cursor = AnalysisHistoryCursor {
        sort_rank: row.get(31)?,
        updated_at_ms: history.updated_at_ms,
        row_key: history.row_key.clone(),
    };
    Ok((history, cursor, row.get(32)?))
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

fn artifact_from_row(row: &Row<'_>) -> rusqlite::Result<ClusterFragmentIndexArtifact> {
    Ok(ClusterFragmentIndexArtifact {
        cache_key: row.get(0)?,
        file_id: row.get(1)?,
        source_size: row.get(2)?,
        source_mtime: row.get(3)?,
        source_sha256: row.get(4)?,
        pipeline_sha256: row.get(5)?,
        blob_sha256: row.get(6)?,
        bytes: row.get(7)?,
        built_by_node_id: row.get(8)?,
        built_at_ms: row.get(9)?,
    })
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
impl ClusterFragmentIndexStore for SqliteStore {
    async fn enqueue_analysis_request(
        &self,
        request: &NewAnalysisRequest,
    ) -> Result<AnalysisRequest, StoreError> {
        if !valid_request(request) {
            return Err(StoreError::Task("invalid analysis request".to_owned()));
        }
        let request = request.clone();
        let semantic_fingerprint = crate::segplan::argv_fingerprint(&[
            "timeline-annotations".to_owned(),
            request.pipeline_version.clone(),
        ]);
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "INSERT INTO analysis_requests
                    (request_id, file_id, source_size, source_mtime, component,
                     pipeline_version, requested_generation, expected_predecessor_generation,
                     priority, trigger,
                     force_rebuild, target_node_id, state, owner_node_id, fence,
                     lease_expires_ms, attempts, not_before_ms, result_cache_key,
                     last_error_code, cancel_requested, created_at_ms, updated_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                        CASE WHEN ?5 = 'skip_markers' THEN COALESCE((
                          SELECT generation_id FROM timeline_annotation_sets
                           WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                        ), '') ELSE '' END,
                        ?8, ?9, ?10, ?11,
                        CASE WHEN ?10 = 0 AND ?5 = 'skip_markers' AND EXISTS (
                          SELECT 1 FROM timeline_annotation_sets
                           WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                             AND argv_fingerprint = ?12 AND publication_priority = 'forced'
                        ) THEN 'ready' ELSE 'queued' END,
                        NULL, 0, NULL, 0, ?13,
                        CASE WHEN ?10 = 0 AND ?5 = 'skip_markers' THEN (
                          SELECT generation_id FROM timeline_annotation_sets
                           WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                             AND argv_fingerprint = ?12 AND publication_priority = 'forced'
                        ) ELSE NULL END,
                        NULL, 0, ?14, ?14
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = ?2 AND size = ?3 AND mtime = ?4)
                    AND (SELECT COUNT(*) FROM analysis_requests
                          WHERE state IN ('queued', 'running', 'submitted')) < ?15
                    AND (?10 = 1 OR (
                      NOT EXISTS (SELECT 1 FROM analysis_requests
                        WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                          AND component = ?5 AND force_rebuild = 1
                          AND target_node_id = ?11
                          AND state IN ('queued', 'running', 'submitted', 'ready'))
                      AND NOT EXISTS (SELECT 1 FROM analysis_requests
                        WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                          AND component = ?5 AND pipeline_version = ?6
                          AND requested_generation = ?7 AND target_node_id = ?11)
                    ))
                 ON CONFLICT DO NOTHING",
                params![
                    request.request_id,
                    request.file_id,
                    request.source_size,
                    request.source_mtime,
                    request.component,
                    request.pipeline_version,
                    request.requested_generation,
                    request.priority,
                    request.trigger,
                    if request.force_rebuild { 1_i64 } else { 0_i64 },
                    request.target_node_id,
                    semantic_fingerprint,
                    request.not_before_ms,
                    request.created_at_ms,
                    MAX_ANALYSIS_REQUESTS,
                ],
            )?;
            if request.force_rebuild {
                transaction.execute(
                    "UPDATE analysis_attempts
                        SET phase = 'canceled', phase_updated_at_ms = ?1,
                            terminal_code = 'publication_superseded'
                      WHERE (request_id, claim_epoch) IN (
                        SELECT older.request_id, older.fence FROM analysis_requests older
                         WHERE older.file_id = ?2 AND older.source_size = ?3
                           AND older.source_mtime = ?4 AND older.component = ?5
                           AND older.pipeline_version = ?6 AND older.force_rebuild = 0
                           AND older.state IN ('queued','running','submitted')
                           AND EXISTS (SELECT 1 FROM analysis_requests successor
                             WHERE successor.request_id = ?7 AND successor.force_rebuild = 1))",
                    params![
                        request.created_at_ms,
                        request.file_id,
                        request.source_size,
                        request.source_mtime,
                        request.component,
                        request.pipeline_version,
                        request.request_id,
                    ],
                )?;
                transaction.execute(
                    "UPDATE analysis_requests
                        SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                            fence = fence + 1, cancel_requested = 1,
                            last_error_code = 'publication_superseded', updated_at_ms = ?1
                      WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                        AND component = ?5 AND pipeline_version = ?6
                        AND force_rebuild = 0 AND state IN ('queued','running','submitted')
                        AND EXISTS (SELECT 1 FROM analysis_requests successor
                          WHERE successor.request_id = ?7 AND successor.force_rebuild = 1)",
                    params![
                        request.created_at_ms,
                        request.file_id,
                        request.source_size,
                        request.source_mtime,
                        request.component,
                        request.pipeline_version,
                        request.request_id,
                    ],
                )?;
                transaction.execute(
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'cancelled', owner_node_id = NULL,
                            lease_expires_ms = NULL, fence = fence + 1,
                            last_error_code = 'publication_superseded', updated_at_ms = ?1
                      WHERE state IN ('queued','running')
                        AND (cache_key, target_node_id) IN (
                          SELECT older.result_cache_key, older.target_node_id
                            FROM analysis_requests older
                           WHERE older.file_id = ?2 AND older.source_size = ?3
                             AND older.source_mtime = ?4 AND older.component = ?5
                             AND older.pipeline_version = ?6 AND older.force_rebuild = 0
                             AND older.state = 'cancelled'
                             AND older.last_error_code = 'publication_superseded'
                             AND older.updated_at_ms = ?1)
                        AND NOT EXISTS (SELECT 1 FROM analysis_requests active
                          WHERE active.result_cache_key = cluster_fragment_index_jobs.cache_key
                            AND active.target_node_id = cluster_fragment_index_jobs.target_node_id
                            AND active.state IN ('queued','running','submitted'))",
                    params![
                        request.created_at_ms,
                        request.file_id,
                        request.source_size,
                        request.source_mtime,
                        request.component,
                        request.pipeline_version,
                    ],
                )?;
            }
            let active = transaction
                .query_row(
                    &format!(
                        "SELECT {REQUEST_COLS} FROM analysis_requests
                          WHERE file_id = ?1 AND source_size = ?2 AND source_mtime = ?3
                            AND component = ?4
                            AND ((pipeline_version = ?5 AND requested_generation = ?6
                                  AND target_node_id = ?7)
                              OR (force_rebuild = 1 AND pipeline_version = ?5
                                AND target_node_id = ?7
                                AND (state IN ('queued', 'running', 'submitted')
                                  OR (?9 = 0 AND state = 'ready'))))
                            AND (request_id = ?8
                              OR state IN ('queued', 'running', 'submitted')
                              OR (?9 = 0 AND requested_generation = ?6)
                              OR (?9 = 0 AND force_rebuild = 1 AND state = 'ready'
                                AND pipeline_version = ?5 AND target_node_id = ?7))
                          ORDER BY CASE WHEN request_id = ?8 THEN 0
                              WHEN force_rebuild = 1
                                AND state IN ('queued', 'running', 'submitted') THEN 1
                              WHEN state IN ('queued', 'running', 'submitted') THEN 2
                              ELSE 3 END,
                            created_at_ms DESC, request_id DESC LIMIT 1"
                    ),
                    params![
                        request.file_id,
                        request.source_size,
                        request.source_mtime,
                        request.component,
                        request.pipeline_version,
                        request.requested_generation,
                        request.target_node_id,
                        request.request_id,
                        if request.force_rebuild { 1_i64 } else { 0_i64 },
                    ],
                    request_from_row,
                )
                .optional()?;
            transaction.commit()?;
            active.ok_or_else(|| {
                StoreError::Task(
                    "analysis source changed or the active request queue is full".to_owned(),
                )
            })
        })
        .await
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
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            let max_attempts = configured_max_attempts(conn)?;
            let backoff_base_ms = configured_backoff_base_ms(conn)?;
            let backoff_max_ms = configured_backoff_max_ms(conn)?.max(backoff_base_ms);
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "UPDATE analysis_attempts
                    SET phase = 'retry_wait', phase_updated_at_ms = ?1,
                        terminal_code = 'lease_expired'
                  WHERE (request_id, claim_epoch) IN (
                    SELECT request_id, fence FROM analysis_requests
                     WHERE state = 'running' AND COALESCE(lease_expires_ms, 0) <= ?1)",
                params![now_ms],
            )?;
            transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        not_before_ms = ?1 + MIN(MAX(1000,
                          (?2 * (1 << MIN(MAX(attempts - 1, 0), 30)) *
                           (75 + (ABS(length(request_id) * 17
                             + unicode(substr(request_id, 1, 1)) * 31
                             + unicode(substr(request_id, -1, 1)) * 13
                             + attempts * 7) % 51))) / 100), ?3),
                        last_error_code = 'lease_expired',
                        updated_at_ms = ?1
                  WHERE state = 'running' AND COALESCE(lease_expires_ms, 0) <= ?1",
                params![now_ms, backoff_base_ms, backoff_max_ms],
            )?;
            transaction.execute(
                "UPDATE analysis_attempts
                    SET phase = 'failed', phase_updated_at_ms = ?1,
                        terminal_code = 'attempt_limit'
                  WHERE (request_id, claim_epoch) IN (
                    SELECT request_id, fence FROM analysis_requests
                     WHERE attempts >= ?2 AND state = 'queued' AND not_before_ms <= ?1)",
                params![now_ms, max_attempts],
            )?;
            transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = 'attempt_limit', updated_at_ms = ?1
                  WHERE attempts >= ?2 AND state = 'queued' AND not_before_ms <= ?1",
                params![now_ms, max_attempts],
            )?;
            let candidate = transaction
                .query_row(
                    &format!(
                        "SELECT {REQUEST_COLS} FROM analysis_requests
                          WHERE (target_node_id = ?1
                              OR (component = 'skip_markers' AND target_node_id = ''))
                            AND attempts < ?2
                            AND state = 'queued' AND not_before_ms <= ?3
                          ORDER BY created_at_ms - CASE WHEN priority = 'forced'
                                     THEN ?4 ELSE 0 END,
                                   created_at_ms, request_id LIMIT 1"
                    ),
                    params![
                        node_id,
                        max_attempts,
                        now_ms,
                        super::super::fragment_index_cluster::ANALYSIS_FORCED_PRIORITY_BOOST_MS
                    ],
                    request_from_row,
                )
                .optional()?;
            let Some(mut candidate) = candidate else {
                transaction.commit()?;
                return Ok(None);
            };
            let changed = transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'running', owner_node_id = ?1, fence = fence + 1,
                        lease_expires_ms = ?2, attempts = attempts + 1,
                        last_error_code = NULL, updated_at_ms = ?3
                  WHERE request_id = ?4 AND fence = ?5
                    AND state = 'queued' AND not_before_ms <= ?3",
                params![
                    node_id,
                    lease_expires_ms,
                    now_ms,
                    candidate.request_id,
                    candidate.fence
                ],
            )?;
            if changed != 1 {
                transaction.commit()?;
                return Ok(None);
            }
            candidate.state = "running".to_owned();
            candidate.owner_node_id = node_id;
            candidate.fence += 1;
            candidate.lease_expires_ms = lease_expires_ms;
            candidate.attempts += 1;
            candidate.updated_at_ms = now_ms;
            candidate.last_error_code.clear();
            transaction.execute(
                "INSERT INTO analysis_attempts
                    (request_id, attempt, claim_node_id, claim_epoch,
                     claim_expires_at_ms, phase, started_at_ms,
                     phase_updated_at_ms, terminal_code)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'claimed', ?6, ?6, NULL)",
                params![
                    candidate.request_id,
                    candidate.attempts,
                    candidate.owner_node_id,
                    candidate.fence,
                    candidate.lease_expires_ms,
                    now_ms,
                ],
            )?;
            transaction.execute(
                "DELETE FROM analysis_attempts
                  WHERE request_id = ?1 AND claim_epoch NOT IN (
                    SELECT claim_epoch FROM analysis_attempts
                     WHERE request_id = ?1 ORDER BY claim_epoch DESC LIMIT ?2)",
                params![candidate.request_id, MAX_ATTEMPT_HISTORY_PER_REQUEST],
            )?;
            transaction.commit()?;
            Ok(Some(candidate))
        })
        .await
    }

    async fn renew_analysis_request(
        &self,
        request_id: &str,
        node_id: &str,
        fence: i64,
        now_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<bool, StoreError> {
        let request_id = request_id.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE analysis_requests SET lease_expires_ms = ?1, updated_at_ms = ?2
                  WHERE request_id = ?3 AND state = 'running' AND owner_node_id = ?4
                    AND fence = ?5 AND lease_expires_ms > ?2 AND ?1 > ?2",
                params![lease_expires_ms, now_ms, request_id, node_id, fence],
            )?;
            if changed == 1 {
                transaction.execute(
                    "UPDATE analysis_attempts
                        SET claim_expires_at_ms = ?1, phase_updated_at_ms = ?2
                      WHERE request_id = ?3 AND claim_epoch = ?4",
                    params![lease_expires_ms, now_ms, request_id, fence],
                )?;
            }
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
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
        let request = request.clone();
        let phase = phase.to_owned();
        let terminal_code = terminal_code.map(str::to_owned);
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE analysis_attempts
                    SET phase = ?1, phase_updated_at_ms = ?2, terminal_code = ?3
                  WHERE request_id = ?4 AND attempt = ?5 AND claim_epoch = ?6
                    AND claim_node_id = ?7
                    AND EXISTS (SELECT 1 FROM analysis_requests
                          WHERE request_id = ?4 AND state = 'running'
                            AND owner_node_id = ?7 AND fence = ?6
                            AND attempts = ?5 AND lease_expires_ms > ?2)",
                params![
                    phase,
                    now_ms,
                    terminal_code,
                    request.request_id,
                    request.attempts,
                    request.fence,
                    request.owner_node_id,
                ],
            )? == 1)
        })
        .await
    }

    async fn analysis_attempts(
        &self,
        request_id: &str,
        limit: i64,
    ) -> Result<Vec<AnalysisAttempt>, StoreError> {
        let request_id = request_id.to_owned();
        let limit = limit.clamp(1, 100);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT request_id, attempt, claim_node_id, claim_epoch,
                        claim_expires_at_ms, phase, started_at_ms,
                        phase_updated_at_ms, COALESCE(terminal_code, '')
                   FROM analysis_attempts WHERE request_id = ?1
                  ORDER BY claim_epoch DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![request_id, limit], attempt_from_row)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
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
        let request = request.clone();
        let job = job.clone();
        self.with_conn(move |conn| {
            let max_attempts = configured_max_attempts(conn)?;
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'submitted', owner_node_id = NULL, lease_expires_ms = NULL,
                        result_cache_key = ?1,
                        expected_predecessor_generation = COALESCE((
                          SELECT generation_cache_key FROM cluster_fragment_index_heads
                           WHERE logical_cache_key = ?12
                        ), ''),
                        last_error_code = NULL, updated_at_ms = ?2
                  WHERE request_id = ?3 AND state = 'running' AND owner_node_id = ?4
                    AND fence = ?5 AND lease_expires_ms > ?2
                    AND file_id = ?6 AND source_size = ?7 AND source_mtime = ?8
                    AND EXISTS (SELECT 1 FROM files
                          WHERE id = ?6 AND size = ?7 AND mtime = ?8)
                    AND (?9 = 1 OR ?1 = COALESCE((
                      SELECT generation_cache_key FROM cluster_fragment_index_heads
                       WHERE logical_cache_key = ?12), ?12))
                    AND (
                      (?9 = 1
                        AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                              WHERE state IN ('queued', 'running')) < ?10
                        AND (NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                          WHERE cache_key = ?1 AND target_node_id = ?13)
                          OR EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                      WHERE cache_key = ?1 AND target_node_id = ?13
                                        AND state IN ('ready', 'failed', 'cancelled'))))
                      OR
                      (?9 = 0 AND (
                        EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                 WHERE cache_key = ?1 AND target_node_id = ?13
                                   AND state IN ('queued', 'running', 'ready'))
                        OR ((SELECT COUNT(*) FROM cluster_fragment_index_jobs
                                WHERE state IN ('queued', 'running')) < ?10
                          AND (NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                            WHERE cache_key = ?1 AND target_node_id = ?13)
                            OR EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                                        WHERE cache_key = ?1 AND target_node_id = ?13 AND (
                                          state = 'cancelled'
                                          OR (state = 'failed'
                                            AND last_error_code = 'queue_expired'
                                            AND attempts <= ?11))))))))",
                params![
                    job.cache_key,
                    now_ms,
                    request.request_id,
                    request.owner_node_id,
                    request.fence,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    if request.force_rebuild { 1_i64 } else { 0_i64 },
                    MAX_ACTIVE_JOBS,
                    max_attempts,
                    logical_cache_key,
                    job.target_node_id,
                ],
            )?;
            if changed != 1 {
                transaction.commit()?;
                return Ok(false);
            }
            transaction.execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, priority, trigger, target_node_id,
                     state, owner_node_id, fence, lease_expires_ms,
                     attempts, not_before_ms, created_at_ms, updated_at_ms, last_error_code)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                        'queued', NULL, 0, NULL, 0, ?10, ?11, ?11, NULL
                 ON CONFLICT(cache_key, target_node_id) DO UPDATE SET
                    file_id = excluded.file_id, source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    pipeline_sha256 = excluded.pipeline_sha256,
                    priority = excluded.priority, trigger = excluded.trigger,
                    state = 'queued',
                    owner_node_id = NULL, lease_expires_ms = NULL,
                    attempts = CASE WHEN ?12 = 1 THEN 0
                      WHEN cluster_fragment_index_jobs.state = 'cancelled'
                        OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                        OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN 0
                      ELSE cluster_fragment_index_jobs.attempts END,
                    attempt_errors = CASE WHEN ?12 = 1 THEN ''
                      WHEN cluster_fragment_index_jobs.state = 'cancelled'
                        OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                        OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN ''
                      ELSE cluster_fragment_index_jobs.attempt_errors END,
                    not_before_ms = excluded.not_before_ms,
                    created_at_ms = excluded.created_at_ms,
                    updated_at_ms = excluded.updated_at_ms, last_error_code = NULL
                  WHERE (?12 = 1 AND cluster_fragment_index_jobs.state
                                      IN ('ready', 'failed', 'cancelled'))
                     OR (?12 = 0 AND (
                       cluster_fragment_index_jobs.state = 'cancelled'
                       OR (cluster_fragment_index_jobs.state = 'failed'
                         AND cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                         AND cluster_fragment_index_jobs.attempts <= ?13)))",
                params![
                    job.cache_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    job.source_sha256,
                    job.pipeline_sha256,
                    job.priority,
                    job.trigger,
                    job.target_node_id,
                    job.not_before_ms,
                    job.created_at_ms,
                    if request.force_rebuild { 1_i64 } else { 0_i64 },
                    max_attempts,
                ],
            )?;
            let handed_off = transaction.query_row(
                "SELECT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                  WHERE cache_key = ?1 AND target_node_id = ?2
                    AND state IN ('queued', 'running', 'ready'))",
                params![job.cache_key, job.target_node_id],
                |row| row.get::<_, i64>(0),
            )? == 1;
            if !handed_off {
                return Err(StoreError::Task(
                    "analysis handoff transaction did not produce a worker job".to_owned(),
                ));
            }
            // A pre-head-schema logical artifact is still valid content. Its
            // first request lazily installs the serving pointer only after
            // verifying both the ready job and immutable artifact exist.
            transaction.execute(
                "INSERT INTO cluster_fragment_index_heads
                    (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                 SELECT ?1, ?2, ?3, ?4
                  WHERE ?1 = ?2
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                          WHERE cache_key = ?2 AND target_node_id = ?5 AND state = 'ready')
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                          WHERE cache_key = ?2)
                 ON CONFLICT(logical_cache_key) DO NOTHING",
                params![
                    logical_cache_key,
                    job.cache_key,
                    request.request_id,
                    now_ms,
                    job.target_node_id
                ],
            )?;
            transaction.execute(
                "UPDATE analysis_attempts
                    SET phase = 'staged', phase_updated_at_ms = ?1,
                        terminal_code = NULL
                  WHERE request_id = ?2 AND claim_epoch = ?3",
                params![now_ms, request.request_id, request.fence],
            )?;
            transaction.commit()?;
            Ok(true)
        })
        .await
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
        let request = request.clone();
        let error_code = error_code.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = CASE WHEN ?1 = 0 AND attempts > 0
                          THEN attempts - 1 ELSE attempts END,
                        not_before_ms = ?2, last_error_code = ?3, updated_at_ms = ?4
                  WHERE request_id = ?5 AND state = 'running' AND owner_node_id = ?6
                    AND fence = ?7 AND lease_expires_ms > ?4",
                params![
                    if charge_attempt { 1_i64 } else { 0_i64 },
                    retry_at_ms,
                    error_code,
                    now_ms,
                    request.request_id,
                    request.owner_node_id,
                    request.fence
                ],
            )?;
            if changed == 1 {
                transaction.execute(
                    "UPDATE analysis_attempts
                        SET phase = 'retry_wait', phase_updated_at_ms = ?1,
                            terminal_code = ?2
                      WHERE request_id = ?3 AND claim_epoch = ?4",
                    params![now_ms, error_code, request.request_id, request.fence],
                )?;
            }
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
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
        let request_id = request_id.to_owned();
        let node_id = node_id.to_owned();
        let error_code = error_code.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = ?1, updated_at_ms = ?2
                  WHERE request_id = ?3 AND state = 'running' AND owner_node_id = ?4
                    AND fence = ?5 AND lease_expires_ms > ?2",
                params![error_code, now_ms, request_id, node_id, fence],
            )?;
            if changed == 1 {
                transaction.execute(
                    "UPDATE analysis_attempts
                        SET phase = 'failed', phase_updated_at_ms = ?1,
                            terminal_code = ?2
                      WHERE request_id = ?3 AND claim_epoch = ?4",
                    params![now_ms, error_code, request_id, fence],
                )?;
            }
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
    }

    async fn settle_analysis_requests(&self, now_ms: i64) -> Result<u64, StoreError> {
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
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
                        phase_updated_at_ms = ?1
                  WHERE (request_id, claim_epoch) IN (
                    SELECT request.request_id, request.fence
                      FROM analysis_requests request
                      JOIN cluster_fragment_index_jobs job
                        ON job.cache_key = request.result_cache_key
                       AND job.target_node_id = request.target_node_id
                     WHERE request.state = 'submitted'
                       AND job.state IN ('ready', 'failed', 'cancelled'))",
                params![now_ms],
            )?;
            let changed = transaction.execute(
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
                            WHEN 'cancelled' THEN 'cancelled'
                            ELSE analysis_requests.state END
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
                          THEN ?1 + 1000 ELSE not_before_ms END,
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
                        updated_at_ms = ?1
                  WHERE state = 'submitted'
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs job
                         WHERE job.cache_key = analysis_requests.result_cache_key
                           AND job.target_node_id = analysis_requests.target_node_id
                           AND job.state IN ('ready', 'failed', 'cancelled'))",
                params![now_ms],
            )?;
            transaction.commit()?;
            Ok(changed as u64)
        })
        .await
    }

    async fn prune_analysis_requests(
        &self,
        older_than_ms: i64,
        limit: i64,
    ) -> Result<u64, StoreError> {
        let limit = limit.clamp(1, MAX_ANALYSIS_REQUESTS);
        self.with_conn(move |conn| {
            let changed = conn.execute(
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
                    WHERE (updated_at_ms < ?1 OR generation_rank > 20)
                      AND (force_rebuild = 1 OR identity_rank > 1
                        OR priority_generation_rank > 1
                        OR NOT EXISTS (SELECT 1 FROM files
                             WHERE files.id = ranked.file_id
                               AND files.size = ranked.source_size
                               AND files.mtime = ranked.source_mtime))
                    ORDER BY updated_at_ms, request_id LIMIT ?2)",
                params![older_than_ms, limit],
            )?;
            Ok(changed as u64)
        })
        .await
    }

    async fn analysis_requests(&self, limit: i64) -> Result<Vec<AnalysisRequest>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "WITH visible AS (
                   SELECT * FROM (
                     SELECT * FROM analysis_requests
                      WHERE state IN ('queued', 'running', 'submitted')
                      ORDER BY updated_at_ms DESC, request_id LIMIT ?1)
                   UNION ALL
                   SELECT * FROM (
                     SELECT * FROM analysis_requests
                      WHERE state IN ('ready', 'failed', 'cancelled')
                      ORDER BY updated_at_ms DESC, request_id LIMIT ?1)
                 )
                 SELECT {REQUEST_COLS} FROM visible
                  ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1
                    WHEN 'submitted' THEN 2 WHEN 'failed' THEN 3 ELSE 4 END,
                    updated_at_ms DESC, request_id LIMIT ?1"
            ))?;
            let rows = statement.query_map(params![limit], request_from_row)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn reopenable_analysis_requests(
        &self,
        component: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AnalysisRequest>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        let component = component.map(str::to_owned);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {REQUEST_COLS} FROM analysis_requests terminal
                  WHERE terminal.state IN ('failed', 'cancelled')
                    AND terminal.force_rebuild = 0
                    AND (?2 IS NULL OR terminal.component = ?2)
                    AND NOT EXISTS (
                      SELECT 1 FROM analysis_requests successor
                       WHERE successor.file_id = terminal.file_id
                         AND successor.source_size = terminal.source_size
                         AND successor.source_mtime = terminal.source_mtime
                         AND successor.component = terminal.component
                         AND successor.target_node_id = terminal.target_node_id
                         AND successor.state IN ('queued','running','submitted','ready'))
                  ORDER BY terminal.updated_at_ms, terminal.request_id LIMIT ?1"
            ))?;
            let rows = statement.query_map(params![limit, component], request_from_row)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn analysis_request(
        &self,
        request_id: &str,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        let request_id = request_id.to_owned();
        self.with_read(move |conn| {
            conn.query_row(
                &format!("SELECT {REQUEST_COLS} FROM analysis_requests WHERE request_id = ?1"),
                params![request_id],
                request_from_row,
            )
            .optional()
            .map_err(Into::into)
        })
        .await
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
        let request_id = request_id.to_owned();
        let requested_generation = requested_generation.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "INSERT INTO analysis_requests
                    (request_id, file_id, source_size, source_mtime, component,
                     pipeline_version, requested_generation,
                     expected_predecessor_generation, priority, trigger,
                     force_rebuild, target_node_id, state, owner_node_id, fence,
                     lease_expires_ms, attempts, not_before_ms, result_cache_key,
                     last_error_code, cancel_requested, created_at_ms, updated_at_ms)
                 SELECT ?1, original.file_id, original.source_size, original.source_mtime,
                        original.component, original.pipeline_version, ?1,
                        CASE WHEN original.component = 'skip_markers' THEN COALESCE((
                          SELECT generation_id FROM timeline_annotation_sets
                           WHERE file_id = original.file_id
                             AND source_size = original.source_size
                             AND source_mtime = original.source_mtime
                        ), '') ELSE original.expected_predecessor_generation END,
                        'forced', 'admin', 1,
                        original.target_node_id, 'queued', NULL, 0, NULL, 0,
                        ?2, NULL, NULL, 0, ?2, ?2
                   FROM analysis_requests original
                  WHERE original.request_id = ?3
                    AND original.state IN ('failed', 'cancelled')
                    AND EXISTS (SELECT 1 FROM files
                          WHERE id = original.file_id
                            AND size = original.source_size
                            AND mtime = original.source_mtime)
                 ON CONFLICT DO NOTHING",
                params![requested_generation, now_ms, request_id],
            )?;
            let request = transaction
                .query_row(
                    &format!("SELECT {REQUEST_COLS} FROM analysis_requests WHERE request_id = ?1"),
                    params![requested_generation],
                    request_from_row,
                )
                .optional()?;
            transaction.commit()?;
            Ok(request)
        })
        .await
    }

    async fn cancel_analysis_request_admin(
        &self,
        request_id: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        let request_id = request_id.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "UPDATE analysis_attempts
                    SET phase = 'canceled', phase_updated_at_ms = ?1,
                        terminal_code = 'admin_cancelled'
                  WHERE (request_id, claim_epoch) = (
                    SELECT request_id, fence FROM analysis_requests
                     WHERE request_id = ?2 AND state IN ('running', 'submitted'))",
                params![now_ms, request_id],
            )?;
            transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                        fence = fence + 1, cancel_requested = 1,
                        last_error_code = 'admin_cancelled',
                        updated_at_ms = ?1
                  WHERE request_id = ?2 AND state IN ('queued', 'running', 'submitted')",
                params![now_ms, request_id],
            )?;
            transaction.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                        fence = fence + 1, last_error_code = 'admin_cancelled',
                        updated_at_ms = ?1
                  WHERE state IN ('queued', 'running')
                    AND cache_key = (SELECT result_cache_key FROM analysis_requests
                                      WHERE request_id = ?2)
                    AND target_node_id = (SELECT target_node_id FROM analysis_requests
                                           WHERE request_id = ?2)
                    AND NOT EXISTS (
                      SELECT 1 FROM analysis_requests other
                       WHERE other.request_id <> ?2
                         AND other.result_cache_key = cluster_fragment_index_jobs.cache_key
                         AND other.target_node_id = cluster_fragment_index_jobs.target_node_id
                         AND other.state IN ('queued', 'running', 'submitted'))",
                params![now_ms, request_id],
            )?;
            let request = transaction
                .query_row(
                    &format!("SELECT {REQUEST_COLS} FROM analysis_requests WHERE request_id = ?1"),
                    params![request_id],
                    request_from_row,
                )
                .optional()?;
            transaction.commit()?;
            Ok(request)
        })
        .await
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
        let request = request.clone();
        let result_key = result_key.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                        result_cache_key = ?1, last_error_code = NULL, updated_at_ms = ?2
                  WHERE request_id = ?3 AND state = 'running' AND owner_node_id = ?4
                    AND fence = ?5 AND lease_expires_ms > ?2
                    AND file_id = ?6 AND source_size = ?7 AND source_mtime = ?8
                    AND EXISTS (SELECT 1 FROM files
                          WHERE id = ?6 AND size = ?7 AND mtime = ?8)",
                params![
                    result_key,
                    now_ms,
                    request.request_id,
                    request.owner_node_id,
                    request.fence,
                    request.file_id,
                    request.source_size,
                    request.source_mtime,
                ],
            )?;
            if changed == 1 {
                transaction.execute(
                    "UPDATE analysis_attempts
                        SET phase = 'published', phase_updated_at_ms = ?1,
                            terminal_code = NULL
                      WHERE request_id = ?2 AND claim_epoch = ?3",
                    params![now_ms, request.request_id, request.fence],
                )?;
            }
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
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
        let request = request.clone();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                        result_cache_key = ?1, last_error_code = NULL, updated_at_ms = ?2
                  WHERE request_id = ?3 AND component = 'skip_markers'
                    AND state = 'running' AND owner_node_id = ?4
                    AND fence = ?5 AND lease_expires_ms > ?2
                    AND file_id = ?6 AND source_size = ?7 AND source_mtime = ?8
                    AND requested_generation = ?1
                    AND expected_predecessor_generation = COALESCE((
                      SELECT generation_id FROM timeline_annotation_sets
                       WHERE file_id = ?6 AND source_size = ?7 AND source_mtime = ?8
                    ), '')
                    AND (force_rebuild = 1 OR COALESCE((
                      SELECT publication_priority FROM timeline_annotation_sets
                       WHERE file_id = ?6 AND source_size = ?7 AND source_mtime = ?8
                    ), 'normal') <> 'forced')
                    AND NOT EXISTS (
                      SELECT 1 FROM analysis_requests newer
                       WHERE newer.file_id = ?6 AND newer.source_size = ?7
                         AND newer.source_mtime = ?8 AND newer.component = 'skip_markers'
                         AND (newer.created_at_ms > analysis_requests.created_at_ms
                           OR (newer.created_at_ms = analysis_requests.created_at_ms
                             AND newer.request_id > analysis_requests.request_id))
                         AND newer.state IN ('queued','running','submitted','ready'))
                    AND EXISTS (SELECT 1 FROM files
                          WHERE id = ?6 AND size = ?7 AND mtime = ?8)",
                params![
                    set.generation_id,
                    now_ms,
                    request.request_id,
                    request.owner_node_id,
                    request.fence,
                    request.file_id,
                    request.source_size,
                    request.source_mtime,
                ],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            transaction.execute(
                "INSERT INTO timeline_annotation_sets
                    (file_id, source_size, source_mtime, argv_fingerprint,
                     generation_id, version, annotations_json, updated_at_ms,
                     publication_priority)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(file_id) DO UPDATE SET
                    source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    argv_fingerprint = excluded.argv_fingerprint,
                    generation_id = excluded.generation_id,
                    version = excluded.version,
                    annotations_json = excluded.annotations_json,
                    updated_at_ms = excluded.updated_at_ms,
                    publication_priority = excluded.publication_priority",
                params![
                    request.file_id,
                    request.source_size,
                    set.source_identity.mtime_ms,
                    set.source_identity.argv_fingerprint,
                    set.generation_id,
                    i64::from(set.version),
                    annotations_json,
                    now_ms,
                    if request.force_rebuild {
                        "forced"
                    } else {
                        "normal"
                    },
                ],
            )?;
            transaction.execute(
                "UPDATE analysis_attempts
                    SET phase = 'published', phase_updated_at_ms = ?1,
                        terminal_code = NULL
                  WHERE request_id = ?2 AND claim_epoch = ?3",
                params![now_ms, request.request_id, request.fence],
            )?;
            transaction.commit()?;
            Ok(true)
        })
        .await
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
        self.with_read(move |conn| {
            let sql = format!(
                "{ANALYSIS_CANONICAL_CTE}, matching AS (
                   SELECT * FROM classified
                    WHERE (?1 = 0
                      OR (?1 = 1 AND disposition IN ('working', 'automatic'))
                      OR (?1 = 2 AND disposition = 'attention')
                      OR (?1 = 3 AND disposition = 'ready')
                      OR (?1 = 4 AND disposition IN ('expected', 'unsupported')))
                      AND (?2 = '' OR LOWER(title) LIKE ?2 ESCAPE '\\'
                        OR CAST(file_id AS TEXT) LIKE ?2 ESCAPE '\\'
                        OR LOWER(row_key) LIKE ?2 ESCAPE '\\'
                        OR LOWER(owner_node_id) LIKE ?2 ESCAPE '\\'
                        OR LOWER(target_node_id) LIKE ?2 ESCAPE '\\'
                        OR LOWER(request_error_code) LIKE ?2 ESCAPE '\\'
                        OR LOWER(job_error_code) LIKE ?2 ESCAPE '\\'
                        OR LOWER(state) LIKE ?2 ESCAPE '\\')
                      AND (?3 = '' OR INSTR(?3, ',' || CASE
                        WHEN COALESCE(NULLIF(job_error_code, ''), request_error_code) = 'source_superseded'
                          THEN 'stale'
                        WHEN state = 'queued' AND not_before_ms > ?4 THEN 'retry_wait'
                        WHEN state = 'running' THEN 'claimed'
                        WHEN state = 'submitted' THEN 'staged'
                        WHEN state = 'ready' THEN 'published'
                        WHEN state = 'cancelled' THEN 'canceled'
                        ELSE state END || ',') > 0)
                 ), page AS (
                   SELECT {HISTORY_COLS}, sort_rank FROM matching
                    WHERE ?5 < 0 OR sort_rank > ?5
                       OR (sort_rank = ?5 AND updated_at_ms < ?6)
                       OR (sort_rank = ?5 AND updated_at_ms = ?6 AND row_key > ?7)
                    ORDER BY sort_rank, updated_at_ms DESC, row_key
                    LIMIT ?8
                 ), totals AS (
                   SELECT COUNT(*) AS filtered_total FROM matching
                 )
                 SELECT {HISTORY_PAGE_COLS}, COALESCE(page.sort_rank, -1) AS sort_rank,
                        totals.filtered_total
                   FROM totals LEFT JOIN page ON 1 = 1
                  ORDER BY page.sort_rank, page.updated_at_ms DESC, page.row_key"
            );
            let mut statement = conn.prepare(&sql)?;
            let rows = statement.query_map(
                params![
                    filter,
                    search,
                    states,
                    now_ms,
                    cursor.sort_rank,
                    cursor.updated_at_ms,
                    cursor.row_key,
                    limit + 1
                ],
                history_from_row,
            )?;
            let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
            let filtered_total = rows.first().map_or(0, |(_, _, total)| *total);
            rows.retain(|(row, _, _)| !row.row_key.is_empty());
            let has_more = rows.len() > limit as usize;
            if has_more {
                rows.pop();
            }
            let next_cursor =
                has_more.then(|| rows.last().expect("non-empty history page").1.clone());
            Ok(AnalysisHistoryPage {
                rows: rows.into_iter().map(|(row, _, _)| row).collect(),
                filtered_total,
                next_cursor,
            })
        })
        .await
    }

    async fn analysis_status_summary(&self) -> Result<AnalysisStatusSummary, StoreError> {
        self.with_read(|conn| {
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
            conn.query_row(&sql, [], |row| {
                Ok(AnalysisStatusSummary {
                    total: row.get(0)?,
                    working: row.get(1)?,
                    queued: row.get(2)?,
                    running: row.get(3)?,
                    submitted: row.get(4)?,
                    attention: row.get(5)?,
                    expected: row.get(6)?,
                    ready: row.get(7)?,
                    latest_error_code: row.get(8)?,
                    latest_error_file_id: row.get(9)?,
                    latest_error_updated_at_ms: row.get(10)?,
                })
            })
            .map_err(Into::into)
        })
        .await
    }

    async fn analysis_file_labels(&self, limit: i64) -> Result<Vec<AnalysisFileLabel>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT files.id, files.item_id, items.title
                   FROM files JOIN items ON items.id = files.item_id
                  WHERE files.id IN (
                    SELECT file_id FROM (
                      SELECT file_id FROM analysis_requests
                       ORDER BY updated_at_ms DESC, request_id LIMIT ?1)
                    UNION SELECT file_id FROM (
                      SELECT file_id FROM cluster_fragment_index_jobs
                       ORDER BY updated_at_ms DESC, cache_key LIMIT ?1))
                  ORDER BY files.id LIMIT ?1",
            )?;
            let rows = statement.query_map(params![limit], |row| {
                Ok(AnalysisFileLabel {
                    file_id: row.get(0)?,
                    item_id: row.get(1)?,
                    title: row.get(2)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn cluster_fragment_index_jobs(
        &self,
        limit: i64,
    ) -> Result<Vec<ClusterFragmentIndexJob>, StoreError> {
        let limit = limit.clamp(1, MAX_LIST_ROWS);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs
                  ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1
                    WHEN 'failed' THEN 2 ELSE 3 END, updated_at_ms DESC, cache_key
                  LIMIT ?1"
            ))?;
            let rows = statement.query_map(params![limit], job_from_row)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn cluster_fragment_index_job(
        &self,
        cache_key: &str,
        target_node_id: &str,
    ) -> Result<Option<ClusterFragmentIndexJob>, StoreError> {
        let cache_key = cache_key.to_owned();
        let target_node_id = target_node_id.to_owned();
        self.with_read(move |conn| {
            conn.query_row(
                &format!("SELECT {JOB_COLS} FROM cluster_fragment_index_jobs WHERE cache_key = ?1 AND target_node_id = ?2"),
                params![cache_key, target_node_id],
                job_from_row,
            )
            .optional()
            .map_err(Into::into)
        })
        .await
    }

    async fn cluster_fragment_index_current_generation(
        &self,
        logical_cache_key: &str,
    ) -> Result<Option<String>, StoreError> {
        let logical_cache_key = logical_cache_key.to_owned();
        self.with_read(move |conn| {
            conn.query_row(
                "SELECT generation_cache_key FROM cluster_fragment_index_heads
                  WHERE logical_cache_key = ?1",
                params![logical_cache_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
        .await
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
        let observation = observation.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_sources
                    (node_id, file_id, object_version, source_size, source_mtime,
                     source_sha256, observed_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(node_id, file_id) DO UPDATE SET
                    object_version = excluded.object_version,
                    source_size = excluded.source_size,
                    source_mtime = excluded.source_mtime,
                    source_sha256 = excluded.source_sha256,
                    observed_at_ms = excluded.observed_at_ms",
                params![
                    observation.node_id,
                    observation.file_id,
                    observation.object_version,
                    observation.source_size,
                    observation.source_mtime,
                    observation.source_sha256,
                    observation.observed_at_ms,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn fragment_index_source(
        &self,
        node_id: &str,
        file_id: i64,
        object_version: &str,
    ) -> Result<Option<FragmentIndexSourceObservation>, StoreError> {
        let node_id = node_id.to_owned();
        let object_version = object_version.to_owned();
        self.with_read(move |conn| {
            conn.query_row(
                "SELECT node_id, file_id, object_version, source_size, source_mtime,
                        source_sha256, observed_at_ms
                   FROM cluster_fragment_index_sources
                  WHERE node_id = ?1 AND file_id = ?2 AND object_version = ?3",
                params![node_id, file_id, object_version],
                |row| {
                    Ok(FragmentIndexSourceObservation {
                        node_id: row.get(0)?,
                        file_id: row.get(1)?,
                        object_version: row.get(2)?,
                        source_size: row.get(3)?,
                        source_mtime: row.get(4)?,
                        source_sha256: row.get(5)?,
                        observed_at_ms: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
        })
        .await
    }

    async fn cluster_fragment_index_artifact(
        &self,
        cache_key: &str,
    ) -> Result<Option<ClusterFragmentIndexArtifact>, StoreError> {
        let cache_key = cache_key.to_owned();
        self.with_read(move |conn| {
            conn.query_row(
                "SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
                        pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms
                   FROM cluster_fragment_index_artifacts WHERE cache_key = COALESCE((
                     SELECT generation_cache_key FROM cluster_fragment_index_heads
                      WHERE logical_cache_key = ?1
                   ), ?1)",
                params![cache_key],
                artifact_from_row,
            )
            .optional()
            .map_err(Into::into)
        })
        .await
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
        let job = job.clone();
        let eligibility_cutoff = job.created_at_ms.saturating_sub(QUEUE_ELIGIBILITY_MS);
        self.with_conn(move |conn| {
            let max_attempts = configured_max_attempts(conn)?;
            conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        not_before_ms = ?1, updated_at_ms = ?1,
                        last_error_code = CASE WHEN state = 'running'
                          THEN 'attempt_limit' ELSE 'queue_expired' END,
                            attempt_errors = CASE WHEN state = 'running'
                              THEN (CASE WHEN attempt_errors = '' THEN 'lease_expired'
                                    ELSE attempt_errors || ',lease_expired' END)
                              ELSE attempt_errors END
                  WHERE (state = 'queued' AND created_at_ms < ?2)
                     OR (state = 'running' AND attempts >= ?3
                       AND COALESCE(lease_expires_ms, 0) <= ?1)",
                params![job.created_at_ms, eligibility_cutoff, max_attempts],
            )?;
            let changed = conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, priority, trigger, target_node_id,
                     state, owner_node_id, fence, lease_expires_ms,
                     attempts, not_before_ms, created_at_ms, updated_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                        'queued', NULL, 0, NULL, 0, ?10, ?11, ?11
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = ?2 AND size = ?3 AND mtime = ?4)
                    AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                     WHERE cache_key = ?1)
                    AND (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < ?12
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
                    attempt_errors = CASE
                        WHEN cluster_fragment_index_jobs.state = 'cancelled'
                          OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                          OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN ''
                        ELSE cluster_fragment_index_jobs.attempt_errors END,
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
                       AND cluster_fragment_index_jobs.attempts <= ?13)
                     OR (cluster_fragment_index_jobs.state = 'queued'
                       AND cluster_fragment_index_jobs.priority = 'normal'
                       AND excluded.priority = 'foreground')",
                params![
                    job.cache_key,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    job.source_sha256,
                    job.pipeline_sha256,
                    job.priority,
                    job.trigger,
                    job.target_node_id,
                    job.not_before_ms,
                    job.created_at_ms,
                    MAX_ACTIVE_JOBS,
                    max_attempts,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
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
        let node_id = node_id.to_owned();
        let excluded_cache_keys = excluded_cache_keys
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        self.with_conn(move |conn| {
            let max_attempts = configured_max_attempts(conn)?;
            let backoff_base_ms = configured_backoff_base_ms(conn)?;
            let backoff_max_ms = configured_backoff_max_ms(conn)?.max(backoff_base_ms);
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        not_before_ms = ?1, updated_at_ms = ?1,
                        last_error_code = CASE WHEN state = 'running'
                          THEN 'attempt_limit' ELSE 'queue_expired' END,
                            attempt_errors = CASE WHEN state = 'running'
                              THEN (CASE WHEN attempt_errors = '' THEN 'lease_expired'
                                    ELSE attempt_errors || ',lease_expired' END)
                              ELSE attempt_errors END
                  WHERE (state = 'queued' AND created_at_ms < ?2)
                     OR (state = 'running' AND attempts >= ?3
                       AND COALESCE(lease_expires_ms, 0) <= ?1)",
                params![
                    now_ms,
                    now_ms.saturating_sub(QUEUE_ELIGIBILITY_MS),
                    max_attempts
                ],
            )?;
            transaction.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        not_before_ms = ?1 + MIN(MAX(1000,
                          (?2 * (1 << MIN(MAX(attempts - 1, 0), 30)) *
                           (75 + (ABS(length(cache_key || ':' || target_node_id) * 17
                             + unicode(substr(cache_key || ':' || target_node_id, 1, 1)) * 31
                             + unicode(substr(cache_key || ':' || target_node_id, -1, 1)) * 13
                             + attempts * 7) % 51))) / 100), ?3),
                        last_error_code = 'lease_expired',
                            attempt_errors = CASE WHEN attempt_errors = ''
                              THEN 'lease_expired'
                              ELSE attempt_errors || ',lease_expired' END,
                            updated_at_ms = ?1
                  WHERE state = 'running' AND COALESCE(lease_expires_ms, 0) <= ?1
                    AND attempts < ?4",
                params![now_ms, backoff_base_ms, backoff_max_ms, max_attempts],
            )?;
            let candidates = {
                let mut statement = transaction.prepare(&format!(
                    "SELECT {JOB_COLS} FROM cluster_fragment_index_jobs job
                          WHERE state = 'queued' AND not_before_ms <= ?1
                            AND attempts < ?2 AND fence < 9223372036854775807
                            AND (target_node_id = '' OR target_node_id = ?5)
                          ORDER BY job.created_at_ms - CASE
                              WHEN job.priority IN ('forced','foreground') THEN ?4 ELSE 0 END,
                            job.created_at_ms, job.cache_key, job.target_node_id LIMIT ?3"
                ))?;
                let rows = statement.query_map(
                    params![
                        now_ms,
                        max_attempts,
                        CLAIM_SCAN_LIMIT,
                        super::super::fragment_index_cluster::ANALYSIS_FORCED_PRIORITY_BOOST_MS,
                        node_id
                    ],
                    job_from_row,
                )?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let candidate = candidates
                .into_iter()
                .find(|candidate| !excluded_cache_keys.contains(&candidate.cache_key));
            let Some(mut candidate) = candidate else {
                transaction.commit()?;
                return Ok(None);
            };
            let changed = transaction.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'running', owner_node_id = ?1, fence = fence + 1,
                        lease_expires_ms = ?2, attempts = attempts + 1,
                        last_error_code = NULL, updated_at_ms = ?3
                  WHERE cache_key = ?4 AND fence = ?5 AND target_node_id = ?6
                    AND state = 'queued' AND not_before_ms <= ?3
                    AND (target_node_id = '' OR target_node_id = ?1)",
                params![
                    node_id,
                    lease_expires_ms,
                    now_ms,
                    candidate.cache_key,
                    candidate.fence,
                    candidate.target_node_id
                ],
            )?;
            if changed != 1 {
                transaction.commit()?;
                return Ok(None);
            }
            candidate.state = "running".to_owned();
            candidate.owner_node_id = node_id;
            candidate.fence += 1;
            candidate.lease_expires_ms = lease_expires_ms;
            candidate.attempts += 1;
            candidate.updated_at_ms = now_ms;
            transaction.commit()?;
            Ok(Some(candidate))
        })
        .await
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
        let replacement = replacement.clone();
        let eligibility_cutoff = replacement
            .created_at_ms
            .saturating_sub(QUEUE_ELIGIBILITY_MS);
        self.with_conn(move |conn| {
            let max_attempts = configured_max_attempts(conn)?;
            conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                        not_before_ms = ?1, updated_at_ms = ?1,
                        last_error_code = CASE WHEN state = 'running'
                          THEN 'attempt_limit' ELSE 'queue_expired' END,
                            attempt_errors = CASE WHEN state = 'running'
                              THEN (CASE WHEN attempt_errors = '' THEN 'lease_expired'
                                    ELSE attempt_errors || ',lease_expired' END)
                              ELSE attempt_errors END
                  WHERE (state = 'queued' AND created_at_ms < ?2)
                     OR (state = 'running' AND attempts >= ?3
                       AND COALESCE(lease_expires_ms, 0) <= ?1)",
                params![replacement.created_at_ms, eligibility_cutoff, max_attempts],
            )?;
            Ok(conn.execute(
                "INSERT INTO cluster_fragment_index_jobs
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, priority, trigger, target_node_id, state,
                     owner_node_id, fence, lease_expires_ms, attempts, not_before_ms,
                     created_at_ms, updated_at_ms, last_error_code)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'queued',
                        NULL, 0, NULL, 0, ?10, ?11, ?11, 'holders_unavailable'
                  WHERE (SELECT COUNT(*) FROM cluster_fragment_index_jobs
                          WHERE state IN ('queued', 'running')) < ?13
                    AND EXISTS (SELECT 1 FROM files
                      WHERE id = ?2 AND size = ?3 AND mtime = ?4)
                    AND EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                      WHERE cache_key = ?1 AND source_size = ?3
                        AND source_sha256 = ?5 AND pipeline_sha256 = ?6)
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
                    attempt_errors = CASE
                      WHEN cluster_fragment_index_jobs.state = 'ready'
                        OR cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                        OR cluster_fragment_index_jobs.file_id <> excluded.file_id THEN ''
                      ELSE cluster_fragment_index_jobs.attempt_errors END,
                    not_before_ms = excluded.not_before_ms,
                    created_at_ms = excluded.created_at_ms,
                    updated_at_ms = excluded.updated_at_ms,
                    last_error_code = 'holders_unavailable'
                  WHERE cluster_fragment_index_jobs.state = 'ready'
                     OR (cluster_fragment_index_jobs.state = 'failed'
                       AND cluster_fragment_index_jobs.last_error_code = 'queue_expired'
                       AND cluster_fragment_index_jobs.attempts <= ?12)",
                params![
                    replacement.cache_key,
                    replacement.file_id,
                    replacement.source_size,
                    replacement.source_mtime,
                    replacement.source_sha256,
                    replacement.pipeline_sha256,
                    replacement.priority,
                    replacement.trigger,
                    replacement.target_node_id,
                    replacement.not_before_ms,
                    replacement.created_at_ms,
                    max_attempts,
                    MAX_ACTIVE_JOBS,
                ],
            )? == 1)
        })
        .await
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
        let cache_key = cache_key.to_owned();
        let target_node_id = target_node_id.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET lease_expires_ms = ?1, updated_at_ms = ?2
                  WHERE cache_key = ?3 AND owner_node_id = ?4 AND fence = ?5
                    AND target_node_id = ?6
                    AND state = 'running' AND lease_expires_ms > ?2 AND ?1 > ?2",
                params![
                    lease_expires_ms,
                    now_ms,
                    cache_key,
                    node_id,
                    fence,
                    target_node_id
                ],
            )? == 1)
        })
        .await
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
        let cache_key = cache_key.to_owned();
        let target_node_id = target_node_id.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
                        attempts = MAX(attempts - 1, 0), not_before_ms = ?1,
                        last_error_code = 'node_local_refusal', updated_at_ms = ?2
                  WHERE cache_key = ?3 AND owner_node_id = ?4 AND fence = ?5
                    AND target_node_id = ?6
                    AND state = 'running' AND lease_expires_ms > ?2",
                params![
                    retry_at_ms,
                    now_ms,
                    cache_key,
                    node_id,
                    fence,
                    target_node_id
                ],
            )? == 1)
        })
        .await
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
        let job = job.clone();
        let artifact = artifact.clone();
        let location = location.clone();
        let logical_cache_key = cluster_fragment_index_key(
            artifact.file_id,
            artifact.source_size,
            artifact.source_mtime,
            &artifact.source_sha256,
            &artifact.pipeline_sha256,
        )
        .ok_or_else(|| StoreError::Task("invalid fragment-index logical key".to_owned()))?;
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let current: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_fragment_index_jobs
                  WHERE cache_key = ?1 AND target_node_id = ?11
                    AND state = 'running' AND owner_node_id = ?2
                    AND fence = ?3 AND lease_expires_ms > ?4
                    AND file_id = ?5 AND source_size = ?6 AND source_mtime = ?7
                    AND source_sha256 = ?8 AND pipeline_sha256 = ?9
                    AND EXISTS (SELECT 1 FROM files current_file
                      WHERE current_file.id = ?5 AND current_file.size = ?6
                        AND current_file.mtime = ?7)
                    AND (?10 = ?1 OR EXISTS (
                      SELECT 1 FROM analysis_requests
                       WHERE component = 'fragment_index' AND state = 'submitted'
                         AND result_cache_key = ?1 AND target_node_id = ?11) OR EXISTS (
                      SELECT 1 FROM cluster_fragment_index_heads
                       WHERE logical_cache_key = ?10 AND generation_cache_key = ?1)))",
                params![
                    job.cache_key,
                    job.owner_node_id,
                    job.fence,
                    now_ms,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    job.source_sha256,
                    job.pipeline_sha256,
                    logical_cache_key,
                    job.target_node_id,
                ],
                |row| row.get(0),
            )?;
            if !current {
                transaction.commit()?;
                return Ok(false);
            }
            transaction.execute(
                "INSERT INTO cluster_fragment_index_artifacts
                    (cache_key, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(cache_key) DO NOTHING",
                params![
                    artifact.cache_key,
                    artifact.file_id,
                    artifact.source_size,
                    artifact.source_mtime,
                    artifact.source_sha256,
                    artifact.pipeline_sha256,
                    artifact.blob_sha256,
                    artifact.bytes,
                    artifact.built_by_node_id,
                    artifact.built_at_ms,
                ],
            )?;
            let matching: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_fragment_index_artifacts
                  WHERE cache_key = ?1 AND source_sha256 = ?2 AND pipeline_sha256 = ?3
                    AND blob_sha256 = ?4 AND bytes = ?5)",
                params![
                    artifact.cache_key,
                    artifact.source_sha256,
                    artifact.pipeline_sha256,
                    artifact.blob_sha256,
                    artifact.bytes,
                ],
                |row| row.get(0),
            )?;
            if !matching {
                return Err(StoreError::Database(
                    "fragment-index key resolved to conflicting artifacts".to_owned(),
                ));
            }
            transaction.execute(
                "INSERT INTO cluster_fragment_index_locations
                    (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(cache_key, node_id) DO UPDATE SET
                    bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                    last_seen_at_ms = excluded.last_seen_at_ms",
                params![
                    location.cache_key,
                    location.node_id,
                    location.bytes,
                    location.verified_at_ms,
                    location.last_seen_at_ms,
                ],
            )?;
            transaction.execute(
                "INSERT INTO cluster_fragment_index_heads
                    (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                 SELECT ?1, ?2, request_id, ?3 FROM analysis_requests
                  WHERE component = 'fragment_index' AND state = 'submitted'
                    AND result_cache_key = ?2
                    AND target_node_id = ?4
                    AND (force_rebuild = 1 OR expected_predecessor_generation = '')
                    AND expected_predecessor_generation = COALESCE((
                      SELECT generation_cache_key FROM cluster_fragment_index_heads
                       WHERE logical_cache_key = ?1
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
                     WHERE request_id = excluded.request_id)",
                params![logical_cache_key, job.cache_key, now_ms, job.target_node_id],
            )?;
            transaction.execute(
                "INSERT INTO cluster_fragment_index_heads
                    (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                 SELECT ?1, ?2, '', ?3 WHERE ?1 = ?2
                 ON CONFLICT(logical_cache_key) DO NOTHING",
                params![logical_cache_key, job.cache_key, now_ms],
            )?;
            let changed = transaction.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = NULL, updated_at_ms = ?1
                  WHERE cache_key = ?2 AND target_node_id = ?5
                    AND state = 'running' AND owner_node_id = ?3
                    AND fence = ?4 AND lease_expires_ms > ?1",
                params![
                    now_ms,
                    job.cache_key,
                    job.owner_node_id,
                    job.fence,
                    job.target_node_id
                ],
            )?;
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
    }

    async fn complete_cluster_fragment_index_by_hydration(
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
            // Deliberately not `artifact.built_by_node_id == job.owner_node_id`:
            // hydration settles from somebody else's build. The location row
            // is still this node's own claim.
            || location.node_id != job.owner_node_id
            || artifact.bytes <= 0
            || location.bytes != artifact.bytes
            || !valid_hex_digest(&artifact.blob_sha256)
        {
            return Err(StoreError::Task(
                "invalid cluster fragment-index hydration".to_owned(),
            ));
        }
        let job = job.clone();
        let artifact = artifact.clone();
        let location = location.clone();
        let logical_cache_key = cluster_fragment_index_key(
            artifact.file_id,
            artifact.source_size,
            artifact.source_mtime,
            &artifact.source_sha256,
            &artifact.pipeline_sha256,
        )
        .ok_or_else(|| StoreError::Task("invalid fragment-index logical key".to_owned()))?;
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let current: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_fragment_index_jobs
                  WHERE cache_key = ?1 AND target_node_id = ?11
                    AND state = 'running' AND owner_node_id = ?2
                    AND fence = ?3 AND lease_expires_ms > ?4
                    AND file_id = ?5 AND source_size = ?6 AND source_mtime = ?7
                    AND source_sha256 = ?8 AND pipeline_sha256 = ?9
                    AND EXISTS (SELECT 1 FROM files current_file
                      WHERE current_file.id = ?5 AND current_file.size = ?6
                        AND current_file.mtime = ?7)
                    AND (?10 = ?1 OR EXISTS (
                      SELECT 1 FROM analysis_requests
                       WHERE component = 'fragment_index' AND state = 'submitted'
                         AND result_cache_key = ?1 AND target_node_id = ?11) OR EXISTS (
                      SELECT 1 FROM cluster_fragment_index_heads
                       WHERE logical_cache_key = ?10 AND generation_cache_key = ?1)))",
                params![
                    job.cache_key,
                    job.owner_node_id,
                    job.fence,
                    now_ms,
                    job.file_id,
                    job.source_size,
                    job.source_mtime,
                    job.source_sha256,
                    job.pipeline_sha256,
                    logical_cache_key,
                    job.target_node_id,
                ],
                |row| row.get(0),
            )?;
            if !current {
                transaction.commit()?;
                return Ok(false);
            }
            // The artifact is somebody else's and is never written here. It
            // has to already be there, byte for byte, or this node has
            // hydrated something that is not what the job asked for.
            let matching: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_fragment_index_artifacts
                  WHERE cache_key = ?1 AND source_sha256 = ?2 AND pipeline_sha256 = ?3
                    AND blob_sha256 = ?4 AND bytes = ?5)",
                params![
                    artifact.cache_key,
                    artifact.source_sha256,
                    artifact.pipeline_sha256,
                    artifact.blob_sha256,
                    artifact.bytes,
                ],
                |row| row.get(0),
            )?;
            if !matching {
                return Err(StoreError::Database(
                    "hydrated fragment index does not match the published artifact".to_owned(),
                ));
            }
            transaction.execute(
                "INSERT INTO cluster_fragment_index_locations
                    (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(cache_key, node_id) DO UPDATE SET
                    bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                    last_seen_at_ms = excluded.last_seen_at_ms",
                params![
                    location.cache_key,
                    location.node_id,
                    location.bytes,
                    location.verified_at_ms,
                    location.last_seen_at_ms,
                ],
            )?;
            transaction.execute(
                "INSERT INTO cluster_fragment_index_heads
                    (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                 SELECT ?1, ?2, request_id, ?3 FROM analysis_requests
                  WHERE component = 'fragment_index' AND state = 'submitted'
                    AND result_cache_key = ?2
                    AND target_node_id = ?4
                    AND (force_rebuild = 1 OR expected_predecessor_generation = '')
                    AND expected_predecessor_generation = COALESCE((
                      SELECT generation_cache_key FROM cluster_fragment_index_heads
                       WHERE logical_cache_key = ?1
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
                     WHERE request_id = excluded.request_id)",
                params![logical_cache_key, job.cache_key, now_ms, job.target_node_id],
            )?;
            transaction.execute(
                "INSERT INTO cluster_fragment_index_heads
                    (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                 SELECT ?1, ?2, '', ?3 WHERE ?1 = ?2
                 ON CONFLICT(logical_cache_key) DO NOTHING",
                params![logical_cache_key, job.cache_key, now_ms],
            )?;
            let changed = transaction.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = 'ready', owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = NULL, updated_at_ms = ?1
                  WHERE cache_key = ?2 AND target_node_id = ?5
                    AND state = 'running' AND owner_node_id = ?3
                    AND fence = ?4 AND lease_expires_ms > ?1",
                params![
                    now_ms,
                    job.cache_key,
                    job.owner_node_id,
                    job.fence,
                    job.target_node_id
                ],
            )?;
            transaction.commit()?;
            Ok(changed == 1)
        })
        .await
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
        // The comma is the attempt history's delimiter, so a code carrying one
        // would read back as two attempts with codes nobody wrote.
        if error_code.is_empty()
            || error_code.len() > MAX_ERROR_CODE_BYTES
            || error_code.contains(',')
            || (retryable && retry_at_ms <= now_ms)
        {
            return Err(StoreError::Task(
                "invalid fragment-index failure code".to_owned(),
            ));
        }
        let cache_key = cache_key.to_owned();
        let target_node_id = target_node_id.to_owned();
        let node_id = node_id.to_owned();
        let error_code = error_code.to_owned();
        self.with_conn(move |conn| {
            let max_attempts = configured_max_attempts(conn)?;
            Ok(conn.execute(
                "UPDATE cluster_fragment_index_jobs
                    SET state = CASE WHEN ?7 = 1 AND attempts < ?8
                              THEN 'queued' ELSE 'failed' END,
                        owner_node_id = NULL, lease_expires_ms = NULL,
                        last_error_code = CASE WHEN ?7 = 1 AND attempts >= ?8
                              THEN 'attempt_limit' ELSE ?1 END,
                        attempt_errors = CASE WHEN attempt_errors = ''
                              THEN ?1 ELSE attempt_errors || ',' || ?1 END,
                        not_before_ms = ?2, updated_at_ms = ?3
                  WHERE cache_key = ?4 AND target_node_id = ?9
                    AND state = 'running' AND owner_node_id = ?5
                    AND fence = ?6 AND lease_expires_ms > ?3",
                params![
                    error_code,
                    retry_at_ms,
                    now_ms,
                    cache_key,
                    node_id,
                    fence,
                    if retryable { 1_i64 } else { 0_i64 },
                    max_attempts,
                    target_node_id,
                ],
            )? == 1)
        })
        .await
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
        let location = location.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cluster_fragment_index_locations
                    (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5
                  WHERE EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts
                                 WHERE cache_key = ?1 AND bytes = ?3)
                 ON CONFLICT(cache_key, node_id) DO UPDATE SET
                    bytes = excluded.bytes, verified_at_ms = excluded.verified_at_ms,
                    last_seen_at_ms = excluded.last_seen_at_ms",
                params![
                    location.cache_key,
                    location.node_id,
                    location.bytes,
                    location.verified_at_ms,
                    location.last_seen_at_ms,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn cluster_fragment_index_locations(
        &self,
        cache_key: &str,
    ) -> Result<Vec<ClusterFragmentIndexLocation>, StoreError> {
        let cache_key = cache_key.to_owned();
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms
                   FROM cluster_fragment_index_locations
                  WHERE cache_key = ?1 ORDER BY last_seen_at_ms DESC, node_id LIMIT 128",
            )?;
            let rows = statement.query_map(params![cache_key], |row| {
                Ok(ClusterFragmentIndexLocation {
                    cache_key: row.get(0)?,
                    node_id: row.get(1)?,
                    bytes: row.get(2)?,
                    verified_at_ms: row.get(3)?,
                    last_seen_at_ms: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn forget_cluster_fragment_index_location(
        &self,
        cache_key: &str,
        node_id: &str,
    ) -> Result<bool, StoreError> {
        let cache_key = cache_key.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM cluster_fragment_index_locations
                  WHERE cache_key = ?1 AND node_id = ?2",
                params![cache_key, node_id],
            )? == 1)
        })
        .await
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
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "DELETE FROM cluster_fragment_index_locations
                  WHERE (cache_key, node_id) IN (
                    SELECT cache_key, node_id
                      FROM cluster_fragment_index_locations
                     WHERE last_seen_at_ms < ?1
                     ORDER BY last_seen_at_ms, cache_key, node_id
                     LIMIT ?2
                  )",
                params![older_than_ms, limit],
            )?;
            let candidates = {
                let mut statement = transaction.prepare(
                    "SELECT j.cache_key
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
                        AND j.updated_at_ms < ?1
                        AND NOT EXISTS (
                          SELECT 1 FROM cluster_fragment_index_jobs active_job
                           WHERE active_job.cache_key = j.cache_key
                             AND (active_job.state IN ('queued', 'running')
                               OR active_job.updated_at_ms >= ?1))
                        AND NOT EXISTS (
                          SELECT 1 FROM analysis_requests active_request
                           WHERE active_request.result_cache_key = j.cache_key
                             AND active_request.state IN ('queued', 'running', 'submitted'))
                        AND NOT EXISTS (
                          SELECT 1 FROM cluster_fragment_index_locations l
                           WHERE l.cache_key = j.cache_key)
                      GROUP BY j.cache_key
                      ORDER BY MIN(j.updated_at_ms), j.cache_key LIMIT ?2",
                )?;
                let rows = statement.query_map(params![older_than_ms, limit], |row| row.get(0))?;
                rows.collect::<Result<Vec<String>, _>>()?
            };
            let mut pruned_artifacts = Vec::new();
            for cache_key in candidates {
                transaction.execute(
                    "DELETE FROM cluster_fragment_index_heads
                      WHERE generation_cache_key = ?1
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                          WHERE cache_key = ?1 AND updated_at_ms < ?2)
                        AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                          WHERE cache_key = ?1 AND (state IN ('queued', 'running')
                            OR updated_at_ms >= ?2))
                        AND NOT EXISTS (SELECT 1 FROM analysis_requests
                          WHERE result_cache_key = ?1
                            AND state IN ('queued', 'running', 'submitted'))
                        AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                          WHERE cache_key = ?1)",
                    params![cache_key, older_than_ms],
                )?;
                let artifact = transaction.execute(
                    "DELETE FROM cluster_fragment_index_artifacts
                      WHERE cache_key = ?1
                        AND EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                          WHERE cache_key = ?1
                            AND state IN ('ready', 'failed', 'cancelled')
                            AND updated_at_ms < ?2)
                        AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs
                          WHERE cache_key = ?1 AND (state IN ('queued', 'running')
                            OR updated_at_ms >= ?2))
                        AND NOT EXISTS (SELECT 1 FROM analysis_requests
                          WHERE result_cache_key = ?1
                            AND state IN ('queued', 'running', 'submitted'))
                        AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations
                          WHERE cache_key = ?1)
                        AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_heads
                          WHERE generation_cache_key = ?1)",
                    params![cache_key, older_than_ms],
                )?;
                if artifact == 1 {
                    pruned_artifacts.push(cache_key);
                }
            }
            transaction.execute(
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
                       AND terminal_job.updated_at_ms < ?1
                       AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts artifact
                         WHERE artifact.cache_key = terminal_job.cache_key)
                       AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_heads head
                         WHERE head.generation_cache_key = terminal_job.cache_key)
                       AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_locations location
                         WHERE location.cache_key = terminal_job.cache_key)
                       AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_jobs active_job
                         WHERE active_job.cache_key = terminal_job.cache_key
                           AND (active_job.state IN ('queued', 'running')
                             OR active_job.updated_at_ms >= ?1))
                       AND NOT EXISTS (SELECT 1 FROM analysis_requests active_request
                         WHERE active_request.result_cache_key = terminal_job.cache_key
                           AND active_request.state IN ('queued', 'running', 'submitted'))
                     ORDER BY terminal_job.updated_at_ms, terminal_job.cache_key,
                              terminal_job.target_node_id
                     LIMIT ?2
                  )",
                params![older_than_ms, limit],
            )?;
            transaction.commit()?;
            Ok(pruned_artifacts)
        })
        .await
    }
}

#[cfg(test)]
mod tests {

    /// No statement may reset `attempts` without resetting `attempt_errors`
    /// on exactly the same conditions.
    ///
    /// Three upserts reopen a job row, each with its own reset conditions. A
    /// reset that misses the history leaves a row carrying the codes of a
    /// budget it no longer has — a history that disagrees with the count
    /// printed beside it. Two of the three are reachable from the
    /// backend-neutral Store contract; the forced hand-off is not, because
    /// reaching it twice needs two active forced requests for one identity,
    /// which a unique index forbids. So the rule is asserted on the
    /// statements themselves, by deriving the history reset from the budget
    /// reset it has to mirror.
    #[test]
    fn every_attempts_reset_resets_the_attempt_history() {
        const SOURCE: &str = include_str!("fragment_index_cluster.rs");
        let production = SOURCE
            .split_once("\n#[cfg(test)]")
            .map_or(SOURCE, |(source, _)| source);
        let squeeze = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        const CLOSE: &str = "cluster_fragment_index_jobs.attempts END";
        let mut resets = 0;
        // Keyed on the close, not the open: `attempts = CASE` also appears on
        // `analysis_requests`, which has no history to keep.
        for (close, _) in production.match_indices(CLOSE) {
            let open = production[..close]
                .rfind("attempts = CASE")
                .expect("every reset of this column opens a CASE");
            let budget = &production[open..close + CLOSE.len()];
            // The history reset is the budget reset with the column and the
            // reset value swapped. Anything else is a different rule.
            let expected = squeeze(budget)
                .replace("attempts", "attempt_errors")
                .replace("THEN 0", "THEN ''");
            let tail = &production[open..(close + CLOSE.len() + 900).min(production.len())];
            let window = squeeze(tail);
            assert!(
                window.contains(&expected),
                "an `attempts` reset without the matching `attempt_errors` reset:\n  \
                 wanted {expected}"
            );
            resets += 1;
        }
        assert_eq!(resets, 3, "three upserts reopen a job row");
    }

    use std::collections::HashSet;

    use super::*;
    use crate::store::fragment_index_cluster::{
        analysis_backoff_ms, cluster_fragment_index_generation_key,
    };
    use crate::store::SettingsStore;

    async fn seed_files(store: &SqliteStore) {
        store
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO libraries (id, name, kind, paths) VALUES (1, 'films', 'movies', '[]')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO items (id, library_id, kind, title, sort_title)
                     VALUES (1, 1, 'movie', 'one', 'one'), (2, 1, 'movie', 'two', 'two')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (1, 1, '/one.mkv', 100, 10), (2, 2, '/two.mkv', 100, 20)",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("seed files");
    }

    fn job(file_id: i64, source_mtime: i64, created_at_ms: i64) -> NewClusterFragmentIndexJob {
        let source_sha256 = "a".repeat(64);
        let pipeline_sha256 = "b".repeat(64);
        NewClusterFragmentIndexJob {
            cache_key: cluster_fragment_index_key(
                file_id,
                100,
                source_mtime,
                &source_sha256,
                &pipeline_sha256,
            )
            .expect("valid key"),
            file_id,
            source_size: 100,
            source_mtime,
            source_sha256,
            pipeline_sha256,
            priority: "normal".to_owned(),
            trigger: "background".to_owned(),
            target_node_id: String::new(),
            not_before_ms: created_at_ms,
            created_at_ms,
        }
    }

    fn request(id: &str, force_rebuild: bool, created_at_ms: i64) -> NewAnalysisRequest {
        NewAnalysisRequest {
            request_id: id.to_owned(),
            file_id: 1,
            source_size: 100,
            source_mtime: 10,
            component: "fragment_index".to_owned(),
            pipeline_version: "test-pipeline".to_owned(),
            requested_generation: if force_rebuild {
                format!("generation-{id}")
            } else {
                "test-generation".to_owned()
            },
            priority: if force_rebuild { "forced" } else { "normal" }.to_owned(),
            trigger: "admin".to_owned(),
            force_rebuild,
            target_node_id: "node-a".to_owned(),
            not_before_ms: created_at_ms,
            created_at_ms,
        }
    }

    #[tokio::test]
    async fn analysis_history_is_canonical_paginated_and_classified() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let cache_key = cluster_fragment_index_key(1, 100, 10, &"a".repeat(64), &"b".repeat(64))
            .expect("valid cache key");
        let seeded_key = cache_key.clone();
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, target_node_id, state, fence, attempts, not_before_ms,
                       last_error_code, created_at_ms, updated_at_ms)
                     VALUES (?1, 1, 100, 10, ?2, ?3, 'node-a', 'failed', 1, 1, 0,
                             'source_unavailable', 10, 20)",
                    params![seeded_key, "a".repeat(64), "b".repeat(64)],
                )?;
                conn.execute(
                    "INSERT INTO analysis_requests
                      (request_id, file_id, source_size, source_mtime, component,
                       force_rebuild, target_node_id, state, fence, attempts,
                       not_before_ms, result_cache_key, created_at_ms, updated_at_ms)
                     VALUES ('old-generation', 1, 100, 10, 'fragment_index', 0,
                             'node-a', 'ready', 1, 1, 0, ?1, 1, 10),
                            ('new-generation', 1, 100, 10, 'fragment_index', 1,
                             'node-a', 'submitted', 1, 1, 0, ?1, 2, 20),
                            ('unsupported', 1, 100, 10, 'fragment_index', 0,
                             'node-a', 'failed', 1, 1, 0, NULL, 3, 30),
                            ('deleted', 1, 100, 10, 'fragment_index', 0,
                             'node-a', 'cancelled', 1, 1, 0, NULL, 4, 40),
                            ('superseded', 1, 100, 10, 'fragment_index', 0,
                             'node-a', 'cancelled', 1, 1, 0, NULL, 5, 50),
                            ('missing-ready', 999, 100, 10, 'fragment_index', 0,
                             'node-a', 'ready', 1, 1, 0, NULL, 6, 60)",
                    params![seeded_key],
                )?;
                conn.execute(
                    "UPDATE analysis_requests SET last_error_code = 'unsupported'
                      WHERE request_id = 'unsupported'",
                    [],
                )?;
                conn.execute(
                    "UPDATE analysis_requests SET last_error_code = 'source_deleted'
                      WHERE request_id = 'deleted'",
                    [],
                )?;
                conn.execute(
                    "UPDATE analysis_requests SET last_error_code = 'source_superseded'
                      WHERE request_id = 'superseded'",
                    [],
                )?;
                for index in 0..25_i64 {
                    conn.execute(
                        "INSERT INTO analysis_requests
                          (request_id, file_id, source_size, source_mtime, component,
                           force_rebuild, target_node_id, state, fence, attempts,
                           not_before_ms, created_at_ms, updated_at_ms)
                         VALUES (?1, 1, 100, 10, 'fragment_index', 0, 'node-a',
                                 'ready', 1, 1, 0, ?2, ?2)",
                        params![format!("ready-{index:02}"), 100 + index],
                    )?;
                }
                Ok(())
            })
            .await
            .expect("seed analysis history");

        let mut cursor = None;
        let mut rows = Vec::new();
        loop {
            let page = store
                .analysis_history(&AnalysisHistoryQuery {
                    limit: 10,
                    cursor,
                    filter: AnalysisHistoryFilter::All,
                    search: String::new(),
                    states: Vec::new(),
                    now_ms: 0,
                })
                .await
                .expect("history page");
            assert_eq!(page.filtered_total, 31);
            rows.extend(page.rows);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(rows.len(), 31);
        let keys = rows.iter().map(|row| &row.row_key).collect::<HashSet<_>>();
        assert_eq!(keys.len(), rows.len(), "keyset pages must not overlap");
        let old = rows
            .iter()
            .find(|row| row.request_id == "old-generation")
            .expect("old generation");
        let new = rows
            .iter()
            .find(|row| row.request_id == "new-generation")
            .expect("new generation");
        assert!(old.job_id.is_empty(), "mutable job leaked into old history");
        assert_eq!(old.state, "ready");
        assert_eq!(new.job_id, cache_key);
        assert_eq!(new.state, "failed");
        assert_eq!(new.action, "retry");
        assert_eq!(old.action, "rebuild");
        assert_eq!(
            rows.iter()
                .find(|row| row.request_id == "superseded")
                .expect("superseded request")
                .action,
            "analyze_current"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.request_id == "missing-ready")
                .expect("deleted ready file")
                .action,
            "none"
        );

        let expected = store
            .analysis_history(&AnalysisHistoryQuery {
                limit: 10,
                cursor: None,
                filter: AnalysisHistoryFilter::Expected,
                search: String::new(),
                states: Vec::new(),
                now_ms: 0,
            })
            .await
            .expect("expected outcomes");
        assert_eq!(expected.filtered_total, 3);
        assert!(expected
            .rows
            .iter()
            .all(|row| matches!(row.disposition.as_str(), "expected" | "unsupported")));

        let stale = store
            .analysis_history(&AnalysisHistoryQuery {
                limit: 10,
                cursor: None,
                filter: AnalysisHistoryFilter::All,
                search: String::new(),
                states: vec!["stale".to_owned()],
                now_ms: 1_000,
            })
            .await
            .expect("stale durable state");
        assert_eq!(stale.filtered_total, 1);
        assert_eq!(stale.rows[0].request_id, "superseded");

        let summary = store.analysis_status_summary().await.expect("summary");
        assert_eq!(summary.total, 31);
        assert_eq!(summary.attention, 1);
        assert_eq!(summary.expected, 3);
        assert_eq!(summary.ready, 27);
        assert_eq!(summary.latest_error_code, "source_unavailable");

        let past_end = store
            .analysis_history(&AnalysisHistoryQuery {
                limit: 10,
                cursor: Some(AnalysisHistoryCursor {
                    sort_rank: 6,
                    updated_at_ms: 0,
                    row_key: "past-the-retained-tail".to_owned(),
                }),
                filter: AnalysisHistoryFilter::All,
                search: String::new(),
                states: Vec::new(),
                now_ms: 0,
            })
            .await
            .expect("valid cursor past retained tail");
        assert!(past_end.rows.is_empty());
        assert_eq!(past_end.filtered_total, 31);
    }

    #[tokio::test]
    async fn operator_requests_are_durable_coalesced_and_lease_fenced() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let first = store
            .enqueue_analysis_request(&request("request-1", false, 10))
            .await
            .expect("enqueue request");
        assert_eq!(first.request_id, "request-1");
        let joined = store
            .enqueue_analysis_request(&request("request-2", false, 11))
            .await
            .expect("join active request");
        assert_eq!(joined.request_id, first.request_id);

        assert!(store
            .claim_analysis_request("node-b", 20, 1_020)
            .await
            .expect("wrong-node claim")
            .is_none());
        let claimed = store
            .claim_analysis_request("node-a", 20, 1_020)
            .await
            .expect("claim")
            .expect("request claim");
        assert_eq!(claimed.state, "running");
        assert_eq!(claimed.fence, 1);
        assert!(!store
            .renew_analysis_request(&claimed.request_id, "node-a", 0, 21, 1_021)
            .await
            .expect("stale renewal"));
        assert!(store
            .renew_analysis_request(&claimed.request_id, "node-a", 1, 21, 1_021)
            .await
            .expect("current renewal"));
        assert!(store
            .fail_analysis_request(&claimed.request_id, "node-a", 1, "source_unavailable", 22)
            .await
            .expect("fail current claim"));
        let rows = store.analysis_requests(10).await.expect("list requests");
        assert_eq!(rows[0].state, "failed");
        assert_eq!(rows[0].last_error_code, "source_unavailable");

        let successor = store
            .enqueue_analysis_request(&request("request-3", true, 23))
            .await
            .expect("enqueue terminal successor");
        assert_eq!(successor.request_id, "request-3");
        assert!(successor.force_rebuild);
        let labels = store.analysis_file_labels(10).await.expect("file labels");
        assert_eq!(labels[0].title, "one");
    }

    #[tokio::test]
    async fn submitted_request_follows_its_fenced_index_job_to_ready() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .enqueue_analysis_request(&request("request-1", false, 10))
            .await
            .expect("enqueue request");
        let claimed = store
            .claim_analysis_request("node-a", 10, 1_010)
            .await
            .expect("claim")
            .expect("request claim");
        let mut job = job(1, 10, 11);
        job.target_node_id = claimed.target_node_id.clone();
        job.priority = claimed.priority.clone();
        job.trigger = claimed.trigger.clone();
        assert!(store
            .submit_fragment_index_analysis(&claimed, &job, 12)
            .await
            .expect("submit request and job atomically"));
        let cache_key = job.cache_key.clone();
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     SELECT cache_key, file_id, source_size, source_mtime, source_sha256,
                            pipeline_sha256, ?2, 10, 'node-a', 13
                       FROM cluster_fragment_index_jobs WHERE cache_key = ?1",
                    params![cache_key, "c".repeat(64)],
                )?;
                conn.execute(
                    "UPDATE cluster_fragment_index_jobs SET state = 'ready', updated_at_ms = 13
                      WHERE cache_key = ?1",
                    params![cache_key],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_locations
                      (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                     VALUES (?1, 'node-a', 10, 13, 13)",
                    params![cache_key],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_heads
                      (logical_cache_key, generation_cache_key, request_id, updated_at_ms)
                     VALUES (?1, ?1, 'request-1', 13)",
                    params![cache_key],
                )?;
                Ok(())
            })
            .await
            .expect("publish fixture");
        assert_eq!(
            store
                .settle_analysis_requests(14)
                .await
                .expect("settle requests"),
            1
        );
        let settled = store.analysis_requests(10).await.expect("list requests");
        assert_eq!(settled[0].state, "ready");
        assert_eq!(settled[0].result_cache_key, job.cache_key);
    }

    #[tokio::test]
    async fn forced_successor_keeps_the_published_artifact_serving() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let job = job(1, 10, 10);
        let seeded = job.clone();
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 2, 2, 0, 10, 10)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                        "c".repeat(64),
                    ],
                )?;
                Ok(())
            })
            .await
            .expect("seed ready generation");

        let mut successor = job.clone();
        successor.created_at_ms = 20;
        successor.not_before_ms = 20;
        store
            .enqueue_analysis_request(&request("forced-request", true, 20))
            .await
            .expect("enqueue forced request");
        let claimed = store
            .claim_analysis_request("node-a", 20, 1_020)
            .await
            .expect("claim forced request")
            .expect("forced request claim");
        successor.cache_key = cluster_fragment_index_generation_key(
            successor.file_id,
            successor.source_size,
            successor.source_mtime,
            &successor.source_sha256,
            &successor.pipeline_sha256,
            &claimed.requested_generation,
        )
        .expect("forced generation key");
        successor.target_node_id = claimed.target_node_id.clone();
        successor.priority = claimed.priority.clone();
        successor.trigger = claimed.trigger.clone();
        assert!(store
            .submit_fragment_index_analysis(&claimed, &successor, 21)
            .await
            .expect("force successor"));
        assert!(store
            .cluster_fragment_index_artifact(&job.cache_key)
            .await
            .expect("read serving artifact")
            .is_some());
        let reopened = store
            .cluster_fragment_index_job(&successor.cache_key, &successor.target_node_id)
            .await
            .expect("read reopened job")
            .expect("job");
        assert_eq!(reopened.state, "queued");
        assert_eq!(reopened.attempts, 0);
    }

    #[tokio::test]
    async fn stale_request_owner_cannot_create_or_reopen_a_worker_job() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .enqueue_analysis_request(&request("request-1", true, 10))
            .await
            .expect("enqueue request");
        let stale = store
            .claim_analysis_request("node-a", 10, 20)
            .await
            .expect("first claim")
            .expect("first owner");
        let current = store
            .claim_analysis_request("node-a", 20, 1_020)
            .await
            .expect("replacement claim")
            .map(|_| panic!("expired lease must enter retry wait"));
        assert!(current.is_none());
        let retry_at = 20 + analysis_backoff_ms("request-1", 1, 5_000, 300_000);
        let current = store
            .claim_analysis_request("node-a", retry_at, retry_at + 1_000)
            .await
            .expect("replacement claim after retry wait")
            .expect("replacement owner");
        assert!(current.fence > stale.fence);

        let mut job = job(1, 10, 21);
        job.target_node_id = stale.target_node_id.clone();
        job.priority = stale.priority.clone();
        job.trigger = stale.trigger.clone();
        job.cache_key = cluster_fragment_index_generation_key(
            job.file_id,
            job.source_size,
            job.source_mtime,
            &job.source_sha256,
            &job.pipeline_sha256,
            &stale.requested_generation,
        )
        .expect("forced stale generation key");
        assert!(!store
            .submit_fragment_index_analysis(&stale, &job, 21)
            .await
            .expect("stale handoff is a fenced no-op"));
        assert!(store
            .cluster_fragment_index_job(&job.cache_key, &job.target_node_id)
            .await
            .expect("read worker job")
            .is_none());
        let requests = store.analysis_requests(10).await.expect("list requests");
        assert_eq!(requests[0].state, "running");
        assert_eq!(requests[0].fence, current.fence);
    }

    #[tokio::test]
    async fn foreground_retry_yields_without_spending_an_attempt() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .enqueue_analysis_request(&request("request-1", false, 10))
            .await
            .expect("enqueue request");
        let claimed = store
            .claim_analysis_request("node-a", 10, 1_010)
            .await
            .expect("claim")
            .expect("request claim");
        assert_eq!(claimed.attempts, 1);
        assert!(store
            .retry_analysis_request(&claimed, "foreground_preempted", 11, 21, false,)
            .await
            .expect("yield request"));
        let retried = store.analysis_requests(10).await.expect("list requests");
        assert_eq!(retried[0].state, "queued");
        assert_eq!(retried[0].attempts, 0);
        assert_eq!(retried[0].not_before_ms, 21);
        assert_eq!(retried[0].last_error_code, "foreground_preempted");
        assert!(store
            .claim_analysis_request("node-a", 20, 1_020)
            .await
            .expect("early claim")
            .is_none());
    }

    #[tokio::test]
    async fn source_replacement_cancels_old_generation_and_admits_new_generation() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .enqueue_analysis_request(&request("old-source", false, 10))
            .await
            .expect("enqueue old source");
        let old_claim = store
            .claim_analysis_request("node-a", 10, 1_010)
            .await
            .expect("claim old source")
            .expect("old source claim");
        assert!(store
            .record_analysis_request_phase(&old_claim, "hashing", None, 11)
            .await
            .expect("record old source phase"));
        store
            .with_conn(|conn| {
                conn.execute("UPDATE files SET size = 200, mtime = 30 WHERE id = 1", [])?;
                Ok(())
            })
            .await
            .expect("replace source generation");
        let mut replacement = request("new-source", false, 30);
        replacement.source_size = 200;
        replacement.source_mtime = 30;
        let admitted = store
            .enqueue_analysis_request(&replacement)
            .await
            .expect("enqueue new source");
        assert_eq!(admitted.request_id, "new-source");
        let requests = store.analysis_requests(10).await.expect("list requests");
        assert_eq!(requests.len(), 2);
        let old = requests
            .iter()
            .find(|row| row.request_id == "old-source")
            .expect("old generation");
        assert_eq!(old.state, "cancelled");
        assert_eq!(old.last_error_code, "source_superseded");
        let attempts = store
            .analysis_attempts("old-source", 10)
            .await
            .expect("old source attempts");
        assert_eq!(attempts[0].phase, "stale");
        assert_eq!(attempts[0].terminal_code, "source_superseded");
    }

    #[tokio::test]
    async fn source_deletion_terminalizes_the_live_attempt_before_cascade() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .enqueue_analysis_request(&request("deleted-source", false, 10))
            .await
            .expect("enqueue source");
        let claim = store
            .claim_analysis_request("node-a", 10, 1_010)
            .await
            .expect("claim source")
            .expect("source claim");
        assert!(store
            .record_analysis_request_phase(&claim, "hashing", None, 11)
            .await
            .expect("record source phase"));
        store
            .with_conn(|conn| {
                conn.execute("DELETE FROM files WHERE id = 1", [])?;
                Ok(())
            })
            .await
            .expect("delete source");
        let request = store
            .analysis_request("deleted-source")
            .await
            .expect("read request")
            .expect("retained request");
        assert_eq!(request.state, "cancelled");
        assert_eq!(request.last_error_code, "source_deleted");
        let attempts = store
            .analysis_attempts("deleted-source", 10)
            .await
            .expect("deleted source attempts");
        assert_eq!(attempts[0].phase, "canceled");
        assert_eq!(attempts[0].terminal_code, "source_deleted");
    }

    #[tokio::test]
    async fn scanner_generation_change_does_not_reuse_old_source_identity() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let ready = job(1, 10, 10);
        let seeded = ready.clone();
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 1, 1, 0, 10, 10)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                        "c".repeat(64),
                    ],
                )?;
                conn.execute("UPDATE files SET mtime = 30 WHERE id = 1", [])?;
                Ok(())
            })
            .await
            .expect("seed ready artifact and replace scanner generation");

        let mut replacement = request("replacement", false, 30);
        replacement.source_mtime = 30;
        store
            .enqueue_analysis_request(&replacement)
            .await
            .expect("enqueue replacement generation");
        let claimed = store
            .claim_analysis_request("node-a", 30, 1_030)
            .await
            .expect("claim replacement")
            .expect("replacement claim");
        let mut replacement_job = ready.clone();
        replacement_job.source_mtime = 30;
        replacement_job.cache_key = cluster_fragment_index_key(
            replacement_job.file_id,
            replacement_job.source_size,
            replacement_job.source_mtime,
            &replacement_job.source_sha256,
            &replacement_job.pipeline_sha256,
        )
        .expect("replacement source identity");
        replacement_job.target_node_id = claimed.target_node_id.clone();
        replacement_job.priority = claimed.priority.clone();
        replacement_job.trigger = claimed.trigger.clone();
        replacement_job.created_at_ms = 31;
        replacement_job.not_before_ms = 31;
        assert!(store
            .submit_fragment_index_analysis(&claimed, &replacement_job, 31)
            .await
            .expect("submit replacement source identity"));
        assert_eq!(
            store
                .settle_analysis_requests(32)
                .await
                .expect("settle replacement"),
            0
        );
        let requests = store.analysis_requests(10).await.expect("list requests");
        assert_eq!(requests[0].state, "submitted");
        assert!(store
            .cluster_fragment_index_artifact(&ready.cache_key)
            .await
            .expect("read artifact")
            .is_some());
    }

    #[tokio::test]
    async fn byte_identical_files_queue_independent_structural_work() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let old = job(1, 10, 1);
        let seeded = old.clone();
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'cancelled', 1, 3, 0, 1, 1)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 1)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                        "c".repeat(64),
                    ],
                )?;
                Ok(())
            })
            .await
            .expect("seed cancelled generation");
        let mut replacement = request("rebind", false, 30_000_001);
        replacement.file_id = 2;
        replacement.source_mtime = 20;
        store
            .enqueue_analysis_request(&replacement)
            .await
            .expect("enqueue surviving file");
        let claimed = store
            .claim_analysis_request("node-a", 30_000_001, 30_001_001)
            .await
            .expect("claim surviving file")
            .expect("surviving claim");
        let mut rebound = old.clone();
        rebound.file_id = 2;
        rebound.source_mtime = 20;
        rebound.cache_key = cluster_fragment_index_key(
            rebound.file_id,
            rebound.source_size,
            rebound.source_mtime,
            &rebound.source_sha256,
            &rebound.pipeline_sha256,
        )
        .expect("independent file identity");
        rebound.target_node_id = claimed.target_node_id.clone();
        rebound.priority = claimed.priority.clone();
        rebound.trigger = claimed.trigger.clone();
        rebound.created_at_ms = 30_000_002;
        rebound.not_before_ms = 30_000_002;
        assert!(store
            .submit_fragment_index_analysis(&claimed, &rebound, 30_000_002)
            .await
            .expect("rebind cancelled content"));
        let job = store
            .cluster_fragment_index_job(&rebound.cache_key, &rebound.target_node_id)
            .await
            .expect("read rebound job")
            .expect("rebound job");
        assert_eq!(job.state, "queued");
        assert_eq!(job.file_id, 2);
        assert_eq!(job.created_at_ms, 30_000_002);
        assert!(store
            .cluster_fragment_index_artifact(&old.cache_key)
            .await
            .expect("read retained artifact")
            .is_some());
    }

    #[tokio::test]
    async fn charged_retries_exhaust_to_a_terminal_attempt_limit() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .put_setting(crate::store::keys::ANALYSIS_MAX_ATTEMPTS, "2")
            .await
            .expect("configure attempt budget");
        store
            .enqueue_analysis_request(&request("attempts", false, 10))
            .await
            .expect("enqueue request");
        let mut now = 10;
        for expected_attempt in 1..=2 {
            let claimed = store
                .claim_analysis_request("node-a", now, now + 1_000)
                .await
                .expect("claim retry")
                .expect("retry claim");
            assert_eq!(claimed.attempts, expected_attempt);
            assert!(store
                .retry_analysis_request(
                    &claimed,
                    "source_attestation_failed",
                    now + 1,
                    now + 2,
                    true,
                )
                .await
                .expect("queue charged retry"));
            now += 2;
        }
        assert!(store
            .claim_analysis_request("node-a", now, now + 1_000)
            .await
            .expect("settle attempt limit")
            .is_none());
        let requests = store.analysis_requests(10).await.expect("list requests");
        assert_eq!(requests[0].state, "failed");
        assert_eq!(requests[0].attempts, 2);
        assert_eq!(requests[0].last_error_code, "attempt_limit");
    }

    #[tokio::test]
    async fn terminal_analysis_history_is_age_and_generation_bounded() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .with_conn(|conn| {
                for generation in 0_i64..25 {
                    conn.execute(
                        "INSERT INTO analysis_requests
                          (request_id, file_id, source_size, source_mtime, component,
                           force_rebuild, target_node_id, state, fence, attempts,
                           not_before_ms, created_at_ms, updated_at_ms)
                         VALUES (?1, 1, 100, 10, 'fragment_index', 0, 'node-a',
                                 'ready', 0, 1, 0, ?2, ?2)",
                        params![format!("history-{generation:02}"), generation],
                    )?;
                }
                Ok(())
            })
            .await
            .expect("seed history");
        assert_eq!(
            store
                .prune_analysis_requests(-1, 100)
                .await
                .expect("prune generation cap"),
            5
        );
        assert_eq!(
            store.analysis_requests(100).await.expect("history").len(),
            20
        );
        assert_eq!(
            store
                .prune_analysis_requests(100, 100)
                .await
                .expect("prune by age"),
            19
        );
        assert_eq!(
            store.analysis_requests(100).await.expect("history").len(),
            1
        );
    }

    #[tokio::test]
    async fn terminal_analysis_history_has_a_hard_global_cap() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .with_conn(|conn| {
                let transaction = conn.unchecked_transaction()?;
                for generation in 0_i64..=8_192 {
                    transaction.execute(
                        "INSERT INTO analysis_requests
                          (request_id, file_id, source_size, source_mtime, component,
                           force_rebuild, target_node_id, state, fence, attempts,
                           not_before_ms, created_at_ms, updated_at_ms)
                         VALUES (?1, 1, 100, ?2, 'fragment_index', 0, 'node-a',
                                 'failed', 0, 1, 0, ?2, ?2)",
                        params![format!("terminal-{generation:05}"), generation],
                    )?;
                }
                transaction.execute(
                    "INSERT INTO analysis_requests
                      (request_id, file_id, source_size, source_mtime, component,
                       force_rebuild, target_node_id, state, fence, attempts,
                       not_before_ms, created_at_ms, updated_at_ms)
                     VALUES ('cap-trigger', 2, 100, 20, 'fragment_index', 0,
                             'node-a', 'queued', 0, 0, 9000, 9000, 9000)",
                    [],
                )?;
                transaction.execute(
                    "UPDATE analysis_requests SET state = 'failed'
                      WHERE request_id = 'cap-trigger'",
                    [],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
            .expect("exercise terminal cap trigger");
        let count = store
            .with_read(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM analysis_requests
                      WHERE state IN ('ready', 'failed', 'cancelled')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(Into::into)
            })
            .await
            .expect("count terminal history");
        assert_eq!(count, 8_192);
    }

    #[tokio::test]
    async fn duplicate_content_rebinds_a_cancelled_job_to_a_surviving_file() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let original = job(1, 10, 10);
        assert!(store
            .enqueue_cluster_fragment_index(&original)
            .await
            .expect("enqueue original"));
        store
            .with_conn(|conn| {
                conn.execute("DELETE FROM files WHERE id = 1", [])?;
                Ok(())
            })
            .await
            .expect("delete original");

        let replacement = job(2, 20, 20);
        assert!(store
            .enqueue_cluster_fragment_index(&replacement)
            .await
            .expect("rebind replacement"));
        let claimed = store
            .claim_cluster_fragment_index("node-b", &[], 20, 1_020)
            .await
            .expect("claim")
            .expect("replacement claim");
        assert_eq!(claimed.file_id, 2);
        assert_eq!(claimed.source_mtime, 20);
        assert_eq!(claimed.source_sha256, replacement.source_sha256);
    }

    #[tokio::test]
    async fn byte_identical_replacement_publishes_an_independent_artifact() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let original = job(1, 10, 10);
        let original_key = original.cache_key.clone();
        let seeded = original.clone();
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 1, 1, 0, 10, 10)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                        "e".repeat(64),
                    ],
                )?;
                conn.execute("DELETE FROM files WHERE id = 1", [])?;
                Ok(())
            })
            .await
            .expect("publish then delete original");

        let replacement = job(2, 20, 20);
        assert!(store
            .enqueue_cluster_fragment_index(&replacement)
            .await
            .expect("enqueue replacement"));
        let claimed = store
            .claim_cluster_fragment_index("node-b", &[], 20, 1_020)
            .await
            .expect("claim")
            .expect("replacement claim");
        assert_eq!(claimed.file_id, 2);

        let rebuilt = ClusterFragmentIndexArtifact {
            cache_key: replacement.cache_key.clone(),
            file_id: 2,
            source_size: 100,
            source_mtime: 20,
            source_sha256: replacement.source_sha256,
            pipeline_sha256: replacement.pipeline_sha256,
            blob_sha256: "e".repeat(64),
            bytes: 10,
            built_by_node_id: "node-b".to_owned(),
            built_at_ms: 21,
        };
        let location = ClusterFragmentIndexLocation {
            cache_key: replacement.cache_key.clone(),
            node_id: "node-b".to_owned(),
            bytes: 10,
            verified_at_ms: 21,
            last_seen_at_ms: 21,
        };
        assert!(store
            .complete_cluster_fragment_index(&claimed, &rebuilt, &location, 21)
            .await
            .expect("complete repair"));
        assert_eq!(
            store
                .cluster_fragment_index_artifact(&replacement.cache_key)
                .await
                .expect("artifact")
                .expect("published artifact")
                .blob_sha256,
            "e".repeat(64)
        );
        assert_eq!(
            store
                .cluster_fragment_index_locations(&replacement.cache_key)
                .await
                .expect("locations")
                .into_iter()
                .map(|location| location.node_id)
                .collect::<Vec<_>>(),
            vec!["node-b".to_owned()]
        );
        assert!(store
            .cluster_fragment_index_artifact(&original_key)
            .await
            .expect("original artifact")
            .is_some());
    }

    #[tokio::test]
    async fn ready_artifact_at_the_attempt_limit_gets_a_fresh_repair_budget() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let original = job(1, 10, 10);
        let seeded = original.clone();
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 5, 5, 0, 1, 1)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 1)",
                    params![
                        seeded.cache_key,
                        seeded.file_id,
                        seeded.source_size,
                        seeded.source_mtime,
                        seeded.source_sha256,
                        seeded.pipeline_sha256,
                        "e".repeat(64),
                    ],
                )?;
                Ok(())
            })
            .await
            .expect("seed exhausted ready artifact");

        let mut repair = original;
        let now = QUEUE_ELIGIBILITY_MS.saturating_add(10);
        repair.not_before_ms = now;
        repair.created_at_ms = now;
        assert!(store
            .requeue_cluster_fragment_index(&repair)
            .await
            .expect("requeue"));
        let claimed = store
            .claim_cluster_fragment_index("node-b", &[], now, now.saturating_add(1_000))
            .await
            .expect("claim")
            .expect("fresh repair claim");
        assert_eq!(claimed.attempts, 1);
        assert_eq!(claimed.created_at_ms, now);
    }

    #[tokio::test]
    async fn node_local_exclusions_skip_an_unreadable_head_job() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let first = job(1, 10, 10);
        let mut second = job(2, 20, 20);
        second.source_sha256 = "c".repeat(64);
        second.cache_key = cluster_fragment_index_key(
            second.file_id,
            second.source_size,
            second.source_mtime,
            &second.source_sha256,
            &second.pipeline_sha256,
        )
        .expect("second key");
        assert!(store
            .enqueue_cluster_fragment_index(&first)
            .await
            .expect("enqueue first"));
        assert!(store
            .enqueue_cluster_fragment_index(&second)
            .await
            .expect("enqueue second"));

        let claimed = store
            .claim_cluster_fragment_index(
                "node-b",
                std::slice::from_ref(&first.cache_key),
                20,
                1_020,
            )
            .await
            .expect("claim")
            .expect("later runnable job");
        assert_eq!(claimed.cache_key, second.cache_key);
    }

    #[tokio::test]
    async fn exclusions_skip_more_than_128_unreadable_head_jobs() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let mut exclusions = Vec::new();
        let mut runnable = String::new();
        for sequence in 1_i64..=130 {
            let mut candidate = job(1, 10, sequence);
            candidate.source_sha256 = format!("{sequence:064x}");
            candidate.cache_key = cluster_fragment_index_key(
                candidate.file_id,
                candidate.source_size,
                candidate.source_mtime,
                &candidate.source_sha256,
                &candidate.pipeline_sha256,
            )
            .expect("candidate key");
            assert!(store
                .enqueue_cluster_fragment_index(&candidate)
                .await
                .expect("enqueue candidate"));
            if sequence < 130 {
                exclusions.push(candidate.cache_key);
            } else {
                runnable = candidate.cache_key;
            }
        }

        let claimed = store
            .claim_cluster_fragment_index("node-b", &exclusions, 130, 1_130)
            .await
            .expect("claim")
            .expect("later runnable job");
        assert_eq!(claimed.cache_key, runnable);
    }

    #[tokio::test]
    async fn expired_unreadable_queue_cannot_wedge_later_admission() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        store
            .with_conn(|conn| {
                let mut insert = conn.prepare(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, owner_node_id, fence, lease_expires_ms,
                       attempts, not_before_ms, created_at_ms, updated_at_ms)
                     VALUES (?1, 1, 100, 10, ?2, ?3, ?4, ?5, 0, ?6, ?7, 1, 1, 1)",
                )?;
                for sequence in 0_i64..MAX_ACTIVE_JOBS {
                    let crashed = sequence >= MAX_ACTIVE_JOBS / 2;
                    insert.execute(params![
                        format!("{sequence:064x}"),
                        format!("{:064x}", sequence.saturating_add(1)),
                        "b".repeat(64),
                        if crashed { "running" } else { "queued" },
                        crashed.then_some("lost-node"),
                        crashed.then_some(1_i64),
                        if crashed {
                            crate::store::fragment_index_cluster::DEFAULT_ANALYSIS_MAX_ATTEMPTS
                        } else {
                            0
                        },
                    ])?;
                }
                Ok(())
            })
            .await
            .expect("fill unreadable queue");

        let now = QUEUE_ELIGIBILITY_MS.saturating_add(10);
        let mut readable = job(2, 20, now);
        readable.source_sha256 = "f".repeat(64);
        readable.pipeline_sha256 = "e".repeat(64);
        readable.cache_key = cluster_fragment_index_key(
            readable.file_id,
            readable.source_size,
            readable.source_mtime,
            &readable.source_sha256,
            &readable.pipeline_sha256,
        )
        .expect("readable key");
        assert!(store
            .enqueue_cluster_fragment_index(&readable)
            .await
            .expect("admit after eligibility window"));
        let claimed = store
            .claim_cluster_fragment_index("node-b", &[], now, now.saturating_add(1_000))
            .await
            .expect("claim")
            .expect("later readable job");
        assert_eq!(claimed.cache_key, readable.cache_key);
    }

    #[tokio::test]
    async fn failed_repair_cannot_bypass_the_active_queue_cap() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let mut repair = job(2, 20, 100);
        repair.source_sha256 = "f".repeat(64);
        repair.pipeline_sha256 = "e".repeat(64);
        repair.cache_key = cluster_fragment_index_key(
            repair.file_id,
            repair.source_size,
            repair.source_mtime,
            &repair.source_sha256,
            &repair.pipeline_sha256,
        )
        .expect("repair key");
        let seeded_repair = repair.clone();
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms, last_error_code)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'failed', 1, 1, 1, 1, 1,
                       'queue_expired')",
                    params![
                        seeded_repair.cache_key,
                        seeded_repair.file_id,
                        seeded_repair.source_size,
                        seeded_repair.source_mtime,
                        seeded_repair.source_sha256,
                        seeded_repair.pipeline_sha256,
                    ],
                )?;
                conn.execute(
                    "INSERT INTO cluster_fragment_index_artifacts
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 1)",
                    params![
                        seeded_repair.cache_key,
                        seeded_repair.file_id,
                        seeded_repair.source_size,
                        seeded_repair.source_mtime,
                        seeded_repair.source_sha256,
                        seeded_repair.pipeline_sha256,
                        "d".repeat(64),
                    ],
                )?;
                let mut insert = conn.prepare(
                    "INSERT INTO cluster_fragment_index_jobs
                      (cache_key, file_id, source_size, source_mtime, source_sha256,
                       pipeline_sha256, state, fence, attempts, not_before_ms,
                       created_at_ms, updated_at_ms)
                     VALUES (?1, 1, 100, 10, ?2, ?3, 'queued', 0, 0, 100, 100, 100)",
                )?;
                for sequence in 0_i64..MAX_ACTIVE_JOBS {
                    insert.execute(params![
                        format!("{sequence:064x}"),
                        format!("{:064x}", sequence.saturating_add(1)),
                        "b".repeat(64),
                    ])?;
                }
                Ok(())
            })
            .await
            .expect("seed full queue and failed repair");

        assert!(!store
            .requeue_cluster_fragment_index(&repair)
            .await
            .expect("full requeue"));
        let active = store
            .with_read(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM cluster_fragment_index_jobs
                      WHERE state IN ('queued', 'running')",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(Into::into)
            })
            .await
            .expect("active count");
        assert_eq!(active, MAX_ACTIVE_JOBS);

        store
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE cluster_fragment_index_jobs SET state = 'failed'
                      WHERE cache_key = ?1",
                    params!["0".repeat(64)],
                )?;
                Ok(())
            })
            .await
            .expect("free one slot");
        assert!(store
            .requeue_cluster_fragment_index(&repair)
            .await
            .expect("requeue with capacity"));
    }

    #[tokio::test]
    async fn retention_prunes_only_terminal_artifacts_without_recent_holders() {
        let store = SqliteStore::open_in_memory().expect("store");
        seed_files(&store).await;
        let stale = job(1, 10, 10);
        let mut current = job(2, 20, 10);
        current.source_sha256 = "c".repeat(64);
        current.pipeline_sha256 = "d".repeat(64);
        current.cache_key = cluster_fragment_index_key(
            current.file_id,
            current.source_size,
            current.source_mtime,
            &current.source_sha256,
            &current.pipeline_sha256,
        )
        .expect("current key");
        let stale_key = stale.cache_key.clone();
        let current_key = current.cache_key.clone();
        let stale_seed = stale.clone();
        let current_seed = current.clone();
        store
            .with_conn(move |conn| {
                for candidate in [&stale_seed, &current_seed] {
                    conn.execute(
                        "INSERT INTO cluster_fragment_index_jobs
                          (cache_key, file_id, source_size, source_mtime, source_sha256,
                           pipeline_sha256, state, fence, attempts, not_before_ms,
                           created_at_ms, updated_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 1, 1, 0, 10, 10)",
                        params![
                            candidate.cache_key,
                            candidate.file_id,
                            candidate.source_size,
                            candidate.source_mtime,
                            candidate.source_sha256,
                            candidate.pipeline_sha256,
                        ],
                    )?;
                    conn.execute(
                        "INSERT INTO cluster_fragment_index_artifacts
                          (cache_key, file_id, source_size, source_mtime, source_sha256,
                           pipeline_sha256, blob_sha256, bytes, built_by_node_id, built_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, 'node-a', 10)",
                        params![
                            candidate.cache_key,
                            candidate.file_id,
                            candidate.source_size,
                            candidate.source_mtime,
                            candidate.source_sha256,
                            candidate.pipeline_sha256,
                            "e".repeat(64),
                        ],
                    )?;
                }
                conn.execute(
                    "INSERT INTO cluster_fragment_index_locations
                      (cache_key, node_id, bytes, verified_at_ms, last_seen_at_ms)
                     VALUES (?1, 'node-a', 10, 20, 20), (?2, 'node-a', 10, 200, 200)",
                    params![stale_key, current_key],
                )?;
                Ok(())
            })
            .await
            .expect("seed artifacts");

        assert_eq!(
            store
                .prune_cluster_fragment_indexes(100, 128)
                .await
                .expect("prune"),
            vec![stale.cache_key.clone()]
        );
        assert!(store
            .cluster_fragment_index_artifact(&stale.cache_key)
            .await
            .expect("stale lookup")
            .is_none());
        assert!(store
            .cluster_fragment_index_artifact(&current.cache_key)
            .await
            .expect("current lookup")
            .is_some());
    }
}
