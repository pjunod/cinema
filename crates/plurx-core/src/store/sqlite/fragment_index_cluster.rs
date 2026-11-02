use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::SqliteStore;
use crate::error::StoreError;
use crate::store::fragment_index_cluster::{
    bounded_analysis_backoff_base_secs, bounded_analysis_backoff_max_secs,
    bounded_analysis_max_attempts, valid_subtitle_source_publication,
};
use crate::store::fragment_index_cluster::{ANALYSIS_CANONICAL_CTE, ANALYSIS_SUMMARY_CTE};
use crate::store::fragment_index_cluster::{
    SUBTITLE_BACKFILL_CANDIDATES, SUBTITLE_BACKFILL_DIAGNOSTICS,
    SUBTITLE_BACKFILL_TERMINAL_SETTLE_MS, SUBTITLE_SOURCE_READY_UNCOVERED,
};
use crate::store::{
    AnalysisAttempt, AnalysisFileLabel, AnalysisHistoryCursor, AnalysisHistoryFilter,
    AnalysisHistoryPage, AnalysisHistoryQuery, AnalysisHistoryRow, AnalysisIndexRepairCandidate,
    AnalysisIndexRepairResult, AnalysisRequest, AnalysisStatusSummary,
    ClusterFragmentIndexArtifact, ClusterFragmentIndexJob, ClusterFragmentIndexLocation,
    ClusterFragmentIndexStore, FragmentIndexSourceObservation, NewAnalysisRequest,
    NewClusterFragmentIndexJob, SubtitleBackfillCandidate, SubtitleBackfillDiagnostics,
    SubtitleSourcePublication, SubtitleSourceStamp, SUBTITLE_SOURCE_REPAIR_LIMIT,
    SUBTITLE_SOURCE_REPAIR_WINDOW_MS,
};

const MAX_ERROR_CODE_BYTES: usize = 64;
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
    COALESCE(last_error_code, ''), attempt_errors, index_retry_deadline_ms,
    index_diagnostic_json";

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
        index_retry_deadline_ms: row.get(19)?,
        index_diagnostic_json: row.get(20)?,
    })
}

const REQUEST_COLS: &str = "request_id, file_id, source_size, source_mtime, component,
    pipeline_version, video_identity, requested_generation, expected_predecessor_generation,
    priority, trigger, force_rebuild,
    target_node_id, state, COALESCE(owner_node_id, ''), fence,
    COALESCE(lease_expires_ms, 0), attempts, not_before_ms,
    COALESCE(result_cache_key, ''), COALESCE(last_error_code, ''),
    cancel_requested, created_at_ms, updated_at_ms";

const HISTORY_COLS: &str = "row_key, request_id, job_id, file_id, item_id, title,
    component, force_rebuild, target_node_id, request_state, job_state, state, disposition,
    action, owner_node_id, claim_epoch, lease_expires_ms, attempts, not_before_ms, request_error_code,
    job_error_code, created_at_ms, updated_at_ms, pipeline_version, requested_generation,
    priority, trigger, cancel_requested, phase, source_size, job_attempt_errors,
    index_retry_deadline_ms, index_diagnostic_json";

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
    COALESCE(page.job_attempt_errors, '') AS job_attempt_errors,
    COALESCE(page.index_retry_deadline_ms, 0) AS index_retry_deadline_ms,
    COALESCE(page.index_diagnostic_json, '') AS index_diagnostic_json";

fn request_from_row(row: &Row<'_>) -> rusqlite::Result<AnalysisRequest> {
    Ok(AnalysisRequest {
        request_id: row.get(0)?,
        file_id: row.get(1)?,
        source_size: row.get(2)?,
        source_mtime: row.get(3)?,
        component: row.get(4)?,
        pipeline_version: row.get(5)?,
        video_identity: row.get(6)?,
        requested_generation: row.get(7)?,
        expected_predecessor_generation: row.get(8)?,
        priority: row.get(9)?,
        trigger: row.get(10)?,
        force_rebuild: row.get::<_, i64>(11)? != 0,
        target_node_id: row.get(12)?,
        state: row.get(13)?,
        owner_node_id: row.get(14)?,
        fence: row.get(15)?,
        lease_expires_ms: row.get(16)?,
        attempts: row.get(17)?,
        not_before_ms: row.get(18)?,
        result_cache_key: row.get(19)?,
        last_error_code: row.get(20)?,
        cancel_requested: row.get::<_, i64>(21)? != 0,
        created_at_ms: row.get(22)?,
        updated_at_ms: row.get(23)?,
    })
}

fn subtitle_source_publication_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<SubtitleSourcePublication> {
    Ok(SubtitleSourcePublication {
        file_id: row.get(0)?,
        source_size: row.get(1)?,
        source_mtime: row.get(2)?,
        source_attestation: row.get(3)?,
        node_id: row.get(4)?,
        ordinal: row.get(5)?,
        kind: row.get(6)?,
        format: row.get(7)?,
        verdict: row.get(8)?,
        attempts: row.get(9)?,
        origin: row.get(10)?,
        sha256: row.get(11)?,
        bytes: row.get(12)?,
        published_at_ms: row.get(13)?,
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
        index_retry_deadline_ms: row.get(31)?,
        index_diagnostic_json: row.get(32)?,
    };
    let cursor = AnalysisHistoryCursor {
        sort_rank: row.get(33)?,
        updated_at_ms: history.updated_at_ms,
        row_key: history.row_key.clone(),
    };
    Ok((history, cursor, row.get(34)?))
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

fn valid_request(request: &NewAnalysisRequest) -> bool {
    !request.request_id.is_empty()
        && request.request_id.len() <= 64
        && request.file_id > 0
        && request.source_size >= 0
        && matches!(
            request.component.as_str(),
            "fragment_index" | "skip_markers" | "subtitle_source"
        )
        && !request.pipeline_version.is_empty()
        && request.pipeline_version.len() <= 128
        && !request.requested_generation.is_empty()
        && request.requested_generation.len() <= 128
        && matches!(
            request.priority.as_str(),
            "normal" | "forced" | "foreground"
        )
        && matches!(
            request.trigger.as_str(),
            "admin" | "background" | "playback"
        )
        && (request.force_rebuild == (request.priority == "forced"))
        && !(request.component == "subtitle_source" && request.force_rebuild)
        && (request.priority != "foreground"
            || matches!(
                request.component.as_str(),
                "subtitle_source" | "fragment_index"
            ))
        && (request.trigger != "playback"
            || matches!(
                request.component.as_str(),
                "subtitle_source" | "fragment_index"
            ))
        && request.target_node_id.len() <= 128
        && ((matches!(
            request.component.as_str(),
            "skip_markers" | "subtitle_source"
        ) && request.target_node_id.is_empty())
            || (request.component == "fragment_index" && !request.target_node_id.is_empty()))
}

#[async_trait]
impl ClusterFragmentIndexStore for SqliteStore {
    async fn subtitle_backfill_diagnostics(
        &self,
        now_ms: i64,
    ) -> Result<SubtitleBackfillDiagnostics, StoreError> {
        self.with_read(move |conn| {
            conn.query_row(SUBTITLE_BACKFILL_DIAGNOSTICS, params![now_ms], |row| {
                Ok(SubtitleBackfillDiagnostics {
                    lease_holder: row.get(0)?,
                    remaining_files: row.get(1)?,
                    remaining_bytes: row.get(2)?,
                })
            })
            .map_err(Into::into)
        })
        .await
    }

    async fn subtitle_backfill_candidates(
        &self,
        after_scanned_at: i64,
        after_id: i64,
        now_ms: i64,
        limit: i64,
    ) -> Result<Vec<SubtitleBackfillCandidate>, StoreError> {
        self.with_read(move |conn| {
            let mut stmt = conn.prepare(SUBTITLE_BACKFILL_CANDIDATES)?;
            let rows = stmt
                .query_map(
                    params![
                        after_scanned_at,
                        after_id,
                        now_ms.saturating_sub(SUBTITLE_BACKFILL_TERMINAL_SETTLE_MS),
                        limit.clamp(1, 128)
                    ],
                    |row| {
                        Ok(SubtitleBackfillCandidate {
                            file_id: row.get(0)?,
                            source_size: row.get(1)?,
                            source_mtime: row.get(2)?,
                            scanned_at: row.get(3)?,
                            uncovered_ordinals: row.get(4)?,
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    async fn enqueue_or_promote_subtitle_source(
        &self,
        stamp: &SubtitleSourceStamp,
        priority: &str,
        now_ms: i64,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        if stamp.file_id <= 0
            || stamp.source_size < 0
            || stamp.pipeline_version.is_empty()
            || stamp.pipeline_version.len() > 128
            || !matches!(priority, "normal" | "foreground")
        {
            return Err(StoreError::Task(
                "invalid subtitle source request".to_owned(),
            ));
        }
        // A completed pass may have published only some ordinals, or a
        // transient representation may still be below its settled-attempt
        // threshold. Retire the ready tombstone before computing the next
        // deterministic generation. This is also the artifact-loss path.
        self.retire_subtitle_source_ready(stamp, now_ms).await?;
        let stamp = stamp.clone();
        let priority = priority.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let existing = tx.query_row(
                &format!("SELECT {REQUEST_COLS} FROM analysis_requests WHERE file_id = ?1 AND source_size = ?2 AND source_mtime = ?3 AND component = 'subtitle_source' AND pipeline_version = ?4 AND state IN ('queued','running','submitted') ORDER BY created_at_ms DESC LIMIT 1"),
                params![stamp.file_id, stamp.source_size, stamp.source_mtime, stamp.pipeline_version], request_from_row,
            ).optional()?;
            if let Some(mut request) = existing {
                tx.execute("UPDATE background_predictions SET state = 'adopted', updated_at_ms = ?1 WHERE request_id = ?2 AND state = 'pending'", params![now_ms, request.request_id])?;
                if priority == "foreground" && request.state == "queued" && request.priority == "normal" {
                    tx.execute("UPDATE analysis_requests SET priority = 'foreground', trigger = 'playback', updated_at_ms = ?1 WHERE request_id = ?2 AND state = 'queued'", params![now_ms, request.request_id])?;
                    request.priority = "foreground".to_owned();
                    request.trigger = "playback".to_owned();
                    request.updated_at_ms = now_ms;
                }
                tx.commit()?;
                return Ok(Some(request));
            }
            let (repair_epoch, recent_repairs): (i64, i64) = tx.query_row(
                "SELECT next_epoch, COALESCE(last_1_ms >= ?5, 0) + COALESCE(last_2_ms >= ?5, 0) + COALESCE(last_3_ms >= ?5, 0) FROM subtitle_source_repair_epochs WHERE file_id = ?1 AND source_size = ?2 AND source_mtime = ?3 AND pipeline_version = ?4",
                params![stamp.file_id, stamp.source_size, stamp.source_mtime, stamp.pipeline_version, now_ms.saturating_sub(SUBTITLE_SOURCE_REPAIR_WINDOW_MS)], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?.unwrap_or((0, 0));
            if recent_repairs >= SUBTITLE_SOURCE_REPAIR_LIMIT {
                tx.commit()?;
                return Ok(None);
            }
            // The counter outlives bounded terminal request history, while
            // the three timestamps enforce a sliding daily allowance.
            let generation = stamp.generation(repair_epoch);
            let terminal = tx.query_row(
                &format!("SELECT {REQUEST_COLS} FROM analysis_requests WHERE request_id = ?1"),
                params![generation], request_from_row,
            ).optional()?;
            if let Some(request) = terminal {
                tx.commit()?;
                return Ok(Some(request));
            }
            tx.execute(
                "INSERT INTO analysis_requests (request_id,file_id,source_size,source_mtime,component,pipeline_version,video_identity,requested_generation,expected_predecessor_generation,priority,trigger,force_rebuild,target_node_id,state,owner_node_id,fence,lease_expires_ms,attempts,not_before_ms,result_cache_key,last_error_code,cancel_requested,created_at_ms,updated_at_ms) SELECT ?1,?2,?3,?4,'subtitle_source',?5,'',?1,'',?6,?7,0,'','queued',NULL,0,NULL,0,?8,NULL,NULL,0,?8,?8 WHERE EXISTS (SELECT 1 FROM files WHERE id = ?2 AND size = ?3 AND mtime = ?4) AND (SELECT COUNT(*) FROM analysis_requests WHERE state IN ('queued','running','submitted')) < ?9 + CASE WHEN ?6 = 'foreground' THEN ?10 ELSE 0 END",
                params![generation, stamp.file_id, stamp.source_size, stamp.source_mtime, stamp.pipeline_version, priority, if priority == "foreground" { "playback" } else { "background" }, now_ms, MAX_ANALYSIS_REQUESTS, super::super::fragment_index_cluster::PLAYBACK_ANALYSIS_HEADROOM],
            )?;
            let result = tx.query_row(&format!("SELECT {REQUEST_COLS} FROM analysis_requests WHERE request_id = ?1"), params![generation], request_from_row).optional()?;
            tx.commit()?;
            result.map(Some).ok_or_else(|| StoreError::Task("subtitle source changed or the request queue is full".to_owned()))
        }).await
    }

    async fn retire_subtitle_source_ready(
        &self,
        stamp: &SubtitleSourceStamp,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let stamp = stamp.clone();
        self.with_conn(move |conn| {
            let sql = format!("UPDATE analysis_requests SET state = 'cancelled', last_error_code = CASE WHEN EXISTS (SELECT 1 FROM subtitle_source_publications p WHERE p.file_id = analysis_requests.file_id AND p.source_size = analysis_requests.source_size AND p.source_mtime = analysis_requests.source_mtime AND p.origin = 'extracted') THEN 'coverage_incomplete' ELSE 'artifact_lost' END, owner_node_id = NULL, lease_expires_ms = NULL, updated_at_ms = ?1 WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4 AND component = 'subtitle_source' AND pipeline_version = ?5 AND state = 'ready' AND (NOT EXISTS (SELECT 1 FROM subtitle_source_publications p WHERE p.file_id = analysis_requests.file_id AND p.source_size = analysis_requests.source_size AND p.source_mtime = analysis_requests.source_mtime AND p.origin = 'extracted') OR {SUBTITLE_SOURCE_READY_UNCOVERED})");
            let changed = conn.execute(&sql, params![now_ms, stamp.file_id, stamp.source_size, stamp.source_mtime, stamp.pipeline_version])?;
            Ok(changed > 0)
        }).await
    }

    async fn retire_subtitle_source_ready_for_ordinal(
        &self,
        stamp: &SubtitleSourceStamp,
        ordinal: i64,
        source_attestation: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if ordinal < 0
            || source_attestation.len() != 64
            || !source_attestation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(StoreError::Task(
                "invalid subtitle source observation".to_owned(),
            ));
        }
        let stamp = stamp.clone();
        let source_attestation = source_attestation.to_owned();
        self.with_conn(move |conn| {
            let changed = conn.execute("UPDATE analysis_requests SET state = 'cancelled', last_error_code = CASE WHEN EXISTS (SELECT 1 FROM subtitle_source_publications p WHERE p.file_id = analysis_requests.file_id AND p.source_size = analysis_requests.source_size AND p.source_mtime = analysis_requests.source_mtime AND p.source_attestation = ?7 AND p.origin = 'extracted') THEN 'coverage_incomplete' ELSE 'artifact_lost' END, owner_node_id = NULL, lease_expires_ms = NULL, updated_at_ms = ?1 WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4 AND component = 'subtitle_source' AND pipeline_version = ?5 AND state = 'ready' AND NOT EXISTS (SELECT 1 FROM subtitle_source_publications p WHERE p.file_id = analysis_requests.file_id AND p.source_size = analysis_requests.source_size AND p.source_mtime = analysis_requests.source_mtime AND p.ordinal = ?6 AND p.source_attestation = ?7 AND p.origin = 'extracted' AND (p.verdict IN ('kept','empty','malformed') OR (p.verdict = 'transient' AND p.attempts >= 3)))", params![now_ms, stamp.file_id, stamp.source_size, stamp.source_mtime, stamp.pipeline_version, ordinal, source_attestation])?;
            Ok(changed > 0)
        }).await
    }

    async fn upsert_subtitle_source_publication(
        &self,
        publication: &SubtitleSourcePublication,
    ) -> Result<(), StoreError> {
        if !valid_subtitle_source_publication(publication) {
            return Err(StoreError::Task(
                "invalid subtitle source publication".to_owned(),
            ));
        }
        let p = publication.clone();
        self.with_conn(move |conn| {
            conn.execute("INSERT INTO subtitle_source_publications(file_id,source_size,source_mtime,source_attestation,node_id,ordinal,kind,format,verdict,attempts,origin,sha256,bytes,published_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(file_id,source_size,source_mtime,node_id,ordinal,format) DO UPDATE SET source_attestation=excluded.source_attestation,kind=excluded.kind,verdict=excluded.verdict,attempts=excluded.attempts,origin=excluded.origin,sha256=excluded.sha256,bytes=excluded.bytes,published_at_ms=excluded.published_at_ms", params![p.file_id,p.source_size,p.source_mtime,p.source_attestation,p.node_id,p.ordinal,p.kind,p.format,p.verdict,p.attempts,p.origin,p.sha256,p.bytes,p.published_at_ms])?;
            Ok(())
        }).await
    }

    async fn list_subtitle_source_publications(
        &self,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
    ) -> Result<Vec<SubtitleSourcePublication>, StoreError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare("SELECT file_id,source_size,source_mtime,source_attestation,node_id,ordinal,kind,format,verdict,attempts,origin,sha256,bytes,published_at_ms FROM subtitle_source_publications WHERE file_id=?1 AND source_size=?2 AND source_mtime=?3 ORDER BY ordinal,format,node_id LIMIT 2048")?;
            let rows = stmt.query_map(params![file_id,source_size,source_mtime], subtitle_source_publication_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into);
            rows
        }).await
    }

    async fn delete_subtitle_source_publications(
        &self,
        file_id: i64,
        node_id: &str,
    ) -> Result<(), StoreError> {
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM subtitle_source_publications WHERE file_id=?1 AND node_id=?2",
                params![file_id, node_id],
            )?;
            Ok(())
        })
        .await
    }

    async fn delete_subtitle_source_publication(
        &self,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
        node_id: &str,
        ordinal: i64,
        format: &str,
    ) -> Result<(), StoreError> {
        let node_id = node_id.to_owned();
        let format = format.to_owned();
        self.with_conn(move |conn| {
            conn.execute("DELETE FROM subtitle_source_publications WHERE file_id=?1 AND source_size=?2 AND source_mtime=?3 AND node_id=?4 AND ordinal=?5 AND format=?6", params![file_id,source_size,source_mtime,node_id,ordinal,format])?;
            Ok(())
        }).await
    }
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
                     pipeline_version, video_identity, requested_generation,
                     expected_predecessor_generation,
                     priority, trigger,
                     force_rebuild, target_node_id, state, owner_node_id, fence,
                     lease_expires_ms, attempts, not_before_ms, result_cache_key,
                     last_error_code, cancel_requested, created_at_ms, updated_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                        CASE WHEN ?5 = 'skip_markers' THEN COALESCE((
                          SELECT generation_id FROM timeline_annotation_sets
                           WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                        ), '') ELSE '' END,
                        ?9, ?10, ?11, ?12,
                        CASE WHEN ?11 = 0 AND ?5 = 'skip_markers' AND EXISTS (
                          SELECT 1 FROM timeline_annotation_sets
                           WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                             AND argv_fingerprint = ?13 AND publication_priority = 'forced'
                        ) THEN 'ready' ELSE 'queued' END,
                        NULL, 0, NULL, 0, ?14,
                        CASE WHEN ?11 = 0 AND ?5 = 'skip_markers' THEN (
                          SELECT generation_id FROM timeline_annotation_sets
                           WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                             AND argv_fingerprint = ?13 AND publication_priority = 'forced'
                        ) ELSE NULL END,
                        NULL, 0, ?15, ?15
                  WHERE EXISTS (SELECT 1 FROM files
                                 WHERE id = ?2 AND size = ?3 AND mtime = ?4)
                    AND (?8 NOT LIKE 'predict:%' OR EXISTS (
                    SELECT 1 FROM background_predictions prediction WHERE prediction.request_id = ?1
                      AND prediction.state = 'pending' AND prediction.expires_ms > ?15
                      AND prediction.file_id = ?2
                      AND json_extract(prediction.request_json,'$.source_size') = ?3
                      AND json_extract(prediction.request_json,'$.source_mtime') = ?4
                      AND json_extract(prediction.request_json,'$.component') = ?5
                      AND json_extract(prediction.request_json,'$.pipeline_version') = ?6
                      AND json_extract(prediction.request_json,'$.target_node_id') = ?12))
                    AND (SELECT COUNT(*) FROM analysis_requests
                          WHERE state IN ('queued', 'running', 'submitted'))
                        < ?16 + CASE WHEN ?9 = 'foreground' AND ?10 = 'playback'
                                     THEN ?17 ELSE 0 END
                    AND (?11 = 0 OR ?5 <> 'fragment_index' OR NOT EXISTS (
                      SELECT 1 FROM analysis_requests legacy
                       WHERE legacy.file_id = ?2 AND legacy.source_size = ?3
                         AND legacy.source_mtime = ?4 AND legacy.component = ?5
                         AND legacy.video_identity = ''
                         -- A force that names an identity is still blocked by
                         -- any active blank row. A force that names none is
                         -- what the admin button sends, and it displaces an
                         -- ordinary blank request -- but not another blank
                         -- force, or two clicks would each buy a whole-file
                         -- index build and neither would cancel the other.
                         AND (?7 <> '' OR legacy.force_rebuild = 1)
                         AND legacy.state IN ('queued','running','submitted','ready')))
                    AND (?11 = 1 OR (
                      NOT EXISTS (SELECT 1 FROM analysis_requests
                        WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                          AND component = ?5 AND force_rebuild = 1
                          AND ((?5 = 'skip_markers' AND target_node_id = ?12)
                            OR (?5 = 'fragment_index' AND pipeline_version = ?6
                              AND video_identity = ?7))
                          AND state IN ('queued', 'running', 'submitted', 'ready'))
                      AND NOT EXISTS (SELECT 1 FROM analysis_requests
                        WHERE file_id = ?2 AND source_size = ?3 AND source_mtime = ?4
                          AND component = ?5 AND pipeline_version = ?6
                          AND video_identity = ?7
                          AND requested_generation = ?8 AND target_node_id = ?12)
                    ))
                 ON CONFLICT DO NOTHING",
                params![
                    request.request_id,
                    request.file_id,
                    request.source_size,
                    request.source_mtime,
                    request.component,
                    request.pipeline_version,
                    request.video_identity,
                    request.requested_generation,
                    request.priority,
                    request.trigger,
                    if request.force_rebuild { 1_i64 } else { 0_i64 },
                    request.target_node_id,
                    semantic_fingerprint,
                    request.not_before_ms,
                    request.created_at_ms,
                    MAX_ANALYSIS_REQUESTS,
                    super::super::fragment_index_cluster::PLAYBACK_ANALYSIS_HEADROOM,
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
                           -- A force is for ONE copy-video identity. Cancelling
                           -- its siblings would strand them: cancelled is
                           -- terminal, so their generations would be refused
                           -- for good and the file would never get its other
                           -- indexes. An empty identity on either side means
                           -- whichever identity is next, which is what every
                           -- pre-column row and every skip_markers row means,
                           -- so it still supersedes the way it always did.
                           AND (older.video_identity = ?8 OR older.video_identity = ''
                                OR ?8 = '')
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
                        request.video_identity,
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
                        AND (video_identity = ?8 OR video_identity = '' OR ?8 = '')
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
                        request.video_identity,
                    ],
                )?;
                transaction.execute(
                    "UPDATE cluster_fragment_index_jobs
                        SET state = 'cancelled', owner_node_id = NULL,
                            lease_expires_ms = NULL, fence = fence + 1,
                            last_error_code = 'publication_superseded', updated_at_ms = ?1
                      WHERE NOT EXISTS (SELECT 1 FROM background_fragment_targets shared
                        WHERE shared.cache_key = cluster_fragment_index_jobs.cache_key
                          AND shared.target_node_id = cluster_fragment_index_jobs.target_node_id)
                      AND state IN ('queued','running')
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
                            AND video_identity = ?8
                            AND ((pipeline_version = ?5 AND requested_generation = ?6
                                  AND target_node_id = ?7)
                              OR (force_rebuild = 1 AND pipeline_version = ?5
                                AND target_node_id = ?7
                                AND (state IN ('queued', 'running', 'submitted')
                                  OR (?10 = 0 AND state = 'ready'))))
                            AND (request_id = ?9
                              OR state IN ('queued', 'running', 'submitted')
                              OR (?10 = 0 AND requested_generation = ?6)
                              OR (?10 = 0 AND force_rebuild = 1 AND state = 'ready'
                                AND pipeline_version = ?5 AND target_node_id = ?7))
                          ORDER BY CASE WHEN request_id = ?9 THEN 0
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
                        request.video_identity,
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

    async fn claim_analysis_request_for_capacity(
        &self,
        node_id: &str,
        pipeline_version: Option<&str>,
        now_ms: i64,
        lease_expires_ms: i64,
        viewer_only: bool,
        targeted_only: bool,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        if node_id.is_empty()
            || node_id.len() > 128
            || pipeline_version.is_some_and(str::is_empty)
            || lease_expires_ms <= now_ms
        {
            return Err(StoreError::Task("invalid analysis claim".to_owned()));
        }
        let node_id = node_id.to_owned();
        let pipeline_version = pipeline_version.map(str::to_owned);
        self.with_conn(move |conn| {
            let capacity = super::super::fragment_index_cluster::analysis_source_capacity_clause("?3");
            let viewer = if viewer_only {
                super::super::fragment_index_cluster::analysis_live_viewer_clause("?3")
            } else { "1".to_owned() };
            let untargeted = if targeted_only {
                "0"
            } else {
                "(component IN ('skip_markers','subtitle_source') AND target_node_id = '')"
            };
            let max_attempts = configured_max_attempts(conn)?;
            let backoff_base_ms = configured_backoff_base_ms(conn)?;
            let backoff_max_ms = configured_backoff_max_ms(conn)?.max(backoff_base_ms);
            let transaction = conn.unchecked_transaction()?;
            // Cluster-wide sweeps; a learner's targeted-only claim leaves
            // them to the voters' claims.
            if !targeted_only {
                transaction.execute(
                    "UPDATE analysis_attempts
                        SET phase = 'retry_wait', phase_updated_at_ms = ?1,
                            terminal_code = 'lease_expired'
                      WHERE (request_id, claim_epoch) IN (
                        SELECT request_id, fence FROM analysis_requests
                         WHERE component <> 'subtitle_source' AND state = 'running' AND COALESCE(lease_expires_ms, 0) <= ?1)",
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
                      WHERE component <> 'subtitle_source' AND state = 'running' AND COALESCE(lease_expires_ms, 0) <= ?1",
                    params![now_ms, backoff_base_ms, backoff_max_ms],
                )?;
                transaction.execute(
                    "UPDATE analysis_attempts
                        SET phase = 'failed', phase_updated_at_ms = ?1,
                            terminal_code = 'attempt_limit'
                      WHERE (request_id, claim_epoch) IN (
                        SELECT request_id, fence FROM analysis_requests
                         WHERE component <> 'subtitle_source' AND attempts >= ?2 AND state = 'queued' AND not_before_ms <= ?1)",
                    params![now_ms, max_attempts],
                )?;
                transaction.execute(
                    "UPDATE analysis_requests
                        SET state = 'failed', owner_node_id = NULL, lease_expires_ms = NULL,
                            last_error_code = 'attempt_limit', updated_at_ms = ?1
                      WHERE component <> 'subtitle_source' AND attempts >= ?2 AND state = 'queued' AND not_before_ms <= ?1",
                    params![now_ms, max_attempts],
                )?;
            }
            let candidate = transaction
                .query_row(
                    &format!(
                        "SELECT {REQUEST_COLS} FROM analysis_requests
                          WHERE (target_node_id = ?1 OR {untargeted})
                            AND component <> 'subtitle_source' AND attempts < ?2
                            AND state = 'queued' AND not_before_ms <= ?3
                            AND (?5 IS NULL OR component <> 'fragment_index' OR pipeline_version = ?5)
                            AND {capacity} AND {viewer}
                          ORDER BY CASE WHEN (component != 'fragment_index' AND priority = 'foreground')
                            OR (component = 'fragment_index' AND EXISTS (
                                SELECT 1 FROM background_job_waiters waiter
                                WHERE waiter.request_scope = 'playback-analysis'
                                  AND waiter.job_id = analysis_requests.request_id
                                  AND waiter.state = 'pending' AND waiter.deadline_ms > ?3))
                            THEN 0 ELSE 1 END,
                                   created_at_ms - CASE WHEN priority = 'forced'
                                     THEN ?4 ELSE 0 END,
                                   created_at_ms, request_id LIMIT 1"
                    ),
                    params![
                        node_id,
                        max_attempts,
                        now_ms,
                        super::super::fragment_index_cluster::ANALYSIS_FORCED_PRIORITY_BOOST_MS,
                        pipeline_version.as_deref(),
                    ],
                    request_from_row,
                )
                .optional()?;
            let Some(mut candidate) = candidate else {
                transaction.commit()?;
                return Ok(None);
            };
            let changed = transaction.execute(
                &format!("UPDATE analysis_requests
                    SET state = 'running', owner_node_id = ?1, fence = fence + 1,
                        lease_expires_ms = ?2, attempts = attempts + 1,
                        last_error_code = NULL, updated_at_ms = ?3
                  WHERE request_id = ?4 AND fence = ?5
                    AND state = 'queued' AND not_before_ms <= ?3
                    AND (?6 IS NULL OR component <> 'fragment_index' OR pipeline_version = ?6)
                    AND {capacity} AND {viewer}"),
                params![
                    node_id,
                    lease_expires_ms,
                    now_ms,
                    candidate.request_id,
                    candidate.fence,
                    pipeline_version.as_deref(),
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

    async fn retire_analysis_requests_for_node(
        &self,
        node_id: &str,
        now_ms: i64,
    ) -> Result<u64, StoreError> {
        if node_id.is_empty() {
            return Err(StoreError::Task("invalid removed node".to_owned()));
        }
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute(
                "UPDATE analysis_attempts
                    SET phase = 'canceled', phase_updated_at_ms = ?1,
                        terminal_code = 'node_removed'
                  WHERE (request_id, claim_epoch) IN (
                    SELECT request_id, fence FROM analysis_requests
                     WHERE target_node_id = ?2
                       AND state IN ('queued', 'running', 'submitted'))",
                params![now_ms, node_id],
            )?;
            let retired = transaction.execute(
                "UPDATE analysis_requests
                    SET state = 'cancelled', owner_node_id = NULL, lease_expires_ms = NULL,
                        fence = fence + 1, cancel_requested = 1,
                        last_error_code = 'node_removed', updated_at_ms = ?1
                  WHERE target_node_id = ?2
                    AND state IN ('queued', 'running', 'submitted')",
                params![now_ms, node_id],
            )?;
            transaction.commit()?;
            Ok(u64::try_from(retired).unwrap_or(u64::MAX))
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
                  WHERE component <> 'subtitle_source' AND request_id = ?3 AND state = 'running' AND owner_node_id = ?4
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
        use crate::store::background_jobs::{BackgroundJobStore, EnqueueOutcome};
        let result = self
            .enqueue_fragment_job(
                crate::store::background_jobs_fragment_admission::EnqueueFragmentJob {
                    job: job.clone(),
                    analysis_request: Some(request.clone()),
                    repair: false,
                    now_ms,
                },
            )
            .await?;
        Ok(matches!(
            result,
            EnqueueOutcome::Accepted { .. }
                | EnqueueOutcome::Existing {
                    cancelled: false,
                    ..
                }
        ))
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
                  WHERE component <> 'subtitle_source' AND request_id = ?5 AND state = 'running' AND owner_node_id = ?6
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
                  WHERE component <> 'subtitle_source' AND request_id = ?3 AND state = 'running' AND owner_node_id = ?4
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

    async fn analysis_reconciliation_page(
        &self,
        after: &str,
        limit: i64,
    ) -> Result<Vec<AnalysisRequest>, StoreError> {
        let after = after.to_owned();
        let limit = limit.clamp(1, 100);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {REQUEST_COLS} FROM analysis_requests
                 WHERE state IN ('queued','running','submitted') AND request_id > ?1
                 ORDER BY request_id LIMIT ?2"
            ))?;
            let rows = statement.query_map(params![after, limit], request_from_row)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn analysis_reconciliation_forced_slot(
        &self,
        replacement: &NewAnalysisRequest,
    ) -> Result<Option<AnalysisRequest>, StoreError> {
        let r = replacement.clone();
        self.with_conn(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {REQUEST_COLS} FROM analysis_requests
                WHERE file_id=?1 AND source_size=?2 AND source_mtime=?3
                  AND component='fragment_index' AND pipeline_version=?4 AND video_identity=?5
                  AND force_rebuild=1 AND state IN ('queued','running','submitted') LIMIT 1"
                ),
                params![
                    r.file_id,
                    r.source_size,
                    r.source_mtime,
                    r.pipeline_version,
                    r.video_identity
                ],
                request_from_row,
            )
            .optional()
            .map_err(Into::into)
        })
        .await
    }

    async fn reconcile_analysis_request(
        &self,
        expected: &AnalysisRequest,
        replacement: &NewAnalysisRequest,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        use super::super::fragment_index_cluster::{
            reconciliation_parameters, ANALYSIS_RECONCILE_SQL,
        };
        let (old, new) = reconciliation_parameters(expected, replacement)?;
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let mut changed = 0;
            for sql in ANALYSIS_RECONCILE_SQL.split("\n-- next\n") {
                changed = transaction.execute(sql, params![old, new, now_ms])?;
            }
            transaction.commit()?;
            Ok(changed == 1)
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
                // Numbered to match the replicated twin, which must bind
                // `$1` before `$2` or `validate_parameter_order` refuses it.
                "SELECT {REQUEST_COLS} FROM analysis_requests terminal
                  WHERE terminal.state IN ('failed', 'cancelled')
                    AND terminal.force_rebuild = 0
                    AND (?1 IS NULL OR terminal.component = ?1)
                    AND NOT EXISTS (
                      SELECT 1 FROM analysis_requests successor
                       WHERE successor.file_id = terminal.file_id
                         AND successor.source_size = terminal.source_size
                         AND successor.source_mtime = terminal.source_mtime
                         AND successor.component = terminal.component
                         AND successor.pipeline_version = terminal.pipeline_version
                         AND successor.video_identity = terminal.video_identity
                         AND successor.target_node_id = terminal.target_node_id
                         AND successor.state IN ('queued','running','submitted','ready'))
                  ORDER BY terminal.updated_at_ms, terminal.request_id LIMIT ?2"
            ))?;
            let rows = statement.query_map(params![component, limit], request_from_row)?;
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
                     pipeline_version, video_identity, requested_generation,
                     expected_predecessor_generation, priority, trigger,
                     force_rebuild, target_node_id, state, owner_node_id, fence,
                     lease_expires_ms, attempts, not_before_ms, result_cache_key,
                     last_error_code, cancel_requested, created_at_ms, updated_at_ms)
                 SELECT ?1, original.file_id, original.source_size, original.source_mtime,
                        original.component, original.pipeline_version,
                        original.video_identity, ?1,
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

    async fn preview_analysis_index_repairs(
        &self,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AnalysisIndexRepairCandidate>, StoreError> {
        let after = after.unwrap_or_default().to_owned();
        let limit = limit.clamp(1, super::super::CONTENT_ANALYSIS_REPAIR_MAX_CANDIDATES);
        self.with_read(move |conn| {
            let mut statement = conn.prepare(
                "SELECT job.cache_key, job.target_node_id, job.fence, job.updated_at_ms,
                        job.file_id, job.source_size, job.source_mtime, job.source_sha256,
                        job.pipeline_sha256, COALESCE((SELECT request.pipeline_version
                          FROM analysis_requests request
                         WHERE request.result_cache_key = job.cache_key
                           AND request.target_node_id = job.target_node_id
                         ORDER BY request.updated_at_ms DESC, request.request_id DESC LIMIT 1), ''),
                        COALESCE((SELECT request.video_identity
                          FROM analysis_requests request
                         WHERE request.result_cache_key = job.cache_key
                           AND request.target_node_id = job.target_node_id
                           AND request.video_identity <> ''
                         ORDER BY request.updated_at_ms DESC, request.request_id DESC LIMIT 1), ''),
                        CASE WHEN (CASE WHEN json_valid(job.index_diagnostic_json)
                                        THEN json_extract(job.index_diagnostic_json, '$.code')
                                        ELSE '' END) = 'index_completion_unverified'
                             THEN 'index_completion_unverified'
                             ELSE COALESCE(job.last_error_code, '') END,
                        COALESCE((SELECT repair.successor_request_id
                          FROM analysis_index_repairs repair
                         WHERE repair.repair_revision = 'video-completion-v1'
                           AND repair.file_id = job.file_id
                           AND repair.source_size = job.source_size
                           AND repair.source_mtime = job.source_mtime
                           AND repair.source_sha256 = job.source_sha256
                           AND repair.pipeline_sha256 = job.pipeline_sha256
                           AND repair.target_node_id = job.target_node_id), ''),
                        EXISTS(SELECT 1 FROM cluster_fragment_index_artifacts artifact
                                WHERE artifact.cache_key = job.cache_key)
                          OR EXISTS(SELECT 1 FROM cluster_fragment_index_heads head
                                WHERE head.logical_cache_key = job.cache_key),
                        (SELECT COUNT(DISTINCT request.video_identity)
                           FROM analysis_requests request
                          WHERE request.result_cache_key = job.cache_key
                            AND request.target_node_id = job.target_node_id
                            AND request.video_identity <> '')
                   FROM cluster_fragment_index_jobs job
                  WHERE job.state = 'failed'
                    AND (job.last_error_code IN ('truncated','index_completion_unverified')
                      OR (CASE WHEN json_valid(job.index_diagnostic_json)
                               THEN json_extract(job.index_diagnostic_json, '$.code')
                               ELSE '' END) = 'index_completion_unverified')
                    AND (job.cache_key || '|' || job.target_node_id) > ?1
                  ORDER BY job.cache_key, job.target_node_id LIMIT ?2",
            )?;
            let rows = statement.query_map(params![after, limit], |row| {
                let mut candidate = AnalysisIndexRepairCandidate {
                    candidate_id: String::new(),
                    repair_revision: super::super::CONTENT_ANALYSIS_REPAIR_REVISION.to_owned(),
                    predecessor_cache_key: row.get(0)?,
                    target_node_id: row.get(1)?,
                    predecessor_fence: row.get(2)?,
                    predecessor_updated_at_ms: row.get(3)?,
                    file_id: row.get(4)?,
                    source_size: row.get(5)?,
                    source_mtime: row.get(6)?,
                    source_sha256: row.get(7)?,
                    pipeline_sha256: row.get(8)?,
                    pipeline_version: row.get(9)?,
                    video_identity: row.get(10)?,
                    cause: row.get(11)?,
                    eligibility: String::new(),
                    successor_request_id: row.get(12)?,
                };
                let already_ready = row.get::<_, i64>(13)? != 0;
                let identities: i64 = row.get(14)?;
                candidate.eligibility = if !candidate.successor_request_id.is_empty() {
                    "already_created"
                } else if already_ready {
                    "already_ready"
                } else if candidate.pipeline_version.is_empty()
                    || candidate.video_identity.is_empty()
                    || identities != 1
                {
                    "identity_unresolved"
                } else {
                    "eligible"
                }
                .to_owned();
                candidate.candidate_id = candidate.recompute_id();
                Ok(candidate)
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
        .await
    }

    async fn apply_analysis_index_repair(
        &self,
        candidate: &AnalysisIndexRepairCandidate,
        successor_request_id: &str,
        now_ms: i64,
    ) -> Result<AnalysisIndexRepairResult, StoreError> {
        if candidate.repair_revision != super::super::CONTENT_ANALYSIS_REPAIR_REVISION
            || !candidate.has_valid_id()
            || candidate.eligibility != "eligible"
            || successor_request_id.is_empty()
            || successor_request_id.len() > 128
        {
            return Err(StoreError::Task(
                "invalid content-analysis repair candidate".to_owned(),
            ));
        }
        let candidate = candidate.clone();
        let successor_request_id = successor_request_id.to_owned();
        self.with_conn(move |conn| {
            let transaction = conn.unchecked_transaction()?;
            let prior: Option<String> = transaction
                .query_row(
                    "SELECT successor_request_id FROM analysis_index_repairs
                      WHERE repair_revision = ?1 AND file_id = ?2 AND source_size = ?3
                        AND source_mtime = ?4 AND source_sha256 = ?5
                        AND pipeline_sha256 = ?6 AND target_node_id = ?7",
                    params![
                        candidate.repair_revision,
                        candidate.file_id,
                        candidate.source_size,
                        candidate.source_mtime,
                        candidate.source_sha256,
                        candidate.pipeline_sha256,
                        candidate.target_node_id
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(prior) = prior {
                transaction.commit()?;
                return Ok(AnalysisIndexRepairResult {
                    candidate_id: candidate.candidate_id,
                    status: "already_created".to_owned(),
                    successor_request_id: prior,
                });
            }
            let active_limit = super::super::MAX_ACTIVE_ANALYSIS_REQUESTS
                .saturating_sub(super::super::CONTENT_ANALYSIS_REPAIR_HEADROOM);
            let valid: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_fragment_index_jobs job
                  JOIN files ON files.id = job.file_id
                 WHERE job.cache_key = ?1 AND job.target_node_id = ?2
                   AND job.state = 'failed'
                   AND (job.last_error_code IN ('truncated','index_completion_unverified')
                     OR (CASE WHEN json_valid(job.index_diagnostic_json)
                              THEN json_extract(job.index_diagnostic_json, '$.code')
                              ELSE '' END) = 'index_completion_unverified')
                   AND job.fence = ?3 AND job.updated_at_ms = ?4
                   AND job.file_id = ?5 AND job.source_size = ?6 AND job.source_mtime = ?7
                   AND job.source_sha256 = ?8 AND job.pipeline_sha256 = ?9
                   AND files.size = job.source_size AND files.mtime = job.source_mtime
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_artifacts artifact
                                    WHERE artifact.cache_key = job.cache_key)
                   AND NOT EXISTS (SELECT 1 FROM cluster_fragment_index_heads head
                                    WHERE head.logical_cache_key = job.cache_key)
                   AND NOT EXISTS (SELECT 1 FROM analysis_requests active
                     WHERE active.file_id = job.file_id AND active.source_size = job.source_size
                       AND active.source_mtime = job.source_mtime
                       AND active.component = 'fragment_index'
                       AND ((active.pipeline_version = ?10 AND active.video_identity = ?11)
                         OR active.video_identity = '')
                       AND active.state IN ('queued','running','submitted','ready')))",
                params![
                    candidate.predecessor_cache_key,
                    candidate.target_node_id,
                    candidate.predecessor_fence,
                    candidate.predecessor_updated_at_ms,
                    candidate.file_id,
                    candidate.source_size,
                    candidate.source_mtime,
                    candidate.source_sha256,
                    candidate.pipeline_sha256,
                    candidate.pipeline_version,
                    candidate.video_identity
                ],
                |row| row.get(0),
            )?;
            let active: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM analysis_requests
                  WHERE state IN ('queued','running','submitted')",
                [],
                |row| row.get(0),
            )?;
            if valid == 0 || active >= active_limit {
                transaction.commit()?;
                return Ok(AnalysisIndexRepairResult {
                    candidate_id: candidate.candidate_id,
                    status: if active >= active_limit {
                        "headroom_blocked"
                    } else {
                        "stale"
                    }
                    .to_owned(),
                    successor_request_id: String::new(),
                });
            }
            transaction.execute(
                "INSERT INTO analysis_requests
                    (request_id, file_id, source_size, source_mtime, component,
                     pipeline_version, video_identity, requested_generation,
                     expected_predecessor_generation, priority, trigger, force_rebuild,
                     target_node_id, state, owner_node_id, fence, lease_expires_ms,
                     attempts, not_before_ms, result_cache_key, last_error_code,
                     cancel_requested, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'fragment_index', ?5, ?6, ?1, '',
                         'forced', 'admin', 1, ?7, 'queued', NULL, 0, NULL, 0,
                         ?8, NULL, NULL, 0, ?8, ?8)",
                params![
                    successor_request_id,
                    candidate.file_id,
                    candidate.source_size,
                    candidate.source_mtime,
                    candidate.pipeline_version,
                    candidate.video_identity,
                    candidate.target_node_id,
                    now_ms
                ],
            )?;
            transaction.execute(
                "INSERT INTO analysis_index_repairs
                    (repair_revision, file_id, source_size, source_mtime, source_sha256,
                     pipeline_sha256, target_node_id, video_identity,
                     predecessor_cache_key, predecessor_fence, successor_request_id, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    candidate.repair_revision,
                    candidate.file_id,
                    candidate.source_size,
                    candidate.source_mtime,
                    candidate.source_sha256,
                    candidate.pipeline_sha256,
                    candidate.target_node_id,
                    candidate.video_identity,
                    candidate.predecessor_cache_key,
                    candidate.predecessor_fence,
                    successor_request_id,
                    now_ms
                ],
            )?;
            transaction.commit()?;
            Ok(AnalysisIndexRepairResult {
                candidate_id: candidate.candidate_id,
                status: "created".to_owned(),
                successor_request_id,
            })
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
                  WHERE NOT EXISTS (SELECT 1 FROM background_fragment_targets shared
                        WHERE shared.cache_key = cluster_fragment_index_jobs.cache_key
                          AND shared.target_node_id = cluster_fragment_index_jobs.target_node_id)
                      AND state IN ('queued', 'running')
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
                  WHERE component <> 'subtitle_source' AND request_id = ?3 AND state = 'running' AND owner_node_id = ?4
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
                    observed_at_ms = excluded.observed_at_ms
                 WHERE cluster_fragment_index_sources.object_version !=
                       'hevc-full-v1:' || excluded.object_version",
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

    async fn cluster_fragment_index_availability(
        &self,
        node_id: &str,
        probes: &[crate::store::ClusterIndexAvailabilityProbe],
    ) -> Result<Vec<bool>, StoreError> {
        use crate::store::fragment_index_cluster::{
            check_cluster_index_availability_batch, cluster_index_availability_answers,
            cluster_index_availability_keys,
        };
        check_cluster_index_availability_batch(probes)?;
        if probes.is_empty() {
            return Ok(Vec::new());
        }
        crate::store::record_http_cluster_index_availability_call();
        let node_id = node_id.to_owned();
        let probes = probes.to_vec();
        self.with_read(move |conn| {
            let file_ids: Vec<i64> = probes.iter().map(|probe| probe.file_id).collect();
            let versions: Vec<&str> = probes
                .iter()
                .map(|probe| probe.object_version.as_str())
                .collect();
            let file_ids = serde_json::to_string(&file_ids)
                .map_err(|error| StoreError::Task(error.to_string()))?;
            let versions = serde_json::to_string(&versions)
                .map_err(|error| StoreError::Task(error.to_string()))?;
            let attested = conn
                .prepare(
                    "SELECT node_id, file_id, object_version, source_size, source_mtime,
                            source_sha256, observed_at_ms
                       FROM cluster_fragment_index_sources
                      WHERE node_id = ?1
                        AND file_id IN (SELECT CAST(value AS INTEGER) FROM json_each(?2))
                        AND object_version IN (SELECT value FROM json_each(?3))",
                )?
                .query_map(params![node_id, file_ids, versions], |row| {
                    Ok(FragmentIndexSourceObservation {
                        node_id: row.get(0)?,
                        file_id: row.get(1)?,
                        object_version: row.get(2)?,
                        source_size: row.get(3)?,
                        source_mtime: row.get(4)?,
                        source_sha256: row.get(5)?,
                        observed_at_ms: row.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let keyed = cluster_index_availability_keys(&probes, &attested);
            let keys: Vec<&str> = keyed
                .iter()
                .flatten()
                .map(|(key, _)| key.as_str())
                .collect();
            if keys.is_empty() {
                return Ok(vec![false; probes.len()]);
            }
            let keys = serde_json::to_string(&keys)
                .map_err(|error| StoreError::Task(error.to_string()))?;
            let artifacts = conn
                .prepare(
                    "SELECT k.value, a.cache_key, a.file_id, a.source_size, a.source_mtime,
                            a.source_sha256, a.pipeline_sha256, a.blob_sha256, a.bytes,
                            a.built_by_node_id, a.built_at_ms
                       FROM json_each(?1) k
                       JOIN cluster_fragment_index_artifacts a
                         ON a.cache_key = COALESCE((
                              SELECT h.generation_cache_key
                                FROM cluster_fragment_index_heads h
                               WHERE h.logical_cache_key = k.value
                            ), k.value)",
                )?
                .query_map(params![keys], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        ClusterFragmentIndexArtifact {
                            cache_key: row.get(1)?,
                            file_id: row.get(2)?,
                            source_size: row.get(3)?,
                            source_mtime: row.get(4)?,
                            source_sha256: row.get(5)?,
                            pipeline_sha256: row.get(6)?,
                            blob_sha256: row.get(7)?,
                            bytes: row.get(8)?,
                            built_by_node_id: row.get(9)?,
                            built_at_ms: row.get(10)?,
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(cluster_index_availability_answers(
                &probes, &keyed, &artifacts,
            ))
        })
        .await
    }

    async fn enqueue_cluster_fragment_index(
        &self,
        job: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError> {
        use crate::store::background_jobs::{BackgroundJobStore, EnqueueOutcome};
        let result = self
            .enqueue_fragment_job(
                crate::store::background_jobs_fragment_admission::EnqueueFragmentJob {
                    job: job.clone(),
                    analysis_request: None,
                    repair: false,
                    now_ms: job.created_at_ms,
                },
            )
            .await?;
        match result {
            EnqueueOutcome::Accepted { .. } => Ok(true),
            EnqueueOutcome::Existing {
                job_id,
                cancelled: false,
                ..
            } => Ok(self.background_job(&job_id).await?.is_some_and(|job| {
                matches!(
                    job.state,
                    crate::store::background_jobs::JobState::Queued
                        | crate::store::background_jobs::JobState::Running
                        | crate::store::background_jobs::JobState::Succeeded
                )
            })),
            _ => Ok(false),
        }
    }

    async fn requeue_cluster_fragment_index(
        &self,
        replacement: &NewClusterFragmentIndexJob,
    ) -> Result<bool, StoreError> {
        use crate::store::background_jobs::{BackgroundJobStore, EnqueueOutcome};
        let result = self
            .enqueue_fragment_job(
                crate::store::background_jobs_fragment_admission::EnqueueFragmentJob {
                    job: replacement.clone(),
                    analysis_request: None,
                    repair: true,
                    now_ms: replacement.created_at_ms,
                },
            )
            .await?;
        let job_id = match result {
            EnqueueOutcome::Accepted { job_id, .. }
            | EnqueueOutcome::Existing {
                job_id,
                cancelled: false,
                ..
            } => job_id,
            _ => return Ok(false),
        };
        Ok(self.background_job(&job_id).await?.is_some_and(|job| {
            matches!(
                job.state,
                crate::store::background_jobs::JobState::Queued
                    | crate::store::background_jobs::JobState::Running
            )
        }))
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

    async fn fragment_index_builders_held_by(
        &self,
        node_id: &str,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
    ) -> Result<Vec<String>, StoreError> {
        let node_id = node_id.to_owned();
        self.with_read(move |conn| {
            // The locations primary key and the artifacts' file index make
            // this two keyed reads.
            let builders = conn
                .prepare(
                    "SELECT DISTINCT artifacts.built_by_node_id
                       FROM cluster_fragment_index_locations AS locations
                       JOIN cluster_fragment_index_artifacts AS artifacts
                         ON artifacts.cache_key = locations.cache_key
                      WHERE locations.node_id = ?1
                        AND artifacts.file_id = ?2
                        AND artifacts.source_size = ?3
                        AND artifacts.source_mtime = ?4
                      ORDER BY artifacts.built_by_node_id
                      LIMIT 16",
                )?
                .query_map(
                    params![node_id, file_id, source_size, source_mtime],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(builders)
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
                let mut statement = transaction
                    .prepare(crate::store::fragment_index_cluster::FRAGMENT_PRUNE_CANDIDATES)?;
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
                crate::store::fragment_index_cluster::FRAGMENT_PRUNE_TERMINAL_JOBS,
                params![older_than_ms, limit],
            )?;
            transaction.commit()?;
            Ok(pruned_artifacts)
        })
        .await
    }
}

#[cfg(test)]
mod tests;
