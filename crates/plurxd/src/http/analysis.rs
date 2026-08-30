//! Admin control and diagnostics for the durable content-analysis queue.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;

fn summary_value(
    summary: plurx_core::store::AnalysisStatusSummary,
    enabled: bool,
    now_ms: i64,
) -> serde_json::Value {
    serde_json::json!({
        "available": true,
        "enabled": enabled,
        "scope": "active_and_recent_terminal",
        "terminal_window": 8192,
        "total": summary.total,
        "active": summary.working,
        "working": summary.working,
        "queued": summary.queued,
        "running": summary.running,
        "submitted": summary.submitted,
        "attention": summary.attention,
        "failed": summary.attention,
        "expected": summary.expected,
        "ready": summary.ready,
        "now_ms": now_ms,
        "latest_error": (!summary.latest_error_code.is_empty()).then(|| serde_json::json!({
            "code": summary.latest_error_code,
            "file_id": summary.latest_error_file_id.to_string(),
            "updated_at_ms": summary.latest_error_updated_at_ms,
        })),
    })
}

/// Compact projection for Activity. It deliberately uses the same merge rule
/// as the full status response so a submitted request and its worker do not
/// appear as two pieces of work on one page and one piece on another.
pub(crate) async fn activity_summary(state: &AppState) -> Result<serde_json::Value, ApiError> {
    let now_ms = crate::state::clock_ms();
    let summary = state.store.analysis_status_summary().await?;
    Ok(summary_value(
        summary,
        state.jobs.analysis_queue_enabled().await,
        now_ms,
    ))
}

#[derive(Default, Deserialize)]
pub struct AnalysisRequestBody {
    #[serde(default)]
    force: bool,
    #[serde(default)]
    components: Vec<String>,
}

#[derive(Serialize)]
pub struct AnalysisRequestResponse {
    request_id: String,
    file_id: String,
    state: String,
    force: bool,
    joined: bool,
    jobs: Vec<AnalysisRequestJobResponse>,
}

#[derive(Clone, Serialize)]
pub struct AnalysisRequestJobResponse {
    job_id: String,
    component: String,
    state: String,
    joined: bool,
}

#[derive(Deserialize)]
pub struct ManualAnnotationBody {
    start_ticks: i64,
    end_ticks: i64,
    timescale: u32,
    start_ms: i64,
    end_ms: i64,
}

#[derive(Deserialize)]
pub struct DiscardManualAnnotationBody {
    revision: u64,
    confirm_discard_manual_override: bool,
}

fn annotation_kind(value: &str) -> Result<plurx_core::segplan::AnnotationKind, ApiError> {
    plurx_core::segplan::AnnotationKind::parse(value).ok_or_else(|| {
        ApiError::BadRequest("annotation kind must be intro, recap, credits, or preview".to_owned())
    })
}

/// PUT /api/v1/files/{file}/timeline-annotations/{kind} — create or correct a
/// durable administrator boundary. Automatic rebuilds never write its row.
pub async fn set_manual_annotation(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path((file_id, kind)): Path<(i64, String)>,
    Json(body): Json<ManualAnnotationBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use plurx_core::segplan::{AnnotationProvenance, TimelineAnnotation};

    let kind = annotation_kind(&kind)?;
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let duration_ms = file
        .duration_ms
        .filter(|duration| *duration > 0)
        .ok_or_else(|| ApiError::Conflict("file has no measured duration".to_owned()))?;
    let source = super::stream::annotation_source_identity(&file);
    let generation_id = uuid::Uuid::new_v4().to_string();
    let annotation = TimelineAnnotation {
        kind,
        start_ticks: body.start_ticks,
        end_ticks: body.end_ticks,
        timescale: body.timescale,
        start_ms: body.start_ms,
        end_ms: body.end_ms,
        provenance: AnnotationProvenance::Manual,
        confidence_millis: 1_000,
        detector_version: "manual-v1".to_owned(),
        // The store assigns the real monotonic value after validating the
        // shape, so this is only the required positive validation placeholder.
        manual_override_revision: Some(1),
    };
    let revision = state
        .store
        .set_manual_timeline_annotation(file_id, duration_ms, &source, &annotation, &generation_id)
        .await?;
    tracing::info!(
        file_id,
        kind = kind.as_str(),
        revision,
        "manual timeline annotation stored"
    );
    Ok(Json(serde_json::json!({
        "file_id": file_id.to_string(),
        "kind": kind.as_str(),
        "generation": generation_id,
        "revision": revision,
        "provenance": "manual",
        "confidence": 1000,
        "start_ms": body.start_ms,
        "end_ms": body.end_ms,
    })))
}

/// DELETE /api/v1/files/{file}/timeline-annotations/{kind} — the separate,
/// explicit confirmation required before a manual correction can disappear.
pub async fn discard_manual_annotation(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path((file_id, kind)): Path<(i64, String)>,
    Json(body): Json<DiscardManualAnnotationBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !body.confirm_discard_manual_override {
        return Err(ApiError::BadRequest(
            "discarding a manual override requires explicit confirmation".to_owned(),
        ));
    }
    let kind = annotation_kind(&kind)?;
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let source = super::stream::annotation_source_identity(&file);
    if !state
        .store
        .discard_manual_timeline_annotation(file_id, &source, kind, body.revision)
        .await?
    {
        return Err(ApiError::Conflict(
            "manual override changed or no longer matches this source".to_owned(),
        ));
    }
    tracing::info!(
        file_id,
        kind = kind.as_str(),
        revision = body.revision,
        "manual timeline annotation discarded"
    );
    Ok(Json(serde_json::json!({
        "file_id": file_id.to_string(),
        "kind": kind.as_str(),
        "discarded_revision": body.revision,
    })))
}

/// POST /api/v1/files/{file}/analysis — durably request analysis before any
/// source hashing or ffmpeg work begins.
pub async fn request(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    Json(body): Json<AnalysisRequestBody>,
) -> Result<(StatusCode, Json<AnalysisRequestResponse>), ApiError> {
    let components = if body.components.is_empty() {
        vec!["fragment_index".to_owned()]
    } else {
        body.components
    };
    if components
        .iter()
        .any(|component| !matches!(component.as_str(), "fragment_index" | "skip_markers"))
    {
        return Err(ApiError::BadRequest(
            "analysis component must be fragment_index or skip_markers".to_owned(),
        ));
    }
    let components = components
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if !state.jobs.analysis_queue_enabled().await {
        return Err(ApiError::Conflict(
            "analysis queue is paused; enable it in Analysis settings".to_owned(),
        ));
    }
    if state.store.get_file(file_id).await?.is_none() {
        return Err(ApiError::NotFound("file"));
    }

    let mut requested = Vec::with_capacity(components.len());
    let mut primary = None;
    for component in components {
        let (request, joined) = state
            .jobs
            .request_file_analysis(file_id, body.force, &component)
            .await?;
        if body.force && joined && !request.force_rebuild {
            return Err(ApiError::Conflict(format!(
                "{component} analysis is already in progress; request a rebuild after it finishes"
            )));
        }
        tracing::info!(
            file_id,
            request_id = %request.request_id,
            component,
            force = body.force,
            joined,
            "analysis requested"
        );
        let entry = AnalysisRequestJobResponse {
            job_id: request.request_id.clone(),
            component: request.component.clone(),
            state: request.state.clone(),
            joined,
        };
        primary.get_or_insert((request, joined));
        requested.push(entry);
    }
    let jobs = state.jobs.clone();
    let transcode = state.transcode.clone();
    tokio::spawn(async move {
        jobs.work_cluster_fragment_index_queue(transcode).await;
    });
    let (request, joined) = primary.expect("at least one validated analysis component");
    Ok((
        StatusCode::ACCEPTED,
        Json(AnalysisRequestResponse {
            request_id: request.request_id,
            file_id: request.file_id.to_string(),
            state: request.state,
            force: request.force_rebuild,
            joined,
            jobs: requested,
        }),
    ))
}

#[derive(Default, Deserialize)]
pub struct AnalysisHistoryParams {
    limit: Option<i64>,
    cursor: Option<String>,
    filter: Option<String>,
    q: Option<String>,
    state: Option<String>,
}

fn durable_state(storage_state: &str, not_before_ms: i64, error_code: &str, now_ms: i64) -> String {
    if error_code == "source_superseded" {
        return "stale".to_owned();
    }
    match storage_state {
        "queued" if not_before_ms > now_ms => "retry_wait",
        "running" => "claimed",
        "submitted" => "staged",
        "ready" => "published",
        "cancelled" => "canceled",
        state => state,
    }
    .to_owned()
}

fn decode_cursor(value: &str) -> Result<plurx_core::store::AnalysisHistoryCursor, ApiError> {
    let mut parts = value.splitn(3, '|');
    let sort_rank = parts
        .next()
        .and_then(|part| part.parse::<i64>().ok())
        .filter(|rank| (0..=6).contains(rank));
    let updated_at_ms = parts
        .next()
        .and_then(|part| part.parse::<i64>().ok())
        .filter(|value| *value >= 0);
    let row_key = parts
        .next()
        .filter(|key| key.len() <= 160 && (key.starts_with("request:") || key.starts_with("job:")));
    match (sort_rank, updated_at_ms, row_key) {
        (Some(sort_rank), Some(updated_at_ms), Some(row_key)) => {
            Ok(plurx_core::store::AnalysisHistoryCursor {
                sort_rank,
                updated_at_ms,
                row_key: row_key.to_owned(),
            })
        }
        _ => Err(ApiError::BadRequest("invalid analysis cursor".to_owned())),
    }
}

fn encode_cursor(cursor: plurx_core::store::AnalysisHistoryCursor) -> String {
    format!(
        "{}|{}|{}",
        cursor.sort_rank, cursor.updated_at_ms, cursor.row_key
    )
}

fn history_filter(
    value: Option<&str>,
) -> Result<plurx_core::store::AnalysisHistoryFilter, ApiError> {
    match value.unwrap_or("all") {
        "all" => Ok(plurx_core::store::AnalysisHistoryFilter::All),
        "working" => Ok(plurx_core::store::AnalysisHistoryFilter::Working),
        "attention" => Ok(plurx_core::store::AnalysisHistoryFilter::Attention),
        "ready" => Ok(plurx_core::store::AnalysisHistoryFilter::Ready),
        "expected" => Ok(plurx_core::store::AnalysisHistoryFilter::Expected),
        _ => Err(ApiError::BadRequest("invalid analysis filter".to_owned())),
    }
}

fn history_row_value(row: plurx_core::store::AnalysisHistoryRow) -> serde_json::Value {
    let durable_state = durable_state(
        &row.state,
        row.not_before_ms,
        &row.request_error_code,
        crate::state::clock_ms(),
    );
    serde_json::json!({
        "row_key": row.row_key,
        "request_id": row.request_id,
        "job_id": row.job_id,
        "file_id": row.file_id.to_string(),
        "item_id": (row.item_id > 0).then(|| row.item_id.to_string()),
        "title": row.title,
        "component": row.component,
        "force_rebuild": row.force_rebuild,
        "target_node_id": row.target_node_id,
        "request_state": row.request_state,
        "job_state": row.job_state,
        "state": row.state,
        "disposition": row.disposition,
        "action": row.action,
        "owner_node_id": row.owner_node_id,
        "lease_expires_ms": row.lease_expires_ms,
        "attempts": row.attempts,
        "not_before_ms": row.not_before_ms,
        "request_error_code": row.request_error_code,
        "job_error_code": row.job_error_code,
        "created_at_ms": row.created_at_ms,
        "updated_at_ms": row.updated_at_ms,
        "pipeline_version": row.pipeline_version,
        "source_size": row.source_size,
        "durable_state": durable_state,
        "claim_epoch": row.claim_epoch,
        "phase": match row.state.as_str() {
            "queued" => "probing",
            "running" => "probing",
            "submitted" => "fragment_index",
            "ready" => "verifying",
            _ => "",
        },
    })
}

fn request_value(request: plurx_core::store::AnalysisRequest, now_ms: i64) -> serde_json::Value {
    let durable_state = durable_state(
        &request.state,
        request.not_before_ms,
        &request.last_error_code,
        now_ms,
    );
    serde_json::json!({
        "job_id": request.request_id,
        "file_id": request.file_id.to_string(),
        "component": request.component,
        "target_node_id": request.target_node_id,
        "state": durable_state,
        "storage_state": request.state,
        "attempt": request.attempts,
        "claim_epoch": request.fence,
        "claim_node_id": request.owner_node_id,
        "claim_expires_at_ms": request.lease_expires_ms,
        "not_before_ms": request.not_before_ms,
        "cancel_requested": durable_state == "canceled",
        "force": request.force_rebuild,
        "generation": request.result_cache_key,
        "last_error_code": request.last_error_code,
        "created_at_ms": request.created_at_ms,
        "updated_at_ms": request.updated_at_ms,
    })
}

/// GET /api/v1/analysis/summary — compact, admin-only polling projection.
pub async fn summary(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(activity_summary(&state).await?))
}

/// GET /api/v1/analysis/jobs — stable, server-filtered keyset history page.
pub async fn jobs(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(params): Query<AnalysisHistoryParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now_ms = crate::state::clock_ms();
    let search = params.q.unwrap_or_default();
    if search.chars().count() > 120 {
        return Err(ApiError::BadRequest(
            "analysis search is limited to 120 characters".to_owned(),
        ));
    }
    let states = params
        .state
        .as_deref()
        .map(|states| {
            let requested = states
                .split(',')
                .filter(|value| !value.is_empty())
                .collect::<std::collections::BTreeSet<_>>();
            const ALLOWED: &[&str] = &[
                "queued",
                "claimed",
                "running",
                "staged",
                "published",
                "ready",
                "retry_wait",
                "failed",
                "canceled",
                "cancelled",
                "stale",
            ];
            if requested.is_empty() || requested.iter().any(|value| !ALLOWED.contains(value)) {
                return Err(ApiError::BadRequest(
                    "invalid analysis state filter".to_owned(),
                ));
            }
            Ok(requested
                .into_iter()
                .map(|state| match state {
                    "running" => "claimed",
                    "ready" => "published",
                    "cancelled" => "canceled",
                    state => state,
                })
                .map(str::to_owned)
                .collect())
        })
        .transpose()?
        .unwrap_or_default();
    let query = plurx_core::store::AnalysisHistoryQuery {
        limit: params.limit.unwrap_or(25),
        cursor: params.cursor.as_deref().map(decode_cursor).transpose()?,
        filter: history_filter(params.filter.as_deref())?,
        search,
        states,
        now_ms,
    };
    let page = state.store.analysis_history(&query).await?;
    let enabled = state.jobs.analysis_queue_enabled().await;
    // Every row here is attributed to a node by id, and an id names no
    // machine. `AdminUser` above is the permission this read needs, so it is
    // passed as the caller's own proof rather than re-derived.
    let hostnames = super::system::node_hostnames(&state, true).await;
    Ok(Json(serde_json::json!({
        "enabled": enabled,
        "node_id": state.node_id,
        "node_hostnames": hostnames,
        "now_ms": now_ms,
        "page_size": query.limit.clamp(10, 100),
        "filtered_total": page.filtered_total,
        "next_cursor": page.next_cursor.map(encode_cursor),
        "rows": page.rows.into_iter().map(history_row_value).collect::<Vec<_>>(),
    })))
}

/// GET /api/v1/analysis/jobs/{job} — exact durable job state.
pub async fn job(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let request = state
        .store
        .analysis_request(&job_id)
        .await?
        .ok_or(ApiError::NotFound("analysis job"))?;
    Ok(Json(request_value(request, crate::state::clock_ms())))
}

/// POST /api/v1/analysis/jobs/{job}/retry — explicit operator override for a
/// terminal failure or cancellation.
pub async fn retry_job(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let current = state
        .store
        .analysis_request(&job_id)
        .await?
        .ok_or(ApiError::NotFound("analysis job"))?;
    if !matches!(current.state.as_str(), "failed" | "cancelled") {
        return Err(ApiError::Conflict(
            "only a failed or canceled analysis job can be retried".to_owned(),
        ));
    }
    let now_ms = crate::state::clock_ms();
    let retried = state
        .store
        .retry_analysis_request_admin(&job_id, now_ms)
        .await?
        .ok_or(ApiError::NotFound("analysis job"))?;
    if retried.state != "queued" {
        return Err(ApiError::Conflict(
            "analysis source changed; request a new generation from the file".to_owned(),
        ));
    }
    let jobs = state.jobs.clone();
    let transcode = state.transcode.clone();
    tokio::spawn(async move {
        jobs.work_cluster_fragment_index_queue(transcode).await;
    });
    tracing::info!(job_id, "analysis job explicitly retried");
    Ok(Json(request_value(retried, now_ms)))
}

/// DELETE /api/v1/analysis/jobs/{job} — durable cooperative cancellation.
pub async fn cancel_job(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let current = state
        .store
        .analysis_request(&job_id)
        .await?
        .ok_or(ApiError::NotFound("analysis job"))?;
    if matches!(current.state.as_str(), "ready" | "failed" | "cancelled") {
        return Ok(Json(request_value(current, crate::state::clock_ms())));
    }
    let now_ms = crate::state::clock_ms();
    let canceled = state
        .store
        .cancel_analysis_request_admin(&job_id, now_ms)
        .await?
        .ok_or(ApiError::NotFound("analysis job"))?;
    tracing::info!(job_id, "analysis job cancellation requested");
    Ok(Json(request_value(canceled, now_ms)))
}

#[cfg(test)]
mod tests {
    /// The history page attributes every row to a node by id, so it has to
    /// carry the names for them. Asserted on the source because the handler
    /// needs a replicated `AppState` behind it, and the rendered end of the
    /// contract is `tests/web/analysis-node-names.test.js`.
    ///
    /// Nothing issues a request to this route and reads the response back, so
    /// the field name is agreed between this file and that one by spelling
    /// alone. `crates/plurxd/tests/cluster_activity.rs` proves the *accessor*
    /// against two real daemons, through `/activity/detail`; it says nothing
    /// about this route's wire shape.
    #[test]
    fn the_history_page_is_sent_the_names_for_the_nodes_it_attributes_rows_to() {
        let source = include_str!("analysis.rs");
        let handler = source
            .split_once("pub async fn jobs(")
            .expect("history handler")
            .1
            .split_once("\n#[cfg(test)]")
            .expect("module after jobs")
            .0;
        let read = handler
            .find("super::system::node_hostnames(&state, true)")
            .expect("the roster read");
        let published = handler
            .find("\"node_hostnames\": hostnames")
            .expect("the history page is sent the roster's machine names");
        assert!(read < published);
        // `AdminUser` is this route's permission, and it is extracted before
        // anything else runs. Passing `true` is that proof being handed on,
        // not a gate being skipped — so the extractor must still be there.
        assert!(handler.contains("_admin: AdminUser"));
        // A roster read that fails must cost the page its labels, not the
        // page: the shared reader swallows the error, so no `?` may appear on
        // this call.
        assert!(!handler.contains("node_hostnames(&state, true).await?"));
        // Unconditional, unlike the activity read. Every reader here is
        // already an admin, so presence answers nothing and an absent field
        // would only make the client guess.
        assert!(!handler.contains("if clustered"));
    }
}
