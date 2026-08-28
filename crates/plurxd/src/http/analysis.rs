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
}

/// POST /api/v1/files/{file}/analysis — durably request analysis before any
/// source hashing or ffmpeg work begins.
pub async fn request(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    Json(body): Json<AnalysisRequestBody>,
) -> Result<(StatusCode, Json<AnalysisRequestResponse>), ApiError> {
    if !body.components.is_empty()
        && body
            .components
            .iter()
            .any(|component| component != "fragment_index")
    {
        return Err(ApiError::BadRequest(
            "only fragment_index analysis is available in this milestone".to_owned(),
        ));
    }
    if !state.jobs.analysis_queue_enabled().await {
        return Err(ApiError::Conflict(
            "analysis queue is paused; enable it in Analysis settings".to_owned(),
        ));
    }
    if state.store.get_file(file_id).await?.is_none() {
        return Err(ApiError::NotFound("file"));
    }

    let (request, joined) = state
        .jobs
        .request_file_analysis(file_id, body.force)
        .await?;
    if body.force && joined && !request.force_rebuild {
        return Err(ApiError::Conflict(
            "analysis is already in progress; request a rebuild after it finishes".to_owned(),
        ));
    }
    let jobs = state.jobs.clone();
    let transcode = state.transcode.clone();
    tokio::spawn(async move {
        jobs.work_cluster_fragment_index_queue(transcode).await;
    });
    tracing::info!(
        file_id,
        request_id = %request.request_id,
        force = body.force,
        joined,
        "analysis requested"
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(AnalysisRequestResponse {
            request_id: request.request_id,
            file_id: request.file_id.to_string(),
            state: request.state,
            force: request.force_rebuild,
            joined,
        }),
    ))
}

#[derive(Default, Deserialize)]
pub struct AnalysisHistoryParams {
    limit: Option<i64>,
    cursor: Option<String>,
    filter: Option<String>,
    q: Option<String>,
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
    let query = plurx_core::store::AnalysisHistoryQuery {
        limit: params.limit.unwrap_or(25),
        cursor: params.cursor.as_deref().map(decode_cursor).transpose()?,
        filter: history_filter(params.filter.as_deref())?,
        search,
    };
    let page = state.store.analysis_history(&query).await?;
    let enabled = state.jobs.analysis_queue_enabled().await;
    Ok(Json(serde_json::json!({
        "enabled": enabled,
        "node_id": state.node_id,
        "now_ms": now_ms,
        "page_size": query.limit.clamp(10, 100),
        "filtered_total": page.filtered_total,
        "next_cursor": page.next_cursor.map(encode_cursor),
        "rows": page.rows.into_iter().map(history_row_value).collect::<Vec<_>>(),
    })))
}
