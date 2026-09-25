//! Administrator observation and cancellation of durable background work.
use super::error::ApiError;
use super::extract::AdminUser;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use plurx_core::store::background_jobs::{
    BackgroundJob, CancelJob, JobKind, JobQuery, JobState, WaiterQuery,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListQuery {
    state: Option<JobState>,
    kind: Option<JobKind>,
    cursor: Option<String>,
}

fn valid_id(id: &str) -> Result<(), ApiError> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| ApiError::BadRequest("invalid job identity".into()))
}

fn summary(job: &BackgroundJob, now_ms: i64) -> Value {
    let kind = job
        .payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let file_id = job.payload.get("file_id").and_then(Value::as_i64);
    json!({
        "id": job.id, "kind": kind, "state": job.state, "priority": job.priority,
        "file_id": file_id.map(|id| id.to_string()),
        "owner_node_id": job.token.as_ref().map(|token| &token.node_id),
        "created_at_ms": job.created_at_ms, "updated_at_ms": job.updated_at_ms,
        "age_ms": now_ms.saturating_sub(job.created_at_ms).max(0),
        "not_before_ms": job.not_before_ms, "failed_attempts": job.failed_attempts,
        "attempt_limit": job.attempt_limit, "yield_count": job.yield_count,
        "abandoned_count": job.abandoned_count, "error_code": job.last_error_code,
        "payload_version": job.payload_version, "supported": job.supported_payload().is_ok(),
        "retry_deadline_ms": job.retry_deadline_ms,
        "observation": "durable_state", "observed_at_ms": now_ms,
    })
}

pub async fn list(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if let Some(cursor) = &query.cursor {
        valid_id(cursor)?;
    }
    let now_ms = crate::state::clock_ms();
    let (page, counts) = tokio::try_join!(
        state.store.list_jobs(JobQuery {
            state: query.state,
            kind: query.kind,
            after_id: query.cursor,
            limit: 100
        }),
        state.store.job_counts(now_ms),
    )?;
    let ids = page
        .jobs
        .iter()
        .map(|job| job.id.clone())
        .collect::<Vec<_>>();
    let labels = state.store.job_labels(&ids).await?;
    let jobs = page
        .jobs
        .iter()
        .map(|job| {
            let mut row = summary(job, now_ms);
            if let Some(label) = labels.iter().find(|label| label.id == job.id) {
                row["title"] = json!(label.title);
                row["library"] = json!(label.library);
            }
            row
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({"jobs": jobs,
        "counts": counts, "next_cursor": page.next_after_id, "observed_at_ms": now_ms})))
}

pub async fn detail(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    valid_id(&id)?;
    let job = state
        .store
        .background_job(&id)
        .await?
        .ok_or(ApiError::NotFound("background job"))?;
    let (attempts, waiters) = tokio::try_join!(
        state.store.job_attempts(&id),
        state.store.job_waiters(WaiterQuery {
            job_id: id.clone(),
            after: None,
            limit: 100,
        })
    )?;
    // Request identifiers, payloads, diagnostics, checkpoints, boot IDs and
    // publication references are not part of this operator response.
    Ok(Json(
        json!({"job": summary(&job, crate::state::clock_ms()), "attempts": attempts,
        "waiters": waiters.waiters.iter().map(|waiter| json!({"state": waiter.state,
            "consumer_kind": waiter.consumer_kind, "target_node_id": waiter.target_node_id,
            "priority": waiter.priority, "deadline_ms": waiter.deadline_ms,
            "updated_at_ms": waiter.updated_at_ms, "failed_attempts": waiter.failed_attempts,
            "attempt_limit": waiter.attempt_limit, "not_before_ms": waiter.not_before_ms,
            "retry_deadline_ms": waiter.retry_deadline_ms, "error_code": waiter.last_error_code})).collect::<Vec<_>>(),
        "more_waiters": waiters.next.is_some()}),
    ))
}

pub async fn cancel(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    valid_id(&id)?;
    let now_ms = crate::state::clock_ms();
    let job = match state
        .store
        .cancel_job(CancelJob {
            job_id: id.clone(),
            now_ms,
        })
        .await?
    {
        Some(job) => job,
        None => state
            .store
            .background_job(&id)
            .await?
            .ok_or(ApiError::NotFound("background job"))?,
    };
    Ok(Json(summary(&job, now_ms)))
}
