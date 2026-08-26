//! Admin control and diagnostics for the durable content-analysis queue.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;

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
            "analysis queue is paused; enable Cluster index cache in Playback settings".to_owned(),
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

/// GET /api/v1/analysis/jobs — one bounded, admin-only status snapshot.
pub async fn jobs(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now_ms = crate::state::clock_ms();
    let (requests, jobs, files) = tokio::try_join!(
        state.store.analysis_requests(500),
        state.store.cluster_fragment_index_jobs(500),
        state.store.analysis_file_labels(500),
    )?;
    Ok(Json(serde_json::json!({
        "enabled": state.jobs.analysis_queue_enabled().await,
        "node_id": state.node_id,
        "now_ms": now_ms,
        "requests": requests.into_iter().map(|request| serde_json::json!({
            "request_id": request.request_id,
            "file_id": request.file_id.to_string(),
            "component": request.component,
            "force_rebuild": request.force_rebuild,
            "target_node_id": request.target_node_id,
            "state": request.state,
            "owner_node_id": request.owner_node_id,
            "lease_expires_ms": request.lease_expires_ms,
            "attempts": request.attempts,
            "not_before_ms": request.not_before_ms,
            "result_cache_key": request.result_cache_key,
            "last_error_code": request.last_error_code,
            "created_at_ms": request.created_at_ms,
            "updated_at_ms": request.updated_at_ms,
        })).collect::<Vec<_>>(),
        "files": files.into_iter().map(|file| serde_json::json!({
            "file_id": file.file_id.to_string(),
            "item_id": file.item_id.to_string(),
            "title": file.title,
        })).collect::<Vec<_>>(),
        "jobs": jobs.into_iter().map(|job| serde_json::json!({
            "job_id": job.cache_key,
            "file_id": job.file_id.to_string(),
            "component": "fragment_index",
            "state": job.state,
            "owner_node_id": job.owner_node_id,
            "lease_expires_ms": job.lease_expires_ms,
            "attempts": job.attempts,
            "not_before_ms": job.not_before_ms,
            "created_at_ms": job.created_at_ms,
            "updated_at_ms": job.updated_at_ms,
            "last_error_code": job.last_error_code,
            "pipeline_version": job.pipeline_sha256.get(..12).unwrap_or(&job.pipeline_sha256),
            "source_size": job.source_size,
        })).collect::<Vec<_>>(),
    })))
}
