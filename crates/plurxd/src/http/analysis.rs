//! Admin control and diagnostics for the durable content-analysis queue.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;

fn summary_value(
    requests: &[plurx_core::store::AnalysisRequest],
    jobs: &[plurx_core::store::ClusterFragmentIndexJob],
    enabled: bool,
    now_ms: i64,
) -> serde_json::Value {
    let by_key: HashMap<&str, &plurx_core::store::ClusterFragmentIndexJob> = jobs
        .iter()
        .map(|job| (job.cache_key.as_str(), job))
        .collect();
    let mut represented = HashSet::new();
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut latest_error: Option<(String, i64, i64)> = None;
    let mut observe = |state: &str, file_id: i64, error: &str, updated_at_ms: i64| {
        *counts.entry(state.to_owned()).or_default() += 1;
        if !error.is_empty()
            && latest_error
                .as_ref()
                .is_none_or(|(_, _, previous)| updated_at_ms > *previous)
        {
            latest_error = Some((error.to_owned(), file_id, updated_at_ms));
        }
    };
    for request in requests {
        let job = (!request.result_cache_key.is_empty())
            .then(|| by_key.get(request.result_cache_key.as_str()).copied())
            .flatten();
        if let Some(job) = job {
            represented.insert(job.cache_key.as_str());
            observe(
                &job.state,
                job.file_id,
                &job.last_error_code,
                job.updated_at_ms,
            );
        } else {
            observe(
                &request.state,
                request.file_id,
                &request.last_error_code,
                request.updated_at_ms,
            );
        }
    }
    for job in jobs {
        if !represented.contains(job.cache_key.as_str()) {
            observe(
                &job.state,
                job.file_id,
                &job.last_error_code,
                job.updated_at_ms,
            );
        }
    }
    let count = |state: &str| counts.get(state).copied().unwrap_or_default();
    serde_json::json!({
        "enabled": enabled,
        "total": counts.values().sum::<u64>(),
        "active": count("queued") + count("running") + count("submitted"),
        "queued": count("queued"),
        "running": count("running"),
        "submitted": count("submitted"),
        "failed": count("failed"),
        "cancelled": count("cancelled"),
        "ready": count("ready"),
        "now_ms": now_ms,
        "latest_error": latest_error.map(|(code, file_id, updated_at_ms)| serde_json::json!({
            "code": code,
            "file_id": file_id.to_string(),
            "updated_at_ms": updated_at_ms,
        })),
    })
}

/// Compact projection for Activity. It deliberately uses the same merge rule
/// as the full status response so a submitted request and its worker do not
/// appear as two pieces of work on one page and one piece on another.
pub(crate) async fn activity_summary(state: &AppState) -> Result<serde_json::Value, ApiError> {
    let now_ms = crate::state::clock_ms();
    let (requests, jobs) = tokio::try_join!(
        state.store.analysis_requests(500),
        state.store.cluster_fragment_index_jobs(500),
    )?;
    Ok(summary_value(
        &requests,
        &jobs,
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
    let enabled = state.jobs.analysis_queue_enabled().await;
    let summary = summary_value(&requests, &jobs, enabled, now_ms);
    Ok(Json(serde_json::json!({
        "enabled": enabled,
        "node_id": state.node_id,
        "now_ms": now_ms,
        "history_limit": 500,
        "summary": summary,
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
